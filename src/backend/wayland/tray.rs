//! the tray icon and its menu, through StatusNotifierItem, which KDE, Waybar and most bars
//! show. the menu is the macOS one, item for item. a tray menu has no tooltips, so a long
//! line is cut short, and `omma --status` shows all of it.

use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
use ksni::{Icon, MenuItem, ToolTip};

use super::desktop::{Message, Waker};
use super::icons;
use crate::app::{Action, MenuLine, PAUSES, TrayIcon};
use crate::queue::BreakType;

/// what the menu shows. ksni builds the menu from it whenever it changes
struct Model {
    status: String,
    suppressed: Option<MenuLine>,
    enabled: bool,
    config_error: Option<(String, MenuLine)>,
    problems: [Option<MenuLine>; 3],
    icon: TrayIcon,
    dark: bool,
    pixels: Vec<Icon>,
    waker: Waker,
}

impl Model {
    fn render(&mut self) {
        self.pixels = icons::render(self.icon, self.dark)
            .into_iter()
            .map(|pixels| Icon {
                width: pixels.size as i32,
                height: pixels.size as i32,
                data: pixels.argb,
            })
            .collect();
    }
}

fn act(action: Action) -> Box<dyn Fn(&mut Model) + Send> {
    Box::new(move |model: &mut Model| model.waker.send(Message::Act(action)))
}

fn send(message: fn() -> Message) -> Box<dyn Fn(&mut Model) + Send> {
    Box::new(move |model: &mut Model| model.waker.send(message()))
}

fn label(text: &str) -> MenuItem<Model> {
    StandardItem {
        label: text.into(),
        enabled: false,
        ..Default::default()
    }
    .into()
}

fn line(line: Option<&MenuLine>, activate: Box<dyn Fn(&mut Model) + Send>) -> MenuItem<Model> {
    StandardItem {
        label: line.map(|l| l.title.clone()).unwrap_or_default(),
        visible: line.is_some(),
        activate,
        ..Default::default()
    }
    .into()
}

fn item(text: &str, activate: Box<dyn Fn(&mut Model) + Send>) -> MenuItem<Model> {
    StandardItem {
        label: text.into(),
        activate,
        ..Default::default()
    }
    .into()
}

impl ksni::Tray for Model {
    // a click opens the menu, as on macOS
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "omma".into()
    }

    fn title(&self) -> String {
        "omma".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.pixels.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "omma".into(),
            description: self.status.clone(),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let [image, shortcuts, access] = &self.problems;
        let mut items = vec![
            label(&self.status),
            StandardItem {
                label: self
                    .suppressed
                    .as_ref()
                    .map(|l| l.title.clone())
                    .unwrap_or_default(),
                enabled: false,
                visible: self.suppressed.is_some(),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: self
                    .config_error
                    .as_ref()
                    .map(|(heading, _)| heading.clone())
                    .unwrap_or_default(),
                enabled: false,
                visible: self.config_error.is_some(),
                ..Default::default()
            }
            .into(),
            line(
                self.config_error.as_ref().map(|(_, detail)| detail),
                send(|| Message::OpenConfig),
            ),
            line(image.as_ref(), send(|| Message::OpenConfig)),
            line(shortcuts.as_ref(), send(|| Message::OpenConfig)),
            line(access.as_ref(), send(|| Message::OpenHelp)),
            MenuItem::Separator,
            item("Take a break now", act(Action::TakeBreak(None))),
            item(
                "Take a short break",
                act(Action::TakeBreak(Some(BreakType::Short))),
            ),
            item(
                "Take a long break",
                act(Action::TakeBreak(Some(BreakType::Long))),
            ),
            MenuItem::Separator,
        ];
        items.push(
            SubMenu {
                label: "Pause".into(),
                submenu: PAUSES
                    .iter()
                    .map(|&(title, secs)| item(title, act(Action::Pause(secs))))
                    .collect(),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            CheckmarkItem {
                label: "Enabled".into(),
                checked: self.enabled,
                activate: act(Action::ToggleEnabled),
                ..Default::default()
            }
            .into(),
        );
        items.extend([
            MenuItem::Separator,
            item("Open config…", send(|| Message::OpenConfig)),
            item("Help…", send(|| Message::OpenHelp)),
            MenuItem::Separator,
            label(concat!("omma ", env!("CARGO_PKG_VERSION"))),
            item("Quit omma", act(Action::Quit)),
        ]);
        items
    }
}

/// the running tray, or nothing where there is no session bus
pub struct Tray {
    handle: Option<Handle<Model>>,
}

impl Tray {
    pub fn start(waker: Waker, dark: bool) -> Tray {
        let mut model = Model {
            status: "Starting…".into(),
            suppressed: None,
            enabled: true,
            config_error: None,
            problems: Default::default(),
            icon: TrayIcon::Normal,
            dark,
            pixels: Vec::new(),
            waker,
        };
        model.render();
        // with no tray showing yet, ksni waits for one, as a bar that starts after omma does
        let handle = match model.assume_sni_available(true).spawn() {
            Ok(handle) => Some(handle),
            Err(e) => {
                log!("can't show a tray icon: {e}");
                None
            }
        };
        Tray { handle }
    }

    fn update(&self, f: impl FnOnce(&mut Model)) {
        if let Some(handle) = &self.handle {
            handle.update(f);
        }
    }

    pub fn set_status(&self, text: &str) {
        let text = text.to_owned();
        self.update(|model| model.status = text);
    }

    pub fn set_suppressed(&self, line: Option<&MenuLine>) {
        let line = line.cloned();
        self.update(|model| model.suppressed = line);
    }

    pub fn set_enabled(&self, on: bool) {
        self.update(|model| model.enabled = on);
    }

    pub fn set_config_error(&self, error: Option<(&str, &MenuLine)>) {
        let error = error.map(|(heading, line)| (heading.to_owned(), line.clone()));
        self.update(|model| model.config_error = error);
    }

    pub fn set_problems(&self, lines: &[Option<MenuLine>; 3]) {
        let lines = lines.clone();
        self.update(|model| model.problems = lines);
    }

    pub fn set_icon(&self, icon: TrayIcon) {
        self.update(|model| {
            model.icon = icon;
            model.render();
        });
    }

    /// redraws the icon for a dark or a light desktop
    pub fn set_dark(&self, dark: bool) {
        self.update(|model| {
            if model.dark != dark {
                model.dark = dark;
                model.render();
            }
        });
    }
}
