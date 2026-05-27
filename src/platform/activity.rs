//! Do-not-disturb: decide whether a scheduled break should be suppressed
//! because the user is busy.
//!
//! Two tiers:
//!  * **Tier 1** — is one of the named apps running, or frontmost? Uses
//!    `NSWorkspace`; needs no special permission.
//!  * **Tier 2** — is the frontmost window fullscreen, or does any on-screen
//!    window title match a pattern (e.g. "Zoom Meeting", or a phrase from a
//!    browser tab)? Uses the Accessibility API, which the user must grant.

use std::ffi::{c_char, c_void, CStr, CString};

use objc2_app_kit::{NSApplicationActivationPolicy, NSWorkspace};
use regex::{Regex, RegexBuilder};

use crate::config::DoNotDisturb;

pub struct DndChecker {
    enabled: bool,
    apps_running: Vec<String>,
    apps_active: Vec<String>,
    check_fullscreen: bool,
    title_regexes: Vec<Regex>,
    needs_ax: bool,
    ax_trusted: bool,
}

impl DndChecker {
    pub fn from_config(dnd: &DoNotDisturb) -> DndChecker {
        // Case-insensitive by default (like app matching). A plain word such
        // as "Plain" is just a case-insensitive substring match; full regex
        // syntax still works.
        let title_regexes = dnd
            .window_title_matches
            .iter()
            .filter_map(|p| match RegexBuilder::new(p).case_insensitive(true).build() {
                Ok(re) => Some(re),
                Err(e) => {
                    eprintln!("omma: ignoring invalid window_title_matches pattern {p:?}: {e}");
                    None
                }
            })
            .collect::<Vec<_>>();

        let needs_ax = dnd.enabled && (dnd.skip_when_fullscreen || !title_regexes.is_empty());
        let ax_trusted = if needs_ax { prompt_accessibility() } else { false };

        if needs_ax && !ax_trusted {
            eprintln!(
                "omma: do-not-disturb fullscreen/window-title rules need Accessibility \
                 permission.\n      Grant it in System Settings → Privacy & Security → \
                 Accessibility, then restart omma."
            );
        }

        DndChecker {
            enabled: dnd.enabled,
            apps_running: lower(&dnd.skip_when_app_running),
            apps_active: lower(&dnd.skip_when_app_active),
            check_fullscreen: dnd.skip_when_fullscreen,
            title_regexes,
            needs_ax,
            ax_trusted,
        }
    }

    /// True if the upcoming break should be suppressed right now.
    pub fn should_suppress(&self) -> bool {
        if !self.enabled {
            return false;
        }

        let workspace = NSWorkspace::sharedWorkspace();

        // Tier 1: frontmost app.
        if !self.apps_active.is_empty() {
            if let Some(front) = workspace.frontmostApplication() {
                if app_matches(&front, &self.apps_active) {
                    return true;
                }
            }
        }

        // Tier 1: any running app.
        if !self.apps_running.is_empty() {
            let running = workspace.runningApplications();
            for i in 0..running.count() {
                let app = running.objectAtIndex(i);
                if app_matches(&app, &self.apps_running) {
                    return true;
                }
            }
        }

        // Tier 2: Accessibility-based checks.
        if self.needs_ax && self.ax_trusted {
            if self.check_fullscreen && frontmost_is_fullscreen(&workspace) {
                return true;
            }
            if !self.title_regexes.is_empty() && any_window_title_matches(&workspace, &self.title_regexes)
            {
                return true;
            }
        }

        false
    }
}

fn lower(v: &[String]) -> Vec<String> {
    v.iter().map(|s| s.to_lowercase()).collect()
}

fn app_matches(app: &objc2_app_kit::NSRunningApplication, patterns: &[String]) -> bool {
    let name = app
        .localizedName()
        .map(|s| s.to_string().to_lowercase())
        .unwrap_or_default();
    let bundle = app
        .bundleIdentifier()
        .map(|s| s.to_string().to_lowercase())
        .unwrap_or_default();
    patterns
        .iter()
        .any(|p| (!name.is_empty() && name.contains(p)) || (!bundle.is_empty() && bundle.contains(p)))
}

// ---------------------------------------------------------------------------
// Accessibility (AX) interop. Inherently unsafe C; kept contained here.
// ---------------------------------------------------------------------------

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFArrayRef = *const c_void;
type CFBooleanRef = *const c_void;
type CFDictionaryRef = *const c_void;
type AXUIElementRef = *const c_void;
type CFIndex = isize;

const CFSTRING_ENCODING_UTF8: u32 = 0x0800_0100;
const AX_SUCCESS: i32 = 0;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> i32;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        cstr: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFStringGetLength(s: CFStringRef) -> CFIndex;
    fn CFStringGetCString(
        s: CFStringRef,
        buffer: *mut c_char,
        size: CFIndex,
        encoding: u32,
    ) -> bool;
    fn CFArrayGetCount(arr: CFArrayRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(arr: CFArrayRef, idx: CFIndex) -> *const c_void;
    fn CFBooleanGetValue(b: CFBooleanRef) -> bool;
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        num_values: CFIndex,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFDictionaryRef;
    static kCFBooleanTrue: CFBooleanRef;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}

/// Check (and, on first run, prompt for) Accessibility permission.
fn prompt_accessibility() -> bool {
    unsafe {
        let keys: [*const c_void; 1] = [kAXTrustedCheckOptionPrompt];
        let values: [*const c_void; 1] = [kCFBooleanTrue];
        let options = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        );
        let trusted = AXIsProcessTrustedWithOptions(options);
        if !options.is_null() {
            CFRelease(options);
        }
        trusted
    }
}

/// Build a CFString from a UTF-8 attribute name. Caller must `CFRelease`.
unsafe fn cfstr(s: &str) -> CFStringRef {
    let c = CString::new(s).unwrap();
    unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), CFSTRING_ENCODING_UTF8) }
}

unsafe fn cfstring_to_string(s: CFStringRef) -> Option<String> {
    if s.is_null() {
        return None;
    }
    let len = unsafe { CFStringGetLength(s) };
    let cap = (len.max(0) * 4 + 1) as usize;
    let mut buf = vec![0 as c_char; cap];
    let ok = unsafe { CFStringGetCString(s, buf.as_mut_ptr(), buf.len() as CFIndex, CFSTRING_ENCODING_UTF8) };
    if !ok {
        return None;
    }
    let cstr = unsafe { CStr::from_ptr(buf.as_ptr()) };
    Some(cstr.to_string_lossy().into_owned())
}

/// Copy an AX attribute as a generic CFType. Returns null on failure.
unsafe fn ax_copy(element: AXUIElementRef, attr: CFStringRef) -> CFTypeRef {
    let mut value: CFTypeRef = std::ptr::null();
    let err = unsafe { AXUIElementCopyAttributeValue(element, attr, &mut value) };
    if err == AX_SUCCESS {
        value
    } else {
        std::ptr::null()
    }
}

fn frontmost_is_fullscreen(workspace: &NSWorkspace) -> bool {
    let Some(front) = workspace.frontmostApplication() else {
        return false;
    };
    let pid = front.processIdentifier();
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return false;
        }
        let focused_attr = cfstr("AXFocusedWindow");
        let fullscreen_attr = cfstr("AXFullScreen");

        let mut result = false;
        let window = ax_copy(app, focused_attr);
        if !window.is_null() {
            let fs = ax_copy(window, fullscreen_attr);
            if !fs.is_null() {
                result = CFBooleanGetValue(fs);
                CFRelease(fs);
            }
            CFRelease(window);
        }

        CFRelease(focused_attr);
        CFRelease(fullscreen_attr);
        CFRelease(app);
        result
    }
}

fn any_window_title_matches(workspace: &NSWorkspace, regexes: &[Regex]) -> bool {
    let running = workspace.runningApplications();
    unsafe {
        let windows_attr = cfstr("AXWindows");
        let title_attr = cfstr("AXTitle");

        let mut matched = false;
        'apps: for i in 0..running.count() {
            let app = running.objectAtIndex(i);
            // Only apps with a visible UI have inspectable windows.
            if app.activationPolicy() != NSApplicationActivationPolicy::Regular {
                continue;
            }
            let pid = app.processIdentifier();
            let ax_app = AXUIElementCreateApplication(pid);
            if ax_app.is_null() {
                continue;
            }

            let windows = ax_copy(ax_app, windows_attr);
            if !windows.is_null() {
                let count = CFArrayGetCount(windows as CFArrayRef);
                for w in 0..count {
                    let window = CFArrayGetValueAtIndex(windows as CFArrayRef, w) as AXUIElementRef;
                    if window.is_null() {
                        continue;
                    }
                    let title_ref = ax_copy(window, title_attr);
                    if !title_ref.is_null() {
                        if let Some(title) = cfstring_to_string(title_ref) {
                            if regexes.iter().any(|re| re.is_match(&title)) {
                                matched = true;
                            }
                        }
                        CFRelease(title_ref);
                    }
                    if matched {
                        CFRelease(windows);
                        CFRelease(ax_app);
                        break 'apps;
                    }
                }
                CFRelease(windows);
            }
            CFRelease(ax_app);
        }

        CFRelease(windows_attr);
        CFRelease(title_attr);
        matched
    }
}
