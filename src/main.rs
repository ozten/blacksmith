mod config;
mod hooks;
mod metrics;
mod retry;
mod runner;
mod session;
mod signals;
mod watchdog;

use clap::Parser;
use std::path::PathBuf;

/// A Rust CLI tool that runs an AI coding agent in a supervised loop:
/// dispatch a prompt, monitor the session, enforce health invariants,
/// collect metrics, and repeat.
#[derive(Parser, Debug)]
#[command(name = "simple-agent-harness", version, about)]
pub struct Cli {
    /// Override max iterations (default: from config)
    #[arg(value_name = "MAX_ITERATIONS")]
    max_iterations: Option<u32>,

    /// Config file path
    #[arg(short, long, default_value = "harness.toml")]
    config: PathBuf,

    /// Prompt file path (overrides config)
    #[arg(short, long)]
    prompt: Option<PathBuf>,

    /// Output directory (overrides config)
    #[arg(short, long)]
    output_dir: Option<PathBuf>,

    /// Stale timeout in minutes (overrides config)
    #[arg(long)]
    timeout: Option<u64>,

    /// Max empty retries (overrides config)
    #[arg(long)]
    retries: Option<u32>,

    /// Validate config and print resolved settings, don't run
    #[arg(long)]
    dry_run: bool,

    /// Extra logging (watchdog checks, retry decisions)
    #[arg(short, long)]
    verbose: bool,

    /// Suppress per-iteration banners, only errors and summary
    #[arg(short, long)]
    quiet: bool,

    /// Print current loop state and exit
    #[arg(long)]
    status: bool,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_ids(false)
        .init();

    tracing::info!("simple-agent-harness starting");
    tracing::debug!(?cli, "parsed CLI arguments");

    // TODO: Load config, merge CLI overrides, and run the main loop
    println!("simple-agent-harness v{}", env!("CARGO_PKG_VERSION"));
    println!("Config file: {}", cli.config.display());

    if cli.dry_run {
        println!("Dry run mode — config validated, not running.");
        return;
    }

    if cli.status {
        println!("Status mode — not yet implemented.");
        return;
    }

    println!("Main loop not yet implemented.");
}
