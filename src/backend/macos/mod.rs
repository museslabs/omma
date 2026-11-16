//! the macOS backend: a status item in the menu bar, and the warning and break screen in
//! Liquid Glass and Core Animation. all of it runs on the main thread. Delegate is the obj-c
//! target for menu items, buttons, global shortcuts and the timer, and owns the App in a RefCell
//! ivar

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
use std::time::{Duration, SystemTime};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject, Sel};
use objc2::{AnyThread, DefinedClass, MainThreadOnly, Message, define_class, msg_send, sel};
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

use super::Backend;
use crate::app::dnd::Reason;
use crate::app::{
    self, Action, App, Built, Buttons, FAST_TICK, HELP_URL, MenuLine, PAUSES, SLOW_TICK, TrayIcon,
    Ui, WarningKey,
};
use crate::config::{Config, ConfigError, DoNotDisturb, KeySpec};
use crate::ipc::Server;
use crate::queue::{Break, BreakType};
use break_screen::BreakScreen;
use hotkey::Hotkeys;
use pre_break_dialog::{HandOff, PreBreakDialog};

/// embedded, so the icon travels with the binary, whether omma runs from the .app or bare
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
/// System Settings, at the Accessibility list
const ACCESSIBILITY_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

/// None if the bytes fail to decode. the status item then shows a text glyph.
/// `description` is what VoiceOver reads for the icon
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

pub struct Macos;

impl Backend for Macos {
    /// the proof that omma runs on the main thread, which AppKit needs
    type System = MainThreadMarker;

    /// the warning and the break screen are made of NSGlassEffectView, new in macOS 26
    fn check_system() -> anyhow::Result<MainThreadMarker> {
        if AnyClass::get(c"NSGlassEffectView").is_none() {
            anyhow::bail!("requires macOS 26 or later");
        }
        MainThreadMarker::new().ok_or_else(|| anyhow::anyhow!("omma must run on the main thread"))
    }

    fn run(
        mtm: MainThreadMarker,
        config: Config,
        config_path: PathBuf,
        config_error: Option<ConfigError>,
        ipc: Server,
    ) -> anyhow::Result<()> {
        let ns_app = NSApplication::sharedApplication(mtm);
        ns_app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

        let now = SystemTime::now();
        let app = App::new(MacUi::new(mtm), config, config_path, config_error, ipc, now);
        let delegate = Delegate::new(mtm, app);
        delegate.with_app(|app| {
            app.ui.build_status_item(&delegate);
            app.sync(now);
        });

        ns_app.run();
        Ok(())
    }
}

/// the status item, its menu, and the warning and break screen
struct MacUi {
    mtm: MainThreadMarker,
    /// the delegate, which every menu item, button, shortcut and timer sends to. it lives as
    /// long as the app, and so does this reference to it
    target: Option<Retained<NSObject>>,
    status_item: Option<Retained<NSStatusItem>>,
    tray_icon: Option<Retained<NSImage>>,
    tray_icon_incoming: Option<Retained<NSImage>>,
    tray_icon_disabled: Option<Retained<NSImage>>,
    enable_item: Option<Retained<NSMenuItem>>,
    status_item_label: Option<Retained<NSMenuItem>>,
    /// why do-not-disturb held off the last break. hidden while there is no reason
    suppressed_item: Option<Retained<NSMenuItem>>,
    /// a heading and the error itself, which opens the config file when clicked. hidden
    /// while the config is fine
    config_error_items: Option<[Retained<NSMenuItem>; 2]>,
    /// one per `Problems` slot, hidden while that slot is empty
    problem_items: Option<[Retained<NSMenuItem>; 3]>,
    break_screen: BreakScreen,
    pre_break_dialog: PreBreakDialog,
    /// the warning's global shortcuts, held while it shows
    hotkeys: Hotkeys,
    /// the warning's bead as it hid, which the next break screen flies its bead in from
    hand_off: Option<HandOff>,
    /// set up by the first sync
    timer: Option<Retained<NSTimer>>,
}

define_class!(
    // SAFETY: NSObject can be subclassed. the delegate adds its RefCell ivar and its own
    // methods, and overrides none of NSObject's
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OmmaDelegate"]
    #[ivars = RefCell<App<MacUi>>]
    struct Delegate;

    impl Delegate {
        #[unsafe(method(onTimer:))]
        fn on_timer(&self, _timer: *mut AnyObject) {
            self.with_app(|app| app.tick(SystemTime::now()));
        }

        #[unsafe(method(takeBreak:))]
        fn take_break(&self, _sender: *mut AnyObject) {
            self.act(Action::TakeBreak(None));
        }

        #[unsafe(method(takeShortBreak:))]
        fn take_short_break(&self, _sender: *mut AnyObject) {
            self.act(Action::TakeBreak(Some(BreakType::Short)));
        }

        #[unsafe(method(takeLongBreak:))]
        fn take_long_break(&self, _sender: *mut AnyObject) {
            self.act(Action::TakeBreak(Some(BreakType::Long)));
        }

        #[unsafe(method(toggleEnabled:))]
        fn toggle_enabled(&self, _sender: *mut AnyObject) {
            self.act(Action::ToggleEnabled);
        }

        /// the item's tag is its length in seconds, and 0 lasts until midnight
        #[unsafe(method(pauseFor:))]
        fn pause_for(&self, sender: &NSMenuItem) {
            self.act(Action::Pause(u64::try_from(sender.tag()).unwrap_or(0)));
        }

        /// opens the file with the state not borrowed. Launch Services can run a nested event
        /// loop, and a tick in that loop borrows the state. if the state is still borrowed, the
        /// tick panics
        #[unsafe(method(openConfig:))]
        fn open_config(&self, _sender: *mut AnyObject) {
            let path = self.with_app(|app| app.config_path().to_path_buf());
            open_config_file(&path);
        }

        #[unsafe(method(openAccessibilitySettings:))]
        fn open_accessibility_settings(&self, _sender: *mut AnyObject) {
            if !open_url(ACCESSIBILITY_SETTINGS) {
                log!("can't open System Settings");
            }
        }

        #[unsafe(method(openHelp:))]
        fn open_help(&self, _sender: *mut AnyObject) {
            if !open_url(HELP_URL) {
                log!("can't open {HELP_URL}");
            }
        }

        #[unsafe(method(skip:))]
        fn skip(&self, _sender: *mut AnyObject) {
            self.act(Action::Skip);
        }

        #[unsafe(method(postpone:))]
        fn postpone(&self, _sender: *mut AnyObject) {
            self.act(Action::Postpone);
        }

        #[unsafe(method(warningSkip:))]
        fn warning_skip(&self, _sender: *mut AnyObject) {
            self.act(Action::Shortcut(WarningKey::Skip));
        }

        #[unsafe(method(warningPostpone:))]
        fn warning_postpone(&self, _sender: *mut AnyObject) {
            self.act(Action::Shortcut(WarningKey::Postpone));
        }

        #[unsafe(method(dismissWarning:))]
        fn dismiss_warning(&self, _sender: *mut AnyObject) {
            self.act(Action::DismissWarning);
        }

        #[unsafe(method(quitApp:))]
        fn quit_app(&self, _sender: *mut AnyObject) {
            self.act(Action::Quit);
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker, app: App<MacUi>) -> Retained<Delegate> {
        let this = Delegate::alloc(mtm).set_ivars(RefCell::new(app));
        // SAFETY: NSObject's init takes no arguments and returns the object it was sent to
        unsafe { msg_send![super(this), init] }
    }

    fn with_app<R>(&self, f: impl FnOnce(&mut App<MacUi>) -> R) -> R {
        f(&mut self.ivars().borrow_mut())
    }

    /// runs a user action, then lets the app react to it
    fn act(&self, action: Action) {
        self.with_app(|app| app.act(SystemTime::now(), action));
    }
}

impl MacUi {
    fn new(mtm: MainThreadMarker) -> MacUi {
        MacUi {
            mtm,
            target: None,
            status_item: None,
            // the incoming icon blinks with the plain one before a break and reads the same
            tray_icon: tray_icon_image(TRAY_ICON_SVG, "omma"),
            tray_icon_incoming: tray_icon_image(TRAY_ICON_INCOMING_SVG, "omma"),
            tray_icon_disabled: tray_icon_image(TRAY_ICON_DISABLED_SVG, "omma, disabled"),
            enable_item: None,
            status_item_label: None,
            suppressed_item: None,
            config_error_items: None,
            problem_items: None,
            break_screen: BreakScreen::new(mtm),
            pre_break_dialog: PreBreakDialog::new(mtm),
            hotkeys: Hotkeys::new(),
            hand_off: None,
            timer: None,
        }
    }

    /// the delegate, once `build_status_item` runs
    fn target(&self) -> &AnyObject {
        self.target
            .as_deref()
            .expect("the status item is built before anything is shown")
    }

    fn build_status_item(&mut self, delegate: &Delegate) {
        self.target = Some(Retained::into_super(delegate.retain()));
        let target: &AnyObject = delegate;
        let status_bar = NSStatusBar::systemStatusBar();
        let item = status_bar.statusItemWithLength(NSVariableStatusItemLength);

        let menu = NSMenu::new(self.mtm);

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
        let pauses = NSMenu::new(self.mtm);
        for (title, secs) in PAUSES {
            self.add_item(&pauses, title, sel!(pauseFor:), target)
                .setTag(isize::try_from(secs).unwrap_or(0));
        }
        let pause = NSMenuItem::new(self.mtm);
        pause.setTitle(&NSString::from_str("Pause"));
        pause.setSubmenu(Some(&pauses));
        menu.addItem(&pause);
        self.enable_item = Some(self.add_item(&menu, "Enabled", sel!(toggleEnabled:), target));
        menu.addItem(&NSMenuItem::separatorItem(self.mtm));
        self.add_item(&menu, "Open config…", sel!(openConfig:), target);
        self.add_item(&menu, "Help…", sel!(openHelp:), target);
        menu.addItem(&NSMenuItem::separatorItem(self.mtm));
        self.add_disabled_item(&menu, concat!("omma ", env!("CARGO_PKG_VERSION")));
        self.add_item(&menu, "Quit omma", sel!(quitApp:), target);

        item.setMenu(Some(&menu));
        self.status_item = Some(item);
        self.apply_status_icon(TrayIcon::Normal);
    }

    /// under the menu bar icon, on the bar's bottom edge, in screen coordinates: where the
    /// warning's bead hangs from. None when the icon is not on any display, as when the menu
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
        // SAFETY: the item does not retain its target, and the delegate lives as long as the
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

    fn apply_status_icon(&self, state: TrayIcon) {
        let Some(item) = &self.status_item else {
            return;
        };
        let Some(button) = item.button(self.mtm) else {
            return;
        };

        let image = match state {
            TrayIcon::Normal => self.tray_icon.as_ref(),
            TrayIcon::Incoming => self.tray_icon_incoming.as_ref().or(self.tray_icon.as_ref()),
            TrayIcon::Disabled => self.tray_icon_disabled.as_ref().or(self.tray_icon.as_ref()),
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

impl Ui for MacUi {
    const WINDOW_ACCESS_PROBLEM: &'static str =
        "the fullscreen and window-title rules need Accessibility permission";

    /// needs no permission
    fn idle_time(&mut self, _now: SystemTime) -> Duration {
        let secs = CGEventSource::seconds_since_last_event_type(
            CGEventSourceStateID::CombinedSessionState,
            CGEventType(!0), // kCGAnyInputEventType
        );
        Duration::try_from_secs_f64(secs).unwrap_or_default()
    }

    fn suppression(&mut self, dnd: &DoNotDisturb) -> Option<Reason> {
        activity::should_suppress(dnd)
    }

    /// the app rules look through NSWorkspace, which needs no permission
    fn needs_window_access(&self, dnd: &DoNotDisturb) -> bool {
        dnd.needs_accessibility()
    }

    fn has_window_access(&mut self, _dnd: &DoNotDisturb) -> bool {
        activity::is_trusted()
    }

    fn request_window_access(&mut self) {
        activity::request_accessibility();
    }

    fn play_sound(&mut self, sound: &str) {
        sound::play(sound);
    }

    fn animations_on(&self) -> bool {
        panel::animations_on()
    }

    fn quit(&mut self) {
        NSApplication::sharedApplication(self.mtm).terminate(None);
    }

    fn hold_shortcuts(&mut self, keys: &[(WarningKey, KeySpec)]) -> Vec<String> {
        let target = self
            .target
            .clone()
            .expect("the status item is built before anything is shown");
        self.hotkeys.hold(&target, keys)
    }

    fn held_shortcut(&self, key: WarningKey) -> Option<KeySpec> {
        self.hotkeys.held(key)
    }

    fn release_shortcuts(&mut self) {
        self.hotkeys.release();
    }

    fn warning_visible(&self) -> bool {
        self.pre_break_dialog.is_visible()
    }

    fn show_warning(&mut self, remaining: u64, buttons: Buttons) {
        let anchor = self.status_anchor();
        let target = self
            .target
            .clone()
            .expect("the status item is built before anything is shown");
        self.pre_break_dialog
            .show(remaining, buttons, &target, anchor);
    }

    fn update_warning(&mut self, remaining: u64) {
        self.pre_break_dialog.update_countdown(remaining);
    }

    fn hide_warning(&mut self, hand_off: bool) {
        // the warning's bead, read before the warning goes, for the break's bead to fly from
        self.hand_off = if hand_off {
            self.pre_break_dialog.hand_off()
        } else {
            None
        };
        self.pre_break_dialog.hide(hand_off);
    }

    fn break_visible(&self) -> bool {
        self.break_screen.is_visible()
    }

    fn show_break(&mut self, brk: &Break, remaining: u64, buttons: Buttons) -> Option<Built> {
        let anchor = self.status_anchor();
        let hand_off = self.hand_off.take();
        let target = self
            .target
            .clone()
            .expect("the status item is built before anything is shown");
        self.break_screen
            .show(brk, remaining, buttons, &target, hand_off, anchor)
    }

    fn update_break(&mut self, remaining: u64) {
        self.break_screen.update_countdown(remaining);
    }

    fn hide_break(&mut self) {
        self.break_screen.hide();
    }

    fn set_status(&mut self, text: &str) {
        if let Some(item) = &self.status_item_label {
            item.setTitle(&NSString::from_str(text));
        }
    }

    fn set_suppressed(&mut self, line: Option<&MenuLine>) {
        if let Some(item) = &self.suppressed_item {
            show_line(item, line);
        }
    }

    fn set_enabled(&mut self, on: bool) {
        if let Some(item) = &self.enable_item {
            item.setState(if on {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        }
    }

    fn set_config_error(&mut self, error: Option<(&str, &MenuLine)>) {
        let Some([heading, detail]) = &self.config_error_items else {
            return;
        };
        if let Some((title, _)) = error {
            heading.setTitle(&NSString::from_str(title));
        }
        heading.setHidden(error.is_none());
        show_line(detail, error.map(|(_, line)| line));
    }

    fn set_problems(&mut self, lines: &[Option<MenuLine>; 3]) {
        if let Some(items) = &self.problem_items {
            for (item, line) in items.iter().zip(lines) {
                show_line(item, line.as_ref());
            }
        }
    }

    fn set_tray_icon(&mut self, icon: TrayIcon) {
        self.apply_status_icon(icon);
    }

    /// the timer runs in the common run-loop modes, so ticks keep coming while the status
    /// menu is open. that is safe for re-entrancy, because menu tracking starts from event
    /// dispatch, never while the delegate's RefCell is borrowed
    fn set_fast_ticks(&mut self, fast: bool) {
        if let Some(timer) = self.timer.take() {
            timer.invalidate();
        }
        let (interval, tolerance) = if fast {
            (FAST_TICK, 0.05)
        } else {
            (SLOW_TICK, 0.1)
        };
        // the timer retains its target, and the run loop the timer, which keeps the
        // delegate and all state alive
        // SAFETY: `target` is the delegate, whose `onTimer:` takes the timer
        let timer = unsafe {
            NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                interval.as_secs_f64(),
                self.target(),
                sel!(onTimer:),
                None,
                true,
            )
        };
        timer.setTolerance(tolerance);
        // SAFETY: the mode is an immutable Foundation constant
        unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
        self.timer = Some(timer);
    }
}

/// fills a menu item with a line, or hides it for None
fn show_line(item: &NSMenuItem, line: Option<&MenuLine>) {
    if let Some(line) = line {
        item.setTitle(&NSString::from_str(&line.title));
        item.setToolTip(Some(&NSString::from_str(&line.tooltip)));
    }
    item.setHidden(line.is_none());
}

/// false if `url` does not parse or no app takes it
fn open_url(url: &str) -> bool {
    NSURL::URLWithString(&NSString::from_str(url))
        .is_some_and(|url| NSWorkspace::sharedWorkspace().openURL(&url))
}

/// opens the config file in the app set for it, or else in the default text editor. it first
/// creates a missing file from the defaults
fn open_config_file(path: &Path) {
    if !app::ensure_config_file(path) {
        return;
    }
    if NSURL::from_file_path(path).is_some_and(|url| NSWorkspace::sharedWorkspace().openURL(&url)) {
        return;
    }
    // `open` waits for Launch Services, so it runs off the main thread. it says why on stderr
    // when it fails
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
