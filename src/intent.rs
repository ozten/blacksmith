use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// A single concept identified by intent analysis, with reasoning.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TargetArea {
    pub concept: String,
    pub reasoning: String,
}

/// The result of LLM-based intent analysis for a task.
///
/// This is the stable, expensive layer (Layer 1) that only invalidates
/// when issue content changes. Concepts are abstract (e.g. "auth_endpoints")
/// rather than concrete file paths, so they survive refactors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentAnalysis {
    pub task_id: String,
    pub content_hash: String,
    pub target_areas: Vec<TargetArea>,
}

/// Compute a content hash from the issue title, description, and acceptance criteria.
///
/// Uses a simple deterministic hash. The same inputs always produce the same hash,
/// so analysis is only re-run when the issue content actually changes.
pub fn content_hash(title: &str, description: &str, acceptance_criteria: &str) -> String {
    let mut hasher = DefaultHasher::new();
    title.hash(&mut hasher);
    description.hash(&mut hasher);
    acceptance_criteria.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Create the intent_analyses table if it doesn't exist.
pub fn create_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS intent_analyses (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id       TEXT NOT NULL,
            content_hash  TEXT NOT NULL,
            target_areas  TEXT NOT NULL,
            created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            UNIQUE(task_id, content_hash)
        );

        CREATE INDEX IF NOT EXISTS idx_intent_analyses_content_hash
            ON intent_analyses(content_hash);
        CREATE INDEX IF NOT EXISTS idx_intent_analyses_task_id
            ON intent_analyses(task_id);",
    )
}

/// Look up a cached intent analysis by content_hash.
///
/// Returns the most recent analysis matching this hash, if any.
/// Since the hash covers title+description+acceptance criteria,
/// a cache hit means the issue content hasn't changed.
pub fn get_by_content_hash(
    conn: &Connection,
    content_hash: &str,
) -> Result<Option<IntentAnalysis>> {
    let mut stmt = conn.prepare(
        "SELECT task_id, content_hash, target_areas
         FROM intent_analyses
         WHERE content_hash = ?1
         ORDER BY id DESC
         LIMIT 1",
    )?;

    let mut rows = stmt.query_map(params![content_hash], |row| {
        let task_id: String = row.get(0)?;
        let hash: String = row.get(1)?;
        let areas_json: String = row.get(2)?;
        Ok((task_id, hash, areas_json))
    })?;

    match rows.next() {
        Some(Ok((task_id, hash, areas_json))) => {
            let target_areas: Vec<TargetArea> =
                serde_json::from_str(&areas_json).unwrap_or_default();
            Ok(Some(IntentAnalysis {
                task_id,
                content_hash: hash,
                target_areas,
            }))
        }
        Some(Err(e)) => Err(e),
        None => Ok(None),
    }
}

/// Look up a cached intent analysis by task_id.
///
/// Returns the most recent analysis for this task, regardless of content hash.
pub fn get_by_task_id(conn: &Connection, task_id: &str) -> Result<Option<IntentAnalysis>> {
    let mut stmt = conn.prepare(
        "SELECT task_id, content_hash, target_areas
         FROM intent_analyses
         WHERE task_id = ?1
         ORDER BY id DESC
         LIMIT 1",
    )?;

    let mut rows = stmt.query_map(params![task_id], |row| {
        let tid: String = row.get(0)?;
        let hash: String = row.get(1)?;
        let areas_json: String = row.get(2)?;
        Ok((tid, hash, areas_json))
    })?;

    match rows.next() {
        Some(Ok((tid, hash, areas_json))) => {
            let target_areas: Vec<TargetArea> =
                serde_json::from_str(&areas_json).unwrap_or_default();
            Ok(Some(IntentAnalysis {
                task_id: tid,
                content_hash: hash,
                target_areas,
            }))
        }
        Some(Err(e)) => Err(e),
        None => Ok(None),
    }
}

/// Store an intent analysis result, replacing any existing entry for the same
/// (task_id, content_hash) pair.
pub fn store(conn: &Connection, analysis: &IntentAnalysis) -> Result<()> {
    let areas_json =
        serde_json::to_string(&analysis.target_areas).unwrap_or_else(|_| "[]".to_string());

    conn.execute(
        "INSERT OR REPLACE INTO intent_analyses (task_id, content_hash, target_areas)
         VALUES (?1, ?2, ?3)",
        params![analysis.task_id, analysis.content_hash, areas_json],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        create_table(&conn).unwrap();
        conn
    }

    #[test]
    fn test_content_hash_deterministic() {
        let h1 = content_hash("title", "desc", "criteria");
        let h2 = content_hash("title", "desc", "criteria");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_content_hash_changes_with_input() {
        let h1 = content_hash("title", "desc", "criteria");
        let h2 = content_hash("title", "desc", "different criteria");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_content_hash_format() {
        let h = content_hash("a", "b", "c");
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_store_and_retrieve_by_content_hash() {
        let conn = setup_db();
        let analysis = IntentAnalysis {
            task_id: "task-1".to_string(),
            content_hash: "abc123".to_string(),
            target_areas: vec![
                TargetArea {
                    concept: "auth".to_string(),
                    reasoning: "handles login".to_string(),
                },
                TargetArea {
                    concept: "config".to_string(),
                    reasoning: "rate limits configurable".to_string(),
                },
            ],
        };

        store(&conn, &analysis).unwrap();

        let retrieved = get_by_content_hash(&conn, "abc123").unwrap().unwrap();
        assert_eq!(retrieved.task_id, "task-1");
        assert_eq!(retrieved.content_hash, "abc123");
        assert_eq!(retrieved.target_areas.len(), 2);
        assert_eq!(retrieved.target_areas[0].concept, "auth");
        assert_eq!(retrieved.target_areas[1].concept, "config");
    }

    #[test]
    fn test_retrieve_by_task_id() {
        let conn = setup_db();
        let analysis = IntentAnalysis {
            task_id: "task-42".to_string(),
            content_hash: "hash1".to_string(),
            target_areas: vec![TargetArea {
                concept: "middleware".to_string(),
                reasoning: "rate limiting".to_string(),
            }],
        };

        store(&conn, &analysis).unwrap();

        let retrieved = get_by_task_id(&conn, "task-42").unwrap().unwrap();
        assert_eq!(retrieved.task_id, "task-42");
        assert_eq!(retrieved.target_areas.len(), 1);
    }

    #[test]
    fn test_cache_miss_returns_none() {
        let conn = setup_db();
        assert!(get_by_content_hash(&conn, "nonexistent").unwrap().is_none());
        assert!(get_by_task_id(&conn, "nonexistent").unwrap().is_none());
    }

    #[test]
    fn test_upsert_replaces_on_same_task_and_hash() {
        let conn = setup_db();

        let v1 = IntentAnalysis {
            task_id: "task-1".to_string(),
            content_hash: "hash1".to_string(),
            target_areas: vec![TargetArea {
                concept: "old".to_string(),
                reasoning: "old reason".to_string(),
            }],
        };
        store(&conn, &v1).unwrap();

        let v2 = IntentAnalysis {
            task_id: "task-1".to_string(),
            content_hash: "hash1".to_string(),
            target_areas: vec![TargetArea {
                concept: "new".to_string(),
                reasoning: "new reason".to_string(),
            }],
        };
        store(&conn, &v2).unwrap();

        let retrieved = get_by_content_hash(&conn, "hash1").unwrap().unwrap();
        assert_eq!(retrieved.target_areas[0].concept, "new");
    }

    #[test]
    fn test_new_hash_creates_new_entry() {
        let conn = setup_db();

        let v1 = IntentAnalysis {
            task_id: "task-1".to_string(),
            content_hash: "hash1".to_string(),
            target_areas: vec![TargetArea {
                concept: "v1".to_string(),
                reasoning: "first".to_string(),
            }],
        };
        store(&conn, &v1).unwrap();

        let v2 = IntentAnalysis {
            task_id: "task-1".to_string(),
            content_hash: "hash2".to_string(),
            target_areas: vec![TargetArea {
                concept: "v2".to_string(),
                reasoning: "second".to_string(),
            }],
        };
        store(&conn, &v2).unwrap();

        // Both entries exist
        let r1 = get_by_content_hash(&conn, "hash1").unwrap().unwrap();
        assert_eq!(r1.target_areas[0].concept, "v1");

        let r2 = get_by_content_hash(&conn, "hash2").unwrap().unwrap();
        assert_eq!(r2.target_areas[0].concept, "v2");

        // get_by_task_id returns the latest (hash2)
        let latest = get_by_task_id(&conn, "task-1").unwrap().unwrap();
        assert_eq!(latest.content_hash, "hash2");
    }

    #[test]
    fn test_empty_target_areas() {
        let conn = setup_db();
        let analysis = IntentAnalysis {
            task_id: "task-empty".to_string(),
            content_hash: "emptyhash".to_string(),
            target_areas: vec![],
        };

        store(&conn, &analysis).unwrap();

        let retrieved = get_by_content_hash(&conn, "emptyhash").unwrap().unwrap();
        assert!(retrieved.target_areas.is_empty());
    }
}
