/// Signal handling for graceful shutdown.
///
/// Handles SIGINT (Ctrl-C), SIGTERM, and STOP file detection.
/// First SIGINT: finish current session then exit.
/// Second SIGINT (within 3s): kill current session immediately.
/// SIGTERM: same as single SIGINT.
pub struct SignalHandler {
    // TODO: shutdown flag, double-sigint detection
}

impl SignalHandler {
    // TODO: pub async fn install() -> Result<SignalHandler, ...>
}
