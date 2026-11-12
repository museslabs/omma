#[cfg(not(target_os = "macos"))]
compile_error!("omma supports macOS only");

mod app;
mod cli;
mod config;
mod ipc;
mod queue;
mod scheduler;

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
            let running = ipc::send(cmd)?;
            if !running {
                eprintln!("omma is not running.");
                std::process::exit(1);
            }
            return Ok(());
        }
    }

    let config_path = config::config_path().ok_or_else(|| {
        anyhow::anyhow!(
            "can't locate the config file: set HOME or XDG_CONFIG_HOME to an absolute path"
        )
    })?;
    let (cfg, config_error) = config::load_or_create(&config_path);
    if let Some(e) = &config_error {
        eprintln!("omma: {}: {e}; using the defaults", config_path.display());
    }
    let server = match ipc::bind() {
        Ok(s) => s,
        Err(ipc::BindError::AlreadyRunning) => {
            eprintln!("omma is already running.");
            std::process::exit(1);
        }
        Err(e) => anyhow::bail!("could not start omma: {e}"),
    };

    app::run(cfg, config_path, config_error, server)
}
