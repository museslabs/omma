mod config;
mod model;
mod scheduler;

#[cfg(target_os = "macos")]
mod platform;

fn main() -> anyhow::Result<()> {
    run_agent()
}

#[cfg(target_os = "macos")]
fn run_agent() -> anyhow::Result<()> {
    let cfg = config::Config::load();
    if cfg.build_queue().is_none() {
        anyhow::bail!("no breaks configured in {}", config::config_path().display());
    }

    platform::run(cfg)
}

#[cfg(not(target_os = "macos"))]
fn run_agent() -> anyhow::Result<()> {
    anyhow::bail!("omma currently supports macOS only")
}
