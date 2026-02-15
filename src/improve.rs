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

/// Handle the `improve show` subcommand.
pub fn handle_show(db_path: &Path, ref_id: &str) -> Result<(), String> {
    let conn = db::open_or_create(db_path).map_err(|e| format!("Failed to open database: {e}"))?;
    let imp = db::get_improvement(&conn, ref_id)
        .map_err(|e| format!("Failed to query improvement: {e}"))?;

    match imp {
        Some(imp) => {
            println!("Ref:      {}", imp.ref_id);
            println!("Title:    {}", imp.title);
            println!("Status:   {}", imp.status);
            println!("Category: {}", imp.category);
            println!("Created:  {}", imp.created);
            if let Some(body) = &imp.body {
                println!("Body:     {body}");
            }
            if let Some(context) = &imp.context {
                println!("Context:  {context}");
            }
            if let Some(tags) = &imp.tags {
                println!("Tags:     {tags}");
            }
            // Show meta if present
            let meta = db::get_improvement_meta(&conn, ref_id)
                .map_err(|e| format!("Failed to read meta: {e}"))?;
            if let Some(meta) = meta {
                println!("Meta:     {meta}");
            }
            Ok(())
        }
        None => Err(format!("No improvement found with ref '{ref_id}'")),
    }
}

/// Handle the `improve update` subcommand.
pub fn handle_update(
    db_path: &Path,
    ref_id: &str,
    status: Option<&str>,
    body: Option<&str>,
    context: Option<&str>,
) -> Result<(), String> {
    let conn = db::open_or_create(db_path).map_err(|e| format!("Failed to open database: {e}"))?;
    let updated = db::update_improvement(&conn, ref_id, status, body, context, None)
        .map_err(|e| format!("Failed to update improvement: {e}"))?;

    if updated {
        println!("Updated {ref_id}");
        Ok(())
    } else {
        Err(format!("No improvement found with ref '{ref_id}'"))
    }
}

/// Handle the `improve promote` subcommand (shorthand for status=promoted).
pub fn handle_promote(db_path: &Path, ref_id: &str) -> Result<(), String> {
    let conn = db::open_or_create(db_path).map_err(|e| format!("Failed to open database: {e}"))?;
    let updated = db::update_improvement(&conn, ref_id, Some("promoted"), None, None, None)
        .map_err(|e| format!("Failed to promote improvement: {e}"))?;

    if updated {
        println!("Promoted {ref_id}");
        Ok(())
    } else {
        Err(format!("No improvement found with ref '{ref_id}'"))
    }
}

/// Handle the `improve dismiss` subcommand (shorthand for status=dismissed with reason in meta).
pub fn handle_dismiss(db_path: &Path, ref_id: &str, reason: Option<&str>) -> Result<(), String> {
    let conn = db::open_or_create(db_path).map_err(|e| format!("Failed to open database: {e}"))?;

    let meta = reason.map(|r| format!(r#"{{"dismiss_reason": "{r}"}}"#));

    let updated = db::update_improvement(
        &conn,
        ref_id,
        Some("dismissed"),
        None,
        None,
        meta.as_deref(),
    )
    .map_err(|e| format!("Failed to dismiss improvement: {e}"))?;

    if updated {
        println!("Dismissed {ref_id}");
        Ok(())
    } else {
        Err(format!("No improvement found with ref '{ref_id}'"))
    }
}

/// Handle the `improve search` subcommand.
pub fn handle_search(db_path: &Path, query: &str) -> Result<(), String> {
    let conn = db::open_or_create(db_path).map_err(|e| format!("Failed to open database: {e}"))?;
    let results = db::search_improvements(&conn, query)
        .map_err(|e| format!("Failed to search improvements: {e}"))?;

    if results.is_empty() {
        println!("No improvements matching '{query}'.");
        return Ok(());
    }

    println!(
        "{:<6} {:<12} {:<14} {:<10} TITLE",
        "REF", "STATUS", "CATEGORY", "CREATED"
    );
    println!("{}", "-".repeat(72));

    for imp in &results {
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

    println!("\n{} result(s)", results.len());
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

    // ── show tests ──────────────────────────────────────────────────────

    #[test]
    fn show_existing_improvement() {
        let (_dir, path) = test_db_path();
        handle_add(
            &path,
            "Show me",
            "workflow",
            Some("body"),
            Some("ctx"),
            Some("t1"),
        )
        .unwrap();
        // Should succeed without error
        handle_show(&path, "R1").unwrap();
    }

    #[test]
    fn show_nonexistent_returns_error() {
        let (_dir, path) = test_db_path();
        // Ensure db exists
        let _conn = db::open_or_create(&path).unwrap();
        let result = handle_show(&path, "R999");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("R999"));
    }

    // ── update tests ────────────────────────────────────────────────────

    #[test]
    fn update_status() {
        let (_dir, path) = test_db_path();
        handle_add(&path, "To update", "workflow", None, None, None).unwrap();
        handle_update(&path, "R1", Some("validated"), None, None).unwrap();

        let conn = db::open_or_create(&path).unwrap();
        let imp = db::get_improvement(&conn, "R1").unwrap().unwrap();
        assert_eq!(imp.status, "validated");
    }

    #[test]
    fn update_body_and_context() {
        let (_dir, path) = test_db_path();
        handle_add(&path, "To update", "workflow", None, None, None).unwrap();
        handle_update(&path, "R1", None, Some("new body"), Some("new context")).unwrap();

        let conn = db::open_or_create(&path).unwrap();
        let imp = db::get_improvement(&conn, "R1").unwrap().unwrap();
        assert_eq!(imp.body.as_deref(), Some("new body"));
        assert_eq!(imp.context.as_deref(), Some("new context"));
    }

    #[test]
    fn update_nonexistent_returns_error() {
        let (_dir, path) = test_db_path();
        let _conn = db::open_or_create(&path).unwrap();
        let result = handle_update(&path, "R999", Some("open"), None, None);
        assert!(result.is_err());
    }

    // ── promote tests ───────────────────────────────────────────────────

    #[test]
    fn promote_sets_status() {
        let (_dir, path) = test_db_path();
        handle_add(&path, "To promote", "cost", None, None, None).unwrap();
        handle_promote(&path, "R1").unwrap();

        let conn = db::open_or_create(&path).unwrap();
        let imp = db::get_improvement(&conn, "R1").unwrap().unwrap();
        assert_eq!(imp.status, "promoted");
    }

    #[test]
    fn promote_sets_resolved_timestamp() {
        let (_dir, path) = test_db_path();
        handle_add(&path, "To promote", "cost", None, None, None).unwrap();
        handle_promote(&path, "R1").unwrap();

        let conn = db::open_or_create(&path).unwrap();
        let resolved: Option<String> = conn
            .query_row(
                "SELECT resolved FROM improvements WHERE ref = 'R1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(resolved.is_some());
    }

    #[test]
    fn promote_nonexistent_returns_error() {
        let (_dir, path) = test_db_path();
        let _conn = db::open_or_create(&path).unwrap();
        let result = handle_promote(&path, "R999");
        assert!(result.is_err());
    }

    // ── dismiss tests ───────────────────────────────────────────────────

    #[test]
    fn dismiss_sets_status() {
        let (_dir, path) = test_db_path();
        handle_add(&path, "To dismiss", "workflow", None, None, None).unwrap();
        handle_dismiss(&path, "R1", None).unwrap();

        let conn = db::open_or_create(&path).unwrap();
        let imp = db::get_improvement(&conn, "R1").unwrap().unwrap();
        assert_eq!(imp.status, "dismissed");
    }

    #[test]
    fn dismiss_with_reason_stores_meta() {
        let (_dir, path) = test_db_path();
        handle_add(&path, "To dismiss", "workflow", None, None, None).unwrap();
        handle_dismiss(&path, "R1", Some("not relevant")).unwrap();

        let conn = db::open_or_create(&path).unwrap();
        let meta = db::get_improvement_meta(&conn, "R1").unwrap();
        assert!(meta.is_some());
        assert!(meta.unwrap().contains("not relevant"));
    }

    #[test]
    fn dismiss_sets_resolved_timestamp() {
        let (_dir, path) = test_db_path();
        handle_add(&path, "To dismiss", "workflow", None, None, None).unwrap();
        handle_dismiss(&path, "R1", None).unwrap();

        let conn = db::open_or_create(&path).unwrap();
        let resolved: Option<String> = conn
            .query_row(
                "SELECT resolved FROM improvements WHERE ref = 'R1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(resolved.is_some());
    }

    #[test]
    fn dismiss_nonexistent_returns_error() {
        let (_dir, path) = test_db_path();
        let _conn = db::open_or_create(&path).unwrap();
        let result = handle_dismiss(&path, "R999", None);
        assert!(result.is_err());
    }

    // ── search tests ────────────────────────────────────────────────────

    #[test]
    fn search_by_title() {
        let (_dir, path) = test_db_path();
        handle_add(&path, "Reduce token usage", "cost", None, None, None).unwrap();
        handle_add(&path, "Fix retry logic", "reliability", None, None, None).unwrap();
        handle_search(&path, "token").unwrap();

        // Verify via DB that search would match
        let conn = db::open_or_create(&path).unwrap();
        let results = db::search_improvements(&conn, "token").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Reduce token usage");
    }

    #[test]
    fn search_by_body() {
        let (_dir, path) = test_db_path();
        handle_add(
            &path,
            "Some title",
            "workflow",
            Some("Parallel tool calls save turns"),
            None,
            None,
        )
        .unwrap();
        handle_add(&path, "Other", "cost", Some("Unrelated body"), None, None).unwrap();

        let conn = db::open_or_create(&path).unwrap();
        let results = db::search_improvements(&conn, "parallel").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Some title");
    }

    #[test]
    fn search_by_context() {
        let (_dir, path) = test_db_path();
        handle_add(
            &path,
            "Context match",
            "workflow",
            None,
            Some("sessions 340-348"),
            None,
        )
        .unwrap();

        let conn = db::open_or_create(&path).unwrap();
        let results = db::search_improvements(&conn, "340").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_no_results() {
        let (_dir, path) = test_db_path();
        handle_add(&path, "Something", "workflow", None, None, None).unwrap();
        // Should not error
        handle_search(&path, "nonexistent_xyz").unwrap();

        let conn = db::open_or_create(&path).unwrap();
        let results = db::search_improvements(&conn, "nonexistent_xyz").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_case_insensitive() {
        let (_dir, path) = test_db_path();
        handle_add(&path, "Token Usage", "cost", None, None, None).unwrap();

        let conn = db::open_or_create(&path).unwrap();
        let results = db::search_improvements(&conn, "token").unwrap();
        assert_eq!(results.len(), 1);
    }
}
