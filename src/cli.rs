//! Command-line interface. With no flags, `omma` starts the menu-bar agent.
//! The action flags are sent to an already-running instance over a local
//! socket (see `ipc`).

use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "omma",
    about = "ὄμμα — a configurable eye-strain break reminder for macOS",
    version,
    disable_help_subcommand = true
)]
pub struct Args {
    /// Take the next scheduled break now.
    #[arg(short = 't', long = "take-break")]
    pub take_break: bool,

    /// Take a short break now.
    #[arg(short = 's', long = "short-break")]
    pub short_break: bool,

    /// Take a long break now.
    #[arg(short = 'l', long = "long-break")]
    pub long_break: bool,

    /// Disable the running instance.
    #[arg(short = 'd', long)]
    pub disable: bool,

    /// Enable the running instance.
    #[arg(short = 'e', long)]
    pub enable: bool,

    /// Quit the running instance.
    #[arg(short = 'q', long)]
    pub quit: bool,

    /// Reload the config of the running instance now.
    #[arg(long)]
    pub reload: bool,

    /// Print the status of the running instance and exit.
    #[arg(long)]
    pub status: bool,

    /// Print the default configuration as TOML and exit.
    #[arg(long = "print-default-config")]
    pub print_default_config: bool,
}

impl Args {
    /// If the user asked to control a running instance, return the command to
    /// send. `None` means "run the agent".
    pub fn client_command(&self) -> Option<crate::ipc::Command> {
        use crate::ipc::Command;
        if self.quit {
            Some(Command::Quit)
        } else if self.reload {
            Some(Command::Reload)
        } else if self.disable {
            Some(Command::Disable)
        } else if self.enable {
            Some(Command::Enable)
        } else if self.short_break {
            Some(Command::TakeShortBreak)
        } else if self.long_break {
            Some(Command::TakeLongBreak)
        } else if self.take_break {
            Some(Command::TakeBreak)
        } else if self.status {
            Some(Command::Status)
        } else {
            None
        }
    }
}
