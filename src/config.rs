//! User configuration, stored as TOML at `~/.config/omma/config.toml`.
//!
//! A default file is written on first run. Unknown/missing keys fall back to
//! defaults, so the file can be partial.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::{Break, BreakQueue, BreakType};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Seconds between short breaks.
    pub short_break_interval: i64,
    /// Seconds between long breaks.
    pub long_break_interval: i64,
    /// Short break length, seconds.
    pub short_break_duration: i64,
    /// Long break length, seconds.
    pub long_break_duration: i64,
    /// Seconds of warning shown before a break begins.
    pub pre_break_warning: i64,
    /// Seconds a postponed break is delayed.
    pub postpone_duration: i64,
    pub allow_postpone: bool,
    /// Strict mode: breaks cannot be skipped or postponed.
    pub strict_break: bool,
    /// Shuffle the order breaks are shown in.
    pub random_order: bool,

    pub short_breaks: Vec<BreakCfg>,
    pub long_breaks: Vec<BreakCfg>,

    pub keybindings: Keybindings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreakCfg {
    pub name: String,
    /// Override the global interval for this break (seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<i64>,
    /// Override the global duration for this break (seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<i64>,
    /// Optional image path shown on the break screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

/// Keys that skip or postpone the break while the break screen is showing.
///
/// Each value is a key spec like `"escape"`, `"space"`, a single letter
/// (`"s"`), or a combination with modifiers (`"cmd+s"`, `"shift+p"`).
/// Recognised modifiers: `cmd`, `shift`, `alt`/`option`, `ctrl`. Recognised
/// named keys: `escape`/`esc`, `space`, `enter`/`return`, `tab`,
/// `delete`/`backspace`. An empty string disables that keybind. A keybind only
/// works when its action is allowed (postpone needs `allow_postpone`, and
/// neither works under `strict_break`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Keybindings {
    pub skip: String,
    pub postpone: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            short_break_interval: 900,
            long_break_interval: 4500,
            short_break_duration: 15,
            long_break_duration: 60,
            pre_break_warning: 10,
            postpone_duration: 300,
            allow_postpone: false,
            strict_break: false,
            random_order: true,
            short_breaks: default_short_breaks(),
            long_breaks: default_long_breaks(),
            keybindings: Keybindings::default(),
        }
    }
}

impl Default for Keybindings {
    fn default() -> Self {
        Keybindings {
            skip: "escape".into(),
            postpone: "space".into(),
        }
    }
}

fn names_to_breaks(names: &[&str]) -> Vec<BreakCfg> {
    names
        .iter()
        .map(|n| BreakCfg {
            name: (*n).to_string(),
            interval: None,
            duration: None,
            image: None,
        })
        .collect()
}

fn default_short_breaks() -> Vec<BreakCfg> {
    names_to_breaks(&[
        "Gently close your eyes",
        "Roll your eyes a few times to each side",
        "Rotate your eyes in clockwise direction",
        "Rotate your eyes in counterclockwise direction",
        "Blink your eyes",
        "Focus on a point in the far distance",
        "Have some water",
        "Relax your shoulders and drop them away from your ears",
        "Stretch your neck gently from side to side",
        "Check your posture and sit back comfortably",
        "Unclench your jaw and relax your face",
        "Stretch your wrists and fingers",
    ])
}

fn default_long_breaks() -> Vec<BreakCfg> {
    names_to_breaks(&[
        "Stand up and stretch your back and legs",
        "Walk around for a while",
        "Lean back at your seat and relax",
        "Step away from the screen and look out a window",
        "Walk over and refill your water",
    ])
}

impl Config {
    /// Build the scheduler queue from this config. Returns `None` when both
    /// break lists are empty.
    pub fn build_queue(&self) -> Option<BreakQueue> {
        let shorts = self
            .short_breaks
            .iter()
            .map(|b| Break {
                kind: BreakType::Short,
                name: b.name.clone(),
                interval: b.interval.unwrap_or(self.short_break_interval),
                duration: b.duration.unwrap_or(self.short_break_duration),
                image: b.image.clone(),
            })
            .collect();
        let longs = self
            .long_breaks
            .iter()
            .map(|b| Break {
                kind: BreakType::Long,
                name: b.name.clone(),
                interval: b.interval.unwrap_or(self.long_break_interval),
                duration: b.duration.unwrap_or(self.long_break_duration),
                image: b.image.clone(),
            })
            .collect();
        BreakQueue::create(
            self.short_break_interval,
            self.long_break_interval,
            self.random_order,
            shorts,
            longs,
        )
    }

    pub fn default_toml() -> anyhow::Result<String> {
        Ok(toml::to_string_pretty(&Config::default())?)
    }

    /// Load config, writing a default file if none exists.
    pub fn load() -> Config {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(data) => match toml::from_str(&data) {
                Ok(cfg) => cfg,
                Err(e) => {
                    eprintln!("omma: invalid config at {}: {e}", path.display());
                    eprintln!("omma: falling back to defaults");
                    Config::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Config::default();
                if let Err(e) = write_default(&path, &cfg) {
                    eprintln!("omma: could not write default config to {}: {e}", path.display());
                }
                cfg
            }
            Err(e) => {
                eprintln!("omma: could not read config at {}: {e}", path.display());
                Config::default()
            }
        }
    }
}

fn write_default(path: &PathBuf, cfg: &Config) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = toml::to_string_pretty(cfg).map_err(std::io::Error::other)?;
    std::fs::write(path, data)
}

pub fn config_path() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join(".config"))
                .unwrap_or_else(|_| PathBuf::from(".config"))
        })
        .join("omma")
        .join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_round_trips_through_toml() {
        let toml = Config::default_toml().unwrap();
        let parsed: Config = toml::from_str(&toml).unwrap();
        assert_eq!(parsed.short_break_interval, 900);
        assert_eq!(parsed.keybindings.skip, "escape");
        assert_eq!(parsed.keybindings.postpone, "space");
        assert!(parsed.build_queue().is_some());
    }

    #[test]
    fn partial_config_uses_defaults() {
        let cfg: Config = toml::from_str("short_break_interval = 20\n").unwrap();
        assert_eq!(cfg.short_break_interval, 20);
        assert_eq!(cfg.long_break_interval, 4500); // default preserved
    }
}
