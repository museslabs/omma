#[cfg(not(target_os = "macos"))]
compile_error!("omma supports macOS only");

mod app;
mod cli;
mod config;
mod ipc;
mod queue;
mod scheduler;

use clap::Parser;

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
    let server = match ipc::bind() {
        Ok(s) => s,
        Err(ipc::BindError::AlreadyRunning) => {
            eprintln!("omma is already running.");
            std::process::exit(1);
        }
        Err(e) => anyhow::bail!("could not start omma: {e}"),
    };

    app::run(cfg, server)
}
