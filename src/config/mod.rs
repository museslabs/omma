//! the config file, parsed once into checked, typed settings. an unknown key, a value out of
//! range or a broken rule rejects the whole file, with an error that says where.

mod keys;

use std::ffi::OsString;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Deserializer};
use toml::Spanned;
use toml::de::{DeTable, DeValue};

pub use keys::{Key, KeySpec};

use crate::queue::{Break, BreakType, Breaks};
use crate::scheduler::Settings;

/// commented, and equal to `Config::default()`; written on first run
pub const DEFAULT_CONFIG: &str = include_str!("default.toml");

/// a config that passed every check
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub schedule: Settings,
    pub audible_alert: AudibleAlert,
    pub notification: Notification,
    pub keybindings: Keybindings,
    pub do_not_disturb: DoNotDisturb,
}

/// the file as written. `Default` fills in what it leaves out.
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ConfigFile {
    short_break_interval: Interval,
    long_break_interval: Interval,
    short_break_duration: BreakLength,
    long_break_duration: BreakLength,
    pre_break_warning: WarningTime,
    postpone_duration: Interval,
    allow_postpone: bool,
    strict_break: bool,
    random_order: bool,
    short_breaks: Vec<BreakEntry>,
    long_breaks: Vec<BreakEntry>,
    smart_pause: SmartPause,
    audible_alert: AudibleAlert,
    notification: Notification,
    keybindings: Keybindings,
    do_not_disturb: DoNotDisturb,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BreakEntry {
    name: BreakName,
    duration: Option<BreakLength>,
    image: Option<String>,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SmartPause {
    enabled: bool,
    idle_seconds: IdleTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudibleAlert {
    pub enabled: bool,
    pub pre_break: bool,
    pub post_break: bool,
    /// system sound name from /System/Library/Sounds
    pub pre_break_sound: String,
    pub post_break_sound: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Notification {
    pub enabled: bool,
}

/// shortcuts for Skip and Postpone; None turns one off. `skip` and `postpone` act on the
/// break screen, the `warning_` pair from any app while the warning shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Keybindings {
    #[serde(deserialize_with = "keys::optional")]
    pub skip: Option<KeySpec>,
    #[serde(deserialize_with = "keys::optional")]
    pub postpone: Option<KeySpec>,
    #[serde(deserialize_with = "keys::optional_global")]
    pub warning_skip: Option<KeySpec>,
    #[serde(deserialize_with = "keys::optional_global")]
    pub warning_postpone: Option<KeySpec>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DoNotDisturb {
    pub enabled: bool,
    pub skip_when_app_running: Vec<AppName>,
    /// the same, for the frontmost app only
    pub skip_when_app_active: Vec<AppName>,
    /// needs accessibility permission
    pub skip_when_fullscreen: bool,
    /// matched against on-screen window titles. needs accessibility permission
    pub window_title_matches: Vec<TitlePattern>,
    /// `skip` advances to the next break, `postpone` retries after the postpone delay
    pub action: DndAction,
}

impl DoNotDisturb {
    /// whether a rule reads other apps' windows, which needs Accessibility permission
    pub fn needs_accessibility(&self) -> bool {
        self.enabled && (self.skip_when_fullscreen || !self.window_title_matches.is_empty())
    }
}

/// part of an app's name or bundle id, lowercase
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct AppName(String);

impl AppName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AppName {
    type Error = &'static str;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        let name = name.trim().to_lowercase();
        if name.is_empty() {
            Err("an empty app name would match every app")
        } else {
            Ok(AppName(name))
        }
    }
}

/// a case-insensitive regex. a plain word matches anywhere in a title.
#[derive(Clone, Debug, Deserialize)]
#[serde(try_from = "String")]
pub struct TitlePattern(Regex);

impl TitlePattern {
    pub fn is_match(&self, title: &str) -> bool {
        self.0.is_match(title)
    }
}

impl TryFrom<String> for TitlePattern {
    type Error = String;

    fn try_from(pattern: String) -> Result<Self, String> {
        if pattern.is_empty() {
            return Err("an empty pattern would match every window".into());
        }
        RegexBuilder::new(&pattern)
            .case_insensitive(true)
            .build()
            .map(TitlePattern)
            .map_err(|e| {
                // the reason is the last line, under a picture of the pattern
                let text = e.to_string();
                let reason = text.lines().last().unwrap_or_default();
                let reason = reason.strip_prefix("error: ").unwrap_or(reason);
                format!("`{pattern}` isn't a valid regex: {reason}")
            })
    }
}

/// patterns are equal when their source is
impl PartialEq for TitlePattern {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_str() == other.0.as_str()
    }
}

impl Eq for TitlePattern {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DndAction {
    Skip,
    Postpone,
}

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

/// time between breaks, and the postpone delay
type Interval = Secs<1, DAY>;
type BreakLength = Secs<1, HOUR>;
type WarningTime = Secs<0, { 10 * MINUTE }>;
/// below 10 s, any pause to read or think would pause the schedule
type IdleTime = Secs<10, DAY>;

/// whole seconds in `MIN..=MAX`. the bounds keep deadlines far from overflowing.
#[derive(Clone, Copy)]
struct Secs<const MIN: u64, const MAX: u64>(u64);

impl<const MIN: u64, const MAX: u64> Secs<MIN, MAX> {
    fn duration(self) -> Duration {
        Duration::from_secs(self.0)
    }
}

impl<'de, const MIN: u64, const MAX: u64> Deserialize<'de> for Secs<MIN, MAX> {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct Visitor<const MIN: u64, const MAX: u64>;

        impl<const MIN: u64, const MAX: u64> serde::de::Visitor<'_> for Visitor<MIN, MAX> {
            type Value = Secs<MIN, MAX>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a whole number of seconds")
            }

            fn visit_i64<E: serde::de::Error>(self, secs: i64) -> Result<Self::Value, E> {
                u64::try_from(secs)
                    .ok()
                    .filter(|s| (MIN..=MAX).contains(s))
                    .map(Secs)
                    .ok_or_else(|| E::custom(format!("must be {MIN} to {MAX} seconds, got {secs}")))
            }
        }

        de.deserialize_i64(Visitor)
    }
}

#[derive(Deserialize)]
#[serde(try_from = "String")]
struct BreakName(String);

impl TryFrom<String> for BreakName {
    type Error = &'static str;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        if name.trim().is_empty() {
            Err("a break needs a name")
        } else {
            Ok(BreakName(name))
        }
    }
}

impl Default for ConfigFile {
    fn default() -> Self {
        ConfigFile {
            short_break_interval: Secs(15 * MINUTE),
            long_break_interval: Secs(75 * MINUTE),
            short_break_duration: Secs(15),
            long_break_duration: Secs(MINUTE),
            pre_break_warning: Secs(10),
            postpone_duration: Secs(5 * MINUTE),
            allow_postpone: false,
            strict_break: false,
            random_order: true,
            short_breaks: entries(&[
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
            ]),
            long_breaks: entries(&[
                "Stand up and stretch your back and legs",
                "Walk around for a while",
                "Lean back at your seat and relax",
                "Step away from the screen and look out a window",
                "Walk over and refill your water",
            ]),
            smart_pause: SmartPause::default(),
            audible_alert: AudibleAlert::default(),
            notification: Notification::default(),
            keybindings: Keybindings::default(),
            do_not_disturb: DoNotDisturb::default(),
        }
    }
}

fn entries(names: &[&str]) -> Vec<BreakEntry> {
    names
        .iter()
        .map(|name| BreakEntry {
            name: BreakName((*name).to_owned()),
            duration: None,
            image: None,
        })
        .collect()
}

impl Default for SmartPause {
    fn default() -> Self {
        SmartPause {
            enabled: true,
            idle_seconds: Secs(MINUTE),
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

impl Default for Notification {
    fn default() -> Self {
        Notification { enabled: true }
    }
}

impl Default for Keybindings {
    fn default() -> Self {
        Keybindings {
            skip: Some(KeySpec::plain(Key::Escape)),
            postpone: Some(KeySpec::plain(Key::Space)),
            warning_skip: None,
            warning_postpone: None,
        }
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

impl Default for Config {
    fn default() -> Self {
        ConfigFile::default()
            .check(None)
            .expect("the built-in defaults pass the checks")
    }
}

impl ConfigFile {
    /// the rules that span keys, and the images' paths. `~/` in a path stands for `home`.
    fn check(self, home: Option<&Path>) -> Result<Config, String> {
        let has_both = !self.short_breaks.is_empty() && !self.long_breaks.is_empty();
        if has_both && self.short_break_interval.0 >= self.long_break_interval.0 {
            return Err(format!(
                "short_break_interval ({} s) must be less than long_break_interval ({} s)",
                self.short_break_interval.0, self.long_break_interval.0
            ));
        }
        let postpone_shown = self.allow_postpone && !self.strict_break;
        let keys = self.keybindings;
        for (skip, postpone, prefix) in [
            (keys.skip, keys.postpone, ""),
            (keys.warning_skip, keys.warning_postpone, "warning_"),
        ] {
            if let (Some(skip), Some(postpone)) = (skip, postpone)
                && skip == postpone
                && postpone_shown
            {
                return Err(format!(
                    "keybindings.{prefix}skip and keybindings.{prefix}postpone are both `{skip}`"
                ));
            }
        }

        let breaks = |entries: Vec<BreakEntry>, kind, default: BreakLength| {
            entries
                .into_iter()
                .map(|entry| {
                    let image = entry
                        .image
                        .map(|path| image_path(&path, home))
                        .transpose()
                        .map_err(|e| format!("image for \"{}\": {e}", entry.name.0))?;
                    Ok(Break {
                        kind,
                        name: entry.name.0,
                        duration: entry.duration.unwrap_or(default).duration(),
                        image,
                    })
                })
                .collect::<Result<_, String>>()
        };
        let breaks = Breaks::new(
            breaks(
                self.short_breaks,
                BreakType::Short,
                self.short_break_duration,
            )?,
            breaks(self.long_breaks, BreakType::Long, self.long_break_duration)?,
        )
        .ok_or("add at least one [[short_breaks]] or [[long_breaks]] entry")?;

        Ok(Config {
            schedule: Settings {
                short_interval: self.short_break_interval.duration(),
                long_interval: self.long_break_interval.duration(),
                pre_break_warning: self.pre_break_warning.duration(),
                postpone: self.postpone_duration.duration(),
                allow_postpone: self.allow_postpone,
                strict: self.strict_break,
                idle_threshold: self
                    .smart_pause
                    .enabled
                    .then(|| self.smart_pause.idle_seconds.duration()),
                random_order: self.random_order,
                breaks,
            },
            audible_alert: self.audible_alert,
            notification: self.notification,
            keybindings: self.keybindings,
            do_not_disturb: self.do_not_disturb,
        })
    }
}

/// an existing file at an absolute path, or one under `home` written `~/...`
fn image_path(path: &str, home: Option<&Path>) -> Result<PathBuf, String> {
    let resolved = match path.strip_prefix("~/") {
        Some(rest) => home
            .ok_or("~ needs a home directory, and there's none")?
            .join(rest),
        None => PathBuf::from(path),
    };
    if !resolved.is_absolute() {
        return Err(format!(
            "`{path}` must be an absolute path or start with ~/"
        ));
    }
    if !resolved.is_file() {
        return Err(format!("no file at {}", resolved.display()));
    }
    Ok(resolved)
}

impl Config {
    /// reads and checks the config at `path`. never writes.
    pub fn from_file(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::new(format!("can't read the file: {e}")))?;
        Config::parse(&text, std::env::home_dir().as_deref())
    }

    fn parse(text: &str, home: Option<&Path>) -> Result<Config, ConfigError> {
        let file: ConfigFile =
            toml::from_str(text).map_err(|e| ConfigError::from_toml(&e, text))?;
        file.check(home).map_err(ConfigError::new)
    }
}

/// the config at startup: the file at `path`, created from the defaults when it's missing.
/// when the file can't be read or fails a check, the defaults stand in and the error says why.
pub fn load_or_create(path: &Path) -> (Config, Option<ConfigError>) {
    let loaded = match path.try_exists() {
        Ok(false) => write_default(path)
            .map(|()| Config::default())
            .map_err(|e| ConfigError::new(format!("can't create the file: {e}"))),
        _ => Config::from_file(path),
    };
    match loaded {
        Ok(config) => (config, None),
        Err(e) => (Config::default(), Some(e)),
    }
}

/// never replaces an existing file
pub fn write_default(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::File::create_new(path)?.write_all(DEFAULT_CONFIG.as_bytes())
}

/// why a config was rejected
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    /// 1-based, when the problem is on one line
    line: Option<usize>,
    message: String,
}

impl ConfigError {
    fn new(message: impl Into<String>) -> ConfigError {
        ConfigError {
            line: None,
            message: message.into(),
        }
    }

    /// names the key when toml's message doesn't, and says where a misplaced key belongs
    fn from_toml(err: &toml::de::Error, text: &str) -> ConfigError {
        let Some(span) = err.span() else {
            return ConfigError::new(err.message());
        };
        let line = text[..span.start].matches('\n').count() + 1;
        let at = &text[span.clone()];
        let key = key_at(text, span.start).or_else(|| {
            // a repeated key is missing from the parsed tree, but the error points at it
            text[span.end..]
                .trim_start_matches([' ', '\t'])
                .starts_with('=')
                .then(|| at.to_owned())
        });

        let message = err.message();
        let unknown = message.starts_with("unknown field");
        // toml quotes a key it doesn't know; then name only the table it's in
        let shown = key.as_deref().map(|key| {
            if unknown {
                key.rsplit_once('.').map_or("", |(table, _)| table)
            } else {
                key
            }
        });
        let message = if unknown && let Some(place) = place_of(at) {
            format!("{at} can't go here; it belongs {place}")
        } else {
            match shown {
                Some(shown) if !shown.is_empty() => format!("{shown}: {message}"),
                _ => message.to_owned(),
            }
        };
        ConfigError {
            line: Some(line),
            message,
        }
    }
}

/// the dotted key whose key or value covers byte `at` of `text`
fn key_at(text: &str, at: usize) -> Option<String> {
    let (root, _) = DeTable::parse_recoverable(text);
    let mut path = Vec::new();
    table_has(root.get_ref(), at, &mut path).then(|| path.join("."))
}

/// pushes the keys down to `at` onto `path`
fn table_has(table: &DeTable<'_>, at: usize, path: &mut Vec<String>) -> bool {
    table.iter().any(|(key, value)| {
        path.push(key.get_ref().to_string());
        let found = key.span().contains(&at) || value_has(value, at, path);
        if !found {
            path.pop();
        }
        found
    })
}

fn value_has(value: &Spanned<DeValue<'_>>, at: usize, path: &mut Vec<String>) -> bool {
    let inside = match value.get_ref() {
        DeValue::Table(table) => table_has(table, at, path),
        DeValue::Array(items) => items.iter().any(|item| value_has(item, at, path)),
        _ => false,
    };
    inside || value.span().contains(&at)
}

/// the keys of a break entry. the default file has only names, so `place_of` needs these.
const BREAK_KEYS: [&str; 3] = ["name", "duration", "image"];

/// where the default config has `key`, if in just one place
fn place_of(key: &str) -> Option<String> {
    let defaults: toml::Table = DEFAULT_CONFIG.parse().ok()?;
    let mut places = Vec::new();
    if defaults.get(key).is_some_and(|v| !v.is_table()) {
        places.push("at the top, above the first [table]".to_owned());
    }
    if BREAK_KEYS.contains(&key) {
        places.push("in a [[short_breaks]] or [[long_breaks]] entry".to_owned());
    }
    for (name, value) in &defaults {
        if value.as_table().is_some_and(|t| t.contains_key(key)) {
            places.push(format!("in [{name}]"));
        }
    }
    let [place]: [String; 1] = places.try_into().ok()?;
    Some(place)
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "line {line}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ConfigError {}

/// `$XDG_CONFIG_HOME/omma/config.toml`, else `~/.config/omma/config.toml`. None when neither
/// is an absolute path.
pub fn config_path() -> Option<PathBuf> {
    config_path_from(std::env::var_os("XDG_CONFIG_HOME"), std::env::home_dir())
}

/// the XDG base directory spec says to ignore an empty or relative `XDG_CONFIG_HOME`
fn config_path_from(xdg_config_home: Option<OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    let base = xdg_config_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.filter(|h| h.is_absolute()).map(|h| h.join(".config")))?;
    Some(base.join("omma").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::scheduler::Core;

    /// the repo stands in for the home directory
    fn home() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    fn parse(text: &str) -> Result<Config, ConfigError> {
        Config::parse(text, Some(home()))
    }

    #[track_caller]
    fn rejects(text: &str) -> String {
        match parse(text) {
            Ok(_) => panic!("accepted {text:?}"),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn the_default_file_is_the_default_config() {
        assert_eq!(parse(DEFAULT_CONFIG).unwrap(), Config::default());
    }

    #[test]
    fn the_schedule_comes_in_seconds_and_smart_pause_can_turn_off() {
        let schedule = Config::default().schedule;
        assert_eq!(schedule.short_interval, Duration::from_secs(900));
        assert_eq!(schedule.long_interval, Duration::from_secs(4500));
        assert_eq!(schedule.pre_break_warning, Duration::from_secs(10));
        assert_eq!(schedule.idle_threshold, Some(Duration::from_secs(60)));

        let off = parse("[smart_pause]\nenabled = false\n").unwrap();
        assert_eq!(off.schedule.idle_threshold, None);
    }

    #[test]
    fn keys_left_out_keep_their_defaults() {
        let cfg = parse("short_break_interval = 20\n").unwrap();
        assert_eq!(cfg.schedule.short_interval, Duration::from_secs(20));
        assert_eq!(cfg.schedule.long_interval, Duration::from_secs(4500));
        assert_eq!(cfg.do_not_disturb, DoNotDisturb::default());
    }

    #[test]
    fn a_misspelt_key_is_an_error_on_its_line() {
        let err = rejects("random_order = false\nshort_break_intervall = 60\n");
        assert!(
            err.starts_with("line 2: unknown field `short_break_intervall`, expected one of"),
            "{err}"
        );
    }

    #[test]
    fn a_key_in_the_wrong_table_says_where_it_belongs() {
        // TOML puts a key appended to the default file into its last table
        let text = format!("{DEFAULT_CONFIG}\nstrict_break = true\n");
        let line = text.lines().count();
        assert_eq!(
            rejects(&text),
            format!(
                "line {line}: strict_break can't go here; it belongs at the top, above the \
                 first [table]"
            )
        );
        assert_eq!(
            rejects("idle_seconds = 30\n"),
            "line 1: idle_seconds can't go here; it belongs in [smart_pause]"
        );
        assert_eq!(
            rejects("[smart_pause]\nimage = \"~/forest.jpg\"\n"),
            "line 2: image can't go here; it belongs in a [[short_breaks]] or [[long_breaks]] \
             entry"
        );
        // in several tables, so no hint
        assert!(rejects("enabled = true\n").starts_with("line 1: unknown field `enabled`"));
    }

    #[test]
    fn break_keys_are_the_keys_of_a_break() {
        let err = rejects("[[short_breaks]]\nname = \"Blink\"\nnope = 1\n");
        let keys = BREAK_KEYS.map(|key| format!("`{key}`")).join(", ");
        assert!(err.ends_with(&format!("expected one of {keys}")), "{err}");
    }

    #[test]
    fn the_dropped_per_break_interval_is_an_error() {
        let err = rejects("[[short_breaks]]\nname = \"Blink\"\ninterval = 60\n");
        assert!(
            err.starts_with("line 3: short_breaks: unknown field `interval`"),
            "{err}"
        );
    }

    #[test]
    fn every_time_has_bounds() {
        // (table, key, min, max, a line that keeps the short < long rule out of the way)
        let cases = [
            ("", "short_break_interval", 1, DAY, "long_breaks = []"),
            ("", "long_break_interval", 1, DAY, "short_breaks = []"),
            ("", "postpone_duration", 1, DAY, ""),
            ("", "short_break_duration", 1, HOUR, ""),
            ("", "long_break_duration", 1, HOUR, ""),
            ("", "pre_break_warning", 0, 600, ""),
            ("smart_pause", "idle_seconds", 10, DAY, ""),
            ("short_breaks", "duration", 1, HOUR, ""),
        ];
        for (table, key, min, max, aside) in cases {
            let (header, path) = match table {
                "" => (String::new(), key.to_owned()),
                "short_breaks" => (
                    "[[short_breaks]]\nname = \"Blink\"".to_owned(),
                    format!("{table}.{key}"),
                ),
                _ => (format!("[{table}]"), format!("{table}.{key}")),
            };
            let text = |value: i64| format!("{aside}\n{header}\n{key} = {value}\n");
            let line = text(0).lines().count();
            let (min, max) = (min as i64, max as i64);
            parse(&text(min)).unwrap();
            parse(&text(max)).unwrap();
            for bad in [min - 1, max + 1, i64::MIN] {
                assert_eq!(
                    rejects(&text(bad)),
                    format!("line {line}: {path}: must be {min} to {max} seconds, got {bad}")
                );
            }
        }
    }

    #[test]
    fn errors_name_the_key() {
        assert_eq!(
            rejects("pre_break_warning = \"10\"\n"),
            "line 1: pre_break_warning: invalid type: string \"10\", expected a whole number \
             of seconds"
        );
        assert_eq!(
            rejects("random_order = true\nrandom_order  = false\n"),
            "line 2: random_order: duplicate key"
        );
        assert_eq!(
            rejects("[do_not_disturb]\naction = \"action\"\n"),
            "line 2: do_not_disturb.action: unknown variant `action`, expected `skip` or \
             `postpone`"
        );
        assert_eq!(
            rejects("[smart_pause]\nenabled = true\n[smart_pause]\n"),
            "line 3: smart_pause: duplicate key"
        );
        assert_eq!(
            rejects("smart_pause = { idle_seconds = 3 }\n"),
            "line 1: smart_pause.idle_seconds: must be 10 to 86400 seconds, got 3"
        );
        assert_eq!(
            rejects("short_breaks = [{ name = \"a\", duration = 0 }]\n"),
            "line 1: short_breaks.duration: must be 1 to 3600 seconds, got 0"
        );
        assert_eq!(
            rejects("[do_not_disturb]\nskip_when_app_active = [\n  \"a\",\n  3,\n]\n"),
            "line 4: do_not_disturb.skip_when_app_active: invalid type: integer `3`, expected a \
             string"
        );
        assert_eq!(
            rejects("[smart_pause]\nidle = 30\n"),
            "line 2: smart_pause: unknown field `idle`, expected `enabled` or `idle_seconds`"
        );
    }

    #[test]
    fn the_short_interval_must_be_less_than_the_long_one() {
        let err = "short_break_interval (600 s) must be less than long_break_interval (600 s)";
        assert_eq!(
            rejects("short_break_interval = 600\nlong_break_interval = 600\n"),
            err
        );
        assert!(
            rejects("short_break_interval = 601\nlong_break_interval = 600\n").contains("601 s")
        );
        // with one kind of break the other interval doesn't matter
        parse("short_break_interval = 900\nlong_break_interval = 60\nlong_breaks = []\n").unwrap();
        parse("short_break_interval = 900\nlong_break_interval = 60\nshort_breaks = []\n").unwrap();
    }

    #[test]
    fn keybindings_parse_at_load_and_empty_turns_one_off() {
        let keys = parse("[keybindings]\nskip = \"cmd+S\"\npostpone = \"\"\n")
            .unwrap()
            .keybindings;
        assert_eq!(keys.skip, Some("cmd+s".parse().unwrap()));
        assert_eq!(keys.postpone, None);

        assert_eq!(
            rejects("[keybindings]\nskip = \"escpae\"\n"),
            "line 2: keybindings.skip: `escpae` isn't a key; use a single character, or escape, space, \
             enter, tab or delete"
        );
    }

    #[test]
    fn skip_and_postpone_need_different_keys_while_postpone_is_on() {
        let clash = "[keybindings]\nskip = \"esc\"\npostpone = \"escape\"\n";
        parse(clash).unwrap();
        assert_eq!(
            rejects(&format!("allow_postpone = true\n{clash}")),
            "keybindings.skip and keybindings.postpone are both `escape`"
        );
        parse(&format!(
            "allow_postpone = true\nstrict_break = true\n{clash}"
        ))
        .unwrap();
        parse("allow_postpone = true\n[keybindings]\nskip = \"\"\npostpone = \"\"\n").unwrap();
    }

    #[test]
    fn warning_shortcuts_need_cmd_or_ctrl() {
        let keys = parse(
            "allow_postpone = true\n[keybindings]\n\
             warning_skip = \"cmd+shift+option+space\"\nwarning_postpone = \"ctrl+p\"\n",
        )
        .unwrap()
        .keybindings;
        assert_eq!(
            keys.warning_skip,
            Some("cmd+shift+alt+space".parse().unwrap())
        );
        assert_eq!(keys.warning_postpone, Some("ctrl+p".parse().unwrap()));
        assert_eq!(Keybindings::default().warning_skip, None);

        for spec in ["space", "shift+s", "alt+s"] {
            assert_eq!(
                rejects(&format!("[keybindings]\nwarning_skip = \"{spec}\"\n")),
                format!(
                    "line 2: keybindings.warning_skip: `{spec}` works from any app, so it needs \
                     cmd or ctrl"
                )
            );
        }
        assert_eq!(
            rejects("[keybindings]\nwarning_skip = \"cmd+σ\"\n"),
            "line 2: keybindings.warning_skip: `σ` isn't on an ASCII keyboard layout, which \
             global shortcuts use"
        );
        assert_eq!(
            rejects(
                "allow_postpone = true\n[keybindings]\n\
                 warning_skip = \"ctrl+s\"\nwarning_postpone = \"control+S\"\n"
            ),
            "keybindings.warning_skip and keybindings.warning_postpone are both `ctrl+s`"
        );
    }

    #[test]
    fn breaks_need_a_name_and_at_least_one_must_exist() {
        assert_eq!(
            rejects("[[short_breaks]]\nname = \" \\t \"\n"),
            "line 2: short_breaks.name: a break needs a name"
        );
        assert_eq!(
            rejects("[[long_breaks]]\nduration = 5\n"),
            "line 1: long_breaks: missing field `name`"
        );
        assert_eq!(
            rejects("short_breaks = []\nlong_breaks = []\n"),
            "add at least one [[short_breaks]] or [[long_breaks]] entry"
        );
    }

    #[test]
    fn a_break_can_set_its_own_duration() {
        let cfg = parse(
            "short_break_duration = 20\n\
             [[short_breaks]]\nname = \"Blink\"\n\
             [[short_breaks]]\nname = \"Look away\"\nduration = 40\n",
        )
        .unwrap();
        let mut queue = crate::queue::BreakQueue::new(&cfg.schedule.breaks, false, UNIX_EPOCH);
        let durations: Vec<_> = (0..2)
            .map(|_| {
                let secs = queue.peek(BreakType::Short).duration.as_secs();
                queue.advance(BreakType::Short);
                secs
            })
            .collect();
        assert_eq!(durations, [20, 40]);
    }

    #[test]
    fn title_patterns_compile_at_load_and_ignore_case() {
        let dnd =
            parse("[do_not_disturb]\nwindow_title_matches = [\"zoom meeting\", \"^πλάνο\"]\n")
                .unwrap()
                .do_not_disturb;
        let [zoom, greek] = &dnd.window_title_matches[..] else {
            panic!("{dnd:?}");
        };
        assert!(zoom.is_match("Zoom Meeting - 3 people"));
        assert!(greek.is_match("ΠΛΆΝΟ ταξιδιού"));
        assert!(!greek.is_match("Το πλάνο"));

        assert_eq!(
            rejects("[do_not_disturb]\nwindow_title_matches = [\"(foo\"]\n"),
            "line 2: do_not_disturb.window_title_matches: `(foo` isn't a valid regex: unclosed group"
        );
        assert_eq!(
            rejects("[do_not_disturb]\nwindow_title_matches = [\"\"]\n"),
            "line 2: do_not_disturb.window_title_matches: an empty pattern would match every window"
        );
    }

    #[test]
    fn app_names_are_trimmed_and_lowercased_and_must_not_be_empty() {
        let dnd =
            parse("[do_not_disturb]\nskip_when_app_active = [\"Zoom\", \" com.Tinyspeck \"]\n")
                .unwrap()
                .do_not_disturb;
        let names: Vec<_> = dnd
            .skip_when_app_active
            .iter()
            .map(AppName::as_str)
            .collect();
        assert_eq!(names, ["zoom", "com.tinyspeck"]);

        assert_eq!(
            rejects("[do_not_disturb]\nskip_when_app_running = [\" \"]\n"),
            "line 2: do_not_disturb.skip_when_app_running: an empty app name would match every app"
        );
    }

    #[test]
    fn only_the_window_rules_need_accessibility() {
        let dnd = |text: &str| {
            parse(&format!("[do_not_disturb]\n{text}\n"))
                .unwrap()
                .do_not_disturb
        };
        assert!(!DoNotDisturb::default().needs_accessibility());
        assert!(!dnd("skip_when_app_running = [\"zoom\"]").needs_accessibility());
        assert!(dnd("skip_when_fullscreen = true").needs_accessibility());
        assert!(dnd("window_title_matches = [\"Zoom\"]").needs_accessibility());
        assert!(!dnd("window_title_matches = [\"Zoom\"]\nenabled = false").needs_accessibility());
    }

    #[test]
    fn images_resolve_at_load_and_must_exist() {
        let image = |path: &str, home: Option<&Path>| {
            let text = format!("[[short_breaks]]\nname = \"Blink\"\nimage = \"{path}\"\n");
            Config::parse(&text, home).map(|cfg| {
                let queue = crate::queue::BreakQueue::new(&cfg.schedule.breaks, false, UNIX_EPOCH);
                queue.peek(BreakType::Short).image.clone()
            })
        };
        let icon = home().join("tray-icon.svg");

        assert_eq!(
            image("~/tray-icon.svg", Some(home())),
            Ok(Some(icon.clone()))
        );
        assert_eq!(image(icon.to_str().unwrap(), None), Ok(Some(icon)));

        let err = |path, home| image(path, home).unwrap_err().to_string();
        let missing = home().join("missing.png");
        assert_eq!(
            err("~/missing.png", Some(home())),
            format!("image for \"Blink\": no file at {}", missing.display())
        );
        assert_eq!(
            err("~/tray-icon.svg", None),
            "image for \"Blink\": ~ needs a home directory, and there's none"
        );
        assert_eq!(
            err("tray-icon.svg", Some(home())),
            "image for \"Blink\": `tray-icon.svg` must be an absolute path or start with ~/"
        );
        // a directory isn't an image
        assert!(err("~/src", Some(home())).ends_with("/src"));
    }

    #[test]
    fn a_change_outside_the_schedule_keeps_the_countdown() {
        let before = Config::default();
        let after = parse("[audible_alert]\npre_break_sound = \"Ping\"\n").unwrap();
        assert_ne!(before, after);
        assert_eq!(before.schedule, after.schedule);

        let start = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let now = start + Duration::from_secs(100);
        let mut core = Core::new(before.schedule, start);
        let remaining = core.remaining(now);
        core.reconfigure(after.schedule);
        assert_eq!(core.remaining(now), remaining);
    }

    /// a fresh directory under the system temp dir, removed on drop
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> TempDir {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir()
                .join(format!("omma-test-{name}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn startup_creates_a_missing_file_and_falls_back_on_a_bad_one() {
        let dir = TempDir::new("startup");
        let path = dir.0.join("omma/config.toml");

        assert!(Config::from_file(&path).is_err());
        assert!(!path.exists(), "from_file must not write");

        assert_eq!(load_or_create(&path), (Config::default(), None));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), DEFAULT_CONFIG);

        std::fs::write(&path, "short_break_interval = 20\n").unwrap();
        let (cfg, err) = load_or_create(&path);
        assert_eq!((cfg.schedule.short_interval.as_secs(), err), (20, None));

        std::fs::write(&path, "short_break_interval = 0\n").unwrap();
        let (cfg, err) = load_or_create(&path);
        assert_eq!(cfg, Config::default());
        assert_eq!(
            err.unwrap().to_string(),
            "line 1: short_break_interval: must be 1 to 86400 seconds, got 0"
        );
    }

    #[test]
    fn config_path_prefers_an_absolute_xdg_config_home() {
        let path = |xdg: Option<&str>, home: Option<&str>| {
            config_path_from(xdg.map(OsString::from), home.map(PathBuf::from))
        };
        let file = |dir: &str| Some(Path::new(dir).join("omma/config.toml"));

        assert_eq!(path(Some("/xdg"), Some("/home/u")), file("/xdg"));
        assert_eq!(path(None, Some("/home/u")), file("/home/u/.config"));
        assert_eq!(path(Some(""), Some("/home/u")), file("/home/u/.config"));
        assert_eq!(
            path(Some("rel/dir"), Some("/home/u")),
            file("/home/u/.config")
        );
        assert_eq!(path(Some("/xdg"), None), file("/xdg"));
        assert_eq!(path(None, None), None);
        assert_eq!(path(Some(""), Some("")), None);
        assert_eq!(path(Some("rel"), Some("home/u")), None);
    }
}
