use tracing::warn;

/// Decision returned by the retry policy after evaluating session output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryDecision {
    /// Session produced sufficient output — proceed to next iteration.
    Proceed,
    /// Session was empty — retry the same iteration (includes 1-based attempt number).
    Retry { attempt: u32 },
    /// Exhausted all retries — skip this iteration.
    Skip,
}

/// Retry policy for empty or crashed sessions.
///
/// Tracks retry attempts for the current iteration and decides whether
/// to re-run a session when output is below `min_output_bytes`.
/// Empty retries do NOT increment the productive iteration counter
/// and do NOT trigger post-session hooks.
pub struct RetryPolicy {
    max_retries: u32,
    min_output_bytes: u64,
    current_attempt: u32,
}

impl RetryPolicy {
    /// Create a new retry policy from config values.
    pub fn new(max_retries: u32, min_output_bytes: u64) -> Self {
        Self {
            max_retries,
            min_output_bytes,
            current_attempt: 0,
        }
    }

    /// Evaluate session output and decide what to do next.
    ///
    /// If output_bytes >= min_output_bytes, returns `Proceed`.
    /// If output_bytes < min_output_bytes and retries remain, returns `Retry`.
    /// If retries are exhausted, returns `Skip`.
    pub fn evaluate(&mut self, output_bytes: u64) -> RetryDecision {
        if output_bytes >= self.min_output_bytes {
            return RetryDecision::Proceed;
        }

        self.current_attempt += 1;

        if self.current_attempt <= self.max_retries {
            warn!(
                output_bytes,
                attempt = self.current_attempt,
                max_retries = self.max_retries,
                "empty session detected, retrying"
            );
            RetryDecision::Retry {
                attempt: self.current_attempt,
            }
        } else {
            warn!(
                output_bytes,
                max_retries = self.max_retries,
                "empty session retries exhausted, skipping iteration"
            );
            RetryDecision::Skip
        }
    }

    /// Reset the retry counter for a new iteration.
    pub fn reset(&mut self) {
        self.current_attempt = 0;
    }

    /// Current attempt count (0 = no retries yet).
    #[allow(dead_code)]
    pub fn current_attempt(&self) -> u32 {
        self.current_attempt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proceed_when_output_sufficient() {
        let mut policy = RetryPolicy::new(2, 100);
        assert_eq!(policy.evaluate(100), RetryDecision::Proceed);
        assert_eq!(policy.evaluate(5000), RetryDecision::Proceed);
        assert_eq!(policy.current_attempt(), 0);
    }

    #[test]
    fn test_retry_when_output_empty() {
        let mut policy = RetryPolicy::new(2, 100);
        assert_eq!(policy.evaluate(0), RetryDecision::Retry { attempt: 1 });
        assert_eq!(policy.current_attempt(), 1);
    }

    #[test]
    fn test_retry_when_output_below_threshold() {
        let mut policy = RetryPolicy::new(2, 100);
        assert_eq!(policy.evaluate(99), RetryDecision::Retry { attempt: 1 });
    }

    #[test]
    fn test_skip_after_max_retries_exhausted() {
        let mut policy = RetryPolicy::new(2, 100);
        // First retry
        assert_eq!(policy.evaluate(50), RetryDecision::Retry { attempt: 1 });
        // Second retry
        assert_eq!(policy.evaluate(50), RetryDecision::Retry { attempt: 2 });
        // Third attempt — exhausted
        assert_eq!(policy.evaluate(50), RetryDecision::Skip);
    }

    #[test]
    fn test_reset_clears_attempt_counter() {
        let mut policy = RetryPolicy::new(2, 100);
        policy.evaluate(0); // attempt 1
        policy.evaluate(0); // attempt 2
        assert_eq!(policy.current_attempt(), 2);

        policy.reset();
        assert_eq!(policy.current_attempt(), 0);

        // Can retry again after reset
        assert_eq!(policy.evaluate(0), RetryDecision::Retry { attempt: 1 });
    }

    #[test]
    fn test_zero_max_retries_skips_immediately() {
        let mut policy = RetryPolicy::new(0, 100);
        assert_eq!(policy.evaluate(50), RetryDecision::Skip);
    }

    #[test]
    fn test_proceed_does_not_increment_attempt() {
        let mut policy = RetryPolicy::new(2, 100);
        // Successful session
        assert_eq!(policy.evaluate(200), RetryDecision::Proceed);
        assert_eq!(policy.current_attempt(), 0);
        // Another successful session
        assert_eq!(policy.evaluate(100), RetryDecision::Proceed);
        assert_eq!(policy.current_attempt(), 0);
    }

    #[test]
    fn test_proceed_after_retry_when_output_recovers() {
        let mut policy = RetryPolicy::new(2, 100);
        // First attempt empty
        assert_eq!(policy.evaluate(50), RetryDecision::Retry { attempt: 1 });
        // Retry succeeds
        assert_eq!(policy.evaluate(200), RetryDecision::Proceed);
        // Attempt counter stays at 1 (proceed doesn't reset)
        assert_eq!(policy.current_attempt(), 1);
    }

    #[test]
    fn test_exact_threshold_proceeds() {
        let mut policy = RetryPolicy::new(2, 100);
        // Exactly at threshold = sufficient
        assert_eq!(policy.evaluate(100), RetryDecision::Proceed);
    }

    #[test]
    fn test_min_output_zero_always_proceeds() {
        let mut policy = RetryPolicy::new(2, 0);
        // Even 0 bytes is >= 0 threshold
        assert_eq!(policy.evaluate(0), RetryDecision::Proceed);
    }
}
