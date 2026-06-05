use std::time::SystemTime;

use crate::backend::{Backend, BreakOverlay, Countdown, PreBreakDismissal, PreBreakNotice};
use crate::config::{self, Config, DndAction};
use crate::ipc::Command;
use crate::model::BreakType;
use crate::scheduler::{Core, CoreEvent, State};

pub trait CommandServer {
    fn drain(&self) -> Vec<Command>;
    fn set_status(&self, status: String);
}

pub struct Controller<B: Backend, S: CommandServer> {
    core: Core,
    config: Config,
    server: S,
    backend: B,
    smart_pause_enabled: bool,
    idle_threshold: f64,
    config_mtime: Option<SystemTime>,
}

impl<B: Backend, S: CommandServer> Controller<B, S> {
    pub fn new(config: Config, server: S, backend: B) -> anyhow::Result<Self> {
        let queue = config.build_queue()?;
        let mut core = Core::new(
            queue,
            config.pre_break_warning,
            config.postpone_duration,
            config.allow_postpone,
            config.strict_break,
        );
        core.start(SystemTime::now());

        let smart_pause_enabled = config.smart_pause.enabled;
        let idle_threshold = config.smart_pause.idle_seconds as f64;
        let config_mtime = file_mtime(&config::config_path());

        Ok(Controller {
            core,
            config,
            server,
            backend,
            smart_pause_enabled,
            idle_threshold,
            config_mtime,
        })
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    pub fn on_tick(&mut self, now: SystemTime, target: &B::ActionTarget) {
        for cmd in self.server.drain() {
            self.handle_command(cmd);
        }

        self.maybe_reload_config(now);
        self.apply_smart_pause(now, target);

        let events = self.core.tick(now);
        self.handle_events(now, events, target);

        if matches!(self.core.state(), State::PreBreak) {
            if let Some(remaining) = self.core.seconds_until_break_start(now) {
                self.backend.update_countdown(Countdown::PreBreak {
                    seconds_until_break: remaining,
                });
            }
        }

        self.refresh_status(now);
    }

    pub fn take_break(&mut self, kind: Option<BreakType>) {
        self.core.take_break(kind);
    }

    pub fn toggle_enabled(&mut self, now: SystemTime) {
        if self.core.is_enabled() {
            self.backend.dismiss_pre_break(PreBreakDismissal::Cancelled);
            self.backend.hide_break();
            self.core.disable();
        } else {
            let _ = self.core.enable(now);
        }
        self.backend.refresh_menu_state(self.core.is_enabled());
    }

    pub fn skip_break(&mut self) {
        self.backend.dismiss_pre_break(PreBreakDismissal::Cancelled);
        self.backend.hide_break();
        self.core.skip();
    }

    pub fn postpone_break(&mut self) {
        self.backend.dismiss_pre_break(PreBreakDismissal::Cancelled);
        self.backend.hide_break();
        self.core.postpone();
    }

    pub fn skip_warning(&mut self, now: SystemTime, target: &B::ActionTarget) {
        self.suppress_warning(now, true, target);
    }

    pub fn postpone_warning(&mut self, now: SystemTime, target: &B::ActionTarget) {
        self.suppress_warning(now, false, target);
    }

    pub fn dismiss_warning(&mut self) {
        self.backend.dismiss_pre_break(PreBreakDismissal::Cancelled);
    }

    pub fn open_config_file(&self) {
        self.backend.open_config_file();
    }

    pub fn quit(&mut self) {
        self.backend.dismiss_pre_break(PreBreakDismissal::Cancelled);
        self.backend.hide_break();
        self.backend.quit();
    }

    fn suppress_warning(&mut self, now: SystemTime, skip: bool, target: &B::ActionTarget) {
        self.backend.dismiss_pre_break(PreBreakDismissal::Cancelled);
        let events = self.core.suppress_break(now, skip);
        self.handle_events(now, events, target);
        self.refresh_status(now);
    }

    fn maybe_reload_config(&mut self, now: SystemTime) {
        if matches!(self.core.state(), State::PreBreak | State::Break) {
            return;
        }

        let path = config::config_path();
        let mtime = file_mtime(&path);
        if mtime == self.config_mtime {
            return;
        }
        self.config_mtime = mtime;

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

        if let Err(e) = self.apply_config(new, now) {
            eprintln!("omma: config not reloaded: {e}");
            return;
        }
        eprintln!("omma: reloaded config");
    }

    fn apply_config(&mut self, new: Config, now: SystemTime) -> anyhow::Result<()> {
        let queue = new.build_queue()?;

        let was_enabled = self.core.is_enabled();
        let mut core = Core::new(
            queue,
            new.pre_break_warning,
            new.postpone_duration,
            new.allow_postpone,
            new.strict_break,
        );
        if was_enabled {
            let _ = core.start(now);
        } else {
            core.disable();
        }
        self.core = core;

        self.backend.config_updated(&new);
        if !new.notification.enabled {
            self.backend.dismiss_pre_break(PreBreakDismissal::Cancelled);
        }
        self.smart_pause_enabled = new.smart_pause.enabled;
        self.idle_threshold = new.smart_pause.idle_seconds as f64;
        self.config = new;
        self.backend.refresh_menu_state(self.core.is_enabled());
        Ok(())
    }

    fn apply_smart_pause(&mut self, now: SystemTime, target: &B::ActionTarget) {
        if !self.smart_pause_enabled || !self.core.is_enabled() {
            return;
        }
        let idle = self.backend.idle_seconds();
        if idle >= self.idle_threshold {
            if matches!(self.core.state(), State::PreBreak) {
                self.backend.dismiss_pre_break(PreBreakDismissal::Cancelled);
            }
            self.core.pause_for_idle(now);
        } else if matches!(self.core.state(), State::Resting) {
            let events = self.core.resume_from_idle(now);
            self.handle_events(now, events, target);
        }
    }

    fn handle_events(&mut self, now: SystemTime, events: Vec<CoreEvent>, target: &B::ActionTarget) {
        for ev in events {
            self.handle_event(now, ev, target);
        }
    }

    fn handle_event(&mut self, now: SystemTime, ev: CoreEvent, target: &B::ActionTarget) {
        match ev {
            CoreEvent::NextBreakScheduled { .. } => {}
            CoreEvent::PreBreak { name, .. } => {
                if self.backend.should_suppress_break() {
                    let skip = self.config.do_not_disturb.action == DndAction::Skip;
                    let events = self.core.suppress_break(now, skip);
                    self.backend.dismiss_pre_break(PreBreakDismissal::Cancelled);
                    self.handle_events(now, events, target);
                    return;
                }

                if self.config.notification.enabled {
                    let skip_key = self.config.keybindings.skip.clone();
                    let postpone_key = self.config.keybindings.postpone.clone();
                    let allow_skip = !self.config.strict_break;
                    let allow_postpone = self.config.allow_postpone && !self.config.strict_break;
                    let seconds_until_break = self
                        .core
                        .seconds_until_break_start(now)
                        .unwrap_or(self.config.pre_break_warning);
                    self.backend.show_pre_break(
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

                let alert = self.config.audible_alert.clone();
                if alert.enabled && alert.pre_break {
                    self.backend.play_sound(&alert.pre_break_sound);
                }
            }
            CoreEvent::StartBreak {
                name,
                duration,
                allow_skip,
                allow_postpone,
                image,
                ..
            } => {
                self.backend
                    .dismiss_pre_break(PreBreakDismissal::BreakStarting);
                let skip_key = self.config.keybindings.skip.clone();
                let postpone_key = self.config.keybindings.postpone.clone();
                self.backend.show_break(
                    BreakOverlay {
                        name: &name,
                        duration,
                        allow_skip,
                        allow_postpone,
                        skip_key: &skip_key,
                        postpone_key: &postpone_key,
                        image: image.as_deref(),
                    },
                    target,
                );
            }
            CoreEvent::Countdown { remaining, total } => {
                self.backend
                    .update_countdown(Countdown::Break { remaining, total });
            }
            CoreEvent::StopBreak => {
                self.backend.dismiss_pre_break(PreBreakDismissal::Cancelled);
                self.backend.hide_break();
                let alert = self.config.audible_alert.clone();
                if alert.enabled && alert.post_break {
                    self.backend.play_sound(&alert.post_break_sound);
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
                self.backend.refresh_menu_state(self.core.is_enabled());
            }
            Command::Disable => {
                self.backend.dismiss_pre_break(PreBreakDismissal::Cancelled);
                self.backend.hide_break();
                self.core.disable();
                self.backend.refresh_menu_state(self.core.is_enabled());
            }
            Command::Quit => self.quit(),
            Command::Status => {}
            Command::Reload => self.config_mtime = None,
        }
    }

    fn refresh_status(&mut self, now: SystemTime) {
        let snapshot = self.status_snapshot(now);
        self.backend.refresh_status(now, &snapshot);
        self.server.set_status(snapshot.text);
    }

    fn status_snapshot(&self, now: SystemTime) -> crate::backend::StatusSnapshot {
        let enabled = self.core.is_enabled();
        let state = self.core.state();
        let seconds_until_next = self.core.seconds_until_next(now);
        let current_break_name = self.core.current_break_name().to_string();
        let text = self.status_text(now);

        crate::backend::StatusSnapshot {
            enabled,
            state,
            seconds_until_next,
            current_break_name,
            text,
        }
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
                    "Next break in {} - {}",
                    fmt_duration(s),
                    self.core.current_break_name()
                ),
                _ => "Next break shortly".to_string(),
            },
        }
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl CommandServer for crate::ipc::Server {
    fn drain(&self) -> Vec<Command> {
        crate::ipc::Server::drain(self)
    }

    fn set_status(&self, status: String) {
        crate::ipc::Server::set_status(self, status);
    }
}
