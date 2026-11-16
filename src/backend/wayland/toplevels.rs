//! do-not-disturb on Wayland. running apps are processes, which /proc lists for anyone. only
//! the compositor knows which window has focus, which is fullscreen and what each is titled.
//! wlroots compositors such as Hyprland, Sway and niri tell clients through the
//! foreign-toplevel protocols.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1, ext_foreign_toplevel_list_v1,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1, zwlr_foreign_toplevel_manager_v1,
};

use super::desktop::Desktop;
use crate::app::dnd::{Reason, app_matches};
use crate::config::DoNotDisturb;

#[derive(Default)]
struct Window {
    title: String,
    app_id: String,
    minimized: bool,
    activated: bool,
    fullscreen: bool,
    /// from the wlr protocol, which has the states. the ext one has only titles and app ids
    wlr: bool,
}

#[derive(Default)]
pub struct Toplevels {
    pub wlr: Option<zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1>,
    pub ext: Option<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1>,
    /// by the protocol id of each window's handle
    windows: HashMap<u32, Window>,
}

impl Toplevels {
    /// whether the compositor tells omma what the rules need to know
    pub fn has_access(&self, dnd: &DoNotDisturb) -> bool {
        if dnd.fullscreen || !dnd.apps_in_front.is_empty() {
            self.wlr.is_some()
        } else if !dnd.window_titles.is_empty() {
            self.wlr.is_some() || self.ext.is_some()
        } else {
            true
        }
    }

    /// the windows to go by: the wlr protocol's when the compositor has it
    fn windows(&self) -> impl Iterator<Item = &Window> {
        let wlr = self.wlr.is_some();
        self.windows
            .values()
            .filter(move |window| window.wlr == wlr)
    }

    /// the first rule that holds now, checked in the order the macOS backend checks them
    pub fn suppression(&self, dnd: &DoNotDisturb) -> Option<Reason> {
        let focused = self.windows().find(|window| window.activated);
        if !dnd.apps_in_front.is_empty()
            && let Some(window) = focused
            && app_matches(&dnd.apps_in_front, [window.app_id.as_str()])
        {
            return Some(Reason::Frontmost(app_name(window)));
        }
        if !dnd.apps_running.is_empty() {
            if let Some(window) = self
                .windows()
                .find(|window| app_matches(&dnd.apps_running, [window.app_id.as_str()]))
            {
                return Some(Reason::Running(app_name(window)));
            }
            if let Some(process) = running_process(&dnd.apps_running) {
                return Some(Reason::Running(process));
            }
        }
        if dnd.fullscreen
            && let Some(window) = focused
            && window.fullscreen
        {
            return Some(Reason::Fullscreen(app_name(window)));
        }
        self.windows()
            .filter(|window| !window.minimized)
            .find(|window| dnd.window_titles.iter().any(|p| p.is_match(&window.title)))
            .map(|window| Reason::Window(window.title.clone()))
    }
}

fn app_name(window: &Window) -> String {
    if window.app_id.is_empty() {
        "an app".into()
    } else {
        window.app_id.clone()
    }
}

/// the name of the first process a rule matches, by its command or the program it runs
fn running_process(rules: &[crate::config::AppName]) -> Option<String> {
    let entries = fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let dir = entry.path();
        let comm = read_trimmed(&dir.join("comm"));
        let program = fs::read(dir.join("cmdline"))
            .ok()
            .and_then(|cmdline| {
                let first = cmdline.split(|&b| b == 0).next()?.to_vec();
                let first = String::from_utf8(first).ok()?;
                Path::new(&first)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_default();
        if app_matches(rules, [comm.as_str(), program.as_str()]) {
            return Some(if comm.is_empty() { program } else { comm });
        }
    }
    None
}

fn read_trimmed(path: &Path) -> String {
    fs::read_to_string(path)
        .map(|text| text.trim().to_owned())
        .unwrap_or_default()
}

impl Dispatch<zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1, ()> for Desktop {
    fn event(
        state: &mut Desktop,
        _: &zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Desktop>,
    ) {
        match event {
            zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } => {
                state.toplevels.windows.insert(
                    toplevel.id().protocol_id(),
                    Window {
                        wlr: true,
                        ..Window::default()
                    },
                );
            }
            zwlr_foreign_toplevel_manager_v1::Event::Finished => {
                state.toplevels.wlr = None;
                state.toplevels.windows.retain(|_, window| !window.wlr);
            }
            _ => {}
        }
    }

    event_created_child!(Desktop, zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1, ()> for Desktop {
    fn event(
        state: &mut Desktop,
        handle: &zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Desktop>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::{Event, State};
        let windows = &mut state.toplevels.windows;
        if let Event::Closed = event {
            windows.remove(&handle.id().protocol_id());
            handle.destroy();
            return;
        }
        let Some(window) = windows.get_mut(&handle.id().protocol_id()) else {
            return;
        };
        match event {
            Event::Title { title } => window.title = title,
            Event::AppId { app_id } => window.app_id = app_id,
            Event::State { state } => {
                let states: Vec<u32> = state
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| u32::from_ne_bytes(*b))
                    .collect();
                let has = |s: State| states.contains(&(s as u32));
                window.minimized = has(State::Minimized);
                window.activated = has(State::Activated);
                window.fullscreen = has(State::Fullscreen);
            }
            _ => {}
        }
    }
}

impl Dispatch<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, ()> for Desktop {
    fn event(
        state: &mut Desktop,
        _: &ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Desktop>,
    ) {
        match event {
            ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } => {
                state
                    .toplevels
                    .windows
                    .insert(toplevel.id().protocol_id(), Window::default());
            }
            ext_foreign_toplevel_list_v1::Event::Finished => {
                state.toplevels.ext = None;
                state.toplevels.windows.retain(|_, window| window.wlr);
            }
            _ => {}
        }
    }

    event_created_child!(Desktop, ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ()> for Desktop {
    fn event(
        state: &mut Desktop,
        handle: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Desktop>,
    ) {
        use ext_foreign_toplevel_handle_v1::Event;
        let windows = &mut state.toplevels.windows;
        if let Event::Closed = event {
            windows.remove(&handle.id().protocol_id());
            handle.destroy();
            return;
        }
        let Some(window) = windows.get_mut(&handle.id().protocol_id()) else {
            return;
        };
        match event {
            Event::Title { title } => window.title = title,
            Event::AppId { app_id } => window.app_id = app_id,
            _ => {}
        }
    }
}
