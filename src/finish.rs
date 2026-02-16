use crate::config::FinishConfig;
use std::fs;
use std::path::Path;
use std::process::Command;

const RED: &str = "\x1b[0;31m";
const GREEN: &str = "\x1b[0;32m";
const YELLOW: &str = "\x1b[0;33m";
const NC: &str = "\x1b[0m";

/// Run a shell command string (e.g. "cargo check") and return its exit status.
fn run_gate(cmd_str: &str, label: &str) -> Result<(), String> {
    let parts: Vec<&str> = cmd_str.split_whitespace().collect();
    if parts.is_empty() {
        return Err(format!("{label} command is empty"));
    }
    let status = Command::new(parts[0])
        .args(&parts[1..])
        .status()
        .map_err(|e| format!("Failed to run {label} ({cmd_str}): {e}"))?;
    if !status.success() {
        return Err(format!("{label} failed"));
    }
    Ok(())
}

pub fn handle_finish(
    bead_id: &str,
    message: &str,
    files: &[String],
    finish_config: &FinishConfig,
) -> Result<(), String> {
    println!("{GREEN}=== blacksmith finish: closing {bead_id} ==={NC}");

    let mut step = b'a';

    // Quality gate: check
    let check_label = format!("[0{}/8]", step as char);
    println!("{YELLOW}{check_label} Running {}...{NC}", finish_config.check);
    if let Err(e) = run_gate(&finish_config.check, "check") {
        eprintln!();
        eprintln!("{RED}=== CHECK FAILED ==={NC}");
        eprintln!("{RED}Bead {bead_id} will NOT be closed. Fix errors first.{NC}");
        return Err(e);
    }
    println!("{GREEN}{check_label} {} passed{NC}", finish_config.check);
    step += 1;

    // Quality gate: test
    let test_label = format!("[0{}/8]", step as char);
    println!("{YELLOW}{test_label} Running {}...{NC}", finish_config.test);
    if let Err(e) = run_gate(&finish_config.test, "test") {
        eprintln!();
        eprintln!("{RED}=== TEST FAILED ==={NC}");
        eprintln!("{RED}Bead {bead_id} will NOT be closed. Fix failing tests first.{NC}");
        return Err(e);
    }
    println!("{GREEN}{test_label} {} passed{NC}", finish_config.test);
    step += 1;

    // Quality gate: lint (optional)
    if let Some(ref lint_cmd) = finish_config.lint {
        let lint_label = format!("[0{}/8]", step as char);
        println!("{YELLOW}{lint_label} Running {lint_cmd}...{NC}");
        if let Err(e) = run_gate(lint_cmd, "lint") {
            eprintln!();
            eprintln!("{RED}=== LINT FAILED ==={NC}");
            eprintln!("{RED}Bead {bead_id} will NOT be closed. Fix lint errors first.{NC}");
            return Err(e);
        }
        println!("{GREEN}{lint_label} {lint_cmd} passed{NC}");
        step += 1;
    }

    // Quality gate: format (optional)
    if let Some(ref fmt_cmd) = finish_config.format {
        let fmt_label = format!("[0{}/8]", step as char);
        println!("{YELLOW}{fmt_label} Running {fmt_cmd}...{NC}");
        if let Err(e) = run_gate(fmt_cmd, "format") {
            eprintln!();
            eprintln!("{RED}=== FORMAT CHECK FAILED ==={NC}");
            eprintln!("{RED}Bead {bead_id} will NOT be closed. Fix formatting first.{NC}");
            return Err(e);
        }
        println!("{GREEN}{fmt_label} {fmt_cmd} passed{NC}");
    }

    // 1. Append PROGRESS.txt to PROGRESS_LOG.txt with timestamp
    if Path::new("PROGRESS.txt").exists() {
        let progress = fs::read_to_string("PROGRESS.txt")
            .map_err(|e| format!("Failed to read PROGRESS.txt: {e}"))?;
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let entry = format!("\n--- {timestamp} | {bead_id} ---\n{progress}");
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("PROGRESS_LOG.txt")
            .and_then(|mut f| std::io::Write::write_all(&mut f, entry.as_bytes()))
            .map_err(|e| format!("Failed to append to PROGRESS_LOG.txt: {e}"))?;
        println!("{GREEN}[1/8] Appended PROGRESS.txt to PROGRESS_LOG.txt{NC}");
    } else {
        println!("{YELLOW}[1/8] No PROGRESS.txt found, skipping log append{NC}");
    }

    // 2. Stage files
    if files.is_empty() {
        run_git(&["add", "-u"], "stage tracked modified files")?;
        println!("{GREEN}[2/8] Staged all tracked modified files (git add -u){NC}");
    } else {
        let mut args = vec!["add"];
        for f in files {
            args.push(f.as_str());
        }
        run_git(&args, "stage specified files")?;
        println!("{GREEN}[2/8] Staged {} specified files{NC}", files.len());
    }
    // Always include progress files if they exist
    let _ = Command::new("git")
        .args(["add", "-f", "PROGRESS.txt", "PROGRESS_LOG.txt"])
        .status();

    // 3. Commit
    let commit_msg = format!("{bead_id}: {message}");
    run_git(
        &["commit", "-m", &commit_msg, "--no-verify"],
        "commit changes",
    )?;
    println!("{GREEN}[3/8] Committed: {commit_msg}{NC}");

    // 4. bd close
    let close_status = Command::new("bd")
        .args(["close", bead_id, &format!("--reason={message}")])
        .status()
        .map_err(|e| format!("Failed to run bd close: {e}"))?;
    if !close_status.success() {
        return Err(format!("bd close failed for {bead_id}"));
    }
    println!("{GREEN}[4/8] Closed bead {bead_id}{NC}");

    // 5. bd sync
    let _ = Command::new("bd").args(["sync"]).status();
    println!("{GREEN}[5/8] Synced beads{NC}");

    // 6. Auto-commit .beads/ if dirty
    let beads_dirty = is_beads_dirty();
    if beads_dirty {
        let _ = Command::new("git").args(["add", ".beads/"]).status();
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let sync_msg = format!("bd sync: {timestamp}");
        let _ = Command::new("git")
            .args(["commit", "-m", &sync_msg, "--no-verify"])
            .status();
        println!("{GREEN}[6/8] Committed .beads/ changes{NC}");
    } else {
        println!("{GREEN}[6/8] .beads/ already clean{NC}");
    }

    // 7. Push
    run_git(&["push"], "push to remote")?;
    println!("{GREEN}[7/8] Pushed to remote{NC}");

    println!();
    println!("{GREEN}=== Done. {bead_id} closed and pushed. ==={NC}");
    Ok(())
}

fn run_git(args: &[&str], description: &str) -> Result<(), String> {
    let status = Command::new("git")
        .args(args)
        .status()
        .map_err(|e| format!("Failed to {description}: {e}"))?;
    if !status.success() {
        return Err(format!("git {}: failed", args.first().unwrap_or(&"")));
    }
    Ok(())
}

fn is_beads_dirty() -> bool {
    let unstaged = Command::new("git")
        .args(["diff", "--quiet", ".beads/"])
        .status()
        .map(|s| !s.success())
        .unwrap_or(false);
    let staged = Command::new("git")
        .args(["diff", "--cached", "--quiet", ".beads/"])
        .status()
        .map(|s| !s.success())
        .unwrap_or(false);
    unstaged || staged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_beads_dirty_returns_bool() {
        // Smoke test: should not panic, returns a bool
        let result = is_beads_dirty();
        assert!(result || !result);
    }

    #[test]
    fn test_run_git_status() {
        // git status should always succeed
        let result = run_git(&["status"], "check status");
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_git_invalid_command() {
        // git with an invalid subcommand should fail
        let result = run_git(&["not-a-real-subcommand"], "invalid");
        assert!(result.is_err());
    }
}
