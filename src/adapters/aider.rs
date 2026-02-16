use super::{AdapterError, AgentAdapter, ExtractionSource};
use serde_json::Value;
use std::io::BufRead;
use std::path::Path;

/// Adapter for Aider's session output / chat log.
///
/// Aider produces plain-text chat transcripts. Key patterns:
///
/// - User prompts appear after `> ` prefixed lines
/// - Assistant responses are free-form text between user prompts
/// - Cost lines match: `Costs: $X.XX session, $Y.YY code ...`
///   or: `Tokens: ... Cost: $X.XX message, $Y.YY session.`
/// - Each assistant response block (between user prompts) counts as one turn
///
/// Supported metrics: turns.total, cost.estimate_usd,
/// session.output_bytes, session.exit_code, session.duration_secs.
pub struct AiderAdapter;

impl AiderAdapter {
    pub fn new() -> Self {
        AiderAdapter
    }
}

impl Default for AiderAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Extracted metrics from an Aider session file.
#[derive(Debug, Default)]
struct RawMetrics {
    turns_total: u64,
    cost_estimate_usd: Option<f64>,
    session_output_bytes: u64,
}

/// Collected text from a session, separated by source type.
#[derive(Debug, Default)]
struct CollectedText {
    raw_lines: Vec<String>,
    text_blocks: Vec<String>,
    tool_commands: Vec<String>,
}

/// Parse an Aider output file and extract metrics and text.
///
/// Aider chat logs are plain text. We detect turn boundaries by looking
/// for user prompt lines (starting with `> `) and count assistant
/// response blocks between them. Cost is extracted from Aider's
/// cost-reporting lines.
fn parse_aider_output(path: &Path) -> Result<(RawMetrics, CollectedText), AdapterError> {
    let file = std::fs::File::open(path)?;
    let file_size = file.metadata()?.len();
    let reader = std::io::BufReader::new(file);

    let mut m = RawMetrics {
        session_output_bytes: file_size,
        ..Default::default()
    };
    let mut text = CollectedText::default();

    // State: are we inside a user prompt or assistant response?
    let mut in_assistant_block = false;
    let mut current_assistant_text = Vec::new();

    for line in reader.lines() {
        let line = line?;
        text.raw_lines.push(line.clone());

        // Check for cost reporting lines
        // Aider formats: "Tokens: Xk sent, Yk received. Cost: $0.05 message, $0.15 session."
        // Or: "Costs: $0.15 session, $0.10 code ..."
        if let Some(cost) = extract_session_cost(&line) {
            m.cost_estimate_usd = Some(cost);
        }

        // Check for shell command lines (aider /run commands)
        // Aider shows: "Running: <command>" or "> /run <command>"
        if let Some(cmd) = line.strip_prefix("Running: ") {
            text.tool_commands.push(cmd.to_string());
        } else if let Some(rest) = line.strip_prefix("> /run ") {
            text.tool_commands.push(rest.to_string());
        }

        // Detect user prompt lines (start with "> " or are just ">")
        let is_user_prompt = line.starts_with("> ") || line == ">";

        if is_user_prompt {
            // End current assistant block if any
            if in_assistant_block && !current_assistant_text.is_empty() {
                m.turns_total += 1;
                text.text_blocks.push(current_assistant_text.join("\n"));
                current_assistant_text.clear();
            }
            in_assistant_block = false;
        } else if !line.is_empty() {
            // Non-empty, non-prompt line — part of assistant response
            if !in_assistant_block {
                in_assistant_block = true;
            }
            current_assistant_text.push(line);
        }
    }

    // Flush final assistant block
    if in_assistant_block && !current_assistant_text.is_empty() {
        m.turns_total += 1;
        text.text_blocks.push(current_assistant_text.join("\n"));
    }

    Ok((m, text))
}

/// Extract the session cost from an Aider cost-reporting line.
///
/// Matches patterns like:
/// - "Tokens: 12k sent, 1.5k received. Cost: $0.05 message, $0.15 session."
/// - "Costs: $0.15 session, $0.10 code ..."
///
/// Returns the session cost value if found.
fn extract_session_cost(line: &str) -> Option<f64> {
    // Pattern 1: "Cost: $X.XX message, $Y.YY session."
    // We want the session cost (last dollar amount before "session")
    if let Some(idx) = line.find("session") {
        // Look backwards from "session" to find the dollar amount
        let before = &line[..idx];
        if let Some(dollar_pos) = before.rfind('$') {
            let cost_str = &before[dollar_pos + 1..];
            let cost_str = cost_str.trim();
            // Parse the number (stop at non-numeric chars except '.')
            let num_str: String = cost_str
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if let Ok(cost) = num_str.parse::<f64>() {
                return Some(cost);
            }
        }
    }
    None
}

const SUPPORTED_METRICS: &[&str] = &[
    "turns.total",
    "cost.estimate_usd",
    "session.output_bytes",
    "session.exit_code",
    "session.duration_secs",
];

impl AgentAdapter for AiderAdapter {
    fn name(&self) -> &str {
        "aider"
    }

    fn extract_builtin_metrics(
        &self,
        output_path: &Path,
    ) -> Result<Vec<(String, Value)>, AdapterError> {
        let (m, _) = parse_aider_output(output_path)?;

        let mut metrics = vec![
            ("turns.total".into(), Value::from(m.turns_total)),
            (
                "session.output_bytes".into(),
                Value::from(m.session_output_bytes),
            ),
        ];

        if let Some(cost) = m.cost_estimate_usd {
            if let Some(n) = serde_json::Number::from_f64(cost) {
                metrics.push(("cost.estimate_usd".into(), Value::Number(n)));
            }
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
        let (_, text) = parse_aider_output(output_path)?;
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

    #[test]
    fn adapter_name() {
        let adapter = AiderAdapter::new();
        assert_eq!(adapter.name(), "aider");
    }

    #[test]
    fn extract_empty_file() {
        let dir = TempDir::new().unwrap();
        let path = write_file(dir.path(), "aider.log", "");
        let adapter = AiderAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let turns = metrics.iter().find(|(k, _)| k == "turns.total").unwrap();
        assert_eq!(turns.1, 0);
    }

    #[test]
    fn extract_turns_from_chat_log() {
        let dir = TempDir::new().unwrap();
        let content = "\
> Fix the bug in main.rs
I'll fix the bug. Let me look at the code.
Here's the change I made.
> Thanks
You're welcome!
";
        let path = write_file(dir.path(), "aider.log", content);
        let adapter = AiderAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let get = |k: &str| metrics.iter().find(|(key, _)| key == k).unwrap().1.clone();
        assert_eq!(get("turns.total"), 2);
    }

    #[test]
    fn extract_single_turn_no_trailing_prompt() {
        let dir = TempDir::new().unwrap();
        let content = "\
> Fix the bug
I fixed it.
Done.
";
        let path = write_file(dir.path(), "aider.log", content);
        let adapter = AiderAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let get = |k: &str| metrics.iter().find(|(key, _)| key == k).unwrap().1.clone();
        assert_eq!(get("turns.total"), 1);
    }

    #[test]
    fn extract_cost_from_token_line() {
        let dir = TempDir::new().unwrap();
        let content = "\
> Fix the bug
I fixed it.
Tokens: 12.3k sent, 1.5k received. Cost: $0.05 message, $0.15 session.
";
        let path = write_file(dir.path(), "aider.log", content);
        let adapter = AiderAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let cost = metrics
            .iter()
            .find(|(k, _)| k == "cost.estimate_usd")
            .unwrap();
        let val = cost.1.as_f64().unwrap();
        assert!((val - 0.15).abs() < 0.001);
    }

    #[test]
    fn extract_cost_from_costs_line() {
        let dir = TempDir::new().unwrap();
        let content = "\
> Do something
Done.
Costs: $0.25 session, $0.10 code edits.
";
        let path = write_file(dir.path(), "aider.log", content);
        let adapter = AiderAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let cost = metrics
            .iter()
            .find(|(k, _)| k == "cost.estimate_usd")
            .unwrap();
        let val = cost.1.as_f64().unwrap();
        assert!((val - 0.25).abs() < 0.001);
    }

    #[test]
    fn no_cost_when_not_reported() {
        let dir = TempDir::new().unwrap();
        let content = "\
> Fix bug
Fixed.
";
        let path = write_file(dir.path(), "aider.log", content);
        let adapter = AiderAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        assert!(!metrics.iter().any(|(k, _)| k == "cost.estimate_usd"));
    }

    #[test]
    fn extract_output_bytes_is_file_size() {
        let dir = TempDir::new().unwrap();
        let content = "> Hello\nHi there!\n";
        let path = write_file(dir.path(), "aider.log", content);
        let expected_size = std::fs::metadata(&path).unwrap().len();
        let adapter = AiderAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let bytes = metrics
            .iter()
            .find(|(k, _)| k == "session.output_bytes")
            .unwrap();
        assert_eq!(bytes.1, expected_size);
    }

    #[test]
    fn file_not_found_returns_error() {
        let adapter = AiderAdapter::new();
        let result = adapter.extract_builtin_metrics(Path::new("/nonexistent/aider.log"));
        assert!(result.is_err());
    }

    #[test]
    fn supported_metrics_list() {
        let adapter = AiderAdapter::new();
        let supported = adapter.supported_metrics();
        assert!(supported.contains(&"turns.total"));
        assert!(supported.contains(&"cost.estimate_usd"));
        assert!(supported.contains(&"session.output_bytes"));
        assert!(supported.contains(&"session.exit_code"));
        assert!(supported.contains(&"session.duration_secs"));
        assert_eq!(supported.len(), 5);
    }

    #[test]
    fn lines_for_source_text() {
        let dir = TempDir::new().unwrap();
        let content = "\
> Fix the bug
I'll fix it now.
Here's the change.
> Thanks
You're welcome!
";
        let path = write_file(dir.path(), "aider.log", content);
        let adapter = AiderAdapter::new();
        let text = adapter
            .lines_for_source(&path, ExtractionSource::Text)
            .unwrap();
        assert_eq!(text.len(), 2);
        assert_eq!(text[0], "I'll fix it now.\nHere's the change.");
        assert_eq!(text[1], "You're welcome!");
    }

    #[test]
    fn lines_for_source_tool_commands() {
        let dir = TempDir::new().unwrap();
        let content = "\
> /run cargo test
Running: cargo test
test result: ok
> Done
All good!
";
        let path = write_file(dir.path(), "aider.log", content);
        let adapter = AiderAdapter::new();
        let cmds = adapter
            .lines_for_source(&path, ExtractionSource::ToolCommands)
            .unwrap();
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0], "cargo test");
        assert_eq!(cmds[1], "cargo test");
    }

    #[test]
    fn lines_for_source_raw() {
        let dir = TempDir::new().unwrap();
        let content = "> Hello\nHi there!\n";
        let path = write_file(dir.path(), "aider.log", content);
        let adapter = AiderAdapter::new();
        let raw = adapter
            .lines_for_source(&path, ExtractionSource::Raw)
            .unwrap();
        assert_eq!(raw.len(), 2);
        assert_eq!(raw[0], "> Hello");
        assert_eq!(raw[1], "Hi there!");
    }

    #[test]
    fn multiple_cost_lines_uses_last() {
        let dir = TempDir::new().unwrap();
        let content = "\
> First question
Answer 1.
Tokens: 5k sent, 1k received. Cost: $0.02 message, $0.02 session.
> Second question
Answer 2.
Tokens: 8k sent, 2k received. Cost: $0.04 message, $0.06 session.
";
        let path = write_file(dir.path(), "aider.log", content);
        let adapter = AiderAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let cost = metrics
            .iter()
            .find(|(k, _)| k == "cost.estimate_usd")
            .unwrap();
        let val = cost.1.as_f64().unwrap();
        // Should use the last session cost reported
        assert!((val - 0.06).abs() < 0.001);
    }

    #[test]
    fn mixed_session() {
        let dir = TempDir::new().unwrap();
        let content = "\
> Fix the login bug
Let me look at the authentication code.
I found the issue in auth.rs.
> /run cargo test
Running: cargo test
test result: ok. 42 passed
> Great, what was the cost?
The fix was straightforward.
Tokens: 25k sent, 3k received. Cost: $0.08 message, $0.12 session.
";
        let path = write_file(dir.path(), "aider.log", content);
        let adapter = AiderAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();

        let get = |k: &str| metrics.iter().find(|(key, _)| key == k).unwrap().1.clone();
        assert_eq!(get("turns.total"), 3); // 3 assistant response blocks
        let cost = get("cost.estimate_usd").as_f64().unwrap();
        assert!((cost - 0.12).abs() < 0.001);

        // Check text extraction
        let text = adapter
            .lines_for_source(&path, ExtractionSource::Text)
            .unwrap();
        assert_eq!(text.len(), 3);
        assert!(text[0].contains("authentication code"));

        // Check tool commands
        let cmds = adapter
            .lines_for_source(&path, ExtractionSource::ToolCommands)
            .unwrap();
        assert_eq!(cmds.len(), 2); // /run line + Running: line
    }

    #[test]
    fn assistant_block_without_user_prompt() {
        // Some aider logs may start with assistant output (e.g., startup messages)
        let dir = TempDir::new().unwrap();
        let content = "\
Aider v0.50.0
Model: gpt-4o
Git repo: /path/to/repo
> Fix bug
Fixed!
";
        let path = write_file(dir.path(), "aider.log", content);
        let adapter = AiderAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        let get = |k: &str| metrics.iter().find(|(key, _)| key == k).unwrap().1.clone();
        // Startup messages count as first block, then the response after "> Fix bug"
        assert_eq!(get("turns.total"), 2);
    }
}
