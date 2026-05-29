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
}

pub trait Backend {
    type ActionTarget: ?Sized;

    fn show_pre_break(&mut self, notice: PreBreakNotice<'_>, target: &Self::ActionTarget);
    fn update_pre_break_countdown(&mut self, seconds_until_break: i64);
    fn hide_pre_break(&mut self);
    fn hide_pre_break_for_break(&mut self);

    fn show_break(&mut self, overlay: BreakOverlay<'_>, target: &Self::ActionTarget);
    fn update_break_countdown(&mut self, remaining: i64, total: i64);
    fn hide_break(&mut self);

    fn play_sound(&self, name: &str);
    fn idle_seconds(&self) -> f64;
    fn should_suppress_break(&self) -> bool;
    fn open_config_file(&self);
}

#[cfg(target_os = "macos")]
pub mod macos;
