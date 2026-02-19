use crate::db;
use std::io::Read;
use std::path::Path;

/// Handle the `progress add` subcommand.
pub fn handle_add(
    db_path: &Path,
    bead_id: Option<&str>,
    body: Option<&str>,
    from_stdin: bool,
    file: Option<&Path>,
) -> Result<(), String> {
    let content = read_entry_content(body, from_stdin, file)?;

    if content.trim().is_empty() {
        return Err("Progress entry is empty".to_string());
    }

    let conn = db::open_or_create(db_path).map_err(|e| format!("Failed to open database: {e}"))?;
    let entry_id = db::insert_progress_entry(&conn, bead_id, &content)
        .map_err(|e| format!("Failed to insert progress entry: {e}"))?;

    if bead_id.is_none() {
        eprintln!("Warning: --bead-id not provided; entry stored without bead association.");
    }

    println!("Saved progress entry #{entry_id}");
    Ok(())
}

/// Handle the `progress list` subcommand.
pub fn handle_list(db_path: &Path, bead_id: Option<&str>, last: i64) -> Result<(), String> {
    let conn = db::open_or_create(db_path).map_err(|e| format!("Failed to open database: {e}"))?;
    let entries = db::list_progress_entries(&conn, bead_id, last)
        .map_err(|e| format!("Failed to list progress entries: {e}"))?;

    if entries.is_empty() {
        println!("No progress entries found.");
        return Ok(());
    }

    println!("{:<6} {:<24} {:<28} PREVIEW", "ID", "BEAD", "CREATED");
    println!("{}", "-".repeat(96));

    for entry in &entries {
        let bead = entry.bead_id.as_deref().unwrap_or("(none)");
        let preview = entry
            .body
            .lines()
            .find(|line| !line.trim().is_empty())
            .map(str::trim)
            .unwrap_or("(empty)");
        println!(
            "{:<6} {:<24} {:<28} {}",
            entry.id,
            bead,
            entry.created,
            truncate_preview(preview, 80)
        );
    }

    println!(
        "\n{} entr{}.",
        entries.len(),
        if entries.len() == 1 { "y" } else { "ies" }
    );
    Ok(())
}

/// Handle the `progress show` subcommand.
pub fn handle_show(db_path: &Path, bead_id: Option<&str>) -> Result<(), String> {
    let conn = db::open_or_create(db_path).map_err(|e| format!("Failed to open database: {e}"))?;
    let entry = db::latest_progress_entry(&conn, bead_id)
        .map_err(|e| format!("Failed to query progress entry: {e}"))?;

    let Some(entry) = entry else {
        println!("No progress entry found.");
        return Ok(());
    };

    println!("ID:      {}", entry.id);
    println!("Bead:    {}", entry.bead_id.as_deref().unwrap_or("(none)"));
    println!("Created: {}", entry.created);
    println!("\n{}", entry.body);
    Ok(())
}

fn read_entry_content(
    body: Option<&str>,
    from_stdin: bool,
    file: Option<&Path>,
) -> Result<String, String> {
    let selected = [body.is_some(), from_stdin, file.is_some()]
        .into_iter()
        .filter(|flag| *flag)
        .count();

    if selected == 0 {
        return Err("Provide one of: --body, --stdin, or --file <path>".to_string());
    }
    if selected > 1 {
        return Err("Use only one input source: --body, --stdin, or --file".to_string());
    }

    if let Some(text) = body {
        return Ok(text.to_string());
    }

    if from_stdin {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("Failed reading stdin: {e}"))?;
        return Ok(buf);
    }

    let path = file.expect("validated file source");
    std::fs::read_to_string(path).map_err(|e| format!("Failed to read {}: {e}", path.display()))
}

fn truncate_preview(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }

    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push_str("...");
    out
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
    fn add_and_show_latest_for_bead() {
        let (_dir, path) = test_db_path();

        handle_add(
            &path,
            Some("simple-agent-harness-c34r"),
            Some("## Handoff\n- Completed X\n- Next Y\n"),
            false,
            None,
        )
        .unwrap();

        let conn = db::open_or_create(&path).unwrap();
        let latest = db::latest_progress_entry(&conn, Some("simple-agent-harness-c34r"))
            .unwrap()
            .unwrap();
        assert_eq!(latest.bead_id.as_deref(), Some("simple-agent-harness-c34r"));
        assert!(latest.body.contains("Completed X"));
    }

    #[test]
    fn add_without_bead_id_persists() {
        let (_dir, path) = test_db_path();

        handle_add(&path, None, Some("entry without bead id"), false, None).unwrap();

        let conn = db::open_or_create(&path).unwrap();
        let latest = db::latest_progress_entry(&conn, None).unwrap().unwrap();
        assert!(latest.bead_id.is_none());
        assert_eq!(latest.body, "entry without bead id");
    }

    #[test]
    fn list_obeys_last_limit() {
        let (_dir, path) = test_db_path();

        for i in 0..3 {
            handle_add(
                &path,
                Some("simple-agent-harness-c34r"),
                Some(&format!("entry {i}")),
                false,
                None,
            )
            .unwrap();
        }

        let conn = db::open_or_create(&path).unwrap();
        let rows = db::list_progress_entries(&conn, Some("simple-agent-harness-c34r"), 2).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].body, "entry 2");
        assert_eq!(rows[1].body, "entry 1");
    }

    #[test]
    fn empty_content_is_rejected() {
        let (_dir, path) = test_db_path();

        let err = handle_add(
            &path,
            Some("simple-agent-harness-c34r"),
            Some("\n\n"),
            false,
            None,
        )
        .unwrap_err();
        assert!(err.contains("empty"));
    }
}
