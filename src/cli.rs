use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "omma",
    about = "ὄμμα — a configurable eye-strain break reminder for macOS",
    version,
    disable_help_subcommand = true
)]
pub struct Args {
    /// take the next scheduled break now
    #[arg(short = 't', long = "take-break")]
    pub take_break: bool,

    /// take a short break now
    #[arg(short = 's', long = "short-break")]
    pub short_break: bool,

    /// take a long break now
    #[arg(short = 'l', long = "long-break")]
    pub long_break: bool,

    /// disable the running instance
    #[arg(short = 'd', long)]
    pub disable: bool,

    /// enable the running instance
    #[arg(short = 'e', long)]
    pub enable: bool,

    /// quit the running instance
    #[arg(short = 'q', long)]
    pub quit: bool,

    /// reload the running instance's config
    #[arg(long)]
    pub reload: bool,

    /// print the running instance's status and exit
    #[arg(long)]
    pub status: bool,

    /// print the default config as toml and exit
    #[arg(long = "print-default-config")]
    pub print_default_config: bool,
}

impl Args {
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
