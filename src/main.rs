#[cfg(not(target_os = "macos"))]
compile_error!("omma supports macOS only");

mod app;
mod cli;
mod config;
mod ipc;
mod queue;
mod scheduler;

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use cli::Mode;

fn main() -> anyhow::Result<()> {
    match cli::Args::parse().mode() {
        Mode::Run => {}
        Mode::PrintDefaultConfig => {
            print!("{}", config::DEFAULT_CONFIG);
            return Ok(());
        }
        Mode::Send(cmd) => {
            let running = ipc::send(&runtime_dir()?, cmd)?;
            if !running {
                eprintln!("omma is not running.");
                std::process::exit(1);
            }
            return Ok(());
        }
    }

    let dir = runtime_dir()?;
    let Some(server) =
        ipc::bind(&dir).with_context(|| format!("could not start omma in {}", dir.display()))?
    else {
        eprintln!("omma is already running.");
        std::process::exit(1);
    };

    let config_path = config::config_path().ok_or_else(|| {
        anyhow::anyhow!(
            "can't locate the config file: set HOME or XDG_CONFIG_HOME to an absolute path"
        )
    })?;
    let (cfg, config_error) = config::load_or_create(&config_path);
    if let Some(e) = &config_error {
        eprintln!("omma: {}: {e}; using the defaults", config_path.display());
    }

    app::run(cfg, config_path, config_error, server)
}

fn runtime_dir() -> anyhow::Result<PathBuf> {
    ipc::runtime_dir().context("can't locate the home directory: set HOME to an absolute path")
}
