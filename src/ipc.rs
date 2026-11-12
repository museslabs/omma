//! one running instance per user. it holds a lock file, and clients reach it through a
//! socket in the same directory.

use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, BufRead, BufReader, ErrorKind, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
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

/// `~/Library/Application Support/omma`, where the running instance keeps its lock file and
/// socket. None when the home directory isn't an absolute path.
pub fn runtime_dir() -> Option<PathBuf> {
    std::env::home_dir()
        .filter(|home| home.is_absolute())
        .map(|home| home.join("Library/Application Support/omma"))
}

const LOCK_FILE: &str = "omma.lock";
const SOCKET: &str = "omma.sock";

pub struct Server {
    rx: Receiver<Command>,
    status: Arc<Mutex<String>>,
    /// locked for as long as the process lives. the kernel releases it however the
    /// process ends, so a crash leaves nothing to clean up.
    _lock: File,
}

/// takes the instance lock in `dir`, creating `dir` if needed, and listens on the socket
/// there. None when another instance holds the lock.
pub fn bind(dir: &Path) -> io::Result<Option<Server>> {
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join(LOCK_FILE))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(None),
        Err(TryLockError::Error(e)) => return Err(e),
    }

    // with the lock held, a socket left here is stale. bind reports anything else in the way.
    let path = dir.join(SOCKET);
    let _ = fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;

    let (tx, rx) = mpsc::channel();
    let status = Arc::new(Mutex::new(String::from("starting")));
    let status_for_thread = Arc::clone(&status);

    thread::Builder::new()
        .name("omma-ipc".into())
        .spawn(move || accept_loop(listener, tx, status_for_thread))?;

    Ok(Some(Server {
        rx,
        status,
        _lock: lock,
    }))
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
pub fn send(dir: &Path, cmd: Command) -> io::Result<bool> {
    let mut stream = match UnixStream::connect(dir.join(SOCKET)) {
        Ok(s) => s,
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
            return Ok(false);
        }
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
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// a fresh path under /tmp, removed on drop. a socket path must fit in 104 bytes,
    /// which a long $TMPDIR can exceed.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Scratch {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = PathBuf::from(format!("/tmp/omma-test-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn bind_creates_a_private_directory() {
        let scratch = Scratch::new();
        let _server = bind(&scratch.0).unwrap().unwrap();
        let mode = fs::metadata(&scratch.0).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn only_one_instance_holds_the_lock() {
        let scratch = Scratch::new();
        let first = bind(&scratch.0).unwrap().unwrap();
        assert!(bind(&scratch.0).unwrap().is_none());
        drop(first);
        assert!(bind(&scratch.0).unwrap().is_some());
    }

    #[test]
    fn a_stale_socket_does_not_block_startup() {
        let scratch = Scratch::new();
        fs::create_dir(&scratch.0).unwrap();
        drop(UnixListener::bind(scratch.0.join(SOCKET)).unwrap());
        assert!(scratch.0.join(SOCKET).exists());
        assert!(bind(&scratch.0).unwrap().is_some());
    }

    #[test]
    fn send_finds_nothing_running() {
        let scratch = Scratch::new();
        assert!(!send(&scratch.0, Command::Status).unwrap(), "no directory");
        fs::create_dir(&scratch.0).unwrap();
        drop(UnixListener::bind(scratch.0.join(SOCKET)).unwrap());
        assert!(
            !send(&scratch.0, Command::Status).unwrap(),
            "a stale socket"
        );
    }

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
