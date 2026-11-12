//! do-not-disturb. tier 1 checks running/frontmost apps via NSWorkspace, no permission.
//! tier 2 checks fullscreen or window titles via the accessibility api, which the user grants.

use std::ffi::{CStr, CString, c_char, c_void};

use objc2_app_kit::{NSApplicationActivationPolicy, NSWorkspace};

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
            && app_matches(&front, &self.apps_active)
        {
            return true;
        }

        // tier 1: any running app
        if !self.apps_running.is_empty() {
            let running = workspace.runningApplications();
            for i in 0..running.count() {
                let app = running.objectAtIndex(i);
                if app_matches(&app, &self.apps_running) {
                    return true;
                }
            }
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

fn app_matches(app: &objc2_app_kit::NSRunningApplication, patterns: &[AppName]) -> bool {
    let name = app
        .localizedName()
        .map(|s| s.to_string().to_lowercase())
        .unwrap_or_default();
    let bundle = app
        .bundleIdentifier()
        .map(|s| s.to_string().to_lowercase())
        .unwrap_or_default();
    patterns.iter().map(AppName::as_str).any(|p| {
        (!name.is_empty() && name.contains(p)) || (!bundle.is_empty() && bundle.contains(p))
    })
}

// accessibility (AX) interop. unsafe C, kept contained here.

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

/// check and, on first run, prompt for accessibility permission
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

/// caller must CFRelease
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
    let ok = unsafe {
        CFStringGetCString(
            s,
            buf.as_mut_ptr(),
            buf.len() as CFIndex,
            CFSTRING_ENCODING_UTF8,
        )
    };
    if !ok {
        return None;
    }
    let cstr = unsafe { CStr::from_ptr(buf.as_ptr()) };
    Some(cstr.to_string_lossy().into_owned())
}

/// null on failure
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

fn any_window_title_matches(workspace: &NSWorkspace, patterns: &[TitlePattern]) -> bool {
    let running = workspace.runningApplications();
    unsafe {
        let windows_attr = cfstr("AXWindows");
        let title_attr = cfstr("AXTitle");

        let mut matched = false;
        'apps: for i in 0..running.count() {
            let app = running.objectAtIndex(i);
            // only regular apps have inspectable windows
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
                        if let Some(title) = cfstring_to_string(title_ref)
                            && patterns.iter().any(|p| p.is_match(&title))
                        {
                            matched = true;
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
