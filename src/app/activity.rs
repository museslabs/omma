//! do-not-disturb. tier 1 checks running/frontmost apps via NSWorkspace, no permission.
//! tier 2 checks fullscreen or window titles via the accessibility api, which the user grants.

use std::ptr::{self, NonNull};

use objc2_app_kit::{NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace};
use objc2_application_services::{
    AXError, AXIsProcessTrustedWithOptions, AXUIElement, kAXTrustedCheckOptionPrompt,
};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFRetained, CFString, CFType, ConcreteType,
};

use crate::config::{AppName, DoNotDisturb, TitlePattern};

pub struct DndChecker {
    enabled: bool,
    apps_running: Vec<AppName>,
    apps_active: Vec<AppName>,
    check_fullscreen: bool,
    title_patterns: Vec<TitlePattern>,
    needs_ax: bool,
    ax_trusted: bool,
}

impl DndChecker {
    pub fn from_config(dnd: &DoNotDisturb) -> DndChecker {
        let needs_ax =
            dnd.enabled && (dnd.skip_when_fullscreen || !dnd.window_title_matches.is_empty());
        let ax_trusted = if needs_ax {
            prompt_accessibility()
        } else {
            false
        };

        if needs_ax && !ax_trusted {
            eprintln!(
                "omma: do-not-disturb fullscreen/window-title rules need Accessibility \
                 permission.\n      Grant it in System Settings → Privacy & Security → \
                 Accessibility, then restart omma."
            );
        }

        DndChecker {
            enabled: dnd.enabled,
            apps_running: dnd.skip_when_app_running.clone(),
            apps_active: dnd.skip_when_app_active.clone(),
            check_fullscreen: dnd.skip_when_fullscreen,
            title_patterns: dnd.window_title_matches.clone(),
            needs_ax,
            ax_trusted,
        }
    }

    pub fn should_suppress(&self) -> bool {
        if !self.enabled {
            return false;
        }

        let workspace = NSWorkspace::sharedWorkspace();

        // tier 1: frontmost app
        if !self.apps_active.is_empty()
            && let Some(front) = workspace.frontmostApplication()
            && running_app_matches(&front, &self.apps_active)
        {
            return true;
        }

        // tier 1: any running app
        if !self.apps_running.is_empty()
            && workspace
                .runningApplications()
                .iter()
                .any(|app| running_app_matches(&app, &self.apps_running))
        {
            return true;
        }

        // tier 2: accessibility-based checks
        if self.needs_ax && self.ax_trusted {
            if self.check_fullscreen && frontmost_is_fullscreen(&workspace) {
                return true;
            }
            if !self.title_patterns.is_empty()
                && any_window_title_matches(&workspace, &self.title_patterns)
            {
                return true;
            }
        }

        false
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

/// check and, on first run, prompt for accessibility permission
fn prompt_accessibility() -> bool {
    // SAFETY: an immutable CFString that the framework exports
    let prompt = unsafe { kAXTrustedCheckOptionPrompt };
    let options =
        CFDictionary::<CFString, CFBoolean>::from_slices(&[prompt], &[CFBoolean::new(true)]);
    // SAFETY: the options map CFString keys to CFBoolean values, as the call expects
    unsafe { AXIsProcessTrustedWithOptions(Some(options.as_opaque())) }
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

fn frontmost_is_fullscreen(workspace: &NSWorkspace) -> bool {
    workspace.frontmostApplication().is_some_and(|front| {
        attribute_as::<AXUIElement>(&ax_app(&front), "AXFocusedWindow")
            .and_then(|window| attribute_as::<CFBoolean>(&window, "AXFullScreen"))
            .is_some_and(|fullscreen| fullscreen.as_bool())
    })
}

fn any_window_title_matches(workspace: &NSWorkspace, patterns: &[TitlePattern]) -> bool {
    workspace
        .runningApplications()
        .iter()
        // only regular apps have inspectable windows
        .filter(|app| app.activationPolicy() == NSApplicationActivationPolicy::Regular)
        .flat_map(|app| windows(&app))
        .filter_map(|window| attribute_as::<CFString>(&window, "AXTitle"))
        .any(|title| {
            let title = title.to_string();
            patterns.iter().any(|p| p.is_match(&title))
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
    fn a_rule_matches_names_beyond_ascii() {
        assert!(app_matches(&rules(&["ΣΗΜΕΙΏΣΕΙΣ"]), "Σημειώσεις", ""));
    }
}
