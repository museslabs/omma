//! the compositor's globals, the outputs, and layer surfaces drawn with GL. a surface is
//! sized in logical pixels and drawn at its preferred scale, which is fractional where the
//! compositor offers it. it draws a frame only once the compositor asks for the next.

use std::collections::HashMap;

use wayland_client::protocol::{
    wl_callback, wl_compositor, wl_output, wl_region, wl_registry, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1, ext_background_effect_surface_v1,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_list_v1;
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notifier_v1;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1;
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1, wp_fractional_scale_v1,
};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_manager_v1;
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

use super::desktop::Desktop;
use super::gl::{Gl, Rect, Target};

pub use zwlr_layer_shell_v1::Layer;
pub use zwlr_layer_surface_v1::{Anchor, KeyboardInteractivity};

/// the namespace compositors see omma's surfaces under, for their layer rules
const NAMESPACE: &str = "omma";

pub struct Output {
    /// the registry's name for it
    pub global: u32,
    pub wl: wl_output::WlOutput,
    /// its whole-number scale, for compositors without fractional scaling
    pub scale: i32,
    /// its current mode, in pixels
    pub mode: (i32, i32),
}

impl Output {
    /// about how big it is in logical pixels, from its mode and whole-number scale
    pub fn logical_size(&self) -> (u32, u32) {
        let scale = self.scale.max(1);
        (
            (self.mode.0 / scale).max(1) as u32,
            (self.mode.1 / scale).max(1) as u32,
        )
    }
}

/// a surface on the layer shell, what it shows, and where its frames are
pub struct LayerSurface {
    pub wl: wl_surface::WlSurface,
    layer: zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
    viewport: Option<wp_viewport::WpViewport>,
    fractional: Option<wp_fractional_scale_v1::WpFractionalScaleV1>,
    blur: Option<ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1>,
    target: Option<Target>,
    /// in logical pixels, as the compositor last configured it
    pub size: (u32, u32),
    pub configured: bool,
    /// the compositor took it away, as when its output went
    pub closed: bool,
    /// pixels per logical pixel: the preferred fractional scale, or a whole one
    scale: f64,
    fractional_scale: Option<f64>,
    buffer_scale: Option<i32>,
    /// a frame callback is out, and drawing waits for it
    pub frame_pending: bool,
    /// something changed that the next frame must show
    pub dirty: bool,
    /// it showed a frame
    pub mapped: bool,
    /// the output it showed up on
    pub output: Option<wl_output::WlOutput>,
}

/// what a new layer surface is
pub struct Spec {
    pub output: Option<wl_output::WlOutput>,
    pub layer: Layer,
    pub anchor: Anchor,
    /// 0 lets the compositor size it along an anchored pair of edges
    pub size: (u32, u32),
    pub exclusive_zone: i32,
    pub keyboard: KeyboardInteractivity,
}

#[derive(Default)]
pub struct Shell {
    pub compositor: Option<wl_compositor::WlCompositor>,
    pub layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    pub viewporter: Option<wp_viewporter::WpViewporter>,
    pub fractional: Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>,
    /// the background-effect manager, and whether the compositor says it can blur
    pub blur: Option<(
        ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1,
        bool,
    )>,
    pub outputs: Vec<Output>,
    surfaces: HashMap<u32, LayerSurface>,
    next_id: u32,
}

impl Shell {
    /// a new layer surface, committed so the compositor configures it. None without the
    /// globals it needs
    pub fn create(&mut self, qh: &QueueHandle<Desktop>, spec: Spec) -> Option<u32> {
        let compositor = self.compositor.as_ref()?;
        let layer_shell = self.layer_shell.as_ref()?;
        self.next_id += 1;
        let id = self.next_id;
        let wl = compositor.create_surface(qh, id);
        let layer = layer_shell.get_layer_surface(
            &wl,
            spec.output.as_ref(),
            spec.layer,
            NAMESPACE.into(),
            qh,
            id,
        );
        layer.set_anchor(spec.anchor);
        layer.set_size(spec.size.0, spec.size.1);
        layer.set_exclusive_zone(spec.exclusive_zone);
        layer.set_keyboard_interactivity(spec.keyboard);
        let fractional = self
            .fractional
            .as_ref()
            .map(|manager| manager.get_fractional_scale(&wl, qh, id));
        let viewport = self
            .viewporter
            .as_ref()
            .filter(|_| fractional.is_some())
            .map(|viewporter| viewporter.get_viewport(&wl, qh, ()));
        wl.commit();
        let guess = spec
            .output
            .as_ref()
            .and_then(|output| self.outputs.iter().find(|o| &o.wl == output))
            .or(self.outputs.first())
            .map_or(1, |output| output.scale);
        self.surfaces.insert(
            id,
            LayerSurface {
                wl,
                layer,
                viewport,
                fractional,
                blur: None,
                target: None,
                size: (spec.size.0.max(1), spec.size.1.max(1)),
                configured: false,
                closed: false,
                scale: f64::from(guess),
                fractional_scale: None,
                buffer_scale: None,
                frame_pending: false,
                dirty: true,
                mapped: false,
                output: spec.output,
            },
        );
        Some(id)
    }

    pub fn get(&self, id: u32) -> Option<&LayerSurface> {
        self.surfaces.get(&id)
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut LayerSurface> {
        self.surfaces.get_mut(&id)
    }

    /// the surface a protocol event is about
    pub fn by_wl(&self, surface: &wl_surface::WlSurface) -> Option<u32> {
        self.surfaces
            .iter()
            .find(|(_, s)| &s.wl == surface)
            .map(|(id, _)| *id)
    }

    /// takes the surface away. its EGL surface goes before the wl_surface it draws on
    pub fn destroy(&mut self, id: u32) {
        let Some(surface) = self.surfaces.remove(&id) else {
            return;
        };
        drop(surface.target);
        if let Some(blur) = surface.blur {
            blur.destroy();
        }
        if let Some(viewport) = surface.viewport {
            viewport.destroy();
        }
        if let Some(fractional) = surface.fractional {
            fractional.destroy();
        }
        surface.layer.destroy();
        surface.wl.destroy();
    }

    /// whether the compositor blurs what is behind a surface that asks
    pub fn can_blur(&self) -> bool {
        self.blur.as_ref().is_some_and(|(_, capable)| *capable)
    }

    /// blurs what is behind `rects` of the surface, or nothing for none, where the compositor
    /// can. it takes effect with the surface's next frame
    pub fn set_blur(&mut self, qh: &QueueHandle<Desktop>, id: u32, rects: &[Rect]) {
        let Some((manager, true)) = &self.blur else {
            return;
        };
        let Some(compositor) = &self.compositor else {
            return;
        };
        let Some(surface) = self.surfaces.get_mut(&id) else {
            return;
        };
        let effect = surface
            .blur
            .get_or_insert_with(|| manager.get_background_effect(&surface.wl, qh, ()));
        if rects.is_empty() {
            effect.set_blur_region(None);
            return;
        }
        let region = compositor.create_region(qh, ());
        for rect in rects {
            region.add(
                rect.x.round() as i32,
                rect.y.round() as i32,
                rect.w.round() as i32,
                rect.h.round() as i32,
            );
        }
        effect.set_blur_region(Some(&region));
        region.destroy();
    }

    /// limits where the surface takes the pointer to `rects`, or everywhere for None. it takes
    /// effect with the surface's next frame
    pub fn set_input(&self, qh: &QueueHandle<Desktop>, id: u32, rects: Option<&[Rect]>) {
        let (Some(compositor), Some(surface)) = (&self.compositor, self.surfaces.get(&id)) else {
            return;
        };
        match rects {
            None => surface.wl.set_input_region(None),
            Some(rects) => {
                let region = compositor.create_region(qh, ());
                for rect in rects {
                    region.add(
                        rect.x.floor() as i32,
                        rect.y.floor() as i32,
                        rect.w.ceil() as i32 + 1,
                        rect.h.ceil() as i32 + 1,
                    );
                }
                surface.wl.set_input_region(Some(&region));
                region.destroy();
            }
        }
    }

    pub fn set_keyboard(&self, id: u32, keyboard: KeyboardInteractivity) {
        if let Some(surface) = self.surfaces.get(&id) {
            surface.layer.set_keyboard_interactivity(keyboard);
            surface.wl.commit();
        }
    }
}

impl LayerSurface {
    /// whether it can draw a frame now
    pub fn ready(&self) -> bool {
        self.configured && !self.closed && !self.frame_pending
    }

    /// draws a frame with `draw`, in logical pixels, and asks for a callback before the next
    pub fn render(
        &mut self,
        id: u32,
        gl: &mut Gl,
        qh: &QueueHandle<Desktop>,
        draw: impl FnOnce(&mut Gl) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let (w, h) = self.size;
        let px = match (&self.viewport, self.fractional_scale) {
            (Some(viewport), Some(scale)) => {
                viewport.set_destination(w as i32, h as i32);
                self.wl.set_buffer_scale(1);
                (
                    (f64::from(w) * scale).round() as u32,
                    (f64::from(h) * scale).round() as u32,
                )
            }
            _ => {
                let scale = self.scale.round().max(1.0) as i32;
                self.wl.set_buffer_scale(scale);
                (w * scale as u32, h * scale as u32)
            }
        };
        if self.target.is_none() {
            self.target = Some(gl.target(&self.wl, px)?);
        }
        let target = self.target.as_mut().expect("made above");
        gl.begin(target, px, Rect::new(0.0, 0.0, w as f32, h as f32))?;
        let drawn = draw(gl);
        self.wl.frame(qh, id);
        self.frame_pending = true;
        self.dirty = false;
        self.mapped = true;
        gl.present(target)?;
        drawn
    }

    fn rescale(&mut self, outputs: &[Output]) {
        let whole = self.buffer_scale.or_else(|| {
            let output = self.output.as_ref()?;
            outputs.iter().find(|o| &o.wl == output).map(|o| o.scale)
        });
        let scale = self
            .fractional_scale
            .or(whole.map(f64::from))
            .unwrap_or(self.scale);
        if (scale - self.scale).abs() > 1e-6 {
            self.scale = scale;
            self.dirty = true;
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Desktop {
    fn event(
        state: &mut Desktop,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Desktop>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => {
                let shell = &mut state.shell;
                match interface.as_str() {
                    "wl_compositor" => {
                        shell.compositor = Some(registry.bind(name, version.min(6), qh, ()));
                    }
                    "zwlr_layer_shell_v1" => {
                        shell.layer_shell = Some(registry.bind(name, version.min(4), qh, ()));
                    }
                    "wp_viewporter" => {
                        shell.viewporter = Some(registry.bind(name, 1, qh, ()));
                    }
                    "wp_fractional_scale_manager_v1" => {
                        shell.fractional = Some(registry.bind(name, 1, qh, ()));
                    }
                    "ext_background_effect_manager_v1" => {
                        shell.blur = Some((registry.bind(name, 1, qh, ()), false));
                    }
                    "wl_output" => {
                        let wl = registry.bind(name, version.min(4), qh, name);
                        shell.outputs.push(Output {
                            global: name,
                            wl,
                            scale: 1,
                            mode: (1920, 1080),
                        });
                    }
                    "wl_seat" => state.input.bind_seat(registry, name, version, qh),
                    "wp_cursor_shape_manager_v1" => {
                        let manager: wp_cursor_shape_manager_v1::WpCursorShapeManagerV1 =
                            registry.bind(name, 1, qh, ());
                        state.input.cursor_shape = Some(manager);
                    }
                    "ext_idle_notifier_v1" => {
                        let notifier: ext_idle_notifier_v1::ExtIdleNotifierV1 =
                            registry.bind(name, version.min(2), qh, ());
                        state.idle.notifier = Some(notifier);
                    }
                    "zwlr_foreign_toplevel_manager_v1" => {
                        let manager: zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1 =
                            registry.bind(name, version.min(3), qh, ());
                        state.toplevels.wlr = Some(manager);
                    }
                    "ext_foreign_toplevel_list_v1" => {
                        let list: ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1 =
                            registry.bind(name, 1, qh, ());
                        state.toplevels.ext = Some(list);
                    }
                    _ => {}
                }
            }
            wl_registry::Event::GlobalRemove { name } => {
                state.shell.outputs.retain(|output| output.global != name);
                state.input.remove_seat(name);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for Desktop {
    fn event(
        state: &mut Desktop,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &u32,
        _: &Connection,
        _: &QueueHandle<Desktop>,
    ) {
        let Some(known) = state.shell.outputs.iter_mut().find(|o| &o.wl == output) else {
            return;
        };
        if let wl_output::Event::Mode {
            flags: WEnum::Value(flags),
            width,
            height,
            ..
        } = event
            && flags.contains(wl_output::Mode::Current)
        {
            known.mode = (width, height);
            return;
        }
        if let wl_output::Event::Scale { factor } = event {
            known.scale = factor.max(1);
            let outputs = std::mem::take(&mut state.shell.outputs);
            for surface in state.shell.surfaces.values_mut() {
                surface.rescale(&outputs);
            }
            state.shell.outputs = outputs;
        }
    }
}

impl Dispatch<wl_surface::WlSurface, u32> for Desktop {
    fn event(
        state: &mut Desktop,
        _: &wl_surface::WlSurface,
        event: wl_surface::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Desktop>,
    ) {
        let shell = &mut state.shell;
        let Some(surface) = shell.surfaces.get_mut(id) else {
            return;
        };
        match event {
            wl_surface::Event::Enter { output } => {
                surface.output = Some(output);
                surface.rescale(&shell.outputs);
            }
            wl_surface::Event::PreferredBufferScale { factor } => {
                surface.buffer_scale = Some(factor.max(1));
                surface.rescale(&shell.outputs);
            }
            _ => {}
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, u32> for Desktop {
    fn event(
        state: &mut Desktop,
        layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Desktop>,
    ) {
        let Some(surface) = state.shell.surfaces.get_mut(id) else {
            return;
        };
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                layer.ack_configure(serial);
                let size = (
                    if width == 0 { surface.size.0 } else { width },
                    if height == 0 { surface.size.1 } else { height },
                );
                if size != surface.size || !surface.configured {
                    surface.size = size;
                    surface.dirty = true;
                }
                surface.configured = true;
            }
            zwlr_layer_surface_v1::Event::Closed => surface.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, u32> for Desktop {
    fn event(
        state: &mut Desktop,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Desktop>,
    ) {
        if let wl_callback::Event::Done { .. } = event
            && let Some(surface) = state.shell.surfaces.get_mut(id)
        {
            surface.frame_pending = false;
        }
    }
}

impl Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, u32> for Desktop {
    fn event(
        state: &mut Desktop,
        _: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Desktop>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            let shell = &mut state.shell;
            if let Some(surface) = shell.surfaces.get_mut(id) {
                surface.fractional_scale = Some(f64::from(scale) / 120.0);
                surface.rescale(&shell.outputs);
            }
        }
    }
}

impl Dispatch<ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1, ()> for Desktop {
    fn event(
        state: &mut Desktop,
        _: &ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1,
        event: ext_background_effect_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Desktop>,
    ) {
        if let ext_background_effect_manager_v1::Event::Capabilities {
            flags: WEnum::Value(flags),
        } = event
            && let Some((_, capable)) = &mut state.shell.blur
        {
            *capable = flags.contains(ext_background_effect_manager_v1::Capability::Blur);
        }
    }
}

delegate_noop!(Desktop: ignore wl_compositor::WlCompositor);
delegate_noop!(Desktop: ignore wl_region::WlRegion);
delegate_noop!(Desktop: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);
delegate_noop!(Desktop: ignore wp_viewporter::WpViewporter);
delegate_noop!(Desktop: ignore wp_viewport::WpViewport);
delegate_noop!(Desktop: ignore wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1);
delegate_noop!(Desktop: ignore ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1);
