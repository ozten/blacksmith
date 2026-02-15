/// Output-growth monitor for agent sessions.
///
/// Runs alongside the agent process, periodically checking the output file size.
/// If no growth is detected for `stale_timeout_mins`, kills the agent process group.
pub struct Watchdog {
    // TODO: check interval, stale timeout, output file path
}

impl Watchdog {
    // TODO: pub async fn monitor(&self, child_pid: i32) -> WatchdogOutcome
}
