/// Retry policy for empty or crashed sessions.
///
/// When a session produces less than `min_output_bytes`, the retry policy
/// determines whether to re-run the session or skip to the next iteration.
pub struct RetryPolicy {
    // TODO: max retries, current attempt count
}

impl RetryPolicy {
    // TODO: pub fn should_retry(&self, output_bytes: u64) -> bool
}
