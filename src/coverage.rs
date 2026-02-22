//! Code coverage metrics collection and reporting.
//!
//! Runs a coverage command (default: `cargo llvm-cov --json`) during the test
//! quality gate, parses the JSON output, and optionally enforces a minimum
//! coverage threshold.

use std::path::Path;
use std::process::Command;

/// Parsed coverage result from `cargo llvm-cov --json`.
#[derive(Debug, Clone)]
pub struct CoverageResult {
    /// Line coverage percentage (0.0–100.0).
    pub line_percent: f64,
    /// Number of lines covered.
    pub lines_covered: u64,
    /// Total number of instrumented lines.
    pub lines_total: u64,
    /// Function coverage percentage (0.0–100.0).
    pub function_percent: f64,
    /// Region coverage percentage (0.0–100.0).
    pub region_percent: f64,
    /// Branch coverage percentage (0.0–100.0), if available.
    pub branch_percent: Option<f64>,
}

impl std::fmt::Display for CoverageResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "lines: {:.1}% ({}/{}), functions: {:.1}%, regions: {:.1}%",
            self.line_percent,
            self.lines_covered,
            self.lines_total,
            self.function_percent,
            self.region_percent,
        )?;
        if let Some(bp) = self.branch_percent {
            write!(f, ", branches: {:.1}%", bp)?;
        }
        Ok(())
    }
}

/// Run a coverage command and parse the JSON output.
///
/// The command should produce LLVM coverage export JSON on stdout
/// (e.g. `cargo llvm-cov --json`).
pub fn run_coverage(command: &str, working_dir: &Path) -> Result<CoverageResult, CoverageError> {
    let output = Command::new("sh")
        .args(["-c", command])
        .current_dir(working_dir)
        .output()
        .map_err(|e| CoverageError::Execute {
            command: command.to_string(),
            source: e,
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CoverageError::CommandFailed {
            command: command.to_string(),
            stderr: stderr.into_owned(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_llvm_cov_json(&stdout)
}

/// Parse the JSON output from `cargo llvm-cov --json`.
///
/// Expected format (LLVM coverage export v2):
/// ```json
/// {
///   "data": [{
///     "totals": {
///       "lines":     { "count": N, "covered": M, "percent": P },
///       "functions": { "count": N, "covered": M, "percent": P },
///       "regions":   { "count": N, "covered": M, "percent": P },
///       "branches":  { "count": N, "covered": M, "percent": P }
///     }
///   }]
/// }
/// ```
pub fn parse_llvm_cov_json(json: &str) -> Result<CoverageResult, CoverageError> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| CoverageError::Parse {
            detail: format!("invalid JSON: {e}"),
        })?;

    let totals = value
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|arr| arr.first())
        .and_then(|entry| entry.get("totals"))
        .ok_or_else(|| CoverageError::Parse {
            detail: "missing data[0].totals in coverage JSON".to_string(),
        })?;

    let lines = totals.get("lines").ok_or_else(|| CoverageError::Parse {
        detail: "missing totals.lines".to_string(),
    })?;
    let functions = totals
        .get("functions")
        .ok_or_else(|| CoverageError::Parse {
            detail: "missing totals.functions".to_string(),
        })?;
    let regions = totals.get("regions").ok_or_else(|| CoverageError::Parse {
        detail: "missing totals.regions".to_string(),
    })?;

    let line_percent = lines["percent"].as_f64().unwrap_or(0.0);
    let lines_covered = lines["covered"].as_u64().unwrap_or(0);
    let lines_total = lines["count"].as_u64().unwrap_or(0);
    let function_percent = functions["percent"].as_f64().unwrap_or(0.0);
    let region_percent = regions["percent"].as_f64().unwrap_or(0.0);

    let branch_percent = totals
        .get("branches")
        .and_then(|b| b["percent"].as_f64());

    Ok(CoverageResult {
        line_percent,
        lines_covered,
        lines_total,
        function_percent,
        region_percent,
        branch_percent,
    })
}

/// Errors from coverage operations.
#[derive(Debug)]
pub enum CoverageError {
    /// Failed to execute the coverage command.
    Execute {
        command: String,
        source: std::io::Error,
    },
    /// Coverage command exited with non-zero status.
    CommandFailed { command: String, stderr: String },
    /// Failed to parse coverage JSON output.
    Parse { detail: String },
}

impl std::fmt::Display for CoverageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoverageError::Execute { command, source } => {
                write!(f, "failed to execute coverage command '{command}': {source}")
            }
            CoverageError::CommandFailed { command, stderr } => {
                write!(
                    f,
                    "coverage command '{command}' failed:\n{}",
                    stderr.lines().take(30).collect::<Vec<_>>().join("\n")
                )
            }
            CoverageError::Parse { detail } => {
                write!(f, "failed to parse coverage output: {detail}")
            }
        }
    }
}

impl std::error::Error for CoverageError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_llvm_cov_json() -> &'static str {
        r#"{
            "data": [{
                "totals": {
                    "lines": { "count": 1000, "covered": 750, "percent": 75.0 },
                    "functions": { "count": 100, "covered": 80, "percent": 80.0 },
                    "regions": { "count": 500, "covered": 350, "percent": 70.0 },
                    "branches": { "count": 200, "covered": 120, "percent": 60.0 }
                }
            }]
        }"#
    }

    #[test]
    fn test_parse_llvm_cov_json() {
        let result = parse_llvm_cov_json(sample_llvm_cov_json()).unwrap();
        assert!((result.line_percent - 75.0).abs() < 0.01);
        assert_eq!(result.lines_covered, 750);
        assert_eq!(result.lines_total, 1000);
        assert!((result.function_percent - 80.0).abs() < 0.01);
        assert!((result.region_percent - 70.0).abs() < 0.01);
        assert!((result.branch_percent.unwrap() - 60.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_no_branches() {
        let json = r#"{
            "data": [{
                "totals": {
                    "lines": { "count": 100, "covered": 90, "percent": 90.0 },
                    "functions": { "count": 10, "covered": 9, "percent": 90.0 },
                    "regions": { "count": 50, "covered": 45, "percent": 90.0 }
                }
            }]
        }"#;
        let result = parse_llvm_cov_json(json).unwrap();
        assert!((result.line_percent - 90.0).abs() < 0.01);
        assert!(result.branch_percent.is_none());
    }

    #[test]
    fn test_parse_invalid_json() {
        let result = parse_llvm_cov_json("not json");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid JSON"));
    }

    #[test]
    fn test_parse_missing_data() {
        let result = parse_llvm_cov_json(r#"{"version": "1"}"#);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("missing data[0].totals"));
    }

    #[test]
    fn test_parse_empty_data_array() {
        let result = parse_llvm_cov_json(r#"{"data": []}"#);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_missing_lines() {
        let json = r#"{"data": [{"totals": {"functions": {"count":0,"covered":0,"percent":0}}}]}"#;
        let result = parse_llvm_cov_json(json);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("missing totals.lines"));
    }

    #[test]
    fn test_coverage_result_display() {
        let result = CoverageResult {
            line_percent: 75.5,
            lines_covered: 755,
            lines_total: 1000,
            function_percent: 80.0,
            region_percent: 70.0,
            branch_percent: Some(60.0),
        };
        let display = result.to_string();
        assert!(display.contains("75.5%"));
        assert!(display.contains("755/1000"));
        assert!(display.contains("functions: 80.0%"));
        assert!(display.contains("branches: 60.0%"));
    }

    #[test]
    fn test_coverage_result_display_no_branches() {
        let result = CoverageResult {
            line_percent: 90.0,
            lines_covered: 900,
            lines_total: 1000,
            function_percent: 85.0,
            region_percent: 80.0,
            branch_percent: None,
        };
        let display = result.to_string();
        assert!(!display.contains("branches"));
    }

    #[test]
    fn test_run_coverage_with_echo() {
        let dir = tempfile::tempdir().unwrap();
        let json = sample_llvm_cov_json().replace('\n', " ");
        let cmd = format!("echo '{json}'");
        let result = run_coverage(&cmd, dir.path()).unwrap();
        assert!((result.line_percent - 75.0).abs() < 0.01);
    }

    #[test]
    fn test_run_coverage_command_fails() {
        let dir = tempfile::tempdir().unwrap();
        let result = run_coverage("exit 1", dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed"));
    }

    #[test]
    fn test_run_coverage_bad_json_output() {
        let dir = tempfile::tempdir().unwrap();
        let result = run_coverage("echo 'not json'", dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid JSON"));
    }

    #[test]
    fn test_coverage_error_display() {
        let err = CoverageError::Execute {
            command: "cargo llvm-cov".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
        };
        assert!(err.to_string().contains("cargo llvm-cov"));
        assert!(err.to_string().contains("not found"));

        let err = CoverageError::CommandFailed {
            command: "cargo llvm-cov".to_string(),
            stderr: "compilation error\ndetails here".to_string(),
        };
        assert!(err.to_string().contains("compilation error"));

        let err = CoverageError::Parse {
            detail: "bad format".to_string(),
        };
        assert!(err.to_string().contains("bad format"));
    }

    #[test]
    fn test_parse_zero_coverage() {
        let json = r#"{
            "data": [{
                "totals": {
                    "lines": { "count": 0, "covered": 0, "percent": 0.0 },
                    "functions": { "count": 0, "covered": 0, "percent": 0.0 },
                    "regions": { "count": 0, "covered": 0, "percent": 0.0 }
                }
            }]
        }"#;
        let result = parse_llvm_cov_json(json).unwrap();
        assert!((result.line_percent).abs() < 0.01);
        assert_eq!(result.lines_covered, 0);
        assert_eq!(result.lines_total, 0);
    }
}
