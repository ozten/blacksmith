pub mod claude;

use serde_json::Value;
use std::path::Path;

/// Source type for configurable extraction rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractionSource {
    /// Tool invocation commands (e.g., tool_use input.command fields).
    ToolCommands,
    /// Assistant text output blocks.
    Text,
    /// Raw file lines, unprocessed.
    Raw,
}

/// Errors produced by adapter operations.
#[derive(Debug)]
pub enum AdapterError {
    Io(std::io::Error),
    Parse(String),
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterError::Io(e) => write!(f, "I/O error: {e}"),
            AdapterError::Parse(msg) => write!(f, "parse error: {msg}"),
        }
    }
}

impl std::error::Error for AdapterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AdapterError::Io(e) => Some(e),
            AdapterError::Parse(_) => None,
        }
    }
}

impl From<std::io::Error> for AdapterError {
    fn from(e: std::io::Error) -> Self {
        AdapterError::Io(e)
    }
}

/// Normalizes agent-specific output into blacksmith events.
pub trait AgentAdapter: Send + Sync {
    /// Human-readable adapter name (e.g., "claude", "codex").
    fn name(&self) -> &str;

    /// Extract built-in metrics from a session output file.
    ///
    /// Returns a Vec of (kind, value) pairs representing the metrics
    /// this adapter knows how to extract from its native format.
    fn extract_builtin_metrics(
        &self,
        output_path: &Path,
    ) -> Result<Vec<(String, Value)>, AdapterError>;

    /// Which built-in event kinds this adapter can produce.
    ///
    /// Used by the brief/targets system to know which metrics are
    /// available. Metrics not in this list are simply skipped in
    /// dashboards rather than showing as errors.
    fn supported_metrics(&self) -> &[&str];

    /// Provide raw text lines for configurable extraction rules to scan.
    ///
    /// The adapter controls what "source" means for each source type.
    /// For example, the Claude adapter maps "tool_commands" to
    /// tool_use input.command fields, while the Codex adapter maps
    /// it to function_call arguments.
    fn lines_for_source(
        &self,
        output_path: &Path,
        source: ExtractionSource,
    ) -> Result<Vec<String>, AdapterError>;
}
