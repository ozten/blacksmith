/// Single session lifecycle: spawn agent subprocess, capture output to file,
/// report results (exit code, output bytes, duration).
use crate::config::{AgentConfig, SessionConfig};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;
use tokio::process::Command;

/// Result of a completed session.
#[derive(Debug)]
pub struct SessionResult {
    /// Process exit code (None if killed by signal).
    pub exit_code: Option<i32>,
    /// Total bytes written to the output file.
    pub output_bytes: u64,
    /// Wall-clock duration of the session.
    pub duration: std::time::Duration,
    /// Path to the output JSONL file.
    pub output_file: PathBuf,
    /// Child PID (for logging/diagnostics).
    pub pid: u32,
}

/// Errors that can occur during session execution.
#[derive(Debug)]
pub enum SessionError {
    /// Failed to create the output file.
    OutputFile {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Failed to spawn the agent subprocess.
    Spawn { source: std::io::Error },
    /// Failed to read from child stdout/stderr.
    Io { source: std::io::Error },
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::OutputFile { path, source } => {
                write!(
                    f,
                    "failed to create output file {}: {}",
                    path.display(),
                    source
                )
            }
            SessionError::Spawn { source } => {
                write!(f, "failed to spawn agent subprocess: {}", source)
            }
            SessionError::Io { source } => {
                write!(f, "I/O error during session: {}", source)
            }
        }
    }
}

impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SessionError::OutputFile { source, .. } => Some(source),
            SessionError::Spawn { source } => Some(source),
            SessionError::Io { source } => Some(source),
        }
    }
}

/// Build the output file path: {output_dir}/{output_prefix}-{global_iteration}.jsonl
///
/// Deprecated: use `DataDir::session_file()` instead. Retained for backwards compatibility
/// with existing session tests.
#[allow(dead_code)]
pub fn output_file_path(session_config: &SessionConfig, global_iteration: u64) -> PathBuf {
    let filename = format!(
        "{}-{}.jsonl",
        session_config.output_prefix, global_iteration
    );
    session_config.output_dir.join(filename)
}

/// Build the command arguments, replacing `{prompt}` placeholders with actual prompt content.
fn build_args(agent_config: &AgentConfig, prompt: &str) -> Vec<String> {
    agent_config
        .args
        .iter()
        .map(|arg| arg.replace("{prompt}", prompt))
        .collect()
}

/// Spawn the agent subprocess, capture stdout+stderr to a file, and return the result.
///
/// The subprocess is spawned in its own process group (via `process_group(0)`)
/// so the watchdog can later kill the entire group if needed.
pub async fn run_session(
    agent_config: &AgentConfig,
    output_path: &Path,
    prompt: &str,
) -> Result<SessionResult, SessionError> {
    // Create/truncate the output file
    let output_file = std::fs::File::create(output_path).map_err(|e| SessionError::OutputFile {
        path: output_path.to_path_buf(),
        source: e,
    })?;
    // We need a second handle for stderr since File doesn't impl Clone
    let output_file_stderr = output_file
        .try_clone()
        .map_err(|e| SessionError::OutputFile {
            path: output_path.to_path_buf(),
            source: e,
        })?;

    let args = build_args(agent_config, prompt);
    tracing::info!(
        command = %agent_config.command,
        args = ?args,
        output = %output_path.display(),
        "spawning agent session"
    );

    let start = Instant::now();

    let mut child = Command::new(&agent_config.command)
        .args(&args)
        .stdout(Stdio::from(output_file))
        .stderr(Stdio::from(output_file_stderr))
        .process_group(0) // New process group for clean kill
        .spawn()
        .map_err(|e| SessionError::Spawn { source: e })?;

    let pid = child.id().unwrap_or(0);
    tracing::info!(pid, "agent subprocess started");

    // Wait for the child to exit
    let status = child
        .wait()
        .await
        .map_err(|e| SessionError::Io { source: e })?;

    let duration = start.elapsed();

    // Read the output file size
    let output_bytes = std::fs::metadata(output_path).map(|m| m.len()).unwrap_or(0);

    let exit_code = status.code();
    tracing::info!(
        exit_code = ?exit_code,
        output_bytes,
        duration_secs = duration.as_secs(),
        "agent session completed"
    );

    Ok(SessionResult {
        exit_code,
        output_bytes,
        duration,
        output_file: output_path.to_path_buf(),
        pid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;

    #[test]
    fn test_output_file_path() {
        let config = SessionConfig {
            output_dir: PathBuf::from("/tmp/output"),
            output_prefix: "claude-iteration".to_string(),
            ..Default::default()
        };
        let path = output_file_path(&config, 42);
        assert_eq!(path, PathBuf::from("/tmp/output/claude-iteration-42.jsonl"));
    }

    #[test]
    fn test_output_file_path_default_dir() {
        let config = SessionConfig::default();
        let path = output_file_path(&config, 0);
        assert_eq!(path, PathBuf::from("./claude-iteration-0.jsonl"));
    }

    #[test]
    fn test_build_args_replaces_prompt_placeholder() {
        let agent = AgentConfig {
            command: "claude".to_string(),
            args: vec![
                "-p".to_string(),
                "{prompt}".to_string(),
                "--verbose".to_string(),
            ],
        };
        let args = build_args(&agent, "hello world");
        assert_eq!(args, vec!["-p", "hello world", "--verbose"]);
    }

    #[test]
    fn test_build_args_no_placeholder() {
        let agent = AgentConfig {
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
        };
        let args = build_args(&agent, "anything");
        assert_eq!(args, vec!["hello"]);
    }

    #[test]
    fn test_build_args_multiple_placeholders() {
        let agent = AgentConfig {
            command: "test".to_string(),
            args: vec![
                "{prompt}".to_string(),
                "mid".to_string(),
                "{prompt}".to_string(),
            ],
        };
        let args = build_args(&agent, "X");
        assert_eq!(args, vec!["X", "mid", "X"]);
    }

    #[tokio::test]
    async fn test_run_session_echo_command() {
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("test-output.jsonl");

        let agent = AgentConfig {
            command: "echo".to_string(),
            args: vec!["hello".to_string(), "{prompt}".to_string()],
        };

        let result = run_session(&agent, &output_path, "world").await.unwrap();

        assert_eq!(result.exit_code, Some(0));
        assert!(result.output_bytes > 0);
        assert!(result.output_file == output_path);
        assert!(result.pid > 0);

        // Check file contents
        let contents = std::fs::read_to_string(&output_path).unwrap();
        assert_eq!(contents.trim(), "hello world");
    }

    #[tokio::test]
    async fn test_run_session_captures_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("stderr-test.jsonl");

        let agent = AgentConfig {
            command: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                "echo stdout-line; echo stderr-line >&2".to_string(),
            ],
        };

        let result = run_session(&agent, &output_path, "unused").await.unwrap();

        assert_eq!(result.exit_code, Some(0));
        let contents = std::fs::read_to_string(&output_path).unwrap();
        assert!(contents.contains("stdout-line"));
        assert!(contents.contains("stderr-line"));
    }

    #[tokio::test]
    async fn test_run_session_nonzero_exit() {
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("fail-test.jsonl");

        let agent = AgentConfig {
            command: "sh".to_string(),
            args: vec!["-c".to_string(), "exit 42".to_string()],
        };

        let result = run_session(&agent, &output_path, "unused").await.unwrap();
        assert_eq!(result.exit_code, Some(42));
    }

    #[tokio::test]
    async fn test_run_session_spawn_failure() {
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("spawn-fail.jsonl");

        let agent = AgentConfig {
            command: "nonexistent-binary-xyz".to_string(),
            args: vec![],
        };

        let err = run_session(&agent, &output_path, "unused")
            .await
            .unwrap_err();
        assert!(matches!(err, SessionError::Spawn { .. }));
        assert!(err.to_string().contains("failed to spawn"));
    }

    #[tokio::test]
    async fn test_run_session_reports_correct_byte_count() {
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("bytes-test.jsonl");

        // Write exactly 5 bytes ("ABCDE") + newline from echo = 6 bytes
        let agent = AgentConfig {
            command: "printf".to_string(),
            args: vec!["ABCDE".to_string()],
        };

        let result = run_session(&agent, &output_path, "unused").await.unwrap();
        assert_eq!(result.output_bytes, 5);
    }

    #[tokio::test]
    async fn test_run_session_duration_is_reasonable() {
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("duration-test.jsonl");

        let agent = AgentConfig {
            command: "sleep".to_string(),
            args: vec!["0.1".to_string()],
        };

        let result = run_session(&agent, &output_path, "unused").await.unwrap();
        // Should take at least ~100ms
        assert!(result.duration.as_millis() >= 80);
        // But not more than a few seconds
        assert!(result.duration.as_secs() < 5);
    }

    #[tokio::test]
    async fn test_run_session_bad_output_path() {
        let agent = AgentConfig {
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
        };

        let err = run_session(
            &agent,
            Path::new("/nonexistent-dir/impossible/output.jsonl"),
            "unused",
        )
        .await
        .unwrap_err();
        assert!(matches!(err, SessionError::OutputFile { .. }));
    }
}
