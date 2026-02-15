use serde::Deserialize;
use std::path::PathBuf;

/// Top-level configuration loaded from harness.toml.
#[derive(Debug, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct HarnessConfig {
    pub session: SessionConfig,
    pub agent: AgentConfig,
    pub watchdog: WatchdogConfig,
    pub retry: RetryConfig,
    pub backoff: BackoffConfig,
    pub shutdown: ShutdownConfig,
    pub hooks: HooksConfig,
    pub prompt: PromptConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct SessionConfig {
    pub max_iterations: u32,
    pub prompt_file: PathBuf,
    pub output_dir: PathBuf,
    pub output_prefix: String,
    pub counter_file: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct WatchdogConfig {
    pub check_interval_secs: u64,
    pub stale_timeout_mins: u64,
    pub min_output_bytes: u64,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct RetryConfig {
    pub max_empty_retries: u32,
    pub retry_delay_secs: u64,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct BackoffConfig {
    pub initial_delay_secs: u64,
    pub max_delay_secs: u64,
    pub max_consecutive_rate_limits: u32,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ShutdownConfig {
    pub stop_file: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct HooksConfig {
    pub pre_session: Vec<String>,
    pub post_session: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct PromptConfig {
    pub file: Option<PathBuf>,
    pub prepend_commands: Vec<String>,
}

// --- Default implementations ---

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            max_iterations: 25,
            prompt_file: PathBuf::from("PROMPT.md"),
            output_dir: PathBuf::from("."),
            output_prefix: "claude-iteration".to_string(),
            counter_file: PathBuf::from(".iteration_counter"),
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            command: "claude".to_string(),
            args: vec![
                "-p".to_string(),
                "{prompt}".to_string(),
                "--dangerously-skip-permissions".to_string(),
                "--verbose".to_string(),
                "--output-format".to_string(),
                "stream-json".to_string(),
            ],
        }
    }
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            check_interval_secs: 60,
            stale_timeout_mins: 20,
            min_output_bytes: 100,
        }
    }
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_empty_retries: 2,
            retry_delay_secs: 5,
        }
    }
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial_delay_secs: 2,
            max_delay_secs: 600,
            max_consecutive_rate_limits: 5,
        }
    }
}

impl Default for ShutdownConfig {
    fn default() -> Self {
        Self {
            stop_file: PathBuf::from("STOP"),
        }
    }
}
