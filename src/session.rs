/// Single session lifecycle: spawn agent subprocess, stream output to file,
/// coordinate with the watchdog for stale detection.
pub struct Session {
    // TODO: child process handle, output file path, session metadata
}

impl Session {
    // TODO: pub async fn run(&mut self) -> Result<SessionResult, Box<dyn std::error::Error>>
}
