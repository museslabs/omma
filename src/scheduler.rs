//! The break scheduler: a deterministic, tick-driven state machine.
//!
//! [`Core`] owns no platform code and no threads. The UI layer calls
//! [`Core::tick`] with the current time; the core
//! advances its state and returns the [`CoreEvent`]s the UI should act on
//! (show a warning, show the break screen, update the countdown, …). User
//! actions (skip, postpone, take a break now, enable/disable) and environment
//! signals (idle, do-not-disturb) are fed in as method calls and take effect
//! on the next tick.

use std::time::{Duration, SystemTime};

use crate::model::{BreakQueue, BreakType};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// Counting down to the next break.
    Waiting,
    /// The pre-break warning is showing.
    PreBreak,
    /// A break screen is on display.
    Break,
    /// Disabled by the user.
    Stopped,
    /// Paused because the user is idle (smart pause).
    Resting,
}

/// Things the UI should do in response to a tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreEvent {
    /// A break has been scheduled `seconds_until` from now.
    NextBreakScheduled {
        name: String,
        kind: BreakType,
        seconds_until: i64,
    },
    /// Show the pre-break warning for the upcoming break.
    PreBreak { name: String, kind: BreakType },
    /// Put up the break screen.
    StartBreak {
        name: String,
        kind: BreakType,
        image: Option<String>,
        duration: i64,
        allow_skip: bool,
        allow_postpone: bool,
    },
    /// Update the countdown shown on the break screen.
    Countdown { remaining: i64, total: i64 },
    /// Take the break screen down (it ran to completion).
    StopBreak,
}

pub struct Core {
    queue: BreakQueue,
    state: State,

    pre_break_warning: i64, // seconds
    postpone_duration: i64, // seconds
    allow_postpone: bool,
    strict_break: bool,

    // Scheduled instants.
    warn_at: Option<SystemTime>,
    break_at: Option<SystemTime>,

    // Break countdown.
    countdown_total: i64,
    countdown_remaining: i64,
    next_count_at: Option<SystemTime>,

    // Pending user actions, consumed on the next tick.
    pending_skip: bool,
    pending_postpone: bool,
    pending_take_break: Option<Option<BreakType>>,

    // Smart pause.
    paused_at: Option<SystemTime>,
}

impl Core {
    pub fn new(
        queue: BreakQueue,
        pre_break_warning: i64,
        postpone_duration: i64,
        allow_postpone: bool,
        strict_break: bool,
    ) -> Core {
        Core {
            queue,
            state: State::Waiting,
            pre_break_warning: pre_break_warning.max(0),
            postpone_duration: postpone_duration.max(1),
            allow_postpone,
            strict_break,
            warn_at: None,
            break_at: None,
            countdown_total: 0,
            countdown_remaining: 0,
            next_count_at: None,
            pending_skip: false,
            pending_postpone: false,
            pending_take_break: None,
            paused_at: None,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn is_enabled(&self) -> bool {
        !matches!(self.state, State::Stopped)
    }

    /// Begin counting down to the first break. Returns the scheduling event.
    pub fn start(&mut self, now: SystemTime) -> Vec<CoreEvent> {
        let mut ev = Vec::new();
        let wait = self.queue.current().interval;
        self.schedule(now, wait, &mut ev);
        ev
    }

    /// Seconds until the next break warning fires, for status display.
    pub fn seconds_until_next(&self, now: SystemTime) -> Option<i64> {
        self.warn_at.map(|w| duration_secs_ceil(now, w))
    }

    /// Seconds until the fullscreen break starts while the warning is visible.
    pub fn seconds_until_break_start(&self, now: SystemTime) -> Option<i64> {
        self.break_at.map(|b| duration_secs_ceil(now, b))
    }

    pub fn current_break_name(&self) -> &str {
        &self.queue.current().name
    }

    // --- user actions -----------------------------------------------------

    pub fn skip(&mut self) {
        self.pending_skip = true;
    }

    pub fn postpone(&mut self) {
        if self.allow_postpone && !self.strict_break {
            self.pending_postpone = true;
        }
    }

    pub fn take_break(&mut self, kind: Option<BreakType>) {
        self.pending_take_break = Some(kind);
    }

    pub fn disable(&mut self) {
        self.state = State::Stopped;
        self.warn_at = None;
        self.break_at = None;
        self.next_count_at = None;
    }

    pub fn enable(&mut self, now: SystemTime) -> Vec<CoreEvent> {
        let mut ev = Vec::new();
        if matches!(self.state, State::Stopped | State::Resting) {
            let wait = self.queue.current().interval;
            self.schedule(now, wait, &mut ev);
        }
        ev
    }

    /// Suppress the break we're currently waiting for or warning about
    /// (do-not-disturb). `skip` advances to the next break; otherwise the same
    /// break is retried after the postpone delay.
    pub fn suppress_break(&mut self, now: SystemTime, skip: bool) -> Vec<CoreEvent> {
        let mut ev = Vec::new();
        if !matches!(self.state, State::Waiting | State::PreBreak) {
            return ev;
        }
        if skip {
            self.queue.advance(None);
            let wait = self.queue.current().interval;
            self.schedule(now, wait, &mut ev);
        } else {
            self.schedule(now, self.postpone_duration, &mut ev);
        }
        ev
    }

    // --- smart pause ------------------------------------------------------

    /// Pause because the user went idle. Only meaningful while waiting.
    pub fn pause_for_idle(&mut self, now: SystemTime) {
        if matches!(self.state, State::Waiting | State::PreBreak) {
            self.paused_at = Some(now);
            self.state = State::Resting;
            self.warn_at = None;
            self.break_at = None;
        }
    }

    /// Resume after idle. A long enough idle period stands in for the next
    /// long break, so we skip it.
    pub fn resume_from_idle(&mut self, now: SystemTime) -> Vec<CoreEvent> {
        let mut ev = Vec::new();
        if !matches!(self.state, State::Resting) {
            return ev;
        }
        if let (Some(paused), Some(long_dur)) = (self.paused_at, self.queue.next_long_duration()) {
            let paused_secs = duration_secs(paused, now);
            if paused_secs > long_dur {
                self.queue.skip_long_break();
            }
        }
        self.paused_at = None;
        let wait = self.queue.current().interval;
        self.schedule(now, wait, &mut ev);
        ev
    }

    // --- the tick ---------------------------------------------------------

    pub fn tick(&mut self, now: SystemTime) -> Vec<CoreEvent> {
        let mut ev = Vec::new();

        // A "take break now" request short-circuits any waiting state.
        if matches!(self.state, State::Waiting | State::PreBreak) {
            if let Some(kind) = self.pending_take_break.take() {
                self.begin_break(now, kind, &mut ev);
                return ev;
            }
        }

        match self.state {
            State::Stopped | State::Resting => {}
            State::Waiting => {
                if let Some(warn) = self.warn_at {
                    if now >= warn {
                        self.state = State::PreBreak;
                        let b = self.queue.current();
                        ev.push(CoreEvent::PreBreak {
                            name: b.name.clone(),
                            kind: b.kind,
                        });
                    }
                }
            }
            State::PreBreak => {
                if let Some(bt) = self.break_at {
                    if now >= bt {
                        self.start_break(now, &mut ev);
                    }
                }
            }
            State::Break => {
                if self.pending_skip {
                    self.pending_skip = false;
                    // The UI closes its own screen when Skip is pressed, so we
                    // just advance — no StopBreak.
                    self.end_break(now, false, &mut ev);
                } else if self.pending_postpone {
                    self.pending_postpone = false;
                    self.postpone_now(now, &mut ev);
                } else if self.next_count_at.is_some() {
                    self.advance_countdown(now, &mut ev);
                }
            }
        }

        ev
    }

    // --- internal helpers -------------------------------------------------

    /// Schedule the current break `wait_secs` from `now` without advancing the
    /// queue.
    fn schedule(&mut self, now: SystemTime, wait_secs: i64, ev: &mut Vec<CoreEvent>) {
        let wait = wait_secs.max(0) as u64;
        let warn = now + Duration::from_secs(wait);
        self.warn_at = Some(warn);
        self.break_at = Some(warn + Duration::from_secs(self.pre_break_warning.max(0) as u64));
        self.next_count_at = None;
        self.state = State::Waiting;

        let b = self.queue.current();
        ev.push(CoreEvent::NextBreakScheduled {
            name: b.name.clone(),
            kind: b.kind,
            seconds_until: wait_secs.max(0),
        });
    }

    /// Step the break countdown forward to `now`.
    ///
    /// Deadlines sit on a fixed one-second cadence anchored to the break's
    /// start (start+1, start+2, …); we decrement once per whole second elapsed
    /// since the last tick. Earlier code re-anchored the next deadline to `now`
    /// — the timer's actual, jittery fire time — so a single late fire pushed
    /// the deadline past the following fire; that fire saw `now < nc`, did
    /// nothing, and the digit visibly lingered for ~2s while the countdown fell
    /// behind real time. Looping over the fixed cadence keeps each digit on
    /// screen for one second and lets a late or coalesced tick catch up instead
    /// of stalling.
    fn advance_countdown(&mut self, now: SystemTime, ev: &mut Vec<CoreEvent>) {
        let mut changed = false;
        while self.next_count_at.is_some_and(|nc| now >= nc) {
            self.countdown_remaining -= 1;
            self.next_count_at = self.next_count_at.map(|nc| nc + Duration::from_secs(1));
            changed = true;
            if self.countdown_remaining <= 0 {
                self.end_break(now, true, ev);
                return;
            }
        }
        if changed {
            ev.push(CoreEvent::Countdown {
                remaining: self.countdown_remaining,
                total: self.countdown_total,
            });
        }
    }

    fn start_break(&mut self, now: SystemTime, ev: &mut Vec<CoreEvent>) {
        self.state = State::Break;
        let b = self.queue.current().clone();
        self.countdown_total = b.duration.max(1);
        self.countdown_remaining = self.countdown_total;
        self.next_count_at = Some(now + Duration::from_secs(1));

        ev.push(CoreEvent::StartBreak {
            name: b.name,
            kind: b.kind,
            image: b.image,
            duration: self.countdown_total,
            allow_skip: !self.strict_break,
            allow_postpone: self.allow_postpone && !self.strict_break,
        });
        ev.push(CoreEvent::Countdown {
            remaining: self.countdown_remaining,
            total: self.countdown_total,
        });
    }

    /// Start a break immediately (used by "take a break now"), skipping the
    /// warning.
    fn begin_break(&mut self, now: SystemTime, kind: Option<BreakType>, ev: &mut Vec<CoreEvent>) {
        if let Some(k) = kind {
            if self.queue.current().kind != k {
                self.queue.advance(Some(k));
            }
        }
        self.start_break(now, ev);
    }

    fn end_break(&mut self, now: SystemTime, ran_to_completion: bool, ev: &mut Vec<CoreEvent>) {
        if ran_to_completion {
            ev.push(CoreEvent::StopBreak);
        }
        self.next_count_at = None;
        self.queue.advance(None);
        let wait = self.queue.current().interval;
        self.schedule(now, wait, ev);
    }

    fn postpone_now(&mut self, now: SystemTime, ev: &mut Vec<CoreEvent>) {
        // Same break, shown again after the postpone delay.
        self.next_count_at = None;
        self.schedule(now, self.postpone_duration, ev);
    }
}

fn duration_secs(from: SystemTime, to: SystemTime) -> i64 {
    match to.duration_since(from) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

fn duration_secs_ceil(from: SystemTime, to: SystemTime) -> i64 {
    match to.duration_since(from) {
        Ok(d) => {
            let secs = d.as_secs() as i64;
            if d.subsec_nanos() > 0 {
                secs + 1
            } else {
                secs
            }
        }
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Break;

    fn brk(kind: BreakType, name: &str, interval: i64, duration: i64) -> Break {
        Break {
            kind,
            name: name.into(),
            interval,
            duration,
            image: None,
        }
    }

    fn core_with(strict: bool, allow_postpone: bool) -> Core {
        let q = BreakQueue::create(
            60,  // short interval (seconds)
            300, // long interval (seconds)
            false,
            vec![brk(BreakType::Short, "look away", 60, 3)],
            vec![brk(BreakType::Long, "stand up", 300, 6)],
        )
        .unwrap();
        // pre_break_warning = 2s, postpone = 300s.
        Core::new(q, 2, 300, allow_postpone, strict)
    }

    fn at(base: SystemTime, secs: u64) -> SystemTime {
        base + Duration::from_secs(secs)
    }

    fn at_ms(base: SystemTime, ms: u64) -> SystemTime {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn schedules_warns_starts_and_counts_down() {
        let base = SystemTime::UNIX_EPOCH;
        let mut core = core_with(false, true);

        let ev = core.start(base);
        assert!(matches!(ev[0], CoreEvent::NextBreakScheduled { seconds_until: 60, .. }));
        assert_eq!(core.state(), State::Waiting);

        // Nothing happens before the 60-second warning.
        assert!(core.tick(at(base, 59)).is_empty());

        // Warning fires at 60s.
        let ev = core.tick(at(base, 60));
        assert!(matches!(ev[0], CoreEvent::PreBreak { .. }));
        assert_eq!(core.state(), State::PreBreak);
        assert_eq!(core.seconds_until_break_start(at(base, 60)), Some(2));
        assert_eq!(core.seconds_until_break_start(at_ms(base, 60_001)), Some(2));
        assert_eq!(core.seconds_until_break_start(at(base, 61)), Some(1));
        assert_eq!(core.seconds_until_break_start(at_ms(base, 61_001)), Some(1));

        // Break starts 2s later (pre_break_warning = 2).
        let ev = core.tick(at(base, 62));
        assert!(matches!(ev[0], CoreEvent::StartBreak { duration: 3, allow_skip: true, .. }));
        assert_eq!(core.state(), State::Break);

        // Countdown ticks each second; ends after `duration` seconds.
        core.tick(at(base, 63));
        core.tick(at(base, 64));
        let ev = core.tick(at(base, 65));
        assert!(ev.iter().any(|e| matches!(e, CoreEvent::StopBreak)));
        // Next break re-scheduled afterwards.
        assert!(ev
            .iter()
            .any(|e| matches!(e, CoreEvent::NextBreakScheduled { .. })));
        assert_eq!(core.state(), State::Waiting);
    }

    #[test]
    fn late_tick_does_not_make_the_countdown_lag() {
        let base = SystemTime::UNIX_EPOCH;
        let mut core = core_with(false, true);
        core.start(base);
        core.tick(at(base, 60)); // PreBreak
        core.tick(at(base, 62)); // break starts (duration 3): deadlines 63/64/65

        // A fire that lands late (the run loop was busy drawing the overlay)
        // still shows "2"…
        let ev = core.tick(at_ms(base, 63_400));
        assert!(matches!(ev.as_slice(), [CoreEvent::Countdown { remaining: 2, .. }]));

        // …and the next on-schedule fire at 64s must show "1", not linger on
        // "2". The old code re-anchored the deadline to 64.4s, so this tick saw
        // now < deadline and emitted nothing — the digit hung for a full extra
        // second and the countdown fell behind the wall clock.
        let ev = core.tick(at(base, 64));
        assert!(matches!(ev.as_slice(), [CoreEvent::Countdown { remaining: 1, .. }]));

        // The break still ends exactly at 65s — three seconds after it began.
        let ev = core.tick(at(base, 65));
        assert!(ev.iter().any(|e| matches!(e, CoreEvent::StopBreak)));
        assert_eq!(core.state(), State::Waiting);
    }

    #[test]
    fn countdown_catches_up_after_a_stalled_run_loop() {
        let base = SystemTime::UNIX_EPOCH;
        let mut core = core_with(false, true);
        core.start(base);
        core.tick(at(base, 60));
        core.tick(at(base, 62)); // break starts (duration 3), ends at 65

        // No ticks fire between 63 and 65 (the main thread was blocked). A
        // single catch-up tick runs the countdown to completion rather than
        // stalling on a stale "3".
        let ev = core.tick(at(base, 65));
        assert!(ev.iter().any(|e| matches!(e, CoreEvent::StopBreak)));
        assert_eq!(core.state(), State::Waiting);
    }

    #[test]
    fn skip_ends_break_without_stop_event() {
        let base = SystemTime::UNIX_EPOCH;
        let mut core = core_with(false, true);
        core.start(base);
        core.tick(at(base, 60));
        core.tick(at(base, 62)); // break started
        core.skip();
        let ev = core.tick(at(base, 62));
        assert!(!ev.iter().any(|e| matches!(e, CoreEvent::StopBreak)));
        assert!(ev
            .iter()
            .any(|e| matches!(e, CoreEvent::NextBreakScheduled { .. })));
        assert_eq!(core.state(), State::Waiting);
    }

    #[test]
    fn strict_break_disallows_skip_and_postpone() {
        let base = SystemTime::UNIX_EPOCH;
        let mut core = core_with(true, true);
        core.start(base);
        core.tick(at(base, 60));
        let ev = core.tick(at(base, 62));
        match &ev[0] {
            CoreEvent::StartBreak {
                allow_skip,
                allow_postpone,
                ..
            } => {
                assert!(!allow_skip);
                assert!(!allow_postpone);
            }
            other => panic!("expected StartBreak, got {other:?}"),
        }
        // postpone() is a no-op under strict mode.
        core.postpone();
        let ev = core.tick(at(base, 63));
        assert!(ev.iter().any(|e| matches!(e, CoreEvent::Countdown { .. })));
        assert_eq!(core.state(), State::Break);
    }

    #[test]
    fn take_break_now_skips_the_wait() {
        let base = SystemTime::UNIX_EPOCH;
        let mut core = core_with(false, true);
        core.start(base);
        core.take_break(Some(BreakType::Long));
        let ev = core.tick(at(base, 1));
        match &ev[0] {
            CoreEvent::StartBreak { kind, .. } => assert_eq!(*kind, BreakType::Long),
            other => panic!("expected StartBreak, got {other:?}"),
        }
        assert_eq!(core.state(), State::Break);
    }

    #[test]
    fn disable_then_enable_reschedules() {
        let base = SystemTime::UNIX_EPOCH;
        let mut core = core_with(false, true);
        core.start(base);
        core.disable();
        assert_eq!(core.state(), State::Stopped);
        assert!(core.tick(at(base, 120)).is_empty());

        let ev = core.enable(at(base, 120));
        assert!(matches!(ev[0], CoreEvent::NextBreakScheduled { .. }));
        assert_eq!(core.state(), State::Waiting);
    }

    #[test]
    fn postpone_reschedules_same_break() {
        let base = SystemTime::UNIX_EPOCH;
        let mut core = core_with(false, true);
        core.start(base);
        core.tick(at(base, 60));
        core.tick(at(base, 62)); // break started
        let before = core.current_break_name().to_string();
        core.postpone();
        let ev = core.tick(at(base, 62));
        // 5-minute postpone.
        assert!(ev
            .iter()
            .any(|e| matches!(e, CoreEvent::NextBreakScheduled { seconds_until: 300, .. })));
        assert_eq!(core.current_break_name(), before);
        assert_eq!(core.state(), State::Waiting);
    }
}
