use clap::Parser;

use crate::ipc::Command;

#[derive(Parser, Debug)]
#[command(version, about)]
pub struct Args {
    #[command(flatten)]
    action: Action,
}

/// at most one of these per run
#[derive(clap::Args, Debug)]
#[group(multiple = false)]
struct Action {
    /// take the next scheduled break now
    #[arg(short = 't', long)]
    take_break: bool,

    /// take a short break now
    #[arg(short = 's', long)]
    short_break: bool,

    /// take a long break now
    #[arg(short = 'l', long)]
    long_break: bool,

    /// disable the running instance
    #[arg(short = 'd', long)]
    disable: bool,

    /// enable the running instance
    #[arg(short = 'e', long)]
    enable: bool,

    /// quit the running instance
    #[arg(short = 'q', long)]
    quit: bool,

    /// reload the running instance's config
    #[arg(long)]
    reload: bool,

    /// print the running instance's status and exit
    #[arg(long)]
    status: bool,

    /// print the default config as toml and exit
    #[arg(long)]
    print_default_config: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    /// start the menu-bar app
    Run,
    PrintDefaultConfig,
    /// send a command to the running instance
    Send(Command),
}

impl Args {
    pub fn mode(&self) -> Mode {
        let a = &self.action;
        if a.print_default_config {
            return Mode::PrintDefaultConfig;
        }
        [
            (a.take_break, Command::TakeBreak),
            (a.short_break, Command::TakeShortBreak),
            (a.long_break, Command::TakeLongBreak),
            (a.disable, Command::Disable),
            (a.enable, Command::Enable),
            (a.quit, Command::Quit),
            (a.reload, Command::Reload),
            (a.status, Command::Status),
        ]
        .into_iter()
        .find_map(|(set, command)| set.then_some(Mode::Send(command)))
        .unwrap_or(Mode::Run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use clap::error::ErrorKind;

    /// every spelling of every flag, as the README documents them
    const FLAGS: [(&str, Mode); 15] = [
        ("-t", Mode::Send(Command::TakeBreak)),
        ("--take-break", Mode::Send(Command::TakeBreak)),
        ("-s", Mode::Send(Command::TakeShortBreak)),
        ("--short-break", Mode::Send(Command::TakeShortBreak)),
        ("-l", Mode::Send(Command::TakeLongBreak)),
        ("--long-break", Mode::Send(Command::TakeLongBreak)),
        ("-d", Mode::Send(Command::Disable)),
        ("--disable", Mode::Send(Command::Disable)),
        ("-e", Mode::Send(Command::Enable)),
        ("--enable", Mode::Send(Command::Enable)),
        ("-q", Mode::Send(Command::Quit)),
        ("--quit", Mode::Send(Command::Quit)),
        ("--reload", Mode::Send(Command::Reload)),
        ("--status", Mode::Send(Command::Status)),
        ("--print-default-config", Mode::PrintDefaultConfig),
    ];

    fn mode(argv: &[&str]) -> Result<Mode, ErrorKind> {
        Args::try_parse_from(argv)
            .map(|args| args.mode())
            .map_err(|e| e.kind())
    }

    #[test]
    fn definition_is_valid() {
        Args::command().debug_assert();
    }

    #[test]
    fn no_flag_runs_the_app() {
        assert_eq!(mode(&["omma"]), Ok(Mode::Run));
    }

    #[test]
    fn each_flag_alone_picks_its_mode() {
        for (flag, expected) in FLAGS {
            assert_eq!(mode(&["omma", flag]), Ok(expected), "{flag}");
        }
    }

    #[test]
    fn flags_for_different_modes_conflict() {
        for (a, mode_a) in FLAGS {
            for (b, mode_b) in FLAGS {
                if mode_a == mode_b {
                    continue;
                }
                assert_eq!(
                    mode(&["omma", a, b]),
                    Err(ErrorKind::ArgumentConflict),
                    "{a} {b}"
                );
            }
        }
    }
}
