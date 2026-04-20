use anyhow::Result;
use clap::Parser;
use lindiction::app::App;
use lindiction::config::Config;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

/// Lindiction — push-to-talk voice dictation for Linux.
///
/// Hold Ctrl+Alt+Space (or your configured binding) to record. Release to
/// transcribe and inject the text at the cursor.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Path to GGML whisper model file (overrides TOML config and env var)
    #[arg(long, env = "LINDICTION_MODEL")]
    model: Option<PathBuf>,

    /// Verbose logging. -v = debug, -vv = trace
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let level = match cli.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("lindiction={level},warn")));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let config = Config::load(cli.model)?;

    App::run(config).await
}
