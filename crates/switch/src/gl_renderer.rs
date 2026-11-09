//! Hardware OpenGL 4.3 Core renderer for Nintendo Switch (`#![no_std]` + `alloc`).
//! Renders decoded GX frames into an internal scaled EFB framebuffer using dynamic GL state,
//! pre-created sampler table, persistent GPU Display List VBO/IBO cache, and circular ring buffers.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use bytemuck::{Pod, Zeroable};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use hashbrown::HashMap;

/// Largest framebuffer the console supports, in guest pixels.
pub const EFB_WIDTH: u32 = 640;
pub const EFB_HEIGHT: u32 = 528;

/// Texture maps a material can address.
pub const TEXTURE_SLOTS: usize = 8;

/// Per-draw fragment shader parameters (44 floats = 176 bytes, aligned to 256-byte UBO stride).
pub const PARAMS_FLOATS: usize = 44;
const PARAMS_STRIDE: usize = 256;

/// Per-draw vertex shader parameters (352 words = 1408 bytes, aligned to 1536-byte UBO stride).
pub const VPARAMS_WORDS: usize = 352;
const VPARAMS_STRIDE: usize = 1536;

/// Persistent GPU Display List cache capacity (~192 MB VBO + ~32 MB IBO = 224 MB VRAM).
const CACHE_VERTICES: u64 = 2_000_000;
const CACHE_INDICES: u64 = 8_000_000;

/// Controls whether the GPU FPS / frame-time overlay pill is drawn in `present`.
pub static FPS_ENABLED: AtomicBool = AtomicBool::new(false);
/// Current 4-ASCII-byte scene code (e.g. `0x42423032` = `"BB02"`) for the optional HUD pill.
pub static CURRENT_SCENE_ID: AtomicU32 = AtomicU32::new(0);
/// Internal resolution scale override: 0 = Auto (1x Handheld, 2x Docked), 1 = Force 1x (60 FPS performance), 2 = Force 2x.
pub static SCALE_OVERRIDE: AtomicU32 = AtomicU32::new(0);

unsafe extern "C" {
    fn switch_now_ns() -> u64;
    fn switch_gl_init(efb_w: u32, efb_h: u32, cache_verts: u32, cache_indices: u32) -> i32;
    fn switch_gl_resize_efb(efb_w: u32, efb_h: u32);
    fn switch_gl_get_window_size(out_w: *mut u32, out_h: *mut u32);
    fn switch_gl_compile_fs(src_ptr: *const u8, src_len: u32) -> u32;
    fn switch_gl_link_program(fs_id: u32, is_raw: u32) -> u32;
    fn switch_gl_upload_texture(existing_tex: u32, w: u32, h: u32, rgba: *const u8) -> u32;
    fn switch_gl_create_sampler(wrap_s: u8, wrap_t: u8, linear: u8) -> u32;
    fn switch_gl_cache_upload(
        vert_offset_elems: u32,
        verts: *const core::ffi::c_void,
        vert_bytes: u32,
        idx_offset_elems: u32,
        indices: *const core::ffi::c_void,
        idx_bytes: u32,
    );
    fn switch_gl_execute_pass(pass: *const GlPassDesc);
    fn switch_gl_copy_efb(
        existing_tex: u32,
        alloc_new_size: u32,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> u32;
    fn switch_gl_efb_color_tex() -> u32;
    fn switch_gl_present(
        src_tex: u32,
        uv_rect: *const f32,
        picture_aspect: f32,
        overlay_verts: *const OverlayVertex,
        overlay_vert_count: u32,
    );
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GlDrawCmd {
    program: u32,
    state_bits: u32,
    first_index: u32,
    index_count: u32,
    base_vertex: i32,
    geom_kind: u8,
    pad: [u8; 3],
    params_offset: u32,
    vparams_offset: u32,
    textures: [u32; 8],
    samplers: [u32; 8],
    viewport: [f32; 4],
    scissor: [i32; 4],
}

#[repr(C)]
struct GlPassDesc {
    vertices: *const core::ffi::c_void,
    vertices_bytes: u32,
    raw_vertices: *const core::ffi::c_void,
    raw_bytes: u32,
    indices: *const core::ffi::c_void,
    indices_bytes: u32,
    params: *const core::ffi::c_void,
    params_bytes: u32,
    vparams: *const core::ffi::c_void,
    vparams_bytes: u32,
    cmds: *const GlDrawCmd,
    cmd_count: u32,
    has_clear: u32,
    clear_color: [f32; 4],
}

/// A range of a frame's vertices to be placed again for a wide picture.
#[derive(Clone, Copy)]
pub struct Flat {
    pub from: u32,
    pub to: u32,
    pub origin: Option<[f32; 4]>,
    pub hud: u32,
    pub extent: (f32, f32),
}

/// A vertex already transformed to clip space on the CPU (112 bytes).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 4],
    pub colors: [[f32; 4]; 2],
    pub uvs: [[f32; 4]; 4],
}

/// A vertex as the game sent it, for GPU transform and lighting in `vs_gx` (96 bytes).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct RawVertex {
    pub pos: [f32; 3],
    pub pad0: f32,
    pub normal: [f32; 3],
    pub pad1: f32,
    pub colors: [[f32; 4]; 2],
    pub tex: [[f32; 2]; 4],
}

/// Where a draw's vertices live and who transforms them.
#[derive(Clone, Copy, PartialEq)]
pub enum Geometry {
    Transformed,
    Raw,
    Cached(i32),
}

/// Overlay vertex in NDC `[-1, 1]` (`+Y` up) with RGBA color (24 bytes).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct OverlayVertex {
    pub pos: [f32; 2],
    pub color: [f32; 4],
}

pub type TexKey = (u32, u32, u32, u32, u64);
pub type SamplerKey = (u8, u8, bool);
pub const WHITE_TEXTURE: usize = 0;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DrawState {
    pub depth_test: bool,
    pub depth_write: bool,
    pub depth_func: u8,
    pub blend: bool,
    pub subtract: bool,
    pub src_factor: u8,
    pub dst_factor: u8,
    pub color_write: bool,
    pub alpha_write: bool,
    pub cull: u8,
}

#[inline(always)]
fn pack_state_bits(state: DrawState) -> u32 {
    (state.depth_test as u32)
        | ((state.depth_write as u32) << 1)
        | (((state.depth_func & 7) as u32) << 2)
        | ((state.blend as u32) << 5)
        | ((state.subtract as u32) << 6)
        | (((state.src_factor & 7) as u32) << 7)
        | (((state.dst_factor & 7) as u32) << 10)
        | ((state.color_write as u32) << 13)
        | ((state.alpha_write as u32) << 14)
        | (((state.cull & 3) as u32) << 15)
}

#[inline(always)]
fn sampler_table_index(key: SamplerKey) -> usize {
    let ws = key.0.min(2) as usize;
    let wt = key.1.min(2) as usize;
    let lin = key.2 as usize;
    ws * 6 + wt * 2 + lin
}

pub struct Draw {
    pub first: u32,
    pub count: u32,
    pub state: DrawState,
    pub shader: u64,
    pub textures: [usize; TEXTURE_SLOTS],
    pub samplers: [SamplerKey; TEXTURE_SLOTS],
    pub params: u32,
    pub geometry: Geometry,
    pub vparams: u32,
    pub viewport: [f32; 4],
    pub scissor: [u32; 4],
}

pub struct Frame {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub draws: Vec<Draw>,
    pub flat: Vec<Flat>,
    params: Vec<u8>,
    pub raw: Vec<RawVertex>,
    vparams: Vec<u8>,
    pub raw_flat: Vec<(u32, [f32; 4])>,
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            vertices: Vec::new(),
            indices: Vec::new(),
            draws: Vec::new(),
            flat: Vec::new(),
            params: Vec::new(),
            raw: Vec::new(),
            vparams: Vec::new(),
            raw_flat: Vec::new(),
        }
    }
}

impl Frame {
    pub fn clear(&mut self) {
        self.vertices.clear();
        self.indices.clear();
        self.draws.clear();
        self.flat.clear();
        self.params.clear();
        self.raw.clear();
        self.vparams.clear();
        self.raw_flat.clear();
    }

    pub fn place_again(&mut self, offset: u32, scale: f32, shift: f32) {
        let at = offset as usize + (VPARAMS_WORDS - 4) * 4;
        self.vparams[at..at + 16]
            .copy_from_slice(bytemuck::cast_slice(&[scale, shift, 1.0f32, 1.0]));
    }

    pub fn push_vparams(&mut self, block: &[u32; VPARAMS_WORDS]) -> u32 {
        let bytes: &[u8] = bytemuck::cast_slice(block);
        let offset = self.vparams.len();
        if offset >= VPARAMS_STRIDE && self.vparams[offset - VPARAMS_STRIDE..][..bytes.len()] == *bytes {
            return (offset - VPARAMS_STRIDE) as u32;
        }
        self.vparams.extend_from_slice(bytes);
        self.vparams.resize(offset + VPARAMS_STRIDE, 0);
        offset as u32
    }

    pub fn push_params(&mut self, block: &[f32; PARAMS_FLOATS]) -> u32 {
        let bytes: &[u8] = bytemuck::cast_slice(block);
        let offset = self.params.len();
        if offset >= PARAMS_STRIDE && self.params[offset - PARAMS_STRIDE..][..bytes.len()] == *bytes {
            return (offset - PARAMS_STRIDE) as u32;
        }
        self.params.extend_from_slice(bytes);
        self.params.resize(offset + PARAMS_STRIDE, 0);
        offset as u32
    }

    pub fn push_draw(&mut self, draw: Draw) {
        if let Some(last) = self.draws.last_mut() {
            if last.first + last.count == draw.first
                && last.state == draw.state
                && last.shader == draw.shader
                && last.textures == draw.textures
                && last.samplers == draw.samplers
                && last.params == draw.params
                && last.geometry == draw.geometry
                && !matches!(draw.geometry, Geometry::Cached(_))
                && last.vparams == draw.vparams
                && last.viewport == draw.viewport
                && last.scissor == draw.scissor
            {
                last.count += draw.count;
                return;
            }
        }
        self.draws.push(draw);
    }
}

pub struct Renderer {
    shaders: HashMap<u64, u32>,
    programs: HashMap<(u64, bool), u32>,
    textures: Vec<u32>,
    is_efb_tex: Vec<bool>,
    texture_ids: HashMap<TexKey, usize>,
    samplers: [u32; 18],
    efb_copies: HashMap<u32, (usize, u32, u32)>,
    cache_used: (u64, u64),
    scale: (u32, u32),
    win_size: (u32, u32),
    cmds_scratch: Vec<GlDrawCmd>,
    overlay_scratch: Vec<OverlayVertex>,
    last_present_ns: u64,
    ema_fps: f32,
    ema_ms: f32,
    /// Newly compiled `(shaders, programs, textures, link_ns)` since last query.
    pub made: (u32, u32, u32, u64),
}

impl Renderer {
    /// Initializes EGL + OpenGL 4.3 Core on the calling thread (Core 1 guest engine thread).
    pub fn new() -> Renderer {
        let mut win_w = 1280u32;
        let mut win_h = 720u32;
        unsafe {
            switch_gl_get_window_size(&mut win_w, &mut win_h);
        }
        crate::aspect::set_window(win_w, win_h);
        let scale = Self::wanted_scale(win_h);
        unsafe {
            switch_gl_init(
                EFB_WIDTH * scale.0,
                EFB_HEIGHT * scale.1,
                CACHE_VERTICES as u32,
                CACHE_INDICES as u32,
            );
            switch_gl_get_window_size(&mut win_w, &mut win_h);
        }
        crate::aspect::set_window(win_w, win_h);

        // Pre-create all 18 possible GX sampler objects so draw encoding uses O(1) array indexing.
        let mut samplers = [0u32; 18];
        for ws in 0..3u8 {
            for wt in 0..3u8 {
                for lin in 0..2u8 {
                    let idx = (ws as usize) * 6 + (wt as usize) * 2 + (lin as usize);
                    samplers[idx] = unsafe { switch_gl_create_sampler(ws, wt, lin) };
                }
            }
        }

        let white = [255u8, 255, 255, 255];
        let white_gl = unsafe { switch_gl_upload_texture(0, 1, 1, white.as_ptr()) };

        Renderer {
            shaders: HashMap::with_capacity(96),
            programs: HashMap::with_capacity(128),
            textures: vec![white_gl],
            is_efb_tex: vec![false],
            texture_ids: HashMap::with_capacity(512),
            samplers,
            efb_copies: HashMap::with_capacity(16),
            cache_used: (0, 0),
            scale,
            win_size: (win_w, win_h),
            cmds_scratch: Vec::with_capacity(2048),
            overlay_scratch: Vec::with_capacity(1024),
            last_present_ns: 0,
            ema_fps: 60.0,
            ema_ms: 16.67,
            made: (0, 0, 0, 0),
        }
    }

    #[inline]
    fn wanted_scale(height: u32) -> (u32, u32) {
        match SCALE_OVERRIDE.load(Ordering::Relaxed) {
            1 => (1, 1),
            2 => (2, 2),
            3 => (3, 2),
            _ => {
                // Handheld (<= 720p): (2, 2) = 1280x1056 EFB (active 1280x720 native 1:1 pixel match)
                // Docked (> 720p): (3, 2) = 1920x1056 EFB (active 1920x1080 native 1:1 pixel match)
                if height > 720 { (3, 2) } else { (2, 2) }
            }
        }
    }

    fn follow_window(&mut self) {
        let mut win_w = self.win_size.0;
        let mut win_h = self.win_size.1;
        unsafe {
            switch_gl_get_window_size(&mut win_w, &mut win_h);
        }
        if (win_w, win_h) != self.win_size {
            self.win_size = (win_w, win_h);
            crate::aspect::set_window(win_w, win_h);
            let wanted = Self::wanted_scale(win_h);
            if wanted != self.scale {
                self.scale = wanted;
                unsafe {
                    switch_gl_resize_efb(EFB_WIDTH * wanted.0, EFB_HEIGHT * wanted.1);
                }
            }
        }
    }

    pub fn ensure_shader(&mut self, key: u64, fragment: impl FnOnce() -> String) {
        if !self.shaders.contains_key(&key) {
            let src = fragment();
            let fs_id = unsafe { switch_gl_compile_fs(src.as_ptr(), src.len() as u32) };
            self.shaders.insert(key, fs_id);
            self.made.0 += 1;
        }
    }

    #[inline]
    fn get_or_link_program(&mut self, shader_key: u64, is_raw: bool) -> u32 {
        if let Some(&prog) = self.programs.get(&(shader_key, is_raw)) {
            return prog;
        }
        let Some(&fs_id) = self.shaders.get(&shader_key) else {
            return 0;
        };
        let t0 = unsafe { switch_now_ns() };
        let prog = unsafe { switch_gl_link_program(fs_id, is_raw as u32) };
        let dt = unsafe { switch_now_ns().saturating_sub(t0) };
        self.programs.insert((shader_key, is_raw), prog);
        self.made.1 += 1;
        self.made.3 += dt;
        prog
    }

    pub fn cache_alloc(&mut self, vertices: &[RawVertex], indices: &[u32]) -> Option<(i32, u32)> {
        let (vertex, index) = self.cache_used;
        if vertex + vertices.len() as u64 > CACHE_VERTICES
            || index + indices.len() as u64 > CACHE_INDICES
        {
            return None;
        }
        let vert_bytes = (vertices.len() * core::mem::size_of::<RawVertex>()) as u32;
        let idx_bytes = (indices.len() * 4) as u32;
        unsafe {
            switch_gl_cache_upload(
                vertex as u32,
                vertices.as_ptr() as *const core::ffi::c_void,
                vert_bytes,
                index as u32,
                indices.as_ptr() as *const core::ffi::c_void,
                idx_bytes,
            );
        }
        self.cache_used = (vertex + vertices.len() as u64, index + indices.len() as u64);
        Some((vertex as i32, index as u32))
    }

    pub fn cache_used(&self) -> u64 {
        self.cache_used.0
    }

    pub fn cache_reset(&mut self) {
        self.cache_used = (0, 0);
    }

    pub fn find_texture(&self, key: &TexKey) -> Option<usize> {
        self.texture_ids.get(key).copied()
    }

    pub fn add_texture(&mut self, key: TexKey, w: u32, h: u32, rgba: &[u8]) -> usize {
        let gl_tex = unsafe { switch_gl_upload_texture(0, w, h, rgba.as_ptr()) };
        let id = self.textures.len();
        self.textures.push(gl_tex);
        self.is_efb_tex.push(false);
        self.texture_ids.insert(key, id);
        self.made.2 += 1;
        id
    }

    pub fn efb_copy(&self, addr: u32) -> Option<usize> {
        self.efb_copies.get(&addr).map(|c| c.0)
    }

    #[inline(always)]
    pub fn is_efb_copy(&self, id: usize) -> bool {
        id < self.is_efb_tex.len() && self.is_efb_tex[id]
    }

    pub fn copy_efb(&mut self, addr: u32, rect: [u32; 4]) {
        let (sx, sy) = self.scale;
        let (x, y, w, h) = (rect[0] * sx, rect[1] * sy, rect[2] * sx, rect[3] * sy);
        let (max_w, max_h) = (EFB_WIDTH * sx, EFB_HEIGHT * sy);
        if x >= max_w || y >= max_h {
            return;
        }
        let (w, h) = (w.min(max_w - x), h.min(max_h - y));
        if w == 0 || h == 0 {
            return;
        }
        match self.efb_copies.get(&addr).copied() {
            Some((id, old_w, old_h)) => {
                let resize = (old_w != w || old_h != h) as u32;
                let gl_tex =
                    unsafe { switch_gl_copy_efb(self.textures[id], resize, x, y, w, h) };
                self.textures[id] = gl_tex;
                if resize != 0 {
                    self.efb_copies.insert(addr, (id, w, h));
                }
            }
            None => {
                let gl_tex = unsafe { switch_gl_copy_efb(0, 1, x, y, w, h) };
                let id = self.textures.len();
                self.textures.push(gl_tex);
                self.is_efb_tex.push(true);
                self.efb_copies.insert(addr, (id, w, h));
            }
        }
    }

    fn encode_and_execute(&mut self, frame: &Frame, clear: Option<[f64; 4]>) {
        let t_enc0 = unsafe { switch_now_ns() };
        self.cmds_scratch.clear();
        let (sx, sy) = self.scale;
        let (max_w, max_h) = (EFB_WIDTH * sx, EFB_HEIGHT * sy);

        let mut last_prog_key: Option<(u64, bool)> = None;
        let mut last_prog_id: u32 = 0;

        for draw in &frame.draws {
            if draw.count == 0 {
                continue;
            }
            let is_raw = draw.geometry != Geometry::Transformed;
            let pkey = (draw.shader, is_raw);
            let program = if last_prog_key == Some(pkey) {
                last_prog_id
            } else {
                let p = self.get_or_link_program(draw.shader, is_raw);
                last_prog_key = Some(pkey);
                last_prog_id = p;
                p
            };
            if program == 0 {
                continue;
            }

            let [vx, vy, vw, vh] = draw.viewport;
            let (vx, vy, vw, vh) = (
                vx * sx as f32,
                vy * sy as f32,
                vw * sx as f32,
                vh * sy as f32,
            );
            let (x0, y0) = (vx.max(0.0), vy.max(0.0));
            let (x1, y1) = ((vx + vw).min(max_w as f32), (vy + vh).min(max_h as f32));
            let [l, t, r, b] = draw.scissor;
            let (l, t, r, b) = (l * sx, t * sy, r * sx, b * sy);
            let (r, b) = (r.min(max_w), b.min(max_h));
            if x1 <= x0 || y1 <= y0 || r <= l || b <= t {
                continue;
            }

            let mut textures = [0u32; 8];
            let mut samplers = [0u32; 8];
            for slot in 0..8 {
                let tid = draw.textures[slot];
                textures[slot] = if tid < self.textures.len() {
                    self.textures[tid]
                } else {
                    self.textures[WHITE_TEXTURE]
                };
                samplers[slot] = self.samplers[sampler_table_index(draw.samplers[slot])];
            }

            let (geom_kind, base_vertex) = match draw.geometry {
                Geometry::Transformed => (0u8, 0i32),
                Geometry::Raw => (1u8, 0i32),
                Geometry::Cached(base) => (2u8, base),
            };

            self.cmds_scratch.push(GlDrawCmd {
                program,
                state_bits: pack_state_bits(draw.state),
                first_index: draw.first,
                index_count: draw.count,
                base_vertex,
                geom_kind,
                pad: [0; 3],
                params_offset: draw.params,
                vparams_offset: draw.vparams,
                textures,
                samplers,
                viewport: [x0, y0, x1 - x0, y1 - y0],
                scissor: [l as i32, t as i32, (r - l) as i32, (b - t) as i32],
            });
        }

        let verts_bytes: &[u8] = bytemuck::cast_slice(&frame.vertices);
        let raw_bytes: &[u8] = bytemuck::cast_slice(&frame.raw);
        let idx_bytes: &[u8] = bytemuck::cast_slice(&frame.indices);

        let (has_clear, clear_color) = match clear {
            Some([r, g, b, a]) => (1u32, [r as f32, g as f32, b as f32, a as f32]),
            None => (0u32, [0.0; 4]),
        };

        let pass = GlPassDesc {
            vertices: if verts_bytes.is_empty() {
                core::ptr::null()
            } else {
                verts_bytes.as_ptr() as *const core::ffi::c_void
            },
            vertices_bytes: verts_bytes.len() as u32,
            raw_vertices: if raw_bytes.is_empty() {
                core::ptr::null()
            } else {
                raw_bytes.as_ptr() as *const core::ffi::c_void
            },
            raw_bytes: raw_bytes.len() as u32,
            indices: if idx_bytes.is_empty() {
                core::ptr::null()
            } else {
                idx_bytes.as_ptr() as *const core::ffi::c_void
            },
            indices_bytes: idx_bytes.len() as u32,
            params: if frame.params.is_empty() {
                core::ptr::null()
            } else {
                frame.params.as_ptr() as *const core::ffi::c_void
            },
            params_bytes: frame.params.len() as u32,
            vparams: if frame.vparams.is_empty() {
                core::ptr::null()
            } else {
                frame.vparams.as_ptr() as *const core::ffi::c_void
            },
            vparams_bytes: frame.vparams.len() as u32,
            cmds: if self.cmds_scratch.is_empty() {
                core::ptr::null()
            } else {
                self.cmds_scratch.as_ptr()
            },
            cmd_count: self.cmds_scratch.len() as u32,
            has_clear,
            clear_color,
        };
        let dt_enc = unsafe { switch_now_ns().saturating_sub(t_enc0) };
        crate::PERF_ENCODE_PASS_NS.fetch_add(dt_enc, Ordering::Relaxed);

        let t0 = unsafe { switch_now_ns() };
        unsafe {
            switch_gl_execute_pass(&pass);
        }
        let dt = unsafe { switch_now_ns().saturating_sub(t0) };
        crate::PERF_GL_NS.fetch_add(dt, Ordering::Relaxed);
    }

    pub fn flush(&mut self, frame: &Frame, clear: Option<[f64; 4]>) {
        if frame.draws.is_empty() && clear.is_none() {
            return;
        }
        self.encode_and_execute(frame, clear);
    }

    pub fn present(&mut self, frame: &Frame, clear: Option<[f64; 4]>, shown: [u32; 4], _number: u64) {
        self.encode_and_execute(frame, clear);

        let now = unsafe { switch_now_ns() };
        if self.last_present_ns != 0 && now > self.last_present_ns {
            let dt_ns = ((now - self.last_present_ns) as f32).clamp(1.0e6, 250.0e6);
            let inst_ms = dt_ns * 1.0e-6;
            self.ema_ms = self.ema_ms * 0.88 + inst_ms * 0.12;
            self.ema_fps = 1000.0 / self.ema_ms.max(0.1);
        }
        self.last_present_ns = now;

        self.overlay_scratch.clear();
        if FPS_ENABLED.load(Ordering::Relaxed) {
            self.build_fps_overlay();
        }

        let (w, h) = (EFB_WIDTH as f32, EFB_HEIGHT as f32);
        let rect = [
            shown[0] as f32 / w,
            shown[1] as f32 / h,
            shown[2] as f32 / w,
            shown[3] as f32 / h,
        ];
        static LAST_PRESENT_LOG: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
        if now.saturating_sub(LAST_PRESENT_LOG.load(Ordering::Relaxed)) > 2_000_000_000 {
            LAST_PRESENT_LOG.store(now, Ordering::Relaxed);
            crate::log_c(&alloc::format!(
                "[present] shown={:?}, rect={:?}, scale={:?}, win={:?}, pic={:.3}",
                shown, rect, self.scale, self.win_size, crate::aspect::picture()
            ));
        }
        let src_tex = unsafe { switch_gl_efb_color_tex() };
        let (ov_ptr, ov_len) = if self.overlay_scratch.is_empty() {
            (core::ptr::null(), 0u32)
        } else {
            (
                self.overlay_scratch.as_ptr(),
                self.overlay_scratch.len() as u32,
            )
        };

        let t0 = unsafe { switch_now_ns() };
        unsafe {
            switch_gl_present(src_tex, rect.as_ptr(), crate::aspect::picture(), ov_ptr, ov_len);
        }
        let dt = unsafe { switch_now_ns().saturating_sub(t0) };
        crate::PERF_GL_NS.fetch_add(dt, Ordering::Relaxed);
        self.follow_window();
    }

    #[inline]
    pub fn current_fps_ms(&self) -> (f32, f32) {
        (self.ema_fps, self.ema_ms)
    }

    fn push_quad_px(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        let (ww, wh) = (self.win_size.0.max(1) as f32, self.win_size.1.max(1) as f32);
        let x0 = (x / ww) * 2.0 - 1.0;
        let x1 = ((x + w) / ww) * 2.0 - 1.0;
        let y0 = 1.0 - (y / wh) * 2.0;
        let y1 = 1.0 - ((y + h) / wh) * 2.0;
        self.overlay_scratch.extend_from_slice(&[
            OverlayVertex { pos: [x0, y0], color },
            OverlayVertex { pos: [x1, y0], color },
            OverlayVertex { pos: [x1, y1], color },
            OverlayVertex { pos: [x0, y0], color },
            OverlayVertex { pos: [x1, y1], color },
            OverlayVertex { pos: [x0, y1], color },
        ]);
    }

    fn build_fps_overlay(&mut self) {
        use core::fmt::Write as _;
        let sc = CURRENT_SCENE_ID.load(Ordering::Relaxed);
        let mut label = String::with_capacity(32);
        if sc != 0 {
            let b = sc.to_be_bytes();
            for &ch in &b {
                let c = if (0x20..=0x7E).contains(&ch) { ch as char } else { '?' };
                label.push(c);
            }
            label.push(' ');
        }
        let fps_tenths = (self.ema_fps * 10.0 + 0.5) as u32;
        let ms_tenths = (self.ema_ms * 10.0 + 0.5) as u32;
        let _ = write!(
            label,
            "{}.{} FPS {}.{}MS",
            fps_tenths / 10,
            fps_tenths % 10,
            ms_tenths / 10,
            ms_tenths % 10
        );

        let scale = if self.win_size.1 > 720 { 3.0f32 } else { 2.0f32 };
        let pad_x = 10.0 * (scale * 0.5);
        let pad_y = 7.0 * (scale * 0.5);
        let text_w = (label.len() as f32) * 8.0 * scale;
        let box_w = text_w + pad_x * 2.0;
        let box_h = 8.0 * scale + pad_y * 2.0;
        let bx = (self.win_size.0 as f32) - box_w - 20.0;
        let by = 18.0;

        self.push_quad_px(bx, by, box_w, box_h, [0.04, 0.06, 0.10, 0.80]);
        let fg = if self.ema_fps >= 57.5 {
            [0.20, 1.0, 0.50, 1.0]
        } else if self.ema_fps >= 45.0 {
            [1.0, 0.85, 0.20, 1.0]
        } else {
            [1.0, 0.35, 0.30, 1.0]
        };

        let mut cx = bx + pad_x;
        let cy = by + pad_y;
        for ch in label.bytes() {
            let rows = glyph_8x8(ch);
            for (ry, &bits) in rows.iter().enumerate() {
                if bits == 0 {
                    continue;
                }
                let mut rx = 0usize;
                while rx < 8 {
                    if (bits & (0x80 >> rx)) != 0 {
                        let start = rx;
                        while rx < 8 && (bits & (0x80 >> rx)) != 0 {
                            rx += 1;
                        }
                        self.push_quad_px(
                            cx + (start as f32) * scale,
                            cy + (ry as f32) * scale,
                            ((rx - start) as f32) * scale,
                            scale,
                            fg,
                        );
                    } else {
                        rx += 1;
                    }
                }
            }
            cx += 8.0 * scale;
        }
    }
}

fn glyph_8x8(ch: u8) -> [u8; 8] {
    match ch {
        b'0' => [0x3C, 0x66, 0x6E, 0x76, 0x66, 0x66, 0x3C, 0x00],
        b'1' => [0x18, 0x38, 0x18, 0x18, 0x18, 0x18, 0x7E, 0x00],
        b'2' => [0x3C, 0x66, 0x06, 0x0C, 0x18, 0x30, 0x7E, 0x00],
        b'3' => [0x7E, 0x06, 0x0C, 0x1C, 0x06, 0x66, 0x3C, 0x00],
        b'4' => [0x0C, 0x1C, 0x34, 0x64, 0x7E, 0x0C, 0x0C, 0x00],
        b'5' => [0x7E, 0x60, 0x7C, 0x06, 0x06, 0x66, 0x3C, 0x00],
        b'6' => [0x1C, 0x30, 0x60, 0x7C, 0x66, 0x66, 0x3C, 0x00],
        b'7' => [0x7E, 0x06, 0x0C, 0x18, 0x30, 0x30, 0x30, 0x00],
        b'8' => [0x3C, 0x66, 0x66, 0x3C, 0x66, 0x66, 0x3C, 0x00],
        b'9' => [0x3C, 0x66, 0x66, 0x3E, 0x06, 0x0C, 0x38, 0x00],
        b'.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x18, 0x00],
        b'A' => [0x18, 0x3C, 0x66, 0x66, 0x7E, 0x66, 0x66, 0x00],
        b'B' => [0x7C, 0x66, 0x66, 0x7C, 0x66, 0x66, 0x7C, 0x00],
        b'C' => [0x3C, 0x66, 0x60, 0x60, 0x60, 0x66, 0x3C, 0x00],
        b'D' => [0x78, 0x6C, 0x66, 0x66, 0x66, 0x6C, 0x78, 0x00],
        b'E' => [0x7E, 0x60, 0x60, 0x7C, 0x60, 0x60, 0x7E, 0x00],
        b'F' => [0x7E, 0x60, 0x60, 0x7C, 0x60, 0x60, 0x60, 0x00],
        b'G' => [0x3C, 0x66, 0x60, 0x6E, 0x66, 0x66, 0x3E, 0x00],
        b'H' => [0x66, 0x66, 0x66, 0x7E, 0x66, 0x66, 0x66, 0x00],
        b'I' => [0x3C, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3C, 0x00],
        b'J' => [0x1E, 0x0C, 0x0C, 0x0C, 0x0C, 0x6C, 0x38, 0x00],
        b'K' => [0x66, 0x6C, 0x78, 0x70, 0x78, 0x6C, 0x66, 0x00],
        b'L' => [0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x7E, 0x00],
        b'M' => [0x63, 0x77, 0x7F, 0x6B, 0x63, 0x63, 0x63, 0x00],
        b'N' => [0x66, 0x76, 0x7E, 0x7E, 0x6E, 0x66, 0x66, 0x00],
        b'O' => [0x3C, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00],
        b'P' => [0x7C, 0x66, 0x66, 0x7C, 0x60, 0x60, 0x60, 0x00],
        b'Q' => [0x3C, 0x66, 0x66, 0x66, 0x6A, 0x6C, 0x36, 0x00],
        b'R' => [0x7C, 0x66, 0x66, 0x7C, 0x6C, 0x66, 0x66, 0x00],
        b'S' => [0x3C, 0x66, 0x60, 0x3C, 0x06, 0x66, 0x3C, 0x00],
        b'T' => [0x7E, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x00],
        b'U' => [0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00],
        b'V' => [0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x18, 0x00],
        b'W' => [0x63, 0x63, 0x63, 0x6B, 0x7F, 0x77, 0x63, 0x00],
        b'X' => [0x66, 0x66, 0x3C, 0x18, 0x3C, 0x66, 0x66, 0x00],
        b'Y' => [0x66, 0x66, 0x66, 0x3C, 0x18, 0x18, 0x18, 0x00],
        b'Z' => [0x7E, 0x06, 0x0C, 0x18, 0x30, 0x60, 0x7E, 0x00],
        _ => [0; 8],
    }
}
