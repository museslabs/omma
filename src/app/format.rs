//! how counts and menu lines read, the same on every platform

/// the warning's count: "9s", or "2:05" from 100 seconds up
pub fn fmt_warning(seconds: u64) -> String {
    if seconds >= 100 {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

/// the break's count, in a clock format throughout the break
pub fn fmt_count(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// the break's count as a screen reader reads it
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn spoken_count(seconds: u64) -> String {
    let unit = |n: u64, name: &str| format!("{n} {name}{}", if n == 1 { "" } else { "s" });
    match (seconds / 60, seconds % 60) {
        (0, s) => format!("{} remaining", unit(s, "second")),
        (m, 0) => format!("{} remaining", unit(m, "minute")),
        (m, s) => format!("{} {} remaining", unit(m, "minute"), unit(s, "second")),
    }
}

pub fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// `text` cut to `chars` characters, with an ellipsis when it was longer
pub fn shorten(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_warning_counts_in_seconds_then_minutes() {
        assert_eq!(fmt_warning(9), "9s");
        assert_eq!(fmt_warning(99), "99s");
        assert_eq!(fmt_warning(100), "1:40");
        assert_eq!(fmt_warning(600), "10:00");
    }

    #[test]
    fn the_pupil_keeps_a_clock_format_throughout_the_break() {
        assert_eq!(fmt_count(0), "0:00");
        assert_eq!(fmt_count(15), "0:15");
        assert_eq!(fmt_count(99), "1:39");
        assert_eq!(fmt_count(100), "1:40");
        assert_eq!(fmt_count(300), "5:00");
        assert_eq!(fmt_count(3600), "60:00");
    }

    #[test]
    fn a_screen_reader_reads_the_count_in_words() {
        assert_eq!(spoken_count(0), "0 seconds remaining");
        assert_eq!(spoken_count(1), "1 second remaining");
        assert_eq!(spoken_count(15), "15 seconds remaining");
        assert_eq!(spoken_count(60), "1 minute remaining");
        assert_eq!(spoken_count(125), "2 minutes 5 seconds remaining");
    }

    #[test]
    fn capitalized_uppercases_the_first_character() {
        assert_eq!(capitalized("can't show"), "Can't show");
        assert_eq!(capitalized("ένα"), "Ένα");
        assert_eq!(capitalized(""), "");
    }

    #[test]
    fn shorten_cuts_whole_characters() {
        assert_eq!(shorten("Next break", 10), "Next break");
        assert_eq!(shorten("Next break", 4), "Next…");
        assert_eq!(shorten("“Πλάνο” is on screen", 7), "“Πλάνο”…");
        assert_eq!(shorten("", 0), "");
    }
}
