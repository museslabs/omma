#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("omma supports macOS and Linux with a Wayland compositor only");

/// writes `omma: <message>` to the running app's stderr and drops a failed write. eprintln!
/// panics after the terminal that started omma closes
macro_rules! log {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), "omma: {}", format_args!($($arg)*));
    }};
}

mod app;
mod backend;
mod cli;
mod config;
mod ipc;
mod mati;
mod queue;
mod scheduler;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use backend::{Backend, Platform};
use clap::Parser;
use cli::Mode;
use ipc::Command;

// exit codes, as the README lists them. clap exits with 2 on invalid flags
const NOT_RUNNING: u8 = 1;
const ALREADY_RUNNING: u8 = 1;
const REFUSED: u8 = 2;
const FAILED: u8 = 3;

fn main() -> ExitCode {
    let result = match cli::Args::parse().mode() {
        Mode::Run => run(),
        Mode::PrintDefaultConfig => {
            print!("{}", config::DEFAULT_CONFIG);
            Ok(ExitCode::SUCCESS)
        }
        Mode::Send(command) => send(command),
    };
    result.unwrap_or_else(|e| {
        eprintln!("omma: {e:#}");
        ExitCode::from(FAILED)
    })
}

fn send(command: Command) -> anyhow::Result<ExitCode> {
    Ok(match ipc::send(&runtime_dir()?, command)? {
        Some(Ok(text)) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Some(Err(text)) => {
            eprintln!("{text}");
            ExitCode::from(REFUSED)
        }
        None => {
            eprintln!("omma is not running.");
            ExitCode::from(NOT_RUNNING)
        }
    })
}

/// takes the instance lock before reading the config, so a second instance neither writes
/// a default config nor reports a config error
fn run() -> anyhow::Result<ExitCode> {
    let system = Platform::check_system()?;
    let dir = runtime_dir()?;
    let Some(server) =
        ipc::bind(&dir).with_context(|| format!("can't start in {}", dir.display()))?
    else {
        eprintln!("omma is already running.");
        return Ok(ExitCode::from(ALREADY_RUNNING));
    };

    let config_path = config::config_path()
        .context("can't locate the config file: set HOME or XDG_CONFIG_HOME to an absolute path")?;
    let (cfg, config_error) = config::load_or_create(&config_path);
    if let Some(e) = &config_error {
        log!("{}: {e}; using the defaults", config_path.display());
    }

    Platform::run(system, cfg, config_path, config_error, server)?;
    Ok(ExitCode::SUCCESS)
}

fn runtime_dir() -> anyhow::Result<PathBuf> {
    #[cfg(target_os = "macos")]
    const HINT: &str = "can't locate the home directory: set HOME to an absolute path";
    #[cfg(not(target_os = "macos"))]
    const HINT: &str = "can't locate a runtime directory: set XDG_RUNTIME_DIR or HOME to an \
                        absolute path";
    ipc::runtime_dir().context(HINT)
}
