use crate::db;
use std::io::Read;
use std::path::Path;

fn resolve_body(
    text: Option<&str>,
    use_stdin: bool,
    file: Option<&Path>,
) -> Result<String, String> {
    let mut sources = 0;
    if text.is_some() {
        sources += 1;
    }
    if use_stdin {
        sources += 1;
    }
    if file.is_some() {
        sources += 1;
    }
    if sources != 1 {
        return Err(
            "Provide exactly one body input source: use one of --text, --stdin, or --file"
                .to_string(),
        );
    }

    let body = if let Some(t) = text {
        t.to_string()
    } else if use_stdin {
        let mut content = String::new();
        std::io::stdin()
            .read_to_string(&mut content)
            .map_err(|e| format!("Failed to read from stdin: {e}"))?;
        content
    } else {
        let path = file.expect("file presence checked above");
        std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read progress body from {}: {e}", path.display()))?
    };

    if body.trim().is_empty() {
        return Err("Progress body is empty".to_string());
    }
    Ok(body)
}

/// Handle `blacksmith progress add`.
pub fn handle_add(
    db_path: &Path,
    bead_id: Option<&str>,
    text: Option<&str>,
    use_stdin: bool,
    file: Option<&Path>,
) -> Result<(), String> {
    let body = resolve_body(text, use_stdin, file)?;
    let conn = db::open_or_create(db_path).map_err(|e| format!("Failed to open database: {e}"))?;

    if bead_id.is_none() {
        eprintln!("Warning: bead ID missing; saving progress entry without bead association.");
    }

    let row_id = db::insert_progress_entry(&conn, bead_id, &body)
        .map_err(|e| format!("Failed to insert progress entry: {e}"))?;
    println!("Saved progress entry #{row_id}");
    Ok(())
}

/// Handle `blacksmith progress list`.
pub fn handle_list(db_path: &Path, bead_id: Option<&str>, last: i64) -> Result<(), String> {
    let conn = db::open_or_create(db_path).map_err(|e| format!("Failed to open database: {e}"))?;
    let entries = db::list_progress_entries(&conn, bead_id, last)
        .map_err(|e| format!("Failed to list progress entries: {e}"))?;

    if entries.is_empty() {
        println!("No progress entries found.");
        return Ok(());
    }

    println!("{:<6} {:<10} {:<24} PREVIEW", "ID", "BEAD", "CREATED");
    println!("{}", "-".repeat(96));
    for entry in &entries {
        let bead = entry.bead_id.as_deref().unwrap_or("-");
        let preview = entry.body.replace('\n', " ");
        let preview = if preview.chars().count() > 52 {
            format!("{}...", preview.chars().take(52).collect::<String>())
        } else {
            preview
        };
        println!(
            "{:<6} {:<10} {:<24} {}",
            entry.id, bead, entry.created, preview
        );
    }
    println!("\n{} progress entr(y/ies)", entries.len());
    Ok(())
}

/// Handle `blacksmith progress show`.
pub fn handle_show(db_path: &Path, bead_id: Option<&str>) -> Result<(), String> {
    let conn = db::open_or_create(db_path).map_err(|e| format!("Failed to open database: {e}"))?;
    let entry = db::latest_progress_entry(&conn, bead_id)
        .map_err(|e| format!("Failed to query progress entry: {e}"))?;

    let Some(entry) = entry else {
        match bead_id {
            Some(id) => println!("No progress entries found for bead {id}."),
            None => println!("No progress entries found."),
        }
        return Ok(());
    };

    println!("ID:      {}", entry.id);
    println!("Bead:    {}", entry.bead_id.as_deref().unwrap_or("-"));
    println!("Created: {}", entry.created);
    println!("\n{}", entry.body);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_db_path() -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("blacksmith.db");
        (dir, path)
    }

    #[test]
    fn resolve_body_rejects_multiple_sources() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("entry.md");
        std::fs::write(&file, "content").unwrap();

        let err = resolve_body(Some("text"), false, Some(&file)).unwrap_err();
        assert!(err.contains("exactly one"));
    }

    #[test]
    fn resolve_body_from_text() {
        let body = resolve_body(Some("## Handoff\n- done"), false, None).unwrap();
        assert!(body.contains("Handoff"));
    }

    #[test]
    fn resolve_body_from_file() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("entry.md");
        std::fs::write(&file, "## Handoff\n- from file\n").unwrap();

        let body = resolve_body(None, false, Some(&file)).unwrap();
        assert!(body.contains("from file"));
    }

    #[test]
    fn add_saves_entry_with_and_without_bead_id() {
        let (_dir, db_path) = test_db_path();
        handle_add(
            &db_path,
            Some("simple-agent-harness-c34r"),
            Some("## Handoff\n- Completed X\n"),
            false,
            None,
        )
        .unwrap();
        handle_add(&db_path, None, Some("entry without bead id"), false, None).unwrap();

        let conn = db::open_or_create(&db_path).unwrap();
        let all = db::list_progress_entries(&conn, None, 10).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all[0].bead_id.is_none());
        assert_eq!(all[1].bead_id.as_deref(), Some("simple-agent-harness-c34r"));
    }

    #[test]
    fn list_and_show_do_not_error() {
        let (_dir, db_path) = test_db_path();
        handle_add(
            &db_path,
            Some("simple-agent-harness-c34r"),
            Some("## Handoff\n- Next Y\n"),
            false,
            None,
        )
        .unwrap();

        handle_list(&db_path, Some("simple-agent-harness-c34r"), 5).unwrap();
        handle_show(&db_path, Some("simple-agent-harness-c34r")).unwrap();
    }
}
