//! keyboard shortcuts as the config writes them: `escape`, `s`, `cmd+shift+s`

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// lowercase
    Char(char),
    Escape,
    Space,
    Enter,
    Tab,
    Delete,
}

/// the modifiers held down with a shortcut's key
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub cmd: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeySpec {
    pub key: Key,
    pub mods: Mods,
}

impl KeySpec {
    pub fn plain(key: Key) -> KeySpec {
        KeySpec {
            key,
            mods: Mods::default(),
        }
    }
}

fn key(name: &str) -> Option<Key> {
    Some(match name {
        "escape" | "esc" => Key::Escape,
        "space" => Key::Space,
        "enter" | "return" => Key::Enter,
        "tab" => Key::Tab,
        "delete" | "backspace" => Key::Delete,
        _ => {
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Key::Char(c),
                _ => return None,
            }
        }
    })
}

/// AppKit matches a shortcut against the character typed with shift applied. shift turns a
/// letter into its capital, and leaves only escape, space, enter and delete alone.
fn takes_shift(key: Key) -> bool {
    match key {
        Key::Char(c) => {
            let mut upper = c.to_uppercase();
            matches!((upper.next(), upper.next()), (Some(u), None) if u != c)
        }
        Key::Tab => false,
        Key::Escape | Key::Space | Key::Enter | Key::Delete => true,
    }
}

/// sets the modifier `name` stands for; false when it stands for none
fn set_modifier(mods: &mut Mods, name: &str) -> bool {
    let held = match name {
        "cmd" | "command" | "super" | "meta" => &mut mods.cmd,
        "shift" => &mut mods.shift,
        "alt" | "option" | "opt" => &mut mods.alt,
        "ctrl" | "control" => &mut mods.ctrl,
        _ => return false,
    };
    *held = true;
    true
}

impl FromStr for KeySpec {
    type Err = String;

    /// modifiers first, then one key, joined by `+`. case and spaces around parts don't matter.
    fn from_str(spec: &str) -> Result<KeySpec, String> {
        let lower = spec.to_lowercase();
        let mut parts: Vec<&str> = lower.split('+').map(str::trim).collect();
        if parts.iter().any(|part| part.is_empty()) {
            return Err(format!(
                "`{spec}` has an empty part; join keys with a single +"
            ));
        }
        let last = parts.pop().unwrap_or_default();

        let mut mods = Mods::default();
        for part in parts {
            if !set_modifier(&mut mods, part) {
                return Err(if key(part).is_none() {
                    format!("`{part}` in `{spec}` isn't a modifier; use cmd, shift, alt or ctrl")
                } else if set_modifier(&mut Mods::default(), last) {
                    format!("`{spec}` has a modifier after its key; put modifiers first")
                } else {
                    format!("`{spec}` has more than one key")
                });
            }
        }
        let Some(key) = key(last) else {
            return Err(if set_modifier(&mut Mods::default(), last) {
                format!("`{spec}` needs a key after its modifiers")
            } else {
                format!(
                    "`{last}` isn't a key; use a single character, or escape, space, enter, \
                     tab or delete"
                )
            });
        };
        if mods.shift && !takes_shift(key) {
            return Err(format!(
                "`{spec}` can't fire: shift goes only with a letter, or escape, space, enter or \
                 delete"
            ));
        }
        Ok(KeySpec { key, mods })
    }
}

impl fmt::Display for KeySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Mods {
            ctrl,
            alt,
            shift,
            cmd,
        } = self.mods;
        for (held, name) in [(ctrl, "ctrl"), (alt, "alt"), (shift, "shift"), (cmd, "cmd")] {
            if held {
                write!(f, "{name}+")?;
            }
        }
        match self.key {
            Key::Char(c) => write!(f, "{c}"),
            Key::Escape => f.write_str("escape"),
            Key::Space => f.write_str("space"),
            Key::Enter => f.write_str("enter"),
            Key::Tab => f.write_str("tab"),
            Key::Delete => f.write_str("delete"),
        }
    }
}

/// a shortcut, or None for `""`
pub(super) fn optional<'de, D: Deserializer<'de>>(de: D) -> Result<Option<KeySpec>, D::Error> {
    let spec = String::deserialize(de)?;
    if spec.trim().is_empty() {
        return Ok(None);
    }
    spec.parse().map(Some).map_err(serde::de::Error::custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(spec: &str) -> KeySpec {
        spec.parse()
            .unwrap_or_else(|e| panic!("{spec:?} didn't parse: {e}"))
    }

    fn error(spec: &str) -> String {
        spec.parse::<KeySpec>().unwrap_err()
    }

    #[test]
    fn named_keys_and_their_aliases() {
        for (names, key) in [
            (&["escape", "esc"][..], Key::Escape),
            (&["space"], Key::Space),
            (&["enter", "return"], Key::Enter),
            (&["tab"], Key::Tab),
            (&["delete", "backspace"], Key::Delete),
        ] {
            for name in names {
                assert_eq!(parse(name), KeySpec::plain(key), "{name}");
            }
        }
    }

    #[test]
    fn a_single_character_is_a_key_and_comes_out_lowercase() {
        assert_eq!(parse("s"), KeySpec::plain(Key::Char('s')));
        assert_eq!(parse("S"), KeySpec::plain(Key::Char('s')));
        assert_eq!(parse("/"), KeySpec::plain(Key::Char('/')));
        assert_eq!(parse("σ"), KeySpec::plain(Key::Char('σ')));
    }

    #[test]
    fn modifiers_and_their_aliases() {
        let cmd = Mods {
            cmd: true,
            ..Mods::default()
        };
        let shift = Mods {
            shift: true,
            ..Mods::default()
        };
        let alt = Mods {
            alt: true,
            ..Mods::default()
        };
        let ctrl = Mods {
            ctrl: true,
            ..Mods::default()
        };
        for (names, mods) in [
            (&["cmd", "command", "super", "meta"][..], cmd),
            (&["shift"], shift),
            (&["alt", "option", "opt"], alt),
            (&["ctrl", "control"], ctrl),
        ] {
            for name in names {
                let spec = parse(&format!("{name}+s"));
                assert_eq!(spec.mods, mods, "{name}");
                assert_eq!(spec.key, Key::Char('s'));
            }
        }
    }

    #[test]
    fn combinations_ignore_case_spaces_and_repeats() {
        let all = KeySpec {
            key: Key::Char('x'),
            mods: Mods {
                ctrl: true,
                alt: true,
                shift: true,
                cmd: true,
            },
        };
        assert_eq!(parse(" Ctrl + Option+SHIFT+cmd + X "), all);
        assert_eq!(parse("cmd+cmd+shift+alt+ctrl+x"), all);
        assert_eq!(all.to_string(), "ctrl+alt+shift+cmd+x");
        assert_eq!(parse(&all.to_string()), all);
        assert_eq!(parse("cmd+delete").to_string(), "cmd+delete");
    }

    #[test]
    fn shift_goes_with_letters_and_keys_it_leaves_alone() {
        for spec in [
            "shift+s",
            "shift+σ",
            "shift+escape",
            "shift+space",
            "shift+enter",
            "shift+delete",
        ] {
            assert!(parse(spec).mods.shift, "{spec}");
        }
        for spec in ["shift+1", "shift+/", "shift+ß", "shift+tab"] {
            assert_eq!(
                error(spec),
                format!(
                    "`{spec}` can't fire: shift goes only with a letter, or escape, space, enter \
                     or delete"
                )
            );
        }
    }

    #[test]
    fn bad_specs_say_what_is_wrong() {
        let not_a_key = "isn't a key; use a single character, or escape, space, enter, tab or \
                         delete";
        assert_eq!(error("f1"), format!("`f1` {not_a_key}"));
        assert_eq!(error("escpae"), format!("`escpae` {not_a_key}"));
        assert_eq!(error("cmd+ab"), format!("`ab` {not_a_key}"));
        assert_eq!(error("cmd"), "`cmd` needs a key after its modifiers");
        assert_eq!(
            error("shift+opt"),
            "`shift+opt` needs a key after its modifiers"
        );
        assert_eq!(error("s+d"), "`s+d` has more than one key");
        assert_eq!(error("cmd+esc+s"), "`cmd+esc+s` has more than one key");
        assert_eq!(
            error("s+cmd"),
            "`s+cmd` has a modifier after its key; put modifiers first"
        );
        assert_eq!(
            error("ctl+s"),
            "`ctl` in `ctl+s` isn't a modifier; use cmd, shift, alt or ctrl"
        );
        let empty = "has an empty part; join keys with a single +";
        assert_eq!(error("cmd++s"), format!("`cmd++s` {empty}"));
        assert_eq!(error("cmd+"), format!("`cmd+` {empty}"));
        assert_eq!(error("+"), format!("`+` {empty}"));
    }
}
