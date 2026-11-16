//! the GL ES 3 renderer every omma surface draws with, through EGL. it draws rounded shapes
//! with borders and soft shadows, textures, and the bead, all in logical pixels with y down.
//! a layer renders into a texture first, so that it can blur and fade as one, as a Core
//! Animation layer does.

use std::num::NonZeroU32;
use std::ptr::NonNull;

use anyhow::{Context, anyhow};
use glow::HasContext;
use glutin::api::egl::config::Config;
use glutin::api::egl::context::PossiblyCurrentContext;
use glutin::api::egl::display::Display;
use glutin::api::egl::surface::Surface;
use glutin::config::{ConfigTemplateBuilder, GlConfig};
use glutin::context::{ContextApi, ContextAttributesBuilder, Version};
use glutin::prelude::*;
use glutin::surface::{SurfaceAttributesBuilder, SwapInterval, WindowSurface};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Proxy};

/// premultiplied linear-blend RGBA, each 0…1
pub type Rgba = [f32; 4];

/// a color premultiplied by its alpha and by `opacity`
pub fn premul([r, g, b, a]: Rgba, opacity: f32) -> Rgba {
    let a = a * opacity;
    [r * a, g * a, b * a, a]
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub fn inset(&self, by: f32) -> Rect {
        Rect::new(
            self.x + by,
            self.y + by,
            self.w - 2.0 * by,
            self.h - 2.0 * by,
        )
    }

    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// an affine map of the plane: (x, y) → (a x + c y + e, b x + d y + f)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Affine {
    pub const IDENTITY: Affine = Affine {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub fn translate(x: f32, y: f32) -> Affine {
        Affine {
            e: x,
            f: y,
            ..Affine::IDENTITY
        }
    }

    pub fn scale_about(sx: f32, sy: f32, x: f32, y: f32) -> Affine {
        Affine {
            a: sx,
            d: sy,
            e: x - sx * x,
            f: y - sy * y,
            ..Affine::IDENTITY
        }
    }

    /// turns by `angle` radians, clockwise on screen, about the point (x, y)
    pub fn rotate_about(angle: f32, x: f32, y: f32) -> Affine {
        let (sin, cos) = angle.sin_cos();
        Affine {
            a: cos,
            b: sin,
            c: -sin,
            d: cos,
            e: x - cos * x + sin * y,
            f: y - sin * x - cos * y,
        }
    }

    /// `self`, then `outer`
    pub fn then(self, outer: Affine) -> Affine {
        Affine {
            a: outer.a * self.a + outer.c * self.b,
            b: outer.b * self.a + outer.d * self.b,
            c: outer.a * self.c + outer.c * self.d,
            d: outer.b * self.c + outer.d * self.d,
            e: outer.a * self.e + outer.c * self.f + outer.e,
            f: outer.b * self.e + outer.d * self.f + outer.f,
        }
    }

    #[cfg(test)]
    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// as a column-major 3×3 matrix, as GL takes it
    fn matrix(&self) -> [f32; 9] {
        [
            self.a, self.b, 0.0, self.c, self.d, 0.0, self.e, self.f, 1.0,
        ]
    }
}

/// a rounded rectangle, filled and bordered
#[derive(Clone, Copy, Debug)]
pub struct Shape {
    pub fill: Rgba,
    pub border: Rgba,
    pub border_width: f32,
    pub radius: f32,
}

impl Shape {
    pub fn filled(fill: Rgba, radius: f32) -> Shape {
        Shape {
            fill,
            border: [0.0; 4],
            border_width: 0.0,
            radius,
        }
    }
}

pub struct Texture {
    tex: glow::Texture,
    pub width: u32,
    pub height: u32,
}

impl Texture {
    pub fn delete(self, gl: &Gl) {
        // SAFETY: the texture belongs to this context, and it is deleted only once
        unsafe { gl.glow.delete_texture(self.tex) };
    }
}

/// an offscreen target from the pool, and the logical area it covers
pub struct Layer {
    slot: usize,
    pub bounds: Rect,
}

struct Offscreen {
    fbo: glow::Framebuffer,
    tex: glow::Texture,
    size: (u32, u32),
    in_use: bool,
}

/// a program and the locations of its uniforms, looked up by name once
struct Program {
    program: glow::Program,
    uniforms: Vec<(&'static str, Option<glow::UniformLocation>)>,
}

impl Program {
    fn loc(&self, name: &str) -> Option<&glow::UniformLocation> {
        self.uniforms
            .iter()
            .find(|(n, _)| *n == name)
            .and_then(|(_, loc)| loc.as_ref())
    }
}

/// what a target covers: its size in pixels, and the logical area that maps onto it
#[derive(Clone, Copy)]
struct View {
    fbo: Option<glow::Framebuffer>,
    px: (u32, u32),
    area: Rect,
}

/// one EGL window surface, for one wl_surface
pub struct Target {
    surface: Surface<WindowSurface>,
    px: (u32, u32),
}

pub struct Gl {
    pub glow: glow::Context,
    display: Display,
    config: Config,
    context: PossiblyCurrentContext,
    quad: glow::VertexArray,
    shape: Program,
    image: Program,
    bead: Program,
    blur: Program,
    pool: Vec<Offscreen>,
    view: View,
    transform: Affine,
}

const VERTEX: &str = include_str!("shaders/quad.vert");
const SHAPE: &str = include_str!("shaders/shape.frag");
const IMAGE: &str = include_str!("shaders/image.frag");
const BEAD: &str = include_str!("shaders/bead.frag");
const BLUR: &str = include_str!("shaders/blur.frag");

const COMMON_UNIFORMS: [&str; 3] = ["u_rect", "u_xform", "u_view"];

/// the blur's standard deviation, in pixels of the texture it runs over, at most. a wider
/// blur runs over a smaller copy
const MAX_TAP_SIGMA: f32 = 6.0;

impl Gl {
    /// an EGL context for the connection's display, current with no surface
    pub fn new(conn: &Connection) -> anyhow::Result<Gl> {
        let ptr = NonNull::new(conn.backend().display_ptr().cast())
            .context("the Wayland connection has no display pointer")?;
        let raw = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(ptr));
        // SAFETY: the display pointer is the live connection's, which outlives omma's GL
        let display = unsafe { Display::new(raw) }.context("start EGL")?;
        let template = ConfigTemplateBuilder::new()
            .with_alpha_size(8)
            .with_transparency(true)
            .build();
        // SAFETY: the template asks for nothing tied to a window
        let config = unsafe { display.find_configs(template) }
            .context("list EGL configs")?
            .filter(|config| config.supports_transparency().unwrap_or(false))
            .max_by_key(|config| config.num_samples() == 0)
            .context("no EGL config with an alpha channel")?;
        let attributes = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::Gles(Some(Version::new(3, 0))))
            .build(None);
        // SAFETY: the config came from this display
        let context = unsafe { display.create_context(&config, &attributes) }
            .context("create a GL ES 3 context")?
            .make_current_surfaceless()
            .context("make the GL context current")?;
        // SAFETY: the loader is the display's, which lives as long as the context
        let glow = unsafe {
            glow::Context::from_loader_function_cstr(|name| display.get_proc_address(name).cast())
        };

        let compile = |fragment: &str, names: &[&'static str]| -> anyhow::Result<Program> {
            // SAFETY: plain GL calls on the current context
            unsafe { compile(&glow, VERTEX, fragment, names) }
        };
        let shape = compile(
            SHAPE,
            &[
                "u_size",
                "u_pad",
                "u_radius",
                "u_fill",
                "u_border",
                "u_border_width",
                "u_shadow",
            ],
        )?;
        let image = compile(IMAGE, &["u_tex", "u_uv", "u_tint"])?;
        let bead_source = format!("#version 300 es\n{}{BEAD}", super::bead::shader_defines());
        let bead = compile(
            &bead_source,
            &[
                "u_d",
                "u_pad",
                "u_parts",
                "u_solid",
                "u_counts",
                "u_drift",
                "u_flat",
                "u_look",
                "u_blink",
                "u_lid",
                "u_level",
                "u_wave",
                "u_body",
                "u_body_at",
                "u_white",
                "u_sky",
                "u_rings_at",
                "u_pupil",
                "u_glow",
            ],
        )?;
        let blur = compile(BLUR, &["u_tex", "u_step", "u_sigma"])?;

        // SAFETY: plain GL calls on the current context
        let quad = unsafe {
            let vao = glow.create_vertex_array().map_err(|e| anyhow!(e))?;
            let vbo = glow.create_buffer().map_err(|e| anyhow!(e))?;
            glow.bind_vertex_array(Some(vao));
            glow.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            let corners: [f32; 8] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
            let bytes: Vec<u8> = corners.iter().flat_map(|c| c.to_ne_bytes()).collect();
            glow.buffer_data_u8_slice(glow::ARRAY_BUFFER, &bytes, glow::STATIC_DRAW);
            glow.enable_vertex_attrib_array(0);
            glow.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
            glow.enable(glow::BLEND);
            // premultiplied alpha throughout
            glow.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
            vao
        };

        Ok(Gl {
            glow,
            display,
            config,
            context,
            quad,
            shape,
            image,
            bead,
            blur,
            pool: Vec::new(),
            view: View {
                fbo: None,
                px: (1, 1),
                area: Rect::new(0.0, 0.0, 1.0, 1.0),
            },
            transform: Affine::IDENTITY,
        })
    }

    /// an EGL surface for `surface`, `px` pixels big
    pub fn target(&self, surface: &WlSurface, px: (u32, u32)) -> anyhow::Result<Target> {
        let ptr = NonNull::new(surface.id().as_ptr().cast()).context("a dead wl_surface")?;
        let raw = RawWindowHandle::Wayland(WaylandWindowHandle::new(ptr));
        let (w, h) = nonzero(px);
        let attributes = SurfaceAttributesBuilder::<WindowSurface>::new().build(raw, w, h);
        // SAFETY: the wl_surface outlives the EGL surface, which `Target` drops first
        let egl = unsafe {
            self.display
                .create_window_surface(&self.config, &attributes)
        }
        .context("create an EGL surface")?;
        self.context
            .make_current(&egl)
            .context("make the GL context current")?;
        // frame callbacks pace omma, so a swap never waits for the compositor
        let _ = egl.set_swap_interval(&self.context, SwapInterval::DontWait);
        Ok(Target { surface: egl, px })
    }

    /// starts drawing to `target`, which shows `area` of the logical plane at `px` pixels,
    /// cleared to transparent
    pub fn begin(&mut self, target: &mut Target, px: (u32, u32), area: Rect) -> anyhow::Result<()> {
        self.context
            .make_current(&target.surface)
            .context("make the GL context current")?;
        if target.px != px {
            let (w, h) = nonzero(px);
            target.surface.resize(&self.context, w, h);
            target.px = px;
        }
        self.view = View {
            fbo: None,
            px,
            area,
        };
        self.transform = Affine::IDENTITY;
        self.bind_view();
        // SAFETY: plain GL calls on the current context
        unsafe {
            self.glow.clear_color(0.0, 0.0, 0.0, 0.0);
            self.glow.clear(glow::COLOR_BUFFER_BIT);
        }
        Ok(())
    }

    /// shows what was drawn since `begin`, and frees the frame's layers
    pub fn present(&mut self, target: &Target) -> anyhow::Result<()> {
        for offscreen in &mut self.pool {
            offscreen.in_use = false;
        }
        target
            .surface
            .swap_buffers(&self.context)
            .context("swap buffers")
    }

    /// the transform every draw goes through
    pub fn transform(&self) -> Affine {
        self.transform
    }

    pub fn set_transform(&mut self, transform: Affine) {
        self.transform = transform;
    }

    /// runs `draw` with `transform` applied inside the current one
    pub fn with<R>(&mut self, transform: Affine, draw: impl FnOnce(&mut Gl) -> R) -> R {
        let outer = self.transform;
        self.transform = transform.then(outer);
        let result = draw(self);
        self.transform = outer;
        result
    }

    /// pixels per logical pixel on the current target
    pub fn scale(&self) -> f32 {
        self.view.px.0 as f32 / self.view.area.w.max(1e-3)
    }

    fn bind_view(&self) {
        // SAFETY: plain GL calls on the current context
        unsafe {
            self.glow.bind_framebuffer(glow::FRAMEBUFFER, self.view.fbo);
            self.glow
                .viewport(0, 0, self.view.px.0 as i32, self.view.px.1 as i32);
        }
    }

    /// uses `program` for a quad over `rect`, padded by `pad` each way
    fn quad(&self, program: &Program, rect: Rect, pad: f32) {
        let area = self.view.area;
        // SAFETY: plain GL calls on the current context
        unsafe {
            self.glow.use_program(Some(program.program));
            self.glow.uniform_4_f32(
                program.loc("u_rect"),
                rect.x - pad,
                rect.y - pad,
                rect.w + 2.0 * pad,
                rect.h + 2.0 * pad,
            );
            self.glow.uniform_matrix_3_f32_slice(
                program.loc("u_xform"),
                false,
                &self.transform.matrix(),
            );
            self.glow
                .uniform_4_f32(program.loc("u_view"), area.x, area.y, area.w, area.h);
            self.glow.bind_vertex_array(Some(self.quad));
        }
    }

    fn draw_quad(&self) {
        // SAFETY: the quad's vertex array is bound
        unsafe { self.glow.draw_arrays(glow::TRIANGLE_STRIP, 0, 4) };
    }

    /// a rounded rectangle, faded by `opacity`
    pub fn shape(&self, rect: Rect, shape: &Shape, opacity: f32) {
        if opacity <= 0.0 {
            return;
        }
        let pad = 2.0;
        let p = &self.shape;
        self.quad(p, rect, pad);
        let mul = |c: Rgba| c.map(|v| v * opacity);
        let fill = mul(shape.fill);
        let border = mul(shape.border);
        // SAFETY: plain GL calls on the current context
        unsafe {
            self.glow.uniform_2_f32(p.loc("u_size"), rect.w, rect.h);
            self.glow.uniform_1_f32(p.loc("u_pad"), pad);
            self.glow.uniform_1_f32(
                p.loc("u_radius"),
                shape.radius.min(rect.w / 2.0).min(rect.h / 2.0),
            );
            self.glow
                .uniform_4_f32(p.loc("u_fill"), fill[0], fill[1], fill[2], fill[3]);
            self.glow.uniform_4_f32(
                p.loc("u_border"),
                border[0],
                border[1],
                border[2],
                border[3],
            );
            self.glow
                .uniform_1_f32(p.loc("u_border_width"), shape.border_width);
            self.glow.uniform_1_f32(p.loc("u_shadow"), 0.0);
        }
        self.draw_quad();
    }

    /// the soft shadow of a rounded rectangle, `sigma` logical pixels soft
    pub fn shadow(&self, rect: Rect, radius: f32, sigma: f32, color: Rgba) {
        let pad = 3.0 * sigma + 2.0;
        let p = &self.shape;
        self.quad(p, rect, pad);
        // SAFETY: plain GL calls on the current context
        unsafe {
            self.glow.uniform_2_f32(p.loc("u_size"), rect.w, rect.h);
            self.glow.uniform_1_f32(p.loc("u_pad"), pad);
            self.glow.uniform_1_f32(
                p.loc("u_radius"),
                radius.min(rect.w / 2.0).min(rect.h / 2.0),
            );
            self.glow
                .uniform_4_f32(p.loc("u_fill"), color[0], color[1], color[2], color[3]);
            self.glow
                .uniform_4_f32(p.loc("u_border"), 0.0, 0.0, 0.0, 0.0);
            self.glow.uniform_1_f32(p.loc("u_border_width"), 0.0);
            self.glow.uniform_1_f32(p.loc("u_shadow"), sigma.max(0.01));
        }
        self.draw_quad();
    }

    /// a texture over `rect`, multiplied by the premultiplied `tint`. `uv` is the part of the
    /// texture to show: left, top, right, bottom
    pub fn image(&self, texture: &Texture, rect: Rect, tint: Rgba, uv: [f32; 4]) {
        self.texture(texture.tex, rect, tint, uv);
    }

    fn texture(&self, tex: glow::Texture, rect: Rect, tint: Rgba, uv: [f32; 4]) {
        if tint[3] <= 0.0 && tint[..3].iter().all(|c| *c <= 0.0) {
            return;
        }
        let p = &self.image;
        self.quad(p, rect, 0.0);
        // SAFETY: plain GL calls on the current context
        unsafe {
            self.glow.active_texture(glow::TEXTURE0);
            self.glow.bind_texture(glow::TEXTURE_2D, Some(tex));
            self.glow.uniform_1_i32(p.loc("u_tex"), 0);
            self.glow
                .uniform_4_f32(p.loc("u_uv"), uv[0], uv[1], uv[2], uv[3]);
            self.glow
                .uniform_4_f32(p.loc("u_tint"), tint[0], tint[1], tint[2], tint[3]);
        }
        self.draw_quad();
    }

    /// the bead's shader over `rect`, padded by `pad` for its shadow and glow. `set` fills
    /// in the bead's own uniforms
    pub fn bead(
        &self,
        rect: Rect,
        pad: f32,
        set: impl FnOnce(&glow::Context, &dyn Fn(&str) -> Option<glow::UniformLocation>),
    ) {
        let p = &self.bead;
        self.quad(p, rect, pad);
        // SAFETY: plain GL calls on the current context
        unsafe { self.glow.uniform_1_f32(p.loc("u_pad"), pad) };
        set(&self.glow, &|name| p.loc(name).cloned());
        self.draw_quad();
    }

    /// uploads `rgba` pixels, `width` by `height`. it premultiplies them unless they are
    /// `premultiplied` already
    pub fn upload(&self, width: u32, height: u32, rgba: &[u8], premultiplied: bool) -> Texture {
        let data: Vec<u8> = if premultiplied {
            rgba.to_vec()
        } else {
            rgba.as_chunks::<4>()
                .0
                .iter()
                .flat_map(|px| {
                    let a = u16::from(px[3]);
                    let mul = |c: u8| ((u16::from(c) * a + 127) / 255) as u8;
                    [mul(px[0]), mul(px[1]), mul(px[2]), px[3]]
                })
                .collect()
        };
        // SAFETY: plain GL calls on the current context, with a buffer of width × height
        // RGBA pixels
        unsafe {
            let tex = self.glow.create_texture().expect("GL makes textures");
            self.glow.bind_texture(glow::TEXTURE_2D, Some(tex));
            self.glow.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            self.glow.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                width.max(1) as i32,
                height.max(1) as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&data)),
            );
            set_sampling(&self.glow);
            Texture { tex, width, height }
        }
    }

    /// starts drawing into a fresh layer covering `bounds`, in the current target's pixels.
    /// `end_layer` goes back to drawing where it was
    pub fn begin_layer(&mut self, bounds: Rect) -> anyhow::Result<(Layer, LayerReturn)> {
        let scale = self.scale();
        let px = (
            (bounds.w * scale).ceil().max(1.0) as u32,
            (bounds.h * scale).ceil().max(1.0) as u32,
        );
        let slot = self.offscreen(px)?;
        let back = LayerReturn {
            view: self.view,
            transform: self.transform,
        };
        self.view = View {
            fbo: Some(self.pool[slot].fbo),
            px,
            area: Rect::new(bounds.x, bounds.y, px.0 as f32 / scale, px.1 as f32 / scale),
        };
        self.transform = Affine::IDENTITY;
        self.bind_view();
        // SAFETY: plain GL calls on the current context
        unsafe {
            self.glow.clear_color(0.0, 0.0, 0.0, 0.0);
            self.glow.clear(glow::COLOR_BUFFER_BIT);
        }
        let bounds = self.view.area;
        Ok((Layer { slot, bounds }, back))
    }

    pub fn end_layer(&mut self, back: LayerReturn) {
        self.view = back.view;
        self.transform = back.transform;
        self.bind_view();
    }

    /// draws `layer` where it was drawn, through the current transform, blurred by `blur`
    /// logical pixels and faded by `opacity`
    pub fn composite(&mut self, layer: Layer, opacity: f32, blur: f32) -> anyhow::Result<()> {
        if opacity <= 0.0 {
            return Ok(());
        }
        let scale = self.scale();
        let sigma = blur * scale;
        let (tex, _) = if sigma > 0.25 {
            self.blurred(layer.slot, sigma)?
        } else {
            (self.pool[layer.slot].tex, self.pool[layer.slot].size)
        };
        // a layer's first row is its bottom, as GL draws
        self.texture(tex, layer.bounds, [opacity; 4], [0.0, 1.0, 1.0, 0.0]);
        Ok(())
    }

    /// the layer in `slot` blurred by `sigma` of its pixels, in a slot of the pool. a wide
    /// blur runs over a smaller copy, and the blur hides the coarse pixels of that copy
    fn blurred(&mut self, slot: usize, sigma: f32) -> anyhow::Result<(glow::Texture, (u32, u32))> {
        let size = self.pool[slot].size;
        let shrink = (sigma / MAX_TAP_SIGMA).max(1.0);
        let small = (
            ((size.0 as f32 / shrink).ceil() as u32).max(1),
            ((size.1 as f32 / shrink).ceil() as u32).max(1),
        );
        let sigma = sigma * small.0 as f32 / size.0 as f32;
        let back = (self.view, self.transform);
        self.transform = Affine::IDENTITY;
        let whole = Rect::new(0.0, 0.0, 1.0, 1.0);
        let mut source = slot;
        if small != size {
            let into = self.offscreen(small)?;
            self.draw_into(into, small);
            let tex = self.pool[source].tex;
            self.texture(tex, whole, [1.0; 4], [0.0, 1.0, 1.0, 0.0]);
            source = into;
        }
        for step in [(1.0 / small.0 as f32, 0.0), (0.0, 1.0 / small.1 as f32)] {
            let into = self.offscreen(small)?;
            self.draw_into(into, small);
            let p = &self.blur;
            self.quad(p, whole, 0.0);
            // SAFETY: plain GL calls on the current context
            unsafe {
                self.glow.active_texture(glow::TEXTURE0);
                self.glow
                    .bind_texture(glow::TEXTURE_2D, Some(self.pool[source].tex));
                self.glow.uniform_1_i32(p.loc("u_tex"), 0);
                self.glow.uniform_2_f32(p.loc("u_step"), step.0, step.1);
                self.glow.uniform_1_f32(p.loc("u_sigma"), sigma);
            }
            self.draw_quad();
            // the pass before is done, and its slot is free for the next layer of this frame
            if source != slot {
                self.pool[source].in_use = false;
            }
            source = into;
        }
        (self.view, self.transform) = back;
        self.bind_view();
        Ok((self.pool[source].tex, small))
    }

    /// draws into the offscreen target in `slot`, cleared, with the unit square filling it
    fn draw_into(&mut self, slot: usize, px: (u32, u32)) {
        self.view = View {
            fbo: Some(self.pool[slot].fbo),
            px,
            area: Rect::new(0.0, 0.0, 1.0, 1.0),
        };
        self.bind_view();
        // SAFETY: plain GL calls on the current context
        unsafe {
            self.glow.clear_color(0.0, 0.0, 0.0, 0.0);
            self.glow.clear(glow::COLOR_BUFFER_BIT);
        }
    }

    /// a free offscreen target of `size` pixels, from the pool or new
    fn offscreen(&mut self, size: (u32, u32)) -> anyhow::Result<usize> {
        let pool: Vec<_> = self.pool.iter().map(|o| (o.in_use, o.size)).collect();
        let slot = match pick(&pool, size) {
            Pick::Reuse(slot) => {
                self.pool[slot].in_use = true;
                return Ok(slot);
            }
            Pick::Replace(slot) => {
                let old = &self.pool[slot];
                // SAFETY: the objects are this context's, and no layer uses them
                unsafe {
                    self.glow.delete_framebuffer(old.fbo);
                    self.glow.delete_texture(old.tex);
                }
                Some(slot)
            }
            Pick::Add => None,
        };
        // SAFETY: plain GL calls on the current context
        let offscreen = unsafe {
            let tex = self.glow.create_texture().map_err(|e| anyhow!(e))?;
            self.glow.bind_texture(glow::TEXTURE_2D, Some(tex));
            self.glow.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                size.0 as i32,
                size.1 as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            set_sampling(&self.glow);
            let fbo = self.glow.create_framebuffer().map_err(|e| anyhow!(e))?;
            self.glow.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            self.glow.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(tex),
                0,
            );
            self.glow.bind_framebuffer(glow::FRAMEBUFFER, self.view.fbo);
            Offscreen {
                fbo,
                tex,
                size,
                in_use: true,
            }
        };
        match slot {
            Some(slot) => {
                self.pool[slot] = offscreen;
                Ok(slot)
            }
            None => {
                self.pool.push(offscreen);
                Ok(self.pool.len() - 1)
            }
        }
    }

    /// lets go of every offscreen target, while nothing is on screen
    pub fn trim(&mut self) {
        for old in self.pool.drain(..) {
            // SAFETY: the objects are this context's, and no frame is being drawn
            unsafe {
                self.glow.delete_framebuffer(old.fbo);
                self.glow.delete_texture(old.tex);
            }
        }
    }
}

/// how many offscreen targets the pool keeps before it replaces free ones
const POOL: usize = 24;

/// where an offscreen target comes from
#[derive(Debug, PartialEq, Eq)]
enum Pick {
    /// a free target of the size asked for
    Reuse(usize),
    /// a free target of another size, made again in its place once the pool is full. the
    /// other targets keep their index, which layers and blur passes in use hold
    Replace(usize),
    /// a new target at the end of the pool
    Add,
}

/// picks a target of `size` from a pool whose targets are (in use, size)
fn pick(pool: &[(bool, (u32, u32))], size: (u32, u32)) -> Pick {
    if let Some(slot) = pool.iter().position(|&(used, s)| !used && s == size) {
        return Pick::Reuse(slot);
    }
    match pool.iter().position(|&(used, _)| !used) {
        Some(slot) if pool.len() >= POOL => Pick::Replace(slot),
        _ => Pick::Add,
    }
}

/// where drawing goes back to after a layer
pub struct LayerReturn {
    view: View,
    transform: Affine,
}

fn nonzero((w, h): (u32, u32)) -> (NonZeroU32, NonZeroU32) {
    (
        NonZeroU32::new(w.max(1)).expect("at least 1"),
        NonZeroU32::new(h.max(1)).expect("at least 1"),
    )
}

/// linear filtering, clamped at the edges, for the bound texture
///
/// SAFETY: needs a current context and a bound 2D texture
unsafe fn set_sampling(gl: &glow::Context) {
    // SAFETY: as the caller promises
    unsafe {
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_S,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_T,
            glow::CLAMP_TO_EDGE as i32,
        );
    }
}

/// links a program from the shared vertex shader and `fragment`
///
/// SAFETY: needs a current context
unsafe fn compile(
    gl: &glow::Context,
    vertex: &str,
    fragment: &str,
    names: &[&'static str],
) -> anyhow::Result<Program> {
    // SAFETY: as the caller promises
    unsafe {
        let program = gl.create_program().map_err(|e| anyhow!(e))?;
        let mut shaders = Vec::new();
        for (kind, source) in [
            (glow::VERTEX_SHADER, vertex),
            (glow::FRAGMENT_SHADER, fragment),
        ] {
            let shader = gl.create_shader(kind).map_err(|e| anyhow!(e))?;
            gl.shader_source(shader, source);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                let log = gl.get_shader_info_log(shader);
                return Err(anyhow!("a shader didn't compile: {log}"));
            }
            gl.attach_shader(program, shader);
            shaders.push(shader);
        }
        gl.bind_attrib_location(program, 0, "a_unit");
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(anyhow!(
                "a shader didn't link: {}",
                gl.get_program_info_log(program)
            ));
        }
        for shader in shaders {
            gl.detach_shader(program, shader);
            gl.delete_shader(shader);
        }
        let uniforms = COMMON_UNIFORMS
            .iter()
            .chain(names)
            .map(|&name| (name, gl.get_uniform_location(program, name)))
            .collect();
        Ok(Program { program, uniforms })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_pool_replaces_a_free_target_in_place() {
        let mut pool = vec![(true, (1, 1)); POOL];
        pool[5] = (false, (8, 8));
        // a free target of the size comes back as it is
        assert_eq!(pick(&pool, (8, 8)), Pick::Reuse(5));
        // another size takes the free slot, and every other index stays
        assert_eq!(pick(&pool, (4, 4)), Pick::Replace(5));
        // with none free, the pool grows rather than take a target in use
        pool[5].0 = true;
        assert_eq!(pick(&pool, (4, 4)), Pick::Add);
    }

    #[test]
    fn a_pool_with_room_adds_rather_than_replaces() {
        let pool = [(false, (8, 8)), (true, (2, 2))];
        assert_eq!(pick(&pool, (4, 4)), Pick::Add);
        assert_eq!(pick(&pool, (8, 8)), Pick::Reuse(0));
    }
}
