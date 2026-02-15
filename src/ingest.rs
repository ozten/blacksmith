/// Metric extraction: parse a session output file via an adapter and write
/// extracted events + observations to the database.
use crate::adapters::{AgentAdapter, ExtractionSource};
use crate::config::CompiledRule;
use crate::db;
use rusqlite::Connection;
use serde_json::Value;
use std::path::Path;

/// Ingestion result with key summary fields extracted from adapter metrics.
#[derive(Debug, Default)]
pub struct IngestResult {
    pub turns_total: u64,
    pub cost_estimate_usd: f64,
    pub session_duration_ms: u64,
}

/// Ingest a session output file: extract metrics via adapter, write events
/// and observation to the database. Returns a summary of extracted metrics.
pub fn ingest_session(
    conn: &Connection,
    session: i64,
    output_path: &Path,
    exit_code: Option<i32>,
    adapter: &dyn AgentAdapter,
) -> Result<IngestResult, IngestError> {
    ingest_session_with_rules(conn, session, output_path, exit_code, &[], adapter)
}

/// Ingest a session output file with configurable extraction rules applied.
pub fn ingest_session_with_rules(
    conn: &Connection,
    session: i64,
    output_path: &Path,
    exit_code: Option<i32>,
    rules: &[CompiledRule],
    adapter: &dyn AgentAdapter,
) -> Result<IngestResult, IngestError> {
    // Extract built-in metrics via the adapter
    let builtin_metrics = adapter
        .extract_builtin_metrics(output_path)
        .map_err(|e| IngestError::Io(std::io::Error::other(e.to_string())))?;

    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    // Write individual events for built-in metrics
    for (kind, value) in &builtin_metrics {
        let value_str = value_to_event_string(value);
        db::insert_event_with_ts(conn, &ts, session, kind, Some(&value_str), None)
            .map_err(IngestError::Db)?;
    }

    // Write exit_code event if provided
    if let Some(code) = exit_code {
        db::insert_event_with_ts(
            conn,
            &ts,
            session,
            "session.exit_code",
            Some(&code.to_string()),
            None,
        )
        .map_err(IngestError::Db)?;
    }

    // Apply configurable extraction rules via adapter's lines_for_source
    let rule_results = apply_rules_via_adapter(rules, output_path, adapter)?;
    for (kind, value) in &rule_results {
        db::insert_event_with_ts(conn, &ts, session, kind, Some(value), None)
            .map_err(IngestError::Db)?;
    }

    // Build observation data JSON (includes both built-in and rule-extracted)
    let data = build_observation_data(&builtin_metrics, exit_code, &rule_results);

    // Extract duration for the observation row
    let duration_ms = builtin_metrics
        .iter()
        .find(|(k, _)| k == "session.duration_ms")
        .and_then(|(_, v)| v.as_u64())
        .unwrap_or(0);
    let duration_secs = (duration_ms / 1000) as i64;

    db::upsert_observation(conn, session, &ts, Some(duration_secs), None, &data)
        .map_err(IngestError::Db)?;

    // Build summary result
    let result = IngestResult {
        turns_total: builtin_metrics
            .iter()
            .find(|(k, _)| k == "turns.total")
            .and_then(|(_, v)| v.as_u64())
            .unwrap_or(0),
        cost_estimate_usd: builtin_metrics
            .iter()
            .find(|(k, _)| k == "cost.estimate_usd")
            .and_then(|(_, v)| v.as_f64())
            .unwrap_or(0.0),
        session_duration_ms: duration_ms,
    };

    Ok(result)
}

/// Convert a serde_json::Value to a string suitable for event storage.
fn value_to_event_string(v: &Value) -> String {
    match v {
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if n.is_f64() && n.as_i64().is_none() && n.as_u64().is_none() {
                    return format!("{:.6}", f);
                }
            }
            n.to_string()
        }
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

/// Map a rule source string to the ExtractionSource enum.
fn source_str_to_enum(source: &str) -> ExtractionSource {
    match source {
        "text" => ExtractionSource::Text,
        "raw" => ExtractionSource::Raw,
        _ => ExtractionSource::ToolCommands, // "tool_commands" is the default
    }
}

/// Apply configurable extraction rules via adapter's lines_for_source.
/// Returns a Vec of (kind, value) pairs for each rule that produced a match.
fn apply_rules_via_adapter(
    rules: &[CompiledRule],
    output_path: &Path,
    adapter: &dyn AgentAdapter,
) -> Result<Vec<(String, String)>, IngestError> {
    if rules.is_empty() {
        return Ok(Vec::new());
    }

    // Group rules by source type to avoid re-parsing the file for each source
    // Cache lines per source type
    let mut source_cache: std::collections::HashMap<&str, Vec<String>> =
        std::collections::HashMap::new();

    let mut results = Vec::new();

    for rule in rules {
        let lines = match source_cache.get(rule.source.as_str()) {
            Some(cached) => cached,
            None => {
                let source_enum = source_str_to_enum(&rule.source);
                let fetched = adapter
                    .lines_for_source(output_path, source_enum)
                    .map_err(|e| IngestError::Io(std::io::Error::other(e.to_string())))?;
                source_cache.insert(&rule.source, fetched);
                source_cache.get(rule.source.as_str()).unwrap()
            }
        };

        apply_single_rule(rule, lines, &mut results);
    }

    Ok(results)
}

/// Apply a single extraction rule against a set of lines.
fn apply_single_rule(rule: &CompiledRule, lines: &[String], results: &mut Vec<(String, String)>) {
    if let Some(ref emit_val) = rule.emit {
        // Emit mode: check if pattern matches anywhere, emit fixed value
        let found = lines.iter().any(|line| rule.pattern.is_match(line));
        if found {
            let val = toml_value_to_string(emit_val);
            results.push((rule.kind.clone(), val));
        }
    } else if rule.count {
        // Count mode: count matching lines (minus anti_pattern exclusions)
        let count = lines
            .iter()
            .filter(|line| {
                if !rule.pattern.is_match(line) {
                    return false;
                }
                if let Some(ref anti) = rule.anti_pattern {
                    return !anti.is_match(line);
                }
                true
            })
            .count();
        results.push((rule.kind.clone(), count.to_string()));
    } else if rule.first_match {
        // First-match mode: find first capture group match and emit it
        for line in lines {
            if let Some(ref anti) = rule.anti_pattern {
                if anti.is_match(line) {
                    continue;
                }
            }
            if let Some(caps) = rule.pattern.captures(line) {
                let matched = caps
                    .get(1)
                    .map(|m| m.as_str())
                    .unwrap_or_else(|| caps.get(0).unwrap().as_str());
                let value = apply_transform(matched, rule.transform.as_deref());
                results.push((rule.kind.clone(), value));
                break;
            }
        }
    } else {
        // Default: collect all matches
        let mut matches = Vec::new();
        for line in lines {
            if let Some(ref anti) = rule.anti_pattern {
                if anti.is_match(line) {
                    continue;
                }
            }
            if let Some(caps) = rule.pattern.captures(line) {
                let matched = caps
                    .get(1)
                    .map(|m| m.as_str())
                    .unwrap_or_else(|| caps.get(0).unwrap().as_str());
                let value = apply_transform(matched, rule.transform.as_deref());
                matches.push(value);
            }
        }
        if !matches.is_empty() {
            // Emit as JSON array if multiple, plain value if single
            if matches.len() == 1 {
                results.push((rule.kind.clone(), matches.into_iter().next().unwrap()));
            } else {
                let arr: Vec<Value> = matches.into_iter().map(Value::String).collect();
                results.push((rule.kind.clone(), Value::Array(arr).to_string()));
            }
        }
    }
}

/// Apply a transform to a matched string.
fn apply_transform(input: &str, transform: Option<&str>) -> String {
    match transform {
        Some("last_segment") => input.rsplit('-').next().unwrap_or(input).to_string(),
        Some("int") => {
            // Extract first integer from the string
            input
                .chars()
                .filter(|c| c.is_ascii_digit())
                .collect::<String>()
        }
        Some("trim") => input.trim().to_string(),
        _ => input.to_string(),
    }
}

/// Convert a TOML value to a string for event storage.
fn toml_value_to_string(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        _ => v.to_string(),
    }
}

/// Build observation data JSON from adapter metrics and rule-extracted metrics.
fn build_observation_data(
    builtin_metrics: &[(String, Value)],
    exit_code: Option<i32>,
    rule_results: &[(String, String)],
) -> String {
    let mut map = serde_json::Map::new();

    // Insert built-in metrics from adapter
    for (kind, value) in builtin_metrics {
        map.insert(kind.clone(), value.clone());
    }

    // Insert exit_code if provided
    if let Some(code) = exit_code {
        map.insert("session.exit_code".to_string(), Value::Number(code.into()));
    }

    // Insert rule-extracted metrics
    for (kind, value) in rule_results {
        // Try to parse as number first for cleaner JSON
        if let Ok(n) = value.parse::<i64>() {
            map.insert(kind.clone(), Value::Number(n.into()));
        } else if let Ok(n) = value.parse::<f64>() {
            map.insert(
                kind.clone(),
                serde_json::Number::from_f64(n)
                    .map(Value::Number)
                    .unwrap_or(Value::String(value.clone())),
            );
        } else if value == "true" {
            map.insert(kind.clone(), Value::Bool(true));
        } else if value == "false" {
            map.insert(kind.clone(), Value::Bool(false));
        } else {
            // Try parsing as JSON (for arrays), fall back to string
            match serde_json::from_str::<Value>(value) {
                Ok(v) if v.is_array() => {
                    map.insert(kind.clone(), v);
                }
                _ => {
                    map.insert(kind.clone(), Value::String(value.clone()));
                }
            }
        }
    }

    Value::Object(map).to_string()
}

#[derive(Debug)]
pub enum IngestError {
    Io(std::io::Error),
    Db(rusqlite::Error),
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IngestError::Io(e) => write!(f, "I/O error during ingestion: {e}"),
            IngestError::Db(e) => write!(f, "database error during ingestion: {e}"),
        }
    }
}

impl std::error::Error for IngestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IngestError::Io(e) => Some(e),
            IngestError::Db(e) => Some(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_jsonl(dir: &Path, lines: &[&str]) -> std::path::PathBuf {
        let path = dir.join("test-session.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{}", line).unwrap();
        }
        path
    }

    fn test_db() -> (TempDir, Connection) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("blacksmith.db");
        let conn = db::open_or_create(&db_path).unwrap();
        (dir, conn)
    }

    fn claude_adapter() -> crate::adapters::claude::ClaudeAdapter {
        crate::adapters::claude::ClaudeAdapter::new()
    }

    #[test]
    fn ingest_session_writes_events() {
        let (_db_dir, conn) = test_db();
        let data_dir = TempDir::new().unwrap();
        let lines = &[
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{}}]}}"#,
            r#"{"type":"result","duration_ms":10000,"total_cost_usd":0.25,"modelUsage":{"opus":{"inputTokens":500,"outputTokens":100,"cacheReadInputTokens":0,"cacheCreationInputTokens":0}}}"#,
        ];
        let path = write_jsonl(data_dir.path(), lines);

        let adapter = claude_adapter();
        let result = ingest_session(&conn, 42, &path, Some(0), &adapter).unwrap();
        assert_eq!(result.turns_total, 2);

        // Verify events were written
        let events = db::events_by_session(&conn, 42).unwrap();
        // 11 built-in metrics + 1 exit_code = 12
        assert_eq!(events.len(), 12);

        // Verify specific event values
        let turns_total = events.iter().find(|e| e.kind == "turns.total").unwrap();
        assert_eq!(turns_total.value.as_deref(), Some("2"));

        let cost = events
            .iter()
            .find(|e| e.kind == "cost.estimate_usd")
            .unwrap();
        assert_eq!(cost.value.as_deref(), Some("0.250000"));
    }

    #[test]
    fn ingest_session_writes_observation() {
        let (_db_dir, conn) = test_db();
        let data_dir = TempDir::new().unwrap();
        let lines = &[
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#,
            r#"{"type":"result","duration_ms":60000,"total_cost_usd":1.0,"modelUsage":{"opus":{"inputTokens":100,"outputTokens":50,"cacheReadInputTokens":0,"cacheCreationInputTokens":0}}}"#,
        ];
        let path = write_jsonl(data_dir.path(), lines);

        let adapter = claude_adapter();
        ingest_session(&conn, 7, &path, None, &adapter).unwrap();

        let obs = db::get_observation(&conn, 7).unwrap().unwrap();
        assert_eq!(obs.session, 7);
        assert_eq!(obs.duration, Some(60));

        // Verify observation data JSON
        let data: Value = serde_json::from_str(&obs.data).unwrap();
        assert_eq!(data["turns.total"], 1);
        assert_eq!(data["cost.output_tokens"], 50);
    }

    #[test]
    fn ingest_session_no_exit_code_omits_event() {
        let (_db_dir, conn) = test_db();
        let data_dir = TempDir::new().unwrap();
        let lines =
            &[r#"{"type":"result","duration_ms":1000,"total_cost_usd":0.0,"modelUsage":{}}"#];
        let path = write_jsonl(data_dir.path(), lines);

        let adapter = claude_adapter();
        ingest_session(&conn, 1, &path, None, &adapter).unwrap();

        let events = db::events_by_session(&conn, 1).unwrap();
        // No exit_code event when None
        assert!(events.iter().all(|e| e.kind != "session.exit_code"));
    }

    #[test]
    fn ingest_session_idempotent_observation() {
        let (_db_dir, conn) = test_db();
        let data_dir = TempDir::new().unwrap();
        let lines =
            &[r#"{"type":"result","duration_ms":1000,"total_cost_usd":0.5,"modelUsage":{}}"#];
        let path = write_jsonl(data_dir.path(), lines);

        let adapter = claude_adapter();
        // Ingest twice — observation should be replaced, not duplicated
        ingest_session(&conn, 1, &path, Some(0), &adapter).unwrap();
        ingest_session(&conn, 1, &path, Some(0), &adapter).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM observations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn build_observation_data_roundtrip() {
        let builtin = vec![
            ("turns.total".to_string(), Value::from(42u64)),
            ("turns.parallel".to_string(), Value::from(5u64)),
            (
                "cost.estimate_usd".to_string(),
                serde_json::Number::from_f64(1.5)
                    .map(Value::Number)
                    .unwrap(),
            ),
            ("session.duration_ms".to_string(), Value::from(120000u64)),
        ];
        let json_str = build_observation_data(&builtin, Some(0), &[]);
        let v: Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(v["turns.total"], 42);
        assert_eq!(v["turns.parallel"], 5);
        assert_eq!(v["cost.estimate_usd"], 1.5);
        assert_eq!(v["session.exit_code"], 0);
        assert_eq!(v["session.duration_ms"], 120000);
    }

    #[test]
    fn ingest_real_file_format() {
        // Test with realistic multi-line JSONL mimicking real Claude output
        let (_db_dir, conn) = test_db();
        let data_dir = TempDir::new().unwrap();
        let lines = &[
            r#"{"type":"system","subtype":"hook_started","hook_id":"abc"}"#,
            r#"{"type":"system","subtype":"init","cwd":"/tmp","session_id":"test"}"#,
            r#"{"type":"assistant","message":{"model":"claude-opus-4-6","content":[{"type":"text","text":"Starting work."}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"x"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Reading files"},{"type":"tool_use","name":"Read","id":"t1","input":{"file_path":"/tmp/a"}},{"type":"tool_use","name":"Read","id":"t2","input":{"file_path":"/tmp/b"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Edit","id":"t3","input":{}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Done."}]}}"#,
            r#"{"type":"result","subtype":"success","duration_ms":229857,"num_turns":49,"total_cost_usd":0.99,"modelUsage":{"claude-opus-4-6":{"inputTokens":24,"outputTokens":9407,"cacheReadInputTokens":939227,"cacheCreationInputTokens":37239},"claude-haiku-4-5-20251001":{"inputTokens":47934,"outputTokens":947,"cacheReadInputTokens":0,"cacheCreationInputTokens":0}}}"#,
        ];
        let path = write_jsonl(data_dir.path(), lines);

        let adapter = claude_adapter();
        let result = ingest_session(&conn, 10, &path, Some(0), &adapter).unwrap();
        assert_eq!(result.turns_total, 4); // 4 assistant messages
        assert!((result.cost_estimate_usd - 0.99).abs() < 0.001);
        assert_eq!(result.session_duration_ms, 229857);

        // Verify observation data has detailed metrics
        let obs = db::get_observation(&conn, 10).unwrap().unwrap();
        let data: Value = serde_json::from_str(&obs.data).unwrap();
        assert_eq!(data["turns.total"], 4);
        assert_eq!(data["turns.narration_only"], 2);
        assert_eq!(data["turns.parallel"], 1);
        assert_eq!(data["turns.tool_calls"], 3);
        assert_eq!(data["cost.input_tokens"], 24 + 47934);
        assert_eq!(data["cost.output_tokens"], 9407 + 947);
    }

    // --- Extraction rule tests (using apply_single_rule directly) ---

    use crate::config::ExtractionRule;

    fn make_rule(kind: &str, pattern: &str) -> ExtractionRule {
        ExtractionRule {
            kind: kind.to_string(),
            pattern: pattern.to_string(),
            anti_pattern: None,
            source: "tool_commands".to_string(),
            transform: None,
            first_match: false,
            count: false,
            emit: None,
        }
    }

    #[test]
    fn rule_count_tool_commands() {
        let lines = vec![
            "cargo test".to_string(),
            "cargo clippy".to_string(),
            "cargo test --filter foo".to_string(),
        ];
        let mut rule = make_rule("extract.test_runs", "cargo test");
        rule.count = true;
        let compiled = rule.compile().unwrap();
        let mut results = Vec::new();
        apply_single_rule(&compiled, &lines, &mut results);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "extract.test_runs");
        assert_eq!(results[0].1, "2"); // matches "cargo test" and "cargo test --filter foo"
    }

    #[test]
    fn rule_count_with_anti_pattern() {
        let lines = vec![
            "cargo test".to_string(),
            "cargo test --filter foo".to_string(),
            "cargo test --filter bar".to_string(),
        ];
        let mut rule = make_rule("extract.full_suite", "cargo test");
        rule.count = true;
        rule.anti_pattern = Some("--filter".to_string());
        let compiled = rule.compile().unwrap();
        let mut results = Vec::new();
        apply_single_rule(&compiled, &lines, &mut results);
        assert_eq!(results[0].1, "1"); // only "cargo test" without --filter
    }

    #[test]
    fn rule_emit_boolean() {
        let lines = vec!["./bd-finish.sh bd-xyz".to_string()];
        let mut rule = make_rule("commit.detected", "bd-finish");
        rule.emit = Some(toml::Value::Boolean(true));
        let compiled = rule.compile().unwrap();
        let mut results = Vec::new();
        apply_single_rule(&compiled, &lines, &mut results);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1, "true");
    }

    #[test]
    fn rule_emit_no_match() {
        let lines = vec!["cargo build".to_string()];
        let mut rule = make_rule("commit.detected", "bd-finish");
        rule.emit = Some(toml::Value::Boolean(true));
        let compiled = rule.compile().unwrap();
        let mut results = Vec::new();
        apply_single_rule(&compiled, &lines, &mut results);
        assert!(results.is_empty());
    }

    #[test]
    fn rule_first_match_with_capture_group() {
        let lines = vec![
            "bd update simple-agent-harness-xyz --status in_progress".to_string(),
            "bd update simple-agent-harness-abc --status in_progress".to_string(),
        ];
        let mut rule = make_rule("extract.bead_id", r"bd update (\S+) --status.?in.?progress");
        rule.first_match = true;
        rule.transform = Some("last_segment".to_string());
        let compiled = rule.compile().unwrap();
        let mut results = Vec::new();
        apply_single_rule(&compiled, &lines, &mut results);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1, "xyz"); // last segment of "simple-agent-harness-xyz"
    }

    #[test]
    fn rule_source_text() {
        let lines = vec![
            "Let me check the tests.".to_string(),
            "All tests passed!".to_string(),
        ];
        let mut rule = make_rule("extract.mentions_tests", "tests");
        rule.source = "text".to_string();
        rule.count = true;
        let compiled = rule.compile().unwrap();
        let mut results = Vec::new();
        apply_single_rule(&compiled, &lines, &mut results);
        assert_eq!(results[0].1, "2");
    }

    #[test]
    fn rule_source_raw() {
        let lines = vec![
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{}}]}}"#.to_string(),
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{}}]}}"#.to_string(),
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}"#.to_string(),
        ];
        let mut rule = make_rule("extract.file_reads", r#""name":\s*"Read""#);
        rule.source = "raw".to_string();
        rule.count = true;
        let compiled = rule.compile().unwrap();
        let mut results = Vec::new();
        apply_single_rule(&compiled, &lines, &mut results);
        assert_eq!(results[0].1, "2");
    }

    #[test]
    fn rule_transform_int() {
        let lines = vec!["Found 42 errors".to_string()];
        let mut rule = make_rule("extract.errors", r"Found (\d+) errors");
        rule.source = "text".to_string();
        rule.first_match = true;
        rule.transform = Some("int".to_string());
        let compiled = rule.compile().unwrap();
        let mut results = Vec::new();
        apply_single_rule(&compiled, &lines, &mut results);
        assert_eq!(results[0].1, "42");
    }

    #[test]
    fn rule_transform_trim() {
        let lines = vec!["status:  done  ".to_string()];
        let mut rule = make_rule("extract.status", r"status:\s+(.+)");
        rule.source = "text".to_string();
        rule.first_match = true;
        rule.transform = Some("trim".to_string());
        let compiled = rule.compile().unwrap();
        let mut results = Vec::new();
        apply_single_rule(&compiled, &lines, &mut results);
        assert_eq!(results[0].1, "done");
    }

    #[test]
    fn rule_no_matches_returns_empty() {
        let lines = vec!["cargo build".to_string()];
        let rule = make_rule("extract.missing", "nonexistent_pattern");
        let compiled = rule.compile().unwrap();
        let mut results = Vec::new();
        apply_single_rule(&compiled, &lines, &mut results);
        assert!(results.is_empty());
    }

    #[test]
    fn rule_count_zero_still_emitted() {
        let lines = vec!["cargo build".to_string()];
        let mut rule = make_rule("extract.test_runs", "cargo test");
        rule.count = true;
        let compiled = rule.compile().unwrap();
        let mut results = Vec::new();
        apply_single_rule(&compiled, &lines, &mut results);
        // Count mode always emits (even 0)
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1, "0");
    }

    #[test]
    fn multiple_rules_applied() {
        let lines = vec![
            "cargo test".to_string(),
            "./bd-finish.sh bd-abc".to_string(),
        ];

        let mut r1 = make_rule("extract.test_runs", "cargo test");
        r1.count = true;
        let mut r2 = make_rule("commit.detected", "bd-finish");
        r2.emit = Some(toml::Value::Boolean(true));

        let c1 = r1.compile().unwrap();
        let c2 = r2.compile().unwrap();
        let mut results = Vec::new();
        apply_single_rule(&c1, &lines, &mut results);
        apply_single_rule(&c2, &lines, &mut results);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "extract.test_runs");
        assert_eq!(results[0].1, "1");
        assert_eq!(results[1].0, "commit.detected");
        assert_eq!(results[1].1, "true");
    }

    #[test]
    fn ingest_with_rules_writes_events_and_observation() {
        let (_db_dir, conn) = test_db();
        let data_dir = TempDir::new().unwrap();
        let lines = &[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"./bd-finish.sh bd-xyz"}}]}}"#,
            r#"{"type":"result","duration_ms":5000,"total_cost_usd":0.5,"modelUsage":{}}"#,
        ];
        let path = write_jsonl(data_dir.path(), lines);

        let mut r1 = make_rule("extract.test_runs", "cargo test");
        r1.count = true;
        let mut r2 = make_rule("commit.detected", "bd-finish");
        r2.emit = Some(toml::Value::Boolean(true));

        let c1 = r1.compile().unwrap();
        let c2 = r2.compile().unwrap();

        let adapter = claude_adapter();
        ingest_session_with_rules(&conn, 1, &path, Some(0), &[c1, c2], &adapter).unwrap();

        // Check events include rule-extracted ones
        let events = db::events_by_session(&conn, 1).unwrap();
        let test_runs = events
            .iter()
            .find(|e| e.kind == "extract.test_runs")
            .unwrap();
        assert_eq!(test_runs.value.as_deref(), Some("1"));

        let commit = events.iter().find(|e| e.kind == "commit.detected").unwrap();
        assert_eq!(commit.value.as_deref(), Some("true"));

        // Check observation includes rule-extracted data
        let obs = db::get_observation(&conn, 1).unwrap().unwrap();
        let data: Value = serde_json::from_str(&obs.data).unwrap();
        assert_eq!(data["extract.test_runs"], 1);
        assert_eq!(data["commit.detected"], true);
        // Built-in metrics still present
        assert_eq!(data["turns.total"], 2);
    }

    #[test]
    fn ingest_with_raw_adapter() {
        let (_db_dir, conn) = test_db();
        let data_dir = TempDir::new().unwrap();
        let path = data_dir.path().join("output.txt");
        std::fs::write(&path, "some output\nmore output\n").unwrap();

        let adapter = crate::adapters::raw::RawAdapter::new();
        let result = ingest_session(&conn, 1, &path, Some(0), &adapter).unwrap();
        // Raw adapter returns no built-in metrics
        assert_eq!(result.turns_total, 0);
        assert_eq!(result.cost_estimate_usd, 0.0);

        // But exit_code event should still be written
        let events = db::events_by_session(&conn, 1).unwrap();
        let exit = events
            .iter()
            .find(|e| e.kind == "session.exit_code")
            .unwrap();
        assert_eq!(exit.value.as_deref(), Some("0"));
    }

    #[test]
    fn compile_invalid_pattern_returns_error() {
        let rule = make_rule("bad", "[invalid");
        assert!(rule.compile().is_err());
    }

    #[test]
    fn compile_invalid_anti_pattern_returns_error() {
        let mut rule = make_rule("test", "valid");
        rule.anti_pattern = Some("[invalid".to_string());
        assert!(rule.compile().is_err());
    }

    #[test]
    fn value_to_event_string_integer() {
        assert_eq!(value_to_event_string(&Value::from(42u64)), "42");
    }

    #[test]
    fn value_to_event_string_float() {
        let v = serde_json::Number::from_f64(1.5)
            .map(Value::Number)
            .unwrap();
        assert_eq!(value_to_event_string(&v), "1.500000");
    }

    #[test]
    fn value_to_event_string_string() {
        assert_eq!(
            value_to_event_string(&Value::String("hello".into())),
            "hello"
        );
    }
}
