//! the seat's pointer and keyboard, turned into the few events that omma's surfaces care
//! about. these are where the pointer is, presses and releases of its main button, and key
//! presses.

use wayland_client::protocol::{wl_keyboard, wl_pointer, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1, wp_cursor_shape_manager_v1,
};
use xkbcommon::xkb;

use super::desktop::Desktop;
use crate::config::{Key, KeySpec, Mods};

/// the pointer's main button, as Linux's input codes number it
const BTN_LEFT: u32 = 0x110;

#[derive(Debug)]
pub enum Event {
    /// the pointer is at (x, y) on surface `on`, in its logical pixels
    Motion {
        on: u32,
        x: f32,
        y: f32,
    },
    Leave {
        on: u32,
    },
    Press {
        on: u32,
        x: f32,
        y: f32,
    },
    Release {
        on: u32,
        x: f32,
        y: f32,
    },
    Key {
        key: KeySpec,
    },
}

struct Xkb {
    _context: xkb::Context,
    _keymap: xkb::Keymap,
    state: xkb::State,
}

#[derive(Default)]
pub struct Input {
    seat: Option<(u32, wl_seat::WlSeat)>,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pub cursor_shape: Option<wp_cursor_shape_manager_v1::WpCursorShapeManagerV1>,
    cursor: Option<wp_cursor_shape_device_v1::WpCursorShapeDeviceV1>,
    /// the surface under the pointer, and where on it
    pointer_on: Option<u32>,
    at: (f32, f32),
    keyboard_on: Option<u32>,
    xkb: Option<Xkb>,
    pub events: Vec<Event>,
}

impl Input {
    pub fn bind_seat(
        &mut self,
        registry: &wl_registry::WlRegistry,
        name: u32,
        version: u32,
        qh: &QueueHandle<Desktop>,
    ) {
        // one seat is all omma needs. a desktop with several seats has its main one first
        if self.seat.is_none() {
            let seat = registry.bind(name, version.min(7), qh, ());
            self.seat = Some((name, seat));
        }
    }

    pub fn remove_seat(&mut self, name: u32) {
        if self
            .seat
            .as_ref()
            .is_some_and(|(global, _)| *global == name)
        {
            self.seat = None;
            self.pointer = None;
            self.keyboard = None;
            self.cursor = None;
        }
    }

    /// the key `keycode` stands for with the modifiers held now, as a shortcut spells it
    fn key(&self, keycode: u32) -> Option<KeySpec> {
        spec(&self.xkb.as_ref()?.state, keycode)
    }

    fn set_cursor(&self, serial: u32) {
        if let Some(cursor) = &self.cursor {
            cursor.set_shape(serial, wp_cursor_shape_device_v1::Shape::Default);
        }
    }
}

/// the key at the Linux key code `keycode`, with the modifiers held now, as a shortcut spells
/// it. the character comes from the keysym, which has Shift's level but not Control's
/// transformation: with Control held, the typed text is a control character, not the letter
fn spec(state: &xkb::State, keycode: u32) -> Option<KeySpec> {
    let code = xkb::Keycode::new(keycode + 8);
    let sym = state.key_get_one_sym(code);
    let active = |name: &str| state.mod_name_is_active(name, xkb::STATE_MODS_EFFECTIVE);
    let mods = Mods {
        ctrl: active(xkb::MOD_NAME_CTRL),
        alt: active(xkb::MOD_NAME_ALT),
        shift: active(xkb::MOD_NAME_SHIFT),
        cmd: active(xkb::MOD_NAME_LOGO),
    };
    use xkb::keysyms;
    let key = match sym.raw() {
        keysyms::KEY_Escape => Key::Escape,
        keysyms::KEY_space => Key::Space,
        keysyms::KEY_Return | keysyms::KEY_KP_Enter => Key::Enter,
        keysyms::KEY_Tab | keysyms::KEY_ISO_Left_Tab => Key::Tab,
        keysyms::KEY_BackSpace | keysyms::KEY_Delete => Key::Delete,
        _ => {
            let c = sym.key_char().filter(|c| !c.is_control())?;
            Key::Char(c.to_lowercase().next().unwrap_or(c))
        }
    };
    Some(KeySpec { key, mods })
}

impl Dispatch<wl_seat::WlSeat, ()> for Desktop {
    fn event(
        state: &mut Desktop,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Desktop>,
    ) {
        let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        else {
            return;
        };
        let input = &mut state.input;
        if caps.contains(wl_seat::Capability::Pointer) {
            if input.pointer.is_none() {
                let pointer = seat.get_pointer(qh, ());
                input.cursor = input
                    .cursor_shape
                    .as_ref()
                    .map(|manager| manager.get_pointer(&pointer, qh, ()));
                input.pointer = Some(pointer);
            }
        } else if let Some(pointer) = input.pointer.take() {
            if pointer.version() >= 3 {
                pointer.release();
            }
            input.cursor = None;
        }
        if caps.contains(wl_seat::Capability::Keyboard) {
            if input.keyboard.is_none() {
                input.keyboard = Some(seat.get_keyboard(qh, ()));
            }
        } else if let Some(keyboard) = input.keyboard.take() {
            if keyboard.version() >= 3 {
                keyboard.release();
            }
            input.xkb = None;
        }
        // smart pause watches this seat
        state
            .idle
            .seat_changed(input.seat.as_ref().map(|(_, seat)| seat), qh);
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Desktop {
    fn event(
        state: &mut Desktop,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Desktop>,
    ) {
        let Desktop { input, shell, .. } = state;
        match event {
            wl_pointer::Event::Enter {
                serial,
                surface,
                surface_x,
                surface_y,
            } => {
                input.set_cursor(serial);
                input.pointer_on = shell.by_wl(&surface);
                input.at = (surface_x as f32, surface_y as f32);
                if let Some(on) = input.pointer_on {
                    let (x, y) = input.at;
                    input.events.push(Event::Motion { on, x, y });
                }
            }
            wl_pointer::Event::Leave { .. } => {
                if let Some(on) = input.pointer_on.take() {
                    input.events.push(Event::Leave { on });
                }
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                input.at = (surface_x as f32, surface_y as f32);
                if let Some(on) = input.pointer_on {
                    let (x, y) = input.at;
                    input.events.push(Event::Motion { on, x, y });
                }
            }
            wl_pointer::Event::Button {
                button,
                state: WEnum::Value(pressed),
                ..
            } if button == BTN_LEFT => {
                let Some(on) = input.pointer_on else {
                    return;
                };
                let (x, y) = input.at;
                input.events.push(match pressed {
                    wl_pointer::ButtonState::Pressed => Event::Press { on, x, y },
                    _ => Event::Release { on, x, y },
                });
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Desktop {
    fn event(
        state: &mut Desktop,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Desktop>,
    ) {
        let Desktop { input, shell, .. } = state;
        match event {
            wl_keyboard::Event::Keymap {
                format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1),
                fd,
                size,
            } => {
                let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
                // SAFETY: the compositor sends a readable keymap of `size` bytes, which is
                // mapped privately and read only
                let keymap = unsafe {
                    xkb::Keymap::new_from_fd(
                        &context,
                        fd,
                        size as usize,
                        xkb::KEYMAP_FORMAT_TEXT_V1,
                        xkb::KEYMAP_COMPILE_NO_FLAGS,
                    )
                };
                if let Ok(Some(keymap)) = keymap {
                    let state = xkb::State::new(&keymap);
                    input.xkb = Some(Xkb {
                        _context: context,
                        _keymap: keymap,
                        state,
                    });
                }
            }
            // keys already down as focus arrives were pressed for something else, and count
            // for nothing here
            wl_keyboard::Event::Enter { surface, .. } => {
                input.keyboard_on = shell.by_wl(&surface);
            }
            wl_keyboard::Event::Leave { .. } => input.keyboard_on = None,
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                if let Some(xkb) = &mut input.xkb {
                    xkb.state
                        .update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
            }
            wl_keyboard::Event::Key {
                key,
                state: WEnum::Value(wl_keyboard::KeyState::Pressed),
                ..
            } => {
                if input.keyboard_on.is_some()
                    && let Some(spec) = input.key(key)
                {
                    input.events.push(Event::Key { key: spec });
                }
            }
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(Desktop: ignore wp_cursor_shape_manager_v1::WpCursorShapeManagerV1);
wayland_client::delegate_noop!(Desktop: ignore wp_cursor_shape_device_v1::WpCursorShapeDeviceV1);

#[cfg(test)]
mod tests {
    use super::*;

    /// Linux key codes of the keys the tests press
    const KEY_S: u32 = 31;
    const KEY_ESC: u32 = 1;

    /// a "us" keymap's state, with the named modifiers held
    fn us(held: &[&str]) -> xkb::State {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &context,
            "",
            "",
            "us",
            "",
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .expect("the us layout is installed");
        let mask = held
            .iter()
            .fold(0, |mask, name| mask | 1 << keymap.mod_get_index(*name));
        let mut state = xkb::State::new(&keymap);
        state.update_mask(mask, 0, 0, 0, 0, 0);
        state
    }

    #[test]
    fn a_key_with_control_held_is_its_letter_with_ctrl() {
        assert_eq!(
            spec(&us(&[xkb::MOD_NAME_CTRL]), KEY_S),
            Some("ctrl+s".parse().unwrap())
        );
        assert_eq!(
            spec(&us(&[xkb::MOD_NAME_CTRL, xkb::MOD_NAME_SHIFT]), KEY_S),
            Some("ctrl+shift+s".parse().unwrap())
        );
    }

    #[test]
    fn plain_and_named_keys_spell_as_the_config_does() {
        assert_eq!(spec(&us(&[]), KEY_S), Some("s".parse().unwrap()));
        assert_eq!(spec(&us(&[]), KEY_ESC), Some("escape".parse().unwrap()));
        assert_eq!(
            spec(&us(&[xkb::MOD_NAME_LOGO]), KEY_ESC),
            Some("super+escape".parse().unwrap())
        );
    }
}
