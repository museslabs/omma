//! the platform omma runs on: its widgets, its event loop and the system services the
//! do-not-disturb rules, smart pause and sounds read. the schedule and everything around it
//! is in `app`, the same on every platform

use std::path::PathBuf;

use crate::config::{Config, ConfigError};
use crate::ipc::Server;

pub trait Backend {
    /// what `check_system` found, which `run` goes on with: the main-thread marker on macOS,
    /// the connection to the compositor on Wayland. `run` must reuse the connection, because
    /// some connections can be made only once
    type System;

    /// fails when the system cannot show omma's windows, before omma takes the instance lock
    fn check_system() -> anyhow::Result<Self::System>;

    /// takes over the process, and returns only on quit or a fatal error. `config_error`
    /// tells why the file at `config_path` did not load, and `config` then holds the defaults
    fn run(
        system: Self::System,
        config: Config,
        config_path: PathBuf,
        config_error: Option<ConfigError>,
        ipc: Server,
    ) -> anyhow::Result<()>;
}

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "linux")]
pub mod wayland;

/// the backend for the platform omma was built for
#[cfg(target_os = "macos")]
pub type Platform = macos::Macos;
#[cfg(target_os = "linux")]
pub type Platform = wayland::Wayland;
