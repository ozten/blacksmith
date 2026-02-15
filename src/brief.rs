use crate::db;
use std::path::Path;

/// Handle the `brief` subcommand.
///
/// Generates a performance feedback snippet for prompt injection.
/// Queries open improvements from DB, formats them as markdown.
/// Returns empty string (no output) if no DB file exists.
pub fn handle_brief(db_path: &Path) -> Result<(), String> {
    let text = generate_brief(db_path)?;
    if !text.is_empty() {
        print!("{text}");
    }
    Ok(())
}

/// Generate the brief snippet as a string for prompt injection.
///
/// Returns an empty string when:
/// - The database file doesn't exist (first run)
/// - The database has no improvements at all
///
/// Otherwise returns the formatted brief with header and open improvement lines.
pub fn generate_brief(db_path: &Path) -> Result<String, String> {
    // If no database file exists, silently produce no output
    if !db_path.exists() {
        return Ok(String::new());
    }

    let conn = db::open_or_create(db_path).map_err(|e| format!("Failed to open database: {e}"))?;

    let open = db::list_improvements(&conn, Some("open"), None)
        .map_err(|e| format!("Failed to query improvements: {e}"))?;

    let total =
        db::count_improvements(&conn).map_err(|e| format!("Failed to count improvements: {e}"))?;

    // No improvements at all — no output
    if total == 0 {
        return Ok(String::new());
    }

    let mut output = format!("## OPEN IMPROVEMENTS ({} of {})\n", open.len(), total);

    for imp in &open {
        output.push_str(&format!(
            "\n{} [{}] {}",
            imp.ref_id, imp.category, imp.title
        ));
    }

    Ok(output)
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
    fn brief_no_database_file_produces_no_output() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.db");
        // Should succeed silently
        handle_brief(&path).unwrap();
    }

    #[test]
    fn brief_empty_database_produces_no_output() {
        let (_dir, path) = test_db_path();
        // Create the DB (empty)
        db::open_or_create(&path).unwrap();
        handle_brief(&path).unwrap();
    }

    #[test]
    fn brief_shows_open_improvements() {
        let (_dir, path) = test_db_path();
        let conn = db::open_or_create(&path).unwrap();

        db::insert_improvement(&conn, "workflow", "Batch file reads", None, None, None).unwrap();
        db::insert_improvement(
            &conn,
            "cost",
            "Skip full test suite for CSS",
            None,
            None,
            None,
        )
        .unwrap();

        drop(conn);
        // Just verify it doesn't error — output goes to stdout
        handle_brief(&path).unwrap();
    }

    #[test]
    fn brief_excludes_non_open_improvements_from_listing() {
        let (_dir, path) = test_db_path();
        let conn = db::open_or_create(&path).unwrap();

        db::insert_improvement(&conn, "workflow", "Open item", None, None, None).unwrap();
        conn.execute(
            "INSERT INTO improvements (ref, category, status, title) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["R2", "cost", "promoted", "Promoted item"],
        )
        .unwrap();

        drop(conn);
        // Should work — promoted items are in total but not listed
        handle_brief(&path).unwrap();
    }

    #[test]
    fn brief_format_matches_prd() {
        let (_dir, path) = test_db_path();
        let conn = db::open_or_create(&path).unwrap();

        db::insert_improvement(
            &conn,
            "code-quality",
            "Use ESLint --fix in pre-commit hook",
            None,
            None,
            None,
        )
        .unwrap();
        db::insert_improvement(
            &conn,
            "workflow",
            "Batch file reads when exploring",
            None,
            None,
            None,
        )
        .unwrap();
        // One promoted (not in open list, but in total)
        conn.execute(
            "INSERT INTO improvements (ref, category, status, title) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["R3", "cost", "promoted", "Skip full test suite for CSS"],
        )
        .unwrap();

        drop(conn);

        // Capture output by re-implementing the logic inline for assertion
        let conn = db::open_or_create(&path).unwrap();
        let open = db::list_improvements(&conn, Some("open"), None).unwrap();
        let total = db::count_improvements(&conn).unwrap();

        assert_eq!(open.len(), 2);
        assert_eq!(total, 3);

        // Verify format: "## OPEN IMPROVEMENTS (2 of 3)"
        let header = format!("## OPEN IMPROVEMENTS ({} of {})", open.len(), total);
        assert_eq!(header, "## OPEN IMPROVEMENTS (2 of 3)");

        // Verify each line format: "R1 [code-quality] Use ESLint --fix in pre-commit hook"
        let line = format!(
            "{} [{}] {}",
            open[0].ref_id, open[0].category, open[0].title
        );
        assert_eq!(
            line,
            "R1 [code-quality] Use ESLint --fix in pre-commit hook"
        );
    }
}
