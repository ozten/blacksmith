use super::{AdapterError, AgentAdapter, ExtractionSource};
use serde_json::Value;
use std::io::BufRead;
use std::path::Path;

/// Adapter for OpenCode session output.
///
/// OpenCode can produce output in two forms:
/// 1. JSONL — one JSON object per line (e.g., streamed messages)
/// 2. Single JSON — an object or array (e.g., session export)
///
/// Supported metrics: turns.total, turns.tool_calls,
/// cost.input_tokens, cost.output_tokens (when available),
/// session.output_bytes, session.exit_code, session.duration_secs.
///
/// OpenCode messages have typed parts:
///   {type: "text", data: {text: "..."}}
///   {type: "toolCall", data: {id: "...", name: "...", input: "..."}}
///   {type: "toolResult", data: {callId: "...", ...}}
///   {type: "finish", data: {reason: "...", timestamp: ...}}
pub struct OpencodeAdapter;

impl OpencodeAdapter {
    pub fn new() -> Self {
        OpencodeAdapter
    }
}

impl Default for OpencodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Extracted metrics from an OpenCode session file.
#[derive(Debug, Default)]
struct RawMetrics {
    turns_total: u64,
    turns_tool_calls: u64,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    session_output_bytes: u64,
    session_exit_code: Option<i64>,
    session_duration_secs: f64,
}

/// Collected text from a session, separated by source type.
#[derive(Debug, Default)]
struct CollectedText {
    raw_lines: Vec<String>,
    text_blocks: Vec<String>,
    tool_commands: Vec<String>,
}

/// Parse an OpenCode output file and extract metrics and text.
///
/// Tries JSONL first (line-by-line). If the first non-empty line isn't
/// a standalone JSON object, falls back to parsing the whole file as a
/// single JSON value (array of messages or session export object).
fn parse_opencode_output(path: &Path) -> Result<(RawMetrics, CollectedText), AdapterError> {
    let file = std::fs::File::open(path)?;
    let file_size = file.metadata()?.len();
    let reader = std::io::BufReader::new(file);

    let mut m = RawMetrics {
        session_output_bytes: file_size,
        ..Default::default()
    };
    let mut text = CollectedText::default();

    // Read all lines; try JSONL first
    let lines: Vec<String> = reader.lines().collect::<Result<_, _>>()?;

    // Collect raw lines for ExtractionSource::Raw
    text.raw_lines = lines.iter().filter(|l| !l.is_empty()).cloned().collect();

    // Detect format: if the entire content parses as a single JSON value
    // that is an array or object with a "messages" key, use single-JSON mode.
    let full_content: String = lines.join("\n");
    let messages: Vec<Value> = if let Ok(val) = serde_json::from_str::<Value>(&full_content) {
        // Extract session-level metadata (tokens, cost) from the wrapper object
        extract_session_metadata(&val, &mut m);
        extract_messages_from_value(&val)
    } else {
        // JSONL mode: each line is a separate JSON object
        lines
            .iter()
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .collect()
    };

    // Track timestamps for duration calculation
    let mut first_timestamp: Option<f64> = None;
    let mut last_timestamp: Option<f64> = None;

    for msg in &messages {
        process_message(
            msg,
            &mut m,
            &mut text,
            &mut first_timestamp,
            &mut last_timestamp,
        );
    }

    // Calculate duration from first to last timestamp
    if let (Some(first), Some(last)) = (first_timestamp, last_timestamp) {
        m.session_duration_secs = (last - first).max(0.0);
    }

    Ok((m, text))
}

/// Extract session-level metadata (tokens, timestamps) from a wrapper object.
///
/// OpenCode session exports may include `prompt_tokens`, `completion_tokens`,
/// `cost`, `created_at`, `updated_at` at the top level or under a `session` key.
fn extract_session_metadata(val: &Value, m: &mut RawMetrics) {
    // Check top-level object
    extract_metadata_from_object(val, m);

    // Check nested "session" object
    if let Some(session) = val.get("session") {
        extract_metadata_from_object(session, m);
    }
}

fn extract_metadata_from_object(obj: &Value, m: &mut RawMetrics) {
    if let Some(pt) = obj.get("prompt_tokens").and_then(|t| t.as_u64()) {
        *m.input_tokens.get_or_insert(0) += pt;
    }
    if let Some(ct) = obj.get("completion_tokens").and_then(|t| t.as_u64()) {
        *m.output_tokens.get_or_insert(0) += ct;
    }
}

/// Extract messages from a single JSON value.
///
/// Handles these shapes:
/// - Array of messages: `[{role, parts, ...}, ...]`
/// - Object with "messages" key: `{messages: [{role, parts, ...}, ...]}`
/// - Object with "session" containing "messages": `{session: {messages: [...]}}`
/// - Single message object: `{role: "assistant", parts: [...]}`
fn extract_messages_from_value(val: &Value) -> Vec<Value> {
    // Direct array of messages
    if let Some(arr) = val.as_array() {
        return arr.clone();
    }

    // Object with "messages" key
    if let Some(msgs) = val.get("messages").and_then(|m| m.as_array()) {
        return msgs.clone();
    }

    // Nested session.messages
    if let Some(msgs) = val
        .get("session")
        .and_then(|s| s.get("messages"))
        .and_then(|m| m.as_array())
    {
        return msgs.clone();
    }

    // Single message-like object (has "role" or "parts")
    if val.get("role").is_some() || val.get("parts").is_some() {
        return vec![val.clone()];
    }

    vec![]
}

/// Process a single message object and accumulate metrics/text.
fn process_message(
    msg: &Value,
    m: &mut RawMetrics,
    text: &mut CollectedText,
    first_ts: &mut Option<f64>,
    last_ts: &mut Option<f64>,
) {
    // Track timestamps from message-level fields
    for ts_field in &["created_at", "finished_at", "timestamp", "updated_at"] {
        if let Some(ts) = msg.get(ts_field).and_then(|t| t.as_f64()) {
            update_timestamps(ts, first_ts, last_ts);
        }
    }

    // Track token usage from message-level "usage" field
    if let Some(usage) = msg.get("usage") {
        accumulate_tokens(usage, m);
    }

    // Track session-level token/cost fields (from session export)
    if let Some(pt) = msg.get("prompt_tokens").and_then(|t| t.as_u64()) {
        *m.input_tokens.get_or_insert(0) += pt;
    }
    if let Some(ct) = msg.get("completion_tokens").and_then(|t| t.as_u64()) {
        *m.output_tokens.get_or_insert(0) += ct;
    }

    let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");

    // Count assistant messages as turns
    if role == "assistant" {
        m.turns_total += 1;
    }

    // Process parts array — only extract text/tools from assistant messages
    if let Some(parts) = msg.get("parts").and_then(|p| p.as_array()) {
        for part in parts {
            process_part(part, m, text, first_ts, last_ts, role);
        }
    }
}

/// Accumulate token usage from a "usage" object.
fn accumulate_tokens(usage: &Value, m: &mut RawMetrics) {
    if let Some(input) = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(|t| t.as_u64())
    {
        *m.input_tokens.get_or_insert(0) += input;
    }
    if let Some(output) = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(|t| t.as_u64())
    {
        *m.output_tokens.get_or_insert(0) += output;
    }
}

/// Update first/last timestamp tracking.
fn update_timestamps(ts: f64, first_ts: &mut Option<f64>, last_ts: &mut Option<f64>) {
    if ts > 0.0 {
        if first_ts.is_none() || ts < first_ts.unwrap() {
            *first_ts = Some(ts);
        }
        if last_ts.is_none() || ts > last_ts.unwrap() {
            *last_ts = Some(ts);
        }
    }
}

/// Process a single part from a message's parts array.
///
/// Parts have the shape: `{type: "text"|"toolCall"|"toolResult"|"finish", data: {...}}`
/// Only collects text/tool data from assistant messages.
fn process_part(
    part: &Value,
    m: &mut RawMetrics,
    text: &mut CollectedText,
    first_ts: &mut Option<f64>,
    last_ts: &mut Option<f64>,
    role: &str,
) {
    let part_type = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let data = part.get("data").unwrap_or(part);

    match part_type {
        "text" => {
            if role == "assistant" {
                if let Some(t) = data.get("text").and_then(|t| t.as_str()) {
                    text.text_blocks.push(t.to_string());
                }
            }
        }
        "toolCall" | "tool_call" => {
            if role == "assistant" {
                m.turns_tool_calls += 1;
                // Extract tool name or command for source mapping
                if let Some(name) = data
                    .get("name")
                    .or_else(|| data.get("command"))
                    .and_then(|n| n.as_str())
                {
                    let input_str = data
                        .get("input")
                        .and_then(|i| {
                            // Input can be a string or an object
                            if let Some(s) = i.as_str() {
                                Some(s.to_string())
                            } else {
                                serde_json::to_string(i).ok()
                            }
                        })
                        .unwrap_or_default();
                    if input_str.is_empty() {
                        text.tool_commands.push(name.to_string());
                    } else {
                        text.tool_commands.push(format!("{} {}", name, input_str));
                    }
                }
            }
        }
        "toolResult" | "tool_result" => {
            // Check for exit code in tool results (from any role)
            if let Some(code) = data.get("exit_code").and_then(|c| c.as_i64()) {
                m.session_exit_code = Some(code);
            }
        }
        "finish" => {
            // Extract timestamp from finish data (from any role)
            if let Some(ts) = data.get("timestamp").and_then(|t| t.as_f64()) {
                update_timestamps(ts, first_ts, last_ts);
            }
            if let Some(code) = data.get("exit_code").and_then(|c| c.as_i64()) {
                m.session_exit_code = Some(code);
            }
        }
        _ => {}
    }
}

const SUPPORTED_METRICS: &[&str] = &[
    "turns.total",
    "turns.tool_calls",
    "cost.input_tokens",
    "cost.output_tokens",
    "session.output_bytes",
    "session.exit_code",
    "session.duration_secs",
];

impl AgentAdapter for OpencodeAdapter {
    fn name(&self) -> &str {
        "opencode"
    }

    fn extract_builtin_metrics(
        &self,
        output_path: &Path,
    ) -> Result<Vec<(String, Value)>, AdapterError> {
        let (m, _) = parse_opencode_output(output_path)?;

        let mut metrics = vec![
            ("turns.total".into(), Value::from(m.turns_total)),
            ("turns.tool_calls".into(), Value::from(m.turns_tool_calls)),
            (
                "session.output_bytes".into(),
                Value::from(m.session_output_bytes),
            ),
            (
                "session.duration_secs".into(),
                serde_json::Number::from_f64(m.session_duration_secs)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
            ),
        ];

        if let Some(tokens) = m.input_tokens {
            metrics.push(("cost.input_tokens".into(), Value::from(tokens)));
        }
        if let Some(tokens) = m.output_tokens {
            metrics.push(("cost.output_tokens".into(), Value::from(tokens)));
        }
        if let Some(code) = m.session_exit_code {
            metrics.push(("session.exit_code".into(), Value::from(code)));
        }

        Ok(metrics)
    }

    fn supported_metrics(&self) -> &[&str] {
        SUPPORTED_METRICS
    }

    fn lines_for_source(
        &self,
        output_path: &Path,
        source: ExtractionSource,
    ) -> Result<Vec<String>, AdapterError> {
        let (_, text) = parse_opencode_output(output_path)?;
        Ok(match source {
            ExtractionSource::ToolCommands => text.tool_commands,
            ExtractionSource::Text => text.text_blocks,
            ExtractionSource::Raw => text.raw_lines,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_file(dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    fn write_jsonl(dir: &Path, lines: &[&str]) -> std::path::PathBuf {
        write_file(dir, "opencode-session.jsonl", &lines.join("\n"))
    }

    #[test]
    fn adapter_name() {
        let adapter = OpencodeAdapter::new();
        assert_eq!(adapter.name(), "opencode");
    }

    #[test]
    fn extract_empty_file() {
        let dir = TempDir::new().unwrap();
        let path = write_jsonl(dir.path(), &[]);
        let adapter = OpencodeAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let turns = metrics.iter().find(|(k, _)| k == "turns.total").unwrap();
        assert_eq!(turns.1, 0);
    }

    #[test]
    fn extract_turns_from_assistant_messages_jsonl() {
        let dir = TempDir::new().unwrap();
        let lines = &[
            r#"{"role":"user","parts":[{"type":"text","data":{"text":"Fix the bug"}}],"created_at":1000.0}"#,
            r#"{"role":"assistant","parts":[{"type":"text","data":{"text":"I'll fix it."}}],"created_at":1001.0}"#,
            r#"{"role":"user","parts":[{"type":"text","data":{"text":"Thanks"}}],"created_at":1002.0}"#,
            r#"{"role":"assistant","parts":[{"type":"text","data":{"text":"Done."}}],"created_at":1003.0}"#,
        ];
        let path = write_jsonl(dir.path(), lines);
        let adapter = OpencodeAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let get = |k: &str| metrics.iter().find(|(key, _)| key == k).unwrap().1.clone();
        assert_eq!(get("turns.total"), 2);
    }

    #[test]
    fn extract_tool_calls_from_parts() {
        let dir = TempDir::new().unwrap();
        let lines = &[
            r#"{"role":"assistant","parts":[{"type":"toolCall","data":{"id":"tc1","name":"read_file","input":"src/main.rs"}},{"type":"toolCall","data":{"id":"tc2","name":"write_file","input":"{\"path\":\"out.rs\"}"}}],"created_at":1000.0}"#,
        ];
        let path = write_jsonl(dir.path(), lines);
        let adapter = OpencodeAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let get = |k: &str| metrics.iter().find(|(key, _)| key == k).unwrap().1.clone();
        assert_eq!(get("turns.tool_calls"), 2);
        assert_eq!(get("turns.total"), 1);
    }

    #[test]
    fn extract_tokens_from_usage() {
        let dir = TempDir::new().unwrap();
        let lines = &[
            r#"{"role":"assistant","parts":[{"type":"text","data":{"text":"Hello"}}],"usage":{"input_tokens":500,"output_tokens":100},"created_at":1000.0}"#,
            r#"{"role":"assistant","parts":[{"type":"text","data":{"text":"Done"}}],"usage":{"input_tokens":800,"output_tokens":200},"created_at":1001.0}"#,
        ];
        let path = write_jsonl(dir.path(), lines);
        let adapter = OpencodeAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let get = |k: &str| metrics.iter().find(|(key, _)| key == k).unwrap().1.clone();
        assert_eq!(get("cost.input_tokens"), 1300);
        assert_eq!(get("cost.output_tokens"), 300);
    }

    #[test]
    fn extract_tokens_with_prompt_completion_keys() {
        let dir = TempDir::new().unwrap();
        let lines = &[
            r#"{"role":"assistant","parts":[{"type":"text","data":{"text":"Hi"}}],"usage":{"prompt_tokens":400,"completion_tokens":50},"created_at":1000.0}"#,
        ];
        let path = write_jsonl(dir.path(), lines);
        let adapter = OpencodeAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let get = |k: &str| metrics.iter().find(|(key, _)| key == k).unwrap().1.clone();
        assert_eq!(get("cost.input_tokens"), 400);
        assert_eq!(get("cost.output_tokens"), 50);
    }

    #[test]
    fn no_tokens_when_not_available() {
        let dir = TempDir::new().unwrap();
        let lines = &[
            r#"{"role":"assistant","parts":[{"type":"text","data":{"text":"Hello"}}],"created_at":1000.0}"#,
        ];
        let path = write_jsonl(dir.path(), lines);
        let adapter = OpencodeAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        assert!(!metrics.iter().any(|(k, _)| k == "cost.input_tokens"));
        assert!(!metrics.iter().any(|(k, _)| k == "cost.output_tokens"));
    }

    #[test]
    fn extract_duration_from_timestamps() {
        let dir = TempDir::new().unwrap();
        let lines = &[
            r#"{"role":"user","parts":[],"created_at":1000.0}"#,
            r#"{"role":"assistant","parts":[{"type":"finish","data":{"reason":"end_turn","timestamp":1060.5}}],"created_at":1001.0,"finished_at":1060.5}"#,
        ];
        let path = write_jsonl(dir.path(), lines);
        let adapter = OpencodeAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let duration = metrics
            .iter()
            .find(|(k, _)| k == "session.duration_secs")
            .unwrap();
        let secs = duration.1.as_f64().unwrap();
        assert!((secs - 60.5).abs() < 0.01);
    }

    #[test]
    fn extract_exit_code_from_tool_result() {
        let dir = TempDir::new().unwrap();
        let lines = &[
            r#"{"role":"tool","parts":[{"type":"toolResult","data":{"callId":"tc1","exit_code":1}}],"created_at":1000.0}"#,
        ];
        let path = write_jsonl(dir.path(), lines);
        let adapter = OpencodeAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let exit_code = metrics
            .iter()
            .find(|(k, _)| k == "session.exit_code")
            .unwrap();
        assert_eq!(exit_code.1, 1);
    }

    #[test]
    fn extract_output_bytes_is_file_size() {
        let dir = TempDir::new().unwrap();
        let lines = &[r#"{"role":"assistant","parts":[],"created_at":1000.0}"#];
        let path = write_jsonl(dir.path(), lines);
        let expected_size = std::fs::metadata(&path).unwrap().len();
        let adapter = OpencodeAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let bytes = metrics
            .iter()
            .find(|(k, _)| k == "session.output_bytes")
            .unwrap();
        assert_eq!(bytes.1, expected_size);
    }

    #[test]
    fn extract_skips_malformed_lines() {
        let dir = TempDir::new().unwrap();
        let lines = &[
            "not valid json",
            r#"{"role":"assistant","parts":[{"type":"text","data":{"text":"OK"}}],"created_at":1000.0}"#,
            "{broken",
        ];
        let path = write_jsonl(dir.path(), lines);
        let adapter = OpencodeAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let turns = metrics.iter().find(|(k, _)| k == "turns.total").unwrap();
        assert_eq!(turns.1, 1);
    }

    #[test]
    fn file_not_found_returns_error() {
        let adapter = OpencodeAdapter::new();
        let result = adapter.extract_builtin_metrics(Path::new("/nonexistent/file.json"));
        assert!(result.is_err());
    }

    #[test]
    fn supported_metrics_list() {
        let adapter = OpencodeAdapter::new();
        let supported = adapter.supported_metrics();
        assert!(supported.contains(&"turns.total"));
        assert!(supported.contains(&"turns.tool_calls"));
        assert!(supported.contains(&"cost.input_tokens"));
        assert!(supported.contains(&"cost.output_tokens"));
        assert!(supported.contains(&"session.output_bytes"));
        assert!(supported.contains(&"session.exit_code"));
        assert!(supported.contains(&"session.duration_secs"));
        assert_eq!(supported.len(), 7);
        // Should NOT contain narration or parallel metrics
        assert!(!supported.contains(&"turns.narration_only"));
        assert!(!supported.contains(&"turns.parallel"));
    }

    // --- Single JSON format tests ---

    #[test]
    fn parse_json_array_of_messages() {
        let dir = TempDir::new().unwrap();
        let content = r#"[
            {"role":"user","parts":[{"type":"text","data":{"text":"Hello"}}],"created_at":1000.0},
            {"role":"assistant","parts":[{"type":"text","data":{"text":"Hi there"}}],"usage":{"input_tokens":100,"output_tokens":20},"created_at":1001.0}
        ]"#;
        let path = write_file(dir.path(), "session.json", content);
        let adapter = OpencodeAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let get = |k: &str| metrics.iter().find(|(key, _)| key == k).unwrap().1.clone();
        assert_eq!(get("turns.total"), 1);
        assert_eq!(get("cost.input_tokens"), 100);
        assert_eq!(get("cost.output_tokens"), 20);
    }

    #[test]
    fn parse_json_object_with_messages_key() {
        let dir = TempDir::new().unwrap();
        let content = r#"{
            "messages": [
                {"role":"user","parts":[{"type":"text","data":{"text":"Fix bug"}}],"created_at":1000.0},
                {"role":"assistant","parts":[{"type":"text","data":{"text":"Fixed"}}],"created_at":1002.0},
                {"role":"assistant","parts":[{"type":"text","data":{"text":"Done"}}],"created_at":1003.0}
            ]
        }"#;
        let path = write_file(dir.path(), "session.json", content);
        let adapter = OpencodeAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let get = |k: &str| metrics.iter().find(|(key, _)| key == k).unwrap().1.clone();
        assert_eq!(get("turns.total"), 2);
    }

    #[test]
    fn parse_nested_session_messages() {
        let dir = TempDir::new().unwrap();
        let content = r#"{
            "session": {
                "id": "sess-1",
                "messages": [
                    {"role":"assistant","parts":[{"type":"text","data":{"text":"Hello"}}],"created_at":1000.0}
                ]
            }
        }"#;
        let path = write_file(dir.path(), "session.json", content);
        let adapter = OpencodeAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let get = |k: &str| metrics.iter().find(|(key, _)| key == k).unwrap().1.clone();
        assert_eq!(get("turns.total"), 1);
    }

    #[test]
    fn parse_session_level_tokens() {
        let dir = TempDir::new().unwrap();
        // OpenCode session export may have token counts at session level
        let content = r#"{
            "prompt_tokens": 5000,
            "completion_tokens": 1200,
            "messages": [
                {"role":"assistant","parts":[{"type":"text","data":{"text":"Done"}}],"created_at":1000.0}
            ]
        }"#;
        let path = write_file(dir.path(), "session.json", content);
        let adapter = OpencodeAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let get = |k: &str| metrics.iter().find(|(key, _)| key == k).unwrap().1.clone();
        // Session-level tokens should be picked up from the top-level object
        // (it's treated as a message too, which picks up prompt_tokens/completion_tokens)
        assert_eq!(get("cost.input_tokens"), 5000);
        assert_eq!(get("cost.output_tokens"), 1200);
    }

    // --- lines_for_source tests ---

    #[test]
    fn lines_for_source_tool_commands() {
        let dir = TempDir::new().unwrap();
        let lines = &[
            r#"{"role":"assistant","parts":[{"type":"toolCall","data":{"id":"tc1","name":"bash","input":"cargo test"}}],"created_at":1000.0}"#,
            r#"{"role":"assistant","parts":[{"type":"toolCall","data":{"id":"tc2","name":"read_file","input":"src/main.rs"}}],"created_at":1001.0}"#,
        ];
        let path = write_jsonl(dir.path(), lines);
        let adapter = OpencodeAdapter::new();
        let cmds = adapter
            .lines_for_source(&path, ExtractionSource::ToolCommands)
            .unwrap();
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0], "bash cargo test");
        assert_eq!(cmds[1], "read_file src/main.rs");
    }

    #[test]
    fn lines_for_source_text() {
        let dir = TempDir::new().unwrap();
        let lines = &[
            r#"{"role":"assistant","parts":[{"type":"text","data":{"text":"I'll help you."}}],"created_at":1000.0}"#,
            r#"{"role":"assistant","parts":[{"type":"text","data":{"text":"Done."}}],"created_at":1001.0}"#,
        ];
        let path = write_jsonl(dir.path(), lines);
        let adapter = OpencodeAdapter::new();
        let text = adapter
            .lines_for_source(&path, ExtractionSource::Text)
            .unwrap();
        assert_eq!(text.len(), 2);
        assert_eq!(text[0], "I'll help you.");
        assert_eq!(text[1], "Done.");
    }

    #[test]
    fn lines_for_source_raw() {
        let dir = TempDir::new().unwrap();
        let lines = &[
            r#"{"role":"user","parts":[]}"#,
            r#"{"role":"assistant","parts":[]}"#,
        ];
        let path = write_jsonl(dir.path(), lines);
        let adapter = OpencodeAdapter::new();
        let raw = adapter
            .lines_for_source(&path, ExtractionSource::Raw)
            .unwrap();
        assert_eq!(raw.len(), 2);
    }

    #[test]
    fn tool_call_with_snake_case_type() {
        let dir = TempDir::new().unwrap();
        let lines = &[
            r#"{"role":"assistant","parts":[{"type":"tool_call","data":{"id":"tc1","name":"grep","input":"pattern"}}],"created_at":1000.0}"#,
        ];
        let path = write_jsonl(dir.path(), lines);
        let adapter = OpencodeAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let get = |k: &str| metrics.iter().find(|(key, _)| key == k).unwrap().1.clone();
        assert_eq!(get("turns.tool_calls"), 1);
    }

    #[test]
    fn no_exit_code_when_no_tool_results() {
        let dir = TempDir::new().unwrap();
        let lines = &[
            r#"{"role":"assistant","parts":[{"type":"text","data":{"text":"Hello"}}],"created_at":1000.0}"#,
        ];
        let path = write_jsonl(dir.path(), lines);
        let adapter = OpencodeAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        assert!(!metrics.iter().any(|(k, _)| k == "session.exit_code"));
    }

    #[test]
    fn mixed_session() {
        let dir = TempDir::new().unwrap();
        let lines = &[
            r#"{"role":"user","parts":[{"type":"text","data":{"text":"Fix the bug"}}],"created_at":100.0}"#,
            r#"{"role":"assistant","parts":[{"type":"text","data":{"text":"Let me check."}},{"type":"toolCall","data":{"id":"tc1","name":"read_file","input":"src/main.rs"}}],"usage":{"input_tokens":500,"output_tokens":100},"created_at":101.0}"#,
            r#"{"role":"tool","parts":[{"type":"toolResult","data":{"callId":"tc1","exit_code":0}}],"created_at":102.0}"#,
            r#"{"role":"assistant","parts":[{"type":"text","data":{"text":"I fixed it."}},{"type":"toolCall","data":{"id":"tc2","name":"write_file","input":"{\"path\":\"src/main.rs\"}"}}],"usage":{"input_tokens":800,"output_tokens":200},"created_at":103.0}"#,
            r#"{"role":"tool","parts":[{"type":"toolResult","data":{"callId":"tc2","exit_code":0}}],"created_at":104.0}"#,
            r#"{"role":"assistant","parts":[{"type":"text","data":{"text":"Done!"}},{"type":"finish","data":{"reason":"end_turn","timestamp":160.0}}],"created_at":105.0,"finished_at":160.0}"#,
        ];
        let path = write_jsonl(dir.path(), lines);
        let adapter = OpencodeAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();

        let get = |k: &str| metrics.iter().find(|(key, _)| key == k).unwrap().1.clone();
        assert_eq!(get("turns.total"), 3); // 3 assistant messages
        assert_eq!(get("turns.tool_calls"), 2); // 2 tool calls
        assert_eq!(get("cost.input_tokens"), 1300);
        assert_eq!(get("cost.output_tokens"), 300);
        let duration = get("session.duration_secs").as_f64().unwrap();
        assert!((duration - 60.0).abs() < 0.01); // 160.0 - 100.0

        // Check text extraction
        let text = adapter
            .lines_for_source(&path, ExtractionSource::Text)
            .unwrap();
        assert_eq!(text.len(), 3);
        assert_eq!(text[0], "Let me check.");
        assert_eq!(text[2], "Done!");

        // Check tool commands
        let cmds = adapter
            .lines_for_source(&path, ExtractionSource::ToolCommands)
            .unwrap();
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0], "read_file src/main.rs");
    }
}
