//! the XDG desktop portal: the desktop's appearance, which the warning and the tray icon
//! follow, and global shortcuts for the warning. each talks to D-Bus on a thread of its own,
//! and sends what it hears to the event loop.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use xkbcommon::xkb;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use super::desktop::{Message, Waker};
use crate::app::WarningKey;
use crate::config::{Key, KeySpec};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const SETTINGS: &str = "org.freedesktop.portal.Settings";
const SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";
const REQUEST: &str = "org.freedesktop.portal.Request";
const SESSION: &str = "org.freedesktop.portal.Session";
const REGISTRY: &str = "org.freedesktop.host.portal.Registry";
/// the app id that omma gives the portal, which asks apps outside a sandbox for one. it matches
/// the launchd label on macOS
const APP_ID: &str = "com.museslabs.omma";

/// what the desktop asks of apps' looks
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Appearance {
    pub dark: bool,
    /// high contrast, which gets solid surfaces, as Increase Contrast does on macOS
    pub contrast: bool,
    pub reduce_motion: bool,
}

impl Default for Appearance {
    /// most Wayland desktops without a preference are dark
    fn default() -> Appearance {
        Appearance {
            dark: true,
            contrast: false,
            reduce_motion: false,
        }
    }
}

/// follows the desktop's appearance on a thread, which sends each change
pub fn watch_appearance(waker: Waker) {
    let spawned = std::thread::Builder::new()
        .name("omma-appearance".into())
        .spawn(move || {
            if let Err(e) = follow(&waker) {
                log!("can't read the desktop's appearance, so omma stays dark: {e}");
            }
        });
    if let Err(e) = spawned {
        log!("can't follow the desktop's appearance: {e}");
    }
}

fn follow(waker: &Waker) -> zbus::Result<()> {
    let bus = Connection::session()?;
    let settings = Proxy::new(&bus, PORTAL, PATH, SETTINGS)?;
    // subscribed before the first read, so no change falls between them
    let changes = settings.receive_signal("SettingChanged")?;
    let mut shown = read(&settings);
    waker.send(Message::Appearance(shown));
    for _ in changes {
        let now = read(&settings);
        if now != shown {
            shown = now;
            waker.send(Message::Appearance(now));
        }
    }
    Ok(())
}

fn read(settings: &Proxy) -> Appearance {
    let get = |namespace: &str, key: &str| -> Option<OwnedValue> {
        ["ReadOne", "Read"].into_iter().find_map(|method| {
            settings
                .call::<_, _, OwnedValue>(method, &(namespace, key))
                .ok()
        })
    };
    let number = |value: Option<OwnedValue>| value.and_then(|v| u32::try_from(inner(&v)).ok());
    let text = |value: Option<OwnedValue>| {
        value.and_then(|v| match inner(&v) {
            Value::Str(s) => Some(s.to_string()),
            _ => None,
        })
    };
    let gnome = "org.gnome.desktop.interface";
    let dark = match number(get("org.freedesktop.appearance", "color-scheme")) {
        Some(1) => true,
        Some(2) => false,
        // no preference: a GTK theme named dark says dark, one named otherwise says light
        _ => {
            text(get(gnome, "gtk-theme")).is_none_or(|theme| theme.to_lowercase().contains("dark"))
        }
    };
    let contrast = number(get("org.freedesktop.appearance", "contrast")) == Some(1);
    let animations_off =
        get(gnome, "enable-animations").is_some_and(|v| matches!(inner(&v), Value::Bool(false)));
    let reduce_motion =
        number(get("org.freedesktop.appearance", "reduced-motion")) == Some(1) || animations_off;
    Appearance {
        dark,
        contrast,
        reduce_motion,
    }
}

/// a value out of the variants older portals wrap it in
fn inner<'a>(value: &'a Value<'a>) -> &'a Value<'a> {
    match value {
        Value::Value(boxed) => inner(boxed),
        other => other,
    }
}

/// where global shortcuts stand
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Binding {
    /// none asked for
    None,
    /// asked for, with no answer yet
    Pending,
    /// the keys asked for, which act while the warning shows. `triggers` holds the trigger
    /// that the desktop gave each key, as it describes it. the user can pick a trigger other
    /// than the one omma asked for, and a desktop such as Hyprland leaves the triggers to its
    /// own config
    Held {
        keys: Vec<(WarningKey, KeySpec)>,
        triggers: Vec<(WarningKey, String)>,
    },
    Failed(String),
}

/// the warning's global shortcuts, through the GlobalShortcuts portal. they are bound once,
/// at startup and when the config changes them, because a desktop can ask the user to
/// confirm. they act only while the warning holds them
pub struct Shortcuts {
    requests: Option<Sender<Request>>,
    asked: Vec<(WarningKey, KeySpec)>,
    pub bound: Binding,
    /// the warning shows, and holds the shortcuts its buttons show
    holding: bool,
    /// keys that the config asked for while the warning held its shortcuts. they are bound
    /// when the warning lets go, so that an open warning keeps the shortcuts it shows
    deferred: Option<Vec<(WarningKey, KeySpec)>>,
}

impl Shortcuts {
    pub fn new(waker: Waker) -> Shortcuts {
        let (tx, rx) = mpsc::channel();
        let lost = tx.clone();
        let spawned = std::thread::Builder::new()
            .name("omma-shortcuts".into())
            .spawn(move || serve(rx, lost, waker));
        Shortcuts::with(spawned.ok().map(|_| tx))
    }

    /// shortcuts that send their requests to `requests`
    fn with(requests: Option<Sender<Request>>) -> Shortcuts {
        Shortcuts {
            requests,
            asked: Vec::new(),
            bound: Binding::None,
            holding: false,
            deferred: None,
        }
    }

    /// asks for `keys`, unless they are what was asked for last. while the warning holds its
    /// shortcuts, the request waits until it lets go
    pub fn bind(&mut self, keys: Vec<(WarningKey, KeySpec)>) {
        if self.holding {
            self.deferred = Some(keys);
        } else {
            self.send(keys);
        }
    }

    fn send(&mut self, keys: Vec<(WarningKey, KeySpec)>) {
        if keys == self.asked && self.bound != Binding::None {
            return;
        }
        self.asked = keys.clone();
        if keys.is_empty() {
            self.bound = Binding::None;
        }
        match &self.requests {
            Some(requests) if requests.send(Request::Bind(keys)).is_ok() => {
                if !self.asked.is_empty() {
                    self.bound = Binding::Pending;
                }
            }
            _ if !self.asked.is_empty() => {
                self.bound = Binding::Failed("can't listen for global shortcuts".into());
            }
            _ => {}
        }
    }

    /// the warning shows, and its shortcuts act
    pub fn hold(&mut self) {
        self.holding = true;
    }

    /// the warning is gone. a request that waited for it goes out now
    pub fn release(&mut self) {
        self.holding = false;
        if let Some(keys) = self.deferred.take() {
            self.send(keys);
        }
    }

    pub fn holding(&self) -> bool {
        self.holding
    }

    /// the portal's answer to the latest request
    pub fn answered(&mut self, result: Result<Answer, String>) {
        self.bound = match result {
            Ok((keys, _)) if keys.is_empty() => Binding::None,
            Ok((keys, triggers)) => Binding::Held { keys, triggers },
            Err(why) => Binding::Failed(why),
        };
    }

    /// the triggers that the user gave the shortcuts in the desktop's settings, for the hints
    /// of the warnings to come
    pub fn triggers_changed(&mut self, changed: Vec<(WarningKey, String)>) {
        if let Binding::Held { triggers, .. } = &mut self.bound {
            *triggers = changed;
        }
    }

    /// the key bound for `key`, when the desktop gave it a trigger
    pub fn held(&self, key: WarningKey) -> Option<KeySpec> {
        self.trigger(key)?;
        match &self.bound {
            Binding::Held { keys, .. } => keys.iter().find(|(k, _)| *k == key).map(|(_, s)| *s),
            _ => None,
        }
    }

    /// the trigger that the desktop gave `key`, as it describes it, for the button's hint
    pub fn trigger(&self, key: WarningKey) -> Option<&str> {
        match &self.bound {
            Binding::Held { triggers, .. } => triggers
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, trigger)| trigger.as_str()),
            _ => None,
        }
    }
}

/// the keys bound, and the trigger that the desktop gave each, as it describes it
pub type Answer = (Vec<(WarningKey, KeySpec)>, Vec<(WarningKey, String)>);

/// the shortcut's id with the portal
fn id(key: WarningKey) -> &'static str {
    match key {
        WarningKey::Skip => "skip",
        WarningKey::Postpone => "postpone",
    }
}

/// the shortcut as the portal's preferred trigger: modifiers, then an xkb key name
fn trigger(spec: KeySpec) -> String {
    let key = match spec.key {
        Key::Escape => "Escape".to_owned(),
        Key::Space => "space".to_owned(),
        Key::Enter => "Return".to_owned(),
        Key::Tab => "Tab".to_owned(),
        Key::Delete => "BackSpace".to_owned(),
        // a character by its keysym's name, as `semicolon` for `;`, which is what xkb looks up
        Key::Char(c) => xkb::keysym_get_name(xkb::utf32_to_keysym(u32::from(c))),
    };
    let mods = spec.mods;
    [
        (mods.ctrl, "CTRL+"),
        (mods.alt, "ALT+"),
        (mods.shift, "SHIFT+"),
        (mods.cmd, "LOGO+"),
    ]
    .into_iter()
    .filter_map(|(held, name)| held.then_some(name))
    .chain([key.as_str()])
    .collect()
}

/// what the shortcut worker is asked to do
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    /// bind these keys, in place of the ones bound before
    Bind(Vec<(WarningKey, KeySpec)>),
    /// the portal lost the session, as when it restarted: bind the same keys again
    Lost,
}

/// what a run of queued requests comes to: whether the session was lost, and the latest keys
/// asked for, if any were
fn coalesce(
    requests: impl IntoIterator<Item = Request>,
) -> (bool, Option<Vec<(WarningKey, KeySpec)>>) {
    requests
        .into_iter()
        .fold((false, None), |(lost, keys), request| match request {
            Request::Bind(asked) => (lost, Some(asked)),
            Request::Lost => (true, keys),
        })
}

/// registers omma's app id with the portal, which a portal needs before it gives global
/// shortcuts to an app outside a sandbox. it must come before any other portal call on the
/// connection. an older portal needs none. false when the portal refused it
fn register(bus: &Connection) -> bool {
    Proxy::new(bus, PORTAL, PATH, REGISTRY)
        .and_then(|registry| {
            let options: HashMap<&str, Value> = HashMap::new();
            registry.call_method("Register", &(APP_ID, options))
        })
        .is_ok()
}

/// binds each request's keys in a session of their own, and binds them again when the portal
/// loses the session. other threads send the presses and changed triggers of the session
fn serve(requests: Receiver<Request>, lost: Sender<Request>, waker: Waker) {
    let bus = match Connection::session() {
        Ok(bus) => bus,
        Err(e) => {
            for request in requests {
                if matches!(&request, Request::Bind(keys) if !keys.is_empty()) {
                    waker.send(Message::ShortcutsBound(Err(format!(
                        "can't take global shortcuts without a session bus: {e}"
                    ))));
                }
            }
            return;
        }
    };
    let mut registered = register(&bus);
    let session: Arc<Mutex<Option<OwnedObjectPath>>> = Arc::default();
    listen(&bus, &session, &waker);
    watch(&bus, &session, lost);
    // the keys of the latest request, which a lost session binds again
    let mut keys = Vec::new();
    while let Ok(first) = requests.recv() {
        let (was_lost, asked) = coalesce(std::iter::once(first).chain(requests.try_iter()));
        let old = session.lock().expect("never poisoned").take();
        if was_lost {
            // the portal that comes back knows neither omma nor its session
            registered = register(&bus);
        } else if let Some(old) = old
            && let Ok(proxy) = Proxy::new(&bus, PORTAL, old, SESSION)
        {
            let _ = proxy.call_method("Close", &());
        }
        match asked {
            Some(asked) => keys = asked,
            None if keys.is_empty() => continue,
            None => {}
        }
        if keys.is_empty() {
            waker.send(Message::ShortcutsBound(Ok((Vec::new(), Vec::new()))));
            continue;
        }
        let result = bind(&bus, &keys).map(|(path, triggers)| {
            *session.lock().expect("never poisoned") = Some(path);
            (keys.clone(), triggers)
        });
        waker.send(Message::ShortcutsBound(result.map_err(|e| {
            let instead = "bind omma --skip and omma --postpone in your compositor";
            if registered {
                format!("can't take global shortcuts: {e}. {instead} instead")
            } else {
                format!(
                    "can't take global shortcuts: the desktop portal doesn't know omma. install \
                     {APP_ID}.desktop as the README says, or {instead}"
                )
            }
        })));
    }
}

/// signals that match `rule`, on a thread of their own named `name`, each handed to `each`
fn on_signals(
    bus: &Connection,
    rule: zbus::Result<zbus::MatchRule<'static>>,
    name: &str,
    mut each: impl FnMut(&zbus::Message) + Send + 'static,
) {
    let Ok(signals) = rule.and_then(|rule| MessageIterator::for_match_rule(rule, bus, None)) else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            for signal in signals.flatten() {
                each(&signal);
            }
        });
}

/// a rule for the signals of `interface`
fn signals(interface: &'static str) -> zbus::Result<zbus::match_rule::Builder<'static>> {
    zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(interface)
}

/// sends the presses of the session's shortcuts, and the triggers the user gives them in the
/// desktop's settings. the rule names no sender, so it still matches after the portal restarts
fn listen(bus: &Connection, session: &Arc<Mutex<Option<OwnedObjectPath>>>, waker: &Waker) {
    let session = Arc::clone(session);
    let waker = waker.clone();
    let rule = signals(SHORTCUTS)
        .and_then(|rule| rule.path(PATH))
        .map(|rule| rule.build());
    on_signals(bus, rule, "omma-shortcut-signals", move |signal| {
        let header = signal.header();
        let current =
            |from: &OwnedObjectPath| session.lock().expect("never poisoned").as_ref() == Some(from);
        match header.member().map(|member| member.as_str()) {
            Some("Activated") => {
                let Ok((from, shortcut, _, _)) =
                    signal
                        .body()
                        .deserialize::<(OwnedObjectPath, String, u64, HashMap<String, OwnedValue>)>(
                        )
                else {
                    return;
                };
                let key = match shortcut.as_str() {
                    "skip" => WarningKey::Skip,
                    "postpone" => WarningKey::Postpone,
                    _ => return,
                };
                if current(&from) {
                    waker.send(Message::Shortcut(key));
                }
            }
            Some("ShortcutsChanged") => {
                let Ok((from, shortcuts)) = signal
                    .body()
                    .deserialize::<(OwnedObjectPath, Vec<(String, HashMap<String, OwnedValue>)>)>()
                else {
                    return;
                };
                if current(&from) {
                    waker.send(Message::ShortcutsChanged(triggers(shortcuts)));
                }
            }
            _ => {}
        }
    });
}

/// tells the worker when the portal loses the session: when the portal's bus name loses its
/// owner, as when it restarts, and when the portal closes the session
fn watch(bus: &Connection, session: &Arc<Mutex<Option<OwnedObjectPath>>>, lost: Sender<Request>) {
    let owner = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")
        .and_then(|rule| rule.interface("org.freedesktop.DBus"))
        .and_then(|rule| rule.member("NameOwnerChanged"))
        .and_then(|rule| rule.add_arg(PORTAL))
        .map(|rule| rule.build());
    let restarted = lost.clone();
    on_signals(bus, owner, "omma-portal-owner", move |signal| {
        // the name, the owner before and the owner after. a portal that starts on demand had
        // no owner before, and loses nothing
        if let Ok((_, before, _)) = signal.body().deserialize::<(String, String, String)>()
            && !before.is_empty()
        {
            let _ = restarted.send(Request::Lost);
        }
    });
    let session = Arc::clone(session);
    let closed = signals(SESSION)
        .and_then(|rule| rule.member("Closed"))
        .map(|rule| rule.build());
    on_signals(bus, closed, "omma-portal-sessions", move |signal| {
        let header = signal.header();
        let current = session.lock().expect("never poisoned").clone();
        // a session that omma closed itself is no longer the current one
        if header
            .path()
            .is_some_and(|path| current.as_deref() == Some(path))
        {
            let _ = lost.send(Request::Lost);
        }
    });
}

/// the trigger that the desktop gave each of omma's shortcuts in `shortcuts`, the list of a
/// session's shortcuts that the portal sends
fn triggers(shortcuts: Vec<(String, HashMap<String, OwnedValue>)>) -> Vec<(WarningKey, String)> {
    let bound = Value::from(shortcuts);
    [WarningKey::Skip, WarningKey::Postpone]
        .into_iter()
        .filter_map(|key| Some((key, trigger_of(&bound, id(key))?)))
        .collect()
}

/// a session with `keys` bound, and the trigger that the desktop gave each key
fn bind(
    bus: &Connection,
    keys: &[(WarningKey, KeySpec)],
) -> Result<(OwnedObjectPath, Vec<(WarningKey, String)>), String> {
    let portal = Proxy::new(bus, PORTAL, PATH, SHORTCUTS).map_err(|e| e.to_string())?;
    let token = next_token();
    let options: HashMap<&str, Value> = HashMap::from([
        ("handle_token", Value::from(token.as_str())),
        ("session_handle_token", Value::from(token.as_str())),
    ]);
    let results = request(bus, &token, || {
        portal
            .call::<_, _, OwnedObjectPath>("CreateSession", &(options,))
            .map_err(describe)
    })?;
    let handle = results
        .get("session_handle")
        .and_then(|v| match inner(v) {
            Value::Str(s) => OwnedObjectPath::try_from(s.as_str()).ok(),
            Value::ObjectPath(p) => Some(OwnedObjectPath::from(p.clone())),
            _ => None,
        })
        .ok_or("the portal made no session")?;

    let shortcuts: Vec<(&str, HashMap<&str, Value>)> = keys
        .iter()
        .map(|&(key, spec)| {
            let description = match key {
                WarningKey::Skip => "Skip the break the warning counts down to",
                WarningKey::Postpone => "Postpone the break the warning counts down to",
            };
            (
                id(key),
                HashMap::from([
                    ("description", Value::from(description)),
                    ("preferred_trigger", Value::from(trigger(spec))),
                ]),
            )
        })
        .collect();
    let token = next_token();
    let options: HashMap<&str, Value> =
        HashMap::from([("handle_token", Value::from(token.as_str()))]);
    let session_path: ObjectPath = handle.as_ref();
    let results = request(bus, &token, || {
        portal
            .call::<_, _, OwnedObjectPath>("BindShortcuts", &(session_path, shortcuts, "", options))
            .map_err(describe)
    })?;
    let triggers = keys
        .iter()
        .filter_map(|&(key, _)| {
            let bound = inner(results.get("shortcuts")?);
            Some((key, trigger_of(bound, id(key))?))
        })
        .collect();
    Ok((handle, triggers))
}

/// the trigger description of the shortcut `id`, in the portal's list of bound shortcuts.
/// None when the shortcut has no trigger
fn trigger_of(bound: &Value, id: &str) -> Option<String> {
    let Value::Array(bound) = bound else {
        return None;
    };
    bound.inner().iter().find_map(|shortcut| {
        let Value::Structure(shortcut) = inner(shortcut) else {
            return None;
        };
        let [Value::Str(name), Value::Dict(properties), ..] = shortcut.fields() else {
            return None;
        };
        if name.as_str() != id {
            return None;
        }
        properties
            .iter()
            .find_map(|(key, value)| match (key, inner(value)) {
                (Value::Str(k), Value::Str(text))
                    if k.as_str() == "trigger_description" && !text.is_empty() =>
                {
                    Some(text.to_string())
                }
                _ => None,
            })
    })
}

/// runs `call`, which starts a portal request with the handle token `token`, and waits for
/// the request's answer
fn request(
    bus: &Connection,
    token: &str,
    call: impl FnOnce() -> Result<OwnedObjectPath, String>,
) -> Result<HashMap<String, OwnedValue>, String> {
    // the request's path is known ahead, so its answer cannot come before omma listens
    let sender = bus
        .unique_name()
        .map(|name| name.trim_start_matches(':').replace('.', "_"))
        .ok_or("no name on the session bus")?;
    let expected = format!("{PATH}/request/{sender}/{token}");
    let answers = |path: &str| {
        Proxy::new(bus, PORTAL, path.to_owned(), REQUEST)
            .and_then(|proxy| proxy.receive_signal("Response"))
            .map_err(|e| e.to_string())
    };
    let mut responses = answers(&expected)?;
    let path = call()?;
    if path.as_str() != expected {
        // an older portal picks its own path
        responses = answers(path.as_str())?;
    }
    let response = responses.next().ok_or("the portal never answered")?;
    let (code, results) = response
        .body()
        .deserialize::<(u32, HashMap<String, OwnedValue>)>()
        .map_err(|e| e.to_string())?;
    match code {
        0 => Ok(results),
        1 => Err("the request was cancelled".into()),
        _ => Err("the desktop refused".into()),
    }
}

fn describe(error: zbus::Error) -> String {
    match &error {
        zbus::Error::MethodError(name, ..)
            if name.as_str().ends_with("UnknownMethod")
                || name.as_str().ends_with("UnknownInterface")
                || name.as_str().ends_with("ServiceUnknown") =>
        {
            "this desktop has no global shortcuts portal".into()
        }
        _ => error.to_string(),
    }
}

fn next_token() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "omma_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers_name_modifiers_then_an_xkb_key() {
        let t = |spec: &str| trigger(spec.parse().unwrap());
        assert_eq!(t("super+shift+alt+space"), "ALT+SHIFT+LOGO+space");
        assert_eq!(t("ctrl+p"), "CTRL+p");
        assert_eq!(t("ctrl+escape"), "CTRL+Escape");
        assert_eq!(t("ctrl+enter"), "CTRL+Return");
        assert_eq!(t("ctrl+delete"), "CTRL+BackSpace");
        assert_eq!(t("ctrl+;"), "CTRL+semicolon");
        assert_eq!(t("ctrl+/"), "CTRL+slash");
        assert_eq!(t("ctrl+1"), "CTRL+1");
    }

    fn keys(spec: &str) -> Vec<(WarningKey, KeySpec)> {
        vec![(WarningKey::Skip, spec.parse().unwrap())]
    }

    #[test]
    fn a_rebind_waits_until_the_warning_lets_go_of_its_shortcuts() {
        let (tx, rx) = mpsc::channel();
        let mut shortcuts = Shortcuts::with(Some(tx));
        shortcuts.bind(keys("ctrl+s"));
        assert_eq!(rx.try_recv(), Ok(Request::Bind(keys("ctrl+s"))));
        shortcuts.answered(Ok((
            keys("ctrl+s"),
            vec![(WarningKey::Skip, "Ctrl+S".into())],
        )));

        shortcuts.hold();
        shortcuts.bind(keys("ctrl+k"));
        // the open warning keeps the shortcut it shows
        assert!(rx.try_recv().is_err());
        assert_eq!(shortcuts.trigger(WarningKey::Skip), Some("Ctrl+S"));

        shortcuts.release();
        assert_eq!(rx.try_recv(), Ok(Request::Bind(keys("ctrl+k"))));
        assert_eq!(shortcuts.bound, Binding::Pending);
    }

    #[test]
    fn a_hint_shows_the_trigger_the_desktop_chose() {
        let mut shortcuts = Shortcuts::with(None);
        let asked = vec![
            (WarningKey::Skip, "ctrl+s".parse().unwrap()),
            (WarningKey::Postpone, "ctrl+p".parse().unwrap()),
        ];
        // the user picked another key for skip, and none for postpone
        shortcuts.answered(Ok((asked, vec![(WarningKey::Skip, "Meta+K".into())])));
        assert_eq!(shortcuts.trigger(WarningKey::Skip), Some("Meta+K"));
        assert!(shortcuts.held(WarningKey::Skip).is_some());
        assert_eq!(shortcuts.trigger(WarningKey::Postpone), None);
        assert_eq!(shortcuts.held(WarningKey::Postpone), None);
    }

    #[test]
    fn queued_requests_come_to_the_latest_keys_and_whether_the_session_was_lost() {
        assert_eq!(coalesce([Request::Lost]), (true, None));
        assert_eq!(
            coalesce([Request::Bind(keys("ctrl+s")), Request::Bind(keys("ctrl+k"))]),
            (false, Some(keys("ctrl+k")))
        );
        assert_eq!(
            coalesce([Request::Bind(keys("ctrl+s")), Request::Lost]),
            (true, Some(keys("ctrl+s")))
        );
    }

    #[test]
    fn changed_triggers_replace_the_hints_of_warnings_to_come() {
        let mut shortcuts = Shortcuts::with(None);
        shortcuts.answered(Ok((
            keys("ctrl+s"),
            vec![(WarningKey::Skip, "Ctrl+S".into())],
        )));
        let changed = triggers(vec![(
            "skip".to_owned(),
            HashMap::from([(
                "trigger_description".to_owned(),
                OwnedValue::try_from(Value::from("Meta+J")).unwrap(),
            )]),
        )]);
        shortcuts.triggers_changed(changed);
        assert_eq!(shortcuts.trigger(WarningKey::Skip), Some("Meta+J"));
        // the user took the trigger away
        shortcuts.triggers_changed(triggers(Vec::new()));
        assert_eq!(shortcuts.trigger(WarningKey::Skip), None);
        assert_eq!(shortcuts.held(WarningKey::Skip), None);
    }

    #[test]
    fn a_trigger_comes_from_the_portal_answer() {
        // a(sa{sv}), as the portal sends it
        let shortcut = |id: &str, trigger: &str| {
            (
                id.to_owned(),
                HashMap::from([(
                    "trigger_description".to_owned(),
                    Value::from(trigger.to_owned()),
                )]),
            )
        };
        let bound = Value::from(vec![shortcut("skip", "Meta+K"), shortcut("postpone", "")]);
        assert_eq!(bound.value_signature().to_string(), "a(sa{sv})");
        assert_eq!(trigger_of(&bound, "skip"), Some("Meta+K".into()));
        assert_eq!(trigger_of(&bound, "postpone"), None);
        assert_eq!(trigger_of(&bound, "other"), None);
    }

    #[test]
    fn values_come_out_of_their_wrapping() {
        let wrapped = Value::Value(Box::new(Value::Value(Box::new(Value::U32(1)))));
        assert_eq!(inner(&wrapped), &Value::U32(1));
    }
}
