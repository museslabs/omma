//! User idle time, for smart pause. `CGEventSourceSecondsSinceLastEventType`
//! reports how long since the last keyboard/mouse event; it needs no special
//! permission.

const COMBINED_SESSION_STATE: i32 = 0; // kCGEventSourceStateCombinedSessionState
const ANY_INPUT_EVENT_TYPE: u32 = !0; // kCGAnyInputEventType

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
}

/// Seconds since the last user input.
pub fn idle_seconds() -> f64 {
    unsafe { CGEventSourceSecondsSinceLastEventType(COMBINED_SESSION_STATE, ANY_INPUT_EVENT_TYPE) }
}
