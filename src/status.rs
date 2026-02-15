/// Status file: writes `harness.status` as JSON on every state transition.
///
/// Uses atomic write pattern: write to temp file then rename.
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Harness states written to the status file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessState {
    Starting,
    PreHooks,
    SessionRunning,
    WatchdogKill,
    Retrying,
    PostHooks,
    RateLimitedBackoff,
    Idle,
    ShuttingDown,
}

/// The JSON payload written to `harness.status`.
#[derive(Debug, Clone, Serialize)]
pub struct StatusData {
    pub pid: u32,
    pub state: HarnessState,
    pub iteration: u32,
    pub max_iterations: u32,
    pub global_iteration: u64,
    pub output_file: String,
    pub output_bytes: u64,
    pub session_start: Option<DateTime<Utc>>,
    pub last_update: DateTime<Utc>,
    pub last_completed_iteration: Option<u64>,
    pub last_committed: bool,
    pub consecutive_rate_limits: u32,
}

/// Manages the status file lifecycle.
pub struct StatusFile {
    path: PathBuf,
}

impl StatusFile {
    /// Create a new StatusFile writer for the given path.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Atomically write status data to the status file.
    ///
    /// Writes to a temporary file in the same directory, then renames
    /// to ensure readers never see a partial write.
    pub fn write(&self, data: &StatusData) -> Result<(), StatusError> {
        let json =
            serde_json::to_string_pretty(data).map_err(|e| StatusError::Serialize { source: e })?;

        let dir = self.path.parent().unwrap_or(Path::new("."));
        let tmp_path = dir.join(format!(".harness.status.tmp.{}", std::process::id()));

        std::fs::write(&tmp_path, json.as_bytes()).map_err(|e| StatusError::Write {
            path: tmp_path.clone(),
            source: e,
        })?;

        std::fs::rename(&tmp_path, &self.path).map_err(|e| StatusError::Rename {
            from: tmp_path,
            to: self.path.clone(),
            source: e,
        })?;

        Ok(())
    }

    /// Remove the status file (on clean shutdown).
    pub fn remove(&self) {
        let _ = std::fs::remove_file(&self.path);
    }

    /// Path to the status file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Mutable state tracker that builds StatusData for each update.
pub struct StatusTracker {
    file: StatusFile,
    pid: u32,
    max_iterations: u32,
    iteration: u32,
    global_iteration: u64,
    output_file: String,
    output_bytes: u64,
    session_start: Option<DateTime<Utc>>,
    last_completed_iteration: Option<u64>,
    last_committed: bool,
    consecutive_rate_limits: u32,
}

impl StatusTracker {
    /// Create a new tracker.
    pub fn new(status_path: PathBuf, max_iterations: u32, global_iteration: u64) -> Self {
        Self {
            file: StatusFile::new(status_path),
            pid: std::process::id(),
            max_iterations,
            iteration: 0,
            global_iteration,
            output_file: String::new(),
            output_bytes: 0,
            session_start: None,
            last_completed_iteration: None,
            last_committed: false,
            consecutive_rate_limits: 0,
        }
    }

    /// Update and write the status file with the given state.
    pub fn update(&self, state: HarnessState) {
        let data = StatusData {
            pid: self.pid,
            state,
            iteration: self.iteration,
            max_iterations: self.max_iterations,
            global_iteration: self.global_iteration,
            output_file: self.output_file.clone(),
            output_bytes: self.output_bytes,
            session_start: self.session_start,
            last_update: Utc::now(),
            last_completed_iteration: self.last_completed_iteration,
            last_committed: self.last_committed,
            consecutive_rate_limits: self.consecutive_rate_limits,
        };

        if let Err(e) = self.file.write(&data) {
            tracing::warn!(error = %e, "failed to write status file");
        }
    }

    /// Set the current productive iteration count.
    pub fn set_iteration(&mut self, iteration: u32) {
        self.iteration = iteration;
    }

    /// Set the global iteration counter.
    pub fn set_global_iteration(&mut self, global: u64) {
        self.global_iteration = global;
    }

    /// Set the current output file path.
    pub fn set_output_file(&mut self, path: &str) {
        self.output_file = path.to_string();
    }

    /// Set the current output size.
    pub fn set_output_bytes(&mut self, bytes: u64) {
        self.output_bytes = bytes;
    }

    /// Mark the start of a new session.
    pub fn set_session_start(&mut self) {
        self.session_start = Some(Utc::now());
    }

    /// Record the last completed iteration.
    pub fn set_last_completed(&mut self, global: u64) {
        self.last_completed_iteration = Some(global);
    }

    /// Set whether the last session committed.
    pub fn set_last_committed(&mut self, committed: bool) {
        self.last_committed = committed;
    }

    /// Set consecutive rate limit count.
    pub fn set_consecutive_rate_limits(&mut self, count: u32) {
        self.consecutive_rate_limits = count;
    }

    /// Remove the status file.
    pub fn remove(&self) {
        self.file.remove();
    }
}

/// Errors from status file operations.
#[derive(Debug)]
pub enum StatusError {
    Serialize {
        source: serde_json::Error,
    },
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    Rename {
        from: PathBuf,
        to: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for StatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StatusError::Serialize { source } => write!(f, "failed to serialize status: {source}"),
            StatusError::Write { path, source } => {
                write!(
                    f,
                    "failed to write temp status file {}: {source}",
                    path.display()
                )
            }
            StatusError::Rename { from, to, source } => {
                write!(
                    f,
                    "failed to rename {} -> {}: {source}",
                    from.display(),
                    to.display()
                )
            }
        }
    }
}

impl std::error::Error for StatusError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StatusError::Serialize { source } => Some(source),
            StatusError::Write { source, .. } => Some(source),
            StatusError::Rename { source, .. } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_status_file_atomic_write() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("harness.status");
        let sf = StatusFile::new(path.clone());

        let data = StatusData {
            pid: 12345,
            state: HarnessState::SessionRunning,
            iteration: 3,
            max_iterations: 25,
            global_iteration: 103,
            output_file: "claude-iteration-103.jsonl".to_string(),
            output_bytes: 49331,
            session_start: Some(Utc::now()),
            last_update: Utc::now(),
            last_completed_iteration: Some(102),
            last_committed: true,
            consecutive_rate_limits: 0,
        };

        sf.write(&data).unwrap();

        // Verify the file exists and is valid JSON
        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["pid"], 12345);
        assert_eq!(parsed["state"], "session_running");
        assert_eq!(parsed["iteration"], 3);
        assert_eq!(parsed["max_iterations"], 25);
        assert_eq!(parsed["global_iteration"], 103);
        assert_eq!(parsed["output_file"], "claude-iteration-103.jsonl");
        assert_eq!(parsed["output_bytes"], 49331);
        assert_eq!(parsed["last_committed"], true);
        assert_eq!(parsed["consecutive_rate_limits"], 0);

        // Verify no temp file left behind
        let tmp_path = dir
            .path()
            .join(format!(".harness.status.tmp.{}", std::process::id()));
        assert!(
            !tmp_path.exists(),
            "temp file should be cleaned up by rename"
        );
    }

    #[test]
    fn test_status_file_overwrite() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("harness.status");
        let sf = StatusFile::new(path.clone());

        let mut data = StatusData {
            pid: 1,
            state: HarnessState::Starting,
            iteration: 0,
            max_iterations: 10,
            global_iteration: 0,
            output_file: String::new(),
            output_bytes: 0,
            session_start: None,
            last_update: Utc::now(),
            last_completed_iteration: None,
            last_committed: false,
            consecutive_rate_limits: 0,
        };

        sf.write(&data).unwrap();

        // Write again with updated state
        data.state = HarnessState::SessionRunning;
        data.iteration = 1;
        sf.write(&data).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["state"], "session_running");
        assert_eq!(parsed["iteration"], 1);
    }

    #[test]
    fn test_status_file_remove() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("harness.status");
        let sf = StatusFile::new(path.clone());

        let data = StatusData {
            pid: 1,
            state: HarnessState::Starting,
            iteration: 0,
            max_iterations: 10,
            global_iteration: 0,
            output_file: String::new(),
            output_bytes: 0,
            session_start: None,
            last_update: Utc::now(),
            last_completed_iteration: None,
            last_committed: false,
            consecutive_rate_limits: 0,
        };

        sf.write(&data).unwrap();
        assert!(path.exists());

        sf.remove();
        assert!(!path.exists());
    }

    #[test]
    fn test_all_harness_states_serialize() {
        let states = vec![
            (HarnessState::Starting, "starting"),
            (HarnessState::PreHooks, "pre_hooks"),
            (HarnessState::SessionRunning, "session_running"),
            (HarnessState::WatchdogKill, "watchdog_kill"),
            (HarnessState::Retrying, "retrying"),
            (HarnessState::PostHooks, "post_hooks"),
            (HarnessState::RateLimitedBackoff, "rate_limited_backoff"),
            (HarnessState::Idle, "idle"),
            (HarnessState::ShuttingDown, "shutting_down"),
        ];

        for (state, expected_str) in states {
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(json, format!("\"{}\"", expected_str));
        }
    }

    #[test]
    fn test_status_tracker_lifecycle() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("harness.status");

        let mut tracker = StatusTracker::new(path.clone(), 25, 100);

        // Starting state
        tracker.update(HarnessState::Starting);
        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["state"], "starting");
        assert_eq!(parsed["global_iteration"], 100);
        assert_eq!(parsed["max_iterations"], 25);

        // Session running
        tracker.set_iteration(1);
        tracker.set_global_iteration(100);
        tracker.set_output_file("claude-iteration-100.jsonl");
        tracker.set_session_start();
        tracker.update(HarnessState::SessionRunning);

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["state"], "session_running");
        assert_eq!(parsed["iteration"], 1);
        assert_eq!(parsed["output_file"], "claude-iteration-100.jsonl");
        assert!(parsed["session_start"].is_string());

        // Idle after completion
        tracker.set_output_bytes(50000);
        tracker.set_last_completed(100);
        tracker.set_global_iteration(101);
        tracker.update(HarnessState::Idle);

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["state"], "idle");
        assert_eq!(parsed["output_bytes"], 50000);
        assert_eq!(parsed["last_completed_iteration"], 100);
        assert_eq!(parsed["global_iteration"], 101);

        // Clean shutdown
        tracker.remove();
        assert!(!path.exists());
    }

    #[test]
    fn test_status_file_write_to_nonexistent_dir_fails() {
        let sf = StatusFile::new(PathBuf::from("/nonexistent/dir/harness.status"));
        let data = StatusData {
            pid: 1,
            state: HarnessState::Starting,
            iteration: 0,
            max_iterations: 10,
            global_iteration: 0,
            output_file: String::new(),
            output_bytes: 0,
            session_start: None,
            last_update: Utc::now(),
            last_completed_iteration: None,
            last_committed: false,
            consecutive_rate_limits: 0,
        };

        let result = sf.write(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_status_error_display() {
        let err = StatusError::Write {
            path: PathBuf::from("/tmp/test"),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "no perms"),
        };
        let msg = err.to_string();
        assert!(msg.contains("failed to write temp status file"));
        assert!(msg.contains("no perms"));
    }
}
