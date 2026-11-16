//! the local calendar, for "the rest of the day"

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// the first midnight after `now` in the user's time zone. mktime moves a midnight that a
/// daylight-saving change skips to the first time that day has
pub fn next_midnight(now: SystemTime) -> SystemTime {
    let secs = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let fallback = UNIX_EPOCH + Duration::from_secs((secs / 86_400 + 1) * 86_400);
    let Ok(time) = libc::time_t::try_from(secs) else {
        return fallback;
    };
    // SAFETY: a zeroed tm is a valid tm, and localtime_r only writes into it
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are to live locals. localtime_r is the thread-safe variant
    if unsafe { libc::localtime_r(&time, &mut tm) }.is_null() {
        return fallback;
    }
    tm.tm_mday += 1;
    tm.tm_hour = 0;
    tm.tm_min = 0;
    tm.tm_sec = 0;
    // let mktime decide whether daylight saving is in effect at that time
    tm.tm_isdst = -1;
    // SAFETY: `tm` is a valid, normalizable tm, which mktime can rewrite
    let midnight = unsafe { libc::mktime(&mut tm) };
    match u64::try_from(midnight) {
        Ok(midnight) if midnight > secs => UNIX_EPOCH + Duration::from_secs(midnight),
        _ => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rest_of_the_day_ends_at_the_next_midnight() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let midnight = next_midnight(now);
        assert!(midnight > now);
        assert!(midnight.duration_since(now).unwrap() <= Duration::from_secs(25 * 3600));
        assert_eq!(next_midnight(midnight - Duration::from_secs(1)), midnight);
        assert!(next_midnight(midnight) > midnight);
    }
}
