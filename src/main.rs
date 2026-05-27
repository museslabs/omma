mod cli;
mod config;
mod ipc;
mod model;
mod scheduler;

#[cfg(target_os = "macos")]
mod platform;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let args = cli::Args::parse();

    if args.print_default_config {
        print!("{}", config::Config::default_toml()?);
        return Ok(());
    }

    // Action flags talk to an already-running agent.
    if let Some(cmd) = args.client_command() {
        let running = ipc::send(cmd)?;
        if !running {
            eprintln!("omma is not running.");
            std::process::exit(1);
        }
        return Ok(());
    }

    run_agent()
}

#[cfg(target_os = "macos")]
fn run_agent() -> anyhow::Result<()> {
    let cfg = config::Config::load();
    if cfg.build_queue().is_none() {
        anyhow::bail!("no breaks configured in {}", config::config_path().display());
    }

    let server = match ipc::Server::bind() {
        Ok(s) => s,
        Err(ipc::BindError::AlreadyRunning) => {
            eprintln!("omma is already running.");
            std::process::exit(1);
        }
        Err(e) => anyhow::bail!("could not start omma: {e}"),
    };

    platform::run(cfg, server)
}

#[cfg(not(target_os = "macos"))]
fn run_agent() -> anyhow::Result<()> {
    anyhow::bail!("omma currently supports macOS only")
}
