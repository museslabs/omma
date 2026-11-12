//! system-wide shortcuts through Carbon's RegisterEventHotKey. they fire from any app, take
//! no focus and need no permission. omma holds them while the warning is up, and they act
//! only on the warning.

use std::ffi::c_void;
use std::ptr;

use objc2::msg_send;
use objc2::runtime::AnyObject;

use crate::config::{Key, KeySpec};

/// what a shortcut does to the warning
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Skip = 1,
    Postpone = 2,
}

impl Action {
    fn from_id(id: u32) -> Option<Action> {
        [Action::Skip, Action::Postpone]
            .into_iter()
            .find(|&action| action as u32 == id)
    }
}

pub struct Hotkeys {
    handler_installed: bool,
    held: Vec<(Action, KeySpec, EventHotKeyRef)>,
}

impl Hotkeys {
    pub fn new() -> Hotkeys {
        Hotkeys {
            handler_installed: false,
            held: Vec::new(),
        }
    }

    /// holds `keys` until `release`, each one Carbon accepts. a press sends `warningSkip:` or
    /// `warningPostpone:` to `target`, which must live as long as the app: it's the delegate.
    pub fn hold(&mut self, target: &AnyObject, keys: &[(Action, KeySpec)]) {
        self.release();
        if keys.is_empty() || !self.install_handler(target) {
            return;
        }
        for &(action, spec) in keys {
            let Some(code) = key_code(spec.key) else {
                eprintln!("omma: no key on the keyboard layout types `{spec}`");
                continue;
            };
            let id = EventHotKeyID {
                signature: SIGNATURE,
                id: action as u32,
            };
            let mut hotkey = ptr::null_mut();
            // SAFETY: plain values in, and `hotkey` is written only on success
            let status = unsafe {
                RegisterEventHotKey(
                    code,
                    carbon_modifiers(spec),
                    id,
                    GetApplicationEventTarget(),
                    0,
                    &mut hotkey,
                )
            };
            if status == NO_ERR {
                self.held.push((action, spec, hotkey));
            } else {
                eprintln!("omma: can't take `{spec}` as a global shortcut (error {status})");
            }
        }
    }

    /// the shortcut held for `action`, if Carbon took it
    pub fn held(&self, action: Action) -> Option<KeySpec> {
        self.held
            .iter()
            .find(|&&(held, _, _)| held == action)
            .map(|&(_, spec, _)| spec)
    }

    pub fn release(&mut self) {
        for (_, _, hotkey) in self.held.drain(..) {
            // SAFETY: each ref came from a successful RegisterEventHotKey and is released once
            unsafe { UnregisterEventHotKey(hotkey) };
        }
    }

    /// once per run; false when Carbon refuses
    fn install_handler(&mut self, target: &AnyObject) -> bool {
        if self.handler_installed {
            return true;
        }
        let pressed = EventTypeSpec {
            event_class: EVENT_CLASS_KEYBOARD,
            event_kind: EVENT_HOT_KEY_PRESSED,
        };
        let user_data = ptr::from_ref(target).cast_mut().cast::<c_void>();
        // SAFETY: the handler matches EventHandlerUPP, and `user_data` points at the delegate,
        // which outlives the handler because both last until the process exits
        let status = unsafe {
            InstallEventHandler(
                GetApplicationEventTarget(),
                on_hotkey,
                1,
                &pressed,
                user_data,
                ptr::null_mut(),
            )
        };
        self.handler_installed = status == NO_ERR;
        if !self.handler_installed {
            eprintln!("omma: can't listen for global shortcuts (error {status})");
        }
        self.handler_installed
    }
}

/// Carbon dispatches hot keys while the main thread fetches events, never while the delegate's
/// state is borrowed, so sending the action straight to the delegate is safe. that holds as
/// long as nothing called with the state borrowed runs a nested event loop (`runModal`, a
/// menu's `popUp`, `nextEventMatchingMask`).
extern "C" fn on_hotkey(
    _call: EventHandlerCallRef,
    event: EventRef,
    target: *mut c_void,
) -> OSStatus {
    let mut id = EventHotKeyID {
        signature: 0,
        id: 0,
    };
    // SAFETY: a hot-key event carries an EventHotKeyID, and `id` has room for exactly one
    let status = unsafe {
        GetEventParameter(
            event,
            EVENT_PARAM_DIRECT_OBJECT,
            TYPE_EVENT_HOT_KEY_ID,
            ptr::null_mut(),
            size_of::<EventHotKeyID>(),
            ptr::null_mut(),
            (&raw mut id).cast(),
        )
    };
    if status != NO_ERR || id.signature != SIGNATURE {
        return EVENT_NOT_HANDLED;
    }
    // SAFETY: the handler was installed with the delegate as its user data
    let target = unsafe { &*target.cast::<AnyObject>() };
    let sender = ptr::null_mut::<AnyObject>();
    match Action::from_id(id.id) {
        // SAFETY: the delegate implements both, each taking a sender
        Some(Action::Skip) => unsafe { msg_send![target, warningSkip: sender] },
        Some(Action::Postpone) => unsafe { msg_send![target, warningPostpone: sender] },
        None => return EVENT_NOT_HANDLED,
    }
    NO_ERR
}

fn carbon_modifiers(spec: KeySpec) -> u32 {
    [
        (spec.mods.cmd, CMD_KEY),
        (spec.mods.shift, SHIFT_KEY),
        (spec.mods.alt, OPTION_KEY),
        (spec.mods.ctrl, CONTROL_KEY),
    ]
    .into_iter()
    .filter(|&(held, _)| held)
    .fold(0, |mask, (_, bit)| mask | bit)
}

/// a hot key is a key code, not a character. letters come from the ASCII-capable layout, so
/// a shortcut keeps working while a Greek or other non-Latin layout is on.
fn key_code(key: Key) -> Option<u32> {
    match key {
        Key::Escape => Some(53),
        Key::Space => Some(49),
        Key::Enter => Some(36),
        Key::Tab => Some(48),
        Key::Delete => Some(51),
        Key::Char(c) => layout_key_code(c),
    }
}

fn layout_key_code(c: char) -> Option<u32> {
    let want = c.to_string();
    // SAFETY: the copied source is released below; the layout data belongs to it and isn't
    // used after the release
    unsafe {
        let source = TISCopyCurrentASCIICapableKeyboardLayoutInputSource();
        if source.is_null() {
            return None;
        }
        let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
        let code = if data.is_null() {
            None
        } else {
            let layout = CFDataGetBytePtr(data).cast::<c_void>();
            let keyboard = u32::from(LMGetKbdType());
            (0..128u16).find(|&code| {
                let mut dead_keys = 0;
                let mut chars = [0u16; 4];
                let mut len = 0;
                UCKeyTranslate(
                    layout,
                    code,
                    UC_KEY_ACTION_DISPLAY,
                    0,
                    keyboard,
                    UC_KEY_TRANSLATE_NO_DEAD_KEYS,
                    &mut dead_keys,
                    chars.len(),
                    &mut len,
                    chars.as_mut_ptr(),
                ) == NO_ERR
                    && String::from_utf16(&chars[..len]).is_ok_and(|typed| typed == want)
            })
        };
        CFRelease(source);
        code.map(u32::from)
    }
}

type OSStatus = i32;
type EventTargetRef = *mut c_void;
type EventHandlerCallRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventRef = *mut c_void;
type EventHotKeyRef = *mut c_void;
type EventHandlerUPP = extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OSStatus;

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

#[repr(C)]
struct EventHotKeyID {
    signature: u32,
    id: u32,
}

const NO_ERR: OSStatus = 0;
const EVENT_NOT_HANDLED: OSStatus = -9874;
/// 'omma'
const SIGNATURE: u32 = u32::from_be_bytes(*b"omma");
const EVENT_CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
const EVENT_HOT_KEY_PRESSED: u32 = 5;
const EVENT_PARAM_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
const TYPE_EVENT_HOT_KEY_ID: u32 = u32::from_be_bytes(*b"hkid");
const CMD_KEY: u32 = 1 << 8;
const SHIFT_KEY: u32 = 1 << 9;
const OPTION_KEY: u32 = 1 << 11;
const CONTROL_KEY: u32 = 1 << 12;
const UC_KEY_ACTION_DISPLAY: u16 = 3;
const UC_KEY_TRANSLATE_NO_DEAD_KEYS: u32 = 1;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: EventHandlerUPP,
        num_types: usize,
        list: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut EventHandlerRef,
    ) -> OSStatus;
    fn RegisterEventHotKey(
        code: u32,
        modifiers: u32,
        id: EventHotKeyID,
        target: EventTargetRef,
        options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OSStatus;
    fn UnregisterEventHotKey(hotkey: EventHotKeyRef) -> OSStatus;
    fn GetEventParameter(
        event: EventRef,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        buffer_size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> OSStatus;
    fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> *mut c_void;
    fn TISGetInputSourceProperty(source: *mut c_void, key: *const c_void) -> *const c_void;
    static kTISPropertyUnicodeKeyLayoutData: *const c_void;
    fn LMGetKbdType() -> u8;
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn UCKeyTranslate(
        layout: *const c_void,
        code: u16,
        action: u16,
        modifier_state: u32,
        keyboard_type: u32,
        options: u32,
        dead_key_state: *mut u32,
        max_len: usize,
        actual_len: *mut usize,
        chars: *mut u16,
    ) -> OSStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_map_to_carbon_bits() {
        let mask = |spec: &str| carbon_modifiers(spec.parse().unwrap());
        assert_eq!(mask("cmd+shift+option+space"), 0x100 | 0x200 | 0x800);
        assert_eq!(mask("ctrl+s"), 0x1000);
        assert_eq!(mask("s"), 0);
    }

    #[test]
    fn named_keys_have_fixed_key_codes() {
        let code = |spec: &str| key_code(spec.parse::<KeySpec>().unwrap().key);
        assert_eq!(code("escape"), Some(53));
        assert_eq!(code("space"), Some(49));
        assert_eq!(code("enter"), Some(36));
        assert_eq!(code("tab"), Some(48));
        assert_eq!(code("delete"), Some(51));
    }
}
