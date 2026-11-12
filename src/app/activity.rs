//! do-not-disturb. the app rules look at running and frontmost apps through NSWorkspace and
//! need no permission. the fullscreen and window-title rules read other apps' windows through
//! the Accessibility API, which the user grants.

use std::fmt;
use std::ptr::{self, NonNull};

use objc2::rc::Retained;
use objc2_app_kit::{NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace};
use objc2_application_services::{
    AXError, AXIsProcessTrusted, AXIsProcessTrustedWithOptions, AXUIElement,
    kAXTrustedCheckOptionPrompt,
};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFRetained, CFString, CFType, ConcreteType,
};

use crate::config::{AppName, DoNotDisturb, TitlePattern};

/// how long an Accessibility call waits for an app before it fails. a hung app would hold
/// omma's main thread for the system's default, about 6 s.
const AX_TIMEOUT_SECS: f32 = 0.25;

/// a do-not-disturb rule that held, with the app's name or the window's title
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
            // a line break in a title would split a menu item
            Reason::Window(title) => {
                write!(f, "“{}” is on screen", title.replace(char::is_control, " "))
            }
        }
    }
}

/// the first rule that holds now. permission for the window rules is checked each time, so
/// a grant while omma runs counts at once.
pub fn should_suppress(dnd: &DoNotDisturb) -> Option<Reason> {
    if !dnd.enabled {
        return None;
    }
    let workspace = NSWorkspace::sharedWorkspace();

    if !dnd.skip_when_app_active.is_empty()
        && let Some(front) = workspace.frontmostApplication()
        && running_app_matches(&front, &dnd.skip_when_app_active)
    {
        return Some(Reason::Frontmost(app_name(&front)));
    }
    if !dnd.skip_when_app_running.is_empty()
        && let Some(app) = workspace
            .runningApplications()
            .iter()
            .find(|app| running_app_matches(app, &dnd.skip_when_app_running))
    {
        return Some(Reason::Running(app_name(&app)));
    }

    // SAFETY: no arguments
    if !dnd.needs_accessibility() || !unsafe { AXIsProcessTrusted() } {
        return None;
    }
    limit_ax_wait();
    if dnd.skip_when_fullscreen
        && let Some(front) = fullscreen_app(&workspace)
    {
        return Some(Reason::Fullscreen(app_name(&front)));
    }
    matching_title(&workspace, &dnd.window_title_matches).map(Reason::Window)
}

/// asks for Accessibility permission unless omma has it. the system's prompt doesn't block.
pub fn request_accessibility() {
    // SAFETY: an immutable CFString that the framework exports
    let prompt = unsafe { kAXTrustedCheckOptionPrompt };
    let options =
        CFDictionary::<CFString, CFBoolean>::from_slices(&[prompt], &[CFBoolean::new(true)]);
    // SAFETY: the options map CFString keys to CFBoolean values, as the call expects
    if !unsafe { AXIsProcessTrustedWithOptions(Some(options.as_opaque())) } {
        eprintln!(
            "omma: the fullscreen and window-title rules need Accessibility permission.\n      \
             Grant it in System Settings → Privacy & Security → Accessibility."
        );
    }
}

/// whether a rule names part of the app's name or bundle id
fn app_matches(rules: &[AppName], name: &str, bundle_id: &str) -> bool {
    let name = name.to_lowercase();
    let bundle_id = bundle_id.to_lowercase();
    rules
        .iter()
        .map(AppName::as_str)
        .any(|rule| name.contains(rule) || bundle_id.contains(rule))
}

fn app_name(app: &NSRunningApplication) -> String {
    app.localizedName()
        .or_else(|| app.bundleIdentifier())
        .map_or_else(|| "an app".into(), |name| name.to_string())
}

fn running_app_matches(app: &NSRunningApplication, rules: &[AppName]) -> bool {
    let name = app
        .localizedName()
        .map(|s| s.to_string())
        .unwrap_or_default();
    let bundle_id = app
        .bundleIdentifier()
        .map(|s| s.to_string())
        .unwrap_or_default();
    app_matches(rules, &name, &bundle_id)
}

/// caps every Accessibility call's wait: the system-wide element's timeout is the global
/// one. set before each round of calls, since Apple doesn't document whether setting it
/// before permission is granted holds.
fn limit_ax_wait() {
    // SAFETY: no arguments
    let system = unsafe { AXUIElement::new_system_wide() };
    // SAFETY: plain values. the call fails only for an invalid element or a negative timeout.
    let _ = unsafe { system.set_messaging_timeout(AX_TIMEOUT_SECS) };
}

/// the app's top-level accessibility element
fn ax_app(app: &NSRunningApplication) -> CFRetained<AXUIElement> {
    // SAFETY: takes any pid; calls on an element whose app is gone just fail
    unsafe { AXUIElement::new_application(app.processIdentifier()) }
}

/// the element's attribute, when the app has it and answers
fn attribute(element: &AXUIElement, name: &'static str) -> Option<CFRetained<CFType>> {
    let mut value: *const CFType = ptr::null();
    // SAFETY: `value` is a valid place for the result
    let error = unsafe {
        element.copy_attribute_value(&CFString::from_static_str(name), NonNull::from(&mut value))
    };
    if error != AXError::Success {
        return None;
    }
    // SAFETY: a Copy function hands over one reference, and CFRetained releases it
    NonNull::new(value.cast_mut()).map(|value| unsafe { CFRetained::from_raw(value) })
}

/// the attribute, when its value is a `T`. another app supplies it, so its type is checked.
fn attribute_as<T: ConcreteType>(
    element: &AXUIElement,
    name: &'static str,
) -> Option<CFRetained<T>> {
    attribute(element, name)?.downcast().ok()
}

/// the frontmost app, when its focused window is fullscreen
fn fullscreen_app(workspace: &NSWorkspace) -> Option<Retained<NSRunningApplication>> {
    let front = workspace.frontmostApplication()?;
    attribute_as::<AXUIElement>(&ax_app(&front), "AXFocusedWindow")
        .and_then(|window| attribute_as::<CFBoolean>(&window, "AXFullScreen"))
        .is_some_and(|fullscreen| fullscreen.as_bool())
        .then_some(front)
}

/// the first title a pattern matches, of a window on screen: one that isn't minimized, of
/// an app that isn't hidden
fn matching_title(workspace: &NSWorkspace, patterns: &[TitlePattern]) -> Option<String> {
    if patterns.is_empty() {
        return None;
    }
    workspace
        .runningApplications()
        .iter()
        // only regular apps have inspectable windows, and a hidden app shows none
        .filter(|app| {
            app.activationPolicy() == NSApplicationActivationPolicy::Regular && !app.isHidden()
        })
        .flat_map(|app| windows(&app))
        .find_map(|window| {
            let title = attribute_as::<CFString>(&window, "AXTitle")?.to_string();
            let minimized =
                || attribute_as::<CFBoolean>(&window, "AXMinimized").is_some_and(|m| m.as_bool());
            // minimized is asked only of a matching window, which keeps the calls few
            (patterns.iter().any(|p| p.is_match(&title)) && !minimized()).then_some(title)
        })
}

/// the app's windows, as far as it answers
fn windows(app: &NSRunningApplication) -> Vec<CFRetained<AXUIElement>> {
    let Some(windows) = attribute_as::<CFArray>(&ax_app(app), "AXWindows") else {
        return Vec::new();
    };
    // SAFETY: an array from the accessibility API holds only CF objects
    let windows = unsafe { windows.cast_unchecked::<CFType>() };
    windows
        .iter()
        .filter_map(|window| window.downcast().ok())
        .collect()
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
    fn a_rule_matches_part_of_the_name_or_bundle_id_in_any_case() {
        let zoom = rules(&["Zoom"]);
        assert!(app_matches(&zoom, "zoom.us", "us.zoom.xos"));
        assert!(app_matches(&zoom, "ZOOM", ""));
        assert!(app_matches(&zoom, "", "us.zoom.xos"));
        assert!(!app_matches(&zoom, "Safari", "com.apple.Safari"));
        assert!(!app_matches(&zoom, "", ""));
        assert!(app_matches(&rules(&["keynote", "zoom"]), "Zoom", ""));
        assert!(!app_matches(&[], "Zoom", "us.zoom.xos"));
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
        assert!(app_matches(&rules(&["ΣΗΜΕΙΏΣΕΙΣ"]), "Σημειώσεις", ""));
    }
}
