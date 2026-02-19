use crate::db;
use std::io::Read;
use std::path::Path;

fn resolve_content(body: Option<&str>, file: Option<&Path>, stdin: bool) -> Result<String, String> {
    let mut source_count = 0;
    if body.is_some() {
        source_count += 1;
    }
    if file.is_some() {
        source_count += 1;
    }
    if stdin {
        source_count += 1;
    }

    if source_count == 0 {
        return Err("Provide exactly one content source: --body, --file, or --stdin".to_string());
    }
    if source_count > 1 {
        return Err("Use only one content source: --body, --file, or --stdin".to_string());
    }

    if let Some(text) = body {
        return Ok(text.to_string());
    }

    if let Some(path) = file {
        return std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()));
    }

    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| format!("Failed to read stdin: {e}"))?;
    Ok(buf)
}

/// Handle `blacksmith progress add`.
pub fn handle_add(
    db_path: &Path,
    bead_id: Option<&str>,
    body: Option<&str>,
    file: Option<&Path>,
    stdin: bool,
) -> Result<(), String> {
    let content = resolve_content(body, file, stdin)?;
    if content.trim().is_empty() {
        return Err("Progress entry is empty.".to_string());
    }

    let conn = db::open_or_create(db_path).map_err(|e| format!("Failed to open database: {e}"))?;
    let id = db::insert_progress_entry(&conn, bead_id, &content)
        .map_err(|e| format!("Failed to save progress entry: {e}"))?;

    if bead_id.is_none() {
        eprintln!("Warning: missing bead ID; entry saved without bead association.");
    }
    println!("Saved progress entry {id}.");
    Ok(())
}

/// Handle `blacksmith progress list`.
pub fn handle_list(db_path: &Path, bead_id: Option<&str>, last: i64) -> Result<(), String> {
    if last <= 0 {
        return Err("--last must be greater than 0".to_string());
    }

    let conn = db::open_or_create(db_path).map_err(|e| format!("Failed to open database: {e}"))?;
    let rows = db::list_progress_entries(&conn, bead_id, last)
        .map_err(|e| format!("Failed to list progress entries: {e}"))?;

    if rows.is_empty() {
        match bead_id {
            Some(id) => println!("No progress entries found for bead '{id}'."),
            None => println!("No progress entries found."),
        }
        return Ok(());
    }

    println!("{:<6} {:<20} {:<26} PREVIEW", "ID", "BEAD", "CREATED");
    println!("{}", "-".repeat(90));
    for row in rows {
        let preview_line = row.content.lines().next().unwrap_or_default().trim();
        let preview = if preview_line.len() > 40 {
            format!("{}...", &preview_line[..40])
        } else {
            preview_line.to_string()
        };
        let bead = row.bead_id.unwrap_or_else(|| "(none)".to_string());
        println!("{:<6} {:<20} {:<26} {}", row.id, bead, row.created, preview);
    }

    Ok(())
}

/// Handle `blacksmith progress show`.
pub fn handle_show(db_path: &Path, bead_id: Option<&str>) -> Result<(), String> {
    let conn = db::open_or_create(db_path).map_err(|e| format!("Failed to open database: {e}"))?;
    let row = db::latest_progress_entry(&conn, bead_id)
        .map_err(|e| format!("Failed to query progress entries: {e}"))?;

    match row {
        Some(row) => {
            let bead = row.bead_id.unwrap_or_else(|| "(none)".to_string());
            println!("ID:      {}", row.id);
            println!("Bead:    {bead}");
            println!("Created: {}", row.created);
            println!("\n{}", row.content);
            Ok(())
        }
        None => {
            match bead_id {
                Some(id) => println!("No progress entries found for bead '{id}'."),
                None => println!("No progress entries found."),
            }
            Ok(())
        }
    }
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
    fn add_persists_multiline_body() {
        let (_dir, path) = test_db_path();
        handle_add(
            &path,
            Some("bd-1"),
            Some("## Handoff\n- Completed X\n- Next Y\n"),
            None,
            false,
        )
        .unwrap();

        let conn = db::open_or_create(&path).unwrap();
        let row = db::latest_progress_entry(&conn, Some("bd-1"))
            .unwrap()
            .unwrap();
        assert!(row.content.contains("## Handoff"));
        assert!(row.content.contains("- Next Y"));
    }

    #[test]
    fn add_without_bead_id_still_persists() {
        let (_dir, path) = test_db_path();
        handle_add(&path, None, Some("entry without bead id"), None, false).unwrap();

        let conn = db::open_or_create(&path).unwrap();
        let rows = db::list_progress_entries(&conn, None, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].bead_id.is_none());
        assert_eq!(rows[0].content, "entry without bead id");
    }

    #[test]
    fn add_requires_exactly_one_content_source() {
        let (_dir, path) = test_db_path();
        let err = handle_add(&path, Some("bd-1"), None, None, false).unwrap_err();
        assert!(err.contains("Provide exactly one content source"));

        let err = handle_add(
            &path,
            Some("bd-1"),
            Some("x"),
            Some(Path::new("x.md")),
            false,
        )
        .unwrap_err();
        assert!(err.contains("Use only one content source"));
    }
}
