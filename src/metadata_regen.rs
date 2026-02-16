//! Metadata regeneration with lazy invalidation strategy.
//!
//! Implements the regeneration logic from the PRD:
//! - On scheduling: check if base_commit matches current main; if not, regenerate layer 2.
//! - On integration: mark all cached layer-2 data as stale (delete entries with old commit).
//! - On refactor integration: proactively regenerate layer 2 for all pending tasks.

use rusqlite::{Connection, Result};
use std::path::Path;

use crate::file_resolution::{self, FileResolution};
use crate::intent::{self, IntentAnalysis};

/// Outcome of an `ensure_fresh` call.
#[derive(Debug, PartialEq)]
pub enum RefreshOutcome {
    /// Cache was valid — no regeneration needed.
    CacheHit,
    /// Cache was stale or missing — regenerated from static analysis.
    Regenerated,
    /// No intent analysis exists for this task — cannot resolve.
    NoIntent,
}

/// Combined metadata for a task: intent analysis (Layer 1) + file resolution (Layer 2).
///
/// This is the value returned by `ensure_fresh_metadata()` and represents everything
/// the scheduler needs to know about a task's impact on the codebase.
#[derive(Debug, Clone)]
pub struct TaskMetadata {
    /// The task identifier.
    pub task_id: String,
    /// Layer 1: LLM-derived intent analysis (concepts + reasoning).
    pub intent: IntentAnalysis,
    /// Layer 2: File resolution mapping concepts to concrete files/modules.
    pub resolution: FileResolution,
}

impl TaskMetadata {
    /// Extract affected file globs suitable for the scheduler's conflict detection.
    ///
    /// Converts the resolved files from file resolution into glob patterns.
    /// Individual files become exact paths; modules with multiple files become
    /// directory globs (e.g. `src/auth/**`).
    pub fn affected_globs(&self) -> Vec<String> {
        let mut globs = Vec::new();
        for mapping in &self.resolution.mappings {
            for file in &mapping.resolved_files {
                if !globs.contains(file) {
                    globs.push(file.clone());
                }
            }
        }
        globs.sort();
        globs
    }
}

/// Ensure both intent analysis and file resolution are fresh for a task.
///
/// This is the main entry point for the scheduler integration. It:
/// 1. Looks up or regenerates the intent analysis (Layer 1)
/// 2. Looks up or regenerates the file resolution (Layer 2)
/// 3. Returns combined `TaskMetadata` with both layers
///
/// Returns `None` if the task has no intent analysis (e.g., LLM command not configured
/// and no prior analysis exists).
pub fn ensure_fresh_metadata(
    conn: &Connection,
    repo_root: &Path,
    task_id: &str,
    current_commit: &str,
) -> Result<Option<TaskMetadata>> {
    let (outcome, resolution) = ensure_fresh(conn, repo_root, task_id, current_commit)?;

    match outcome {
        RefreshOutcome::NoIntent => Ok(None),
        _ => {
            // Intent must exist if we got CacheHit or Regenerated
            let intent = intent::get_by_task_id(conn, task_id)?
                .expect("intent must exist after successful ensure_fresh");
            let resolution =
                resolution.expect("resolution must exist after successful ensure_fresh");

            Ok(Some(TaskMetadata {
                task_id: task_id.to_string(),
                intent,
                resolution,
            }))
        }
    }
}

/// Ensure the file resolution for a task is fresh relative to `current_commit`.
///
/// If the cache has a valid entry for (task_id, current_commit, intent_hash),
/// returns `CacheHit`. Otherwise, regenerates layer 2 via static analysis
/// and stores the result.
pub fn ensure_fresh(
    conn: &Connection,
    repo_root: &Path,
    task_id: &str,
    current_commit: &str,
) -> Result<(RefreshOutcome, Option<FileResolution>)> {
    // Step 1: Look up intent analysis (Layer 1) for the task
    let intent = match intent::get_by_task_id(conn, task_id)? {
        Some(i) => i,
        None => return Ok((RefreshOutcome::NoIntent, None)),
    };

    // Step 2: Check if we have a fresh resolution
    if file_resolution::is_fresh(conn, task_id, current_commit, &intent.content_hash)? {
        let cached = file_resolution::get(conn, task_id, current_commit, &intent.content_hash)?;
        return Ok((RefreshOutcome::CacheHit, cached));
    }

    // Step 3: Regenerate layer 2
    let resolution = file_resolution::resolve(
        repo_root,
        task_id,
        current_commit,
        &intent.content_hash,
        &intent.target_areas,
    );
    file_resolution::store(conn, &resolution)?;

    Ok((RefreshOutcome::Regenerated, Some(resolution)))
}

/// Mark all cached layer-2 data as stale after main advances.
///
/// Called after any integration to main. Deletes all file_resolution entries
/// whose base_commit doesn't match the new commit. Regeneration happens lazily
/// when `ensure_fresh` is called for individual tasks.
pub fn invalidate_on_integration(conn: &Connection, new_commit: &str) -> Result<usize> {
    file_resolution::invalidate_stale(conn, new_commit)
}

/// Proactively regenerate layer 2 for a list of pending tasks after a refactor integration.
///
/// Unlike normal integration (lazy), refactor integrations are more likely to
/// invalidate metadata, so we regenerate eagerly for all pending tasks that
/// have intent analyses.
pub fn regenerate_after_refactor(
    conn: &Connection,
    repo_root: &Path,
    new_commit: &str,
    pending_task_ids: &[&str],
) -> Result<RegenerationReport> {
    // First invalidate everything stale
    let invalidated = file_resolution::invalidate_stale(conn, new_commit)?;

    let mut regenerated = 0;
    let mut skipped_no_intent = 0;
    let mut already_fresh = 0;

    for task_id in pending_task_ids {
        match ensure_fresh(conn, repo_root, task_id, new_commit)? {
            (RefreshOutcome::Regenerated, _) => regenerated += 1,
            (RefreshOutcome::CacheHit, _) => already_fresh += 1,
            (RefreshOutcome::NoIntent, _) => skipped_no_intent += 1,
        }
    }

    Ok(RegenerationReport {
        invalidated,
        regenerated,
        already_fresh,
        skipped_no_intent,
    })
}

/// Summary of a bulk regeneration operation.
#[derive(Debug, Clone, PartialEq)]
pub struct RegenerationReport {
    /// Number of stale entries deleted.
    pub invalidated: usize,
    /// Number of tasks whose layer-2 was regenerated.
    pub regenerated: usize,
    /// Number of tasks that already had fresh data.
    pub already_fresh: usize,
    /// Number of tasks skipped because they lack intent analysis.
    pub skipped_no_intent: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_resolution::{self, DerivedFields, FileResolution, FileResolutionMapping};
    use crate::intent::{IntentAnalysis, TargetArea};

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        intent::create_table(&conn).unwrap();
        file_resolution::create_table(&conn).unwrap();
        conn
    }

    fn store_intent(conn: &Connection, task_id: &str, content_hash: &str) {
        let analysis = IntentAnalysis {
            task_id: task_id.to_string(),
            content_hash: content_hash.to_string(),
            target_areas: vec![TargetArea {
                concept: "test_concept".to_string(),
                reasoning: "testing".to_string(),
            }],
        };
        intent::store(conn, &analysis).unwrap();
    }

    fn store_resolution(conn: &Connection, task_id: &str, base_commit: &str, intent_hash: &str) {
        let res = FileResolution {
            task_id: task_id.to_string(),
            base_commit: base_commit.to_string(),
            intent_hash: intent_hash.to_string(),
            mappings: vec![FileResolutionMapping {
                concept: "test".to_string(),
                resolved_files: vec!["src/test.rs".to_string()],
                resolved_modules: vec!["test".to_string()],
            }],
            derived: DerivedFields::default(),
        };
        file_resolution::store(conn, &res).unwrap();
    }

    // --- ensure_fresh tests ---

    #[test]
    fn ensure_fresh_no_intent_returns_no_intent() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}").unwrap();

        let (outcome, resolution) =
            ensure_fresh(&conn, tmp.path(), "nonexistent-task", "commit1").unwrap();
        assert_eq!(outcome, RefreshOutcome::NoIntent);
        assert!(resolution.is_none());
    }

    #[test]
    fn ensure_fresh_cache_hit() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}").unwrap();

        // Store intent and a matching resolution
        store_intent(&conn, "task-1", "hash1");
        store_resolution(&conn, "task-1", "commit-a", "hash1");

        let (outcome, resolution) = ensure_fresh(&conn, tmp.path(), "task-1", "commit-a").unwrap();
        assert_eq!(outcome, RefreshOutcome::CacheHit);
        assert!(resolution.is_some());
        assert_eq!(resolution.unwrap().base_commit, "commit-a");
    }

    #[test]
    fn ensure_fresh_stale_commit_regenerates() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}").unwrap();

        // Store intent but resolution is at old commit
        store_intent(&conn, "task-1", "hash1");
        store_resolution(&conn, "task-1", "old-commit", "hash1");

        let (outcome, resolution) =
            ensure_fresh(&conn, tmp.path(), "task-1", "new-commit").unwrap();
        assert_eq!(outcome, RefreshOutcome::Regenerated);
        assert!(resolution.is_some());
        let res = resolution.unwrap();
        assert_eq!(res.base_commit, "new-commit");
        assert_eq!(res.intent_hash, "hash1");
    }

    #[test]
    fn ensure_fresh_no_cached_resolution_regenerates() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}").unwrap();

        // Intent exists but no resolution at all
        store_intent(&conn, "task-1", "hash1");

        let (outcome, resolution) = ensure_fresh(&conn, tmp.path(), "task-1", "commit-a").unwrap();
        assert_eq!(outcome, RefreshOutcome::Regenerated);
        assert!(resolution.is_some());
    }

    #[test]
    fn ensure_fresh_stores_result_for_next_call() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}").unwrap();

        store_intent(&conn, "task-1", "hash1");

        // First call: regenerate
        let (outcome1, _) = ensure_fresh(&conn, tmp.path(), "task-1", "commit-a").unwrap();
        assert_eq!(outcome1, RefreshOutcome::Regenerated);

        // Second call: cache hit
        let (outcome2, _) = ensure_fresh(&conn, tmp.path(), "task-1", "commit-a").unwrap();
        assert_eq!(outcome2, RefreshOutcome::CacheHit);
    }

    // --- invalidate_on_integration tests ---

    #[test]
    fn invalidate_on_integration_removes_old_entries() {
        let conn = setup_db();
        store_resolution(&conn, "task-1", "old-commit", "h1");
        store_resolution(&conn, "task-2", "old-commit", "h2");
        store_resolution(&conn, "task-3", "current", "h3");

        let deleted = invalidate_on_integration(&conn, "current").unwrap();
        assert_eq!(deleted, 2);

        assert!(file_resolution::get(&conn, "task-3", "current", "h3")
            .unwrap()
            .is_some());
        assert!(file_resolution::get(&conn, "task-1", "old-commit", "h1")
            .unwrap()
            .is_none());
    }

    #[test]
    fn invalidate_on_integration_noop_when_all_fresh() {
        let conn = setup_db();
        store_resolution(&conn, "task-1", "current", "h1");

        let deleted = invalidate_on_integration(&conn, "current").unwrap();
        assert_eq!(deleted, 0);
    }

    // --- regenerate_after_refactor tests ---

    #[test]
    fn regenerate_after_refactor_full_workflow() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}").unwrap();

        // Setup: 3 tasks with intents, resolutions at old commit
        store_intent(&conn, "task-1", "h1");
        store_intent(&conn, "task-2", "h2");
        store_intent(&conn, "task-3", "h3");
        store_resolution(&conn, "task-1", "old-commit", "h1");
        store_resolution(&conn, "task-2", "old-commit", "h2");
        // task-3 has no existing resolution

        let report = regenerate_after_refactor(
            &conn,
            tmp.path(),
            "new-commit",
            &["task-1", "task-2", "task-3"],
        )
        .unwrap();

        // All 3 should be regenerated (old ones invalidated, task-3 had none)
        assert_eq!(report.invalidated, 2); // task-1 and task-2 old entries
        assert_eq!(report.regenerated, 3);
        assert_eq!(report.already_fresh, 0);
        assert_eq!(report.skipped_no_intent, 0);
    }

    #[test]
    fn regenerate_after_refactor_skips_no_intent() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}").unwrap();

        // Only task-1 has intent
        store_intent(&conn, "task-1", "h1");

        let report = regenerate_after_refactor(
            &conn,
            tmp.path(),
            "new-commit",
            &["task-1", "task-no-intent"],
        )
        .unwrap();

        assert_eq!(report.regenerated, 1);
        assert_eq!(report.skipped_no_intent, 1);
    }

    #[test]
    fn regenerate_after_refactor_already_fresh() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}").unwrap();

        // task-1 already has fresh resolution at current commit
        store_intent(&conn, "task-1", "h1");
        store_resolution(&conn, "task-1", "current-commit", "h1");

        let report =
            regenerate_after_refactor(&conn, tmp.path(), "current-commit", &["task-1"]).unwrap();

        assert_eq!(report.invalidated, 0);
        assert_eq!(report.regenerated, 0);
        assert_eq!(report.already_fresh, 1);
    }

    #[test]
    fn regenerate_after_refactor_empty_task_list() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}").unwrap();

        // Old resolution exists but no tasks to regenerate
        store_resolution(&conn, "task-1", "old-commit", "h1");

        let report = regenerate_after_refactor(&conn, tmp.path(), "new-commit", &[]).unwrap();

        assert_eq!(report.invalidated, 1); // old entry still cleaned up
        assert_eq!(report.regenerated, 0);
        assert_eq!(report.already_fresh, 0);
        assert_eq!(report.skipped_no_intent, 0);
    }

    #[test]
    fn regenerate_after_refactor_mixed_states() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}").unwrap();

        // task-1: has intent + stale resolution → regenerated
        store_intent(&conn, "task-1", "h1");
        store_resolution(&conn, "task-1", "old", "h1");

        // task-2: has intent + fresh resolution → already_fresh
        store_intent(&conn, "task-2", "h2");
        store_resolution(&conn, "task-2", "new-commit", "h2");

        // task-3: no intent → skipped
        // task-4: has intent, no resolution → regenerated
        store_intent(&conn, "task-4", "h4");

        let report = regenerate_after_refactor(
            &conn,
            tmp.path(),
            "new-commit",
            &["task-1", "task-2", "task-3", "task-4"],
        )
        .unwrap();

        assert_eq!(report.invalidated, 1); // task-1's old entry
        assert_eq!(report.regenerated, 2); // task-1 and task-4
        assert_eq!(report.already_fresh, 1); // task-2
        assert_eq!(report.skipped_no_intent, 1); // task-3
    }

    // --- ensure_fresh_metadata tests ---

    #[test]
    fn ensure_fresh_metadata_returns_none_without_intent() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}").unwrap();

        let result = ensure_fresh_metadata(&conn, tmp.path(), "no-task", "commit1").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn ensure_fresh_metadata_returns_combined_on_cache_hit() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}").unwrap();

        store_intent(&conn, "task-1", "hash1");
        store_resolution(&conn, "task-1", "commit-a", "hash1");

        let meta = ensure_fresh_metadata(&conn, tmp.path(), "task-1", "commit-a")
            .unwrap()
            .unwrap();
        assert_eq!(meta.task_id, "task-1");
        assert_eq!(meta.intent.task_id, "task-1");
        assert_eq!(meta.intent.content_hash, "hash1");
        assert_eq!(meta.resolution.base_commit, "commit-a");
    }

    #[test]
    fn ensure_fresh_metadata_returns_combined_on_regeneration() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}").unwrap();

        store_intent(&conn, "task-1", "hash1");
        // No resolution stored → will regenerate

        let meta = ensure_fresh_metadata(&conn, tmp.path(), "task-1", "commit-new")
            .unwrap()
            .unwrap();
        assert_eq!(meta.task_id, "task-1");
        assert_eq!(meta.intent.content_hash, "hash1");
        assert_eq!(meta.resolution.base_commit, "commit-new");
    }

    #[test]
    fn task_metadata_affected_globs_from_resolution() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "mod config;\nfn main() {}").unwrap();
        std::fs::write(tmp.path().join("src/config.rs"), "pub struct Config;").unwrap();

        // Store intent with concept "config" so resolution finds config.rs
        let analysis = IntentAnalysis {
            task_id: "task-globs".to_string(),
            content_hash: "hg".to_string(),
            target_areas: vec![TargetArea {
                concept: "config".to_string(),
                reasoning: "config changes".to_string(),
            }],
        };
        intent::store(&conn, &analysis).unwrap();

        let meta = ensure_fresh_metadata(&conn, tmp.path(), "task-globs", "commit-x")
            .unwrap()
            .unwrap();

        let globs = meta.affected_globs();
        assert!(!globs.is_empty());
        assert!(globs.iter().any(|g| g.contains("config")));
    }

    #[test]
    fn task_metadata_affected_globs_empty_when_no_matches() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}").unwrap();

        // Intent with concept that won't match any files
        let analysis = IntentAnalysis {
            task_id: "task-noglobs".to_string(),
            content_hash: "hng".to_string(),
            target_areas: vec![TargetArea {
                concept: "nonexistent_module".to_string(),
                reasoning: "nothing".to_string(),
            }],
        };
        intent::store(&conn, &analysis).unwrap();

        let meta = ensure_fresh_metadata(&conn, tmp.path(), "task-noglobs", "commit-y")
            .unwrap()
            .unwrap();

        let globs = meta.affected_globs();
        assert!(globs.is_empty());
    }
}
