/// Rate limit detection: scan session output for rate limit indicators.
///
/// Looks for patterns like:
/// - JSON: `"error":"rate_limit"` or `"error": "rate_limit"`
/// - Text: `usage limit`, `hit your limit`, `resets.*UTC` (case-insensitive)
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

/// Compiled regex patterns for rate limit detection.
static RATE_LIMIT_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r#""error"\s*:\s*"rate_limit""#).unwrap(),
        Regex::new(r"(?i)usage limit").unwrap(),
        Regex::new(r"(?i)hit your limit").unwrap(),
        Regex::new(r"(?i)resets.*UTC").unwrap(),
    ]
});

/// Scan a file's contents for rate limit indicators.
///
/// Returns `true` if any rate limit pattern is found.
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

    detect_rate_limit_in_text(&contents)
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

    #[test]
    fn test_detect_json_error_rate_limit() {
        assert!(detect_rate_limit_in_text(
            r#"{"error":"rate_limit","message":"too many requests"}"#
        ));
    }

    #[test]
    fn test_detect_json_error_rate_limit_with_spaces() {
        assert!(detect_rate_limit_in_text(
            r#"{"error" : "rate_limit", "message":"too many requests"}"#
        ));
    }

    #[test]
    fn test_detect_usage_limit() {
        assert!(detect_rate_limit_in_text(
            "You have exceeded your usage limit for this model."
        ));
    }

    #[test]
    fn test_detect_usage_limit_case_insensitive() {
        assert!(detect_rate_limit_in_text("Usage Limit exceeded"));
    }

    #[test]
    fn test_detect_hit_your_limit() {
        assert!(detect_rate_limit_in_text(
            "You've hit your limit. Please wait."
        ));
    }

    #[test]
    fn test_detect_hit_your_limit_case_insensitive() {
        assert!(detect_rate_limit_in_text("HIT YOUR LIMIT"));
    }

    #[test]
    fn test_detect_resets_utc() {
        assert!(detect_rate_limit_in_text(
            "Your limit resets at 2026-02-15T00:00:00 UTC"
        ));
    }

    #[test]
    fn test_detect_resets_utc_case_insensitive() {
        assert!(detect_rate_limit_in_text("Resets tomorrow at 3pm UTC"));
    }

    #[test]
    fn test_no_rate_limit_normal_output() {
        assert!(!detect_rate_limit_in_text(
            "Session completed successfully with 50000 bytes of output."
        ));
    }

    #[test]
    fn test_no_rate_limit_empty() {
        assert!(!detect_rate_limit_in_text(""));
    }

    #[test]
    fn test_detect_rate_limit_from_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("output.jsonl");
        std::fs::write(&path, r#"{"error":"rate_limit"}"#).unwrap();
        assert!(detect_rate_limit(&path));
    }

    #[test]
    fn test_detect_no_rate_limit_from_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("output.jsonl");
        std::fs::write(&path, "normal output here").unwrap();
        assert!(!detect_rate_limit(&path));
    }

    #[test]
    fn test_detect_rate_limit_missing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.jsonl");
        assert!(!detect_rate_limit(&path));
    }

    #[test]
    fn test_backoff_delay_basic() {
        // 2 * 2^0 = 2
        assert_eq!(backoff_delay(2, 0, 600), 2);
        // 2 * 2^1 = 4
        assert_eq!(backoff_delay(2, 1, 600), 4);
        // 2 * 2^2 = 8
        assert_eq!(backoff_delay(2, 2, 600), 8);
        // 2 * 2^3 = 16
        assert_eq!(backoff_delay(2, 3, 600), 16);
    }

    #[test]
    fn test_backoff_delay_capped() {
        // 2 * 2^10 = 2048, but capped at 600
        assert_eq!(backoff_delay(2, 10, 600), 600);
    }

    #[test]
    fn test_backoff_delay_overflow_safe() {
        // Very large exponent should not panic, should saturate
        assert_eq!(backoff_delay(2, 63, 600), 600);
    }

    #[test]
    fn test_backoff_delay_zero_initial() {
        assert_eq!(backoff_delay(0, 5, 600), 0);
    }

    #[test]
    fn test_multiline_detection() {
        let text = "line 1: normal output\nline 2: still normal\nline 3: usage limit reached\nline 4: back to normal";
        assert!(detect_rate_limit_in_text(text));
    }
}
