use super::{AdapterError, AgentAdapter, ExtractionSource};
use serde_json::Value;
use std::io::BufRead;
use std::path::Path;

/// Pass-through adapter for unknown agent formats.
///
/// Returns no built-in metrics. For `lines_for_source`, all source types
/// return raw file lines unchanged — configurable extraction rules can
/// still match against the output.
pub struct RawAdapter;

impl RawAdapter {
    pub fn new() -> Self {
        RawAdapter
    }
}

impl Default for RawAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentAdapter for RawAdapter {
    fn name(&self) -> &str {
        "raw"
    }

    fn extract_builtin_metrics(
        &self,
        _output_path: &Path,
    ) -> Result<Vec<(String, Value)>, AdapterError> {
        // Raw adapter has no built-in metrics — configurable rules only
        Ok(Vec::new())
    }

    fn supported_metrics(&self) -> &[&str] {
        &[]
    }

    fn lines_for_source(
        &self,
        output_path: &Path,
        _source: ExtractionSource,
    ) -> Result<Vec<String>, AdapterError> {
        // All source types return raw lines for the raw adapter
        let file = std::fs::File::open(output_path)?;
        let reader = std::io::BufReader::new(file);
        let lines: Vec<String> = reader.lines().collect::<Result<_, _>>()?;
        Ok(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_raw_adapter_name() {
        let adapter = RawAdapter::new();
        assert_eq!(adapter.name(), "raw");
    }

    #[test]
    fn test_raw_adapter_no_builtin_metrics() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output.txt");
        std::fs::write(&path, "some output\nmore output\n").unwrap();
        let adapter = RawAdapter::new();
        let metrics = adapter.extract_builtin_metrics(&path).unwrap();
        assert!(metrics.is_empty());
    }

    #[test]
    fn test_raw_adapter_no_supported_metrics() {
        let adapter = RawAdapter::new();
        assert!(adapter.supported_metrics().is_empty());
    }

    #[test]
    fn test_raw_adapter_lines_for_source_returns_raw_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output.txt");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "line one").unwrap();
        writeln!(f, "line two").unwrap();
        writeln!(f, "line three").unwrap();
        drop(f);

        let adapter = RawAdapter::new();
        // All source types should return the same raw lines
        for source in [
            ExtractionSource::ToolCommands,
            ExtractionSource::Text,
            ExtractionSource::Raw,
        ] {
            let lines = adapter.lines_for_source(&path, source).unwrap();
            assert_eq!(lines, vec!["line one", "line two", "line three"]);
        }
    }

    #[test]
    fn test_raw_adapter_missing_file_returns_io_error() {
        let adapter = RawAdapter::new();
        let result = adapter.extract_builtin_metrics(Path::new("/nonexistent/file.txt"));
        // extract_builtin_metrics returns Ok(empty) regardless of file
        assert!(result.unwrap().is_empty());

        let result =
            adapter.lines_for_source(Path::new("/nonexistent/file.txt"), ExtractionSource::Raw);
        assert!(result.is_err());
    }

    #[test]
    fn test_raw_adapter_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, "").unwrap();
        let adapter = RawAdapter::new();
        let lines = adapter
            .lines_for_source(&path, ExtractionSource::Raw)
            .unwrap();
        assert!(lines.is_empty());
    }
}
