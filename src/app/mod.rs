//! all main-thread. Delegate is the obj-c target for menu items, buttons, global shortcuts
//! and the timer, and owns the App state in a RefCell ivar.

mod activity;
mod bead;
mod break_screen;
mod charm;
mod flip;
mod glass_button;
mod hotkey;
mod panel;
mod pre_break_dialog;
mod sound;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject, Sel};
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSControlStateValueOff, NSControlStateValueOn,
    NSImage, NSMenu, NSMenuItem, NSScreen, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
    NSWorkspace,
};
use objc2_core_graphics::{CGEventSource, CGEventSourceStateID, CGEventType};
use objc2_foundation::{
    MainThreadMarker, NSData, NSPoint, NSPointInRect, NSRunLoop, NSRunLoopCommonModes, NSSize,
    NSString, NSTimer, NSURL,
};

use crate::config::{self, Config, ConfigError, DndAction, DoNotDisturb, KeySpec};
use crate::ipc::{Command, Reply, Request, Server};
use crate::queue::BreakType;
use crate::scheduler::{Core, Event, Phase};
use break_screen::BreakScreen;
use glass_button::{Button, Buttons};
use hotkey::{Action, Hotkeys};
use pre_break_dialog::PreBreakDialog;

/// embedded so it travels with the binary whether launched from .app or bare
const TRAY_ICON_SVG: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tray-icon.svg"));
const TRAY_ICON_INCOMING_SVG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tray-icon-incoming.svg"
));
const TRAY_ICON_DISABLED_SVG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tray-icon-disabled.svg"
));

const TRAY_ICON_SIZE: f64 = 18.0;
/// the tray icon blinks, and the timer ticks fast, this many seconds before the warning
const INCOMING_WINDOW_SECS: u64 = 60;
/// while a countdown is on screen or the tray icon blinks. sub-second so a tick landing
/// near a deadline doesn't make digits linger or jump. the core is deadline-based either way.
const FAST_TICK_SECS: f64 = 0.2;
const SLOW_TICK_SECS: f64 = 1.0;
/// the config file is checked for changes at most this often
const CONFIG_POLL: Duration = Duration::from_secs(1);
/// the config error, do-not-disturb and problem items are cut to this many characters.
/// their tooltips show the whole text.
const MENU_CHARS: usize = 72;
const NEEDS_ACCESSIBILITY: &str =
    "the fullscreen and window-title rules need Accessibility permission";
/// System Settings, at the Accessibility list
const ACCESSIBILITY_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

/// None if the bytes fail to decode. the status item then shows a text glyph.
/// `description` is what VoiceOver reads for the icon.
fn tray_icon_image(bytes: &[u8], description: &str) -> Option<Retained<NSImage>> {
    let data = NSData::with_bytes(bytes);
    let image = NSImage::initWithData(NSImage::alloc(), &data)?;
    image.setAccessibilityDescription(Some(&NSString::from_str(description)));
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
    Disabled,
}

/// what stands in for a config file with an error
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fallback {
    Defaults,
    LastGood,
}

impl Fallback {
    fn describe(self) -> &'static str {
        match self {
            Fallback::Defaults => "using the defaults",
            Fallback::LastGood => "kept the last good config",
        }
    }
}

/// the warning and the break screen are made of NSGlassEffectView, new in macOS 26
pub fn check_system() -> anyhow::Result<()> {
    if AnyClass::get(c"NSGlassEffectView").is_none() {
        anyhow::bail!("requires macOS 26 or later");
    }
    Ok(())
}

/// `config_error` is why the file at `config_path` didn't load, with the defaults in its place
pub fn run(
    config: Config,
    config_path: PathBuf,
    config_error: Option<ConfigError>,
    ipc: Server,
) -> anyhow::Result<()> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| anyhow::anyhow!("omma must run on the main thread"))?;

    let ns_app = NSApplication::sharedApplication(mtm);
    ns_app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let now = SystemTime::now();
    let app = App::new(mtm, config, config_path, config_error, ipc, now);
    let delegate = Delegate::new(mtm, app);
    let target: &AnyObject = &delegate;
    delegate.with_app(|app| {
        app.build_status_item(&delegate);
        app.sync(now, target);
    });

    ns_app.run();
    Ok(())
}

/// what went wrong outside the config file. each one shows in the menu and in `omma --status`
#[derive(Clone, Default, PartialEq, Eq)]
struct Problems {
    /// why a break's image didn't load. it stays until a break shows its image or the
    /// config reloads
    image: Option<String>,
    /// the shortcuts the last warning couldn't take
    shortcuts: Option<String>,
    /// a window rule is on without the permission it needs
    accessibility: Option<String>,
}

impl Problems {
    /// in the order of their menu items
    fn slots(&self) -> [Option<&str>; 3] {
        [&self.image, &self.shortcuts, &self.accessibility].map(Option::as_deref)
    }
}

/// mutated only from the main thread, via the delegate
struct App {
    mtm: MainThreadMarker,
    core: Core,
    config: Config,
    config_path: PathBuf,
    config_mtime: Option<SystemTime>,
    config_checked: Option<SystemTime>,
    /// the file's last error, until it loads
    config_error: Option<(ConfigError, Fallback)>,
    ipc: Server,
    status_item: Option<Retained<NSStatusItem>>,
    tray_icon: Option<Retained<NSImage>>,
    tray_icon_incoming: Option<Retained<NSImage>>,
    tray_icon_disabled: Option<Retained<NSImage>>,
    enable_item: Option<Retained<NSMenuItem>>,
    status_item_label: Option<Retained<NSMenuItem>>,
    /// why do-not-disturb held off the last break. hidden while there's no reason
    suppressed_item: Option<Retained<NSMenuItem>>,
    /// a heading and the error itself, which opens the config file when clicked. hidden
    /// while the config is fine
    config_error_items: Option<[Retained<NSMenuItem>; 2]>,
    /// one per `Problems` slot, hidden while that slot is empty
    problem_items: Option<[Retained<NSMenuItem>; 3]>,
    problems: Problems,
    break_screen: BreakScreen,
    pre_break_dialog: PreBreakDialog,
    /// the warning's global shortcuts, held while it shows
    hotkeys: Hotkeys,
    /// the user closed the warning without skipping or postponing; cleared when it ends
    warning_dismissed: bool,
    /// why do-not-disturb held off the last break, while omma waits for the next one
    suppressed: Option<String>,
    shown: Shown,
    /// set up by the first `sync`
    timer: Option<Retained<NSTimer>>,
}

/// what the widgets show, so `sync` touches only what changed
#[derive(Default)]
struct Shown {
    countdown: Option<u64>,
    status: String,
    suppressed: Option<String>,
    enabled: Option<bool>,
    /// the warning's global shortcuts are held
    hotkeys: bool,
    config_error: Option<(ConfigError, Fallback)>,
    problems: Problems,
    tray_icon: TrayIconState,
    fast_ticks: bool,
}

define_class!(
    // SAFETY: NSObject can be subclassed. the delegate adds its RefCell ivar and its own
    // methods, and overrides none of NSObject's
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

        /// opens the file with the state unborrowed. if Launch Services ran a nested event
        /// loop, a tick inside it would borrow the state again and panic.
        #[unsafe(method(openConfig:))]
        fn open_config(&self, _sender: *mut AnyObject) {
            let path = self.with_app(|app| app.config_path.clone());
            open_config_file(&path);
        }

        #[unsafe(method(openAccessibilitySettings:))]
        fn open_accessibility_settings(&self, _sender: *mut AnyObject) {
            let opened = NSURL::URLWithString(&NSString::from_str(ACCESSIBILITY_SETTINGS))
                .is_some_and(|url| NSWorkspace::sharedWorkspace().openURL(&url));
            if !opened {
                log!("can't open System Settings");
            }
        }

        #[unsafe(method(skip:))]
        fn skip(&self, _sender: *mut AnyObject) {
            self.act(|app, now| app.core.skip(now));
        }

        #[unsafe(method(postpone:))]
        fn postpone(&self, _sender: *mut AnyObject) {
            self.act(|app, now| app.core.postpone(now));
        }

        /// a global shortcut whose press was queued before the warning ended or was dismissed
        /// arrives after its hotkeys were released; then it does nothing
        #[unsafe(method(warningSkip:))]
        fn warning_skip(&self, _sender: *mut AnyObject) {
            self.act(|app, now| app.shown.hotkeys.then(|| app.core.skip(now)).flatten());
        }

        #[unsafe(method(warningPostpone:))]
        fn warning_postpone(&self, _sender: *mut AnyObject) {
            self.act(|app, now| app.shown.hotkeys.then(|| app.core.postpone(now)).flatten());
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
        // SAFETY: NSObject's init takes no arguments and returns the object it was sent to
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
        config_error: Option<ConfigError>,
        ipc: Server,
        now: SystemTime,
    ) -> App {
        if config.do_not_disturb.needs_accessibility() {
            activity::request_accessibility();
        }
        // the incoming icon blinks with the plain one before a break and reads the same
        let tray_icon = tray_icon_image(TRAY_ICON_SVG, "omma");
        let tray_icon_incoming = tray_icon_image(TRAY_ICON_INCOMING_SVG, "omma");
        let tray_icon_disabled = tray_icon_image(TRAY_ICON_DISABLED_SVG, "omma, disabled");
        let break_screen = BreakScreen::new(mtm);
        let pre_break_dialog = PreBreakDialog::new(mtm);

        let core = Core::new(config.schedule.clone(), now);

        let config_mtime = file_mtime(&config_path);

        let mut app = App {
            mtm,
            core,
            config,
            config_path,
            config_mtime,
            config_checked: None,
            config_error: config_error.map(|e| (e, Fallback::Defaults)),
            ipc,
            status_item: None,
            tray_icon,
            tray_icon_incoming,
            tray_icon_disabled,
            enable_item: None,
            status_item_label: None,
            suppressed_item: None,
            config_error_items: None,
            problem_items: None,
            problems: Problems::default(),
            break_screen,
            pre_break_dialog,
            hotkeys: Hotkeys::new(),
            warning_dismissed: false,
            suppressed: None,
            shown: Shown::default(),
            timer: None,
        };
        app.check_accessibility();
        app
    }

    fn on_tick(&mut self, now: SystemTime, target: &AnyObject) {
        for request in self.ipc.drain() {
            let event = self.handle_request(now, request);
            self.react(now, event, target);
        }

        self.maybe_reload_config(now);
        // while the permission is missing, each tick checks again, so a grant clears the item
        // at once
        if self.problems.accessibility.is_some() {
            self.check_accessibility();
        }

        let event = self.core.tick(now, idle_time());
        self.react(now, event, target);
    }

    /// runs a client's command, then replies with what it did
    fn handle_request(&mut self, now: SystemTime, request: Request) -> Option<Event> {
        let (event, reply) = self.run_command(now, request.command);
        request.reply(&reply);
        if request.command == Command::Quit {
            // `terminate:` ends in `exit()` with `request` still open, so the client's
            // connection closes with the process. `omma --quit` returns once omma is gone.
            self.quit();
        }
        event
    }

    fn run_command(&mut self, now: SystemTime, command: Command) -> (Option<Event>, Reply) {
        match command {
            Command::TakeBreak => self.cli_take_break(now, None),
            Command::TakeShortBreak => self.cli_take_break(now, Some(BreakType::Short)),
            Command::TakeLongBreak => self.cli_take_break(now, Some(BreakType::Long)),
            Command::Enable => self.cli_set_enabled(now, true),
            Command::Disable => self.cli_set_enabled(now, false),
            Command::Status => (None, Ok(self.status_report(now))),
            Command::Reload => {
                let reply = self.reload_config();
                if let Err(text) = &reply {
                    log!("{text}");
                }
                (None, reply)
            }
            Command::Quit => (None, Ok("quitting".into())),
        }
    }

    fn cli_take_break(
        &mut self,
        now: SystemTime,
        kind: Option<BreakType>,
    ) -> (Option<Event>, Reply) {
        let event = self.core.take_break(now, kind);
        let reply = if event.is_some() {
            Ok(format!("break started: {}", self.core.current().name))
        } else if self.core.is_enabled() {
            Err("a break is already running".into())
        } else {
            Err("omma is disabled".into())
        };
        (event, reply)
    }

    fn cli_set_enabled(&mut self, now: SystemTime, on: bool) -> (Option<Event>, Reply) {
        let unchanged = self.core.is_enabled() == on;
        let event = self.core.set_enabled(now, on);
        let state = if on { "enabled" } else { "disabled" };
        let text = if unchanged {
            format!("already {state}")
        } else {
            state.into()
        };
        (event, Ok(text))
    }

    /// the status line, why do-not-disturb held off the last break, the config file's error
    /// and the problems, each on its own line when there is one
    fn status_report(&self, now: SystemTime) -> String {
        let error = self
            .config_error
            .as_ref()
            .map(|(e, fallback)| self.describe_config_error(e, *fallback));
        [
            Some(self.core.status_text(now)),
            self.suppressed.clone(),
            error,
        ]
        .into_iter()
        .flatten()
        .chain(
            self.problems
                .slots()
                .into_iter()
                .flatten()
                .map(String::from),
        )
        .collect::<Vec<_>>()
        .join("\n")
    }

    /// runs the event's side effects, then brings the widgets in line with the core
    fn react(&mut self, now: SystemTime, event: Option<Event>, target: &AnyObject) {
        if let Some(event) = event {
            self.handle_event(now, event);
        }
        self.sync(now, target);
    }

    fn handle_event(&mut self, now: SystemTime, event: Event) {
        let sounds = &self.config.sounds;
        match event {
            Event::Warn => {
                let dnd = &self.config.do_not_disturb;
                set_problem(&mut self.problems.accessibility, accessibility_problem(dnd));
                self.suppressed = activity::should_suppress(dnd).map(|reason| match dnd.action {
                    DndAction::Skip => format!("Skipped the last break: {reason}"),
                    DndAction::Postpone => format!("Postponed the break: {reason}"),
                });
                if let Some(text) = &self.suppressed {
                    log!("do-not-disturb: {text}");
                    self.core.suppress(now, dnd.action);
                } else if let Some(name) = &sounds.pre_break {
                    sound::play(name);
                }
            }
            Event::BreakEnded { completed: true } => {
                if let Some(name) = &sounds.post_break {
                    sound::play(name);
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
        if !matches!(phase, Phase::Waiting { .. }) {
            self.suppressed = None;
        }

        // with pre_break_warning = 0 the warning phase still lasts a tick, to check
        // do-not-disturb and play the sound, but shows nothing
        let show_warning =
            warning && !self.config.schedule.pre_break_warning.is_zero() && !self.warning_dismissed;
        // the warning's bead, read before the warning goes, for the break's bead to fly from
        let hand_off = in_break.then(|| self.pre_break_dialog.hand_off()).flatten();
        // the global shortcuts follow the warning, whether or not its panel could open
        if show_warning != self.shown.hotkeys {
            if show_warning {
                let keys = self.config.keybindings;
                let wanted: Vec<_> = [
                    (Action::Skip, keys.warning_skip, self.core.can_skip()),
                    (
                        Action::Postpone,
                        keys.warning_postpone,
                        self.core.can_postpone(),
                    ),
                ]
                .into_iter()
                .filter_map(|(action, key, allowed)| Some((action, key.filter(|_| allowed)?)))
                .collect();
                let refused = self.hotkeys.hold(target, &wanted);
                set_problem(
                    &mut self.problems.shortcuts,
                    (!refused.is_empty()).then(|| refused.join("; ")),
                );
            } else {
                self.hotkeys.release();
            }
            self.shown.hotkeys = show_warning;
        }
        if show_warning != self.pre_break_dialog.is_visible() {
            if show_warning {
                let buttons = self.buttons(
                    self.hotkeys.held(Action::Skip),
                    self.hotkeys.held(Action::Postpone),
                );
                let anchor = self.status_anchor();
                self.pre_break_dialog
                    .show(remaining.unwrap_or(0), buttons, target, anchor);
                self.shown.countdown = remaining;
            } else {
                self.pre_break_dialog.hide(in_break);
            }
        }

        if in_break != self.break_screen.is_visible() {
            if in_break {
                let keys = self.config.keybindings;
                let buttons = self.buttons(keys.skip, keys.postpone);
                let anchor = self.status_anchor();
                let built = self.break_screen.show(
                    self.core.current(),
                    remaining.unwrap_or(0),
                    buttons,
                    target,
                    hand_off,
                    anchor,
                );
                // a break gets one screen, so this runs once per break, as start_countdown_at
                // requires. building takes time, so the countdown starts from the clock after
                // it, and `now` stays when the screen was asked for.
                if let Some(built) = built {
                    let built_at = SystemTime::now();
                    self.core.start_countdown_at(now, built_at + built.settling);
                    match built.image {
                        Some(Err(problem)) => set_problem(&mut self.problems.image, Some(problem)),
                        Some(Ok(())) => self.problems.image = None,
                        None => {}
                    }
                }
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
        // with reduced motion the incoming icon holds steady instead of blinking
        let lit = incoming && (!panel::animations_on() || tray_icon_blink_on(now));
        self.sync_tray_icon(if matches!(phase, Phase::Disabled) {
            TrayIconState::Disabled
        } else if lit {
            TrayIconState::Incoming
        } else {
            TrayIconState::Normal
        });
        self.sync_timer(warning || in_break || incoming, target);
    }

    /// the skip and postpone buttons the core allows, with these shortcuts
    fn buttons(&self, skip: Option<KeySpec>, postpone: Option<KeySpec>) -> Buttons {
        Buttons {
            skip: self.core.can_skip().then_some(Button { key: skip }),
            postpone: self.core.can_postpone().then_some(Button { key: postpone }),
        }
    }

    fn sync_menu(&mut self, now: SystemTime) {
        let status = self.core.status_text(now);
        if status != self.shown.status {
            if let Some(item) = &self.status_item_label {
                item.setTitle(&NSString::from_str(&status));
            }
            self.shown.status = status;
        }

        if self.shown.suppressed != self.suppressed {
            if let Some(item) = &self.suppressed_item {
                if let Some(text) = &self.suppressed {
                    item.setTitle(&NSString::from_str(&shorten(text, MENU_CHARS)));
                    item.setToolTip(Some(&NSString::from_str(text)));
                }
                item.setHidden(self.suppressed.is_none());
            }
            self.shown.suppressed = self.suppressed.clone();
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

        if self.shown.config_error != self.config_error {
            if let Some([heading, detail]) = &self.config_error_items {
                show_config_error(heading, detail, self.config_error.as_ref());
            }
            self.shown.config_error = self.config_error.clone();
        }

        if self.shown.problems != self.problems {
            if let Some(items) = &self.problem_items {
                for (item, problem) in items.iter().zip(self.problems.slots()) {
                    show_problem(item, problem);
                }
            }
            self.shown.problems = self.problems.clone();
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
    /// menu is open. that's re-entrancy-safe, because menu tracking starts from event
    /// dispatch, never while the delegate's RefCell is borrowed.
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
        // SAFETY: `target` is the delegate, whose `onTimer:` takes the timer
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
        // SAFETY: the mode is an immutable Foundation constant
        unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
        self.timer = Some(timer);
        self.shown.fast_ticks = fast;
    }

    fn quit(&self) {
        NSApplication::sharedApplication(self.mtm).terminate(None);
    }

    /// reloads the config file when it changed, looking at most once per `CONFIG_POLL`
    fn maybe_reload_config(&mut self, now: SystemTime) {
        if self
            .config_checked
            .is_some_and(|t| now.duration_since(t).is_ok_and(|d| d < CONFIG_POLL))
        {
            return;
        }
        self.config_checked = Some(now);
        if file_mtime(&self.config_path) != self.config_mtime {
            let (Ok(text) | Err(text)) = self.reload_config();
            log!("{text}");
        }
    }

    /// rereads the config file. after an error the config in effect stays, and the menu
    /// shows the error until the file loads.
    fn reload_config(&mut self) -> Reply {
        self.config_mtime = file_mtime(&self.config_path);
        match Config::from_file(&self.config_path) {
            Ok(new) => {
                self.apply_config(new);
                self.config_error = None;
                Ok("reloaded the config".into())
            }
            Err(e) => {
                let fallback = match self.config_error {
                    Some((_, Fallback::Defaults)) => Fallback::Defaults,
                    _ => Fallback::LastGood,
                };
                let text = self.describe_config_error(&e, fallback);
                self.config_error = Some((e, fallback));
                Err(text)
            }
        }
    }

    fn describe_config_error(&self, error: &ConfigError, fallback: Fallback) -> String {
        format!(
            "{}: {error}; {}",
            self.config_path.display(),
            fallback.describe()
        )
    }

    /// asks for Accessibility when `new` needs it and the config it replaces didn't
    fn apply_config(&mut self, new: Config) {
        self.core.reconfigure(new.schedule.clone());
        if new.do_not_disturb.needs_accessibility()
            && !self.config.do_not_disturb.needs_accessibility()
        {
            activity::request_accessibility();
        }
        self.config = new;
        self.problems.image = None;
        self.check_accessibility();
    }

    fn check_accessibility(&mut self) {
        let problem = accessibility_problem(&self.config.do_not_disturb);
        set_problem(&mut self.problems.accessibility, problem);
    }

    fn build_status_item(&mut self, delegate: &Delegate) {
        let status_bar = NSStatusBar::systemStatusBar();
        let item = status_bar.statusItemWithLength(NSVariableStatusItemLength);

        let menu = NSMenu::new(self.mtm);
        let target: &AnyObject = delegate;

        self.status_item_label = Some(self.add_disabled_item(&menu, "Starting…"));
        let suppressed = self.add_disabled_item(&menu, "");
        suppressed.setHidden(true);
        self.suppressed_item = Some(suppressed);
        let heading = self.add_disabled_item(&menu, "");
        let detail = self.add_item(&menu, "", sel!(openConfig:), target);
        heading.setHidden(true);
        detail.setHidden(true);
        self.config_error_items = Some([heading, detail]);
        let problems = [
            self.add_item(&menu, "", sel!(openConfig:), target),
            self.add_item(&menu, "", sel!(openConfig:), target),
            self.add_item(&menu, "", sel!(openAccessibilitySettings:), target),
        ];
        for item in &problems {
            item.setHidden(true);
        }
        self.problem_items = Some(problems);
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

    /// under the menu-bar icon, on the bar's bottom edge, in screen coordinates: where the
    /// warning's bead hangs from. None when the icon isn't on any display, as when the menu
    /// bar is too full for it
    fn status_anchor(&self) -> Option<NSPoint> {
        let button = self.status_item.as_ref()?.button(self.mtm)?;
        let frame = button.window()?.frame();
        let middle = NSPoint::new(
            frame.origin.x + frame.size.width / 2.0,
            frame.origin.y + frame.size.height / 2.0,
        );
        // the icon's window overhangs the bar, so the edge comes from the display
        let screen = NSScreen::screens(self.mtm)
            .iter()
            .find(|screen| NSPointInRect(middle, screen.frame()))?;
        Some(NSPoint::new(
            middle.x,
            pre_break_dialog::menu_bar_edge(&screen),
        ))
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
        // SAFETY: `action` is a delegate method that takes the sender
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm),
                &title,
                Some(action),
                &key,
            )
        };
        // SAFETY: the item doesn't retain its target, and the delegate lives as long as the
        // app
        unsafe { item.setTarget(Some(target)) };
        menu.addItem(&item);
        item
    }

    fn add_disabled_item(&self, menu: &NSMenu, title: &str) -> Retained<NSMenuItem> {
        let title = NSString::from_str(title);
        let key = NSString::from_str("");
        // SAFETY: with no action, the item never sends anything
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
            TrayIconState::Disabled => self.tray_icon_disabled.as_ref().or(self.tray_icon.as_ref()),
        };
        match image {
            Some(image) => {
                button.setTitle(&NSString::from_str(""));
                button.setImage(Some(image));
            }
            // an eye, when no svg for this state decoded
            None => button.setTitle(&NSString::from_str("\u{1F441}")),
        }
    }
}

/// fills the menu's two config error items, or hides them when there's no error
fn show_config_error(
    heading: &NSMenuItem,
    detail: &NSMenuItem,
    error: Option<&(ConfigError, Fallback)>,
) {
    if let Some((error, fallback)) = error {
        let full = error.to_string();
        let title = format!("⚠ Config error: {}", fallback.describe());
        heading.setTitle(&NSString::from_str(&title));
        detail.setTitle(&NSString::from_str(&shorten(&full, MENU_CHARS)));
        detail.setToolTip(Some(&NSString::from_str(&full)));
    }
    heading.setHidden(error.is_none());
    detail.setHidden(error.is_none());
}

/// fills a problem's item, or hides it when there's no problem
fn show_problem(item: &NSMenuItem, problem: Option<&str>) {
    if let Some(problem) = problem {
        let title = format!("⚠ {}", shorten(&capitalized(problem), MENU_CHARS));
        item.setTitle(&NSString::from_str(&title));
        item.setToolTip(Some(&NSString::from_str(problem)));
    }
    item.setHidden(problem.is_none());
}

/// replaces a problem and logs it when it's new
fn set_problem(slot: &mut Option<String>, problem: Option<String>) {
    if let Some(text) = &problem
        && slot.as_ref() != Some(text)
    {
        log!("{text}");
    }
    *slot = problem;
}

fn accessibility_problem(dnd: &DoNotDisturb) -> Option<String> {
    (dnd.needs_accessibility() && !activity::is_trusted()).then(|| NEEDS_ACCESSIBILITY.into())
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// `text` cut to `chars` characters, with an ellipsis when it was longer
fn shorten(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.into(),
    }
}

/// each half of the blink lasts a whole number of fast ticks. otherwise the ticks, which
/// are what change the icon, would catch one half late and the next early
const BLINK_HALF_MS: u128 = 600;

fn tray_icon_blink_on(now: SystemTime) -> bool {
    now.duration_since(UNIX_EPOCH)
        .is_ok_and(|elapsed| (elapsed.as_millis() / BLINK_HALF_MS).is_multiple_of(2))
}

/// time since the last keyboard, mouse or trackpad input to any app. needs no permission.
fn idle_time() -> Duration {
    let secs = CGEventSource::seconds_since_last_event_type(
        CGEventSourceStateID::CombinedSessionState,
        CGEventType(!0), // kCGAnyInputEventType
    );
    Duration::try_from_secs_f64(secs).unwrap_or_default()
}

/// opens the config file in the app set for it, or else in the default text editor. a
/// missing file is created from the defaults first.
fn open_config_file(path: &Path) {
    match path.try_exists() {
        Ok(true) => {}
        Ok(false) => {
            if let Err(e) = config::write_default(path) {
                log!("can't create {}: {e}", path.display());
                return;
            }
        }
        Err(e) => log!("can't check for {}: {e}", path.display()),
    }
    if NSURL::from_file_path(path).is_some_and(|url| NSWorkspace::sharedWorkspace().openURL(&url)) {
        return;
    }
    // `open` waits for Launch Services, so it runs off the main thread. it says why on stderr
    // when it fails.
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let opened = std::process::Command::new("open")
            .arg("-t")
            .arg(&path)
            .status();
        if !opened.is_ok_and(|status| status.success()) {
            log!("can't open {}", path.display());
        }
    });
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capitalized_uppercases_the_first_character() {
        assert_eq!(capitalized("can't show"), "Can't show");
        assert_eq!(capitalized("ένα"), "Ένα");
        assert_eq!(capitalized(""), "");
    }

    #[test]
    fn shorten_cuts_whole_characters() {
        assert_eq!(shorten("Next break", 10), "Next break");
        assert_eq!(shorten("Next break", 4), "Next…");
        assert_eq!(shorten("“Πλάνο” is on screen", 7), "“Πλάνο”…");
        assert_eq!(shorten("", 0), "");
    }
}
