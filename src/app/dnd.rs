//! do-not-disturb: why a rule held a break back. each platform reads its own apps and
//! windows, and says what it found here

use std::fmt;

use crate::config::AppName;

/// a do-not-disturb rule that held, with the app's name or the window's title
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    Running(String),
    Frontmost(String),
    Fullscreen(String),
    Window(String),
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Reason::Running(app) => write!(f, "{app} is running"),
            Reason::Frontmost(app) => write!(f, "{app} is in front"),
            Reason::Fullscreen(app) => write!(f, "{app} is fullscreen"),
            // a line break in a title splits a menu item
            Reason::Window(title) => {
                write!(f, "“{}” is on screen", title.replace(char::is_control, " "))
            }
        }
    }
}

/// whether a rule names part of any of the app's names: its name, bundle id, app id or
/// process name, whichever the platform has
pub fn app_matches<'a>(rules: &[AppName], names: impl IntoIterator<Item = &'a str>) -> bool {
    let names: Vec<String> = names
        .into_iter()
        .filter(|name| !name.is_empty())
        .map(str::to_lowercase)
        .collect();
    rules
        .iter()
        .map(AppName::as_str)
        .any(|rule| names.iter().any(|name| name.contains(rule)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(names: &[&str]) -> Vec<AppName> {
        names
            .iter()
            .map(|name| AppName::try_from(name.to_string()).unwrap())
            .collect()
    }

    #[test]
    fn a_rule_matches_part_of_any_name_in_any_case() {
        let zoom = rules(&["Zoom"]);
        assert!(app_matches(&zoom, ["zoom.us", "us.zoom.xos"]));
        assert!(app_matches(&zoom, ["ZOOM", ""]));
        assert!(app_matches(&zoom, ["", "us.zoom.xos"]));
        assert!(!app_matches(&zoom, ["Safari", "com.apple.Safari"]));
        assert!(!app_matches(&zoom, ["", ""]));
        assert!(app_matches(&rules(&["keynote", "zoom"]), ["Zoom"]));
        assert!(!app_matches(&[], ["Zoom", "us.zoom.xos"]));
    }

    #[test]
    fn a_reason_names_the_app_or_the_window() {
        assert_eq!(
            Reason::Running("Zoom".into()).to_string(),
            "Zoom is running"
        );
        assert_eq!(
            Reason::Frontmost("Zoom".into()).to_string(),
            "Zoom is in front"
        );
        assert_eq!(
            Reason::Fullscreen("Keynote".into()).to_string(),
            "Keynote is fullscreen"
        );
        assert_eq!(
            Reason::Window("Zoom\nMeeting".into()).to_string(),
            "“Zoom Meeting” is on screen"
        );
    }

    #[test]
    fn a_rule_matches_names_beyond_ascii() {
        assert!(app_matches(&rules(&["ΣΗΜΕΙΏΣΕΙΣ"]), ["Σημειώσεις"]));
    }
}
