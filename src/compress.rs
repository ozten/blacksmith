//! Session file compression using zstd.
//!
//! After each session completes and is ingested, compress sessions older than
//! `compress_after` iterations. Compressed files are named `{N}.jsonl.zst`.

use std::path::Path;

/// Compress old session files in the sessions directory.
///
/// Any `.jsonl` file whose iteration number is <= `current_iteration - compress_after`
/// is compressed to `.jsonl.zst` and the original removed.
/// Errors during compression of individual files are logged but do not stop processing.
pub fn compress_old_sessions(sessions_dir: &Path, current_iteration: u64, compress_after: u32) {
    if compress_after == 0 {
        return;
    }

    let threshold = current_iteration.saturating_sub(compress_after as u64);
    if current_iteration < compress_after as u64 {
        return;
    }

    let entries = match std::fs::read_dir(sessions_dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "failed to read sessions directory for compression");
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };

        // Only process uncompressed .jsonl files (not already .jsonl.zst)
        if !file_name.ends_with(".jsonl") || file_name.ends_with(".jsonl.zst") {
            continue;
        }

        // Extract iteration number from filename: "{N}.jsonl"
        let iteration: u64 = match file_name
            .strip_suffix(".jsonl")
            .and_then(|s| s.parse().ok())
        {
            Some(n) => n,
            None => continue,
        };

        if iteration <= threshold {
            if let Err(e) = compress_file(&path) {
                tracing::warn!(
                    error = %e,
                    file = %path.display(),
                    "failed to compress session file"
                );
            } else {
                tracing::debug!(
                    file = %path.display(),
                    iteration,
                    "compressed session file"
                );
            }
        }
    }
}

/// Compress a single file with zstd, writing to `{path}.zst` and removing the original.
fn compress_file(path: &Path) -> std::io::Result<()> {
    let dest = path.with_extension("jsonl.zst");
    let input = std::fs::read(path)?;
    let compressed = zstd::encode_all(input.as_slice(), 3)?;
    std::fs::write(&dest, compressed)?;
    std::fs::remove_file(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_compress_old_sessions_basic() {
        let dir = tempdir().unwrap();
        let sessions = dir.path();

        // Create session files 0-9
        for i in 0..10 {
            std::fs::write(sessions.join(format!("{i}.jsonl")), format!("data {i}")).unwrap();
        }

        // Current iteration 9, compress_after 5 → threshold = 4
        // Sessions 0,1,2,3,4 should be compressed
        compress_old_sessions(sessions, 9, 5);

        for i in 0..=4 {
            assert!(
                !sessions.join(format!("{i}.jsonl")).exists(),
                "session {i}.jsonl should be removed"
            );
            assert!(
                sessions.join(format!("{i}.jsonl.zst")).exists(),
                "session {i}.jsonl.zst should exist"
            );
        }
        for i in 5..10 {
            assert!(
                sessions.join(format!("{i}.jsonl")).exists(),
                "session {i}.jsonl should still exist"
            );
            assert!(
                !sessions.join(format!("{i}.jsonl.zst")).exists(),
                "session {i}.jsonl.zst should not exist"
            );
        }
    }

    #[test]
    fn test_compressed_file_is_valid_zstd() {
        let dir = tempdir().unwrap();
        let sessions = dir.path();
        let original_data = "hello world\nline 2\n";
        std::fs::write(sessions.join("0.jsonl"), original_data).unwrap();

        compress_old_sessions(sessions, 5, 3);

        let compressed = std::fs::read(sessions.join("0.jsonl.zst")).unwrap();
        let decompressed = zstd::decode_all(compressed.as_slice()).unwrap();
        assert_eq!(String::from_utf8(decompressed).unwrap(), original_data);
    }

    #[test]
    fn test_compress_after_zero_does_nothing() {
        let dir = tempdir().unwrap();
        let sessions = dir.path();
        std::fs::write(sessions.join("0.jsonl"), "data").unwrap();

        compress_old_sessions(sessions, 10, 0);

        assert!(sessions.join("0.jsonl").exists());
        assert!(!sessions.join("0.jsonl.zst").exists());
    }

    #[test]
    fn test_compress_current_less_than_threshold() {
        let dir = tempdir().unwrap();
        let sessions = dir.path();
        std::fs::write(sessions.join("0.jsonl"), "data").unwrap();

        // current_iteration (2) < compress_after (5) → nothing to compress
        compress_old_sessions(sessions, 2, 5);

        assert!(sessions.join("0.jsonl").exists());
    }

    #[test]
    fn test_already_compressed_files_skipped() {
        let dir = tempdir().unwrap();
        let sessions = dir.path();
        std::fs::write(sessions.join("0.jsonl.zst"), "already compressed").unwrap();

        compress_old_sessions(sessions, 10, 3);

        // Should not touch already-compressed files
        let contents = std::fs::read_to_string(sessions.join("0.jsonl.zst")).unwrap();
        assert_eq!(contents, "already compressed");
    }

    #[test]
    fn test_non_numeric_files_ignored() {
        let dir = tempdir().unwrap();
        let sessions = dir.path();
        std::fs::write(sessions.join("notes.jsonl"), "some notes").unwrap();
        std::fs::write(sessions.join("0.jsonl"), "data").unwrap();

        compress_old_sessions(sessions, 10, 3);

        // notes.jsonl should be untouched (non-numeric prefix)
        assert!(sessions.join("notes.jsonl").exists());
        // 0.jsonl should be compressed
        assert!(!sessions.join("0.jsonl").exists());
        assert!(sessions.join("0.jsonl.zst").exists());
    }

    #[test]
    fn test_empty_sessions_directory() {
        let dir = tempdir().unwrap();
        compress_old_sessions(dir.path(), 10, 3);
        // No panic, no files created
    }

    #[test]
    fn test_missing_sessions_directory() {
        let path = Path::new("/nonexistent/sessions");
        compress_old_sessions(path, 10, 3);
        // Should log warning but not panic
    }

    #[test]
    fn test_compress_exact_threshold() {
        let dir = tempdir().unwrap();
        let sessions = dir.path();
        // threshold = 10 - 5 = 5
        // Session 5 is AT threshold → should be compressed (<=)
        // Session 6 is ABOVE threshold → should not be compressed
        std::fs::write(sessions.join("5.jsonl"), "data5").unwrap();
        std::fs::write(sessions.join("6.jsonl"), "data6").unwrap();

        compress_old_sessions(sessions, 10, 5);

        assert!(!sessions.join("5.jsonl").exists());
        assert!(sessions.join("5.jsonl.zst").exists());
        assert!(sessions.join("6.jsonl").exists());
    }

    #[test]
    fn test_compress_high_iteration_numbers() {
        let dir = tempdir().unwrap();
        let sessions = dir.path();
        std::fs::write(sessions.join("100.jsonl"), "data100").unwrap();
        std::fs::write(sessions.join("105.jsonl"), "data105").unwrap();

        // current=110, compress_after=5, threshold=105
        compress_old_sessions(sessions, 110, 5);

        assert!(!sessions.join("100.jsonl").exists());
        assert!(sessions.join("100.jsonl.zst").exists());
        // 105 is at threshold → compressed
        assert!(!sessions.join("105.jsonl").exists());
        assert!(sessions.join("105.jsonl.zst").exists());
    }
}
