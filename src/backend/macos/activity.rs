//! do-not-disturb. the app rules look at running and frontmost apps through NSWorkspace and
//! need no permission. the fullscreen and window-title rules read other apps' windows through
//! the Accessibility API, which the user grants

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

use crate::app::dnd::{Reason, app_matches};
use crate::config::{AppName, DoNotDisturb, TitlePattern};

/// how long an Accessibility call waits for an app before it fails. without this limit, a hung
/// app holds omma's main thread for the system default, about 6 s
const AX_TIMEOUT_SECS: f32 = 0.25;

/// the first rule that holds now. omma checks the permission for the window rules on each call,
/// so a grant while omma runs takes effect at once
pub fn should_suppress(dnd: &DoNotDisturb) -> Option<Reason> {
    let workspace = NSWorkspace::sharedWorkspace();

    if !dnd.apps_in_front.is_empty()
        && let Some(front) = workspace.frontmostApplication()
        && running_app_matches(&front, &dnd.apps_in_front)
    {
        return Some(Reason::Frontmost(app_name(&front)));
    }
    if !dnd.apps_running.is_empty()
        && let Some(app) = workspace
            .runningApplications()
            .iter()
            .find(|app| running_app_matches(app, &dnd.apps_running))
    {
        return Some(Reason::Running(app_name(&app)));
    }

    if !dnd.needs_accessibility() || !is_trusted() {
        return None;
    }
    limit_ax_wait();
    if dnd.fullscreen
        && let Some(front) = fullscreen_app(&workspace)
    {
        return Some(Reason::Fullscreen(app_name(&front)));
    }
    matching_title(&workspace, &dnd.window_titles).map(Reason::Window)
}

/// whether omma has Accessibility permission. it can change while omma runs
pub fn is_trusted() -> bool {
    // SAFETY: no arguments
    unsafe { AXIsProcessTrusted() }
}

/// asks for Accessibility permission unless omma has it. the system's prompt does not block
pub fn request_accessibility() {
    // SAFETY: an immutable CFString that the framework exports
    let prompt = unsafe { kAXTrustedCheckOptionPrompt };
    let options =
        CFDictionary::<CFString, CFBoolean>::from_slices(&[prompt], &[CFBoolean::new(true)]);
    // SAFETY: the options map CFString keys to CFBoolean values, as the call expects
    unsafe { AXIsProcessTrustedWithOptions(Some(options.as_opaque())) };
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
    app_matches(rules, [name.as_str(), bundle_id.as_str()])
}

/// a timeout on the system-wide element applies to every element. omma sets it before each
/// round of calls, because Apple does not document whether a timeout set before the user grants
/// the permission holds
fn limit_ax_wait() {
    // SAFETY: no arguments
    let system = unsafe { AXUIElement::new_system_wide() };
    // SAFETY: plain values. the call fails only for an invalid element or a negative timeout
    let _ = unsafe { system.set_messaging_timeout(AX_TIMEOUT_SECS) };
}

fn ax_app(app: &NSRunningApplication) -> CFRetained<AXUIElement> {
    // SAFETY: the call takes any pid. calls on an element whose app is gone fail
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

/// the attribute, when its value is a `T`. another app supplies the value, so this checks its type
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

/// the first title a pattern matches, of a window on screen: one that is not minimized, of
/// an app that is not hidden
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
            // only a matching window gets the minimized query, which keeps the calls few
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
