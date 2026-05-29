//! The macOS backend: status-bar UI, timer, break overlays, warning dialog,
//! sounds, idle detection and do-not-disturb checks.
//!
//! Everything here runs on the main thread. The `Delegate` object is the
//! Objective-C target for menu items, buttons and the timer; it owns the whole
//! [`App`] state in a `RefCell` ivar.

mod activity;
mod break_screen;
mod idle;
mod pre_break_dialog;
mod sound;

use std::cell::RefCell;
use std::time::SystemTime;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, Sel};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuItem, NSStatusBar,
    NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{MainThreadMarker, NSData, NSSize, NSString, NSTimer};

use super::{Backend, BreakOverlay, PreBreakNotice};

use crate::config::{Config, DndAction};
use crate::ipc::{self, Command, Server};
use crate::model::BreakType;
use crate::scheduler::{Core, CoreEvent, State};
use activity::DndChecker;
use break_screen::BreakScreen;
use pre_break_dialog::PreBreakDialog;

/// The menu-bar glyph, embedded so it travels with the binary whether omma is
/// launched from its `.app` bundle or run bare. Rendered as a template image,
/// so only the alpha channel matters and macOS tints it to match the menu bar.
const TRAY_ICON_PNG: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tray-icon.png"));

/// Side length, in points, of the menu-bar glyph. macOS scales the source art
/// down to this and renders it at the screen's backing scale.
const TRAY_ICON_SIZE: f64 = 18.0;
/// Visual countdowns need a sub-second heartbeat because a one-second NSTimer
/// can fire just before or after the scheduler's exact deadline, making digits
/// visibly linger or jump. The scheduler remains deadline-based, so this only
/// improves UI refresh latency.
const TICK_INTERVAL_SECS: f64 = 0.2;

/// Decode the embedded PNG into a template [`NSImage`] sized for the menu bar.
/// Returns `None` if the bytes ever fail to decode, letting the caller fall
/// back to a text glyph.
fn tray_icon_image() -> Option<Retained<NSImage>> {
    let data = NSData::with_bytes(TRAY_ICON_PNG);
    let image = NSImage::initWithData(NSImage::alloc(), &data)?;
    // Template images are drawn from alpha alone, so the glyph follows the menu
    // bar's appearance (dark in light mode, light in dark mode) automatically.
    image.setTemplate(true);
    image.setSize(NSSize {
        width: TRAY_ICON_SIZE,
        height: TRAY_ICON_SIZE,
    });
    Some(image)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MacosBackend;

impl MacosBackend {
    pub fn run(self, config: Config, server: Server) -> anyhow::Result<()> {
        run(config, server)
    }
}

/// Build the status item, schedule the timer and run the app.
fn run(config: Config, server: Server) -> anyhow::Result<()> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| anyhow::anyhow!("omma must run on the main thread"))?;

    let queue = config
        .build_queue()
        .ok_or_else(|| anyhow::anyhow!("no breaks configured"))?;

    let postpone_secs = config.postpone_duration.max(0);
    let mut core = Core::new(
        queue,
        config.pre_break_warning,
        postpone_secs,
        config.allow_postpone,
        config.strict_break,
    );
    core.start(SystemTime::now());

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let dnd = DndChecker::from_config(&config.do_not_disturb);
    let smart_pause_enabled = config.smart_pause.enabled;
    let idle_threshold = config.smart_pause.idle_seconds.max(1) as f64;
    let config_mtime = file_mtime(&crate::config::config_path());

    let state = App {
        core,
        config,
        server,
        mtm,
        status_item: None,
        enable_item: None,
        status_item_label: None,
        break_screen: BreakScreen::new(mtm),
        pre_break_dialog: PreBreakDialog::new(mtm),
        dnd,
        smart_pause_enabled,
        idle_threshold,
        config_mtime,
    };

    let delegate = Delegate::new(mtm, state);

    // Build the menu bar item, with the delegate as the action target.
    delegate.with_app(|app| app.build_status_item(&delegate));

    // Drive the scheduler frequently enough for second-boundary countdowns to
    // change close to their actual deadlines. NSTimer retains its target, so
    // the delegate (and thus all state) stays alive for the run loop's lifetime.
    let target: &AnyObject = &delegate;
    let _timer: Retained<NSTimer> = unsafe {
        NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
            TICK_INTERVAL_SECS,
            target,
            sel!(onTimer:),
            None,
            true,
        )
    };
    _timer.setTolerance(0.05);

    app.run();
    Ok(())
}

/// All app state. Mutated only from the main thread, through the delegate.
struct App {
    core: Core,
    config: Config,
    server: Server,
    mtm: MainThreadMarker,
    status_item: Option<Retained<NSStatusItem>>,
    enable_item: Option<Retained<NSMenuItem>>,
    status_item_label: Option<Retained<NSMenuItem>>,
    break_screen: BreakScreen,
    pre_break_dialog: PreBreakDialog,
    dnd: DndChecker,
    smart_pause_enabled: bool,
    idle_threshold: f64,
    config_mtime: Option<SystemTime>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OmmaDelegate"]
    #[ivars = RefCell<App>]
    struct Delegate;

    impl Delegate {
        #[unsafe(method(onTimer:))]
        fn on_timer(&self, _timer: *mut AnyObject) {
            let now = SystemTime::now();
            self.with_app(|app| app.on_tick(now, self));
        }

        #[unsafe(method(takeBreak:))]
        fn take_break(&self, _sender: *mut AnyObject) {
            self.with_app(|app| app.core.take_break(None));
        }

        #[unsafe(method(takeShortBreak:))]
        fn take_short_break(&self, _sender: *mut AnyObject) {
            self.with_app(|app| app.core.take_break(Some(BreakType::Short)));
        }

        #[unsafe(method(takeLongBreak:))]
        fn take_long_break(&self, _sender: *mut AnyObject) {
            self.with_app(|app| app.core.take_break(Some(BreakType::Long)));
        }

        #[unsafe(method(toggleEnabled:))]
        fn toggle_enabled(&self, _sender: *mut AnyObject) {
            self.with_app(|app| {
                if app.core.is_enabled() {
                    app.hide_pre_break();
                    app.hide_break();
                    app.core.disable();
                } else {
                    let _ = app.core.enable(SystemTime::now());
                }
                app.refresh_menu_state();
            });
        }

        #[unsafe(method(openConfig:))]
        fn open_config(&self, _sender: *mut AnyObject) {
            self.with_app(|app| app.open_config_file());
        }

        #[unsafe(method(skipBreak:))]
        fn skip_break(&self, _sender: *mut AnyObject) {
            self.with_app(|app| {
                app.hide_pre_break();
                app.hide_break();
                app.core.skip();
            });
        }

        #[unsafe(method(postponeBreak:))]
        fn postpone_break(&self, _sender: *mut AnyObject) {
            self.with_app(|app| {
                app.hide_pre_break();
                app.hide_break();
                app.core.postpone();
            });
        }

        #[unsafe(method(skipWarning:))]
        fn skip_warning(&self, _sender: *mut AnyObject) {
            let now = SystemTime::now();
            self.with_app(|app| {
                app.hide_pre_break();
                let events = app.core.suppress_break(now, true);
                for ev in events {
                    app.handle_event(now, ev, self);
                }
                app.refresh_status_label(now);
                app.publish_status(now);
            });
        }

        #[unsafe(method(dismissWarning:))]
        fn dismiss_warning(&self, _sender: *mut AnyObject) {
            self.with_app(|app| app.hide_pre_break());
        }

        #[unsafe(method(postponeWarning:))]
        fn postpone_warning(&self, _sender: *mut AnyObject) {
            let now = SystemTime::now();
            self.with_app(|app| {
                app.hide_pre_break();
                let events = app.core.suppress_break(now, false);
                for ev in events {
                    app.handle_event(now, ev, self);
                }
                app.refresh_status_label(now);
                app.publish_status(now);
            });
        }

        #[unsafe(method(quitApp:))]
        fn quit_app(&self, _sender: *mut AnyObject) {
            self.with_app(|app| app.quit());
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker, app: App) -> Retained<Delegate> {
        let this = Delegate::alloc(mtm).set_ivars(RefCell::new(app));
        unsafe { msg_send![super(this), init] }
    }

    fn with_app<R>(&self, f: impl FnOnce(&mut App) -> R) -> R {
        f(&mut self.ivars().borrow_mut())
    }
}

impl App {
    /// Called by the main run-loop timer.
    fn on_tick(&mut self, now: SystemTime, delegate: &Delegate) {
        // Handle any CLI commands that came in over the socket.
        for cmd in self.server.drain() {
            self.handle_command(cmd);
        }

        self.maybe_reload_config(now);
        self.apply_smart_pause(now, delegate);

        let events = self.core.tick(now);
        for ev in events {
            self.handle_event(now, ev, delegate);
        }
        if matches!(self.core.state(), State::PreBreak) {
            if let Some(remaining) = self.core.seconds_until_break_start(now) {
                self.update_pre_break_countdown(remaining);
            }
        }

        self.refresh_status_label(now);
        self.publish_status(now);
    }

    /// Re-read the config file when it changes on disk and apply it live.
    fn maybe_reload_config(&mut self, now: SystemTime) {
        // Don't swap the schedule out from under an in-progress break; retry
        // on a later tick.
        if matches!(self.core.state(), State::PreBreak | State::Break) {
            return;
        }

        let path = crate::config::config_path();
        let mtime = file_mtime(&path);
        if mtime == self.config_mtime {
            return;
        }
        self.config_mtime = mtime;

        // Parse directly so a transient/partial write doesn't clobber the
        // running config with defaults — only apply a clean parse.
        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(_) => return,
        };
        let new: Config = match toml::from_str(&data) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("omma: config not reloaded (parse error): {e}");
                return;
            }
        };

        self.apply_config(new, now);
        eprintln!("omma: reloaded config");
    }

    /// Rebuild the schedule and feature state from a freshly read config.
    fn apply_config(&mut self, new: Config, now: SystemTime) {
        let Some(queue) = new.build_queue() else {
            eprintln!("omma: config has no breaks; keeping previous schedule");
            return;
        };

        let was_enabled = self.core.is_enabled();
        let postpone_secs = new.postpone_duration.max(0);
        let mut core = Core::new(
            queue,
            new.pre_break_warning,
            postpone_secs,
            new.allow_postpone,
            new.strict_break,
        );
        if was_enabled {
            let _ = core.start(now);
        } else {
            core.disable();
        }
        self.core = core;

        self.dnd = DndChecker::from_config(&new.do_not_disturb);
        if !new.notification.enabled {
            self.hide_pre_break();
        }
        self.smart_pause_enabled = new.smart_pause.enabled;
        self.idle_threshold = new.smart_pause.idle_seconds.max(1) as f64;
        self.config = new;
        self.refresh_menu_state();
    }

    /// Pause the schedule while the user is idle; resume when they return.
    fn apply_smart_pause(&mut self, now: SystemTime, delegate: &Delegate) {
        if !self.smart_pause_enabled || !self.core.is_enabled() {
            return;
        }
        let idle = self.idle_seconds();
        if idle >= self.idle_threshold {
            if matches!(self.core.state(), State::PreBreak) {
                self.hide_pre_break();
            }
            self.core.pause_for_idle(now);
        } else if matches!(self.core.state(), State::Resting) {
            for ev in self.core.resume_from_idle(now) {
                self.handle_event(now, ev, delegate);
            }
        }
    }

    fn handle_event(&mut self, now: SystemTime, ev: CoreEvent, delegate: &Delegate) {
        match ev {
            CoreEvent::NextBreakScheduled { .. } => {}
            CoreEvent::PreBreak { name, .. } => {
                // Do-not-disturb: skip/postpone instead of interrupting.
                if self.should_suppress_break() {
                    let skip = self.config.do_not_disturb.action == DndAction::Skip;
                    let _ = self.core.suppress_break(now, skip);
                    self.hide_pre_break();
                    return;
                }
                if self.config.notification.enabled {
                    let target: &AnyObject = delegate;
                    let skip_key = self.config.keybindings.skip.clone();
                    let postpone_key = self.config.keybindings.postpone.clone();
                    let allow_skip = !self.config.strict_break;
                    let allow_postpone = self.config.allow_postpone && !self.config.strict_break;
                    let seconds_until_break = self
                        .core
                        .seconds_until_break_start(now)
                        .unwrap_or(self.config.pre_break_warning);
                    self.show_pre_break(
                        PreBreakNotice {
                            name: &name,
                            allow_skip,
                            allow_postpone,
                            seconds_until_break,
                            skip_key: &skip_key,
                            postpone_key: &postpone_key,
                        },
                        target,
                    );
                }
                let alert = &self.config.audible_alert;
                if alert.enabled && alert.pre_break {
                    self.play_sound(&alert.pre_break_sound);
                }
            }
            CoreEvent::StartBreak {
                name,
                duration,
                allow_skip,
                allow_postpone,
                ..
            } => {
                self.hide_pre_break_for_break();
                let target: &AnyObject = delegate;
                let skip_key = self.config.keybindings.skip.clone();
                let postpone_key = self.config.keybindings.postpone.clone();
                self.show_break(
                    BreakOverlay {
                        name: &name,
                        duration,
                        allow_skip,
                        allow_postpone,
                        skip_key: &skip_key,
                        postpone_key: &postpone_key,
                    },
                    target,
                );
            }
            CoreEvent::Countdown { remaining, total } => {
                self.update_break_countdown(remaining, total);
            }
            CoreEvent::StopBreak => {
                self.hide_pre_break();
                self.hide_break();
                let alert = &self.config.audible_alert;
                if alert.enabled && alert.post_break {
                    self.play_sound(&alert.post_break_sound);
                }
            }
        }
    }

    fn handle_command(&mut self, cmd: Command) {
        match cmd {
            Command::TakeBreak => self.core.take_break(None),
            Command::TakeShortBreak => self.core.take_break(Some(BreakType::Short)),
            Command::TakeLongBreak => self.core.take_break(Some(BreakType::Long)),
            Command::Enable => {
                let _ = self.core.enable(SystemTime::now());
                self.refresh_menu_state();
            }
            Command::Disable => {
                self.hide_pre_break();
                self.hide_break();
                self.core.disable();
                self.refresh_menu_state();
            }
            Command::Quit => self.quit(),
            Command::Status => {} // handled by the IPC thread from the snapshot
            // Invalidate the cached mtime so the reload runs this same tick.
            Command::Reload => self.config_mtime = None,
        }
    }

    fn quit(&mut self) {
        self.hide_pre_break();
        self.hide_break();
        let _ = std::fs::remove_file(ipc::socket_path());
        let app = NSApplication::sharedApplication(self.mtm);
        app.terminate(None);
    }

    // --- menu bar ---------------------------------------------------------

    fn build_status_item(&mut self, delegate: &Delegate) {
        let status_bar = NSStatusBar::systemStatusBar();
        let item = status_bar.statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = item.button(self.mtm) {
            match tray_icon_image() {
                Some(image) => button.setImage(Some(&image)),
                None => button.setTitle(&NSString::from_str("\u{1F441}")), // 👁 fallback
            }
        }

        let menu = NSMenu::new(self.mtm);
        let target: &AnyObject = delegate;

        self.status_item_label = Some(self.add_disabled_item(&menu, "Starting…"));
        menu.addItem(&NSMenuItem::separatorItem(self.mtm));
        self.add_item(&menu, "Take a break now", sel!(takeBreak:), target);
        self.add_item(&menu, "Take a short break", sel!(takeShortBreak:), target);
        self.add_item(&menu, "Take a long break", sel!(takeLongBreak:), target);
        menu.addItem(&NSMenuItem::separatorItem(self.mtm));
        self.enable_item = Some(self.add_item(&menu, "Enabled", sel!(toggleEnabled:), target));
        menu.addItem(&NSMenuItem::separatorItem(self.mtm));
        self.add_item(&menu, "Open config…", sel!(openConfig:), target);
        self.add_item(&menu, "Quit omma", sel!(quitApp:), target);

        item.setMenu(Some(&menu));
        self.status_item = Some(item);
        self.refresh_menu_state();
    }

    fn add_item(
        &self,
        menu: &NSMenu,
        title: &str,
        action: Sel,
        target: &AnyObject,
    ) -> Retained<NSMenuItem> {
        let title = NSString::from_str(title);
        let key = NSString::from_str("");
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm),
                &title,
                Some(action),
                &key,
            )
        };
        unsafe { item.setTarget(Some(target)) };
        menu.addItem(&item);
        item
    }

    fn add_disabled_item(&self, menu: &NSMenu, title: &str) -> Retained<NSMenuItem> {
        let title = NSString::from_str(title);
        let key = NSString::from_str("");
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm),
                &title,
                None,
                &key,
            )
        };
        item.setEnabled(false);
        menu.addItem(&item);
        item
    }

    fn refresh_menu_state(&self) {
        if let Some(item) = &self.enable_item {
            let enabled = self.core.is_enabled();
            item.setTitle(&NSString::from_str(if enabled {
                "Enabled"
            } else {
                "Disabled"
            }));
            // 1 = NSControlStateValueOn, 0 = off.
            item.setState(if enabled { 1 } else { 0 });
        }
    }

    fn refresh_status_label(&self, now: SystemTime) {
        let text = self.status_text(now);
        if let Some(item) = &self.status_item_label {
            item.setTitle(&NSString::from_str(&text));
        }
    }

    fn publish_status(&self, now: SystemTime) {
        self.server.set_status(self.status_text(now));
    }

    fn status_text(&self, now: SystemTime) -> String {
        if !self.core.is_enabled() {
            return "Disabled".to_string();
        }
        match self.core.state() {
            State::Break | State::PreBreak => "Break in progress".to_string(),
            State::Resting => "Paused (idle)".to_string(),
            _ => match self.core.seconds_until_next(now) {
                Some(s) if s > 0 => format!(
                    "Next break in {} — {}",
                    fmt_duration(s),
                    self.core.current_break_name()
                ),
                _ => "Next break shortly".to_string(),
            },
        }
    }
}

impl Backend for App {
    type ActionTarget = AnyObject;

    fn show_pre_break(&mut self, notice: PreBreakNotice<'_>, target: &Self::ActionTarget) {
        self.pre_break_dialog.show(
            notice.name,
            notice.allow_skip,
            notice.allow_postpone,
            notice.seconds_until_break,
            notice.skip_key,
            notice.postpone_key,
            target,
        );
    }

    fn update_pre_break_countdown(&mut self, seconds_until_break: i64) {
        self.pre_break_dialog.update_countdown(seconds_until_break);
    }

    fn hide_pre_break(&mut self) {
        self.pre_break_dialog.hide();
    }

    fn hide_pre_break_for_break(&mut self) {
        self.pre_break_dialog.hide_for_break();
    }

    fn show_break(&mut self, overlay: BreakOverlay<'_>, target: &Self::ActionTarget) {
        self.break_screen.show(
            overlay.name,
            overlay.duration,
            overlay.allow_skip,
            overlay.allow_postpone,
            overlay.skip_key,
            overlay.postpone_key,
            target,
        );
    }

    fn update_break_countdown(&mut self, remaining: i64, total: i64) {
        self.break_screen.update_countdown(remaining, total);
    }

    fn hide_break(&mut self) {
        self.break_screen.hide();
    }

    fn play_sound(&self, name: &str) {
        sound::play(name);
    }

    fn idle_seconds(&self) -> f64 {
        idle::idle_seconds()
    }

    fn should_suppress_break(&self) -> bool {
        self.dnd.should_suppress()
    }

    fn open_config_file(&self) {
        let path = crate::config::config_path();
        let _ = std::process::Command::new("open").arg(path).status();
    }
}

fn file_mtime(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn fmt_duration(secs: i64) -> String {
    let secs = secs.max(0);
    let m = secs / 60;
    let s = secs % 60;
    if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}
