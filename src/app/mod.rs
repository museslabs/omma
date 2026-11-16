//! the app around the schedule, the same on every platform. it owns the core, the config and
//! what went wrong, answers the CLI, and tells the platform's `Ui` what to show. the
//! platform owns the widgets and the event loop, and calls in with the user's actions and
//! its timer's ticks

mod clock;
pub mod dnd;
pub mod format;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use clock::next_midnight;

use crate::config::{self, Config, ConfigError, DndAction, DoNotDisturb, KeySpec};
use crate::ipc::{Command, Reply, Request, Server};
use crate::queue::{Break, BreakType};
use crate::scheduler::{Core, Event, Phase, fmt_duration};
use dnd::Reason;
use format::{capitalized, shorten};

/// the tray icon blinks, and the timer ticks fast, this many seconds before the warning
const INCOMING_WINDOW_SECS: u64 = 60;
/// while a countdown is on screen or the tray icon blinks. it is under a second, so a tick
/// that lands near a deadline does not make digits linger or jump. the core works from
/// deadlines either way
pub const FAST_TICK: Duration = Duration::from_millis(200);
pub const SLOW_TICK: Duration = Duration::from_secs(1);
/// omma checks the config file for changes at most this often
const CONFIG_POLL: Duration = Duration::from_secs(1);
/// the config error, do-not-disturb and problem items show at most this many characters.
/// their tooltips show the whole text
const MENU_CHARS: usize = 72;
/// the docs, from their first page
pub const HELP_URL: &str = "https://museslabs.org/docs/omma/introduction/";
/// the Pause submenu: each item and its length in seconds, where 0 lasts until midnight
pub const PAUSES: [(&str, u64); 4] = [
    ("For 30 minutes", 30 * 60),
    ("For 1 hour", 60 * 60),
    ("For 2 hours", 2 * 60 * 60),
    ("For the rest of the day", 0),
];
/// each half of the tray icon's blink lasts a whole number of fast ticks. if not, the ticks,
/// which change the icon, catch one half late and the next early
const BLINK_HALF_MS: u128 = 600;

/// a shortcut that acts on the warning from any app
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarningKey {
    Skip = 1,
    Postpone = 2,
}

/// the buttons a window shows: skip and postpone, when allowed
#[derive(Clone, Copy, Debug)]
pub struct Buttons {
    pub skip: Option<Button>,
    pub postpone: Option<Button>,
}

#[derive(Clone, Copy, Debug)]
pub struct Button {
    pub key: Option<KeySpec>,
}

/// a break screen that `Ui::show_break` put up
pub struct Built {
    /// how long after `show_break` returned the countdown comes into focus: the entrance, or
    /// the cross-fade under reduced motion
    pub settling: Duration,
    /// whether the break's image loaded, or why not. None when the break has no image
    pub image: Option<Result<(), String>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrayIcon {
    #[default]
    Normal,
    /// blinks with the plain one before a break
    Incoming,
    Disabled,
}

/// a menu item's title, and the whole text for its tooltip
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuLine {
    pub title: String,
    pub tooltip: String,
}

/// what went wrong outside the config file. each one shows in the menu and in `omma --status`
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Problems {
    /// why a break's image did not load. it stays until a break shows its image or the
    /// config reloads
    pub image: Option<String>,
    /// the shortcuts that the last warning failed to take
    pub shortcuts: Option<String>,
    /// a window rule is on without the access it needs
    pub window_access: Option<String>,
}

impl Problems {
    /// in the order of their menu items
    fn slots(&self) -> [Option<&str>; 3] {
        [&self.image, &self.shortcuts, &self.window_access].map(Option::as_deref)
    }

    /// each slot as a menu item, None where there is no problem
    pub fn lines(&self) -> [Option<MenuLine>; 3] {
        self.slots().map(|problem| {
            problem.map(|text| MenuLine {
                title: format!("⚠ {}", shorten(&capitalized(text), MENU_CHARS)),
                tooltip: text.into(),
            })
        })
    }
}

/// what takes the place of a config file with an error
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

/// something the user did from the menu, a button, a key or a global shortcut
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// the next break, or the next of a kind
    TakeBreak(Option<BreakType>),
    ToggleEnabled,
    /// off for this many seconds, or until midnight for 0
    Pause(u64),
    Skip,
    Postpone,
    /// a global shortcut, which counts only while the warning holds its shortcuts
    Shortcut(WarningKey),
    /// closes the warning without skipping or postponing
    DismissWarning,
    Quit,
}

/// the platform's widgets and system services. `App` calls these only from the thread that
/// runs the platform's event loop
pub trait Ui {
    /// what the menu says when a window rule lacks the access it needs
    const WINDOW_ACCESS_PROBLEM: &'static str;

    /// time since the last keyboard or pointer input to any app
    fn idle_time(&mut self, now: SystemTime) -> Duration;
    /// the first do-not-disturb rule that holds now
    fn suppression(&mut self, dnd: &DoNotDisturb) -> Option<Reason>;
    /// whether the rules read what other apps are doing in a way that needs access granted,
    /// or a protocol that some systems lack
    fn needs_window_access(&self, dnd: &DoNotDisturb) -> bool;
    /// whether the access these rules need is there now. it can change while omma runs
    fn has_window_access(&mut self, dnd: &DoNotDisturb) -> bool;
    /// asks for it, on a system that has a way to ask
    fn request_window_access(&mut self);
    /// `sound` is what the config checked: a name or a file, as the platform plays it
    fn play_sound(&mut self, sound: &str);
    /// false when the user asked the system to reduce motion
    fn animations_on(&self) -> bool;
    /// the config in effect, at startup and after each change, for the parts that the
    /// platform manages itself
    fn config_changed(&mut self, _config: &Config) {}
    /// ends the event loop
    fn quit(&mut self);

    /// takes each of `keys` for the warning while it shows, and says why it failed to take
    /// the others
    fn hold_shortcuts(&mut self, keys: &[(WarningKey, KeySpec)]) -> Vec<String>;
    /// the shortcut held for `key`, if one was taken
    fn held_shortcut(&self, key: WarningKey) -> Option<KeySpec>;
    fn release_shortcuts(&mut self);

    fn warning_visible(&self) -> bool;
    /// the warning, which counts down `remaining` seconds to the break. without skip, it
    /// offers Dismiss instead. its keys are global shortcuts, which the buttons only show
    fn show_warning(&mut self, remaining: u64, buttons: Buttons);
    fn update_warning(&mut self, remaining: u64);
    /// with `hand_off`, the break screen about to show takes over the warning's bead
    fn hide_warning(&mut self, hand_off: bool);

    fn break_visible(&self) -> bool;
    /// covers every display with `brk`, which counts down from `remaining`. None when there
    /// was no display to show it on. the next sync tries again
    fn show_break(&mut self, brk: &Break, remaining: u64, buttons: Buttons) -> Option<Built>;
    fn update_break(&mut self, remaining: u64);
    fn hide_break(&mut self);

    /// the menu's first line
    fn set_status(&mut self, text: &str);
    /// why do-not-disturb held off the last break, hidden for None
    fn set_suppressed(&mut self, line: Option<&MenuLine>);
    fn set_enabled(&mut self, on: bool);
    /// a heading and the error, hidden for None
    fn set_config_error(&mut self, error: Option<(&str, &MenuLine)>);
    /// one item per `Problems` slot, hidden where there is no problem
    fn set_problems(&mut self, lines: &[Option<MenuLine>; 3]);
    fn set_tray_icon(&mut self, icon: TrayIcon);
    /// ticks every `FAST_TICK` while true, else every `SLOW_TICK`
    fn set_fast_ticks(&mut self, fast: bool);
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
    tray_icon: Option<TrayIcon>,
    fast_ticks: Option<bool>,
}

pub struct App<U: Ui> {
    pub ui: U,
    core: Core,
    config: Config,
    config_path: PathBuf,
    config_mtime: Option<SystemTime>,
    config_checked: Option<SystemTime>,
    /// the file's last error, until it loads
    config_error: Option<(ConfigError, Fallback)>,
    ipc: Server,
    problems: Problems,
    /// the user closed the warning without skipping or postponing. cleared when the warning
    /// ends
    warning_dismissed: bool,
    /// why do-not-disturb held off the last break, while omma waits for the next one
    suppressed: Option<String>,
    shown: Shown,
    /// the `--quit` that ends the process. its connection closes as the process exits,
    /// so `omma --quit` returns once omma is gone
    quitting: Option<Request>,
}

impl<U: Ui> App<U> {
    /// `config_error` tells why the file at `config_path` did not load, and `config` then
    /// holds the defaults. call `sync` once the platform's widgets are up
    pub fn new(
        mut ui: U,
        config: Config,
        config_path: PathBuf,
        config_error: Option<ConfigError>,
        ipc: Server,
        now: SystemTime,
    ) -> App<U> {
        if ui.needs_window_access(&config.do_not_disturb) {
            ui.request_window_access();
        }
        ui.config_changed(&config);
        let core = Core::new(config.schedule.clone(), now);
        let config_mtime = file_mtime(&config_path);
        let mut app = App {
            ui,
            core,
            config,
            config_path,
            config_mtime,
            config_checked: None,
            config_error: config_error.map(|e| (e, Fallback::Defaults)),
            ipc,
            problems: Problems::default(),
            warning_dismissed: false,
            suppressed: None,
            shown: Shown::default(),
            quitting: None,
        };
        app.check_window_access();
        app
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// the platform's timer: answers the CLI, picks up config changes and moves the
    /// schedule on
    pub fn tick(&mut self, now: SystemTime) {
        for request in self.ipc.drain() {
            let event = self.handle_request(now, request);
            self.react(now, event);
        }

        self.maybe_reload_config(now);
        // while the access is missing, each tick checks again, so a grant clears the item at
        // once
        if self.problems.window_access.is_some() {
            self.check_window_access();
        }

        let idle = self.ui.idle_time(now);
        let event = self.core.tick(now, idle);
        self.react(now, event);
    }

    /// runs a user action, then brings the widgets in line
    pub fn act(&mut self, now: SystemTime, action: Action) {
        let event = match action {
            Action::TakeBreak(kind) => self.core.take_break(now, kind),
            Action::ToggleEnabled => {
                let on = !self.core.is_enabled();
                self.core.set_enabled(now, on)
            }
            Action::Pause(secs) => {
                let until = match secs {
                    0 => next_midnight(now),
                    secs => now + Duration::from_secs(secs),
                };
                self.core.disable_until(now, Some(until))
            }
            Action::Skip => self.core.skip(now),
            Action::Postpone => self.core.postpone(now),
            // a press queued before the warning ended, or before the user dismissed it,
            // arrives after omma released its shortcuts. then it does nothing
            Action::Shortcut(key) => self
                .shown
                .hotkeys
                .then(|| match key {
                    WarningKey::Skip => self.core.skip(now),
                    WarningKey::Postpone => self.core.postpone(now),
                })
                .flatten(),
            Action::DismissWarning => {
                self.warning_dismissed = true;
                None
            }
            Action::Quit => {
                self.ui.quit();
                return;
            }
        };
        self.react(now, event);
    }

    /// runs a client's command, then replies with what it did
    fn handle_request(&mut self, now: SystemTime, request: Request) -> Option<Event> {
        let (event, reply) = self.run_command(now, request.command);
        request.reply(&reply);
        if request.command == Command::Quit {
            self.quitting = Some(request);
            self.ui.quit();
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
            Command::Pause(length) => {
                let event = self.core.disable_until(now, now.checked_add(length));
                (
                    event,
                    Ok(format!("paused for {}", fmt_duration(length.as_secs()))),
                )
            }
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
        let unchanged = match self.core.phase() {
            Phase::Disabled { until } => !on && until.is_none(),
            _ => on,
        };
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
    /// and the problems. each one that exists gets its own line
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
    fn react(&mut self, now: SystemTime, event: Option<Event>) {
        if let Some(event) = event {
            self.handle_event(now, event);
        }
        self.sync(now);
    }

    fn handle_event(&mut self, now: SystemTime, event: Event) {
        match event {
            Event::Warn => {
                self.check_window_access();
                let dnd = &self.config.do_not_disturb;
                self.suppressed = self.ui.suppression(dnd).map(|reason| match dnd.action {
                    DndAction::Skip => format!("Skipped the last break: {reason}"),
                    DndAction::Postpone => format!("Postponed the break: {reason}"),
                });
                if let Some(text) = &self.suppressed {
                    log!("do-not-disturb: {text}");
                    self.core.suppress(now, dnd.action);
                } else if let Some(sound) = &self.config.sounds.pre_break {
                    self.ui.play_sound(sound);
                }
            }
            Event::BreakEnded { completed: true } => {
                if let Some(sound) = &self.config.sounds.post_break {
                    self.ui.play_sound(sound);
                }
            }
            Event::BreakStarted | Event::BreakEnded { completed: false } => {}
        }
    }

    /// the only place that shows or hides the warning and the break screen. every widget
    /// follows the core's phase, and `sync` touches each one only when what it shows changes
    pub fn sync(&mut self, now: SystemTime) {
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
        // the global shortcuts follow the warning, whether or not its window opened
        if show_warning != self.shown.hotkeys {
            if show_warning {
                let refused = self.ui.hold_shortcuts(&warning_shortcuts(&self.config));
                set_problem(
                    &mut self.problems.shortcuts,
                    (!refused.is_empty()).then(|| refused.join("; ")),
                );
            } else {
                self.ui.release_shortcuts();
            }
            self.shown.hotkeys = show_warning;
        }
        if show_warning != self.ui.warning_visible() {
            if show_warning {
                let buttons = self.buttons(
                    self.ui.held_shortcut(WarningKey::Skip),
                    self.ui.held_shortcut(WarningKey::Postpone),
                );
                self.ui.show_warning(remaining.unwrap_or(0), buttons);
                self.shown.countdown = remaining;
            } else {
                // the break's bead takes over from the warning's
                self.ui.hide_warning(in_break);
            }
        }

        if in_break != self.ui.break_visible() {
            if in_break {
                let keys = self.config.keybindings;
                let buttons = self.buttons(keys.skip, keys.postpone);
                let built =
                    self.ui
                        .show_break(self.core.current(), remaining.unwrap_or(0), buttons);
                // a break gets one screen, so this runs once per break, as start_countdown_at
                // requires. building takes time, so the countdown starts from the clock after
                // the build, and `now` stays the time when the app asked for the screen
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
                self.ui.hide_break();
            }
        }

        if remaining != self.shown.countdown {
            self.shown.countdown = remaining;
            if let Some(secs) = remaining {
                if self.ui.warning_visible() {
                    self.ui.update_warning(secs);
                }
                if self.ui.break_visible() {
                    self.ui.update_break(secs);
                }
            }
        }

        self.sync_menu(now);

        let incoming = matches!(phase, Phase::Waiting { .. })
            && remaining.is_some_and(|secs| secs <= INCOMING_WINDOW_SECS);
        // with reduced motion the incoming icon holds steady instead of blinking
        let lit = incoming && (!self.ui.animations_on() || tray_icon_blink_on(now));
        let icon = if matches!(phase, Phase::Disabled { .. }) {
            TrayIcon::Disabled
        } else if lit {
            TrayIcon::Incoming
        } else {
            TrayIcon::Normal
        };
        if self.shown.tray_icon != Some(icon) {
            self.ui.set_tray_icon(icon);
            self.shown.tray_icon = Some(icon);
        }
        let fast = warning || in_break || incoming;
        if self.shown.fast_ticks != Some(fast) {
            self.ui.set_fast_ticks(fast);
            self.shown.fast_ticks = Some(fast);
        }
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
            self.ui.set_status(&status);
            self.shown.status = status;
        }

        if self.shown.suppressed != self.suppressed {
            let line = self.suppressed.as_ref().map(|text| MenuLine {
                title: shorten(text, MENU_CHARS),
                tooltip: text.clone(),
            });
            self.ui.set_suppressed(line.as_ref());
            self.shown.suppressed = self.suppressed.clone();
        }

        let enabled = self.core.is_enabled();
        if self.shown.enabled != Some(enabled) {
            self.ui.set_enabled(enabled);
            self.shown.enabled = Some(enabled);
        }

        if self.shown.config_error != self.config_error {
            match &self.config_error {
                Some((error, fallback)) => {
                    let heading = format!("⚠ Config error: {}", fallback.describe());
                    let full = error.to_string();
                    let detail = MenuLine {
                        title: shorten(&full, MENU_CHARS),
                        tooltip: full,
                    };
                    self.ui.set_config_error(Some((&heading, &detail)));
                }
                None => self.ui.set_config_error(None),
            }
            self.shown.config_error = self.config_error.clone();
        }

        if self.shown.problems != self.problems {
            self.ui.set_problems(&self.problems.lines());
            self.shown.problems = self.problems.clone();
        }
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
    /// shows the error until the file loads
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

    /// asks for window access when `new` needs it and the config it replaces did not
    fn apply_config(&mut self, new: Config) {
        self.core.reconfigure(new.schedule.clone());
        if self.ui.needs_window_access(&new.do_not_disturb)
            && !self.ui.needs_window_access(&self.config.do_not_disturb)
        {
            self.ui.request_window_access();
        }
        self.ui.config_changed(&new);
        self.config = new;
        self.problems.image = None;
        self.check_window_access();
    }

    fn check_window_access(&mut self) {
        let dnd = &self.config.do_not_disturb;
        let missing = self.ui.needs_window_access(dnd) && !self.ui.has_window_access(dnd);
        let problem = missing.then(|| U::WINDOW_ACCESS_PROBLEM.to_owned());
        set_problem(&mut self.problems.window_access, problem);
    }
}

/// the warning's global shortcuts that the schedule lets act: skip unless breaks are strict,
/// and postpone only when postponing is on. with no warning, none act
pub fn warning_shortcuts(config: &Config) -> Vec<(WarningKey, KeySpec)> {
    let keys = config.keybindings;
    let schedule = &config.schedule;
    if schedule.pre_break_warning.is_zero() {
        return Vec::new();
    }
    [
        (WarningKey::Skip, keys.warning_skip, schedule.can_skip()),
        (
            WarningKey::Postpone,
            keys.warning_postpone,
            schedule.can_postpone(),
        ),
    ]
    .into_iter()
    .filter_map(|(action, key, allowed)| Some((action, key.filter(|_| allowed)?)))
    .collect()
}

/// replaces a problem and logs it when it is new
fn set_problem(slot: &mut Option<String>, problem: Option<String>) {
    if let Some(text) = &problem
        && slot.as_ref() != Some(text)
    {
        log!("{text}");
    }
    *slot = problem;
}

fn tray_icon_blink_on(now: SystemTime) -> bool {
    now.duration_since(UNIX_EPOCH)
        .is_ok_and(|elapsed| (elapsed.as_millis() / BLINK_HALF_MS).is_multiple_of(2))
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// makes sure that the config file is there to open, and creates it from the defaults when
/// it is gone. false when the file cannot be created
pub fn ensure_config_file(path: &Path) -> bool {
    match path.try_exists() {
        Ok(true) => true,
        Ok(false) => match config::write_default(path) {
            Ok(()) => true,
            Err(e) => {
                log!("can't create {}: {e}", path.display());
                false
            }
        },
        Err(e) => {
            log!("can't check for {}: {e}", path.display());
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_shortcuts_the_schedule_lets_act_are_taken() {
        let mut config = Config::default();
        config.keybindings.warning_skip = Some("ctrl+s".parse().unwrap());
        config.keybindings.warning_postpone = Some("ctrl+p".parse().unwrap());
        let taken = |config: &Config| -> Vec<WarningKey> {
            warning_shortcuts(config)
                .into_iter()
                .map(|(k, _)| k)
                .collect()
        };
        // postponing is off by default
        assert_eq!(taken(&config), [WarningKey::Skip]);
        config.schedule.allow_postpone = true;
        assert_eq!(taken(&config), [WarningKey::Skip, WarningKey::Postpone]);
        config.schedule.strict = true;
        assert_eq!(taken(&config), []);
        // pre_break_warning = 0 shows no warning, so no shortcut is taken
        config.schedule.strict = false;
        config.schedule.pre_break_warning = Duration::ZERO;
        assert_eq!(taken(&config), []);
    }

    #[test]
    fn a_problem_line_is_capitalized_and_cut_with_its_whole_text_in_the_tooltip() {
        let long = "x".repeat(MENU_CHARS + 5);
        let problems = Problems {
            image: Some("can't show it".into()),
            shortcuts: None,
            window_access: Some(long.clone()),
        };
        let [image, shortcuts, access] = problems.lines();
        assert_eq!(
            image,
            Some(MenuLine {
                title: "⚠ Can't show it".into(),
                tooltip: "can't show it".into(),
            })
        );
        assert_eq!(shortcuts, None);
        let access = access.unwrap();
        assert_eq!(access.tooltip, long);
        assert!(access.title.ends_with('…'));
    }

    #[test]
    fn the_blink_alternates_in_whole_fast_ticks() {
        let at = |ms: u64| UNIX_EPOCH + Duration::from_millis(ms);
        assert!(tray_icon_blink_on(at(0)));
        assert!(tray_icon_blink_on(at(599)));
        assert!(!tray_icon_blink_on(at(600)));
        assert!(tray_icon_blink_on(at(1200)));
        assert!(BLINK_HALF_MS.is_multiple_of(FAST_TICK.as_millis()));
    }
}
