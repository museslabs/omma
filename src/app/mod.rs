//! all main-thread. Delegate is the obj-c target for menu items, buttons and the timer,
//! and owns the App state in a RefCell ivar.

mod activity;
mod break_screen;
mod glass_button;
mod idle;
mod pre_break_dialog;
mod sound;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, Sel};
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSControlStateValueOff, NSControlStateValueOn,
    NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{
    MainThreadMarker, NSData, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSTimer,
};

use crate::config::Config;
use crate::ipc::{self, Command, Server};
use crate::queue::BreakType;
use crate::scheduler::{Core, Event, Phase};
use activity::DndChecker;
use break_screen::BreakScreen;
use pre_break_dialog::PreBreakDialog;

/// embedded so it travels with the binary whether launched from .app or bare
const TRAY_ICON_SVG: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tray-icon.svg"));
const TRAY_ICON_INCOMING_SVG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tray-icon-incoming.svg"
));

const TRAY_ICON_SIZE: f64 = 18.0;
/// the tray icon blinks, and the timer ticks fast, this many seconds before the warning
const INCOMING_WINDOW_SECS: u64 = 60;
/// while something counts down on screen. sub-second so a tick landing near a deadline
/// doesn't make digits linger or jump; the core is deadline-based either way.
const FAST_TICK_SECS: f64 = 0.2;
const SLOW_TICK_SECS: f64 = 1.0;
/// the config file is checked for changes at most this often
const CONFIG_POLL: Duration = Duration::from_secs(1);

/// None if the bytes fail to decode, caller falls back to a text glyph
fn tray_icon_image(bytes: &[u8]) -> Option<Retained<NSImage>> {
    let data = NSData::with_bytes(bytes);
    let image = NSImage::initWithData(NSImage::alloc(), &data)?;
    // template images draw from alpha alone, so the glyph auto-tints with the menu bar
    image.setTemplate(true);
    image.setSize(NSSize {
        width: TRAY_ICON_SIZE,
        height: TRAY_ICON_SIZE,
    });
    Some(image)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum TrayIconState {
    #[default]
    Normal,
    Incoming,
}

pub fn run(config: Config, config_path: PathBuf, ipc: Server) -> anyhow::Result<()> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| anyhow::anyhow!("omma must run on the main thread"))?;

    let ns_app = NSApplication::sharedApplication(mtm);
    ns_app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let now = SystemTime::now();
    let app = App::new(mtm, config, config_path, ipc, now);
    let delegate = Delegate::new(mtm, app);
    let target: &AnyObject = &delegate;
    delegate.with_app(|app| {
        app.build_status_item(&delegate);
        app.sync(now, target);
    });

    ns_app.run();
    Ok(())
}

/// mutated only from the main thread, via the delegate
struct App {
    mtm: MainThreadMarker,
    core: Core,
    config: Config,
    config_path: PathBuf,
    config_mtime: Option<SystemTime>,
    config_checked: Option<SystemTime>,
    ipc: Server,
    status_item: Option<Retained<NSStatusItem>>,
    tray_icon: Option<Retained<NSImage>>,
    tray_icon_incoming: Option<Retained<NSImage>>,
    enable_item: Option<Retained<NSMenuItem>>,
    status_item_label: Option<Retained<NSMenuItem>>,
    break_screen: BreakScreen,
    pre_break_dialog: PreBreakDialog,
    dnd: DndChecker,
    /// the user closed the warning without skipping or postponing; cleared when it ends
    warning_dismissed: bool,
    shown: Shown,
    /// set up by the first `sync`
    timer: Option<Retained<NSTimer>>,
}

/// what the widgets show, so `sync` touches only what changed
#[derive(Default)]
struct Shown {
    countdown: Option<u64>,
    status: String,
    enabled: Option<bool>,
    tray_icon: TrayIconState,
    fast_ticks: bool,
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
            let target: &AnyObject = self;
            self.with_app(|app| app.on_tick(now, target));
        }

        #[unsafe(method(takeBreak:))]
        fn take_break(&self, _sender: *mut AnyObject) {
            self.act(|app, now| app.core.take_break(now, None));
        }

        #[unsafe(method(takeShortBreak:))]
        fn take_short_break(&self, _sender: *mut AnyObject) {
            self.act(|app, now| app.core.take_break(now, Some(BreakType::Short)));
        }

        #[unsafe(method(takeLongBreak:))]
        fn take_long_break(&self, _sender: *mut AnyObject) {
            self.act(|app, now| app.core.take_break(now, Some(BreakType::Long)));
        }

        #[unsafe(method(toggleEnabled:))]
        fn toggle_enabled(&self, _sender: *mut AnyObject) {
            self.act(|app, now| {
                let on = !app.core.is_enabled();
                app.core.set_enabled(now, on)
            });
        }

        #[unsafe(method(openConfig:))]
        fn open_config(&self, _sender: *mut AnyObject) {
            self.with_app(|app| app.open_config_file());
        }

        #[unsafe(method(skip:))]
        fn skip(&self, _sender: *mut AnyObject) {
            self.act(|app, now| app.core.skip(now));
        }

        #[unsafe(method(postpone:))]
        fn postpone(&self, _sender: *mut AnyObject) {
            self.act(|app, now| app.core.postpone(now));
        }

        #[unsafe(method(dismissWarning:))]
        fn dismiss_warning(&self, _sender: *mut AnyObject) {
            self.act(|app, _| {
                app.warning_dismissed = true;
                None
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

    /// runs a user action, then lets the app react to it
    fn act(&self, action: impl FnOnce(&mut App, SystemTime) -> Option<Event>) {
        let now = SystemTime::now();
        let target: &AnyObject = self;
        self.with_app(|app| {
            let event = action(app, now);
            app.react(now, event, target);
        });
    }
}

impl App {
    fn new(
        mtm: MainThreadMarker,
        config: Config,
        config_path: PathBuf,
        ipc: Server,
        now: SystemTime,
    ) -> App {
        let dnd = DndChecker::from_config(&config.do_not_disturb);
        let tray_icon = tray_icon_image(TRAY_ICON_SVG);
        let tray_icon_incoming = tray_icon_image(TRAY_ICON_INCOMING_SVG);
        let break_screen = BreakScreen::new(mtm);
        let pre_break_dialog = PreBreakDialog::new(mtm);

        let core = Core::new(config.schedule.clone(), now);

        let config_mtime = file_mtime(&config_path);

        App {
            mtm,
            core,
            config,
            config_path,
            config_mtime,
            config_checked: None,
            ipc,
            status_item: None,
            tray_icon,
            tray_icon_incoming,
            enable_item: None,
            status_item_label: None,
            break_screen,
            pre_break_dialog,
            dnd,
            warning_dismissed: false,
            shown: Shown::default(),
            timer: None,
        }
    }

    fn on_tick(&mut self, now: SystemTime, target: &AnyObject) {
        for cmd in self.ipc.drain() {
            let event = self.handle_command(now, cmd);
            self.react(now, event, target);
        }

        self.maybe_reload_config(now);

        let idle = Duration::try_from_secs_f64(idle::idle_seconds()).unwrap_or_default();
        let event = self.core.tick(now, idle);
        self.react(now, event, target);
    }

    fn handle_command(&mut self, now: SystemTime, cmd: Command) -> Option<Event> {
        match cmd {
            Command::TakeBreak => self.core.take_break(now, None),
            Command::TakeShortBreak => self.core.take_break(now, Some(BreakType::Short)),
            Command::TakeLongBreak => self.core.take_break(now, Some(BreakType::Long)),
            Command::Enable => self.core.set_enabled(now, true),
            Command::Disable => self.core.set_enabled(now, false),
            Command::Quit => {
                self.quit();
                None
            }
            // answered on the IPC thread
            Command::Status => None,
            Command::Reload => {
                self.config_mtime = None;
                self.config_checked = None;
                None
            }
        }
    }

    /// runs the event's side effects, then brings the widgets in line with the core
    fn react(&mut self, now: SystemTime, event: Option<Event>, target: &AnyObject) {
        if let Some(event) = event {
            self.handle_event(now, event);
        }
        self.sync(now, target);
    }

    fn handle_event(&mut self, now: SystemTime, event: Event) {
        let alert = &self.config.audible_alert;
        match event {
            Event::Warn => {
                if self.dnd.should_suppress() {
                    self.core.suppress(now, self.config.do_not_disturb.action);
                } else if alert.enabled && alert.pre_break {
                    sound::play(&alert.pre_break_sound);
                }
            }
            Event::BreakEnded { completed: true } => {
                if alert.enabled && alert.post_break {
                    sound::play(&alert.post_break_sound);
                }
            }
            Event::BreakStarted | Event::BreakEnded { completed: false } => {}
        }
    }

    /// the only place the warning and the break screen are shown or hidden. every widget
    /// follows the core's phase, and each is touched only when what it shows changes.
    fn sync(&mut self, now: SystemTime, target: &AnyObject) {
        let phase = self.core.phase();
        let remaining = self.core.remaining(now);
        let warning = matches!(phase, Phase::Warning { .. });
        let in_break = matches!(phase, Phase::Break { .. });
        if !warning {
            self.warning_dismissed = false;
        }

        let show_warning = warning && self.config.notification.enabled && !self.warning_dismissed;
        if show_warning != self.pre_break_dialog.is_visible() {
            if show_warning {
                self.pre_break_dialog.show(
                    &self.core.current().name,
                    self.core.can_skip(),
                    self.core.can_postpone(),
                    remaining.unwrap_or(0),
                    self.config.keybindings.skip,
                    self.config.keybindings.postpone,
                    target,
                );
                self.shown.countdown = remaining;
            } else if in_break {
                self.pre_break_dialog.hide_for_break();
            } else {
                self.pre_break_dialog.hide();
            }
        }

        if in_break != self.break_screen.is_visible() {
            if in_break {
                let current = self.core.current();
                self.break_screen.show(
                    &current.name,
                    remaining.unwrap_or(0),
                    self.core.can_skip(),
                    self.core.can_postpone(),
                    self.config.keybindings.skip,
                    self.config.keybindings.postpone,
                    current.image.as_deref(),
                    target,
                );
                self.shown.countdown = remaining;
            } else {
                self.break_screen.hide();
            }
        }

        if remaining != self.shown.countdown {
            self.shown.countdown = remaining;
            if let Some(secs) = remaining {
                if self.pre_break_dialog.is_visible() {
                    self.pre_break_dialog.update_countdown(secs);
                }
                if self.break_screen.is_visible() {
                    self.break_screen.update_countdown(secs);
                }
            }
        }

        self.sync_menu(now);

        let incoming = matches!(phase, Phase::Waiting { .. })
            && remaining.is_some_and(|secs| secs <= INCOMING_WINDOW_SECS);
        self.sync_tray_icon(if incoming && tray_icon_blink_on(now) {
            TrayIconState::Incoming
        } else {
            TrayIconState::Normal
        });
        self.sync_timer(warning || in_break || incoming, target);
    }

    fn sync_menu(&mut self, now: SystemTime) {
        let status = self.core.status_text(now);
        if status != self.shown.status {
            if let Some(item) = &self.status_item_label {
                item.setTitle(&NSString::from_str(&status));
            }
            self.ipc.set_status(status.clone());
            self.shown.status = status;
        }

        let enabled = self.core.is_enabled();
        if self.shown.enabled != Some(enabled) {
            if let Some(item) = &self.enable_item {
                item.setState(if enabled {
                    NSControlStateValueOn
                } else {
                    NSControlStateValueOff
                });
            }
            self.shown.enabled = Some(enabled);
        }
    }

    fn sync_tray_icon(&mut self, state: TrayIconState) {
        if state != self.shown.tray_icon {
            self.shown.tray_icon = state;
            self.apply_status_icon(state);
        }
    }

    /// ticks fast while a countdown is on screen or the warning is near, slowly otherwise.
    /// the timer runs in the common run-loop modes so ticks keep coming while the status
    /// menu is open. that's re-entrancy-safe: menu tracking starts from event dispatch,
    /// never while the delegate's RefCell is borrowed.
    fn sync_timer(&mut self, fast: bool, target: &AnyObject) {
        if self.timer.is_some() && self.shown.fast_ticks == fast {
            return;
        }
        if let Some(timer) = self.timer.take() {
            timer.invalidate();
        }
        let (interval, tolerance) = if fast {
            (FAST_TICK_SECS, 0.05)
        } else {
            (SLOW_TICK_SECS, 0.1)
        };
        // the timer retains its target, and the run loop the timer, which keeps the
        // delegate and all state alive
        let timer = unsafe {
            NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                interval,
                target,
                sel!(onTimer:),
                None,
                true,
            )
        };
        timer.setTolerance(tolerance);
        unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
        self.timer = Some(timer);
        self.shown.fast_ticks = fast;
    }

    fn open_config_file(&self) {
        let _ = std::process::Command::new("open")
            .arg(&self.config_path)
            .status();
    }

    /// removes the socket itself: `terminate:` with no app delegate ends in `exit()`, which
    /// runs no destructors or pending timers
    fn quit(&self) {
        let _ = std::fs::remove_file(ipc::socket_path());
        NSApplication::sharedApplication(self.mtm).terminate(None);
    }

    fn maybe_reload_config(&mut self, now: SystemTime) {
        if matches!(
            self.core.phase(),
            Phase::Warning { .. } | Phase::Break { .. }
        ) {
            return;
        }
        if self
            .config_checked
            .is_some_and(|t| now.duration_since(t).is_ok_and(|d| d < CONFIG_POLL))
        {
            return;
        }
        self.config_checked = Some(now);

        let mtime = file_mtime(&self.config_path);
        if mtime == self.config_mtime {
            return;
        }
        self.config_mtime = mtime;

        match Config::from_file(&self.config_path) {
            Ok(new) => {
                self.apply_config(new);
                eprintln!("omma: reloaded the config");
            }
            Err(e) => eprintln!(
                "omma: {}: {e}; kept the previous config",
                self.config_path.display()
            ),
        }
    }

    fn apply_config(&mut self, new: Config) {
        self.core.reconfigure(new.schedule.clone());
        self.dnd = DndChecker::from_config(&new.do_not_disturb);
        self.config = new;
    }

    fn build_status_item(&mut self, delegate: &Delegate) {
        let status_bar = NSStatusBar::systemStatusBar();
        let item = status_bar.statusItemWithLength(NSVariableStatusItemLength);

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
        self.apply_status_icon(TrayIconState::Normal);
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

    fn apply_status_icon(&self, state: TrayIconState) {
        let Some(item) = &self.status_item else {
            return;
        };
        let Some(button) = item.button(self.mtm) else {
            return;
        };

        let image = match state {
            TrayIconState::Normal => self.tray_icon.as_ref(),
            TrayIconState::Incoming => self.tray_icon_incoming.as_ref().or(self.tray_icon.as_ref()),
        };
        match image {
            Some(image) => {
                button.setTitle(&NSString::from_str(""));
                button.setImage(Some(image));
            }
            None => button.setTitle(&NSString::from_str("\u{1F441}")), // fallback glyph
        }
    }
}

fn tray_icon_blink_on(now: SystemTime) -> bool {
    now.duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() / 500 % 2 == 0)
        .unwrap_or(false)
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}
