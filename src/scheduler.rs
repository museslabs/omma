//! the break schedule as a pure state machine, with no platform code and no clock reads.
//! each call returns the event it caused, if any. the ui follows `phase()` and uses events
//! only for side effects such as sounds.

use std::time::{Duration, SystemTime};

use crate::config::DndAction;
use crate::queue::{Break, BreakQueue, BreakType, Breaks};

/// a jump of at least this much between ticks means the mac slept or the clock moved
/// forward. it counts as rest whatever the smart pause setting.
const SLEEP_GAP: Duration = Duration::from_secs(60);

/// time arithmetic caps durations here so it can't overflow; anything longer means never
const NEVER: Duration = Duration::from_secs(100 * 365 * 24 * 60 * 60);

/// everything that shapes the schedule
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub short_interval: Duration,
    pub long_interval: Duration,
    pub pre_break_warning: Duration,
    pub postpone: Duration,
    pub allow_postpone: bool,
    pub strict: bool,
    /// idle time that pauses the schedule; None turns smart pause off
    pub idle_threshold: Option<Duration>,
    pub random_order: bool,
    pub breaks: Breaks,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Disabled,
    /// waiting since `since` to warn about the current break. the wait is the postpone delay
    /// when `postponed`, otherwise what the current break's kind calls for.
    Waiting {
        since: SystemTime,
        postponed: bool,
    },
    Warning {
        break_at: SystemTime,
    },
    Break {
        ends_at: SystemTime,
    },
    /// smart pause: no input since `since`
    Paused {
        since: SystemTime,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Warn,
    BreakStarted,
    BreakEnded { completed: bool },
}

pub struct Core {
    settings: Settings,
    queue: BreakQueue,
    /// the break being waited for, warned about or taken
    current: Break,
    phase: Phase,
    /// a long break is due one long interval after this
    long_since: SystemTime,
    /// the clock at the previous tick, to spot sleep and clock changes
    last_tick: SystemTime,
}

impl Core {
    pub fn new(settings: Settings, now: SystemTime) -> Core {
        let queue = BreakQueue::new(&settings.breaks, settings.random_order, now);
        let current = pick(&settings, &queue, now, now);
        Core {
            settings,
            queue,
            current,
            phase: Phase::Waiting {
                since: now,
                postponed: false,
            },
            long_since: now,
            last_tick: now,
        }
    }

    /// applies new settings without restarting the schedule. each wait keeps its start, so a
    /// changed interval counts from when the wait began, and a deadline that has already
    /// passed fires on the next tick. a postponed break stays unless the break lists or
    /// `random_order` changed. a warning or break under way keeps its break and deadline.
    pub fn reconfigure(&mut self, settings: Settings) {
        if settings == self.settings {
            return;
        }
        let rebuild = settings.breaks != self.settings.breaks
            || settings.random_order != self.settings.random_order;
        if rebuild {
            self.queue = BreakQueue::new(&settings.breaks, settings.random_order, self.last_tick);
        }
        self.settings = settings;
        if let Phase::Waiting { since, postponed } = self.phase
            && (rebuild || !postponed)
        {
            self.current = pick(&self.settings, &self.queue, self.long_since, since);
        }
    }

    /// advances the schedule to `now`; `idle` is the time since the last user input.
    ///
    /// rest is time away from the screen. it's idle time past the smart pause threshold,
    /// which pauses the schedule, or a jump of `SLEEP_GAP` or more between ticks. once the
    /// user is back, the next break waits at least a full short interval. rest at least as
    /// long as the next long break counts as that break. a clock moved back moves every
    /// deadline back with it.
    #[must_use]
    pub fn tick(&mut self, now: SystemTime, idle: Duration) -> Option<Event> {
        self.follow_clock(now);
        let gap = now.duration_since(self.last_tick).unwrap_or_default();
        self.last_tick = now;

        let slept = gap >= SLEEP_GAP;
        let idle_away = self.settings.idle_threshold.is_some_and(|t| idle >= t);

        match self.phase {
            Phase::Disabled => None,
            Phase::Paused { since } => {
                if !idle_away {
                    let back = earlier(now, idle);
                    self.end_rest(now, back.duration_since(since).unwrap_or_default());
                }
                None
            }
            // idling through a break is the point of it; only sleep counts here
            Phase::Break { ends_at } => {
                if slept {
                    self.credit(now, gap);
                }
                (now >= ends_at).then(|| {
                    self.finish_current(now);
                    Event::BreakEnded { completed: true }
                })
            }
            Phase::Waiting { .. } | Phase::Warning { .. } if idle_away || slept => {
                // no input arrives during a sleep, which the idle counter may not include
                let away = if slept { gap.max(idle) } else { idle };
                if idle_away {
                    self.phase = Phase::Paused {
                        since: earlier(now, away),
                    };
                } else {
                    self.end_rest(now, away);
                }
                None
            }
            Phase::Waiting { since, postponed } => {
                (now >= self.warn_at(since, postponed)).then(|| {
                    self.phase = Phase::Warning {
                        break_at: later(now, self.settings.pre_break_warning),
                    };
                    Event::Warn
                })
            }
            Phase::Warning { break_at } => (now >= break_at).then(|| self.start_break(now)),
        }
    }

    /// moves on past the warned or running break. ignored in other phases and in strict mode.
    #[must_use]
    pub fn skip(&mut self, now: SystemTime) -> Option<Event> {
        if !self.can_skip() || !self.break_due() {
            return None;
        }
        self.follow_clock(now);
        let event = self.interrupted();
        self.finish_current(now);
        event
    }

    /// retries the warned or running break after the postpone delay. ignored in other
    /// phases and when the settings don't allow postponing.
    #[must_use]
    pub fn postpone(&mut self, now: SystemTime) -> Option<Event> {
        if !self.can_postpone() || !self.break_due() {
            return None;
        }
        self.follow_clock(now);
        let event = self.interrupted();
        self.defer(now);
        event
    }

    /// do-not-disturb holding back a warned break. the system acts here, not the user, so
    /// the skip and postpone settings don't apply.
    pub fn suppress(&mut self, now: SystemTime, action: DndAction) {
        if !matches!(self.phase, Phase::Warning { .. }) {
            return;
        }
        self.follow_clock(now);
        match action {
            DndAction::Skip => self.finish_current(now),
            DndAction::Postpone => self.defer(now),
        }
    }

    /// starts a break at once with no warning: the current break, or the next of `kind`.
    /// ignored during a break and while disabled. a smart pause ends first, as if the tick
    /// that notices the user is back had come before the click.
    #[must_use]
    pub fn take_break(&mut self, now: SystemTime, kind: Option<BreakType>) -> Option<Event> {
        if matches!(self.phase, Phase::Disabled | Phase::Break { .. }) {
            return None;
        }
        self.follow_clock(now);
        if let Phase::Paused { since } = self.phase {
            self.end_rest(now, now.duration_since(since).unwrap_or_default());
        }
        if let Some(kind) = kind {
            self.current = self.queue.peek(kind).clone();
        }
        Some(self.start_break(now))
    }

    /// disabling ends a running break uncompleted. enabling starts the schedule afresh.
    #[must_use]
    pub fn set_enabled(&mut self, now: SystemTime, on: bool) -> Option<Event> {
        self.follow_clock(now);
        match (on, self.phase) {
            (true, Phase::Disabled) => {
                self.long_since = now;
                self.schedule(now);
                None
            }
            (true, _) => None,
            (false, phase) => {
                self.phase = Phase::Disabled;
                matches!(phase, Phase::Break { .. })
                    .then_some(Event::BreakEnded { completed: false })
            }
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn current(&self) -> &Break {
        &self.current
    }

    pub fn is_enabled(&self) -> bool {
        self.phase != Phase::Disabled
    }

    pub fn can_skip(&self) -> bool {
        !self.settings.strict
    }

    pub fn can_postpone(&self) -> bool {
        self.settings.allow_postpone && !self.settings.strict
    }

    /// whole seconds, rounded up, until the phase's deadline: the warning, the break or
    /// the break's end. a break counts down from its length at most, so a countdown that
    /// `start_countdown_at` started late shows its first second for longer.
    pub fn remaining(&self, now: SystemTime) -> Option<u64> {
        match self.phase {
            Phase::Waiting { since, postponed } => {
                Some(ceil_secs(now, self.warn_at(since, postponed)))
            }
            Phase::Warning { break_at } => Some(ceil_secs(now, break_at)),
            Phase::Break { ends_at } => {
                Some(ceil_secs(now, ends_at).min(whole_secs(self.current.duration)))
            }
            Phase::Disabled | Phase::Paused { .. } => None,
        }
    }

    /// starts the running break's countdown at `at`, when its screen has come into focus,
    /// so every second of it is on screen. the break then ends one length after `at`.
    /// `now` is when the screen was asked for. ignored outside a break, and when the
    /// countdown had moved past its first second by `now`. call it once per break, since
    /// each call in the first second moves the end again.
    pub fn start_countdown_at(&mut self, now: SystemTime, at: SystemTime) {
        self.follow_clock(now);
        let length = self.current.duration;
        if let Phase::Break { ends_at } = &mut self.phase
            && ceil_secs(now, *ends_at) >= whole_secs(length)
        {
            *ends_at = (*ends_at).max(later(at, length));
        }
    }

    pub fn status_text(&self, now: SystemTime) -> String {
        match self.phase {
            Phase::Disabled => "Disabled".into(),
            Phase::Paused { .. } => "Paused (idle)".into(),
            Phase::Warning { .. } => "Break soon".into(),
            Phase::Break { .. } => "Break in progress".into(),
            Phase::Waiting { since, postponed } => {
                match ceil_secs(now, self.warn_at(since, postponed)) {
                    0 => "Next break shortly".into(),
                    secs => format!(
                        "Next break in {} - {}",
                        fmt_duration(secs),
                        self.current.name
                    ),
                }
            }
        }
    }

    fn warn_at(&self, since: SystemTime, postponed: bool) -> SystemTime {
        if postponed {
            later(since, self.settings.postpone)
        } else if self.current.kind == BreakType::Long {
            self.long_due().max(since)
        } else {
            later(since, self.settings.short_interval)
        }
    }

    fn long_due(&self) -> SystemTime {
        later(self.long_since, self.settings.long_interval)
    }

    fn break_due(&self) -> bool {
        matches!(self.phase, Phase::Warning { .. } | Phase::Break { .. })
    }

    /// the event for leaving Warning or Break early
    fn interrupted(&self) -> Option<Event> {
        matches!(self.phase, Phase::Break { .. }).then_some(Event::BreakEnded { completed: false })
    }

    fn start_break(&mut self, now: SystemTime) -> Event {
        self.phase = Phase::Break {
            ends_at: later(now, self.current.duration),
        };
        Event::BreakStarted
    }

    /// the current break is over, taken or skipped. waits for the next one.
    fn finish_current(&mut self, now: SystemTime) {
        self.queue.advance(self.current.kind);
        if self.current.kind == BreakType::Long {
            self.long_since = now;
        }
        self.schedule(now);
    }

    /// the same break again after the postpone delay
    fn defer(&mut self, now: SystemTime) {
        self.phase = Phase::Waiting {
            since: now,
            postponed: true,
        };
    }

    fn schedule(&mut self, now: SystemTime) {
        self.current = pick(&self.settings, &self.queue, self.long_since, now);
        self.phase = Phase::Waiting {
            since: now,
            postponed: false,
        };
    }

    /// the user is back after `rest` away. any rest earns a full short interval before the
    /// next break, so an overdue long break waits that long too.
    fn end_rest(&mut self, now: SystemTime, rest: Duration) {
        self.credit(now, rest);
        let long_since_floor = earlier(
            later(now, self.settings.short_interval),
            self.settings.long_interval,
        );
        self.long_since = self.long_since.max(long_since_floor);
        self.schedule(now);
    }

    /// rest at least as long as the next long break counts as that break
    fn credit(&mut self, now: SystemTime, rest: Duration) {
        if self.queue.has(BreakType::Long) && rest >= self.queue.peek(BreakType::Long).duration {
            self.long_since = now;
        }
    }

    /// when the clock went back, moves every stored time back with it so deadlines keep
    /// their distance from `now`
    fn follow_clock(&mut self, now: SystemTime) {
        let Ok(back) = self.last_tick.duration_since(now) else {
            return;
        };
        let shift = |t: &mut SystemTime| *t = earlier(*t, back);
        shift(&mut self.last_tick);
        shift(&mut self.long_since);
        match &mut self.phase {
            Phase::Waiting { since, .. } | Phase::Paused { since } => shift(since),
            Phase::Warning { break_at: t } | Phase::Break { ends_at: t } => shift(t),
            Phase::Disabled => {}
        }
    }
}

/// the break to wait for from `since`. it's a long one when it falls due before the next
/// short one would be over, so the long takes that short's place instead of following right
/// after it.
/// a wait that starts the long cycle afresh always ends in a short, so shorts can't starve
/// however long they take.
fn pick(
    settings: &Settings,
    queue: &BreakQueue,
    long_since: SystemTime,
    since: SystemTime,
) -> Break {
    let long = !queue.has(BreakType::Short)
        || (queue.has(BreakType::Long) && long_since < since && {
            let short_takes = settings
                .short_interval
                .saturating_add(settings.pre_break_warning)
                .saturating_add(queue.peek(BreakType::Short).duration);
            later(long_since, settings.long_interval) <= later(since, short_takes)
        });
    let kind = if long {
        BreakType::Long
    } else {
        BreakType::Short
    };
    queue.peek(kind).clone()
}

fn later(t: SystemTime, d: Duration) -> SystemTime {
    t.checked_add(d.min(NEVER)).unwrap_or(t)
}

fn earlier(t: SystemTime, d: Duration) -> SystemTime {
    t.checked_sub(d.min(NEVER)).unwrap_or(t)
}

/// whole seconds from `now` to `t`, rounded up; 0 once `t` has passed
fn ceil_secs(now: SystemTime, t: SystemTime) -> u64 {
    t.duration_since(now).map_or(0, whole_secs)
}

/// rounded up
fn whole_secs(d: Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

fn fmt_duration(secs: u64) -> String {
    let (m, s) = (secs / 60, secs % 60);
    if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::*;
    use crate::config::Config;
    use crate::queue::brk;
    use BreakType::{Long, Short};

    const BREAK_ENDED: Option<Event> = Some(Event::BreakEnded { completed: true });
    const BREAK_CUT: Option<Event> = Some(Event::BreakEnded { completed: false });

    /// seconds from an arbitrary base, far enough from the epoch to set the clock back
    fn at(s: u64) -> SystemTime {
        at_ms(s * 1000)
    }

    fn at_ms(ms: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_000_000) + Duration::from_millis(ms)
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    /// shorts every 60 s, a long due every 300 s, a 2 s warning, a 30 s postpone
    fn settings() -> Settings {
        Settings {
            short_interval: secs(60),
            long_interval: secs(300),
            pre_break_warning: secs(2),
            postpone: secs(30),
            allow_postpone: true,
            strict: false,
            idle_threshold: None,
            random_order: false,
            breaks: Breaks::new(
                vec![brk(Short, "s0", 3), brk(Short, "s1", 3)],
                vec![brk(Long, "L", 6)],
            )
            .unwrap(),
        }
    }

    /// ticks once a second since the last tick, as the app's timer would, then at `now`, with
    /// no idle time. returns the event at `now`; an event before it fails the test.
    fn step(core: &mut Core, now: SystemTime) -> Option<Event> {
        loop {
            let t = core.last_tick + secs(1);
            if t >= now {
                return core.tick(now, Duration::ZERO);
            }
            assert_eq!(core.tick(t, Duration::ZERO), None, "event before {now:?}");
        }
    }

    /// s0 started at 62 and ends at 65
    fn in_break() -> Core {
        let mut core = Core::new(settings(), at(0));
        assert_eq!(step(&mut core, at(60)), Some(Event::Warn));
        assert_eq!(step(&mut core, at(62)), Some(Event::BreakStarted));
        core
    }

    /// ticks once a second over `from..to` with no idle time; returns each break that
    /// started, with the second it started at
    fn breaks_between(core: &mut Core, from: u64, to: u64) -> Vec<(u64, Break)> {
        let mut started = Vec::new();
        for s in from..to {
            if step(core, at(s)) == Some(Event::BreakStarted) {
                started.push((s, core.current().clone()));
            }
        }
        started
    }

    fn kinds(breaks: &[(u64, Break)]) -> Vec<BreakType> {
        breaks.iter().map(|(_, b)| b.kind).collect()
    }

    fn starts(breaks: &[(u64, Break)]) -> Vec<u64> {
        breaks.iter().map(|(s, _)| *s).collect()
    }

    #[test]
    fn warns_then_breaks_then_waits_again() {
        let mut core = Core::new(settings(), at(0));
        assert_eq!(core.remaining(at(0)), Some(60));
        assert_eq!(step(&mut core, at(59)), None);

        assert_eq!(step(&mut core, at(60)), Some(Event::Warn));
        assert_eq!(core.remaining(at(60)), Some(2));
        assert_eq!(core.remaining(at_ms(60_001)), Some(2));
        assert_eq!(core.remaining(at(61)), Some(1));

        assert_eq!(step(&mut core, at(62)), Some(Event::BreakStarted));
        assert_eq!(core.current().name, "s0");
        assert_eq!(core.remaining(at(62)), Some(3));
        assert_eq!(step(&mut core, at(63)), None);
        assert_eq!(step(&mut core, at(64)), None);
        assert_eq!(step(&mut core, at(65)), BREAK_ENDED);

        assert!(matches!(core.phase(), Phase::Waiting { .. }));
        assert_eq!(core.current().name, "s1");
        assert_eq!(core.remaining(at(65)), Some(60));
    }

    #[test]
    fn late_tick_does_not_make_the_countdown_lag() {
        let mut core = in_break();
        // a late fire, with the run loop busy drawing the overlay, still shows "2"
        assert_eq!(core.tick(at_ms(63_400), Duration::ZERO), None);
        assert_eq!(core.remaining(at_ms(63_400)), Some(2));
        // and the next on-time fire shows "1"
        assert_eq!(core.tick(at(64), Duration::ZERO), None);
        assert_eq!(core.remaining(at(64)), Some(1));
        assert_eq!(core.tick(at(65), Duration::ZERO), BREAK_ENDED);
    }

    #[test]
    fn a_countdown_started_late_holds_its_first_second_and_ends_later() {
        let mut core = in_break();
        core.start_countdown_at(at_ms(62_050), at_ms(62_600));
        assert_eq!(core.remaining(at(62)), Some(3));
        assert_eq!(core.remaining(at_ms(63_500)), Some(3));
        assert_eq!(core.remaining(at_ms(63_600)), Some(2));
        assert_eq!(step(&mut core, at(65)), None);
        assert_eq!(core.remaining(at(65)), Some(1));
        assert_eq!(core.tick(at_ms(65_600), Duration::ZERO), BREAK_ENDED);
    }

    #[test]
    fn a_screen_slow_to_build_still_gets_its_first_second() {
        let mut core = in_break();
        core.start_countdown_at(at(62), at_ms(63_800));
        assert_eq!(core.remaining(at_ms(63_800)), Some(3));
        assert_eq!(core.remaining(at_ms(64_800)), Some(2));
        assert_eq!(step(&mut core, at(66)), None);
        assert_eq!(core.tick(at_ms(66_800), Duration::ZERO), BREAK_ENDED);
    }

    #[test]
    fn a_countdown_already_under_way_keeps_its_deadline() {
        // a screen built mid-break, once a display appears, doesn't wind the count back up
        let mut core = in_break();
        core.start_countdown_at(at_ms(63_200), at_ms(63_800));
        assert_eq!(core.remaining(at_ms(63_200)), Some(2));
        assert_eq!(step(&mut core, at(65)), BREAK_ENDED);

        // and there is no countdown to start outside a break
        let mut core = Core::new(settings(), at(0));
        core.start_countdown_at(at(0), at(5));
        assert_eq!(core.remaining(at(0)), Some(60));
        assert_eq!(step(&mut core, at(60)), Some(Event::Warn));
        core.start_countdown_at(at(60), at(65));
        assert_eq!(core.remaining(at(60)), Some(2));
        assert_eq!(step(&mut core, at(62)), Some(Event::BreakStarted));
        assert_eq!(step(&mut core, at(65)), BREAK_ENDED);
    }

    #[test]
    fn countdown_catches_up_after_a_stalled_run_loop() {
        let mut core = in_break();
        // no ticks from 62 until 64.2: the first one after the stall shows "1", not "3"
        assert_eq!(core.tick(at_ms(64_200), Duration::ZERO), None);
        assert_eq!(core.remaining(at_ms(64_200)), Some(1));
        assert_eq!(core.tick(at(65), Duration::ZERO), BREAK_ENDED);

        // and a stall past the end ends the break on its first tick
        let mut core = in_break();
        assert_eq!(core.tick(at(66), Duration::ZERO), BREAK_ENDED);
    }

    #[test]
    fn skip_and_postpone_only_act_on_a_warned_or_running_break() {
        let mut core = Core::new(settings(), at(0));
        assert_eq!(core.skip(at(10)), None);
        assert_eq!(core.postpone(at(10)), None);
        assert_eq!(core.current().name, "s0");
        assert_eq!(core.remaining(at(10)), Some(50));

        assert_eq!(core.set_enabled(at(20), false), None);
        assert_eq!(core.skip(at(21)), None);
        assert_eq!(core.postpone(at(21)), None);
        core.suppress(at(21), DndAction::Skip);
        assert_eq!(core.phase(), Phase::Disabled);
    }

    #[test]
    fn actions_while_paused_leave_the_pause_alone() {
        let mut core = Core::new(
            Settings {
                idle_threshold: Some(secs(60)),
                ..settings()
            },
            at(0),
        );
        assert_eq!(core.tick(at(1), secs(61)), None);
        let paused = core.phase();
        assert!(matches!(paused, Phase::Paused { .. }));
        assert_eq!(core.skip(at(2)), None);
        assert_eq!(core.postpone(at(2)), None);
        core.suppress(at(2), DndAction::Postpone);
        assert_eq!(core.phase(), paused);
        assert_eq!(core.current().name, "s0");
    }

    #[test]
    fn suppress_only_acts_on_a_warning() {
        let mut core = Core::new(settings(), at(0));
        core.suppress(at(10), DndAction::Skip);
        assert_eq!(core.current().name, "s0");
        assert_eq!(core.remaining(at(10)), Some(50));

        let mut core = in_break();
        core.suppress(at(63), DndAction::Postpone);
        assert!(matches!(core.phase(), Phase::Break { .. }));
    }

    #[test]
    fn a_skip_right_after_a_break_leaves_the_next_one_alone() {
        let mut core = in_break();
        assert_eq!(step(&mut core, at(65)), BREAK_ENDED);
        // a second Esc or a double click lands on the fading break screen
        assert_eq!(core.skip(at_ms(65_200)), None);
        // the next break warns at 125, starts at 127 and runs its full 3 s
        assert_eq!(step(&mut core, at(125)), Some(Event::Warn));
        assert_eq!(step(&mut core, at(127)), Some(Event::BreakStarted));
        assert_eq!(step(&mut core, at(129)), None);
        assert_eq!(step(&mut core, at(130)), BREAK_ENDED);
    }

    #[test]
    fn a_late_skip_after_postponing_keeps_the_postponed_break() {
        let mut core = Core::new(settings(), at(0));
        assert_eq!(step(&mut core, at(60)), Some(Event::Warn));
        assert_eq!(core.postpone(at(61)), None);
        // a late click on the fading warning's Skip
        assert_eq!(core.skip(at(61)), None);
        assert_eq!(core.remaining(at(61)), Some(30));
        assert_eq!(step(&mut core, at(91)), Some(Event::Warn));
        assert_eq!(core.current().name, "s0");
    }

    #[test]
    fn postpone_retries_the_same_break_later() {
        let mut core = in_break();
        assert_eq!(core.postpone(at(63)), BREAK_CUT);
        assert!(matches!(core.phase(), Phase::Waiting { .. }));
        assert_eq!(core.current().name, "s0");
        assert_eq!(core.remaining(at(63)), Some(30));
        assert_eq!(step(&mut core, at(93)), Some(Event::Warn));
        assert_eq!(core.current().name, "s0");
    }

    #[test]
    fn skip_moves_on_to_the_next_break() {
        let mut core = Core::new(settings(), at(0));
        assert_eq!(step(&mut core, at(60)), Some(Event::Warn));
        assert_eq!(core.skip(at(61)), None);
        assert_eq!(core.current().name, "s1");
        assert_eq!(core.remaining(at(61)), Some(60));

        assert_eq!(step(&mut core, at(121)), Some(Event::Warn));
        assert_eq!(step(&mut core, at(123)), Some(Event::BreakStarted));
        assert_eq!(core.skip(at(124)), BREAK_CUT);
        assert_eq!(core.current().name, "s0");
    }

    #[test]
    fn do_not_disturb_postpones_or_skips_a_warning() {
        let mut core = Core::new(settings(), at(0));
        assert_eq!(step(&mut core, at(60)), Some(Event::Warn));
        core.suppress(at(60), DndAction::Postpone);
        assert_eq!(core.current().name, "s0");
        assert_eq!(core.remaining(at(60)), Some(30));

        assert_eq!(step(&mut core, at(90)), Some(Event::Warn));
        core.suppress(at(90), DndAction::Skip);
        assert_eq!(core.current().name, "s1");
        assert_eq!(core.remaining(at(90)), Some(60));
    }

    #[test]
    fn strict_mode_ignores_skip_and_postpone_but_not_do_not_disturb() {
        let mut core = Core::new(
            Settings {
                strict: true,
                ..settings()
            },
            at(0),
        );
        assert!(!core.can_skip());
        assert!(!core.can_postpone());

        assert_eq!(step(&mut core, at(60)), Some(Event::Warn));
        assert_eq!(core.skip(at(61)), None);
        assert_eq!(core.postpone(at(61)), None);
        assert!(matches!(core.phase(), Phase::Warning { .. }));

        core.suppress(at(61), DndAction::Skip);
        assert_eq!(core.current().name, "s1");

        assert_eq!(step(&mut core, at(121)), Some(Event::Warn));
        core.suppress(at(121), DndAction::Postpone);
        assert_eq!(core.remaining(at(121)), Some(30));

        assert_eq!(step(&mut core, at(151)), Some(Event::Warn));
        assert_eq!(step(&mut core, at(153)), Some(Event::BreakStarted));
        assert_eq!(core.skip(at(154)), None);
        assert_eq!(core.postpone(at(154)), None);
        assert!(matches!(core.phase(), Phase::Break { .. }));
    }

    #[test]
    fn postpone_needs_allow_postpone() {
        let mut core = Core::new(
            Settings {
                allow_postpone: false,
                ..settings()
            },
            at(0),
        );
        assert!(core.can_skip());
        assert!(!core.can_postpone());
        assert_eq!(step(&mut core, at(60)), Some(Event::Warn));
        assert_eq!(core.postpone(at(61)), None);
        assert!(matches!(core.phase(), Phase::Warning { .. }));
    }

    #[test]
    fn take_break_starts_the_current_break_or_one_of_the_asked_kind() {
        let mut core = Core::new(settings(), at(0));
        assert_eq!(
            core.take_break(at(5), Some(Long)),
            Some(Event::BreakStarted)
        );
        assert_eq!(core.current().name, "L");
        assert_eq!(core.remaining(at(5)), Some(6));
        assert_eq!(step(&mut core, at(11)), BREAK_ENDED);

        assert_eq!(core.take_break(at(12), None), Some(Event::BreakStarted));
        assert_eq!(core.current().name, "s0");
    }

    #[test]
    fn take_break_while_paused_ends_the_pause_first() {
        let paused = || {
            let mut core = Core::new(
                Settings {
                    idle_threshold: Some(secs(60)),
                    ..settings()
                },
                at(0),
            );
            for s in 1..600 {
                assert_eq!(core.tick(at(s), secs(s)), None);
            }
            assert!(matches!(core.phase(), Phase::Paused { .. }));
            core
        };
        // the user is back and clicks before the tick that would notice
        let mut clicked = paused();
        assert_eq!(
            clicked.take_break(at(600), Some(Short)),
            Some(Event::BreakStarted)
        );
        let mut ticked = paused();
        assert_eq!(ticked.tick(at(600), Duration::ZERO), None);
        assert_eq!(
            ticked.take_break(at(600), Some(Short)),
            Some(Event::BreakStarted)
        );
        // either way the 600 s away stood in for the long break
        for core in [&mut clicked, &mut ticked] {
            assert_eq!(step(core, at(603)), BREAK_ENDED);
            assert_eq!(core.current().kind, Short);
            assert_eq!(core.remaining(at(603)), Some(60));
        }
    }

    #[test]
    fn take_break_during_a_break_does_not_chain_another() {
        let mut core = in_break();
        assert_eq!(core.take_break(at(63), None), None);
        assert_eq!(step(&mut core, at(65)), BREAK_ENDED);
        assert!(breaks_between(&mut core, 66, 125).is_empty());
    }

    #[test]
    fn take_break_while_disabled_does_nothing_now_or_after_enabling() {
        let mut core = Core::new(settings(), at(0));
        assert_eq!(core.set_enabled(at(1), false), None);
        assert_eq!(core.take_break(at(2), None), None);
        assert_eq!(core.phase(), Phase::Disabled);
        let on = 3 * 3600;
        assert!(breaks_between(&mut core, 3, on).is_empty());

        assert_eq!(core.set_enabled(at(on), true), None);
        assert_eq!(core.remaining(at(on)), Some(60));
        assert_eq!(step(&mut core, at(on + 60)), Some(Event::Warn));
    }

    #[test]
    fn disabling_during_a_break_ends_it_uncompleted() {
        let mut core = in_break();
        assert_eq!(core.set_enabled(at(63), false), BREAK_CUT);
        assert_eq!(core.phase(), Phase::Disabled);
        assert_eq!(core.remaining(at(63)), None);

        // clicks on the fading break screen while disabled
        assert_eq!(core.skip(at(64)), None);
        assert_eq!(core.postpone(at(64)), None);

        assert_eq!(core.set_enabled(at(100), true), None);
        assert!(matches!(core.phase(), Phase::Waiting { .. }));
        assert_eq!(core.remaining(at(100)), Some(60));
        // the first break after enabling runs in full
        assert_eq!(step(&mut core, at(160)), Some(Event::Warn));
        assert_eq!(step(&mut core, at(162)), Some(Event::BreakStarted));
        assert_eq!(step(&mut core, at(165)), BREAK_ENDED);
    }

    #[test]
    fn waking_after_hours_waits_a_full_interval_and_credits_the_long_break() {
        let mut core = Core::new(settings(), at(0));
        assert_eq!(step(&mut core, at(10)), None);
        // asleep from 10 s to 5 h, woken with a key press
        let wake = 5 * 3600;
        assert_eq!(core.tick(at(wake), Duration::ZERO), None);
        assert_eq!(core.remaining(at(wake)), Some(60));
        // the long break is a full long interval after the wake, not overdue
        let breaks = breaks_between(&mut core, wake + 1, wake + 310);
        assert_eq!(kinds(&breaks), [Short, Short, Short, Short, Long]);
        assert_eq!(breaks[0].0, wake + 62);
        assert_eq!(breaks[4].0, wake + 302);
    }

    #[test]
    fn waking_before_any_input_still_credits_the_sleep() {
        let mut core = Core::new(settings(), at(0));
        assert_eq!(kinds(&breaks_between(&mut core, 1, 100)), [Short]);
        // asleep from 100 s to 200 s; the first tick after waking comes before any key press,
        // and the idle counter reads 190 s
        assert_eq!(core.tick(at(200), secs(190)), None);
        let breaks = breaks_between(&mut core, 201, 600);
        let long = breaks.iter().find(|(_, b)| b.kind == Long).unwrap().0;
        assert_eq!(long, 200 + 302);
    }

    #[test]
    fn waking_without_input_pauses_until_the_user_is_back() {
        let mut core = Core::new(
            Settings {
                idle_threshold: Some(secs(60)),
                ..settings()
            },
            at(0),
        );
        assert_eq!(step(&mut core, at(10)), None);
        // the idle counter kept running through the sleep
        let wake = 5 * 3600;
        assert_eq!(core.tick(at(wake), secs(wake - 5)), None);
        assert!(matches!(core.phase(), Phase::Paused { .. }));
        assert_eq!(core.status_text(at(wake)), "Paused (idle)");
        assert_eq!(core.tick(at(wake + 30), secs(wake + 25)), None);
        assert!(matches!(core.phase(), Phase::Paused { .. }));

        assert_eq!(core.tick(at(wake + 31), Duration::ZERO), None);
        assert_eq!(core.remaining(at(wake + 31)), Some(60));
    }

    #[test]
    fn setting_the_clock_forward_during_a_warning_counts_as_rest() {
        let mut core = Core::new(settings(), at(0));
        assert_eq!(step(&mut core, at(60)), Some(Event::Warn));
        let jump = 60 + 2 * 3600;
        assert_eq!(core.tick(at(jump), Duration::ZERO), None);
        assert!(matches!(core.phase(), Phase::Waiting { .. }));
        assert_eq!(core.remaining(at(jump)), Some(60));
    }

    #[test]
    fn a_short_gap_is_just_time_passing() {
        let mut core = Core::new(settings(), at(0));
        assert_eq!(step(&mut core, at(50)), None);
        // a 30 s stall: shorter than SLEEP_GAP, so the overdue warning comes, in full
        assert_eq!(core.tick(at(80), Duration::ZERO), Some(Event::Warn));
        assert_eq!(core.remaining(at(80)), Some(2));
    }

    #[test]
    fn sleep_counts_as_rest_below_a_longer_idle_threshold() {
        let mut core = Core::new(
            Settings {
                idle_threshold: Some(secs(300)),
                ..settings()
            },
            at(0),
        );
        assert_eq!(step(&mut core, at(10)), None);
        // asleep across the warning time, woken by a key press 2 s before the tick
        assert_eq!(core.tick(at(160), secs(2)), None);
        assert!(matches!(core.phase(), Phase::Waiting { .. }));
        assert_eq!(core.remaining(at(160)), Some(60));
    }

    #[test]
    fn a_stall_shorter_than_sleep_is_not_rest_whatever_the_threshold() {
        for threshold in [1, 5] {
            let mut core = Core::new(
                Settings {
                    idle_threshold: Some(secs(threshold)),
                    ..settings()
                },
                at(0),
            );
            assert_eq!(step(&mut core, at(50)), None);
            // a 6 s main-thread stall while the user types
            assert_eq!(core.tick(at(56), Duration::ZERO), None);
            assert_eq!(core.remaining(at(56)), Some(4));
            assert_eq!(step(&mut core, at(60)), Some(Event::Warn));
        }
    }

    #[test]
    fn an_overdue_long_break_waits_a_full_short_interval_after_rest() {
        let mut core = Core::new(
            Settings {
                breaks: Breaks::new(vec![brk(Short, "s", 3)], vec![brk(Long, "L", 300)]).unwrap(),
                ..settings()
            },
            at(0),
        );
        assert_eq!(
            starts(&breaks_between(&mut core, 1, 280)),
            [62, 127, 192, 257]
        );
        assert_eq!(core.current().kind, Long);
        // asleep 130 s across the long's warning at 300: too short to stand in for it
        assert_eq!(core.tick(at(410), Duration::ZERO), None);
        assert_eq!(core.current().kind, Long);
        assert_eq!(core.remaining(at(410)), Some(60));
        assert_eq!(step(&mut core, at(470)), Some(Event::Warn));
    }

    #[test]
    fn a_long_due_during_the_next_short_takes_its_place() {
        let mut core = Core::new(
            Settings {
                short_interval: secs(1200),
                long_interval: secs(3670),
                pre_break_warning: secs(10),
                breaks: Breaks::new(vec![brk(Short, "s", 15)], vec![brk(Long, "L", 60)]).unwrap(),
                ..settings()
            },
            at(0),
        );
        // the third short would end at 3675, after the long falls due at 3670
        let breaks = breaks_between(&mut core, 1, 3700);
        assert_eq!(kinds(&breaks), [Short, Short, Long]);
        assert_eq!(starts(&breaks), [1210, 2435, 3680]);
    }

    #[test]
    fn sleeping_through_a_break_ends_it_and_credits_the_rest() {
        let mut core = in_break();
        let wake = 4 * 3600;
        assert_eq!(core.tick(at(wake), Duration::ZERO), BREAK_ENDED);
        assert_eq!(core.current().kind, Short);
        assert_eq!(core.remaining(at(wake)), Some(60));
    }

    #[test]
    fn idling_through_a_break_does_not_pause_it() {
        let mut core = Core::new(
            Settings {
                idle_threshold: Some(secs(60)),
                ..settings()
            },
            at(0),
        );
        assert_eq!(core.take_break(at(1), None), Some(Event::BreakStarted));
        assert_eq!(core.tick(at(2), secs(1000)), None);
        assert!(matches!(core.phase(), Phase::Break { .. }));
        assert_eq!(core.tick(at(4), secs(1000)), BREAK_ENDED);
    }

    #[test]
    fn walking_away_during_a_warning_pauses_and_reschedules() {
        let mut core = Core::new(
            Settings {
                idle_threshold: Some(secs(60)),
                pre_break_warning: secs(10),
                ..settings()
            },
            at(0),
        );
        // the last input was at 5 s
        for s in 1..60 {
            assert_eq!(core.tick(at(s), secs(s.saturating_sub(5))), None);
        }
        assert_eq!(core.tick(at(60), secs(55)), Some(Event::Warn));
        for s in 61..65 {
            assert_eq!(core.tick(at(s), secs(s - 5)), None);
        }
        assert_eq!(core.tick(at(65), secs(60)), None);
        assert!(matches!(core.phase(), Phase::Paused { .. }));
        assert_eq!(core.tick(at(66), Duration::ZERO), None);
        assert_eq!(core.remaining(at(66)), Some(60));
    }

    /// the user is idle from 0 s until `back`, with smart pause after `threshold` seconds and
    /// a 60 s long break; returns when the first long break starts
    fn first_long_after_idle(threshold: u64, back: u64) -> u64 {
        let mut core = Core::new(
            Settings {
                idle_threshold: Some(secs(threshold)),
                breaks: Breaks::new(vec![brk(Short, "s", 3)], vec![brk(Long, "L", 60)]).unwrap(),
                ..settings()
            },
            at(0),
        );
        for s in 1..back {
            assert_eq!(core.tick(at(s), secs(s)), None);
        }
        assert!(matches!(core.phase(), Phase::Paused { .. }));
        let breaks = breaks_between(&mut core, back, back + 1000);
        breaks.iter().find(|(_, b)| b.kind == Long).unwrap().0
    }

    #[test]
    fn idle_credit_counts_the_time_before_the_pause() {
        // 59 s away is short of the 60 s long break: the long keeps its slot at 300 + 2
        assert_eq!(first_long_after_idle(30, 59), 302);
        // 60 s is enough: the long comes a full long interval after the user is back
        assert_eq!(first_long_after_idle(30, 60), 60 + 302);
        // threshold 60 and 120 s idle in all
        assert_eq!(first_long_after_idle(60, 120), 120 + 302);
    }

    #[test]
    fn the_defaults_give_four_shorts_then_a_long() {
        let mut core = Core::new(Config::default().schedule, at(0));
        let breaks = breaks_between(&mut core, 1, 9100);
        assert_eq!(
            kinds(&breaks),
            [
                Short, Short, Short, Short, Long, Short, Short, Short, Short, Long
            ]
        );
        // the first long warns at 75 min; the next 75 min after the first one ends
        assert_eq!(starts(&breaks)[4], 4500 + 10);
        assert_eq!(starts(&breaks)[9], 4570 + 4500 + 10);
    }

    #[test]
    fn after_idle_credit_the_defaults_give_four_shorts_then_a_long() {
        let mut core = Core::new(Config::default().schedule, at(0));
        assert_eq!(kinds(&breaks_between(&mut core, 1, 1000)), [Short]);
        // idle from 1000 s, paused at 1060, back at 1200
        for s in 1000..1200 {
            assert_eq!(core.tick(at(s), secs(s - 1000)), None);
        }
        let back = 1200;
        let breaks = breaks_between(&mut core, back, back + 4600);
        assert_eq!(kinds(&breaks), [Short, Short, Short, Short, Long]);
        assert_eq!(starts(&breaks)[4], back + 4500 + 10);
    }

    #[test]
    fn every_short_comes_up_when_the_intervals_are_close() {
        let mut core = Core::new(
            Settings {
                short_interval: secs(60),
                long_interval: secs(100),
                breaks: Breaks::new(
                    vec![
                        brk(Short, "s0", 3),
                        brk(Short, "water", 3),
                        brk(Short, "s2", 3),
                    ],
                    vec![brk(Long, "L", 6)],
                )
                .unwrap(),
                ..settings()
            },
            at(0),
        );
        let breaks = breaks_between(&mut core, 1, 1000);
        let names: Vec<_> = breaks.iter().map(|(_, b)| b.name.as_str()).collect();
        assert_eq!(names[..6], ["s0", "L", "water", "L", "s2", "L"]);
        assert!(
            kinds(&breaks)[..16]
                .chunks(2)
                .all(|pair| pair == [Short, Long])
        );
    }

    #[test]
    fn shorts_come_up_even_when_one_takes_longer_than_the_gap_to_the_long() {
        // a short's interval, warning and 38 s duration add up to the 100 s long interval
        let mut core = Core::new(
            Settings {
                short_interval: secs(60),
                long_interval: secs(100),
                breaks: Breaks::new(vec![brk(Short, "s", 38)], vec![brk(Long, "L", 6)]).unwrap(),
                ..settings()
            },
            at(0),
        );
        let breaks = breaks_between(&mut core, 1, 2000);
        assert!(
            kinds(&breaks)[..10]
                .chunks(2)
                .all(|pair| pair == [Short, Long])
        );
    }

    #[test]
    fn a_manual_short_break_leaves_the_long_due_time_alone() {
        let mut core = Core::new(settings(), at(0));
        assert_eq!(
            starts(&breaks_between(&mut core, 1, 270)),
            [62, 127, 192, 257]
        );
        assert_eq!(core.current().kind, Long);
        // the long warns at 300; a short taken at 270 doesn't push it back
        assert_eq!(
            core.take_break(at(270), Some(Short)),
            Some(Event::BreakStarted)
        );
        let breaks = breaks_between(&mut core, 271, 310);
        assert_eq!(kinds(&breaks), [Long]);
        assert_eq!(breaks[0].0, 302);
    }

    #[test]
    fn reconfiguring_keeps_when_the_wait_began() {
        let mut core = Core::new(settings(), at(0));
        assert_eq!(step(&mut core, at(40)), None);

        // a change that doesn't touch this wait leaves the countdown alone
        core.reconfigure(Settings {
            postpone: secs(99),
            ..settings()
        });
        assert_eq!(core.remaining(at(40)), Some(20));
        assert_eq!(core.current().name, "s0");

        // a longer interval counts from when the wait began
        core.reconfigure(Settings {
            short_interval: secs(90),
            ..settings()
        });
        assert_eq!(core.remaining(at(40)), Some(50));

        // a shorter one that has already run out warns on the next tick
        core.reconfigure(Settings {
            short_interval: secs(30),
            ..settings()
        });
        assert_eq!(core.remaining(at(40)), Some(0));
        assert_eq!(step(&mut core, at(41)), Some(Event::Warn));
    }

    fn new_breaks() -> Settings {
        Settings {
            breaks: Breaks::new(vec![brk(Short, "n0", 5), brk(Short, "n1", 5)], vec![]).unwrap(),
            ..settings()
        }
    }

    #[test]
    fn reconfiguring_the_breaks_restarts_the_queue() {
        let mut core = Core::new(settings(), at(0));
        assert_eq!(step(&mut core, at(60)), Some(Event::Warn));
        assert_eq!(core.skip(at(61)), None);
        assert_eq!(core.current().name, "s1");

        core.reconfigure(new_breaks());
        assert_eq!(core.current().name, "n0");
        assert_eq!(core.remaining(at(61)), Some(60));
    }

    #[test]
    fn reconfiguring_the_breaks_replaces_a_postponed_break_but_keeps_the_delay() {
        let mut core = Core::new(settings(), at(0));
        assert_eq!(step(&mut core, at(60)), Some(Event::Warn));
        assert_eq!(core.postpone(at(61)), None);
        core.reconfigure(new_breaks());
        assert_eq!(core.current().name, "n0");
        assert_eq!(core.remaining(at(61)), Some(30));

        let breaks = breaks_between(&mut core, 62, 300);
        let names: Vec<_> = breaks.iter().map(|(_, b)| b.name.as_str()).collect();
        assert_eq!(names[..3], ["n0", "n1", "n0"]);
    }

    #[test]
    fn reconfiguring_keeps_a_pause() {
        let mut core = Core::new(
            Settings {
                idle_threshold: Some(secs(60)),
                ..settings()
            },
            at(0),
        );
        assert_eq!(core.tick(at(1), secs(61)), None);
        let paused = core.phase();
        core.reconfigure(Settings {
            idle_threshold: Some(secs(60)),
            ..new_breaks()
        });
        assert_eq!(core.phase(), paused);
    }

    #[test]
    fn huge_intervals_never_fire_and_never_panic() {
        let huge = Duration::MAX;
        let mut core = Core::new(
            Settings {
                short_interval: huge,
                long_interval: huge,
                postpone: huge,
                ..settings()
            },
            at(0),
        );
        assert_eq!(step(&mut core, at(1)), None);
        assert!(core.remaining(at(1)).unwrap() > 100 * 365 * 24 * 3600 - 10);
        assert!(core.status_text(at(1)).starts_with("Next break in"));

        let mut core = Core::new(
            Settings {
                pre_break_warning: huge,
                breaks: Breaks::new(vec![brk(Short, "s", u64::MAX)], vec![]).unwrap(),
                ..settings()
            },
            at(0),
        );
        assert_eq!(step(&mut core, at(60)), Some(Event::Warn));
        assert_eq!(step(&mut core, at(61)), None);
        assert_eq!(core.take_break(at(62), None), Some(Event::BreakStarted));
        assert_eq!(step(&mut core, at(63)), None);
    }

    #[test]
    fn setting_the_clock_back_during_a_break_keeps_its_length() {
        let mut core = Core::new(settings(), at(3590));
        assert_eq!(core.take_break(at(3600), None), Some(Event::BreakStarted));
        assert_eq!(step(&mut core, at(3601)), None);
        // the clock goes back an hour
        assert_eq!(step(&mut core, at(2)), None);
        assert_eq!(core.remaining(at(2)), Some(2));
        assert_eq!(step(&mut core, at(3)), None);
        assert_eq!(step(&mut core, at(4)), BREAK_ENDED);
    }

    #[test]
    fn setting_the_clock_back_while_waiting_keeps_the_countdown() {
        let mut core = Core::new(settings(), at(5000));
        assert_eq!(step(&mut core, at(5030)), None);
        assert_eq!(step(&mut core, at(1430)), None);
        assert_eq!(core.remaining(at(1430)), Some(30));
        assert_eq!(step(&mut core, at(1460)), Some(Event::Warn));
    }

    #[test]
    fn an_action_after_the_clock_went_back_keeps_its_deadline() {
        let mut core = Core::new(settings(), at(5000));
        assert_eq!(step(&mut core, at(5001)), None);
        // the clock goes back an hour before the next tick
        assert_eq!(core.take_break(at(1401), None), Some(Event::BreakStarted));
        assert_eq!(step(&mut core, at(1402)), None);
        assert_eq!(core.remaining(at(1402)), Some(2));
        assert_eq!(step(&mut core, at(1404)), BREAK_ENDED);
    }

    #[test]
    fn status_text_follows_the_phase() {
        let mut core = Core::new(
            Settings {
                idle_threshold: Some(secs(60)),
                ..settings()
            },
            at(0),
        );
        assert_eq!(core.status_text(at(0)), "Next break in 1m 00s - s0");
        assert_eq!(core.status_text(at_ms(59_500)), "Next break in 1s - s0");
        assert_eq!(core.status_text(at(60)), "Next break shortly");
        assert_eq!(step(&mut core, at(60)), Some(Event::Warn));
        assert_eq!(core.status_text(at(60)), "Break soon");
        assert_eq!(step(&mut core, at(62)), Some(Event::BreakStarted));
        assert_eq!(core.status_text(at(62)), "Break in progress");
        assert_eq!(step(&mut core, at(65)), BREAK_ENDED);
        assert_eq!(core.tick(at(66), secs(60)), None);
        assert_eq!(core.status_text(at(66)), "Paused (idle)");
        assert_eq!(core.set_enabled(at(67), false), None);
        assert_eq!(core.status_text(at(67)), "Disabled");
    }
}
