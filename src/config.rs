use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::{Break, BreakQueue, BreakType};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub short_break_interval: i64,
    pub long_break_interval: i64,
    pub short_break_duration: i64,
    pub long_break_duration: i64,
    pub pre_break_warning: i64,
    pub postpone_duration: i64,
    pub allow_postpone: bool,
    /// if true, breaks cannot be skipped or postponed
    pub strict_break: bool,
    pub random_order: bool,

    pub short_breaks: Vec<BreakCfg>,
    pub long_breaks: Vec<BreakCfg>,

    pub smart_pause: SmartPause,
    pub audible_alert: AudibleAlert,
    pub notification: NotificationCfg,
    pub keybindings: Keybindings,
    pub do_not_disturb: DoNotDisturb,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreakCfg {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SmartPause {
    pub enabled: bool,
    pub idle_seconds: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudibleAlert {
    pub enabled: bool,
    pub pre_break: bool,
    pub post_break: bool,
    /// system sound name from /System/Library/Sounds
    pub pre_break_sound: String,
    pub post_break_sound: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationCfg {
    pub enabled: bool,
}

/// key spec accepts `escape`, `space`, a letter like `s`, or combos like `cmd+s`.
/// modifiers: cmd, shift, alt/option, ctrl. named keys: escape/esc, space,
/// enter/return, tab, delete/backspace. empty string disables the bind.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Keybindings {
    pub skip: String,
    pub postpone: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DoNotDisturb {
    pub enabled: bool,
    /// case-insensitive substring match against app name or bundle id
    pub skip_when_app_running: Vec<String>,
    pub skip_when_app_active: Vec<String>,
    /// needs accessibility permission
    pub skip_when_fullscreen: bool,
    /// regex match against on-screen window titles. needs accessibility permission
    pub window_title_matches: Vec<String>,
    /// `skip` advances to the next break, `postpone` retries after the postpone delay
    pub action: DndAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DndAction {
    Skip,
    Postpone,
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
            smart_pause: SmartPause::default(),
            audible_alert: AudibleAlert::default(),
            notification: NotificationCfg::default(),
            keybindings: Keybindings::default(),
            do_not_disturb: DoNotDisturb::default(),
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

impl Default for SmartPause {
    fn default() -> Self {
        SmartPause {
            enabled: true,
            idle_seconds: 60,
        }
    }
}

impl Default for AudibleAlert {
    fn default() -> Self {
        AudibleAlert {
            enabled: true,
            pre_break: true,
            post_break: true,
            pre_break_sound: "Tink".into(),
            post_break_sound: "Glass".into(),
        }
    }
}

impl Default for NotificationCfg {
    fn default() -> Self {
        NotificationCfg { enabled: true }
    }
}

impl Default for DoNotDisturb {
    fn default() -> Self {
        DoNotDisturb {
            enabled: true,
            skip_when_app_running: vec![],
            skip_when_app_active: vec![],
            skip_when_fullscreen: false,
            window_title_matches: vec![],
            action: DndAction::Skip,
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
    pub fn validate(&self) -> anyhow::Result<()> {
        let mut errors = Vec::new();

        require_positive(
            "short_break_interval",
            self.short_break_interval,
            &mut errors,
        );
        require_positive("long_break_interval", self.long_break_interval, &mut errors);
        require_positive(
            "short_break_duration",
            self.short_break_duration,
            &mut errors,
        );
        require_positive("long_break_duration", self.long_break_duration, &mut errors);
        require_non_negative("pre_break_warning", self.pre_break_warning, &mut errors);
        require_positive("postpone_duration", self.postpone_duration, &mut errors);
        require_positive(
            "smart_pause.idle_seconds",
            self.smart_pause.idle_seconds,
            &mut errors,
        );

        validate_breaks("short_breaks", &self.short_breaks, &mut errors);
        validate_breaks("long_breaks", &self.long_breaks, &mut errors);

        if self.short_breaks.is_empty() && self.long_breaks.is_empty() {
            errors.push("at least one short_breaks or long_breaks entry is required".to_string());
        }

        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("{}", errors.join("\n"))
        }
    }

    pub fn build_queue(&self) -> anyhow::Result<BreakQueue> {
        self.validate()?;

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
        .ok_or_else(|| anyhow::anyhow!("at least one break must be configured"))
    }

    pub fn default_toml() -> anyhow::Result<String> {
        Ok(toml::to_string_pretty(&Config::default())?)
    }

    /// write a default file if none exists
    pub fn load() -> Config {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(data) => match toml::from_str::<Config>(&data) {
                Ok(cfg) => match cfg.validate() {
                    Ok(()) => cfg,
                    Err(e) => {
                        eprintln!("omma: invalid config at {}: {e}", path.display());
                        eprintln!("omma: falling back to defaults");
                        Config::default()
                    }
                },
                Err(e) => {
                    eprintln!("omma: invalid config at {}: {e}", path.display());
                    eprintln!("omma: falling back to defaults");
                    Config::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Config::default();
                if let Err(e) = write_default(&path, &cfg) {
                    eprintln!(
                        "omma: could not write default config to {}: {e}",
                        path.display()
                    );
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

fn require_positive(name: &str, value: i64, errors: &mut Vec<String>) {
    if value <= 0 {
        errors.push(format!("{name} must be greater than 0, got {value}"));
    }
}

fn require_non_negative(name: &str, value: i64, errors: &mut Vec<String>) {
    if value < 0 {
        errors.push(format!("{name} must be 0 or greater, got {value}"));
    }
}

fn validate_breaks(section: &str, breaks: &[BreakCfg], errors: &mut Vec<String>) {
    for (idx, b) in breaks.iter().enumerate() {
        let prefix = format!("{section}[{idx}]");
        if b.name.trim().is_empty() {
            errors.push(format!("{prefix}.name must not be empty"));
        }
        if let Some(interval) = b.interval {
            require_positive(&format!("{prefix}.interval"), interval, errors);
        }
        if let Some(duration) = b.duration {
            require_positive(&format!("{prefix}.duration"), duration, errors);
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
        assert_eq!(parsed.do_not_disturb.action, DndAction::Skip);
        assert!(!parsed.do_not_disturb.skip_when_fullscreen);
        assert_eq!(parsed.keybindings.skip, "escape");
        assert_eq!(parsed.keybindings.postpone, "space");
        assert!(parsed.build_queue().is_ok());
    }

    #[test]
    fn partial_config_uses_defaults() {
        let cfg: Config = toml::from_str("short_break_interval = 20\n").unwrap();
        assert_eq!(cfg.short_break_interval, 20);
        assert_eq!(cfg.long_break_interval, 4500);
        assert!(cfg.smart_pause.enabled);
    }

    #[test]
    fn validation_rejects_non_positive_schedule_values() {
        let mut cfg = Config {
            short_break_interval: 0,
            ..Config::default()
        };
        cfg.long_breaks[0].duration = Some(-1);

        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("short_break_interval must be greater than 0"));
        assert!(err.contains("long_breaks[0].duration must be greater than 0"));
    }

    #[test]
    fn validation_rejects_empty_break_names() {
        let mut cfg = Config::default();
        cfg.short_breaks[0].name = " \t ".into();

        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("short_breaks[0].name must not be empty"));
    }
}
