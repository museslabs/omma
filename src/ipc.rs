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
use std::thread;
use std::time::Duration;

use anyhow::{Context, bail};

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

/// what a command did, or why the running instance refused it
pub type Reply = Result<String, String>;

/// `~/Library/Application Support/omma`, where the running instance keeps its lock file and
/// socket. None when the home directory isn't an absolute path.
pub fn runtime_dir() -> Option<PathBuf> {
    std::env::home_dir()
        .filter(|home| home.is_absolute())
        .map(|home| home.join("Library/Application Support/omma"))
}

const LOCK_FILE: &str = "omma.lock";
const SOCKET: &str = "omma.sock";
/// a client writes its command as soon as it connects
const READ_TIMEOUT: Duration = Duration::from_secs(1);
/// a command line is cut off here. the longest command name is 11 bytes.
const MAX_LINE: u64 = 64;
/// the main thread writes replies, and mustn't wait on a client
const WRITE_TIMEOUT: Duration = Duration::from_millis(100);
/// keeps a persistent accept error, such as running out of file descriptors, from spinning
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
/// covers the main thread's next tick, up to a second away
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);

pub struct Server {
    rx: Receiver<Request>,
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
    thread::Builder::new()
        .name("omma-ipc".into())
        .spawn(move || accept_loop(listener, tx))?;

    Ok(Some(Server { rx, _lock: lock }))
}

impl Server {
    pub fn drain(&self) -> Vec<Request> {
        self.rx.try_iter().collect()
    }
}

/// a client's command, waiting for its reply. the connection closes when the request is
/// dropped, and the client waits for that.
pub struct Request {
    pub command: Command,
    stream: UnixStream,
}

impl Request {
    pub fn reply(&self, reply: &Reply) {
        // a client that hung up has nothing to read it
        let _ = write!(&self.stream, "{}", encode(reply));
    }
}

/// reads each connection on its own thread, so a slow client holds up only itself
fn accept_loop(listener: UnixListener, tx: Sender<Request>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            thread::sleep(ACCEPT_BACKOFF);
            continue;
        };
        let tx = tx.clone();
        // a connection that gets no thread is dropped, and its client sees no reply
        let _ = thread::Builder::new()
            .name("omma-ipc-client".into())
            .spawn(move || {
                if let Some(request) = read_request(stream) {
                    let _ = tx.send(request);
                }
            });
    }
}

/// None when the client sent no command in time, or one this instance doesn't know, which
/// is answered here
fn read_request(stream: UnixStream) -> Option<Request> {
    stream.set_read_timeout(Some(READ_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT)).ok()?;
    let mut line = String::new();
    BufReader::new((&stream).take(MAX_LINE))
        .read_line(&mut line)
        .ok()?;
    let line = line.trim();
    match line.parse() {
        Ok(command) => Some(Request { command, stream }),
        Err(UnknownCommand) => {
            let reply = Err(format!(
                "unknown command '{line}' (is the running omma older than this CLI?)"
            ));
            let _ = write!(&stream, "{}", encode(&reply));
            None
        }
    }
}

/// sends `command` to the running instance and waits for its reply. None when no instance
/// is running.
pub fn send(dir: &Path, command: Command) -> anyhow::Result<Option<Reply>> {
    let stream = match UnixStream::connect(dir.join(SOCKET)) {
        Ok(stream) => stream,
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
            return Ok(None);
        }
        Err(e) => return Err(e).context("talking to the running omma"),
    };
    exchange(&stream, command)
        .context("talking to the running omma")
        .map(Some)
}

fn exchange(mut stream: &UnixStream, command: Command) -> anyhow::Result<Reply> {
    stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
    writeln!(stream, "{command}")?;
    // the reply counts once the connection closes. after quit that's when the process exits.
    let mut response = String::new();
    if let Err(e) = stream.read_to_string(&mut response) {
        if !matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) {
            return Err(e.into());
        }
        let secs = REPLY_TIMEOUT.as_secs();
        if command == Command::Quit && decode(&response).is_some() {
            bail!("omma is quitting but still running after {secs} s");
        }
        bail!("no reply within {secs} s");
    }
    match decode(&response) {
        Some(reply) => Ok(reply),
        None if response.is_empty() => bail!("omma closed the connection without replying"),
        None => bail!("unexpected reply {response:?}"),
    }
}

/// `ok <text>` or `err <text>`, then a newline. the text may span lines.
fn encode(reply: &Reply) -> String {
    match reply {
        Ok(text) => format!("ok {text}\n"),
        Err(text) => format!("err {text}\n"),
    }
}

/// None unless `response` is one reply ending in a newline
fn decode(response: &str) -> Option<Reply> {
    let (status, text) = response.strip_suffix('\n')?.split_once(' ')?;
    match status {
        "ok" => Some(Ok(text.into())),
        "err" => Some(Err(text.into())),
        _ => None,
    }
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
        assert!(
            send(&scratch.0, Command::Status).unwrap().is_none(),
            "no directory"
        );
        fs::create_dir(&scratch.0).unwrap();
        drop(UnixListener::bind(scratch.0.join(SOCKET)).unwrap());
        assert!(
            send(&scratch.0, Command::Status).unwrap().is_none(),
            "a stale socket"
        );
    }

    /// sends `command` to the server in `dir` from another thread, answers it with `reply`,
    /// and returns what the client got
    fn round_trip(dir: &Path, server: &Server, command: Command, reply: Reply) -> Option<Reply> {
        let client = {
            let dir = dir.to_path_buf();
            thread::spawn(move || send(&dir, command).unwrap())
        };
        let request = server.rx.recv_timeout(REPLY_TIMEOUT).unwrap();
        assert_eq!(request.command, command);
        request.reply(&reply);
        drop(request);
        client.join().unwrap()
    }

    #[test]
    fn the_client_gets_the_reply() {
        let scratch = Scratch::new();
        let server = bind(&scratch.0).unwrap().unwrap();
        let ok = Ok("Next break in 5m - Look away".to_string());
        assert_eq!(
            round_trip(&scratch.0, &server, Command::Status, ok.clone()),
            Some(ok)
        );
        let err = Err("omma is disabled".to_string());
        assert_eq!(
            round_trip(&scratch.0, &server, Command::TakeBreak, err.clone()),
            Some(err)
        );
    }

    #[test]
    fn silent_clients_do_not_hold_up_others() {
        let scratch = Scratch::new();
        let server = bind(&scratch.0).unwrap().unwrap();
        let socket = scratch.0.join(SOCKET);
        // read one after another, these would outlast the client's timeout
        let _silent: Vec<_> = (0..4)
            .map(|_| UnixStream::connect(&socket).unwrap())
            .collect();
        let ok = Ok("fine".to_string());
        assert_eq!(
            round_trip(&scratch.0, &server, Command::Status, ok.clone()),
            Some(ok)
        );
    }

    #[test]
    fn a_slow_client_does_not_hold_up_others() {
        let scratch = Scratch::new();
        let server = bind(&scratch.0).unwrap().unwrap();
        let mut slow = UnixStream::connect(scratch.0.join(SOCKET)).unwrap();
        // a byte every 100 ms keeps each of the server's reads short of its timeout
        let _trickle = thread::spawn(move || {
            for _ in 0..50 {
                if slow.write_all(b"s").is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
        });
        let ok = Ok("fine".to_string());
        assert_eq!(
            round_trip(&scratch.0, &server, Command::Status, ok.clone()),
            Some(ok)
        );
    }

    #[test]
    fn a_long_line_is_cut_off() {
        let scratch = Scratch::new();
        let _server = bind(&scratch.0).unwrap().unwrap();
        let mut stream = UnixStream::connect(scratch.0.join(SOCKET)).unwrap();
        stream.set_read_timeout(Some(REPLY_TIMEOUT)).unwrap();
        let _ = stream.write_all(&[b's'; 4096]);
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        let expected = format!(
            "unknown command '{}' (is the running omma older than this CLI?)",
            "s".repeat(64)
        );
        assert_eq!(decode(&response), Some(Err(expected)));
    }

    #[test]
    fn the_client_waits_for_the_connection_to_close() {
        let scratch = Scratch::new();
        let server = bind(&scratch.0).unwrap().unwrap();
        let dir = scratch.0.clone();
        let client = thread::spawn(move || send(&dir, Command::Quit).unwrap());
        let request = server.rx.recv_timeout(REPLY_TIMEOUT).unwrap();
        request.reply(&Ok("quitting".into()));
        thread::sleep(Duration::from_millis(300));
        assert!(!client.is_finished());
        drop(request);
        assert_eq!(client.join().unwrap(), Some(Ok("quitting".into())));
    }

    #[test]
    fn replies_survive_the_wire() {
        for reply in [
            Ok(String::new()),
            Ok("enabled".into()),
            Ok("Disabled\nconfig.toml: line 3: oops; kept the last good config".into()),
            Err("a break is already running".into()),
        ] {
            assert_eq!(decode(&encode(&reply)), Some(reply));
        }
        assert_eq!(decode("ok\n"), None, "no separator");
        assert_eq!(decode("ok fine"), None, "cut short");
        assert_eq!(decode("Next break in 5m\n"), None);
    }

    #[test]
    fn an_unknown_command_is_refused() {
        let scratch = Scratch::new();
        let _server = bind(&scratch.0).unwrap().unwrap();
        let mut stream = UnixStream::connect(scratch.0.join(SOCKET)).unwrap();
        stream.set_read_timeout(Some(REPLY_TIMEOUT)).unwrap();
        writeln!(stream, "snooze").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert_eq!(
            decode(&response),
            Some(Err(
                "unknown command 'snooze' (is the running omma older than this CLI?)".into()
            ))
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
