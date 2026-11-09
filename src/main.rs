#[cfg(not(target_os = "macos"))]
compile_error!("omma supports macOS only");

mod backend;
mod cli;
mod config;
mod controller;
mod ipc;
mod model;
mod scheduler;

use clap::Parser;
use ipc::CommandTransport;

fn main() -> anyhow::Result<()> {
    let args = cli::Args::parse();

    if args.print_default_config {
        print!("{}", config::Config::default_toml()?);
        return Ok(());
    }

    if let Some(cmd) = args.client_command() {
        let running = ipc::send(cmd)?;
        if !running {
            eprintln!("omma is not running.");
            std::process::exit(1);
        }
        return Ok(());
    }

    let cfg = config::Config::load();
    let server = match ipc::LocalSocketTransport.bind() {
        Ok(s) => s,
        Err(ipc::BindError::AlreadyRunning) => {
            eprintln!("omma is already running.");
            std::process::exit(1);
        }
        Err(e) => anyhow::bail!("could not start omma: {e}"),
    };

    backend::macos::MacosBackend.run(cfg, server)
}
