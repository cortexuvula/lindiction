use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use lindiction::app::{App, ExitAction};
use lindiction::autostart;
use lindiction::config::Config;
use lindiction::replace::{self, AddOutcome};
use std::path::PathBuf;
use std::process::ExitCode;
use tracing_subscriber::EnvFilter;

/// Lindiction — push-to-talk voice dictation for Linux.
///
/// Hold Ctrl+Alt+Space (or your configured binding) to record. Release to
/// transcribe and inject the text at the cursor.
///
/// Running with no subcommand starts the daemon.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Path to GGML whisper model file (overrides TOML config and env var).
    /// Only applies when running the daemon (no subcommand).
    #[arg(long, env = "LINDICTION_MODEL", global = true)]
    model: Option<PathBuf>,

    /// Verbose logging. -v = debug, -vv = trace
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Manage auto-start on graphical login via the systemd --user unit.
    #[command(subcommand)]
    Autostart(AutostartAction),
    /// Manage the [postprocess].replacements word-fix dictionary without
    /// hand-editing config.toml.
    #[command(subcommand)]
    Replace(ReplaceAction),
}

#[derive(Subcommand, Debug)]
enum AutostartAction {
    /// Enable auto-start on login.
    Enable,
    /// Disable auto-start on login.
    Disable,
    /// Print the current enabled/disabled status.
    Status,
}

#[derive(Subcommand, Debug)]
enum ReplaceAction {
    /// Add (or overwrite) a replacement: `lindiction replace add clod Claude`.
    /// Matches the running daemon's own behavior: case-insensitive,
    /// word-bounded.
    Add {
        /// The misheard word/phrase to look for.
        from: String,
        /// The text to substitute in (spelled how you want it to appear).
        to: String,
    },
    /// Print all configured replacements in file order.
    List,
    /// Remove the replacement whose `from` matches (case-insensitive).
    Remove {
        /// The `from` side of the entry to delete.
        from: String,
    },
    /// Open the config file in `$EDITOR` for free-form editing.
    Edit,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let level = match cli.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("lindiction={level},warn")));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    match cli.command {
        Some(Command::Autostart(action)) => match run_autostart(action) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::FAILURE
            }
        },
        Some(Command::Replace(action)) => match run_replace(action) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::FAILURE
            }
        },
        None => match run_daemon(cli.model) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::FAILURE
            }
        },
    }
}

/// Run an `autostart` subcommand synchronously. These shell out to
/// `systemctl --user` and don't need a tokio runtime.
fn run_autostart(action: AutostartAction) -> Result<()> {
    match action {
        AutostartAction::Enable => {
            autostart::enable()?;
            println!("{}", autostart::status().describe());
            Ok(())
        }
        AutostartAction::Disable => {
            autostart::disable()?;
            println!("{}", autostart::status().describe());
            Ok(())
        }
        AutostartAction::Status => {
            println!("{}", autostart::status().describe());
            Ok(())
        }
    }
}

/// Print the "restart the daemon to apply" hint after mutating the
/// config. Kept short on purpose — longer text buries the update
/// confirmation. The suggested command works if the user is on the
/// bundled systemd --user unit; if they run the daemon manually they
/// will figure out their own restart path from this hint.
fn print_restart_hint() {
    println!("Restart the daemon to apply: systemctl --user restart lindiction");
}

/// Run a `replace` subcommand. All actions touch only the config file
/// (or spawn $EDITOR) — they don't talk to a running daemon.
fn run_replace(action: ReplaceAction) -> Result<()> {
    match action {
        ReplaceAction::Add { from, to } => {
            match replace::add(&from, &to)? {
                AddOutcome::Added => {
                    println!("Added: {from} → {to}");
                }
                AddOutcome::Updated { previous } => {
                    println!("Updated: {from} → {to}  (was: {previous})");
                }
            }
            print_restart_hint();
            Ok(())
        }
        ReplaceAction::List => {
            let items = replace::list()?;
            if items.is_empty() {
                println!("(no replacements configured)");
                return Ok(());
            }
            // Left-aligned `from` column sized to the longest entry so
            // the arrows line up — poor man's table without bringing
            // in a formatting crate.
            let width = items
                .iter()
                .map(|(f, _)| f.chars().count())
                .max()
                .unwrap_or(0);
            for (from, to) in items {
                println!("{from:<width$}  →  {to}");
            }
            Ok(())
        }
        ReplaceAction::Remove { from } => {
            match replace::remove(&from)? {
                Some(prev) => {
                    println!("Removed: {from} → {prev}");
                    print_restart_hint();
                }
                None => {
                    println!("No replacement for `{from}` (case-insensitive match).");
                }
            }
            Ok(())
        }
        ReplaceAction::Edit => {
            replace::edit_in_editor()?;
            print_restart_hint();
            Ok(())
        }
    }
}

/// Entry point for the daemon. Kept in its own tokio runtime so
/// `run_autostart` can stay sync and avoid a runtime spin-up.
///
/// On `ExitAction::Restart`, replaces the current process image with a
/// fresh lindiction invocation, preserving argv and environment. The
/// replacement is invisible to systemd (PID and cgroup persist). If the
/// syscall fails we surface the error; the caller exits non-zero and the
/// unit's `Restart=on-failure` policy relaunches us.
fn run_daemon(cli_model: Option<PathBuf>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let action = runtime.block_on(async move {
        let config = Config::load(cli_model)?;
        App::run(config).await
    })?;
    // Drop the runtime so all tokio worker threads and background tasks
    // finish before we replace the process image. Leaving the runtime
    // alive across the replacement would leak file descriptors into the
    // new process that neither we nor systemd would track.
    drop(runtime);
    match action {
        ExitAction::Quit => Ok(()),
        ExitAction::Restart => replace_self_with_fresh_instance(),
    }
}

fn replace_self_with_fresh_instance() -> Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().context("determining current executable path")?;
    // args_os() preserves non-UTF-8 argv bytes. Skip argv[0] because
    // Command::new puts the binary path back at position 0.
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    tracing::info!(
        binary = %exe.display(),
        argc = args.len(),
        "restarting via execve syscall"
    );
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(&args);
    // Returns only on failure; success replaces the process image in place.
    let err = CommandExt::exec(&mut cmd);
    Err(anyhow!(
        "process image replacement failed: {err} (binary: {})",
        exe.display()
    ))
}
