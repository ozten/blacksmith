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
        CREATE INDEX IF NOT EXISTS idx_improvements_category ON improvements(category);

        CREATE TABLE IF NOT EXISTS events (
            id        INTEGER PRIMARY KEY AUTOINCREMENT,
            ts        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            session   INTEGER NOT NULL,
            kind      TEXT NOT NULL,
            value     TEXT,
            tags      TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_events_session ON events(session);
        CREATE INDEX IF NOT EXISTS idx_events_kind ON events(kind);
        CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts);

        CREATE TABLE IF NOT EXISTS observations (
            session   INTEGER PRIMARY KEY,
            ts        TEXT NOT NULL,
            duration  INTEGER,
            outcome   TEXT,
            data      TEXT NOT NULL
        );",
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

/// Count total improvements (all statuses).
pub fn count_improvements(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM improvements", [], |row| row.get(0))
}

/// Fetch a single improvement by its ref (e.g. "R1").
/// Returns None if no matching ref exists.
pub fn get_improvement(conn: &Connection, ref_id: &str) -> Result<Option<Improvement>> {
    let mut stmt = conn.prepare(
        "SELECT ref, created, category, status, title, body, context, tags FROM improvements WHERE ref = ?1",
    )?;
    let mut rows = stmt.query_map(rusqlite::params![ref_id], map_improvement)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

/// Fetch the meta JSON field for an improvement by ref.
pub fn get_improvement_meta(conn: &Connection, ref_id: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT meta FROM improvements WHERE ref = ?1",
        rusqlite::params![ref_id],
        |row| row.get(0),
    )
}

/// Update an improvement's fields by ref. Only non-None values are updated.
pub fn update_improvement(
    conn: &Connection,
    ref_id: &str,
    status: Option<&str>,
    body: Option<&str>,
    context: Option<&str>,
    meta: Option<&str>,
) -> Result<bool> {
    let mut sets = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    let mut idx = 1;

    if let Some(s) = status {
        sets.push(format!("status = ?{idx}"));
        params.push(Box::new(s.to_string()));
        idx += 1;
    }
    if let Some(b) = body {
        sets.push(format!("body = ?{idx}"));
        params.push(Box::new(b.to_string()));
        idx += 1;
    }
    if let Some(c) = context {
        sets.push(format!("context = ?{idx}"));
        params.push(Box::new(c.to_string()));
        idx += 1;
    }
    if let Some(m) = meta {
        sets.push(format!("meta = ?{idx}"));
        params.push(Box::new(m.to_string()));
        idx += 1;
    }

    if sets.is_empty() {
        return Ok(false);
    }

    // Add resolved timestamp when moving to a terminal status
    if let Some(s) = status {
        if s == "promoted" || s == "dismissed" {
            sets.push("resolved = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')".to_string());
        }
    }

    let sql = format!(
        "UPDATE improvements SET {} WHERE ref = ?{idx}",
        sets.join(", ")
    );
    params.push(Box::new(ref_id.to_string()));

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = conn.execute(&sql, param_refs.as_slice())?;
    Ok(rows > 0)
}

/// Full-text search across title, body, and context fields.
/// Returns improvements where the query appears in any of these fields (case-insensitive).
pub fn search_improvements(conn: &Connection, query: &str) -> Result<Vec<Improvement>> {
    let pattern = format!("%{query}%");
    let mut stmt = conn.prepare(
        "SELECT ref, created, category, status, title, body, context, tags \
         FROM improvements \
         WHERE title LIKE ?1 OR body LIKE ?1 OR context LIKE ?1 \
         ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![pattern], map_improvement)?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

// ── Events ──────────────────────────────────────────────────────────────

/// A row from the events table.
#[derive(Debug)]
pub struct Event {
    pub id: i64,
    pub ts: String,
    pub session: i64,
    pub kind: String,
    pub value: Option<String>,
    pub tags: Option<String>,
}

/// Insert a single event into the events table.
/// Returns the rowid of the inserted event.
pub fn insert_event(
    conn: &Connection,
    session: i64,
    kind: &str,
    value: Option<&str>,
    tags: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO events (session, kind, value, tags) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![session, kind, value, tags],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Insert a single event with an explicit timestamp.
/// Returns the rowid of the inserted event.
pub fn insert_event_with_ts(
    conn: &Connection,
    ts: &str,
    session: i64,
    kind: &str,
    value: Option<&str>,
    tags: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO events (ts, session, kind, value, tags) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![ts, session, kind, value, tags],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Query events for a specific session, ordered by id.
pub fn events_by_session(conn: &Connection, session: i64) -> Result<Vec<Event>> {
    let mut stmt = conn.prepare(
        "SELECT id, ts, session, kind, value, tags FROM events WHERE session = ?1 ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![session], map_event)?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// Query events by kind, ordered by id.
pub fn events_by_kind(conn: &Connection, kind: &str) -> Result<Vec<Event>> {
    let mut stmt = conn.prepare(
        "SELECT id, ts, session, kind, value, tags FROM events WHERE kind = ?1 ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![kind], map_event)?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

fn map_event(row: &rusqlite::Row) -> Result<Event> {
    Ok(Event {
        id: row.get(0)?,
        ts: row.get(1)?,
        session: row.get(2)?,
        kind: row.get(3)?,
        value: row.get(4)?,
        tags: row.get(5)?,
    })
}

// ── Observations ────────────────────────────────────────────────────────

/// A row from the observations table (per-session materialized summary).
#[derive(Debug)]
pub struct Observation {
    pub session: i64,
    pub ts: String,
    pub duration: Option<i64>,
    pub outcome: Option<String>,
    pub data: String,
}

/// Insert or replace an observation for a session.
pub fn upsert_observation(
    conn: &Connection,
    session: i64,
    ts: &str,
    duration: Option<i64>,
    outcome: Option<&str>,
    data: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO observations (session, ts, duration, outcome, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![session, ts, duration, outcome, data],
    )?;
    Ok(())
}

/// Get the observation for a specific session.
pub fn get_observation(conn: &Connection, session: i64) -> Result<Option<Observation>> {
    let mut stmt = conn.prepare(
        "SELECT session, ts, duration, outcome, data FROM observations WHERE session = ?1",
    )?;
    let mut rows = stmt.query_map(rusqlite::params![session], map_observation)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

/// List recent observations, ordered by session descending, limited to `limit` rows.
pub fn recent_observations(conn: &Connection, limit: i64) -> Result<Vec<Observation>> {
    let mut stmt = conn.prepare(
        "SELECT session, ts, duration, outcome, data FROM observations ORDER BY session DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![limit], map_observation)?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

fn map_observation(row: &rusqlite::Row) -> Result<Observation> {
    Ok(Observation {
        session: row.get(0)?,
        ts: row.get(1)?,
        duration: row.get(2)?,
        outcome: row.get(3)?,
        data: row.get(4)?,
    })
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

    // ── Events table tests ──────────────────────────────────────────────

    // ── get_improvement tests ────────────────────────────────────────

    #[test]
    fn get_improvement_found() {
        let (_dir, conn) = test_db();
        insert_improvement(
            &conn,
            "workflow",
            "Test item",
            Some("body"),
            Some("ctx"),
            Some("t1"),
        )
        .unwrap();

        let imp = get_improvement(&conn, "R1").unwrap().unwrap();
        assert_eq!(imp.ref_id, "R1");
        assert_eq!(imp.title, "Test item");
        assert_eq!(imp.body.as_deref(), Some("body"));
        assert_eq!(imp.context.as_deref(), Some("ctx"));
        assert_eq!(imp.tags.as_deref(), Some("t1"));
    }

    #[test]
    fn get_improvement_not_found() {
        let (_dir, conn) = test_db();
        let imp = get_improvement(&conn, "R999").unwrap();
        assert!(imp.is_none());
    }

    #[test]
    fn get_improvement_meta_found() {
        let (_dir, conn) = test_db();
        conn.execute(
            "INSERT INTO improvements (ref, category, title, meta) VALUES (?1, ?2, ?3, ?4)",
            params!["R1", "workflow", "Test", r#"{"key": "val"}"#],
        )
        .unwrap();

        let meta = get_improvement_meta(&conn, "R1").unwrap();
        assert!(meta.is_some());
        assert!(meta.unwrap().contains("key"));
    }

    // ── update_improvement tests ─────────────────────────────────────

    #[test]
    fn update_improvement_status() {
        let (_dir, conn) = test_db();
        insert_improvement(&conn, "workflow", "Item", None, None, None).unwrap();

        let updated = update_improvement(&conn, "R1", Some("validated"), None, None, None).unwrap();
        assert!(updated);

        let imp = get_improvement(&conn, "R1").unwrap().unwrap();
        assert_eq!(imp.status, "validated");
    }

    #[test]
    fn update_improvement_body_context() {
        let (_dir, conn) = test_db();
        insert_improvement(&conn, "workflow", "Item", None, None, None).unwrap();

        update_improvement(&conn, "R1", None, Some("new body"), Some("new ctx"), None).unwrap();

        let imp = get_improvement(&conn, "R1").unwrap().unwrap();
        assert_eq!(imp.body.as_deref(), Some("new body"));
        assert_eq!(imp.context.as_deref(), Some("new ctx"));
    }

    #[test]
    fn update_improvement_sets_resolved_on_promote() {
        let (_dir, conn) = test_db();
        insert_improvement(&conn, "workflow", "Item", None, None, None).unwrap();

        update_improvement(&conn, "R1", Some("promoted"), None, None, None).unwrap();

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
    fn update_improvement_sets_resolved_on_dismiss() {
        let (_dir, conn) = test_db();
        insert_improvement(&conn, "workflow", "Item", None, None, None).unwrap();

        update_improvement(&conn, "R1", Some("dismissed"), None, None, None).unwrap();

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
    fn update_improvement_no_resolved_on_other_status() {
        let (_dir, conn) = test_db();
        insert_improvement(&conn, "workflow", "Item", None, None, None).unwrap();

        update_improvement(&conn, "R1", Some("validated"), None, None, None).unwrap();

        let resolved: Option<String> = conn
            .query_row(
                "SELECT resolved FROM improvements WHERE ref = 'R1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(resolved.is_none());
    }

    #[test]
    fn update_improvement_nonexistent() {
        let (_dir, conn) = test_db();
        let updated = update_improvement(&conn, "R999", Some("open"), None, None, None).unwrap();
        assert!(!updated);
    }

    #[test]
    fn update_improvement_nothing_to_update() {
        let (_dir, conn) = test_db();
        insert_improvement(&conn, "workflow", "Item", None, None, None).unwrap();
        let updated = update_improvement(&conn, "R1", None, None, None, None).unwrap();
        assert!(!updated);
    }

    #[test]
    fn update_improvement_meta() {
        let (_dir, conn) = test_db();
        insert_improvement(&conn, "workflow", "Item", None, None, None).unwrap();

        update_improvement(&conn, "R1", None, None, None, Some(r#"{"reason": "test"}"#)).unwrap();

        let meta = get_improvement_meta(&conn, "R1").unwrap();
        assert!(meta.is_some());
        assert!(meta.unwrap().contains("test"));
    }

    // ── search_improvements tests ────────────────────────────────────

    #[test]
    fn search_improvements_by_title() {
        let (_dir, conn) = test_db();
        insert_improvement(&conn, "workflow", "Reduce token usage", None, None, None).unwrap();
        insert_improvement(&conn, "cost", "Fix retry logic", None, None, None).unwrap();

        let results = search_improvements(&conn, "token").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Reduce token usage");
    }

    #[test]
    fn search_improvements_by_body() {
        let (_dir, conn) = test_db();
        insert_improvement(
            &conn,
            "workflow",
            "Title",
            Some("Use parallel tool calls"),
            None,
            None,
        )
        .unwrap();

        let results = search_improvements(&conn, "parallel").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_improvements_by_context() {
        let (_dir, conn) = test_db();
        insert_improvement(
            &conn,
            "workflow",
            "Title",
            None,
            Some("sessions 340-348"),
            None,
        )
        .unwrap();

        let results = search_improvements(&conn, "340").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_improvements_no_match() {
        let (_dir, conn) = test_db();
        insert_improvement(&conn, "workflow", "Something", None, None, None).unwrap();

        let results = search_improvements(&conn, "nonexistent").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_improvements_case_insensitive() {
        let (_dir, conn) = test_db();
        insert_improvement(&conn, "cost", "Token Usage", None, None, None).unwrap();

        let results = search_improvements(&conn, "token").unwrap();
        assert_eq!(results.len(), 1);
    }

    // ── Events table tests ──────────────────────────────────────────────

    #[test]
    fn events_table_exists() {
        let (_dir, conn) = test_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn insert_event_basic() {
        let (_dir, conn) = test_db();
        let id = insert_event(&conn, 1, "turns.total", Some("67"), None).unwrap();
        assert!(id > 0);

        let events = events_by_session(&conn, 1).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].session, 1);
        assert_eq!(events[0].kind, "turns.total");
        assert_eq!(events[0].value.as_deref(), Some("67"));
        assert!(events[0].tags.is_none());
    }

    #[test]
    fn insert_event_with_tags() {
        let (_dir, conn) = test_db();
        insert_event(&conn, 1, "commit.detected", Some("true"), Some("ci,deploy")).unwrap();

        let events = events_by_session(&conn, 1).unwrap();
        assert_eq!(events[0].tags.as_deref(), Some("ci,deploy"));
    }

    #[test]
    fn insert_event_null_value() {
        let (_dir, conn) = test_db();
        insert_event(&conn, 1, "session.start", None, None).unwrap();

        let events = events_by_session(&conn, 1).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].value.is_none());
    }

    #[test]
    fn events_timestamp_auto_set() {
        let (_dir, conn) = test_db();
        insert_event(&conn, 1, "session.start", None, None).unwrap();

        let events = events_by_session(&conn, 1).unwrap();
        assert!(events[0].ts.contains('T'));
        assert!(events[0].ts.ends_with('Z'));
    }

    #[test]
    fn insert_event_with_explicit_ts() {
        let (_dir, conn) = test_db();
        let ts = "2026-01-15T10:30:00Z";
        insert_event_with_ts(&conn, ts, 42, "turns.total", Some("55"), None).unwrap();

        let events = events_by_session(&conn, 42).unwrap();
        assert_eq!(events[0].ts, ts);
    }

    #[test]
    fn multiple_events_per_session() {
        let (_dir, conn) = test_db();
        insert_event(&conn, 5, "turns.total", Some("67"), None).unwrap();
        insert_event(&conn, 5, "cost.estimate_usd", Some("24.57"), None).unwrap();
        insert_event(&conn, 5, "session.outcome", Some("\"completed\""), None).unwrap();

        let events = events_by_session(&conn, 5).unwrap();
        assert_eq!(events.len(), 3);
        // Should be ordered by id (insertion order)
        assert_eq!(events[0].kind, "turns.total");
        assert_eq!(events[1].kind, "cost.estimate_usd");
        assert_eq!(events[2].kind, "session.outcome");
    }

    #[test]
    fn events_by_kind_query() {
        let (_dir, conn) = test_db();
        insert_event(&conn, 1, "turns.total", Some("50"), None).unwrap();
        insert_event(&conn, 2, "turns.total", Some("67"), None).unwrap();
        insert_event(&conn, 2, "cost.estimate_usd", Some("20"), None).unwrap();
        insert_event(&conn, 3, "turns.total", Some("80"), None).unwrap();

        let turns = events_by_kind(&conn, "turns.total").unwrap();
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].session, 1);
        assert_eq!(turns[1].session, 2);
        assert_eq!(turns[2].session, 3);
    }

    #[test]
    fn events_session_index_works() {
        let (_dir, conn) = test_db();
        // Insert events across multiple sessions
        for session in 1..=10 {
            insert_event(&conn, session, "turns.total", Some("50"), None).unwrap();
        }
        // Query specific session — index should be used
        let events = events_by_session(&conn, 5).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].session, 5);
    }

    #[test]
    fn events_empty_session_returns_empty() {
        let (_dir, conn) = test_db();
        let events = events_by_session(&conn, 999).unwrap();
        assert!(events.is_empty());
    }

    // ── Observations table tests ────────────────────────────────────────

    #[test]
    fn observations_table_exists() {
        let (_dir, conn) = test_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM observations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn upsert_observation_insert() {
        let (_dir, conn) = test_db();
        let data = r#"{"turns.total": 67, "cost.estimate_usd": 24.57}"#;
        upsert_observation(
            &conn,
            1,
            "2026-01-15T10:30:00Z",
            Some(1847),
            Some("completed"),
            data,
        )
        .unwrap();

        let obs = get_observation(&conn, 1).unwrap().unwrap();
        assert_eq!(obs.session, 1);
        assert_eq!(obs.ts, "2026-01-15T10:30:00Z");
        assert_eq!(obs.duration, Some(1847));
        assert_eq!(obs.outcome.as_deref(), Some("completed"));
        assert_eq!(obs.data, data);
    }

    #[test]
    fn upsert_observation_replaces() {
        let (_dir, conn) = test_db();
        let data1 = r#"{"turns.total": 50}"#;
        let data2 = r#"{"turns.total": 67, "cost.estimate_usd": 24.57}"#;

        upsert_observation(
            &conn,
            1,
            "2026-01-15T10:30:00Z",
            Some(1000),
            Some("failed"),
            data1,
        )
        .unwrap();
        upsert_observation(
            &conn,
            1,
            "2026-01-15T10:30:00Z",
            Some(1847),
            Some("completed"),
            data2,
        )
        .unwrap();

        let obs = get_observation(&conn, 1).unwrap().unwrap();
        assert_eq!(obs.duration, Some(1847));
        assert_eq!(obs.outcome.as_deref(), Some("completed"));
        assert_eq!(obs.data, data2);

        // Should still be just one row
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM observations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn observation_nullable_fields() {
        let (_dir, conn) = test_db();
        upsert_observation(&conn, 1, "2026-01-15T10:30:00Z", None, None, "{}").unwrap();

        let obs = get_observation(&conn, 1).unwrap().unwrap();
        assert!(obs.duration.is_none());
        assert!(obs.outcome.is_none());
    }

    #[test]
    fn get_observation_nonexistent() {
        let (_dir, conn) = test_db();
        let obs = get_observation(&conn, 999).unwrap();
        assert!(obs.is_none());
    }

    #[test]
    fn recent_observations_ordering() {
        let (_dir, conn) = test_db();
        for session in 1..=5 {
            let data = format!(r#"{{"session": {session}}}"#);
            upsert_observation(
                &conn,
                session,
                "2026-01-15T10:30:00Z",
                Some(1000),
                Some("completed"),
                &data,
            )
            .unwrap();
        }

        let recent = recent_observations(&conn, 3).unwrap();
        assert_eq!(recent.len(), 3);
        // Should be descending by session
        assert_eq!(recent[0].session, 5);
        assert_eq!(recent[1].session, 4);
        assert_eq!(recent[2].session, 3);
    }

    #[test]
    fn recent_observations_limit() {
        let (_dir, conn) = test_db();
        for session in 1..=10 {
            upsert_observation(&conn, session, "2026-01-15T10:30:00Z", None, None, "{}").unwrap();
        }

        let recent = recent_observations(&conn, 5).unwrap();
        assert_eq!(recent.len(), 5);
    }

    #[test]
    fn observations_session_is_primary_key() {
        let (_dir, conn) = test_db();
        // Insert two different sessions — both should succeed
        upsert_observation(&conn, 1, "2026-01-15T10:30:00Z", None, None, "{}").unwrap();
        upsert_observation(&conn, 2, "2026-01-15T11:30:00Z", None, None, "{}").unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM observations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn events_and_observations_coexist() {
        let (_dir, conn) = test_db();

        // Insert events for session 1
        insert_event(&conn, 1, "turns.total", Some("67"), None).unwrap();
        insert_event(&conn, 1, "cost.estimate_usd", Some("24.57"), None).unwrap();

        // Insert observation for session 1
        let data = r#"{"turns.total": 67, "cost.estimate_usd": 24.57}"#;
        upsert_observation(
            &conn,
            1,
            "2026-01-15T10:30:00Z",
            Some(1847),
            Some("completed"),
            data,
        )
        .unwrap();

        // Both should be queryable
        let events = events_by_session(&conn, 1).unwrap();
        assert_eq!(events.len(), 2);

        let obs = get_observation(&conn, 1).unwrap().unwrap();
        assert_eq!(obs.data, data);
    }

    #[test]
    fn all_three_tables_created() {
        let (_dir, conn) = test_db();

        // Verify all three tables exist
        let tables: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<Vec<_>>>()
                .unwrap()
        };

        assert!(tables.contains(&"improvements".to_string()));
        assert!(tables.contains(&"events".to_string()));
        assert!(tables.contains(&"observations".to_string()));
    }

    #[test]
    fn events_indexes_created() {
        let (_dir, conn) = test_db();

        let indexes: Vec<String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='events' ORDER BY name",
                )
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<Vec<_>>>()
                .unwrap()
        };

        assert!(indexes.contains(&"idx_events_session".to_string()));
        assert!(indexes.contains(&"idx_events_kind".to_string()));
        assert!(indexes.contains(&"idx_events_ts".to_string()));
    }
}
