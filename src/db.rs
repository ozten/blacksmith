use rusqlite::{Connection, Result};
use std::path::Path;

/// Opens (or creates) the blacksmith SQLite database at the given path.
///
/// Creates the improvements table and indexes if they don't already exist.
/// Returns an open connection ready for use.
pub fn open_or_create(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;

    // Enable WAL mode for better concurrent read performance
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS improvements (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            ref        TEXT UNIQUE,
            created    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            resolved   TEXT,
            category   TEXT NOT NULL,
            status     TEXT NOT NULL DEFAULT 'open',
            title      TEXT NOT NULL,
            body       TEXT,
            context    TEXT,
            tags       TEXT,
            meta       TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_improvements_status ON improvements(status);
        CREATE INDEX IF NOT EXISTS idx_improvements_category ON improvements(category);",
    )?;

    Ok(conn)
}

/// Assigns the next auto-increment ref (R1, R2, ...) for a new improvement.
///
/// Reads the current max ref number from the table and returns the next one.
pub fn next_ref(conn: &Connection) -> Result<String> {
    let max_num: Option<i64> = conn.query_row(
        "SELECT MAX(CAST(SUBSTR(ref, 2) AS INTEGER)) FROM improvements WHERE ref LIKE 'R%'",
        [],
        |row| row.get(0),
    )?;
    let next = max_num.unwrap_or(0) + 1;
    Ok(format!("R{next}"))
}

/// Insert a new improvement record, auto-assigning the next ref.
/// Returns the assigned ref (e.g. "R1").
pub fn insert_improvement(
    conn: &Connection,
    category: &str,
    title: &str,
    body: Option<&str>,
    context: Option<&str>,
    tags: Option<&str>,
) -> Result<String> {
    let ref_id = next_ref(conn)?;
    conn.execute(
        "INSERT INTO improvements (ref, category, title, body, context, tags) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![ref_id, category, title, body, context, tags],
    )?;
    Ok(ref_id)
}

/// A row from the improvements table.
#[derive(Debug)]
pub struct Improvement {
    pub ref_id: String,
    pub created: String,
    pub category: String,
    pub status: String,
    pub title: String,
    pub body: Option<String>,
    pub context: Option<String>,
    pub tags: Option<String>,
}

/// List improvements with optional status and category filters.
pub fn list_improvements(
    conn: &Connection,
    status: Option<&str>,
    category: Option<&str>,
) -> Result<Vec<Improvement>> {
    let mut sql =
        "SELECT ref, created, category, status, title, body, context, tags FROM improvements"
            .to_string();
    let mut conditions = Vec::new();

    if status.is_some() {
        conditions.push("status = ?1");
    }
    if category.is_some() {
        conditions.push(if status.is_some() {
            "category = ?2"
        } else {
            "category = ?1"
        });
    }

    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" ORDER BY id ASC");

    let mut stmt = conn.prepare(&sql)?;

    let rows = match (status, category) {
        (Some(s), Some(c)) => {
            let iter = stmt.query_map(rusqlite::params![s, c], map_improvement)?;
            iter.collect::<Result<Vec<_>>>()?
        }
        (Some(s), None) => {
            let iter = stmt.query_map(rusqlite::params![s], map_improvement)?;
            iter.collect::<Result<Vec<_>>>()?
        }
        (None, Some(c)) => {
            let iter = stmt.query_map(rusqlite::params![c], map_improvement)?;
            iter.collect::<Result<Vec<_>>>()?
        }
        (None, None) => {
            let iter = stmt.query_map([], map_improvement)?;
            iter.collect::<Result<Vec<_>>>()?
        }
    };

    Ok(rows)
}

fn map_improvement(row: &rusqlite::Row) -> Result<Improvement> {
    Ok(Improvement {
        ref_id: row.get(0)?,
        created: row.get(1)?,
        category: row.get(2)?,
        status: row.get(3)?,
        title: row.get(4)?,
        body: row.get(5)?,
        context: row.get(6)?,
        tags: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use tempfile::TempDir;

    fn test_db() -> (TempDir, Connection) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("blacksmith.db");
        let conn = open_or_create(&path).unwrap();
        (dir, conn)
    }

    #[test]
    fn creates_database_and_table() {
        let (_dir, conn) = test_db();

        // Verify improvements table exists by querying it
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM improvements", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn idempotent_creation() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("blacksmith.db");

        // Open twice — should not error
        let conn1 = open_or_create(&path).unwrap();
        drop(conn1);
        let conn2 = open_or_create(&path).unwrap();

        let count: i64 = conn2
            .query_row("SELECT COUNT(*) FROM improvements", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn insert_and_query_improvement() {
        let (_dir, conn) = test_db();

        conn.execute(
            "INSERT INTO improvements (ref, category, title) VALUES (?1, ?2, ?3)",
            params!["R1", "workflow", "Use parallel tool calls"],
        )
        .unwrap();

        let title: String = conn
            .query_row(
                "SELECT title FROM improvements WHERE ref = 'R1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(title, "Use parallel tool calls");
    }

    #[test]
    fn default_status_is_open() {
        let (_dir, conn) = test_db();

        conn.execute(
            "INSERT INTO improvements (ref, category, title) VALUES (?1, ?2, ?3)",
            params!["R1", "cost", "Reduce token usage"],
        )
        .unwrap();

        let status: String = conn
            .query_row(
                "SELECT status FROM improvements WHERE ref = 'R1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "open");
    }

    #[test]
    fn created_timestamp_auto_set() {
        let (_dir, conn) = test_db();

        conn.execute(
            "INSERT INTO improvements (ref, category, title) VALUES (?1, ?2, ?3)",
            params!["R1", "reliability", "Add retry logic"],
        )
        .unwrap();

        let created: String = conn
            .query_row(
                "SELECT created FROM improvements WHERE ref = 'R1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        // Should be a valid ISO timestamp
        assert!(created.contains('T'));
        assert!(created.ends_with('Z'));
    }

    #[test]
    fn ref_uniqueness_enforced() {
        let (_dir, conn) = test_db();

        conn.execute(
            "INSERT INTO improvements (ref, category, title) VALUES (?1, ?2, ?3)",
            params!["R1", "workflow", "First"],
        )
        .unwrap();

        let result = conn.execute(
            "INSERT INTO improvements (ref, category, title) VALUES (?1, ?2, ?3)",
            params!["R1", "cost", "Duplicate"],
        );
        assert!(result.is_err());
    }

    #[test]
    fn next_ref_empty_table() {
        let (_dir, conn) = test_db();
        assert_eq!(next_ref(&conn).unwrap(), "R1");
    }

    #[test]
    fn next_ref_after_inserts() {
        let (_dir, conn) = test_db();

        conn.execute(
            "INSERT INTO improvements (ref, category, title) VALUES (?1, ?2, ?3)",
            params!["R1", "workflow", "First"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO improvements (ref, category, title) VALUES (?1, ?2, ?3)",
            params!["R3", "cost", "Third"],
        )
        .unwrap();

        // Should be R4 (max is R3)
        assert_eq!(next_ref(&conn).unwrap(), "R4");
    }

    #[test]
    fn next_ref_handles_gaps() {
        let (_dir, conn) = test_db();

        conn.execute(
            "INSERT INTO improvements (ref, category, title) VALUES (?1, ?2, ?3)",
            params!["R5", "performance", "Skip ahead"],
        )
        .unwrap();

        assert_eq!(next_ref(&conn).unwrap(), "R6");
    }

    #[test]
    fn all_columns_insertable() {
        let (_dir, conn) = test_db();

        conn.execute(
            "INSERT INTO improvements (ref, category, status, title, body, context, tags, meta)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                "R1",
                "code-quality",
                "promoted",
                "Enforce clippy lints",
                "Run clippy --fix as part of CI",
                "Sessions 10-15 had repeated lint warnings",
                "ci,lint,quality",
                r#"{"bead_id": "beads-abc"}"#,
            ],
        )
        .unwrap();

        let (body, context, tags, meta): (String, String, String, String) = conn
            .query_row(
                "SELECT body, context, tags, meta FROM improvements WHERE ref = 'R1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();

        assert_eq!(body, "Run clippy --fix as part of CI");
        assert_eq!(context, "Sessions 10-15 had repeated lint warnings");
        assert_eq!(tags, "ci,lint,quality");
        assert!(meta.contains("bead_id"));
    }

    #[test]
    fn index_on_status_works() {
        let (_dir, conn) = test_db();

        for i in 1..=5 {
            let status = if i <= 3 { "open" } else { "promoted" };
            conn.execute(
                "INSERT INTO improvements (ref, category, status, title) VALUES (?1, ?2, ?3, ?4)",
                params![format!("R{i}"), "workflow", status, format!("Item {i}")],
            )
            .unwrap();
        }

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM improvements WHERE status = 'open'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn resolved_timestamp_nullable() {
        let (_dir, conn) = test_db();

        conn.execute(
            "INSERT INTO improvements (ref, category, title) VALUES (?1, ?2, ?3)",
            params!["R1", "workflow", "Test"],
        )
        .unwrap();

        let resolved: Option<String> = conn
            .query_row(
                "SELECT resolved FROM improvements WHERE ref = 'R1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(resolved.is_none());

        // Set resolved
        conn.execute(
            "UPDATE improvements SET resolved = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE ref = 'R1'",
            [],
        )
        .unwrap();

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
    fn opens_existing_database() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("blacksmith.db");

        // Create and insert
        {
            let conn = open_or_create(&path).unwrap();
            conn.execute(
                "INSERT INTO improvements (ref, category, title) VALUES (?1, ?2, ?3)",
                params!["R1", "workflow", "Persisted"],
            )
            .unwrap();
        }

        // Reopen and verify data persisted
        {
            let conn = open_or_create(&path).unwrap();
            let title: String = conn
                .query_row(
                    "SELECT title FROM improvements WHERE ref = 'R1'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(title, "Persisted");
        }
    }
}
