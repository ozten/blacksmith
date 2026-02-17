use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct UiConfig {
    #[serde(default)]
    pub dashboard: DashboardConfig,
    #[serde(default)]
    pub projects: Vec<ProjectEntry>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DashboardConfig {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            bind: default_bind(),
            poll_interval_secs: default_poll_interval(),
        }
    }
}

fn default_port() -> u16 {
    8080
}
fn default_bind() -> String {
    "127.0.0.1".to_string()
}
fn default_poll_interval() -> u64 {
    10
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ProjectEntry {
    pub name: String,
    pub url: String,
}

/// Load config from blacksmith-ui.toml in the given directory, or default.
pub fn load_config(dir: &Path) -> UiConfig {
    let path = dir.join("blacksmith-ui.toml");
    match std::fs::read_to_string(&path) {
        Ok(contents) => match toml::from_str(&contents) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!("failed to parse {}: {e}", path.display());
                UiConfig::default()
            }
        },
        Err(_) => UiConfig::default(),
    }
}

/// Path to the runtime-added instances file.
pub fn runtime_instances_path() -> PathBuf {
    let dir = std::env::current_dir().unwrap_or_default();
    dir.join(".blacksmith-ui-instances.json")
}

/// Load runtime-added instances from disk.
pub fn load_runtime_instances() -> Vec<ProjectEntry> {
    let path = runtime_instances_path();
    match std::fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Save runtime-added instances to disk.
pub fn save_runtime_instances(instances: &[ProjectEntry]) -> std::io::Result<()> {
    let path = runtime_instances_path();
    let json = serde_json::to_string_pretty(instances)?;
    std::fs::write(&path, json)
}
