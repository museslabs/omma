use clap::Parser;
use omma::{cli, config, ipc};

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

    run_app()
}

#[cfg(target_os = "macos")]
fn run_app() -> anyhow::Result<()> {
    use omma::ipc::CommandTransport;

    let cfg = config::Config::load();
    let server = match ipc::LocalSocketTransport.bind() {
        Ok(s) => s,
        Err(ipc::BindError::AlreadyRunning) => {
            eprintln!("omma is already running.");
            std::process::exit(1);
        }
        Err(e) => anyhow::bail!("could not start omma: {e}"),
    };

    omma::backend::macos::MacosBackend.run(cfg, server)
}

#[cfg(not(target_os = "macos"))]
fn run_app() -> anyhow::Result<()> {
    anyhow::bail!("omma currently supports macOS only")
}
