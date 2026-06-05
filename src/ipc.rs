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

impl Command {
    pub fn as_wire(self) -> &'static str {
        match self {
            Command::TakeBreak => "take-break",
            Command::TakeShortBreak => "short-break",
            Command::TakeLongBreak => "long-break",
            Command::Enable => "enable",
            Command::Disable => "disable",
            Command::Quit => "quit",
            Command::Status => "status",
            Command::Reload => "reload",
        }
    }

    pub fn from_wire(s: &str) -> Option<Command> {
        Some(match s.trim() {
            "take-break" => Command::TakeBreak,
            "short-break" => Command::TakeShortBreak,
            "long-break" => Command::TakeLongBreak,
            "enable" => Command::Enable,
            "disable" => Command::Disable,
            "quit" => Command::Quit,
            "status" => Command::Status,
            "reload" => Command::Reload,
            _ => return None,
        })
    }
}

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

pub trait CommandClient {
    fn send(&self, cmd: Command) -> std::io::Result<bool>;
}

pub trait CommandTransport {
    type Server;

    fn bind(&self) -> Result<Self::Server, BindError>;
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod local_socket {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::{BindError, Command, CommandClient, CommandTransport};

    #[derive(Clone, Copy, Debug, Default)]
    pub struct LocalSocketTransport;

    pub fn socket_path() -> PathBuf {
        std::env::temp_dir().join("omma.sock")
    }

    pub struct Server {
        pub rx: Receiver<Command>,
        pub status: Arc<Mutex<String>>,
    }

    impl CommandTransport for LocalSocketTransport {
        type Server = Server;

        fn bind(&self) -> Result<Server, BindError> {
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

            let reply = match Command::from_wire(&line) {
                Some(Command::Status) => status
                    .lock()
                    .map(|g| g.clone())
                    .unwrap_or_else(|_| "unknown".into()),
                Some(cmd) => {
                    let _ = tx.send(cmd);
                    "ok".into()
                }
                None => "error: unknown command".into(),
            };
            let _ = writeln!(stream, "{reply}");
        }
    }

    impl CommandClient for LocalSocketTransport {
        /// returns Ok(false) if no app is running
        fn send(&self, cmd: Command) -> std::io::Result<bool> {
            let path = socket_path();
            let mut stream = match UnixStream::connect(&path) {
                Ok(s) => s,
                Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(ref e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                    return Ok(false)
                }
                Err(e) => return Err(e),
            };
            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
            writeln!(stream, "{}", cmd.as_wire())?;
            stream.flush()?;

            let mut resp = String::new();
            stream.read_to_string(&mut resp)?;
            let resp = resp.trim();
            if !resp.is_empty() {
                println!("{resp}");
            }
            Ok(true)
        }
    }

    pub fn send(cmd: Command) -> std::io::Result<bool> {
        LocalSocketTransport.send(cmd)
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use local_socket::{send, socket_path, LocalSocketTransport, Server};

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn send(_cmd: Command) -> std::io::Result<bool> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "IPC transport is not implemented on this platform",
    ))
}
