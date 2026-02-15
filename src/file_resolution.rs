//! Layer 2: File Resolution data model and cache.
//!
//! Maps abstract concepts from intent analysis (Layer 1) onto concrete files
//! and modules at a specific commit. Invalidates every time main advances
//! (keyed by base_commit).

use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};

/// A single concept-to-files mapping: which files and modules correspond to a concept.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileResolutionMapping {
    pub concept: String,
    pub resolved_files: Vec<String>,
    pub resolved_modules: Vec<String>,
}

/// Derived analysis fields computed from the mappings and import graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DerivedFields {
    /// All modules directly touched by this task's concepts.
    pub affected_modules: Vec<String>,
    /// Transitive dependents — modules that import from affected_modules.
    pub blast_radius: Vec<String>,
    /// Public API signatures at module boundaries that this task may affect.
    pub boundary_signatures: Vec<String>,
}

/// The result of file resolution for a task at a specific commit.
///
/// This is Layer 2 — volatile, cheap to regenerate via static analysis.
/// Invalidates whenever main advances (base_commit changes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileResolution {
    pub task_id: String,
    pub base_commit: String,
    pub intent_hash: String,
    pub mappings: Vec<FileResolutionMapping>,
    pub derived: DerivedFields,
}

/// Create the file_resolutions table if it doesn't exist.
pub fn create_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS file_resolutions (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id      TEXT NOT NULL,
            base_commit  TEXT NOT NULL,
            intent_hash  TEXT NOT NULL,
            mappings     TEXT NOT NULL,
            derived      TEXT NOT NULL,
            created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            UNIQUE(task_id, base_commit, intent_hash)
        );

        CREATE INDEX IF NOT EXISTS idx_file_resolutions_task_commit
            ON file_resolutions(task_id, base_commit);
        CREATE INDEX IF NOT EXISTS idx_file_resolutions_commit
            ON file_resolutions(base_commit);",
    )
}

/// Look up cached file resolution for a task at a specific commit.
///
/// Returns the resolution if the cache has a valid entry for this
/// (task_id, base_commit, intent_hash) triple. A cache hit means
/// the resolution is still valid — same commit, same intent analysis.
pub fn get(
    conn: &Connection,
    task_id: &str,
    base_commit: &str,
    intent_hash: &str,
) -> Result<Option<FileResolution>> {
    let mut stmt = conn.prepare(
        "SELECT task_id, base_commit, intent_hash, mappings, derived
         FROM file_resolutions
         WHERE task_id = ?1 AND base_commit = ?2 AND intent_hash = ?3
         ORDER BY id DESC
         LIMIT 1",
    )?;

    let mut rows = stmt.query_map(params![task_id, base_commit, intent_hash], |row| {
        let task_id: String = row.get(0)?;
        let base_commit: String = row.get(1)?;
        let intent_hash: String = row.get(2)?;
        let mappings_json: String = row.get(3)?;
        let derived_json: String = row.get(4)?;
        Ok((
            task_id,
            base_commit,
            intent_hash,
            mappings_json,
            derived_json,
        ))
    })?;

    match rows.next() {
        Some(Ok((task_id, base_commit, intent_hash, mappings_json, derived_json))) => {
            let mappings: Vec<FileResolutionMapping> =
                serde_json::from_str(&mappings_json).unwrap_or_default();
            let derived: DerivedFields = serde_json::from_str(&derived_json).unwrap_or_default();
            Ok(Some(FileResolution {
                task_id,
                base_commit,
                intent_hash,
                mappings,
                derived,
            }))
        }
        Some(Err(e)) => Err(e),
        None => Ok(None),
    }
}

/// Look up the most recent file resolution for a task (any commit).
///
/// Useful for checking whether a task has ever been resolved, even if
/// the cached entry is stale (base_commit doesn't match current HEAD).
pub fn get_latest_for_task(conn: &Connection, task_id: &str) -> Result<Option<FileResolution>> {
    let mut stmt = conn.prepare(
        "SELECT task_id, base_commit, intent_hash, mappings, derived
         FROM file_resolutions
         WHERE task_id = ?1
         ORDER BY id DESC
         LIMIT 1",
    )?;

    let mut rows = stmt.query_map(params![task_id], |row| {
        let task_id: String = row.get(0)?;
        let base_commit: String = row.get(1)?;
        let intent_hash: String = row.get(2)?;
        let mappings_json: String = row.get(3)?;
        let derived_json: String = row.get(4)?;
        Ok((
            task_id,
            base_commit,
            intent_hash,
            mappings_json,
            derived_json,
        ))
    })?;

    match rows.next() {
        Some(Ok((task_id, base_commit, intent_hash, mappings_json, derived_json))) => {
            let mappings: Vec<FileResolutionMapping> =
                serde_json::from_str(&mappings_json).unwrap_or_default();
            let derived: DerivedFields = serde_json::from_str(&derived_json).unwrap_or_default();
            Ok(Some(FileResolution {
                task_id,
                base_commit,
                intent_hash,
                mappings,
                derived,
            }))
        }
        Some(Err(e)) => Err(e),
        None => Ok(None),
    }
}

/// Store a file resolution result, replacing any existing entry for the
/// same (task_id, base_commit, intent_hash) triple.
pub fn store(conn: &Connection, resolution: &FileResolution) -> Result<()> {
    let mappings_json =
        serde_json::to_string(&resolution.mappings).unwrap_or_else(|_| "[]".to_string());
    let derived_json =
        serde_json::to_string(&resolution.derived).unwrap_or_else(|_| "{}".to_string());

    conn.execute(
        "INSERT OR REPLACE INTO file_resolutions (task_id, base_commit, intent_hash, mappings, derived)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            resolution.task_id,
            resolution.base_commit,
            resolution.intent_hash,
            mappings_json,
            derived_json,
        ],
    )?;
    Ok(())
}

/// Invalidate all cached file resolutions that were computed against
/// a different commit than `current_commit`.
///
/// This is called when main advances. Rather than eagerly regenerating,
/// we just delete stale entries — regeneration happens lazily when the
/// scheduler next needs the data.
pub fn invalidate_stale(conn: &Connection, current_commit: &str) -> Result<usize> {
    let count = conn.execute(
        "DELETE FROM file_resolutions WHERE base_commit != ?1",
        params![current_commit],
    )?;
    Ok(count)
}

/// Check whether a cached resolution exists and is fresh (matches current commit).
pub fn is_fresh(
    conn: &Connection,
    task_id: &str,
    current_commit: &str,
    intent_hash: &str,
) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM file_resolutions
         WHERE task_id = ?1 AND base_commit = ?2 AND intent_hash = ?3",
        params![task_id, current_commit, intent_hash],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        create_table(&conn).unwrap();
        conn
    }

    fn sample_resolution() -> FileResolution {
        FileResolution {
            task_id: "task-13".to_string(),
            base_commit: "abc123".to_string(),
            intent_hash: "a8f3c1".to_string(),
            mappings: vec![
                FileResolutionMapping {
                    concept: "auth_endpoints".to_string(),
                    resolved_files: vec![
                        "src/auth/handlers.rs".to_string(),
                        "src/auth/routes.rs".to_string(),
                    ],
                    resolved_modules: vec!["auth".to_string()],
                },
                FileResolutionMapping {
                    concept: "config".to_string(),
                    resolved_files: vec!["src/config/mod.rs".to_string()],
                    resolved_modules: vec!["config".to_string()],
                },
            ],
            derived: DerivedFields {
                affected_modules: vec!["auth".to_string(), "config".to_string()],
                blast_radius: vec!["auth".to_string(), "config".to_string(), "api".to_string()],
                boundary_signatures: vec![
                    "pub fn auth::handlers::login(req: Request) -> Response".to_string()
                ],
            },
        }
    }

    #[test]
    fn store_and_retrieve() {
        let conn = setup_db();
        let res = sample_resolution();
        store(&conn, &res).unwrap();

        let retrieved = get(&conn, "task-13", "abc123", "a8f3c1").unwrap().unwrap();
        assert_eq!(retrieved.task_id, "task-13");
        assert_eq!(retrieved.base_commit, "abc123");
        assert_eq!(retrieved.intent_hash, "a8f3c1");
        assert_eq!(retrieved.mappings.len(), 2);
        assert_eq!(retrieved.mappings[0].concept, "auth_endpoints");
        assert_eq!(retrieved.mappings[0].resolved_files.len(), 2);
        assert_eq!(retrieved.mappings[1].concept, "config");
        assert_eq!(retrieved.derived.affected_modules.len(), 2);
        assert_eq!(retrieved.derived.blast_radius.len(), 3);
        assert_eq!(retrieved.derived.boundary_signatures.len(), 1);
    }

    #[test]
    fn cache_miss_returns_none() {
        let conn = setup_db();
        assert!(get(&conn, "no-task", "no-commit", "no-hash")
            .unwrap()
            .is_none());
    }

    #[test]
    fn different_commit_is_cache_miss() {
        let conn = setup_db();
        let res = sample_resolution();
        store(&conn, &res).unwrap();

        // Same task+intent but different commit → miss
        assert!(get(&conn, "task-13", "different-commit", "a8f3c1")
            .unwrap()
            .is_none());
    }

    #[test]
    fn different_intent_hash_is_cache_miss() {
        let conn = setup_db();
        let res = sample_resolution();
        store(&conn, &res).unwrap();

        // Same task+commit but different intent → miss
        assert!(get(&conn, "task-13", "abc123", "different-intent")
            .unwrap()
            .is_none());
    }

    #[test]
    fn upsert_replaces_existing() {
        let conn = setup_db();
        let mut res = sample_resolution();
        store(&conn, &res).unwrap();

        // Update mappings
        res.mappings = vec![FileResolutionMapping {
            concept: "updated".to_string(),
            resolved_files: vec!["src/new.rs".to_string()],
            resolved_modules: vec!["new".to_string()],
        }];
        store(&conn, &res).unwrap();

        let retrieved = get(&conn, "task-13", "abc123", "a8f3c1").unwrap().unwrap();
        assert_eq!(retrieved.mappings.len(), 1);
        assert_eq!(retrieved.mappings[0].concept, "updated");
    }

    #[test]
    fn get_latest_for_task_returns_most_recent() {
        let conn = setup_db();

        let res1 = FileResolution {
            task_id: "task-1".to_string(),
            base_commit: "commit-old".to_string(),
            intent_hash: "hash1".to_string(),
            mappings: vec![FileResolutionMapping {
                concept: "old".to_string(),
                resolved_files: vec![],
                resolved_modules: vec![],
            }],
            derived: DerivedFields::default(),
        };
        store(&conn, &res1).unwrap();

        let res2 = FileResolution {
            task_id: "task-1".to_string(),
            base_commit: "commit-new".to_string(),
            intent_hash: "hash2".to_string(),
            mappings: vec![FileResolutionMapping {
                concept: "new".to_string(),
                resolved_files: vec![],
                resolved_modules: vec![],
            }],
            derived: DerivedFields::default(),
        };
        store(&conn, &res2).unwrap();

        let latest = get_latest_for_task(&conn, "task-1").unwrap().unwrap();
        assert_eq!(latest.base_commit, "commit-new");
        assert_eq!(latest.mappings[0].concept, "new");
    }

    #[test]
    fn get_latest_for_task_miss() {
        let conn = setup_db();
        assert!(get_latest_for_task(&conn, "nonexistent").unwrap().is_none());
    }

    #[test]
    fn invalidate_stale_removes_old_commits() {
        let conn = setup_db();

        // Store entries at two different commits
        let res1 = FileResolution {
            task_id: "task-1".to_string(),
            base_commit: "old-commit".to_string(),
            intent_hash: "h1".to_string(),
            mappings: vec![],
            derived: DerivedFields::default(),
        };
        let res2 = FileResolution {
            task_id: "task-2".to_string(),
            base_commit: "current-commit".to_string(),
            intent_hash: "h2".to_string(),
            mappings: vec![],
            derived: DerivedFields::default(),
        };
        let res3 = FileResolution {
            task_id: "task-3".to_string(),
            base_commit: "another-old".to_string(),
            intent_hash: "h3".to_string(),
            mappings: vec![],
            derived: DerivedFields::default(),
        };
        store(&conn, &res1).unwrap();
        store(&conn, &res2).unwrap();
        store(&conn, &res3).unwrap();

        let deleted = invalidate_stale(&conn, "current-commit").unwrap();
        assert_eq!(deleted, 2);

        // Only current-commit entry survives
        assert!(get(&conn, "task-2", "current-commit", "h2")
            .unwrap()
            .is_some());
        assert!(get(&conn, "task-1", "old-commit", "h1").unwrap().is_none());
        assert!(get(&conn, "task-3", "another-old", "h3").unwrap().is_none());
    }

    #[test]
    fn invalidate_stale_noop_when_all_fresh() {
        let conn = setup_db();

        let res = FileResolution {
            task_id: "task-1".to_string(),
            base_commit: "current".to_string(),
            intent_hash: "h1".to_string(),
            mappings: vec![],
            derived: DerivedFields::default(),
        };
        store(&conn, &res).unwrap();

        let deleted = invalidate_stale(&conn, "current").unwrap();
        assert_eq!(deleted, 0);
    }

    #[test]
    fn is_fresh_returns_true_for_matching_entry() {
        let conn = setup_db();
        let res = sample_resolution();
        store(&conn, &res).unwrap();

        assert!(is_fresh(&conn, "task-13", "abc123", "a8f3c1").unwrap());
    }

    #[test]
    fn is_fresh_returns_false_for_stale_commit() {
        let conn = setup_db();
        let res = sample_resolution();
        store(&conn, &res).unwrap();

        assert!(!is_fresh(&conn, "task-13", "new-commit", "a8f3c1").unwrap());
    }

    #[test]
    fn is_fresh_returns_false_for_missing_task() {
        let conn = setup_db();
        assert!(!is_fresh(&conn, "no-task", "any", "any").unwrap());
    }

    #[test]
    fn empty_mappings_and_derived() {
        let conn = setup_db();
        let res = FileResolution {
            task_id: "task-empty".to_string(),
            base_commit: "commit".to_string(),
            intent_hash: "hash".to_string(),
            mappings: vec![],
            derived: DerivedFields::default(),
        };
        store(&conn, &res).unwrap();

        let retrieved = get(&conn, "task-empty", "commit", "hash").unwrap().unwrap();
        assert!(retrieved.mappings.is_empty());
        assert!(retrieved.derived.affected_modules.is_empty());
        assert!(retrieved.derived.blast_radius.is_empty());
        assert!(retrieved.derived.boundary_signatures.is_empty());
    }

    #[test]
    fn multiple_tasks_same_commit() {
        let conn = setup_db();

        let res1 = FileResolution {
            task_id: "task-a".to_string(),
            base_commit: "same-commit".to_string(),
            intent_hash: "ha".to_string(),
            mappings: vec![FileResolutionMapping {
                concept: "auth".to_string(),
                resolved_files: vec!["src/auth.rs".to_string()],
                resolved_modules: vec!["auth".to_string()],
            }],
            derived: DerivedFields::default(),
        };
        let res2 = FileResolution {
            task_id: "task-b".to_string(),
            base_commit: "same-commit".to_string(),
            intent_hash: "hb".to_string(),
            mappings: vec![FileResolutionMapping {
                concept: "db".to_string(),
                resolved_files: vec!["src/db.rs".to_string()],
                resolved_modules: vec!["db".to_string()],
            }],
            derived: DerivedFields::default(),
        };

        store(&conn, &res1).unwrap();
        store(&conn, &res2).unwrap();

        let r1 = get(&conn, "task-a", "same-commit", "ha").unwrap().unwrap();
        let r2 = get(&conn, "task-b", "same-commit", "hb").unwrap().unwrap();
        assert_eq!(r1.mappings[0].concept, "auth");
        assert_eq!(r2.mappings[0].concept, "db");
    }
}
