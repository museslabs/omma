use std::time::SystemTime;

use crate::config::Config;
use crate::scheduler::State;

pub struct PreBreakNotice<'a> {
    pub name: &'a str,
    pub allow_skip: bool,
    pub allow_postpone: bool,
    pub seconds_until_break: i64,
    pub skip_key: &'a str,
    pub postpone_key: &'a str,
}

pub struct BreakOverlay<'a> {
    pub name: &'a str,
    pub duration: i64,
    pub allow_skip: bool,
    pub allow_postpone: bool,
    pub skip_key: &'a str,
    pub postpone_key: &'a str,
    pub image: Option<&'a str>,
}

pub enum PreBreakDismissal {
    Cancelled,
    BreakStarting,
}

pub enum Countdown {
    PreBreak { seconds_until_break: i64 },
    Break { remaining: i64, total: i64 },
}

pub struct StatusSnapshot {
    pub enabled: bool,
    pub state: State,
    pub seconds_until_next: Option<i64>,
    pub current_break_name: String,
    pub text: String,
}

pub trait Backend {
    type ActionTarget: ?Sized;

    fn config_updated(&mut self, config: &Config);

    fn show_pre_break(&mut self, notice: PreBreakNotice<'_>, target: &Self::ActionTarget);
    fn dismiss_pre_break(&mut self, dismissal: PreBreakDismissal);

    fn show_break(&mut self, overlay: BreakOverlay<'_>, target: &Self::ActionTarget);
    fn update_countdown(&mut self, countdown: Countdown);
    fn hide_break(&mut self);

    fn refresh_menu_state(&mut self, enabled: bool);
    fn refresh_status(&mut self, now: SystemTime, status: &StatusSnapshot);

    fn play_sound(&self, name: &str);
    fn idle_seconds(&self) -> f64;
    fn should_suppress_break(&self) -> bool;
    fn open_config_file(&self);
    fn quit(&mut self);
}

#[cfg(target_os = "macos")]
pub mod macos;
