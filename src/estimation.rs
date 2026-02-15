//! Time estimation for bead completion.
//!
//! Provides serial and parallel time estimates based on historical bead_metrics data
//! and the current dependency graph of open beads.
//!
//! Serial estimate: avg_time = sum(wall_time_secs) / count(completed), remaining = count(open) * avg_time.
//! Parallel estimate: critical_path through dependency DAG, clamped by serial_time / N workers,
//! plus integration overhead.

use crate::db::{self, BeadMetrics};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet, VecDeque};

/// Minimum completed beads needed to produce an estimate.
const MIN_COMPLETED_FOR_ESTIMATE: usize = 3;

/// Result of a time estimation.
#[derive(Debug)]
pub struct Estimate {
    /// Serial time estimate in seconds (one worker).
    pub serial_secs: Option<f64>,
    /// Parallel time estimate in seconds (N workers).
    pub parallel_secs: Option<f64>,
    /// Number of completed beads used for the average.
    pub completed_count: usize,
    /// Number of open (remaining) beads.
    pub open_count: usize,
    /// Average wall time per bead in seconds.
    pub avg_time_per_bead: Option<f64>,
    /// Average integration time per bead in seconds.
    pub avg_integration_time: Option<f64>,
    /// Number of workers used for parallel estimate.
    pub workers: u32,
    /// Critical path length (number of beads on the longest dependency chain).
    pub critical_path_len: usize,
    /// Beads in dependency cycles (excluded from parallel estimate).
    pub cycled_beads: Vec<String>,
}

/// A bead node for dependency graph construction.
#[derive(Debug, Clone)]
pub struct BeadNode {
    pub id: String,
    pub depends_on: Vec<String>,
}

/// Compute serial and parallel time estimates.
///
/// `conn`: Database connection for reading bead_metrics.
/// `open_beads`: Open beads with their dependency edges (from `bd list --json`).
/// `workers`: Number of parallel workers (from config).
pub fn estimate(conn: &Connection, open_beads: &[BeadNode], workers: u32) -> Estimate {
    let completed = db::completed_bead_metrics(conn).unwrap_or_default();
    let completed_count = completed.len();
    let open_count = open_beads.len();

    if completed_count < MIN_COMPLETED_FOR_ESTIMATE {
        return Estimate {
            serial_secs: None,
            parallel_secs: None,
            completed_count,
            open_count,
            avg_time_per_bead: None,
            avg_integration_time: None,
            workers,
            critical_path_len: 0,
            cycled_beads: Vec::new(),
        };
    }

    let total_wall_time: f64 = completed.iter().map(|m| m.wall_time_secs).sum();
    let avg_time = total_wall_time / completed_count as f64;

    let serial_secs = avg_time * open_count as f64;

    // Integration overhead: avg integration time * open beads
    let avg_integration_time = compute_avg_integration_time(&completed);
    let integration_overhead = avg_integration_time.unwrap_or(0.0) * open_count as f64;

    // Parallel estimate: build DAG, find critical path
    let (critical_path_time, critical_path_len, cycled_beads) =
        compute_critical_path(open_beads, avg_time);

    // parallel_time = max(critical_path_time, serial_time / N) + integration_overhead
    let n = workers.max(1) as f64;
    let parallel_secs = critical_path_time.max(serial_secs / n) + integration_overhead;

    Estimate {
        serial_secs: Some(serial_secs),
        parallel_secs: Some(parallel_secs),
        completed_count,
        open_count,
        avg_time_per_bead: Some(avg_time),
        avg_integration_time,
        workers,
        critical_path_len,
        cycled_beads,
    }
}

/// Compute average integration time from completed beads.
fn compute_avg_integration_time(completed: &[BeadMetrics]) -> Option<f64> {
    let with_integration: Vec<f64> = completed
        .iter()
        .filter_map(|m| m.integration_time_secs)
        .filter(|&t| t > 0.0)
        .collect();

    if with_integration.is_empty() {
        None
    } else {
        Some(with_integration.iter().sum::<f64>() / with_integration.len() as f64)
    }
}

/// Build the dependency DAG from open beads and compute the critical path.
///
/// Returns (critical_path_time_secs, critical_path_len, cycled_bead_ids).
///
/// Cycled beads are excluded from the DAG before computing the critical path.
/// Each bead on the path is assumed to take `avg_time` seconds.
fn compute_critical_path(open_beads: &[BeadNode], avg_time: f64) -> (f64, usize, Vec<String>) {
    if open_beads.is_empty() {
        return (0.0, 0, Vec::new());
    }

    let open_ids: HashSet<&str> = open_beads.iter().map(|b| b.id.as_str()).collect();

    // Build adjacency list: for each bead, store its dependencies (edges: dep -> bead)
    // We only consider edges within the open set.
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();

    for bead in open_beads {
        in_degree.entry(bead.id.as_str()).or_insert(0);
        for dep in &bead.depends_on {
            if open_ids.contains(dep.as_str()) {
                *in_degree.entry(bead.id.as_str()).or_insert(0) += 1;
                dependents
                    .entry(dep.as_str())
                    .or_default()
                    .push(bead.id.as_str());
            }
        }
    }

    // Phase 1: Kahn's algorithm to detect cycles
    let mut queue: VecDeque<&str> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&id, _)| id)
        .collect();

    let mut topo_order: Vec<&str> = Vec::new();
    let mut remaining_in_degree = in_degree.clone();

    while let Some(node) = queue.pop_front() {
        topo_order.push(node);
        if let Some(deps) = dependents.get(node) {
            for &dep in deps {
                if let Some(deg) = remaining_in_degree.get_mut(dep) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(dep);
                    }
                }
            }
        }
    }

    // Nodes not in topo_order are in cycles
    let topo_set: HashSet<&str> = topo_order.iter().copied().collect();
    let cycled_beads: Vec<String> = open_ids
        .iter()
        .filter(|id| !topo_set.contains(**id))
        .map(|id| id.to_string())
        .collect();

    if topo_order.is_empty() {
        // All beads are in cycles
        return (0.0, 0, cycled_beads);
    }

    // Phase 2: Longest path in DAG (critical path)
    // dist[node] = longest path ending at node (in number of beads)
    let mut dist: HashMap<&str, usize> = HashMap::new();

    for &node in &topo_order {
        let my_dist = dist.get(node).copied().unwrap_or(1);
        if let Some(deps) = dependents.get(node) {
            for &dep in deps {
                if topo_set.contains(dep) {
                    let new_dist = my_dist + 1;
                    let entry = dist.entry(dep).or_insert(1);
                    if new_dist > *entry {
                        *entry = new_dist;
                    }
                }
            }
        }
        dist.entry(node).or_insert(my_dist);
    }

    let critical_path_len = dist.values().copied().max().unwrap_or(1);
    let critical_path_time = critical_path_len as f64 * avg_time;

    (critical_path_time, critical_path_len, cycled_beads)
}

/// Format an estimate for display.
pub fn format_estimate(est: &Estimate) -> String {
    let mut lines = Vec::new();

    lines.push(format!(
        "Beads: {} completed, {} remaining",
        est.completed_count, est.open_count
    ));

    match est.avg_time_per_bead {
        Some(avg) => {
            lines.push(format!("Avg time/bead: {}", format_duration(avg)));
        }
        None => {
            lines.push(format!(
                "Insufficient data: need {} completed beads, have {}",
                MIN_COMPLETED_FOR_ESTIMATE, est.completed_count
            ));
            return lines.join("\n");
        }
    }

    if let Some(avg_int) = est.avg_integration_time {
        lines.push(format!(
            "Avg integration time: {}",
            format_duration(avg_int)
        ));
    }

    if let Some(serial) = est.serial_secs {
        lines.push(format!("Serial ETA: ~{}", format_duration(serial)));
    }

    if let Some(parallel) = est.parallel_secs {
        lines.push(format!(
            "Parallel ETA: ~{} @ {} worker{}",
            format_duration(parallel),
            est.workers,
            if est.workers == 1 { "" } else { "s" }
        ));
        if est.critical_path_len > 1 {
            lines.push(format!(
                "  Critical path: {} beads deep",
                est.critical_path_len
            ));
        }
    }

    if !est.cycled_beads.is_empty() {
        lines.push(format!(
            "Warning: {} bead{} in dependency cycle{} (excluded from estimate)",
            est.cycled_beads.len(),
            if est.cycled_beads.len() == 1 { "" } else { "s" },
            if est.cycled_beads.len() == 1 { "" } else { "s" },
        ));
    }

    lines.join("\n")
}

/// Format seconds as a human-readable duration string.
fn format_duration(secs: f64) -> String {
    if secs < 60.0 {
        format!("{:.0}s", secs)
    } else if secs < 3600.0 {
        let mins = secs / 60.0;
        format!("{:.0}m", mins)
    } else {
        let hours = (secs / 3600.0).floor();
        let mins = ((secs % 3600.0) / 60.0).round();
        if mins > 0.0 {
            format!("{:.0}h {:.0}m", hours, mins)
        } else {
            format!("{:.0}h", hours)
        }
    }
}

/// Query open beads from the `bd` command and parse into BeadNode structs.
pub fn query_open_beads() -> Vec<BeadNode> {
    match std::process::Command::new("bd")
        .args(["list", "--status=open", "--json"])
        .output()
    {
        Ok(output) if output.status.success() => {
            parse_open_beads_json(&String::from_utf8_lossy(&output.stdout))
        }
        _ => Vec::new(),
    }
}

/// Parse JSON output from `bd list --status=open --json` into BeadNode structs.
fn parse_open_beads_json(json_str: &str) -> Vec<BeadNode> {
    let parsed: Result<Vec<serde_json::Value>, _> = serde_json::from_str(json_str);
    match parsed {
        Ok(beads) => beads
            .iter()
            .filter_map(|b| {
                let id = b.get("id")?.as_str()?.to_string();
                let depends_on = b
                    .get("dependencies")
                    .and_then(|d| d.as_array())
                    .map(|deps| {
                        deps.iter()
                            .filter_map(|dep| {
                                dep.get("depends_on_id")
                                    .and_then(|d| d.as_str())
                                    .map(|s| s.to_string())
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                Some(BeadNode { id, depends_on })
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── format_duration tests ──

    #[test]
    fn format_duration_seconds() {
        assert_eq!(format_duration(45.0), "45s");
    }

    #[test]
    fn format_duration_minutes() {
        assert_eq!(format_duration(300.0), "5m");
    }

    #[test]
    fn format_duration_hours_and_minutes() {
        assert_eq!(format_duration(5400.0), "1h 30m");
    }

    #[test]
    fn format_duration_exact_hour() {
        assert_eq!(format_duration(3600.0), "1h");
    }

    // ── compute_critical_path tests ──

    #[test]
    fn critical_path_empty() {
        let (time, len, cycled) = compute_critical_path(&[], 300.0);
        assert_eq!(time, 0.0);
        assert_eq!(len, 0);
        assert!(cycled.is_empty());
    }

    #[test]
    fn critical_path_single_bead() {
        let beads = vec![BeadNode {
            id: "a".into(),
            depends_on: vec![],
        }];
        let (time, len, cycled) = compute_critical_path(&beads, 300.0);
        assert_eq!(time, 300.0);
        assert_eq!(len, 1);
        assert!(cycled.is_empty());
    }

    #[test]
    fn critical_path_linear_chain() {
        // a -> b -> c (c depends on b, b depends on a)
        let beads = vec![
            BeadNode {
                id: "a".into(),
                depends_on: vec![],
            },
            BeadNode {
                id: "b".into(),
                depends_on: vec!["a".into()],
            },
            BeadNode {
                id: "c".into(),
                depends_on: vec!["b".into()],
            },
        ];
        let (time, len, cycled) = compute_critical_path(&beads, 300.0);
        assert_eq!(len, 3);
        assert_eq!(time, 900.0);
        assert!(cycled.is_empty());
    }

    #[test]
    fn critical_path_parallel_beads() {
        // a, b, c all independent — critical path is 1
        let beads = vec![
            BeadNode {
                id: "a".into(),
                depends_on: vec![],
            },
            BeadNode {
                id: "b".into(),
                depends_on: vec![],
            },
            BeadNode {
                id: "c".into(),
                depends_on: vec![],
            },
        ];
        let (time, len, cycled) = compute_critical_path(&beads, 300.0);
        assert_eq!(len, 1);
        assert_eq!(time, 300.0);
        assert!(cycled.is_empty());
    }

    #[test]
    fn critical_path_diamond() {
        // a -> b, a -> c, b -> d, c -> d
        // Two paths of length 3: a->b->d, a->c->d
        let beads = vec![
            BeadNode {
                id: "a".into(),
                depends_on: vec![],
            },
            BeadNode {
                id: "b".into(),
                depends_on: vec!["a".into()],
            },
            BeadNode {
                id: "c".into(),
                depends_on: vec!["a".into()],
            },
            BeadNode {
                id: "d".into(),
                depends_on: vec!["b".into(), "c".into()],
            },
        ];
        let (time, len, cycled) = compute_critical_path(&beads, 300.0);
        assert_eq!(len, 3);
        assert_eq!(time, 900.0);
        assert!(cycled.is_empty());
    }

    #[test]
    fn critical_path_with_cycle() {
        // a -> b -> a (cycle), c independent
        let beads = vec![
            BeadNode {
                id: "a".into(),
                depends_on: vec!["b".into()],
            },
            BeadNode {
                id: "b".into(),
                depends_on: vec!["a".into()],
            },
            BeadNode {
                id: "c".into(),
                depends_on: vec![],
            },
        ];
        let (time, len, cycled) = compute_critical_path(&beads, 300.0);
        // c is the only non-cycled bead
        assert_eq!(len, 1);
        assert_eq!(time, 300.0);
        assert_eq!(cycled.len(), 2);
        let mut sorted_cycled = cycled.clone();
        sorted_cycled.sort();
        assert_eq!(sorted_cycled, vec!["a", "b"]);
    }

    #[test]
    fn critical_path_external_deps_ignored() {
        // b depends on "external" which is not in the open set
        let beads = vec![
            BeadNode {
                id: "a".into(),
                depends_on: vec![],
            },
            BeadNode {
                id: "b".into(),
                depends_on: vec!["external".into()],
            },
        ];
        let (time, len, cycled) = compute_critical_path(&beads, 300.0);
        // Both are independent within the open set
        assert_eq!(len, 1);
        assert_eq!(time, 300.0);
        assert!(cycled.is_empty());
    }

    // ── parse_open_beads_json tests ──

    #[test]
    fn parse_json_valid() {
        let json = r#"[
            {
                "id": "beads-abc",
                "title": "Task A",
                "status": "open",
                "dependencies": [
                    {"issue_id": "beads-abc", "depends_on_id": "beads-def", "type": "blocks"}
                ]
            },
            {
                "id": "beads-def",
                "title": "Task B",
                "status": "open",
                "dependencies": []
            }
        ]"#;
        let beads = parse_open_beads_json(json);
        assert_eq!(beads.len(), 2);
        assert_eq!(beads[0].id, "beads-abc");
        assert_eq!(beads[0].depends_on, vec!["beads-def"]);
        assert_eq!(beads[1].id, "beads-def");
        assert!(beads[1].depends_on.is_empty());
    }

    #[test]
    fn parse_json_empty_array() {
        let beads = parse_open_beads_json("[]");
        assert!(beads.is_empty());
    }

    #[test]
    fn parse_json_invalid() {
        let beads = parse_open_beads_json("not json");
        assert!(beads.is_empty());
    }

    #[test]
    fn parse_json_no_dependencies_field() {
        let json = r#"[{"id": "beads-abc", "title": "Task A"}]"#;
        let beads = parse_open_beads_json(json);
        assert_eq!(beads.len(), 1);
        assert!(beads[0].depends_on.is_empty());
    }

    // ── estimate integration tests ──

    #[test]
    fn estimate_insufficient_data() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = db::open_or_create(&db_path).unwrap();

        // Only 2 completed beads (need 3)
        db::upsert_bead_metrics(
            &conn,
            "b1",
            1,
            300.0,
            50,
            None,
            None,
            Some("2026-01-01T00:00:00Z"),
        )
        .unwrap();
        db::upsert_bead_metrics(
            &conn,
            "b2",
            1,
            400.0,
            60,
            None,
            None,
            Some("2026-01-02T00:00:00Z"),
        )
        .unwrap();

        let open = vec![BeadNode {
            id: "b3".into(),
            depends_on: vec![],
        }];
        let est = estimate(&conn, &open, 2);

        assert!(est.serial_secs.is_none());
        assert!(est.parallel_secs.is_none());
        assert_eq!(est.completed_count, 2);
        assert_eq!(est.open_count, 1);
    }

    #[test]
    fn estimate_serial_basic() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = db::open_or_create(&db_path).unwrap();

        // 3 completed beads: 300s, 400s, 500s → avg = 400s
        db::upsert_bead_metrics(
            &conn,
            "b1",
            1,
            300.0,
            50,
            None,
            None,
            Some("2026-01-01T00:00:00Z"),
        )
        .unwrap();
        db::upsert_bead_metrics(
            &conn,
            "b2",
            1,
            400.0,
            60,
            None,
            None,
            Some("2026-01-02T00:00:00Z"),
        )
        .unwrap();
        db::upsert_bead_metrics(
            &conn,
            "b3",
            1,
            500.0,
            70,
            None,
            None,
            Some("2026-01-03T00:00:00Z"),
        )
        .unwrap();

        // 2 open beads
        let open = vec![
            BeadNode {
                id: "b4".into(),
                depends_on: vec![],
            },
            BeadNode {
                id: "b5".into(),
                depends_on: vec![],
            },
        ];

        let est = estimate(&conn, &open, 1);

        assert_eq!(est.completed_count, 3);
        assert_eq!(est.open_count, 2);
        // avg = 400, serial = 400 * 2 = 800
        assert!((est.serial_secs.unwrap() - 800.0).abs() < 0.01);
        assert!((est.avg_time_per_bead.unwrap() - 400.0).abs() < 0.01);
    }

    #[test]
    fn estimate_parallel_with_chain() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = db::open_or_create(&db_path).unwrap();

        // 3 completed beads: avg = 300s, no integration time
        db::upsert_bead_metrics(
            &conn,
            "c1",
            1,
            300.0,
            50,
            None,
            None,
            Some("2026-01-01T00:00:00Z"),
        )
        .unwrap();
        db::upsert_bead_metrics(
            &conn,
            "c2",
            1,
            300.0,
            50,
            None,
            None,
            Some("2026-01-02T00:00:00Z"),
        )
        .unwrap();
        db::upsert_bead_metrics(
            &conn,
            "c3",
            1,
            300.0,
            50,
            None,
            None,
            Some("2026-01-03T00:00:00Z"),
        )
        .unwrap();

        // 4 open beads: a -> b -> c, d independent
        // Critical path = 3 (a->b->c)
        let open = vec![
            BeadNode {
                id: "a".into(),
                depends_on: vec![],
            },
            BeadNode {
                id: "b".into(),
                depends_on: vec!["a".into()],
            },
            BeadNode {
                id: "c".into(),
                depends_on: vec!["b".into()],
            },
            BeadNode {
                id: "d".into(),
                depends_on: vec![],
            },
        ];

        let est = estimate(&conn, &open, 3);

        // serial = 300 * 4 = 1200
        assert!((est.serial_secs.unwrap() - 1200.0).abs() < 0.01);

        // critical_path = 3 * 300 = 900
        // serial/N = 1200/3 = 400
        // parallel = max(900, 400) + 0 (no integration overhead) = 900
        assert!((est.parallel_secs.unwrap() - 900.0).abs() < 0.01);
        assert_eq!(est.critical_path_len, 3);
    }

    #[test]
    fn estimate_with_integration_overhead() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = db::open_or_create(&db_path).unwrap();

        // 3 completed with integration_time: 30s, 60s, 90s → avg = 60s
        db::upsert_bead_metrics(
            &conn,
            "c1",
            1,
            300.0,
            50,
            None,
            Some(30.0),
            Some("2026-01-01T00:00:00Z"),
        )
        .unwrap();
        db::upsert_bead_metrics(
            &conn,
            "c2",
            1,
            300.0,
            50,
            None,
            Some(60.0),
            Some("2026-01-02T00:00:00Z"),
        )
        .unwrap();
        db::upsert_bead_metrics(
            &conn,
            "c3",
            1,
            300.0,
            50,
            None,
            Some(90.0),
            Some("2026-01-03T00:00:00Z"),
        )
        .unwrap();

        // 2 independent open beads
        let open = vec![
            BeadNode {
                id: "a".into(),
                depends_on: vec![],
            },
            BeadNode {
                id: "b".into(),
                depends_on: vec![],
            },
        ];

        let est = estimate(&conn, &open, 2);

        // serial = 300 * 2 = 600
        // critical_path = 1 * 300 = 300
        // serial/N = 600/2 = 300
        // integration_overhead = 60 * 2 = 120
        // parallel = max(300, 300) + 120 = 420
        assert!((est.parallel_secs.unwrap() - 420.0).abs() < 0.01);
        assert!((est.avg_integration_time.unwrap() - 60.0).abs() < 0.01);
    }

    #[test]
    fn estimate_no_open_beads() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = db::open_or_create(&db_path).unwrap();

        db::upsert_bead_metrics(
            &conn,
            "c1",
            1,
            300.0,
            50,
            None,
            None,
            Some("2026-01-01T00:00:00Z"),
        )
        .unwrap();
        db::upsert_bead_metrics(
            &conn,
            "c2",
            1,
            300.0,
            50,
            None,
            None,
            Some("2026-01-02T00:00:00Z"),
        )
        .unwrap();
        db::upsert_bead_metrics(
            &conn,
            "c3",
            1,
            300.0,
            50,
            None,
            None,
            Some("2026-01-03T00:00:00Z"),
        )
        .unwrap();

        let est = estimate(&conn, &[], 2);

        assert_eq!(est.open_count, 0);
        assert!((est.serial_secs.unwrap() - 0.0).abs() < 0.01);
    }

    // ── format_estimate tests ──

    #[test]
    fn format_insufficient_data() {
        let est = Estimate {
            serial_secs: None,
            parallel_secs: None,
            completed_count: 1,
            open_count: 5,
            avg_time_per_bead: None,
            avg_integration_time: None,
            workers: 2,
            critical_path_len: 0,
            cycled_beads: Vec::new(),
        };
        let output = format_estimate(&est);
        assert!(output.contains("Insufficient data"));
        assert!(output.contains("need 3"));
    }

    #[test]
    fn format_with_estimate() {
        let est = Estimate {
            serial_secs: Some(1200.0),
            parallel_secs: Some(600.0),
            completed_count: 5,
            open_count: 4,
            avg_time_per_bead: Some(300.0),
            avg_integration_time: Some(30.0),
            workers: 3,
            critical_path_len: 2,
            cycled_beads: Vec::new(),
        };
        let output = format_estimate(&est);
        assert!(output.contains("5 completed, 4 remaining"));
        assert!(output.contains("Serial ETA"));
        assert!(output.contains("Parallel ETA"));
        assert!(output.contains("3 workers"));
        assert!(output.contains("Critical path: 2 beads"));
    }

    #[test]
    fn format_with_cycles() {
        let est = Estimate {
            serial_secs: Some(600.0),
            parallel_secs: Some(300.0),
            completed_count: 3,
            open_count: 2,
            avg_time_per_bead: Some(300.0),
            avg_integration_time: None,
            workers: 1,
            critical_path_len: 1,
            cycled_beads: vec!["a".into(), "b".into()],
        };
        let output = format_estimate(&est);
        assert!(output.contains("2 beads in dependency cycles"));
    }
}
