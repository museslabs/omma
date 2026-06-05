//! all main-thread. Delegate is the obj-c target for menu items, buttons and the timer,
//! and owns the App state in a RefCell ivar.

mod activity;
mod break_screen;
mod glass_button;
mod idle;
mod pre_break_dialog;
mod sound;

use std::cell::RefCell;
use std::time::{SystemTime, UNIX_EPOCH};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, Sel};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuItem, NSStatusBar,
    NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{MainThreadMarker, NSData, NSSize, NSString, NSTimer};

use super::{Backend, BreakOverlay, Countdown, PreBreakDismissal, PreBreakNotice, StatusSnapshot};

use crate::config::Config;
use crate::ipc::{self, Server};
use crate::model::BreakType;
use crate::scheduler::State;
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
/// start blinking the tray icon this many seconds before the warning
const TRAY_ICON_INCOMING_WINDOW_SECS: i64 = 60;
/// sub-second so a 1s NSTimer firing near a deadline doesn't make digits linger or jump.
/// the scheduler stays deadline-based, this only affects ui refresh latency.
const TICK_INTERVAL_SECS: f64 = 0.2;

type AppController = crate::controller::Controller<App, Server>;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrayIconState {
    Normal,
    Incoming,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MacosBackend;

impl MacosBackend {
    pub fn run(self, config: Config, server: Server) -> anyhow::Result<()> {
        run(config, server)
    }
}

fn run(config: Config, server: Server) -> anyhow::Result<()> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| anyhow::anyhow!("omma must run on the main thread"))?;

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let dnd = DndChecker::from_config(&config.do_not_disturb);
    let state = App {
        mtm,
        status_item: None,
        tray_icon: tray_icon_image(TRAY_ICON_SVG),
        tray_icon_incoming: tray_icon_image(TRAY_ICON_INCOMING_SVG),
        tray_icon_state: TrayIconState::Normal,
        enable_item: None,
        status_item_label: None,
        break_screen: BreakScreen::new(mtm),
        pre_break_dialog: PreBreakDialog::new(mtm),
        dnd,
    };

    let controller = crate::controller::Controller::new(config, server, state)?;
    let delegate = Delegate::new(mtm, controller);

    delegate.with_controller(|controller| {
        controller.backend_mut().build_status_item(&delegate);
    });

    // NSTimer retains its target, so the delegate (and all state) stays alive for the run loop
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

/// mutated only from the main thread, via the delegate
struct App {
    mtm: MainThreadMarker,
    status_item: Option<Retained<NSStatusItem>>,
    tray_icon: Option<Retained<NSImage>>,
    tray_icon_incoming: Option<Retained<NSImage>>,
    tray_icon_state: TrayIconState,
    enable_item: Option<Retained<NSMenuItem>>,
    status_item_label: Option<Retained<NSMenuItem>>,
    break_screen: BreakScreen,
    pre_break_dialog: PreBreakDialog,
    dnd: DndChecker,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OmmaDelegate"]
    #[ivars = RefCell<AppController>]
    struct Delegate;

    impl Delegate {
        #[unsafe(method(onTimer:))]
        fn on_timer(&self, _timer: *mut AnyObject) {
            let now = SystemTime::now();
            let target: &AnyObject = self;
            self.with_controller(|controller| controller.on_tick(now, target));
        }

        #[unsafe(method(takeBreak:))]
        fn take_break(&self, _sender: *mut AnyObject) {
            self.with_controller(|controller| controller.take_break(None));
        }

        #[unsafe(method(takeShortBreak:))]
        fn take_short_break(&self, _sender: *mut AnyObject) {
            self.with_controller(|controller| controller.take_break(Some(BreakType::Short)));
        }

        #[unsafe(method(takeLongBreak:))]
        fn take_long_break(&self, _sender: *mut AnyObject) {
            self.with_controller(|controller| controller.take_break(Some(BreakType::Long)));
        }

        #[unsafe(method(toggleEnabled:))]
        fn toggle_enabled(&self, _sender: *mut AnyObject) {
            let now = SystemTime::now();
            self.with_controller(|controller| controller.toggle_enabled(now));
        }

        #[unsafe(method(openConfig:))]
        fn open_config(&self, _sender: *mut AnyObject) {
            self.with_controller(|controller| controller.open_config_file());
        }

        #[unsafe(method(skipBreak:))]
        fn skip_break(&self, _sender: *mut AnyObject) {
            self.with_controller(|controller| controller.skip_break());
        }

        #[unsafe(method(postponeBreak:))]
        fn postpone_break(&self, _sender: *mut AnyObject) {
            self.with_controller(|controller| controller.postpone_break());
        }

        #[unsafe(method(skipWarning:))]
        fn skip_warning(&self, _sender: *mut AnyObject) {
            let now = SystemTime::now();
            let target: &AnyObject = self;
            self.with_controller(|controller| controller.skip_warning(now, target));
        }

        #[unsafe(method(dismissWarning:))]
        fn dismiss_warning(&self, _sender: *mut AnyObject) {
            self.with_controller(|controller| controller.dismiss_warning());
        }

        #[unsafe(method(postponeWarning:))]
        fn postpone_warning(&self, _sender: *mut AnyObject) {
            let now = SystemTime::now();
            let target: &AnyObject = self;
            self.with_controller(|controller| controller.postpone_warning(now, target));
        }

        #[unsafe(method(quitApp:))]
        fn quit_app(&self, _sender: *mut AnyObject) {
            self.with_controller(|controller| controller.quit());
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker, controller: AppController) -> Retained<Delegate> {
        let this = Delegate::alloc(mtm).set_ivars(RefCell::new(controller));
        unsafe { msg_send![super(this), init] }
    }

    fn with_controller<R>(&self, f: impl FnOnce(&mut AppController) -> R) -> R {
        f(&mut self.ivars().borrow_mut())
    }
}

impl App {
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
        self.refresh_menu_state(true);
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

    fn refresh_menu_state(&self, enabled: bool) {
        if let Some(item) = &self.enable_item {
            item.setTitle(&NSString::from_str(if enabled {
                "Enabled"
            } else {
                "Disabled"
            }));
            // 1 = NSControlStateValueOn, 0 = off
            item.setState(if enabled { 1 } else { 0 });
        }
    }

    fn refresh_status_icon(&mut self, now: SystemTime, status: &StatusSnapshot) {
        let state = self.desired_tray_icon_state(now, status);
        if state == self.tray_icon_state {
            return;
        }
        self.tray_icon_state = state;
        self.apply_status_icon(state);
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

    fn desired_tray_icon_state(&self, now: SystemTime, status: &StatusSnapshot) -> TrayIconState {
        if !matches!(status.state, State::Waiting) {
            return TrayIconState::Normal;
        }
        let incoming = status
            .seconds_until_next
            .is_some_and(|s| (0..=TRAY_ICON_INCOMING_WINDOW_SECS).contains(&s));
        if incoming && tray_icon_blink_on(now) {
            TrayIconState::Incoming
        } else {
            TrayIconState::Normal
        }
    }

    fn refresh_status_label(&self, text: &str) {
        if let Some(item) = &self.status_item_label {
            item.setTitle(&NSString::from_str(text));
        }
    }
}

impl Backend for App {
    type ActionTarget = AnyObject;

    fn config_updated(&mut self, config: &Config) {
        self.dnd = DndChecker::from_config(&config.do_not_disturb);
    }

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

    fn dismiss_pre_break(&mut self, dismissal: PreBreakDismissal) {
        match dismissal {
            PreBreakDismissal::Cancelled => self.pre_break_dialog.hide(),
            PreBreakDismissal::BreakStarting => self.pre_break_dialog.hide_for_break(),
        }
    }

    fn show_break(&mut self, overlay: BreakOverlay<'_>, target: &Self::ActionTarget) {
        self.break_screen.show(
            overlay.name,
            overlay.duration,
            overlay.allow_skip,
            overlay.allow_postpone,
            overlay.skip_key,
            overlay.postpone_key,
            overlay.image,
            target,
        );
    }

    fn update_countdown(&mut self, countdown: Countdown) {
        match countdown {
            Countdown::PreBreak {
                seconds_until_break,
            } => self.pre_break_dialog.update_countdown(seconds_until_break),
            Countdown::Break { remaining, total } => {
                self.break_screen.update_countdown(remaining, total);
            }
        }
    }

    fn hide_break(&mut self) {
        self.break_screen.hide();
    }

    fn refresh_menu_state(&mut self, enabled: bool) {
        App::refresh_menu_state(self, enabled);
    }

    fn refresh_status(&mut self, now: SystemTime, status: &StatusSnapshot) {
        App::refresh_menu_state(self, status.enabled);
        self.refresh_status_icon(now, status);
        self.refresh_status_label(&status.text);
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

    fn quit(&mut self) {
        let _ = std::fs::remove_file(ipc::socket_path());
        let app = NSApplication::sharedApplication(self.mtm);
        app.terminate(None);
    }
}

fn tray_icon_blink_on(now: SystemTime) -> bool {
    now.duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() / 500 % 2 == 0)
        .unwrap_or(false)
}
