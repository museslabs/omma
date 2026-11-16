//! time since the last input, for smart pause. Wayland tells a client when the seat stays idle
//! for a set time and when input comes back, not how long the seat is idle. that is all the
//! schedule needs. it only asks whether the idle time reached the threshold, and how long ago
//! the last input was once it did.

use std::time::{Duration, Instant, SystemTime};

use wayland_client::protocol::wl_seat;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1, ext_idle_notifier_v1,
};

use super::desktop::Desktop;

#[derive(Default)]
pub struct Idle {
    pub notifier: Option<ext_idle_notifier_v1::ExtIdleNotifierV1>,
    notification: Option<ext_idle_notification_v1::ExtIdleNotificationV1>,
    seat: Option<wl_seat::WlSeat>,
    /// the threshold the notification is for
    threshold: Option<Duration>,
    /// when the compositor said the seat went idle
    idle_since: Option<Instant>,
}

impl Idle {
    /// asks to hear when the seat stays idle for `threshold`, or nothing for None
    pub fn watch(&mut self, threshold: Option<Duration>, qh: &QueueHandle<Desktop>) {
        if threshold == self.threshold && (self.notification.is_some() || threshold.is_none()) {
            return;
        }
        self.threshold = threshold;
        self.renew(qh);
    }

    pub fn seat_changed(&mut self, seat: Option<&wl_seat::WlSeat>, qh: &QueueHandle<Desktop>) {
        if self.seat.as_ref() == seat {
            return;
        }
        self.seat = seat.cloned();
        self.renew(qh);
    }

    fn renew(&mut self, qh: &QueueHandle<Desktop>) {
        if let Some(old) = self.notification.take() {
            old.destroy();
        }
        self.idle_since = None;
        let (Some(notifier), Some(seat), Some(threshold)) =
            (&self.notifier, &self.seat, self.threshold)
        else {
            return;
        };
        let ms = u32::try_from(threshold.as_millis()).unwrap_or(u32::MAX);
        // input alone counts, as on macOS: a video that keeps the screen awake does not keep
        // the user from being away
        self.notification = Some(if notifier.version() >= 2 {
            notifier.get_input_idle_notification(ms, seat, qh, ())
        } else {
            notifier.get_idle_notification(ms, seat, qh, ())
        });
    }

    /// time since the last input, as far as the compositor told omma. it is zero until the seat
    /// stays idle for the threshold. after that, it is the threshold plus the time since then
    pub fn idle_time(&self, _now: SystemTime) -> Duration {
        match (self.idle_since, self.threshold) {
            (Some(since), Some(threshold)) => threshold + since.elapsed(),
            _ => Duration::ZERO,
        }
    }

    /// whether smart pause can work here
    pub fn available(&self) -> bool {
        self.notifier.is_some()
    }
}

impl Dispatch<ext_idle_notification_v1::ExtIdleNotificationV1, ()> for Desktop {
    fn event(
        state: &mut Desktop,
        notification: &ext_idle_notification_v1::ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Desktop>,
    ) {
        let idle = &mut state.idle;
        if idle.notification.as_ref() != Some(notification) {
            return;
        }
        match event {
            ext_idle_notification_v1::Event::Idled => idle.idle_since = Some(Instant::now()),
            ext_idle_notification_v1::Event::Resumed => idle.idle_since = None,
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(Desktop: ignore ext_idle_notifier_v1::ExtIdleNotifierV1);
