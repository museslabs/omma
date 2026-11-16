//! the Wayland desktop as `App` sees it: the tray, the warning and the break screen, and the
//! system services behind the rules. it is also the state that the event queue dispatches
//! Wayland events to.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, SystemTime};

use anyhow::{Context, bail};
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::{Connection, EventQueue, QueueHandle};

use super::anim::Clock;
use super::break_screen::BreakScreen;
use super::gl::Gl;
use super::idle::Idle;
use super::input::{Event, Input};
use super::picture::Pictures;
use super::portal::{self, Appearance, Binding, Shortcuts};
use super::sound;
use super::text::Fonts;
use super::toplevels::Toplevels;
use super::tray::Tray;
use super::warning::{HandOff, Warning};
use super::wl::Shell;
use crate::app::dnd::Reason;
use crate::app::{
    self, Action, Built, Button, Buttons, MenuLine, SLOW_TICK, TrayIcon, Ui, WarningKey,
};
use crate::config::{Config, DoNotDisturb, KeySpec};
use crate::queue::Break;

/// what other threads tell the event loop
pub enum Message {
    Act(Action),
    OpenConfig,
    OpenHelp,
    Appearance(Appearance),
    Shortcut(WarningKey),
    ShortcutsBound(Result<portal::Answer, String>),
    /// the triggers that the user gave the shortcuts in the desktop's settings
    ShortcutsChanged(Vec<(WarningKey, String)>),
}

/// sends a message to the event loop, and wakes it
#[derive(Clone)]
pub struct Waker {
    tx: Sender<Message>,
    pipe: Arc<UnixStream>,
}

impl Waker {
    pub fn send(&self, message: Message) {
        let _ = self.tx.send(message);
        // a full pipe has a wake-up in it already
        let _ = (&*self.pipe).write(&[1]);
    }
}

/// how the warning and break screen look, from the desktop's settings when each shows
#[derive(Clone, Copy, Debug)]
pub struct Look {
    pub dark: bool,
    /// solid surfaces in place of glass, for high contrast
    pub solid: bool,
    pub animate: bool,
    /// the compositor blurs what is behind the glass
    pub blurred: bool,
}

pub struct Desktop {
    conn: Connection,
    pub qh: QueueHandle<Desktop>,
    pub shell: Shell,
    pub input: Input,
    pub idle: Idle,
    pub toplevels: Toplevels,
    gl: Gl,
    fonts: Option<Fonts>,
    fonts_loading: Option<Receiver<Fonts>>,
    pub clock: Clock,
    appearance: Appearance,
    warning: Option<Warning>,
    leaving_warnings: Vec<Warning>,
    breaks: Option<BreakScreen>,
    leaving_breaks: Vec<BreakScreen>,
    /// the warning's bead as it hid, for the break screen showing next to fly in from
    hand_off: Option<HandOff>,
    /// the display the warning last showed on
    home: Option<WlOutput>,
    pictures: Pictures,
    tray: Tray,
    pub shortcuts: Shortcuts,
    /// what the user did with the pointer and keyboard, for `App` to act on
    pub actions: Vec<Action>,
    pub quitting: bool,
    pub tick: Duration,
}

impl Desktop {
    /// connects to the compositor and starts the tray and the portal threads. the receiver
    /// gets the threads' messages, and the stream is readable whenever one is waiting
    pub fn connect(
        conn: Connection,
    ) -> anyhow::Result<(Desktop, EventQueue<Desktop>, Receiver<Message>, UnixStream)> {
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());

        let (tx, rx) = mpsc::channel();
        let (wake_read, wake_write) = UnixStream::pair().context("make a wake-up pipe")?;
        wake_read.set_nonblocking(true)?;
        wake_write.set_nonblocking(true)?;
        let waker = Waker {
            tx,
            pipe: Arc::new(wake_write),
        };

        // the system's fonts take a moment to load, so they load while omma waits for its
        // first break
        let (fonts_tx, fonts_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("omma-fonts".into())
            .spawn(move || {
                let _ = fonts_tx.send(Fonts::new());
            })
            .context("start loading fonts")?;

        let gl = Gl::new(&conn).context("start OpenGL ES 3")?;
        let appearance = Appearance::default();
        let mut desktop = Desktop {
            conn: conn.clone(),
            qh,
            shell: Shell::default(),
            input: Input::default(),
            idle: Idle::default(),
            toplevels: Toplevels::default(),
            gl,
            fonts: None,
            fonts_loading: Some(fonts_rx),
            clock: Clock::new(),
            appearance,
            warning: None,
            leaving_warnings: Vec::new(),
            breaks: None,
            leaving_breaks: Vec::new(),
            hand_off: None,
            home: None,
            pictures: Pictures::default(),
            tray: Tray::start(waker.clone(), appearance.dark),
            shortcuts: Shortcuts::new(waker.clone()),
            actions: Vec::new(),
            quitting: false,
            tick: SLOW_TICK,
        };
        // the globals, then what each says about itself
        queue
            .roundtrip(&mut desktop)
            .context("list the compositor's globals")?;
        queue
            .roundtrip(&mut desktop)
            .context("read the outputs and the seat")?;
        if desktop.shell.layer_shell.is_none() || desktop.shell.compositor.is_none() {
            bail!(super::NO_LAYER_SHELL);
        }
        if !desktop.idle.available() {
            log!("smart pause can't work: the compositor has no ext-idle-notify protocol");
        }
        portal::watch_appearance(waker);
        Ok((desktop, queue, rx, wake_read))
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    fn load_fonts(&mut self) {
        if self.fonts.is_some() {
            return;
        }
        let loaded = self.fonts_loading.take().and_then(|rx| rx.recv().ok());
        self.fonts = Some(loaded.unwrap_or_else(Fonts::new));
    }

    fn look(&self) -> Look {
        Look {
            dark: self.appearance.dark,
            solid: self.appearance.contrast,
            animate: !self.appearance.reduce_motion,
            blurred: self.shell.can_blur(),
        }
    }

    pub fn set_appearance(&mut self, appearance: Appearance) {
        self.appearance = appearance;
        self.tray.set_dark(appearance.dark);
    }

    /// turns the pointer and keyboard events since the last call into actions
    pub fn route_input(&mut self) {
        let now = self.clock.now();
        let warning_surface = self.warning.as_ref().map(|w| w.surface);
        let on_warning = |id: u32| warning_surface == Some(id);
        for event in std::mem::take(&mut self.input.events) {
            let action = match event {
                Event::Motion { on, x, y } => {
                    if on_warning(on) {
                        self.warning
                            .as_mut()
                            .expect("checked")
                            .pointer_motion(x, y, now);
                    } else if let Some(breaks) = &mut self.breaks {
                        breaks.pointer_motion(on, x, y, now);
                    }
                    None
                }
                Event::Leave { on } => {
                    if on_warning(on) {
                        self.warning.as_mut().expect("checked").pointer_leave(now);
                    } else if let Some(breaks) = &mut self.breaks {
                        breaks.pointer_leave(on, now);
                    }
                    None
                }
                Event::Press { on, x, y } => {
                    if on_warning(on) {
                        self.warning.as_mut().expect("checked").press(x, y, now);
                    } else if let Some(breaks) = &mut self.breaks {
                        breaks.press(on, x, y);
                    }
                    None
                }
                Event::Release { on, x, y } => {
                    if on_warning(on) {
                        self.warning.as_mut().expect("checked").release(x, y, now)
                    } else {
                        self.breaks
                            .as_mut()
                            .and_then(|breaks| breaks.release(on, x, y))
                    }
                }
                Event::Key { key } => self.breaks.as_ref().and_then(|b| b.key(key)),
            };
            self.actions.extend(action);
        }
    }

    /// draws a frame on each surface that has something new to show and can draw
    pub fn render(&mut self) {
        let t = self.clock.now();
        self.retire(t);
        let Desktop {
            shell,
            gl,
            fonts,
            qh,
            warning,
            leaving_warnings,
            breaks,
            leaving_breaks,
            ..
        } = self;
        let Some(fonts) = fonts.as_mut() else {
            return;
        };
        for scene in warning.iter_mut().chain(leaving_warnings.iter_mut()) {
            let id = scene.surface;
            let Some(surface) = shell.get(id) else {
                continue;
            };
            if !surface.ready() || !(surface.dirty || scene.moving(t)) {
                continue;
            }
            scene.advance(t);
            scene.update_input(shell, qh);
            let surface = shell.get_mut(id).expect("checked above");
            if let Err(e) = surface.render(id, gl, qh, |gl| scene.draw(gl, fonts, t)) {
                log!("can't draw the warning: {e:#}");
            }
            scene.prune(t);
        }
        for screen in breaks.iter_mut().chain(leaving_breaks.iter_mut()) {
            let ids: Vec<u32> = screen.surfaces().collect();
            for id in ids {
                let Some(surface) = shell.get(id) else {
                    continue;
                };
                if !surface.ready()
                    || !(surface.dirty || screen.moving(id, t))
                    || !screen.may_draw(id, shell)
                {
                    continue;
                }
                let size = surface.size;
                let surface = shell.get_mut(id).expect("checked above");
                if let Err(e) = surface.render(id, gl, qh, |gl| screen.draw(id, size, gl, fonts, t))
                {
                    log!("can't draw the break screen: {e:#}");
                }
            }
            screen.prune(t);
        }
    }

    /// takes away what finished leaving, and brings the break screen in line with the
    /// displays
    fn retire(&mut self, t: f64) {
        let gl = &mut self.gl;
        let shell = &mut self.shell;
        for scene in self.leaving_warnings.extract_if(.., |w| w.gone(t)) {
            scene.destroy(gl, shell);
        }
        for screen in self.leaving_breaks.extract_if(.., |b| b.gone(t)) {
            screen.destroy(gl, shell);
        }
        // with every display gone, the break screen goes too, so that `break_visible` is
        // false. the next sync then shows a new screen once a display is back, and the
        // countdown keeps its deadline
        let empty = self.breaks.as_mut().is_some_and(|breaks| {
            breaks.reconcile(shell, &self.qh, &mut self.pictures, gl, t);
            breaks.is_empty()
        });
        if empty && let Some(screen) = self.breaks.take() {
            screen.destroy(gl, shell);
        }
        if self
            .warning
            .as_ref()
            .is_some_and(|w| shell.get(w.surface).is_none_or(|s| s.closed))
        {
            let scene = self.warning.take().expect("checked");
            scene.destroy(gl, shell);
        }
        let showing = self.warning.is_some()
            || self.breaks.is_some()
            || !self.leaving_warnings.is_empty()
            || !self.leaving_breaks.is_empty();
        if !showing {
            gl.trim();
        }
    }
}

impl Ui for Desktop {
    const WINDOW_ACCESS_PROBLEM: &'static str =
        "fullscreen, window_titles and apps_in_front need wlr-foreign-toplevel-management";

    fn idle_time(&mut self, now: SystemTime) -> Duration {
        self.idle.idle_time(now)
    }

    fn suppression(&mut self, dnd: &DoNotDisturb) -> Option<Reason> {
        self.toplevels.suppression(dnd)
    }

    fn needs_window_access(&self, dnd: &DoNotDisturb) -> bool {
        dnd.fullscreen || !dnd.window_titles.is_empty() || !dnd.apps_in_front.is_empty()
    }

    fn has_window_access(&mut self, dnd: &DoNotDisturb) -> bool {
        self.toplevels.has_access(dnd)
    }

    /// there is nothing to grant: the compositor offers the protocol or does not
    fn request_window_access(&mut self) {}

    fn play_sound(&mut self, sound: &str) {
        sound::play(sound);
    }

    fn animations_on(&self) -> bool {
        !self.appearance.reduce_motion
    }

    fn config_changed(&mut self, config: &Config) {
        self.idle.watch(config.schedule.idle_threshold, &self.qh);
        // an action the schedule forbids gets no shortcut, which the desktop would otherwise
        // reserve and can ask the user to approve
        self.shortcuts.bind(app::warning_shortcuts(config));
    }

    fn quit(&mut self) {
        self.quitting = true;
    }

    fn hold_shortcuts(&mut self, keys: &[(WarningKey, KeySpec)]) -> Vec<String> {
        self.shortcuts.hold();
        if keys.is_empty() {
            return Vec::new();
        }
        match &self.shortcuts.bound {
            Binding::Failed(why) => vec![why.clone()],
            _ => Vec::new(),
        }
    }

    fn held_shortcut(&self, key: WarningKey) -> Option<KeySpec> {
        self.shortcuts.held(key)
    }

    fn release_shortcuts(&mut self) {
        self.shortcuts.release();
    }

    fn warning_visible(&self) -> bool {
        self.warning.is_some()
    }

    fn show_warning(&mut self, remaining: u64, buttons: Buttons) {
        self.load_fonts();
        let now = self.clock.now();
        let look = self.look();
        // the triggers the desktop gave the keys, which can differ from the ones asked for
        let hint = |button: Option<Button>, key| {
            let trigger = self.shortcuts.trigger(key)?;
            button?.key.map(|_| trigger.to_owned())
        };
        let hints = [
            hint(buttons.skip, WarningKey::Skip),
            hint(buttons.postpone, WarningKey::Postpone),
        ];
        let fonts = self.fonts.as_mut().expect("loaded above");
        if let Some(old) = self.warning.take() {
            old.destroy(&self.gl, &mut self.shell);
        }
        self.warning = Warning::show(
            &mut self.shell,
            &self.qh,
            fonts,
            look,
            remaining,
            buttons,
            hints,
            now,
        );
    }

    fn update_warning(&mut self, remaining: u64) {
        if let Some(warning) = &mut self.warning {
            warning.update_countdown(remaining);
            if let Some(surface) = self.shell.get_mut(warning.surface) {
                surface.dirty = true;
            }
        }
    }

    fn hide_warning(&mut self, hand_off: bool) {
        let Some(mut warning) = self.warning.take() else {
            return;
        };
        let now = self.clock.now();
        let off = warning.hand_off(&self.shell);
        self.home = off.output.clone().or(self.home.take());
        self.hand_off = hand_off.then_some(off);
        warning.hide(hand_off, now);
        self.leaving_warnings.push(warning);
    }

    fn break_visible(&self) -> bool {
        self.breaks.is_some()
    }

    fn show_break(&mut self, brk: &Break, remaining: u64, buttons: Buttons) -> Option<Built> {
        self.load_fonts();
        let now = self.clock.now();
        let look = self.look();
        let hand_off = self.hand_off.take();
        let (screen, built) = BreakScreen::show(
            &mut self.shell,
            &self.qh,
            &mut self.pictures,
            look,
            brk,
            remaining,
            buttons,
            hand_off,
            self.home.clone(),
            now,
        )?;
        if let Some(old) = self.breaks.replace(screen) {
            old.destroy(&self.gl, &mut self.shell);
        }
        Some(built)
    }

    fn update_break(&mut self, remaining: u64) {
        let now = self.clock.now();
        if let Some(breaks) = &mut self.breaks {
            breaks.update_countdown(remaining, now);
            for id in breaks.surfaces().collect::<Vec<_>>() {
                if let Some(surface) = self.shell.get_mut(id) {
                    surface.dirty = true;
                }
            }
        }
    }

    fn hide_break(&mut self) {
        let Some(mut screen) = self.breaks.take() else {
            return;
        };
        screen.hide(&self.shell, self.clock.now());
        self.leaving_breaks.push(screen);
    }

    fn set_status(&mut self, text: &str) {
        self.tray.set_status(text);
    }

    fn set_suppressed(&mut self, line: Option<&MenuLine>) {
        self.tray.set_suppressed(line);
    }

    fn set_enabled(&mut self, on: bool) {
        self.tray.set_enabled(on);
    }

    fn set_config_error(&mut self, error: Option<(&str, &MenuLine)>) {
        self.tray.set_config_error(error);
    }

    fn set_problems(&mut self, lines: &[Option<MenuLine>; 3]) {
        self.tray.set_problems(lines);
    }

    fn set_tray_icon(&mut self, icon: TrayIcon) {
        self.tray.set_icon(icon);
    }

    fn set_fast_ticks(&mut self, fast: bool) {
        self.tick = if fast { app::FAST_TICK } else { SLOW_TICK };
    }
}

/// opens `target`, a file or a link, in the app the desktop has for it
pub fn open(target: &str) {
    let target = target.to_owned();
    let spawned = std::thread::Builder::new()
        .name("omma-open".into())
        .spawn(move || {
            let opened = Command::new("xdg-open")
                .arg(&target)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .status();
            if !opened.is_ok_and(|status| status.success()) {
                log!("can't open {target}: is xdg-open installed?");
            }
        });
    if let Err(e) = spawned {
        log!("can't open a file: {e}");
    }
}

/// opens the config file, created from the defaults first when it is gone
pub fn open_config(path: &Path) {
    if app::ensure_config_file(path) {
        open(&path.to_string_lossy());
    }
}
