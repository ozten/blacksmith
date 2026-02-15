use crate::db;
use std::path::Path;

/// Handle the `improve add` subcommand.
pub fn handle_add(
    db_path: &Path,
    title: &str,
    category: &str,
    body: Option<&str>,
    context: Option<&str>,
    tags: Option<&str>,
) -> Result<(), String> {
    let conn = db::open_or_create(db_path).map_err(|e| format!("Failed to open database: {e}"))?;
    let ref_id = db::insert_improvement(&conn, category, title, body, context, tags)
        .map_err(|e| format!("Failed to insert improvement: {e}"))?;
    println!("Created improvement {ref_id}: {title}");
    Ok(())
}

/// Handle the `improve list` subcommand.
pub fn handle_list(
    db_path: &Path,
    status: Option<&str>,
    category: Option<&str>,
) -> Result<(), String> {
    let conn = db::open_or_create(db_path).map_err(|e| format!("Failed to open database: {e}"))?;
    let improvements = db::list_improvements(&conn, status, category)
        .map_err(|e| format!("Failed to list improvements: {e}"))?;

    if improvements.is_empty() {
        println!("No improvements found.");
        return Ok(());
    }

    // Print table header
    println!(
        "{:<6} {:<12} {:<14} {:<10} TITLE",
        "REF", "STATUS", "CATEGORY", "CREATED"
    );
    println!("{}", "-".repeat(72));

    for imp in &improvements {
        // Truncate created to date only (first 10 chars of ISO timestamp)
        let date = if imp.created.len() >= 10 {
            &imp.created[..10]
        } else {
            &imp.created
        };
        println!(
            "{:<6} {:<12} {:<14} {:<10} {}",
            imp.ref_id, imp.status, imp.category, date, imp.title
        );
    }

    println!("\n{} improvement(s)", improvements.len());
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
    fn add_creates_improvement() {
        let (_dir, path) = test_db_path();
        handle_add(&path, "Test title", "workflow", None, None, None).unwrap();

        let conn = db::open_or_create(&path).unwrap();
        let items = db::list_improvements(&conn, None, None).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].ref_id, "R1");
        assert_eq!(items[0].title, "Test title");
        assert_eq!(items[0].category, "workflow");
        assert_eq!(items[0].status, "open");
    }

    #[test]
    fn add_with_all_fields() {
        let (_dir, path) = test_db_path();
        handle_add(
            &path,
            "Full record",
            "cost",
            Some("Detailed body text"),
            Some("sessions 1-5"),
            Some("tag1,tag2"),
        )
        .unwrap();

        let conn = db::open_or_create(&path).unwrap();
        let items = db::list_improvements(&conn, None, None).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].body.as_deref(), Some("Detailed body text"));
        assert_eq!(items[0].context.as_deref(), Some("sessions 1-5"));
        assert_eq!(items[0].tags.as_deref(), Some("tag1,tag2"));
    }

    #[test]
    fn add_multiple_increments_ref() {
        let (_dir, path) = test_db_path();
        handle_add(&path, "First", "workflow", None, None, None).unwrap();
        handle_add(&path, "Second", "cost", None, None, None).unwrap();
        handle_add(&path, "Third", "reliability", None, None, None).unwrap();

        let conn = db::open_or_create(&path).unwrap();
        let items = db::list_improvements(&conn, None, None).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].ref_id, "R1");
        assert_eq!(items[1].ref_id, "R2");
        assert_eq!(items[2].ref_id, "R3");
    }

    #[test]
    fn list_empty_database() {
        let (_dir, path) = test_db_path();
        // Should not error on empty db
        handle_list(&path, None, None).unwrap();
    }

    #[test]
    fn list_filters_by_status() {
        let (_dir, path) = test_db_path();
        let conn = db::open_or_create(&path).unwrap();

        // Insert items with different statuses
        db::insert_improvement(&conn, "workflow", "Open item", None, None, None).unwrap();
        conn.execute(
            "INSERT INTO improvements (ref, category, status, title) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["R2", "cost", "promoted", "Promoted item"],
        )
        .unwrap();

        let open = db::list_improvements(&conn, Some("open"), None).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].title, "Open item");

        let promoted = db::list_improvements(&conn, Some("promoted"), None).unwrap();
        assert_eq!(promoted.len(), 1);
        assert_eq!(promoted[0].title, "Promoted item");
    }

    #[test]
    fn list_filters_by_category() {
        let (_dir, path) = test_db_path();
        let conn = db::open_or_create(&path).unwrap();

        db::insert_improvement(&conn, "workflow", "Workflow item", None, None, None).unwrap();
        db::insert_improvement(&conn, "cost", "Cost item", None, None, None).unwrap();

        let workflow = db::list_improvements(&conn, None, Some("workflow")).unwrap();
        assert_eq!(workflow.len(), 1);
        assert_eq!(workflow[0].title, "Workflow item");
    }

    #[test]
    fn list_filters_by_status_and_category() {
        let (_dir, path) = test_db_path();
        let conn = db::open_or_create(&path).unwrap();

        db::insert_improvement(&conn, "workflow", "Open workflow", None, None, None).unwrap();
        db::insert_improvement(&conn, "cost", "Open cost", None, None, None).unwrap();
        conn.execute(
            "INSERT INTO improvements (ref, category, status, title) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["R3", "workflow", "promoted", "Promoted workflow"],
        )
        .unwrap();

        let result = db::list_improvements(&conn, Some("open"), Some("workflow")).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Open workflow");
    }
}
