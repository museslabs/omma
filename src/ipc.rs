use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    TakeBreak,
    TakeShortBreak,
    TakeLongBreak,
    Enable,
    Disable,
    Quit,
    Status,
    Reload,
}

/// each command's name on the wire. renaming one breaks CLIs talking to an older instance.
const COMMANDS: [(Command, &str); 8] = [
    (Command::TakeBreak, "take-break"),
    (Command::TakeShortBreak, "short-break"),
    (Command::TakeLongBreak, "long-break"),
    (Command::Enable, "enable"),
    (Command::Disable, "disable"),
    (Command::Quit, "quit"),
    (Command::Status, "status"),
    (Command::Reload, "reload"),
];

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (_, name) = COMMANDS
            .iter()
            .find(|(command, _)| command == self)
            .expect("COMMANDS names every command");
        f.write_str(name)
    }
}

impl FromStr for Command {
    type Err = UnknownCommand;

    fn from_str(s: &str) -> Result<Command, UnknownCommand> {
        COMMANDS
            .iter()
            .find(|(_, name)| *name == s)
            .map(|(command, _)| *command)
            .ok_or(UnknownCommand)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct UnknownCommand;

#[derive(Debug)]
pub enum BindError {
    AlreadyRunning,
    Io(std::io::Error),
}

impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BindError::AlreadyRunning => write!(f, "omma is already running"),
            BindError::Io(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for BindError {}

pub fn socket_path() -> PathBuf {
    std::env::temp_dir().join("omma.sock")
}

pub struct Server {
    rx: Receiver<Command>,
    status: Arc<Mutex<String>>,
}

pub fn bind() -> Result<Server, BindError> {
    let path = socket_path();

    // a live app owns the socket if connect succeeds
    if path.exists() {
        match UnixStream::connect(&path) {
            Ok(_) => return Err(BindError::AlreadyRunning),
            Err(_) => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    let listener = UnixListener::bind(&path).map_err(BindError::Io)?;

    let (tx, rx) = mpsc::channel();
    let status = Arc::new(Mutex::new(String::from("starting")));
    let status_for_thread = Arc::clone(&status);

    std::thread::Builder::new()
        .name("omma-ipc".into())
        .spawn(move || accept_loop(listener, tx, status_for_thread))
        .map_err(BindError::Io)?;

    Ok(Server { rx, status })
}

impl Server {
    pub fn drain(&self) -> Vec<Command> {
        self.rx.try_iter().collect()
    }

    pub fn set_status(&self, s: String) {
        if let Ok(mut g) = self.status.lock() {
            *g = s;
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(socket_path());
    }
}

fn accept_loop(listener: UnixListener, tx: Sender<Command>, status: Arc<Mutex<String>>) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));

        let mut reader = BufReader::new(match stream.try_clone() {
            Ok(s) => s,
            Err(_) => continue,
        });
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            continue;
        }

        let reply = match line.trim().parse() {
            Ok(Command::Status) => status
                .lock()
                .map(|g| g.clone())
                .unwrap_or_else(|_| "unknown".into()),
            Ok(cmd) => {
                let _ = tx.send(cmd);
                "ok".into()
            }
            Err(UnknownCommand) => "error: unknown command".into(),
        };
        let _ = writeln!(stream, "{reply}");
    }
}

/// returns Ok(false) if no app is running
pub fn send(cmd: Command) -> std::io::Result<bool> {
    let path = socket_path();
    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(ref e) if e.kind() == std::io::ErrorKind::ConnectionRefused => return Ok(false),
        Err(e) => return Err(e),
    };
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    writeln!(stream, "{cmd}")?;
    stream.flush()?;

    let mut resp = String::new();
    stream.read_to_string(&mut resp)?;
    let resp = resp.trim();
    if !resp.is_empty() {
        println!("{resp}");
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_round_trip() {
        for (command, name) in COMMANDS {
            assert_eq!(command.to_string(), name);
            assert_eq!(name.parse(), Ok(command));
        }
    }

    #[test]
    fn every_command_has_a_wire_name() {
        use Command::*;
        for command in [
            TakeBreak,
            TakeShortBreak,
            TakeLongBreak,
            Enable,
            Disable,
            Quit,
            Status,
            Reload,
        ] {
            // a new command stops this compiling: add it to the match and to the list above
            match command {
                TakeBreak | TakeShortBreak | TakeLongBreak | Enable | Disable | Quit | Status
                | Reload => {}
            }
            assert!(COMMANDS.iter().any(|(c, _)| *c == command), "{command:?}");
        }
    }

    #[test]
    fn wire_names_are_distinct() {
        for (i, (a, name_a)) in COMMANDS.iter().enumerate() {
            for (b, name_b) in &COMMANDS[i + 1..] {
                assert_ne!(a, b);
                assert_ne!(name_a, name_b);
            }
        }
    }

    #[test]
    fn unknown_names_are_rejected() {
        assert_eq!("".parse::<Command>(), Err(UnknownCommand));
        assert_eq!("Status".parse::<Command>(), Err(UnknownCommand));
    }
}
