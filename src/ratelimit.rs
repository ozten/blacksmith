/// Rate limit and quota exhaustion detection for agent session JSONL output.
///
/// Supports two JSONL formats:
/// - **Claude**: last `"type":"result"` event with `is_error`/`subtype` fields
/// - **Codex**: `"type":"error"` or `"type":"turn.failed"` events with `message`/`error.message`
///
/// Rate limit patterns: `rate limit`, `rate_limit`, `usage limit`, `hit your limit`
/// Quota patterns: `usage limit`, `hit your limit`, `purchase more credits`, `upgrade to`
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

/// Compiled regex patterns for rate limit detection within result events.
static RATE_LIMIT_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?i)rate[_.\s]limit").unwrap(),
        Regex::new(r"(?i)usage limit").unwrap(),
        Regex::new(r"(?i)hit your limit").unwrap(),
    ]
});

/// Patterns that indicate hard quota exhaustion (not a transient rate limit).
static QUOTA_EXHAUSTION_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?i)usage limit").unwrap(),
        Regex::new(r"(?i)hit your (?:usage )?limit").unwrap(),
        Regex::new(r"(?i)purchase more credits").unwrap(),
        Regex::new(r"(?i)upgrade to (?:pro|plus|team)").unwrap(),
    ]
});

/// Patterns that indicate authentication failure (invalid/expired API key).
static AUTH_FAILURE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?i)invalid api key").unwrap(),
        Regex::new(r"(?i)authentication_failed").unwrap(),
        Regex::new(r"(?i)invalid x-api-key").unwrap(),
        Regex::new(r"(?i)api key.*expired").unwrap(),
        Regex::new(r"(?i)unauthorized.*api.key").unwrap(),
    ]
});

/// Inspect a JSONL file for rate limit indicators.
///
/// Returns `true` if the session ended with a rate limit error.
/// Supports both Claude (`"type":"result"`) and Codex (`"type":"error"`) formats.
pub fn detect_rate_limit(output_path: &Path) -> bool {
    let contents = match std::fs::read_to_string(output_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %output_path.display(),
                "failed to read output file for rate limit check"
            );
            return false;
        }
    };

    detect_rate_limit_in_content(&contents)
}

/// Inspect a JSONL file for hard quota exhaustion.
///
/// Returns `Some(message)` with the quota error message if detected,
/// `None` if the session didn't fail due to quota.
pub fn detect_quota_exhaustion(output_path: &Path) -> Option<String> {
    let contents = match std::fs::read_to_string(output_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %output_path.display(),
                "failed to read output file for quota check"
            );
            return None;
        }
    };

    detect_quota_in_content(&contents)
}

/// Inspect a JSONL file for authentication failure.
///
/// Returns `Some(message)` if an auth failure is detected, `None` otherwise.
/// Auth failures are fatal — the API key is invalid/expired and retrying won't help.
pub fn detect_auth_failure(output_path: &Path) -> Option<String> {
    let contents = match std::fs::read_to_string(output_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %output_path.display(),
                "failed to read output file for auth check"
            );
            return None;
        }
    };

    detect_auth_failure_in_content(&contents)
}

/// Detect authentication failure in JSONL content.
/// Checks both the `"error"` field on assistant messages and the result event text.
fn detect_auth_failure_in_content(jsonl_content: &str) -> Option<String> {
    for line in jsonl_content.lines().rev() {
        // Claude format: assistant message with "error":"authentication_failed"
        if line.contains("\"error\"") || line.contains("\"type\":\"result\"") {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line) {
                // Check the "error" field on assistant messages
                if let Some(error_str) = parsed["error"].as_str() {
                    if matches_auth_patterns(error_str) {
                        let msg = parsed["message"]["content"][0]["text"]
                            .as_str()
                            .unwrap_or(error_str);
                        return Some(msg.to_string());
                    }
                }
                // Check the "result" text on result events
                let is_error = parsed["is_error"].as_bool().unwrap_or(false);
                if is_error {
                    if let Some(result_text) = parsed["result"].as_str() {
                        if matches_auth_patterns(result_text) {
                            return Some(result_text.to_string());
                        }
                    }
                }
            }
        }
        // Codex format: error events
        if line.contains("\"type\":\"error\"") || line.contains("\"type\":\"turn.failed\"") {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line) {
                let msg = parsed["message"]
                    .as_str()
                    .or_else(|| parsed["error"]["message"].as_str())
                    .unwrap_or("");
                if matches_auth_patterns(msg) {
                    return Some(msg.to_string());
                }
            }
        }
    }
    None
}

/// Check text for authentication failure patterns.
fn matches_auth_patterns(text: &str) -> bool {
    AUTH_FAILURE_PATTERNS.iter().any(|p| p.is_match(text))
}

/// Extract `apiKeySource` from a JSONL session file's init event.
///
/// Returns the value of the `apiKeySource` field (e.g. "ANTHROPIC_API_KEY")
/// or `None` if the init event doesn't contain one.
pub fn extract_api_key_source(output_path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(output_path).ok()?;
    for line in contents.lines() {
        if line.contains("\"type\":\"system\"") && line.contains("\"subtype\":\"init\"") {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line) {
                return parsed["apiKeySource"].as_str().map(|s| s.to_string());
            }
        }
    }
    None
}

/// Detect rate limiting in JSONL content (any format).
fn detect_rate_limit_in_content(jsonl_content: &str) -> bool {
    // Try Claude format first
    if detect_rate_limit_in_result_event(jsonl_content) {
        return true;
    }
    // Try Codex/generic format: look for error events
    detect_rate_limit_in_error_events(jsonl_content)
}

/// Detect quota exhaustion in JSONL content (any format).
/// Returns the error message if quota exhaustion is detected.
fn detect_quota_in_content(jsonl_content: &str) -> Option<String> {
    // Scan all lines for error events with quota patterns
    for line in jsonl_content.lines().rev() {
        // Claude format: "type":"result" with is_error
        if line.contains("\"type\":\"result\"") {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line) {
                let is_error = parsed["is_error"].as_bool().unwrap_or(false);
                let subtype = parsed["subtype"].as_str().unwrap_or("");
                if is_error || subtype == "error" {
                    let text = parsed["result"].as_str().unwrap_or("");
                    if matches_quota_patterns(text) {
                        return Some(text.to_string());
                    }
                }
            }
        }
        // Codex format: "type":"error" or "type":"turn.failed"
        if line.contains("\"type\":\"error\"") || line.contains("\"type\":\"turn.failed\"") {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line) {
                // Check "message" field (type:error) or "error.message" (type:turn.failed)
                let msg = parsed["message"]
                    .as_str()
                    .or_else(|| parsed["error"]["message"].as_str())
                    .unwrap_or("");
                if matches_quota_patterns(msg) {
                    return Some(msg.to_string());
                }
            }
        }
    }
    None
}

/// Find the last `"type":"result"` line in JSONL content and check for rate limiting.
/// (Claude format)
fn detect_rate_limit_in_result_event(jsonl_content: &str) -> bool {
    let result_line = jsonl_content
        .lines()
        .rev()
        .find(|line| line.contains("\"type\":\"result\""));

    let result_line = match result_line {
        Some(line) => line,
        None => return false,
    };

    let parsed: serde_json::Value = match serde_json::from_str(result_line) {
        Ok(v) => v,
        Err(_) => return false,
    };

    let is_error = parsed["is_error"].as_bool().unwrap_or(false);
    let subtype = parsed["subtype"].as_str().unwrap_or("");

    if !is_error && subtype != "error" {
        return false;
    }

    detect_rate_limit_in_text(result_line)
}

/// Check for rate limiting in Codex-format error events.
/// Looks for "type":"error" and "type":"turn.failed" events.
fn detect_rate_limit_in_error_events(jsonl_content: &str) -> bool {
    for line in jsonl_content.lines().rev() {
        if (line.contains("\"type\":\"error\"") || line.contains("\"type\":\"turn.failed\""))
            && detect_rate_limit_in_text(line)
        {
            return true;
        }
    }
    false
}

/// Check text content for rate limit patterns.
fn detect_rate_limit_in_text(text: &str) -> bool {
    for pattern in RATE_LIMIT_PATTERNS.iter() {
        if pattern.is_match(text) {
            tracing::debug!(pattern = %pattern, "rate limit pattern matched");
            return true;
        }
    }
    false
}

/// Check text for quota exhaustion patterns.
fn matches_quota_patterns(text: &str) -> bool {
    QUOTA_EXHAUSTION_PATTERNS.iter().any(|p| p.is_match(text))
}

/// Calculate exponential backoff delay for rate limiting.
///
/// Returns `initial_delay * 2^consecutive_count`, capped at `max_delay`.
pub fn backoff_delay(initial_delay_secs: u64, consecutive_count: u32, max_delay_secs: u64) -> u64 {
    let shift = 1u64.checked_shl(consecutive_count).unwrap_or(u64::MAX);
    let delay = initial_delay_secs.saturating_mul(shift);
    delay.min(max_delay_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Helper: build a result event JSON line.
    fn result_event(is_error: bool, subtype: &str, result_text: &str) -> String {
        serde_json::json!({
            "type": "result",
            "subtype": subtype,
            "is_error": is_error,
            "result": result_text,
            "duration_ms": 1000,
            "session_id": "test-session"
        })
        .to_string()
    }

    /// Helper: build a JSONL file with tool output lines + a final result event.
    fn jsonl_with_result(
        tool_lines: &[&str],
        is_error: bool,
        subtype: &str,
        result_text: &str,
    ) -> String {
        let mut lines: Vec<String> = tool_lines.iter().map(|l| l.to_string()).collect();
        lines.push(result_event(is_error, subtype, result_text));
        lines.join("\n")
    }

    // --- Result event detection tests ---

    #[test]
    fn test_error_result_with_rate_limit_detected() {
        let jsonl = result_event(
            true,
            "error",
            "You have been rate limited. Please try again.",
        );
        assert!(detect_rate_limit_in_result_event(&jsonl));
    }

    #[test]
    fn test_error_result_with_rate_limit_json_error() {
        let line =
            r#"{"type":"result","subtype":"error","is_error":true,"result":"error: rate_limit"}"#;
        assert!(detect_rate_limit_in_result_event(line));
    }

    #[test]
    fn test_error_result_with_usage_limit() {
        let jsonl = result_event(
            true,
            "error",
            "You have exceeded your usage limit for this model.",
        );
        assert!(detect_rate_limit_in_result_event(&jsonl));
    }

    #[test]
    fn test_error_result_with_hit_your_limit() {
        let jsonl = result_event(true, "error", "You've hit your limit. Please wait.");
        assert!(detect_rate_limit_in_result_event(&jsonl));
    }

    #[test]
    fn test_error_result_with_rate_limit_case_insensitive() {
        let jsonl = result_event(true, "error", "RATE LIMIT exceeded");
        assert!(detect_rate_limit_in_result_event(&jsonl));
    }

    #[test]
    fn test_error_result_without_rate_limit_keywords() {
        let jsonl = result_event(true, "error", "Internal server error occurred");
        assert!(!detect_rate_limit_in_result_event(&jsonl));
    }

    // --- Successful session never rate-limited (the core false-positive fix) ---

    #[test]
    fn test_successful_session_never_rate_limited() {
        // Session output mentions rate limiting (e.g., agent read SPEC.md), but
        // the session itself succeeded — should NOT be classified as rate-limited.
        let jsonl = jsonl_with_result(
            &[
                r#"{"type":"assistant","message":"I read the file that says 'rate limit detection'"}"#,
            ],
            false,
            "success",
            "Done. Implemented rate limit detection feature.",
        );
        assert!(!detect_rate_limit_in_result_event(&jsonl));
    }

    #[test]
    fn test_successful_session_with_rate_limit_in_tool_output() {
        // Tool output contains rate limit keywords, but session succeeded
        let jsonl = jsonl_with_result(
            &[
                r#"{"type":"tool_result","content":"usage limit reached, resets at UTC midnight"}"#,
                r#"{"type":"assistant","message":"I found the rate limit code"}"#,
            ],
            false,
            "success",
            "Completed successfully.",
        );
        assert!(!detect_rate_limit_in_result_event(&jsonl));
    }

    #[test]
    fn test_successful_result_with_rate_limit_in_result_text() {
        // Even if the result text mentions rate limiting, a success is never rate-limited
        let jsonl = result_event(
            false,
            "success",
            "Implemented rate_limit feature with usage limit handling",
        );
        assert!(!detect_rate_limit_in_result_event(&jsonl));
    }

    // --- Edge cases ---

    #[test]
    fn test_no_result_event_not_rate_limited() {
        let jsonl = r#"{"type":"assistant","message":"hello"}
{"type":"tool_result","content":"some output"}"#;
        assert!(!detect_rate_limit_in_result_event(jsonl));
    }

    #[test]
    fn test_empty_content_not_rate_limited() {
        assert!(!detect_rate_limit_in_result_event(""));
    }

    #[test]
    fn test_malformed_result_line_not_rate_limited() {
        let jsonl = r#"{"type":"result" invalid json here"#;
        assert!(!detect_rate_limit_in_result_event(jsonl));
    }

    #[test]
    fn test_multiple_result_events_uses_last() {
        // First result is an error with rate limit, second (last) is success
        let line1 = result_event(true, "error", "rate_limit exceeded");
        let line2 = result_event(false, "success", "Session completed.");
        let jsonl = format!("{}\n{}", line1, line2);
        assert!(!detect_rate_limit_in_result_event(&jsonl));
    }

    // --- File-based detection ---

    #[test]
    fn test_detect_rate_limit_from_file_with_error_result() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("output.jsonl");
        let content = result_event(true, "error", "rate_limit: too many requests");
        std::fs::write(&path, content).unwrap();
        assert!(detect_rate_limit(&path));
    }

    #[test]
    fn test_detect_no_rate_limit_from_file_successful_session() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("output.jsonl");
        let content = jsonl_with_result(
            &[r#"{"type":"tool_result","content":"rate limit code here"}"#],
            false,
            "success",
            "Done.",
        );
        std::fs::write(&path, content).unwrap();
        assert!(!detect_rate_limit(&path));
    }

    #[test]
    fn test_detect_rate_limit_missing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.jsonl");
        assert!(!detect_rate_limit(&path));
    }

    // --- Codex format tests ---

    #[test]
    fn test_codex_error_event_with_usage_limit() {
        let jsonl = r#"{"type":"thread.started","thread_id":"abc"}
{"type":"turn.started"}
{"type":"error","message":"You've hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro), visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again at 10:15 PM."}"#;
        assert!(detect_rate_limit_in_content(jsonl));
    }

    #[test]
    fn test_codex_turn_failed_with_usage_limit() {
        let jsonl = r#"{"type":"thread.started","thread_id":"abc"}
{"type":"turn.started"}
{"type":"turn.failed","error":{"message":"You've hit your usage limit. Upgrade to Pro."}}"#;
        assert!(detect_rate_limit_in_content(jsonl));
    }

    #[test]
    fn test_codex_error_event_without_rate_limit() {
        let jsonl = r#"{"type":"error","message":"Internal server error"}"#;
        assert!(!detect_rate_limit_in_content(jsonl));
    }

    // --- Quota exhaustion tests ---

    #[test]
    fn test_codex_quota_exhaustion_detected() {
        let jsonl = r#"{"type":"error","message":"You've hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro), visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again at 10:15 PM."}"#;
        let result = detect_quota_in_content(jsonl);
        assert!(result.is_some());
        assert!(result.unwrap().contains("hit your usage limit"));
    }

    #[test]
    fn test_codex_quota_purchase_credits() {
        let jsonl = r#"{"type":"error","message":"Please purchase more credits to continue."}"#;
        assert!(detect_quota_in_content(jsonl).is_some());
    }

    #[test]
    fn test_codex_quota_upgrade_to_pro() {
        let jsonl = r#"{"type":"turn.failed","error":{"message":"Upgrade to Pro to continue using this model."}}"#;
        assert!(detect_quota_in_content(jsonl).is_some());
    }

    #[test]
    fn test_claude_quota_exhaustion_detected() {
        let jsonl = result_event(
            true,
            "error",
            "You've hit your usage limit for this billing period.",
        );
        assert!(detect_quota_in_content(&jsonl).is_some());
    }

    #[test]
    fn test_transient_rate_limit_not_quota() {
        // A plain "rate limit" is not quota exhaustion
        let jsonl = r#"{"type":"error","message":"rate limit exceeded, please retry"}"#;
        assert!(detect_quota_in_content(jsonl).is_none());
    }

    #[test]
    fn test_quota_from_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("output.jsonl");
        let content = r#"{"type":"thread.started","thread_id":"abc"}
{"type":"turn.started"}
{"type":"error","message":"You've hit your usage limit. Upgrade to Pro."}"#;
        std::fs::write(&path, content).unwrap();
        assert!(detect_quota_exhaustion(&path).is_some());
    }

    #[test]
    fn test_no_quota_from_normal_failure() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("output.jsonl");
        std::fs::write(&path, r#"{"type":"error","message":"compilation failed"}"#).unwrap();
        assert!(detect_quota_exhaustion(&path).is_none());
    }

    // --- Authentication failure tests ---

    #[test]
    fn test_claude_auth_failure_detected_from_error_field() {
        // Real-world Claude auth failure: assistant message with error field
        let jsonl = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Invalid API key · Fix external API key"}]},"error":"authentication_failed"}
{"type":"result","subtype":"success","is_error":true,"result":"Invalid API key · Fix external API key"}"#;
        let result = detect_auth_failure_in_content(jsonl);
        assert!(result.is_some());
        // Returns the error message text (from result event or error field)
        let msg = result.unwrap();
        assert!(
            msg.contains("Invalid API key") || msg.contains("authentication_failed"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn test_claude_auth_failure_detected_from_result_text() {
        let jsonl = result_event(true, "error", "Invalid API key");
        let result = detect_auth_failure_in_content(&jsonl);
        assert!(result.is_some());
        assert!(result.unwrap().contains("Invalid API key"));
    }

    #[test]
    fn test_auth_failure_invalid_x_api_key() {
        let jsonl = result_event(true, "error", "Invalid x-api-key in request header");
        assert!(detect_auth_failure_in_content(&jsonl).is_some());
    }

    #[test]
    fn test_auth_failure_expired_key() {
        let jsonl = result_event(true, "error", "Your API key has expired");
        assert!(detect_auth_failure_in_content(&jsonl).is_some());
    }

    #[test]
    fn test_successful_session_not_auth_failure() {
        let jsonl = result_event(false, "success", "Implemented API key validation");
        assert!(detect_auth_failure_in_content(&jsonl).is_none());
    }

    #[test]
    fn test_normal_error_not_auth_failure() {
        let jsonl = result_event(true, "error", "compilation failed");
        assert!(detect_auth_failure_in_content(&jsonl).is_none());
    }

    #[test]
    fn test_auth_failure_from_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("output.jsonl");
        let content = r#"{"type":"assistant","error":"authentication_failed","message":{"content":[{"type":"text","text":"Invalid API key"}]}}
{"type":"result","subtype":"success","is_error":true,"result":"Invalid API key · Fix external API key"}"#;
        std::fs::write(&path, content).unwrap();
        assert!(detect_auth_failure(&path).is_some());
    }

    #[test]
    fn test_auth_failure_missing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.jsonl");
        assert!(detect_auth_failure(&path).is_none());
    }

    // --- API key source extraction tests ---

    #[test]
    fn test_extract_api_key_source_present() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("output.jsonl");
        let content = r#"{"type":"system","subtype":"init","apiKeySource":"ANTHROPIC_API_KEY","session_id":"test"}
{"type":"result","subtype":"success","is_error":false,"result":"Done"}"#;
        std::fs::write(&path, content).unwrap();
        assert_eq!(
            extract_api_key_source(&path),
            Some("ANTHROPIC_API_KEY".to_string())
        );
    }

    #[test]
    fn test_extract_api_key_source_not_present() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("output.jsonl");
        let content = r#"{"type":"system","subtype":"init","session_id":"test"}
{"type":"result","subtype":"success","is_error":false,"result":"Done"}"#;
        std::fs::write(&path, content).unwrap();
        assert_eq!(extract_api_key_source(&path), None);
    }

    #[test]
    fn test_extract_api_key_source_missing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.jsonl");
        assert_eq!(extract_api_key_source(&path), None);
    }

    // --- Backoff tests (unchanged) ---

    #[test]
    fn test_backoff_delay_basic() {
        assert_eq!(backoff_delay(2, 0, 600), 2);
        assert_eq!(backoff_delay(2, 1, 600), 4);
        assert_eq!(backoff_delay(2, 2, 600), 8);
        assert_eq!(backoff_delay(2, 3, 600), 16);
    }

    #[test]
    fn test_backoff_delay_capped() {
        assert_eq!(backoff_delay(2, 10, 600), 600);
    }

    #[test]
    fn test_backoff_delay_overflow_safe() {
        assert_eq!(backoff_delay(2, 63, 600), 600);
    }

    #[test]
    fn test_backoff_delay_zero_initial() {
        assert_eq!(backoff_delay(0, 5, 600), 0);
    }
}
