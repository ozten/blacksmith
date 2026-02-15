use crate::config::HarnessConfig;
use crate::data_dir::DataDir;
use std::path::{Path, PathBuf};

/// Consolidate legacy V2 files into the .blacksmith/ directory structure.
///
/// This moves:
/// - `blacksmith.db` / `harness.db` from the old output directory into `.blacksmith/`
/// - `{output_prefix}-{N}.jsonl` files into `.blacksmith/sessions/{N}.jsonl`
/// - The legacy counter file into `.blacksmith/counter`
/// - The legacy status file into `.blacksmith/status`
///
/// Files are moved (renamed), not copied. If any move fails, the migration
/// stops immediately and reports the error.
pub fn consolidate(config: &HarnessConfig, data_dir: &DataDir) -> Result<(), String> {
    let output_dir = &config.session.output_dir;
    let output_prefix = &config.session.output_prefix;
    let mut summary = MigrationSummary::default();

    // 1. Move database files
    for db_name in &["blacksmith.db", "harness.db"] {
        let src = output_dir.join(db_name);
        if src.exists() {
            let dest = data_dir.db();
            if dest.exists() {
                println!(
                    "  skip: {} (already exists at {})",
                    src.display(),
                    dest.display()
                );
                summary.skipped += 1;
            } else {
                move_file(&src, &dest)?;
                summary.moved += 1;
                println!("  moved: {} -> {}", src.display(), dest.display());
            }
        }
    }

    // 2. Move {output_prefix}-{N}.jsonl files into sessions/{N}.jsonl
    let session_files = find_session_files(output_dir, output_prefix)?;
    for (src, iteration) in &session_files {
        let dest = data_dir.session_file(*iteration);
        if dest.exists() {
            println!(
                "  skip: {} (already exists at {})",
                src.display(),
                dest.display()
            );
            summary.skipped += 1;
        } else {
            move_file(src, &dest)?;
            summary.moved += 1;
            println!("  moved: {} -> {}", src.display(), dest.display());
        }
    }

    // 3. Move counter file
    let legacy_counter = &config.session.counter_file;
    if legacy_counter.exists() {
        let dest = data_dir.counter();
        if dest.exists() {
            println!(
                "  skip: {} (already exists at {})",
                legacy_counter.display(),
                dest.display()
            );
            summary.skipped += 1;
        } else {
            move_file(legacy_counter, &dest)?;
            summary.moved += 1;
            println!(
                "  moved: {} -> {}",
                legacy_counter.display(),
                dest.display()
            );
        }
    }

    // 4. Move status file (legacy name was typically in the output dir)
    let legacy_status = output_dir.join("status");
    if legacy_status.exists() {
        let dest = data_dir.status();
        if dest.exists() {
            println!(
                "  skip: {} (already exists at {})",
                legacy_status.display(),
                dest.display()
            );
            summary.skipped += 1;
        } else {
            move_file(&legacy_status, &dest)?;
            summary.moved += 1;
            println!("  moved: {} -> {}", legacy_status.display(), dest.display());
        }
    }

    // Print summary
    println!();
    println!("Migration complete:");
    println!("  {} file(s) moved", summary.moved);
    if summary.skipped > 0 {
        println!("  {} file(s) skipped (already exist)", summary.skipped);
    }
    if summary.moved == 0 && summary.skipped == 0 {
        println!("  No legacy files found to migrate.");
    }

    Ok(())
}

/// Find all `{output_prefix}-{N}.jsonl` files in the given directory.
/// Returns (path, iteration_number) pairs sorted by iteration number.
fn find_session_files(dir: &Path, prefix: &str) -> Result<Vec<(PathBuf, u32)>, String> {
    let mut results = Vec::new();

    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("failed to read directory {}: {}", dir.display(), e))?;

    let expected_prefix = format!("{}-", prefix);
    let expected_suffix = ".jsonl";

    for entry in entries {
        let entry = entry.map_err(|e| format!("failed to read directory entry: {e}"))?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        if let Some(rest) = name.strip_prefix(&expected_prefix) {
            if let Some(num_str) = rest.strip_suffix(expected_suffix) {
                if let Ok(n) = num_str.parse::<u32>() {
                    results.push((entry.path(), n));
                }
            }
        }
    }

    results.sort_by_key(|(_, n)| *n);
    Ok(results)
}

/// Move a file from src to dest using rename. Falls back to copy+delete
/// if rename fails (e.g. cross-device move).
fn move_file(src: &Path, dest: &Path) -> Result<(), String> {
    // Try rename first (atomic, same filesystem)
    match std::fs::rename(src, dest) {
        Ok(()) => Ok(()),
        Err(rename_err) => {
            // Rename can fail across filesystems — fall back to copy + remove
            std::fs::copy(src, dest).map_err(|e| {
                format!(
                    "failed to move {} -> {}: rename failed ({}), copy also failed ({})",
                    src.display(),
                    dest.display(),
                    rename_err,
                    e
                )
            })?;
            std::fs::remove_file(src).map_err(|e| {
                format!(
                    "copied {} -> {} but failed to remove source: {}",
                    src.display(),
                    dest.display(),
                    e
                )
            })?;
            Ok(())
        }
    }
}

#[derive(Default)]
struct MigrationSummary {
    moved: usize,
    skipped: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SessionConfig;

    #[test]
    fn test_find_session_files_basic() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // Create some session files
        std::fs::write(dir.join("claude-iteration-0.jsonl"), "{}").unwrap();
        std::fs::write(dir.join("claude-iteration-5.jsonl"), "{}").unwrap();
        std::fs::write(dir.join("claude-iteration-12.jsonl"), "{}").unwrap();
        // Non-matching files
        std::fs::write(dir.join("other-file.jsonl"), "{}").unwrap();
        std::fs::write(dir.join("claude-iteration-abc.jsonl"), "{}").unwrap();

        let results = find_session_files(dir, "claude-iteration").unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].1, 0);
        assert_eq!(results[1].1, 5);
        assert_eq!(results[2].1, 12);
    }

    #[test]
    fn test_find_session_files_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let results = find_session_files(tmp.path(), "claude-iteration").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_find_session_files_custom_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        std::fs::write(dir.join("test-run-0.jsonl"), "{}").unwrap();
        std::fs::write(dir.join("test-run-1.jsonl"), "{}").unwrap();
        std::fs::write(dir.join("claude-iteration-0.jsonl"), "{}").unwrap();

        let results = find_session_files(dir, "test-run").unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_move_file_same_filesystem() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("source.txt");
        let dest = tmp.path().join("dest.txt");

        std::fs::write(&src, "hello").unwrap();
        move_file(&src, &dest).unwrap();

        assert!(!src.exists());
        assert!(dest.exists());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "hello");
    }

    #[test]
    fn test_consolidate_moves_session_files() {
        let tmp = tempfile::tempdir().unwrap();
        let output_dir = tmp.path().to_path_buf();
        let data_root = tmp.path().join(".blacksmith");

        // Create legacy session files
        std::fs::write(output_dir.join("claude-iteration-0.jsonl"), r#"{"a":1}"#).unwrap();
        std::fs::write(output_dir.join("claude-iteration-3.jsonl"), r#"{"b":2}"#).unwrap();

        // Create legacy counter file
        let counter_file = output_dir.join(".iteration_counter");
        std::fs::write(&counter_file, "4").unwrap();

        let config = HarnessConfig {
            session: SessionConfig {
                output_dir: output_dir.clone(),
                output_prefix: "claude-iteration".to_string(),
                counter_file: counter_file.clone(),
                ..Default::default()
            },
            ..Default::default()
        };

        let dd = DataDir::new(&data_root);
        dd.init().unwrap();

        consolidate(&config, &dd).unwrap();

        // Session files moved
        assert!(!output_dir.join("claude-iteration-0.jsonl").exists());
        assert!(!output_dir.join("claude-iteration-3.jsonl").exists());
        assert!(data_root.join("sessions/0.jsonl").exists());
        assert!(data_root.join("sessions/3.jsonl").exists());
        assert_eq!(
            std::fs::read_to_string(data_root.join("sessions/0.jsonl")).unwrap(),
            r#"{"a":1}"#
        );

        // Counter moved
        assert!(!counter_file.exists());
        assert!(data_root.join("counter").exists());
        assert_eq!(
            std::fs::read_to_string(data_root.join("counter")).unwrap(),
            "4"
        );
    }

    #[test]
    fn test_consolidate_moves_database() {
        let tmp = tempfile::tempdir().unwrap();
        let output_dir = tmp.path().to_path_buf();
        let data_root = tmp.path().join(".blacksmith");

        // Create legacy db
        std::fs::write(output_dir.join("blacksmith.db"), "sqlite").unwrap();

        let config = HarnessConfig {
            session: SessionConfig {
                output_dir: output_dir.clone(),
                ..Default::default()
            },
            ..Default::default()
        };

        let dd = DataDir::new(&data_root);
        dd.init().unwrap();

        consolidate(&config, &dd).unwrap();

        assert!(!output_dir.join("blacksmith.db").exists());
        assert!(data_root.join("blacksmith.db").exists());
    }

    #[test]
    fn test_consolidate_moves_harness_db() {
        let tmp = tempfile::tempdir().unwrap();
        let output_dir = tmp.path().to_path_buf();
        let data_root = tmp.path().join(".blacksmith");

        // Create legacy harness.db
        std::fs::write(output_dir.join("harness.db"), "sqlite").unwrap();

        let config = HarnessConfig {
            session: SessionConfig {
                output_dir: output_dir.clone(),
                ..Default::default()
            },
            ..Default::default()
        };

        let dd = DataDir::new(&data_root);
        dd.init().unwrap();

        consolidate(&config, &dd).unwrap();

        assert!(!output_dir.join("harness.db").exists());
        // harness.db gets moved to the standard blacksmith.db location
        assert!(data_root.join("blacksmith.db").exists());
    }

    #[test]
    fn test_consolidate_skips_existing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let output_dir = tmp.path().to_path_buf();
        let data_root = tmp.path().join(".blacksmith");

        // Create legacy session file
        std::fs::write(output_dir.join("claude-iteration-0.jsonl"), "old").unwrap();

        let dd = DataDir::new(&data_root);
        dd.init().unwrap();

        // Pre-populate destination
        std::fs::write(data_root.join("sessions/0.jsonl"), "new").unwrap();

        let config = HarnessConfig {
            session: SessionConfig {
                output_dir: output_dir.clone(),
                ..Default::default()
            },
            ..Default::default()
        };

        consolidate(&config, &dd).unwrap();

        // Source should NOT be deleted (it was skipped)
        assert!(output_dir.join("claude-iteration-0.jsonl").exists());
        // Destination should keep its existing content
        assert_eq!(
            std::fs::read_to_string(data_root.join("sessions/0.jsonl")).unwrap(),
            "new"
        );
    }

    #[test]
    fn test_consolidate_no_legacy_files() {
        let tmp = tempfile::tempdir().unwrap();
        let output_dir = tmp.path().to_path_buf();
        let data_root = tmp.path().join(".blacksmith");

        let config = HarnessConfig {
            session: SessionConfig {
                output_dir: output_dir.clone(),
                ..Default::default()
            },
            ..Default::default()
        };

        let dd = DataDir::new(&data_root);
        dd.init().unwrap();

        // Should succeed with nothing to do
        consolidate(&config, &dd).unwrap();
    }

    #[test]
    fn test_consolidate_moves_status_file() {
        let tmp = tempfile::tempdir().unwrap();
        let output_dir = tmp.path().to_path_buf();
        let data_root = tmp.path().join(".blacksmith");

        // Create legacy status file
        std::fs::write(output_dir.join("status"), "running").unwrap();

        let config = HarnessConfig {
            session: SessionConfig {
                output_dir: output_dir.clone(),
                ..Default::default()
            },
            ..Default::default()
        };

        let dd = DataDir::new(&data_root);
        dd.init().unwrap();

        consolidate(&config, &dd).unwrap();

        assert!(!output_dir.join("status").exists());
        assert!(data_root.join("status").exists());
        assert_eq!(
            std::fs::read_to_string(data_root.join("status")).unwrap(),
            "running"
        );
    }
}
