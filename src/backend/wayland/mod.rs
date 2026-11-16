//! the Wayland backend: a tray icon through StatusNotifierItem, and the warning and break
//! screen on the layer shell, drawn with GL ES. one thread runs the event loop. D-Bus, the
//! CLI and font loading have threads of their own, which wake it with messages.

mod anim;
mod bead;
mod break_screen;
mod button;
mod desktop;
mod flip;
mod gl;
mod icons;
mod idle;
mod input;
mod picture;
mod portal;
mod sound;
mod text;
mod toplevels;
mod tray;
mod warning;
mod wl;

use std::io::{ErrorKind, Read};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, bail};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};

use super::Backend;
use crate::app::{Action, App, HELP_URL};
use crate::config::{Config, ConfigError};
use crate::ipc::Server;
use desktop::{Desktop, Message};

/// why omma cannot run when the compositor has no layer shell
const NO_LAYER_SHELL: &str = "needs a Wayland compositor with the layer-shell protocol. Hyprland, \
                              Sway, niri, river and KDE Plasma have it";

pub struct Wayland;

impl Backend for Wayland {
    /// the connection to the compositor. an inherited `WAYLAND_SOCKET` works only once, so
    /// `run` goes on with this connection rather than make another
    type System = Connection;

    /// a compositor with the layer shell, which every surface omma shows is on
    fn check_system() -> anyhow::Result<Connection> {
        let conn = Connection::connect_to_env()
            .context("can't reach a Wayland compositor: omma runs in a Wayland session")?;
        let (globals, _) = registry_queue_init::<Probe>(&conn)
            .context("can't list the Wayland compositor's globals")?;
        let has = |name: &str| {
            globals
                .contents()
                .with_list(|list| list.iter().any(|global| global.interface == name))
        };
        if !has("zwlr_layer_shell_v1") {
            bail!(NO_LAYER_SHELL);
        }
        Ok(conn)
    }

    fn run(
        conn: Connection,
        config: Config,
        config_path: PathBuf,
        config_error: Option<ConfigError>,
        ipc: Server,
    ) -> anyhow::Result<()> {
        let (desktop, mut queue, messages, wake) = Desktop::connect(conn)?;
        let now = SystemTime::now();
        let mut app = App::new(desktop, config, config_path, config_error, ipc, now);
        app.sync(now);
        run_loop(&mut app, &mut queue, &messages, &wake)
    }
}

/// draws what is due, waits for the compositor, a message or the next tick, and handles
/// whichever came, until quit
fn run_loop(
    app: &mut App<Desktop>,
    queue: &mut EventQueue<Desktop>,
    messages: &Receiver<Message>,
    wake: &UnixStream,
) -> anyhow::Result<()> {
    let mut next_tick = Instant::now();
    let mut tick = app.ui.tick;
    loop {
        if app.ui.quitting {
            return Ok(());
        }
        app.ui.render();
        app.ui
            .connection()
            .flush()
            .context("talk to the Wayland compositor")?;

        let timeout = next_tick.saturating_duration_since(Instant::now());
        if let Some(guard) = queue.prepare_read() {
            let (wayland, woken) = wait(guard.connection_fd().as_raw_fd(), wake, timeout);
            if wayland {
                if let Err(e) = guard.read() {
                    let blocked = matches!(&e, wayland_client::backend::WaylandError::Io(io) if io.kind() == ErrorKind::WouldBlock);
                    if !blocked {
                        return Err(e).context("read from the Wayland compositor");
                    }
                }
            } else {
                drop(guard);
            }
            if woken {
                drain(wake);
            }
        }
        queue
            .dispatch_pending(&mut app.ui)
            .context("handle the Wayland compositor's events")?;

        for message in messages.try_iter() {
            handle(app, message);
        }
        app.ui.route_input();
        for action in std::mem::take(&mut app.ui.actions) {
            app.act(SystemTime::now(), action);
        }

        let now = Instant::now();
        if app.ui.tick != tick {
            // a faster tick starts now, rather than after the slow one already waited for
            tick = app.ui.tick;
            next_tick = next_tick.min(now + tick);
        }
        if now >= next_tick {
            app.tick(SystemTime::now());
            next_tick = now + app.ui.tick;
            tick = app.ui.tick;
        }
    }
}

fn handle(app: &mut App<Desktop>, message: Message) {
    match message {
        Message::Act(action) => app.act(SystemTime::now(), action),
        Message::OpenConfig => desktop::open_config(app.config_path()),
        Message::OpenHelp => desktop::open(HELP_URL),
        Message::Appearance(appearance) => app.ui.set_appearance(appearance),
        Message::Shortcut(key) => {
            if app.ui.shortcuts.holding() {
                app.act(SystemTime::now(), Action::Shortcut(key));
            }
        }
        Message::ShortcutsBound(result) => {
            if let Err(why) = &result {
                log!("{why}");
            }
            app.ui.shortcuts.answered(result);
        }
        Message::ShortcutsChanged(triggers) => app.ui.shortcuts.triggers_changed(triggers),
    }
}

/// waits up to `timeout` for the compositor or a message: whether each has something
fn wait(wayland: i32, wake: &UnixStream, timeout: Duration) -> (bool, bool) {
    let mut fds = [
        libc::pollfd {
            fd: wayland,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake.as_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let ms = i32::try_from(timeout.as_millis().saturating_add(1)).unwrap_or(i32::MAX);
    // SAFETY: two valid pollfds, which poll only writes the results into
    let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
    if ready <= 0 {
        return (false, false);
    }
    let readable =
        |fd: &libc::pollfd| fd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0;
    (readable(&fds[0]), readable(&fds[1]))
}

/// empties the wake-up pipe, whose messages are in the channel
fn drain(mut wake: &UnixStream) {
    let mut buffer = [0u8; 64];
    while matches!(wake.read(&mut buffer), Ok(n) if n > 0) {}
}

/// just enough state to list the globals at startup
struct Probe;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Probe {
    fn event(
        _: &mut Probe,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Probe>,
    ) {
    }
}
