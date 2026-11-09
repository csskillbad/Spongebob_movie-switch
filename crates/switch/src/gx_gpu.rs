// Hardware-accelerated GameCube GX command processor for Nintendo Switch (`#![no_std]` + `alloc`).
// Decodes BP/CP/XF state, display lists, vertex streams, EFB texture copies, and 16:9 widescreen
// HUD anchoring, feeding `gl_renderer::Renderer` with persistent GPU Display List caching.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use hashbrown::{HashMap, HashSet};
use runtime::Ctx;

use crate::gl_renderer::{
    Draw, DrawState, EFB_HEIGHT, EFB_WIDTH, Flat, Frame, Geometry, PARAMS_FLOATS, RawVertex,
    Renderer, SamplerKey, TEXTURE_SLOTS, VPARAMS_WORDS, Vertex, WHITE_TEXTURE,
};
use crate::tev_gl;
use crate::texture;

#[inline(always)]
fn in_ram(addr: u32, len: usize) -> bool {
    let end = addr as u64 + len as u64;
    addr >= runtime::mem::RAM_BASE
        && end <= (runtime::mem::RAM_BASE + runtime::mem::RAM_SIZE) as u64
}

#[inline(always)]
fn fifo_slice(c: &Ctx) -> &[u8] {
    if c.fifo_len == 0 {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(c.mem.add(0x0200_0000), c.fifo_len) }
    }
}

/// Byte widths of position/normal/texcoord component formats: u8 s8 u16 s16 f32.
const COMP: [usize; 8] = [1, 1, 2, 2, 4, 0, 0, 0];
/// Byte widths of vertex color formats: 565, 888, 888x, 4444, 6666, 8888.
const COLOR: [usize; 8] = [2, 3, 4, 2, 3, 4, 0, 0];

#[inline(always)]
fn be16(d: &[u8], o: usize) -> usize {
    u16::from_be_bytes([d[o], d[o + 1]]) as usize
}

#[inline(always)]
fn be32(d: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(d[o..o + 4].try_into().unwrap())
}

/// Masked guest memory accessor for Switch's 128 MB guest address buffer (`addr & 0x07FF_FFFF`).
#[derive(Clone, Copy)]
struct Mem(*const u8);

unsafe impl Send for Mem {}
unsafe impl Sync for Mem {}

impl Mem {
    #[inline(always)]
    fn r8(self, addr: u32) -> u8 {
        unsafe { self.0.add((addr & 0x07FF_FFFF) as usize).read() }
    }

    #[inline(always)]
    fn r64(self, addr: u32) -> u64 {
        unsafe {
            u64::from_be(
                (self.0.add((addr & 0x07FF_FFFF) as usize) as *const u64).read_unaligned(),
            )
        }
    }

    #[inline(always)]
    unsafe fn slice(self, addr: u32, len: usize) -> &'static [u8] {
        unsafe { core::slice::from_raw_parts(self.0.add((addr & 0x07FF_FFFF) as usize), len) }
    }
}

#[inline(always)]
fn fetch(
    c: Mem,
    data: &[u8],
    o: &mut usize,
    kind: u32,
    size: usize,
    array: (u32, u32),
    buf: &mut [u8; 16],
) -> u32 {
    let index = match kind {
        1 => {
            buf[..size].copy_from_slice(&data[*o..*o + size]);
            *o += size;
            return u32::MAX;
        }
        2 => {
            *o += 1;
            data[*o - 1] as u32
        }
        3 => {
            *o += 2;
            be16(data, *o - 2) as u32
        }
        _ => return u32::MAX,
    };
    let addr = array.0.wrapping_add(index * array.1);
    if in_ram(addr, 16) {
        unsafe {
            core::ptr::copy_nonoverlapping(
                c.0.add((addr & 0x07FF_FFFF) as usize),
                buf.as_mut_ptr(),
                16,
            );
        }
        return index;
    }
    for (k, byte) in buf[..size].iter_mut().enumerate() {
        *byte = c.r8(addr.wrapping_add(k as u32));
    }
    index
}

#[derive(Clone, Copy)]
struct Format {
    vcd_lo: u32,
    vcd_hi: u32,
    a: u32,
    b: u32,
    arrays: [(u32, u32); 12],
}

#[inline(always)]
unsafe fn be_f32_at(b: &[u8; 16], offset: usize) -> f32 {
    let w = u32::from_be(core::ptr::read_unaligned(b.as_ptr().add(offset) as *const u32));
    f32::from_bits(w)
}

#[inline(always)]
unsafe fn be_u16_at(b: &[u8; 16], offset: usize) -> u16 {
    u16::from_be(core::ptr::read_unaligned(b.as_ptr().add(offset) as *const u16))
}

#[inline(always)]
unsafe fn be_i16_at(b: &[u8; 16], offset: usize) -> i16 {
    i16::from_be(core::ptr::read_unaligned(b.as_ptr().add(offset) as *const i16))
}

fn decode_raw(
    mem: Mem,
    data: &[u8],
    count: usize,
    format: &Format,
    out: &mut Vec<RawVertex>,
    ranges: &mut [(u32, u32); 12],
) {
    let Format {
        vcd_lo,
        vcd_hi,
        a,
        b,
        ref arrays,
    } = *format;
    let kind = |shift: u32| (vcd_lo >> shift) & 3;
    let indexed = |k: u32| match k {
        2 => 1,
        3 => 2,
        _ => 0,
    };
    let (pos_count, pos_format) = (2 + (a & 1) as usize, (a >> 1) & 7);
    let pos_scale = 1.0 / (1u64 << ((a >> 4) & 31)) as f32;
    let pos_size = pos_count * COMP[pos_format as usize];
    let nbt = (a >> 9) & 1 != 0;
    let normal_format = (a >> 10) & 7;
    let normal_size = match kind(11) {
        1 => (if nbt { 9 } else { 3 }) * COMP[normal_format as usize],
        k => indexed(k) * (if nbt && a >> 31 != 0 { 3 } else { 1 }),
    };
    let normal_bytes = 3 * COMP[normal_format as usize];
    let color_format = [(a >> 14) & 7, (a >> 18) & 7];
    let color_kinds = [kind(13), kind(15)];
    let tex = [
        (a >> 21, a >> 22),
        (b, b >> 1),
        (b >> 9, b >> 10),
        (b >> 18, b >> 19),
    ];
    let tex_kind = |i: usize| (vcd_hi >> (i * 2)) & 3;
    let tex_scale =
        [a >> 25, b >> 4, b >> 13, b >> 22].map(|shift| 1.0 / (1u64 << (shift & 31)) as f32);
    let mut note = |n: usize, index: u32| {
        if index != u32::MAX {
            ranges[n] = (ranges[n].0.min(index), ranges[n].1.max(index));
        }
    };
    out.reserve(count);
    let mut buf = [0u8; 16];
    let mut o = 0;
    for _ in 0..count {
        note(
            0,
            fetch(mem, data, &mut o, kind(9), pos_size, arrays[0], &mut buf),
        );
        let pos = match pos_format {
            4 => [
                unsafe { be_f32_at(&buf, 0) },
                unsafe { be_f32_at(&buf, 4) },
                if pos_count == 3 { unsafe { be_f32_at(&buf, 8) } } else { 0.0 },
            ],
            2 => [
                (unsafe { be_u16_at(&buf, 0) }) as f32 * pos_scale,
                (unsafe { be_u16_at(&buf, 2) }) as f32 * pos_scale,
                if pos_count == 3 { (unsafe { be_u16_at(&buf, 4) }) as f32 * pos_scale } else { 0.0 },
            ],
            3 => [
                (unsafe { be_i16_at(&buf, 0) }) as f32 * pos_scale,
                (unsafe { be_i16_at(&buf, 2) }) as f32 * pos_scale,
                if pos_count == 3 { (unsafe { be_i16_at(&buf, 4) }) as f32 * pos_scale } else { 0.0 },
            ],
            0 => [
                buf[0] as f32 * pos_scale,
                buf[1] as f32 * pos_scale,
                if pos_count == 3 { buf[2] as f32 * pos_scale } else { 0.0 },
            ],
            1 => [
                buf[0] as i8 as f32 * pos_scale,
                buf[1] as i8 as f32 * pos_scale,
                if pos_count == 3 { buf[2] as i8 as f32 * pos_scale } else { 0.0 },
            ],
            _ => [
                unsafe { be_f32_at(&buf, 0) },
                unsafe { be_f32_at(&buf, 4) },
                if pos_count == 3 { unsafe { be_f32_at(&buf, 8) } } else { 0.0 },
            ],
        };
        let mut normal = [0.0f32; 3];
        if kind(11) != 0 {
            let mut at = o;
            note(
                1,
                fetch(
                    mem,
                    data,
                    &mut at,
                    kind(11),
                    normal_bytes,
                    arrays[1],
                    &mut buf,
                ),
            );
            normal = match normal_format {
                4 => [
                    unsafe { be_f32_at(&buf, 0) },
                    unsafe { be_f32_at(&buf, 4) },
                    unsafe { be_f32_at(&buf, 8) },
                ],
                0 | 1 => [
                    buf[0] as i8 as f32 / 64.0,
                    buf[1] as i8 as f32 / 64.0,
                    buf[2] as i8 as f32 / 64.0,
                ],
                2 | 3 => [
                    (unsafe { be_i16_at(&buf, 0) }) as f32 / 16384.0,
                    (unsafe { be_i16_at(&buf, 2) }) as f32 / 16384.0,
                    (unsafe { be_i16_at(&buf, 4) }) as f32 / 16384.0,
                ],
                _ => [
                    unsafe { be_f32_at(&buf, 0) },
                    unsafe { be_f32_at(&buf, 4) },
                    unsafe { be_f32_at(&buf, 8) },
                ],
            };
        }
        o += normal_size;
        let mut colors = [[0.0f32; 4]; 2];
        for n in 0..2 {
            if color_kinds[n] != 0 {
                let fmt = color_format[n];
                note(
                    2 + n,
                    fetch(
                        mem,
                        data,
                        &mut o,
                        color_kinds[n],
                        COLOR[fmt as usize],
                        arrays[2 + n],
                        &mut buf,
                    ),
                );
                colors[n] = decode_color(fmt, &buf);
            }
        }
        let mut coords = [[0.0f32; 2]; 4];
        for i in 0..4 {
            let k = tex_kind(i);
            if k == 0 {
                continue;
            }
            let (components, component) = (1 + (tex[i].0 & 1) as usize, tex[i].1 & 7);
            note(
                4 + i,
                fetch(
                    mem,
                    data,
                    &mut o,
                    k,
                    components * COMP[component as usize],
                    arrays[4 + i],
                    &mut buf,
                ),
            );
            let scale = tex_scale[i];
            match component {
                4 => {
                    coords[i][0] = unsafe { be_f32_at(&buf, 0) };
                    if components > 1 {
                        coords[i][1] = unsafe { be_f32_at(&buf, 4) };
                    }
                }
                2 => {
                    coords[i][0] = (unsafe { be_u16_at(&buf, 0) }) as f32 * scale;
                    if components > 1 {
                        coords[i][1] = (unsafe { be_u16_at(&buf, 2) }) as f32 * scale;
                    }
                }
                3 => {
                    coords[i][0] = (unsafe { be_i16_at(&buf, 0) }) as f32 * scale;
                    if components > 1 {
                        coords[i][1] = (unsafe { be_i16_at(&buf, 2) }) as f32 * scale;
                    }
                }
                0 => {
                    coords[i][0] = buf[0] as f32 * scale;
                    if components > 1 {
                        coords[i][1] = buf[1] as f32 * scale;
                    }
                }
                1 => {
                    coords[i][0] = buf[0] as i8 as f32 * scale;
                    if components > 1 {
                        coords[i][1] = buf[1] as i8 as f32 * scale;
                    }
                }
                _ => {
                    coords[i][0] = unsafe { be_f32_at(&buf, 0) };
                    if components > 1 {
                        coords[i][1] = unsafe { be_f32_at(&buf, 4) };
                    }
                }
            }
        }
        out.push(RawVertex {
            pos,
            pad0: 0.0,
            normal,
            pad1: 0.0,
            colors,
            tex: coords,
        });
    }
}

fn emit_indices(op: u8, base: u32, count: u32, out: &mut Vec<u32>) {
    let needed = match op & 0xF8 {
        0x80 | 0x88 => (count as usize / 4) * 6,
        0x90 => count as usize - (count as usize % 3),
        0x98 | 0xA0 => (count as usize).saturating_sub(2) * 3,
        _ => 0,
    };
    out.reserve(needed);
    match op & 0xF8 {
        0x80 | 0x88 => {
            for q in (0..count / 4).map(|q| base + q * 4) {
                out.extend_from_slice(&[q, q + 1, q + 2, q, q + 2, q + 3]);
            }
        }
        0x90 => out.extend(base..base + count - count % 3),
        0x98 => {
            for i in 0..count.saturating_sub(2) {
                let v = base + i;
                out.extend_from_slice(&if i % 2 == 0 {
                    [v, v + 1, v + 2]
                } else {
                    [v + 1, v, v + 2]
                });
            }
        }
        0xA0 => {
            for i in 1..count.saturating_sub(1) {
                out.extend_from_slice(&[base, base + i, base + i + 1]);
            }
        }
        _ => {}
    }
}

#[inline]
fn hash_bytes(data: &[u8]) -> u64 {
    texture::hash(data)
}

#[inline(always)]
fn fast_block_hash(data: &[u8]) -> u64 {
    if data.len() >= 32 {
        let w0 = u64::from_le_bytes(data[..8].try_into().unwrap());
        let w1 = u64::from_le_bytes(data[data.len() / 2..data.len() / 2 + 8].try_into().unwrap());
        let w2 = u64::from_le_bytes(data[data.len() - 8..].try_into().unwrap());
        (data.len() as u64)
            ^ w0.wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ w1.rotate_left(23).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
            ^ w2.rotate_left(47)
    } else {
        texture::hash(data)
    }
}

const BLOCK_SHIFT: u32 = 12;

#[inline(always)]
fn decode_color(format: u32, b: &[u8; 16]) -> [f32; 4] {
    let wide = |v: u32, bits: u32| v as f32 * (1.0 / ((1u32 << bits) - 1) as f32);
    match format {
        0 => {
            let v = (b[0] as u32) << 8 | b[1] as u32;
            [
                wide(v >> 11, 5),
                wide((v >> 5) & 63, 6),
                wide(v & 31, 5),
                1.0,
            ]
        }
        1 | 2 => [
            wide(b[0] as u32, 8),
            wide(b[1] as u32, 8),
            wide(b[2] as u32, 8),
            1.0,
        ],
        3 => {
            let v = (b[0] as u32) << 8 | b[1] as u32;
            [
                wide(v >> 12, 4),
                wide((v >> 8) & 15, 4),
                wide((v >> 4) & 15, 4),
                wide(v & 15, 4),
            ]
        }
        4 => {
            let v = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
            [
                wide(v >> 18, 6),
                wide((v >> 12) & 63, 6),
                wide((v >> 6) & 63, 6),
                wide(v & 63, 6),
            ]
        }
        _ => [
            wide(b[0] as u32, 8),
            wide(b[1] as u32, 8),
            wide(b[2] as u32, 8),
            wide(b[3] as u32, 8),
        ],
    }
}

#[inline(always)]
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let squared = dot(v, v);
    if squared.is_finite() && squared > 1e-30 {
        let scale = 1.0 / libm::sqrtf(squared);
        return v.map(|x| x * scale);
    }
    let largest = v.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    if largest == 0.0 || !largest.is_finite() {
        return [0.0; 3];
    }
    let v = v.map(|x| x / largest);
    let length = libm::sqrtf(dot(v, v));
    v.map(|x| x / length)
}

#[derive(Clone, Copy, Default)]
struct Light {
    color: [f32; 4],
    cos_att: [f32; 3],
    dist_att: [f32; 3],
    pos: [f32; 3],
    dir: [f32; 3],
}

impl Light {
    fn amount(&self, ctrl: u32, pos: [f32; 3], normal: [f32; 3]) -> f32 {
        let to_light = [
            self.pos[0] - pos[0],
            self.pos[1] - pos[1],
            self.pos[2] - pos[2],
        ];
        let mut direction = normalize(to_light);
        let diffuse = (ctrl >> 7) & 3;
        let ratio = |above: f32, below: f32| {
            if below == 0.0 {
                0.0
            } else {
                above.max(0.0) / below
            }
        };
        let attenuation = match (ctrl >> 9) & 3 {
            1 => {
                let cos = if dot(direction, normal) >= 0.0 {
                    dot(self.dir, normal).max(0.0)
                } else {
                    0.0
                };
                let powers = [1.0, cos, cos * cos];
                let dist_att = if diffuse == 0 {
                    self.dist_att
                } else {
                    normalize(self.dist_att)
                };
                ratio(dot(powers, self.cos_att), dot(powers, dist_att))
            }
            3 => {
                let distance = libm::sqrtf(dot(to_light, to_light));
                let cos = dot(direction, self.dir).max(0.0);
                ratio(
                    dot([1.0, cos, cos * cos], self.cos_att),
                    dot([1.0, distance, distance * distance], self.dist_att),
                )
            }
            _ => {
                if direction == [0.0; 3] {
                    direction = normal;
                }
                1.0
            }
        };
        match diffuse {
            0 => attenuation,
            1 => attenuation * dot(direction, normal),
            _ => attenuation * dot(direction, normal).max(0.0),
        }
    }
}

#[derive(Clone, Copy)]
struct Channel {
    material: [f32; 4],
    ambient: [f32; 4],
    ctrl: [u32; 2],
}

#[derive(Clone, Copy)]
struct Material {
    shader: u64,
    textures: [usize; TEXTURE_SLOTS],
    samplers: [SamplerKey; TEXTURE_SLOTS],
    coord_used: [bool; 8],
    coord_bias: [[f32; 2]; 8],
    samples_copy: bool,
}

#[derive(Clone, Copy)]
struct CachedMaterial {
    key: u64,
    material: Material,
}

const DUMMY_MATERIAL: Material = Material {
    shader: 0,
    textures: [WHITE_TEXTURE; TEXTURE_SLOTS],
    samplers: [(0, 0, false); TEXTURE_SLOTS],
    coord_used: [false; 8],
    coord_bias: [[0.0; 2]; 8],
    samples_copy: false,
};

const DUMMY_CACHED_MATERIAL: CachedMaterial = CachedMaterial {
    key: 0,
    material: DUMMY_MATERIAL,
};

#[derive(Clone, Copy)]
enum Source {
    Position,
    Tex(usize),
    Nothing,
}

#[derive(Clone, Copy)]
enum Matrices {
    One([f32; 12], [f32; 9]),
    All([u32; 0x180]),
}

impl Matrices {
    fn rows(&self, matrix: usize) -> ([f32; 12], [f32; 9]) {
        match self {
            Matrices::One(position, normal) => (*position, *normal),
            Matrices::All(all) => (
                core::array::from_fn(|k| {
                    let idx = (matrix * 4 + k) & 0xFF;
                    f32::from_bits(all[idx])
                }),
                core::array::from_fn(|k| {
                    let idx = 0x100 + (matrix & 31) * 3 + k;
                    f32::from_bits(all[idx.min(all.len() - 1)])
                }),
            ),
        }
    }
}

struct VertexJob {
    data: core::ops::Range<usize>,
    count: usize,
    base: u32,
    a: u32,
    b: u32,
    cc: u32,
    hud: u32,
    state: Arc<VState>,
}

#[derive(Clone)]
struct VState {
    vcd_lo: u32,
    vcd_hi: u32,
    arrays: [(u32, u32); 12],
    default_matrix: usize,
    matrices: Matrices,
    gens: [(Source, usize, bool); 8],
    gen_rows: [[f32; 12]; 8],
    coord_used: [bool; 8],
    coord_bias: [[f32; 2]; 8],
    channels: [Channel; 2],
    lit_channels: usize,
    lighting: bool,
    lights: [Light; 8],
    proj: [f32; 6],
    orthographic: bool,
    snap: [f32; 4],
    samples_copy: bool,
    origin: Option<[f32; 4]>,
    raw_ok: bool,
}

fn vparams_block(state: &VState, over_flat: Option<[f32; 4]>) -> [u32; VPARAMS_WORDS] {
    let mut words = [0u32; VPARAMS_WORDS];
    if let Some(origin) = over_flat {
        for k in 0..4 {
            words[VPARAMS_WORDS - 8 + k] = origin[k].to_bits();
        }
        words[VPARAMS_WORDS - 4] = 1.0f32.to_bits();
        words[VPARAMS_WORDS - 1] = 1.0f32.to_bits();
    }
    let mut put = |at: usize, values: &[f32]| {
        for (k, value) in values.iter().enumerate() {
            words[at + k] = value.to_bits();
        }
    };
    put(0, &state.proj[..4]);
    put(
        4,
        &[state.proj[4], state.proj[5], state.lighting as u32 as f32],
    );
    let (position, normal) = state.matrices.rows(state.default_matrix);
    put(8, &position);
    for row in 0..3 {
        put(20 + row * 4, &normal[row * 3..row * 3 + 3]);
    }
    for n in 0..8 {
        let at = 32 + n * 16;
        put(at, &state.gen_rows[n]);
        let source = match state.gens[n].0 {
            _ if !state.coord_used[n] => -1.0,
            Source::Position => 0.0,
            Source::Tex(k) => 1.0 + k as f32,
            Source::Nothing => 5.0,
        };
        put(
            at + 12,
            &[
                source,
                state.gens[n].2 as u32 as f32,
                state.coord_bias[n][0],
                state.coord_bias[n][1],
            ],
        );
    }
    for n in 0..2 {
        let at = 160 + n * 12;
        put(at, &state.channels[n].material);
        put(at + 4, &state.channels[n].ambient);
    }
    for n in 0..8 {
        let (at, light) = (184 + n * 20, &state.lights[n]);
        put(at, &light.color);
        put(at + 4, &light.cos_att);
        put(at + 8, &light.dist_att);
        put(at + 12, &light.pos);
        put(at + 16, &light.dir);
    }
    for n in 0..2 {
        let at = 160 + n * 12 + 8;
        let has_color = (state.vcd_lo >> (13 + 2 * n)) & 3 != 0;
        words[at] = state.channels[n].ctrl[0];
        words[at + 1] = state.channels[n].ctrl[1];
        words[at + 2] = has_color as u32 | ((n < state.lit_channels) as u32) << 1;
    }
    words
}

struct Template {
    serial: u64,
    material: Material,
    vstate: VState,
    vstate_arc: Option<Arc<VState>>,
    state: DrawState,
    viewport: [f32; 4],
    scissor: [u32; 4],
    params: u32,
    vparams: Option<u32>,
    vparams_flat: Option<u32>,
}

impl Template {
    fn vparams(&mut self, frame: &mut Frame, flat_started: bool) -> u32 {
        match self.vstate.origin.filter(|_| flat_started) {
            Some(origin) => *self.vparams_flat.get_or_insert_with(|| {
                let offset = frame.push_vparams(&vparams_block(&self.vstate, Some(origin)));
                frame.raw_flat.push((offset, origin));
                offset
            }),
            None => *self
                .vparams
                .get_or_insert_with(|| frame.push_vparams(&vparams_block(&self.vstate, None))),
        }
    }
}

#[derive(Clone, Copy)]
struct ListShape {
    plain: bool,
    spans: [(u32, u32); 12],
    index_count: u32,
    draws: u32,
    vertices: u32,
    vats_used: u8,
}

enum Ahead {
    Cached(ListShape, (i32, u32)),
    Owned(ListShape, Vec<RawVertex>, Vec<u32>),
    Late,
}

#[derive(Clone, Copy)]
struct ListCheck {
    blocks: u64,
    exact: u64,
    place: (i32, u32),
}

#[derive(Clone, Copy, Default)]
struct Churn {
    builds: u32,
    vertices: u64,
    built: u64,
    run: u8,
    out_until: u64,
    outs: u8,
}

impl VertexJob {
    fn run(
        &self,
        mem: Mem,
        data: &[u8],
        out: &mut [core::mem::MaybeUninit<Vertex>],
    ) -> (f32, f32) {
        let VertexJob { a, b, cc, .. } = *self;
        let VState {
            vcd_lo,
            vcd_hi,
            ref gens,
            ref gen_rows,
            coord_used,
            coord_bias,
            ref channels,
            lit_channels,
            lighting,
            ref lights,
            proj,
            orthographic,
            ..
        } = *self.state;
        let data = &data[self.data.clone()];
        let [snap_w, snap_h, snap_cx, snap_cy] = self.state.snap;
        let kind = |shift: u32| (vcd_lo >> shift) & 3;
        let array = |n: usize| self.state.arrays[n];
        let indexed = |k: u32| match k {
            2 => 1,
            3 => 2,
            _ => 0,
        };

        let (pos_count, pos_format) = (2 + (a & 1) as usize, (a >> 1) & 7);
        let pos_scale = 1.0 / (1u64 << ((a >> 4) & 31)) as f32;
        let pos_size = pos_count * COMP[pos_format as usize];
        let nbt = (a >> 9) & 1 != 0;
        let normal_size = match kind(11) {
            1 => (if nbt { 9 } else { 3 }) * COMP[((a >> 10) & 7) as usize],
            k => indexed(k) * (if nbt && a >> 31 != 0 { 3 } else { 1 }),
        };
        let color_format = [(a >> 14) & 7, (a >> 18) & 7];
        let tex = [
            (a >> 21, a >> 22),
            (b, b >> 1),
            (b >> 9, b >> 10),
            (b >> 18, b >> 19),
            (b >> 27, b >> 28),
            (cc >> 5, cc >> 6),
            (cc >> 14, cc >> 15),
            (cc >> 23, cc >> 24),
        ];
        let tex_kind = |i: usize| (vcd_hi >> (i * 2)) & 3;
        let tex_direct = |i: usize| (1 + (tex[i].0 & 1) as usize) * COMP[(tex[i].1 & 7) as usize];
        let tex_shift =
            [a >> 25, b >> 4, b >> 13, b >> 22, cc, cc >> 9, cc >> 18, cc >> 27].map(|v| v & 31);
        let normal_format = (a >> 10) & 7;

        let mut loaded = usize::MAX;
        let (mut position_rows, mut normal_rows) = ([0.0f32; 12], [0.0f32; 9]);

        let (mut tex_present, mut tex_count) = ([0usize; 8], 0);
        for i in (0..8).filter(|&i| tex_kind(i) != 0) {
            tex_present[tex_count] = i;
            tex_count += 1;
        }
        let tex_scale = tex_shift.map(|shift| 1.0 / (1u64 << shift) as f32);
        let (mut gens_used, mut gens_count) = ([0usize; 8], 0);
        for n in (0..8).filter(|&n| coord_used[n]) {
            gens_used[gens_count] = n;
            gens_count += 1;
        }

        #[derive(Clone, Copy)]
        struct Part {
            ctrl: u32,
            material_from_vertex: bool,
            ambient_from_vertex: bool,
            lit: bool,
            lights: u32,
        }
        let color_kinds = [kind(13), kind(15)];
        let parts: [[Part; 2]; 2] = core::array::from_fn(|n| {
            core::array::from_fn(|part| {
                let ctrl = channels[n].ctrl[part];
                Part {
                    ctrl,
                    material_from_vertex: color_kinds[n] != 0 && ctrl & 1 != 0,
                    ambient_from_vertex: color_kinds[n] != 0 && ctrl & 0x40 != 0,
                    lit: ctrl & 2 != 0 && n < lit_channels,
                    lights: ((ctrl >> 2) & 0xF) | ((ctrl >> 11) & 0xF) << 4,
                }
            })
        });
        let has_normal = lighting && kind(11) != 0;
        let normal_bytes = 3 * COMP[normal_format as usize];
        let matrix_indices = ((vcd_lo >> 1) & 0xFF).count_ones() as usize;

        let constant_colors: [Option<[f32; 4]>; 2] = core::array::from_fn(|n| {
            if color_kinds[n] == 0 && !parts[n][0].lit && !parts[n][1].lit {
                let m = channels[n].material;
                Some([m[0], m[1], m[2], m[3]])
            } else {
                None
            }
        });

        let mut buf = [0u8; 16];
        let mut o = 0usize;
        let mut extent = (f32::MAX, f32::MIN);
        for slot in out.iter_mut() {
            let mut matrix = self.state.default_matrix;
            if vcd_lo & 1 != 0 {
                matrix = (data[o] & 0x3F) as usize;
                o += 1;
            }
            o += matrix_indices;

            fetch(mem, data, &mut o, kind(9), pos_size, array(0), &mut buf);
            let comp = |i: usize| match pos_format {
                0 => buf[i] as f32 * pos_scale,
                1 => buf[i] as i8 as f32 * pos_scale,
                2 => u16::from_be_bytes([buf[i * 2], buf[i * 2 + 1]]) as f32 * pos_scale,
                3 => i16::from_be_bytes([buf[i * 2], buf[i * 2 + 1]]) as f32 * pos_scale,
                _ => f32::from_be_bytes(buf[i * 4..i * 4 + 4].try_into().unwrap()),
            };
            let p = [
                comp(0),
                comp(1),
                if pos_count == 3 { comp(2) } else { 0.0 },
            ];

            let mut normal = [0.0f32; 3];
            if has_normal {
                let mut at = o;
                fetch(mem, data, &mut at, kind(11), normal_bytes, array(1), &mut buf);
                normal = core::array::from_fn(|i| match normal_format {
                    0 | 1 => buf[i] as i8 as f32 / 64.0,
                    2 | 3 => i16::from_be_bytes([buf[i * 2], buf[i * 2 + 1]]) as f32 / 16384.0,
                    _ => f32::from_be_bytes(buf[i * 4..i * 4 + 4].try_into().unwrap()),
                });
            }
            o += normal_size;

            let mut vertex_colors = [[0.0f32; 4]; 2];
            for n in 0..2 {
                let k = color_kinds[n];
                if k == 0 {
                    continue;
                }
                let fmt = color_format[n];
                fetch(mem, data, &mut o, k, COLOR[fmt as usize], array(2 + n), &mut buf);
                vertex_colors[n] = decode_color(fmt, &buf);
            }

            let mut tex_in = [[0.0f32; 2]; 8];
            for &i in &tex_present[..tex_count] {
                fetch(
                    mem,
                    data,
                    &mut o,
                    tex_kind(i),
                    tex_direct(i),
                    array(4 + i),
                    &mut buf,
                );
                let scale = tex_scale[i];
                for (n, dst) in tex_in[i]
                    .iter_mut()
                    .enumerate()
                    .take(1 + (tex[i].0 & 1) as usize)
                {
                    *dst = match tex[i].1 & 7 {
                        0 => buf[n] as f32 * scale,
                        1 => buf[n] as i8 as f32 * scale,
                        2 => u16::from_be_bytes([buf[n * 2], buf[n * 2 + 1]]) as f32 * scale,
                        3 => i16::from_be_bytes([buf[n * 2], buf[n * 2 + 1]]) as f32 * scale,
                        _ => f32::from_be_bytes(buf[n * 4..n * 4 + 4].try_into().unwrap()),
                    };
                }
            }
            let mut uvs = [[0.0f32; 4]; 4];
            for &n in &gens_used[..gens_count] {
                let (source, _, projective) = gens[n];
                let inp = match source {
                    Source::Tex(k) => [tex_in[k][0], tex_in[k][1], 1.0, 1.0],
                    Source::Position => [p[0], p[1], p[2], 1.0],
                    Source::Nothing => [0.0; 4],
                };
                let rows = &gen_rows[n];
                let row = |r: usize| {
                    let m = &rows[r * 4..r * 4 + 4];
                    m[0] * inp[0] + m[1] * inp[1] + m[2] * inp[2] + m[3] * inp[3]
                };
                let (mut s, mut t) = (row(0), row(1));
                if projective {
                    let q = row(2);
                    if q != 0.0 {
                        s /= q;
                        t /= q;
                    }
                }
                uvs[n / 2][n % 2 * 2] = s - coord_bias[n][0];
                uvs[n / 2][n % 2 * 2 + 1] = t - coord_bias[n][1];
            }

            if matrix != loaded {
                loaded = matrix;
                (position_rows, normal_rows) = self.state.matrices.rows(matrix);
            }
            let row = |r: usize| {
                let m = &position_rows[r * 4..r * 4 + 4];
                m[0] * p[0] + m[1] * p[1] + m[2] * p[2] + m[3]
            };
            let (x, y, z) = (row(0), row(1), row(2));

            let mut colors = [[0.0f32; 4]; 2];
            if lighting {
                normal = normalize(
                    [0, 1, 2].map(|r| dot([0, 1, 2].map(|k| normal_rows[r * 3 + k]), normal)),
                );
            }
            for (n, channel) in channels.iter().enumerate() {
                if let Some(c) = constant_colors[n] {
                    colors[n] = c;
                    continue;
                }
                let vertex = vertex_colors[n];
                let shade = |part: &Part| {
                    let material = if part.material_from_vertex {
                        vertex
                    } else {
                        channel.material
                    };
                    if !part.lit {
                        return material;
                    }
                    let mut sum = if part.ambient_from_vertex {
                        vertex
                    } else {
                        channel.ambient
                    };
                    let mut remaining = part.lights;
                    while remaining != 0 {
                        let light = &lights[remaining.trailing_zeros() as usize];
                        remaining &= remaining - 1;
                        let amount = light.amount(part.ctrl, [x, y, z], normal);
                        sum = core::array::from_fn(|i| sum[i] + light.color[i] * amount);
                    }
                    core::array::from_fn(|i| material[i] * sum[i].clamp(0.0, 1.0))
                };
                let color = shade(&parts[n][0]);
                let alpha = if parts[n][1].ctrl == parts[n][0].ctrl {
                    color
                } else {
                    shade(&parts[n][1])
                };
                colors[n] = [color[0], color[1], color[2], alpha[3]];
            }

            let pos = if orthographic {
                let (mut clip_x, mut clip_y) = (proj[0] * x + proj[1], proj[2] * y + proj[3]);
                if snap_w > 0.0 && snap_h > 0.0 {
                    let snap = |pixel: f32| libm::floorf(pixel + 5.0 / 12.0);
                    clip_x = (snap(snap_cx + clip_x * snap_w) - snap_cx) / snap_w;
                    clip_y = -(snap(snap_cy - clip_y * snap_h) - snap_cy) / snap_h;
                }
                [clip_x, clip_y, proj[4] * z + proj[5] + 1.0, 1.0]
            } else {
                [
                    proj[0] * x + proj[1] * z,
                    proj[2] * y + proj[3] * z,
                    proj[4] * z + proj[5] - z,
                    -z,
                ]
            };
            if orthographic {
                extent = (extent.0.min(pos[0]), extent.1.max(pos[0]));
            }
            slot.write(Vertex { pos, colors, uvs });
        }
        extent
    }
}

#[derive(Default, Clone, Copy)]
pub struct FrameStats {
    pub draws: u32,
    pub vertices: u32,
    pub display_lists: u32,
    pub bp_loads: u32,
    pub cp_loads: u32,
    pub xf_loads: u32,
}

pub enum Raised {
    Token(u16),
    Break,
    Finish,
}

const BP_PE_DONE: usize = 0x45;
const BP_PE_TOKEN: usize = 0x47;
const BP_PE_TOKEN_INT: usize = 0x48;
const BP_EFB_COPY: usize = 0x52;

pub struct Gx {
    pending: Vec<u8>,
    consumed: u64,
    limit: Option<u64>,
    break_signalled: bool,
    marks: Vec<(u32, u64)>,
    pub raised: Vec<Raised>,
    pub bp: [u32; 256],
    pub cp: [u32; 256],
    pub xf: Vec<u32>,
    pub stats: FrameStats,
    pub last_stats: FrameStats,
    pub frames: u64,
    pub token: u16,
    tmem: Vec<u8>,
    tev_reg: [[f32; 4]; 4],
    tev_konst: [[f32; 4]; 4],
    frame: Frame,
    pub renderer: Option<Renderer>,
    clear: Option<[f64; 4]>,
    pub copied_to_display: bool,
    pub display_copies: Vec<u32>,
    finish_requests: u32,
    texture_cache: HashMap<(u32, u32, u32), Option<usize>>,
    smooth_textures: HashSet<usize>,
    draw_index: u32,
    flat_started: bool,
    jobs: Vec<VertexJob>,
    job_data: Vec<u8>,
    job_vertices: usize,
    mem: Mem,
    material: Option<Material>,
    material_cache: [CachedMaterial; 16],
    material_cache_len: usize,
    has_paletted_textures: bool,
    template: Option<Template>,
    template_serial: u64,
    last_serial: u64,
    pending_ortho: bool,
    shapes: HashMap<(u64, u64), ListShape>,
    checks: HashMap<(u64, u64, u64), ListCheck>,
    places: HashMap<(u64, u64, u64), (i32, u32)>,
    churn: HashMap<(u64, u64), Churn>,
    contents: HashMap<(u64, u64, u32), (i32, u32)>,
    seen: HashMap<(u64, u64), u64>,
    cache_full: bool,
    cp_sig: Option<u64>,
    block_hash: Vec<u64>,
    block_stamp: Vec<u32>,
    epoch: u32,
    ahead_pos: u64,
    ahead_cp: [u32; 256],
    ahead_sig: Option<u64>,
    ahead: VecDeque<(u64, Ahead)>,
    ahead_bad: bool,
    ahead_data: Vec<u8>,
    spare: Vec<(Vec<RawVertex>, Vec<u32>)>,
    list_vertices: Vec<RawVertex>,
    list_indices: Vec<u32>,
    hud: u32,
    hud_marks: VecDeque<(u64, u32)>,
    hud_marked: u32,
    hud_widgets: u32,
}

struct WorkerSlice {
    jobs: *const VertexJob,
    job_count: usize,
    mem: Mem,
    data: *const u8,
    data_len: usize,
    out_ptr: *mut core::mem::MaybeUninit<Vertex>,
    extents_ptr: *mut (f32, f32),
}

extern "C" fn run_worker_jobs(arg: *mut core::ffi::c_void) {
    let task = unsafe { &*(arg as *const WorkerSlice) };
    let jobs = unsafe { core::slice::from_raw_parts(task.jobs, task.job_count) };
    let data = unsafe { core::slice::from_raw_parts(task.data, task.data_len) };
    let mut out_curr = task.out_ptr;
    let mut ext_curr = task.extents_ptr;
    for job in jobs {
        let out_slice = unsafe { core::slice::from_raw_parts_mut(out_curr, job.count) };
        let ext = job.run(task.mem, data, out_slice);
        unsafe {
            *ext_curr = ext;
            out_curr = out_curr.add(job.count);
            ext_curr = ext_curr.add(1);
        }
    }
}

impl Gx {
    pub fn new() -> Gx {
        Gx {
            pending: Vec::with_capacity(256 * 1024),
            consumed: 0,
            limit: None,
            break_signalled: false,
            marks: Vec::with_capacity(64),
            raised: Vec::with_capacity(16),
            bp: core::array::from_fn(|reg| if reg == 0xFE { 0xFF_FFFF } else { 0 }),
            cp: [0; 256],
            xf: vec![0; 0x1100],
            stats: FrameStats::default(),
            last_stats: FrameStats::default(),
            frames: 0,
            token: 0,
            tmem: vec![0; 0x10_0000],
            tev_reg: [[0.0; 4]; 4],
            tev_konst: [[0.0; 4]; 4],
            frame: Frame::default(),
            renderer: None,
            clear: Some([0.0, 0.0, 0.0, 1.0]),
            copied_to_display: false,
            display_copies: Vec::new(),
            finish_requests: 0,
            texture_cache: HashMap::with_capacity(256),
            smooth_textures: HashSet::new(),
            draw_index: 0,
            flat_started: false,
            jobs: Vec::with_capacity(256),
            job_data: Vec::with_capacity(64 * 1024),
            job_vertices: 0,
            mem: Mem(core::ptr::null()),
            material: None,
            material_cache: [DUMMY_CACHED_MATERIAL; 16],
            material_cache_len: 0,
            has_paletted_textures: false,
            template: None,
            template_serial: 0,
            last_serial: 0,
            pending_ortho: false,
            shapes: HashMap::with_capacity(2048),
            checks: HashMap::with_capacity(2048),
            places: HashMap::with_capacity(2048),
            churn: HashMap::with_capacity(256),
            contents: HashMap::with_capacity(2048),
            seen: HashMap::with_capacity(2048),
            cache_full: false,
            cp_sig: None,
            block_hash: vec![0; (runtime::mem::RAM_SIZE >> BLOCK_SHIFT) as usize],
            block_stamp: vec![0; (runtime::mem::RAM_SIZE >> BLOCK_SHIFT) as usize],
            epoch: 1,
            ahead_pos: 0,
            ahead_cp: [0; 256],
            ahead_sig: None,
            ahead: VecDeque::with_capacity(512),
            ahead_bad: false,
            ahead_data: Vec::with_capacity(64 * 1024),
            spare: Vec::with_capacity(64),
            list_vertices: Vec::with_capacity(4096),
            list_indices: Vec::with_capacity(8192),
            hud: 0,
            hud_marks: VecDeque::with_capacity(64),
            hud_marked: 0,
            hud_widgets: 0,
        }
    }

    #[inline(always)]
    fn xf_float(&self, index: usize) -> f32 {
        f32::from_bits(self.xf.get(index).copied().unwrap_or(0))
    }

    #[inline(always)]
    fn sampling(&self, id: usize, sampler: SamplerKey) -> SamplerKey {
        if self.smooth_textures.contains(&id) {
            (sampler.0, sampler.1, true)
        } else {
            sampler
        }
    }

    fn map_texture(&mut self, c: &mut Ctx, map: usize) -> Option<(usize, SamplerKey, [f32; 2])> {
        let reg = |low: usize, high: usize| {
            self.bp[if map < 4 { low + map } else { high + map - 4 }]
        };
        let (mode, image, address, tlut) =
            (reg(0x80, 0xA0), reg(0x88, 0xA8), reg(0x94, 0xB4), reg(0x98, 0xB8));
        let (w, h, format) = (
            (image & 0x3FF) + 1,
            ((image >> 10) & 0x3FF) + 1,
            (image >> 20) & 0xF,
        );
        let address = ((address & 0xFF_FFFF) << 5) | 0x8000_0000;
        let sampler = ((mode & 3) as u8, ((mode >> 2) & 3) as u8, mode & 0x10 != 0);

        if let Some(id) = self.renderer.as_ref().and_then(|r| r.efb_copy(address)) {
            let bias = if sampler.2 {
                [0.0; 2]
            } else {
                [0.5 / w as f32, 0.5 / h as f32]
            };
            return Some((id, sampler, bias));
        }

        let cache_key = (address, image, tlut);
        if let Some(&cached) = self.texture_cache.get(&cache_key) {
            return cached.map(|id| (id, self.sampling(id, sampler), [0.0; 2]));
        }
        self.texture_cache.insert(cache_key, None);

        let size = texture::data_size(format, w as usize, h as usize)?;
        if !in_ram(address, size) {
            return None;
        }
        // Zero-copy read directly from masked guest RAM!
        let data = unsafe { Mem(c.mem).slice(address, size) };

        let (palette_base, palette_format) = (((tlut & 0x3FF) << 9) as usize, (tlut >> 10) & 3);
        let palette_end = (palette_base + texture::palette_entries(format) * 2).min(self.tmem.len());
        let palette = &self.tmem[palette_base..palette_end];
        let contents = texture::hash(data) ^ texture::hash(palette).rotate_left(1);
        let key = (address, w, h, format | palette_format << 8, contents);

        let renderer = self.renderer.as_mut()?;
        let id = match renderer.find_texture(&key) {
            Some(id) => id,
            None => {
                let lookup = |i: usize| {
                    let entry = palette.get(i * 2..i * 2 + 2);
                    entry.map_or([0; 4], |e| {
                        texture::palette_color(palette_format, u16::from_be_bytes([e[0], e[1]]))
                    })
                };
                let rgba = texture::decode(format, w as usize, h as usize, data, &lookup);
                renderer.add_texture(key, w, h, &rgba)
            }
        };
        self.texture_cache.insert(cache_key, Some(id));
        if texture::palette_entries(format) != 0 {
            self.has_paletted_textures = true;
        }
        Some((id, self.sampling(id, sampler), [0.0; 2]))
    }

    fn tev_params(&self) -> [f32; PARAMS_FLOATS] {
        let mut block = [0.0f32; PARAMS_FLOATS];
        for r in 0..4 {
            block[r * 4..r * 4 + 4].copy_from_slice(&self.tev_reg[r]);
            block[16 + r * 4..20 + r * 4].copy_from_slice(&self.tev_konst[r]);
        }
        block[32] = (self.bp[0xF3] & 0xFF) as f32;
        block[33] = ((self.bp[0xF3] >> 8) & 0xFF) as f32;
        let packed = |v: u32| {
            f32::from_bits(((v >> 19) & 1) << 31 | ((v >> 11) & 0xFF) << 23 | (v & 0x7FF) << 12)
        };
        let color = self.bp[0xF2];
        block[36..39]
            .copy_from_slice(&[16, 8, 0].map(|shift| ((color >> shift) & 0xFF) as f32 / 255.0));
        block[40] = packed(self.bp[0xEE]);
        block[41] = (self.bp[0xEF] & 0xFF_FFFF) as f32;
        block[42] = (1u32 << (self.bp[0xF0] & 31)) as f32;
        block[43] = packed(self.bp[0xF1]);
        block
    }

    fn finish_vertices(&mut self) {
        let t0 = unsafe { crate::switch_now_ns() };
        self.finish_vertices_inner();
        let dt = unsafe { crate::switch_now_ns().saturating_sub(t0) };
        crate::PERF_FINISH_VERTS_NS.fetch_add(dt, core::sync::atomic::Ordering::Relaxed);
    }

    fn finish_vertices_inner(&mut self) {
        self.pending_ortho = false;
        if self.jobs.is_empty() {
            return;
        }
        let jobs = core::mem::take(&mut self.jobs);
        let total = core::mem::take(&mut self.job_vertices);
        let (mem, data) = (self.mem, &self.job_data);
        let mut vertices = core::mem::take(&mut self.frame.vertices);
        vertices.reserve(total);
        let spare = &mut vertices.spare_capacity_mut()[..total];
        let mut extents = Vec::with_capacity(jobs.len());
        unsafe { extents.set_len(jobs.len()) };

        if jobs.len() >= 2 && total >= 128 {
            let target_mid = total / 2;
            let mut accum = 0;
            let mut mid = 1;
            for (idx, j) in jobs.iter().enumerate() {
                accum += j.count;
                if accum >= target_mid || idx + 1 == jobs.len() {
                    mid = (idx + 1).min(jobs.len() - 1).max(1);
                    break;
                }
            }
            let worker_verts: usize = jobs[..mid].iter().map(|j| j.count).sum();

            let worker_slice = WorkerSlice {
                jobs: jobs.as_ptr(),
                job_count: mid,
                mem,
                data: data.as_ptr(),
                data_len: data.len(),
                out_ptr: spare.as_mut_ptr(),
                extents_ptr: extents.as_mut_ptr(),
            };

            unsafe {
                crate::switch_dispatch_worker(run_worker_jobs, &worker_slice as *const _ as *mut _);
            }

            let mut out_curr = unsafe { spare.as_mut_ptr().add(worker_verts) };
            let mut ext_curr = unsafe { extents.as_mut_ptr().add(mid) };
            for job in &jobs[mid..] {
                let out_slice = unsafe { core::slice::from_raw_parts_mut(out_curr, job.count) };
                let ext = job.run(mem, data, out_slice);
                unsafe {
                    *ext_curr = ext;
                    out_curr = out_curr.add(job.count);
                    ext_curr = ext_curr.add(1);
                }
            }

            unsafe {
                crate::switch_wait_worker();
            }
        } else {
            let mut spare_slice = spare;
            for (idx, job) in jobs.iter().enumerate() {
                let (out, rest) = core::mem::take(&mut spare_slice).split_at_mut(job.count);
                spare_slice = rest;
                extents[idx] = job.run(mem, data, out);
            }
        }
        unsafe { vertices.set_len(vertices.len() + total) };
        self.frame.vertices = vertices;

        for (job, (left, right)) in jobs.iter().zip(extents) {
            let range = (job.base, job.base + job.count as u32);
            let state = &job.state;
            if state.orthographic && !state.samples_copy && (left > -0.98 || right < 0.98) {
                self.frame.flat.push(Flat {
                    from: range.0,
                    to: range.1,
                    origin: None,
                    hud: job.hud,
                    extent: (left, right),
                });
                self.flat_started = true;
            }
            let over_flat = job.hud != 0;
            if let (false, false, true, Some(origin)) = (
                state.orthographic,
                state.samples_copy,
                over_flat,
                state.origin,
            ) {
                let at = origin[0] / origin[3];
                self.frame.flat.push(Flat {
                    from: range.0,
                    to: range.1,
                    origin: Some(origin),
                    hud: job.hud,
                    extent: (at, at),
                });
            }
        }
        self.jobs = jobs;
        self.jobs.clear();
        self.job_data.clear();
    }

    #[inline(always)]
    fn forget_material(&mut self) {
        self.material = None;
        self.template = None;
    }

    fn build_template(&mut self, c: &mut Ctx) {
        let material = match self.material {
            Some(material) => material,
            None => {
                let config = tev_gl::Config::from_registers(&self.bp);
                let shader = config.key();
                let mut mat_key = shader;
                for (map, _) in config.texture_uses() {
                    let reg = |low: usize, high: usize| {
                        self.bp[if map < 4 { low + map } else { high + map - 4 }]
                    };
                    let (mode, image, address, tlut) =
                        (reg(0x80, 0xA0), reg(0x88, 0xA8), reg(0x94, 0xB4), reg(0x98, 0xB8));
                    let stage_sig = (mode as u64)
                        ^ ((image as u64) << 16)
                        ^ ((address as u64) << 32)
                        ^ ((tlut as u64) << 48);
                    mat_key = mat_key.rotate_left(7) ^ stage_sig;
                }

                let mut cached_mat = None;
                for i in 0..self.material_cache_len {
                    if self.material_cache[i].key == mat_key {
                        let m = self.material_cache[i].material;
                        if i > 0 {
                            let hit = self.material_cache[i];
                            self.material_cache.copy_within(0..i, 1);
                            self.material_cache[0] = hit;
                        }
                        cached_mat = Some(m);
                        break;
                    }
                }

                let material = match cached_mat {
                    Some(m) => m,
                    None => {
                        let mut textures = [WHITE_TEXTURE; TEXTURE_SLOTS];
                        let mut samplers: [SamplerKey; TEXTURE_SLOTS] = [(0, 0, false); TEXTURE_SLOTS];
                        let mut coord_used = [false; 8];
                        let mut coord_bias = [[0.0f32; 2]; 8];
                        for (map, coord) in config.texture_uses() {
                            coord_used[coord] = true;
                            if let Some((id, sampler, bias)) = self.map_texture(c, map) {
                                textures[map] = id;
                                samplers[map] = sampler;
                                coord_bias[coord] = bias;
                            }
                        }
                        let samples_copy = self
                            .renderer
                            .as_ref()
                            .is_some_and(|r| textures.iter().any(|&id| r.is_efb_copy(id)));
                        if let Some(renderer) = &mut self.renderer {
                            renderer.ensure_shader(shader, || config.fragment_glsl());
                        }
                        let m = Material {
                            shader,
                            textures,
                            samplers,
                            coord_used,
                            coord_bias,
                            samples_copy,
                        };
                        if self.material_cache_len < self.material_cache.len() {
                            self.material_cache.copy_within(0..self.material_cache_len, 1);
                            self.material_cache_len += 1;
                        } else {
                            let len = self.material_cache.len();
                            self.material_cache.copy_within(0..len - 1, 1);
                        }
                        self.material_cache[0] = CachedMaterial { key: mat_key, material: m };
                        m
                    }
                };
                self.material = Some(material);
                material
            }
        };
        let Material {
            coord_used,
            coord_bias,
            samples_copy,
            ..
        } = material;

        let (vcd_lo, vcd_hi) = (self.cp[0x50], self.cp[0x60]);
        let arrays: [(u32, u32); 12] = core::array::from_fn(|n| {
            (
                (self.cp[0xA0 + n] & 0x03FF_FFFF) | 0x8000_0000,
                self.cp[0xB0 + n] & 0xFF,
            )
        });

        let gens: [(Source, usize, bool); 8] = core::array::from_fn(|n| {
            let reg = self.xf[0x1040 + n];
            let source = match (reg >> 7) & 0x1F {
                0 => Source::Position,
                row @ 5..=12 => Source::Tex(row as usize - 5),
                _ => Source::Nothing,
            };
            let matrix = if n < 4 {
                self.cp[0x30] >> (6 + 6 * n)
            } else {
                self.cp[0x40] >> (6 * (n - 4))
            };
            (source, (matrix & 0x3F) as usize, reg & 2 != 0)
        });

        let channels: [Channel; 2] = core::array::from_fn(|n| {
            let color = |reg: u32| [24, 16, 8, 0].map(|s| ((reg >> s) & 0xFF) as f32 / 255.0);
            Channel {
                material: color(self.xf[0x100C + n]),
                ambient: color(self.xf[0x100A + n]),
                ctrl: [self.xf[0x100E + n], self.xf[0x1010 + n]],
            }
        });
        let lit_channels = (self.xf[0x1009] & 3).min(2) as usize;
        let lit = |ctrl: u32| ctrl & 2 != 0 && ctrl & 0x783C != 0;
        let lighting = channels[..lit_channels]
            .iter()
            .any(|ch| lit(ch.ctrl[0]) || lit(ch.ctrl[1]));
        let lights: [Light; 8] = core::array::from_fn(|n| {
            if !lighting {
                return Light::default();
            }
            let base = 0x600 + n * 16;
            let vector = |at: usize| [0, 1, 2].map(|k| self.xf_float(base + at + k));
            Light {
                color: [24, 16, 8, 0].map(|s| ((self.xf[base + 3] >> s) & 0xFF) as f32 / 255.0),
                cos_att: vector(4),
                dist_att: vector(7),
                pos: vector(10),
                dir: vector(13),
            }
        });
        let proj: [f32; 6] = core::array::from_fn(|i| self.xf_float(0x1020 + i));
        let orthographic = self.xf[0x1026] & 1 != 0;

        let (snap_w, snap_h) = (self.xf_float(0x101A), -self.xf_float(0x101B));
        let (snap_cx, snap_cy) = (self.xf_float(0x101D) - 342.0, self.xf_float(0x101E) - 342.0);

        let gen_rows: [[f32; 12]; 8] = core::array::from_fn(|n| {
            core::array::from_fn(|k| {
                if coord_used[n] {
                    self.xf_float((gens[n].1 * 4 + k) & 0xFF)
                } else {
                    0.0
                }
            })
        });
        let default_matrix = (self.cp[0x30] & 0x3F) as usize;
        let matrices = if vcd_lo & 1 != 0 {
            let mut all = [0u32; 0x180];
            all[..0x100].copy_from_slice(&self.xf[..0x100]);
            all[0x100..].copy_from_slice(&self.xf[0x400..0x480]);
            Matrices::All(all)
        } else {
            Matrices::One(
                core::array::from_fn(|k| self.xf_float((default_matrix * 4 + k) & 0xFF)),
                core::array::from_fn(|k| self.xf_float(0x400 + (default_matrix & 31) * 3 + k)),
            )
        };
        let origin = [3, 7, 11].map(|k| self.xf_float((default_matrix * 4 + k) & 0xFF));
        let origin = (!orthographic && origin[2] < 0.0).then(|| {
            [
                proj[0] * origin[0] + proj[1] * origin[2],
                proj[2] * origin[1] + proj[3] * origin[2],
                proj[4] * origin[2] + proj[5],
                -origin[2],
            ]
        });
        let raw_ok = !orthographic
            && vcd_lo & 0x1FF == 0
            && vcd_hi >> 8 == 0
            && (0..8).all(|n| !coord_used[n] || !matches!(gens[n].0, Source::Tex(k) if k >= 4));
        let vstate = VState {
            vcd_lo,
            vcd_hi,
            arrays,
            default_matrix,
            matrices,
            gens,
            gen_rows,
            coord_used,
            coord_bias,
            channels,
            lit_channels,
            lighting,
            lights,
            proj,
            orthographic,
            snap: [snap_w, snap_h, snap_cx, snap_cy],
            samples_copy,
            origin,
            raw_ok,
        };

        let (zmode, cmode) = (self.bp[0x40], self.bp[0x41]);
        let state = DrawState {
            depth_test: zmode & 1 != 0,
            depth_func: ((zmode >> 1) & 7) as u8,
            depth_write: zmode & 0x10 != 0,
            blend: cmode & 1 != 0 || cmode & 0x800 != 0,
            subtract: cmode & 0x800 != 0,
            color_write: cmode & 8 != 0,
            alpha_write: cmode & 0x10 != 0,
            dst_factor: ((cmode >> 5) & 7) as u8,
            src_factor: ((cmode >> 8) & 7) as u8,
            cull: ((self.bp[0x00] >> 14) & 3) as u8,
        };
        let (half_w, half_h) = (self.xf_float(0x101A), -self.xf_float(0x101B));
        let viewport = if half_w > 0.0 && half_h > 0.0 {
            let (cx, cy) = (self.xf_float(0x101D) - 342.0, self.xf_float(0x101E) - 342.0);
            [cx - half_w, cy - half_h, half_w * 2.0, half_h * 2.0]
        } else {
            [0.0, 0.0, 640.0, 480.0]
        };
        let (tl, br) = (self.bp[0x20], self.bp[0x21]);
        let edge = |v: u32, extra: u32| ((v & 0x7FF) + extra).saturating_sub(342);
        let scissor = [edge(tl >> 12, 0), edge(tl, 0), edge(br >> 12, 1), edge(br, 1)];
        let params = self.frame.push_params(&self.tev_params());
        self.template_serial += 1;
        self.template = Some(Template {
            serial: self.template_serial,
            material,
            vstate,
            vstate_arc: None,
            state,
            viewport,
            scissor,
            params,
            vparams: None,
            vparams_flat: None,
        });
    }

    fn draw(&mut self, op: u8, count: usize, data: &[u8], c: &mut Ctx) {
        self.draw_index += 1;
        if self.template.is_none() {
            self.build_template(c);
        }
        let mut template = self.template.take().expect("template built");
        self.draw_with(&mut template, op, count, data);
        self.template = Some(template);
    }

    fn draw_with(&mut self, template: &mut Template, op: u8, count: usize, data: &[u8]) {
        let vat = (op & 7) as usize;
        let (a, b, cc) = (self.cp[0x70 + vat], self.cp[0x80 + vat], self.cp[0x90 + vat]);
        let vstate = &template.vstate;

        let raw = vstate.raw_ok;
        if raw && self.pending_ortho {
            self.finish_vertices();
        }
        let base = if raw {
            if !matches!(op & 0xF8, 0x80 | 0x88 | 0x90 | 0x98 | 0xA0) || template.state.cull == 3
            {
                return;
            }
            let base = self.frame.raw.len() as u32;
            let format = Format {
                vcd_lo: vstate.vcd_lo,
                vcd_hi: vstate.vcd_hi,
                a,
                b,
                arrays: vstate.arrays,
            };
            decode_raw(
                self.mem,
                data,
                count,
                &format,
                &mut self.frame.raw,
                &mut [(u32::MAX, 0); 12],
            );
            base
        } else {
            let base = (self.frame.vertices.len() + self.job_vertices) as u32;
            let bytes = self.job_data.len();
            self.job_data.extend_from_slice(data);
            self.job_vertices += count;
            let state_arc = template
                .vstate_arc
                .get_or_insert_with(|| Arc::new(template.vstate.clone()))
                .clone();
            self.jobs.push(VertexJob {
                data: bytes..self.job_data.len(),
                count,
                base,
                a,
                b,
                cc,
                hud: self.hud,
                state: state_arc,
            });
            self.pending_ortho |= vstate.orthographic;
            base
        };

        let first = self.frame.indices.len() as u32;
        emit_indices(op, base, count as u32, &mut self.frame.indices);
        let emitted = self.frame.indices.len() as u32 - first;
        if emitted == 0 {
            return;
        }
        if template.state.cull == 3 {
            self.frame.indices.truncate(first as usize);
            return;
        }
        let geometry = if raw {
            Geometry::Raw
        } else {
            Geometry::Transformed
        };
        let vparams = if raw {
            template.vparams(&mut self.frame, false)
        } else {
            0
        };
        if self.last_serial == template.serial {
            if let Some(last) = self.frame.draws.last_mut() {
                if last.geometry == geometry
                    && last.vparams == vparams
                    && last.first + last.count == first
                {
                    last.count += emitted;
                    return;
                }
            }
        }
        let Material {
            shader,
            textures,
            samplers,
            ..
        } = template.material;
        self.frame.push_draw(Draw {
            first,
            count: emitted,
            state: template.state,
            shader,
            textures,
            samplers,
            params: template.params,
            geometry,
            vparams,
            viewport: template.viewport,
            scissor: template.scissor,
        });
        self.last_serial = template.serial;
    }

    fn cp_signature(&mut self) -> u64 {
        if let Some(signature) = self.cp_sig {
            return signature;
        }
        let mut hash = 0u64;
        let registers = [
            0x50..0x51,
            0x60..0x61,
            0x70..0x78,
            0x80..0x88,
            0x90..0x98,
            0xB0..0xBC,
        ];
        for register in registers.into_iter().flatten() {
            hash = (hash.rotate_left(7) ^ self.cp[register] as u64)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
        self.cp_sig = Some(hash);
        hash
    }

    fn spans_at(&self, spans: &[(u32, u32); 12]) -> Option<[(u32, u32); 12]> {
        let mut at = [(0u32, 0u32); 12];
        for n in (0..12).filter(|&n| spans[n].1 != 0) {
            let base = (self.cp[0xA0 + n] & 0x03FF_FFFF) | 0x8000_0000;
            let addr = base as u64 + spans[n].0 as u64;
            if addr + spans[n].1 as u64 + 16
                > (runtime::mem::RAM_BASE + runtime::mem::RAM_SIZE) as u64
            {
                return None;
            }
            at[n] = (addr as u32, spans[n].1);
        }
        Some(at)
    }

    fn array_bases(&self) -> [u32; 12] {
        core::array::from_fn(|n| (self.cp[0xA0 + n] & 0x03FF_FFFF) | 0x8000_0000)
    }

    fn exact_hash(&self, spans: &[(u32, u32); 12]) -> u64 {
        let mut hash = 0u64;
        for &(addr, len) in spans.iter().filter(|span| span.1 != 0) {
            let bytes = unsafe { self.mem.slice(addr, len as usize) };
            hash = (hash.rotate_left(5) ^ fast_block_hash(bytes)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
        hash
    }

    fn spans_hash(&mut self, spans: &[(u32, u32); 12]) -> u64 {
        let mut hash = 0u64;
        for &(addr, len) in spans.iter().filter(|span| span.1 != 0) {
            let from = addr - runtime::mem::RAM_BASE;
            for block in
                (from >> BLOCK_SHIFT) as usize..=((from + len - 1) >> BLOCK_SHIFT) as usize
            {
                if self.block_stamp[block] != self.epoch {
                    let block_addr = runtime::mem::RAM_BASE + ((block as u32) << BLOCK_SHIFT);
                    let bytes = unsafe { self.mem.slice(block_addr, 1 << BLOCK_SHIFT) };
                    self.block_hash[block] = fast_block_hash(bytes);
                    self.block_stamp[block] = self.epoch;
                }
                hash = (hash.rotate_left(5) ^ self.block_hash[block])
                    .wrapping_mul(0x9E37_79B9_7F4A_7C15);
            }
        }
        hash
    }

    fn build_list(&mut self, list: &[u8]) -> Option<([(u32, u32); 12], u32, u32, u8)> {
        let mut vertices = core::mem::take(&mut self.list_vertices);
        let mut indices = core::mem::take(&mut self.list_indices);
        vertices.clear();
        indices.clear();
        let (vcd_lo, vcd_hi) = (self.cp[0x50], self.cp[0x60]);
        let arrays: [(u32, u32); 12] = core::array::from_fn(|n| {
            (
                (self.cp[0xA0 + n] & 0x03FF_FFFF) | 0x8000_0000,
                self.cp[0xB0 + n] & 0xFF,
            )
        });
        let mut ranges = [(u32::MAX, 0u32); 12];
        let (mut at, mut draws, mut total) = (0, 0u32, 0u32);
        let mut vats_used = 0u8;
        let mut plain = true;

        while at < list.len() {
            let op = list[at];
            if op == 0 || op == 0x48 {
                at += 1;
                continue;
            }
            if !(0x80..=0xBF).contains(&op) || at + 3 > list.len() {
                static FAIL_LOG_COUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
                if FAIL_LOG_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed) < 10 {
                    crate::log_c(&format!("[dl-plain-fail] len={} at={} op={:#04x} vcd=({:#x},{:#x}) vat0=({:#x},{:#x},{:#x}) vsz0={} head={:02x?}",
                        list.len(), at, op, vcd_lo, vcd_hi,
                        self.cp[0x70], self.cp[0x80], self.cp[0x90],
                        self.vertex_size(0),
                        &list[..list.len().min(32)]
                    ));
                }
                plain = false;
                break;
            }
            let (count, vat) = (be16(list, at + 1), (op & 7) as usize);
            vats_used |= 1 << (vat as u8);
            let need = 3 + count * self.vertex_size(vat);
            if at + need > list.len() {
                static OVERRUN_LOG_COUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
                if OVERRUN_LOG_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed) < 10 {
                    crate::log_c(&format!("[dl-overrun-fail] len={} at={} need={} count={} vat={} vsz={} vcd=({:#x},{:#x}) vat=({:#x},{:#x},{:#x}) head={:02x?}",
                        list.len(), at, need, count, vat, self.vertex_size(vat),
                        vcd_lo, vcd_hi,
                        self.cp[0x70 + vat], self.cp[0x80 + vat], self.cp[0x90 + vat],
                        &list[..list.len().min(32)]
                    ));
                }
                plain = false;
                break;
            }
            let format = Format {
                vcd_lo,
                vcd_hi,
                a: self.cp[0x70 + vat],
                b: self.cp[0x80 + vat],
                arrays,
            };
            let base = vertices.len() as u32;
            decode_raw(
                self.mem,
                &list[at + 3..at + need],
                count,
                &format,
                &mut vertices,
                &mut ranges,
            );
            emit_indices(op, base, count as u32, &mut indices);
            draws += 1;
            total += count as u32;
            at += need;
        }

        let mut spans = [(0u32, 0u32); 12];
        for n in (0..12).filter(|&n| ranges[n].0 != u32::MAX) {
            let stride = arrays[n].1 as u64;
            let (offset, len) = (
                ranges[n].0 as u64 * stride,
                (ranges[n].1 - ranges[n].0) as u64 * stride + stride.clamp(1, 16),
            );
            if offset + len > runtime::mem::RAM_SIZE as u64 {
                plain = false;
                break;
            }
            spans[n] = (offset as u32, len as u32);
        }
        plain &= self.spans_at(&spans).is_some();
        self.list_vertices = vertices;
        self.list_indices = indices;
        plain.then_some((spans, draws, total, vats_used))
    }

    fn resolve_list(&mut self, addr: u32, size: usize) -> Ahead {
        let t0 = unsafe { crate::switch_now_ns() };
        let res = self.resolve_list_inner(addr, size);
        let dt = unsafe { crate::switch_now_ns().saturating_sub(t0) };
        crate::PERF_RESOLVE_NS.fetch_add(dt, core::sync::atomic::Ordering::Relaxed);
        res
    }

    fn resolve_list_inner(&mut self, addr: u32, size: usize) -> Ahead {
        if self.renderer.is_none() || size == 0 || !in_ram(addr, size) {
            static FAIL_RAM: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
            if FAIL_RAM.fetch_add(1, core::sync::atomic::Ordering::Relaxed) < 5 {
                crate::log_c(&format!("[late-ram] addr={:#010x} size={} in_ram={}", addr, size, in_ram(addr, size)));
            }
            return Ahead::Late;
        }
        if self.cp[0x50] & 0x1FF != 0 || self.cp[0x60] >> 8 != 0 {
            static FAIL_CP: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
            if FAIL_CP.fetch_add(1, core::sync::atomic::Ordering::Relaxed) < 5 {
                crate::log_c(&format!("[late-cp] cp50={:#010x} cp60={:#010x}", self.cp[0x50], self.cp[0x60]));
            }
            return Ahead::Late;
        }
        let list = unsafe { self.mem.slice(addr, size) };
        let list_hash = if size >= 16 {
            let w0 = u64::from_le_bytes(list[..8].try_into().unwrap());
            let w1 = u64::from_le_bytes(list[size - 8..].try_into().unwrap());
            (addr as u64)
                ^ ((size as u64) << 32)
                ^ w0.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ w1.rotate_left(31)
        } else {
            (addr as u64) ^ ((size as u64) << 32)
        };
        let cp_sig = self.cp_signature();
        let key = (list_hash, cp_sig);
        let mut built = false;
        let shape = match self.shapes.get(&key).copied() {
            Some(shape) => shape,
            None => {
                match self.build_list(list) {
                    Some((spans, draws, vertices, vats_used)) => {
                        built = true;
                        let s = ListShape {
                            plain: true,
                            spans,
                            index_count: self.list_indices.len() as u32,
                            draws,
                            vertices,
                            vats_used,
                        };
                        self.shapes.insert(key, s);
                        s
                    }
                    None => {
                        return Ahead::Late;
                    }
                }
            }
        };

        let mut place = None;
        if let (true, Some(at)) = (!self.cache_full, self.spans_at(&shape.spans)) {
            let mut bases = 0u64;
            for n in (0..12).filter(|&n| shape.spans[n].1 != 0) {
                let base = (self.cp[0xA0 + n] & 0x03FF_FFFF) | 0x8000_0000;
                let stride = self.cp[0xB0 + n] & 0xFF;
                bases = (bases.rotate_left(9) ^ ((n as u64) << 32) ^ (base as u64) ^ ((stride as u64) << 48))
                    .wrapping_mul(0x9E37_79B9_7F4A_7C15);
            }
            let check_key = (key.0, cp_sig, bases);
            if let Some(check) = self.checks.get(&check_key) {
                place = Some(check.place);
            } else {
                let exact = self.exact_hash(&at);
                place = self.places.get(&(key.0, cp_sig, exact)).copied();
                if place.is_none() && (built || self.build_list(list).is_some()) {
                    built = true;
                    let content = (
                        fast_block_hash(bytemuck::cast_slice(&self.list_vertices)),
                        fast_block_hash(bytemuck::cast_slice(&self.list_indices)),
                        self.list_vertices.len() as u32,
                    );
                    let shared = self.contents.get(&content).copied();
                    place = shared.or_else(|| {
                        self.renderer
                            .as_mut()
                            .and_then(|r| r.cache_alloc(&self.list_vertices, &self.list_indices))
                    });
                    match place {
                        None => self.cache_full = true,
                        Some(p) => {
                            self.places.insert((key.0, cp_sig, exact), p);
                            if shared.is_none() {
                                self.contents.insert(content, p);
                            }
                        }
                    }
                }
                if let Some(p) = place {
                    if self.checks.len() > 1 << 16 {
                        self.checks.clear();
                    }
                    self.checks.insert(check_key, ListCheck { blocks: 0, exact, place: p });
                }
            }
        }
        match place {
            Some(place) => Ahead::Cached(shape, place),
            None if built || self.build_list(list).is_some() => {
                let (vertices, indices) = self.spare.pop().unwrap_or_default();
                Ahead::Owned(
                    shape,
                    core::mem::replace(&mut self.list_vertices, vertices),
                    core::mem::replace(&mut self.list_indices, indices),
                )
            }
            None => {
                static FAIL_LATE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
                if FAIL_LATE.fetch_add(1, core::sync::atomic::Ordering::Relaxed) < 5 {
                    crate::log_c(&format!("[late-none] built={} cache_full={}", built, self.cache_full));
                }
                Ahead::Late
            }
        }
    }

    fn cached_list(&mut self, addr: u32, size: usize, at: u64, c: &mut Ctx) -> bool {
        crate::STAT_CLIST_CALLS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        while self.ahead.front().is_some_and(|ahead| ahead.0 < at) {
            if let Some((_, Ahead::Owned(_, vertices, indices))) = self.ahead.pop_front() {
                self.spare.push((vertices, indices));
            }
        }
        let noted = match self.ahead.front() {
            Some(ahead) if ahead.0 == at => self.ahead.pop_front().map(|ahead| ahead.1),
            _ => None,
        };
        if noted.is_some() {
            crate::STAT_AHEAD_HITS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
        let give_back = |gx: &mut Gx, ahead: Option<Ahead>| {
            if let Some(Ahead::Owned(_, vertices, indices)) = ahead {
                gx.spare.push((vertices, indices));
            }
            false
        };
        if self.renderer.is_none() {
            return give_back(self, noted);
        }
        if self.template.is_none() {
            self.build_template(c);
        }
        let raw_ok = self
            .template
            .as_ref()
            .is_some_and(|template| template.vstate.raw_ok);
        if !raw_ok {
            crate::STAT_CLIST_RAW_FAIL.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            return give_back(self, noted);
        }
        if self.hud != 0 && self.template.as_ref().map_or(false, |t| t.vstate.orthographic) {
            crate::STAT_CLIST_HUD_FAIL.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            return give_back(self, noted);
        }
        if self.pending_ortho {
            self.finish_vertices();
        }
        let ahead = match noted {
            Some(ahead) => ahead,
            None => self.resolve_list(addr, size),
        };
        let shape = match &ahead {
            Ahead::Cached(shape, _) | Ahead::Owned(shape, ..) => *shape,
            Ahead::Late => {
                crate::STAT_CLIST_RES_FAIL.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                return false;
            }
        };
        self.stats.draws += shape.draws;
        self.stats.vertices += shape.vertices;
        self.draw_index += shape.draws;
        let mut template = self.template.take().expect("template built");
        let drawn = shape.index_count != 0 && template.state.cull != 3;
        let (first, geometry) = match ahead {
            Ahead::Cached(_, (base_vertex, first_index)) => {
                crate::STAT_CACHED_DRAWS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                (first_index, Geometry::Cached(base_vertex))
            }
            Ahead::Owned(_, vertices, indices) => {
                crate::STAT_RAW_DRAWS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                let (base, first) = (self.frame.raw.len() as u32, self.frame.indices.len() as u32);
                if drawn {
                    self.frame.raw.extend_from_slice(&vertices);
                    self.frame
                        .indices
                        .extend(indices.iter().map(|index| index + base));
                }
                self.spare.push((vertices, indices));
                (first, Geometry::Raw)
            }
            Ahead::Late => unreachable!(),
        };
        if drawn {
            let vparams = template.vparams(&mut self.frame, false);
            let Material {
                shader,
                textures,
                samplers,
                ..
            } = template.material;
            self.frame.draws.push(Draw {
                first,
                count: shape.index_count,
                state: template.state,
                shader,
                textures,
                samplers,
                params: template.params,
                geometry,
                vparams,
                viewport: template.viewport,
                scissor: template.scissor,
            });
            self.last_serial = 0;
        }
        self.template = Some(template);
        true
    }

    pub fn look_ahead(&mut self, c: &Ctx) {
        if self.renderer.is_none() {
            return;
        }
        self.mem = Mem(c.mem);
        if self.ahead_pos < self.consumed || self.ahead_bad {
            self.ahead_pos = self.consumed;
            self.ahead_cp.copy_from_slice(&self.cp);
            self.ahead_sig = self.cp_sig;
            self.ahead_bad = false;
        }
        let start = (self.ahead_pos - self.consumed) as usize;
        let fifo = fifo_slice(c);
        let mut data = core::mem::take(&mut self.ahead_data);
        data.clear();
        if start < self.pending.len() {
            data.extend_from_slice(&self.pending[start..]);
            data.extend_from_slice(fifo);
        } else {
            data.extend_from_slice(fifo.get(start - self.pending.len()..).unwrap_or(&[]));
        }
        if data.is_empty() {
            self.ahead_data = data;
            return;
        }
        core::mem::swap(&mut self.cp, &mut self.ahead_cp);
        core::mem::swap(&mut self.cp_sig, &mut self.ahead_sig);
        let mut i = 0;
        while i < data.len() {
            let rest = &data[i..];
            let op = rest[0];
            let need = match op {
                0x00 | 0x48 => 1,
                0x08 => 6,
                0x10 if rest.len() >= 5 => 5 + 4 * ((be16(rest, 1) & 0xF) + 1),
                0x20 | 0x28 | 0x30 | 0x38 | 0x61 => 5,
                0x40 => 9,
                0x80..=0xBF if rest.len() >= 3 => {
                    3 + be16(rest, 1) * self.vertex_size((op & 7) as usize)
                }
                0x10 | 0x80..=0xBF => break,
                _ => {
                    self.ahead_bad = true;
                    break;
                }
            };
            if rest.len() < need {
                break;
            }
            match op {
                0x08 => {
                    let (register, value) = (rest[1] as usize, be32(rest, 2));
                    if self.cp[register] != value {
                        self.cp[register] = value;
                        self.cp_sig = None;
                    }
                }
                0x40 => {
                    let (addr, size) = (be32(rest, 1) | 0x8000_0000, be32(rest, 5));
                    let ahead = self.resolve_list(addr, size as usize);
                    self.ahead.push_back((self.ahead_pos + i as u64, ahead));
                }
                _ => {}
            }
            i += need;
        }
        core::mem::swap(&mut self.cp, &mut self.ahead_cp);
        core::mem::swap(&mut self.cp_sig, &mut self.ahead_sig);
        self.ahead_pos += i as u64;
        self.ahead_data = data;
    }

    pub fn cache_room_used(&self) -> u64 {
        self.renderer
            .as_ref()
            .map_or(0, |renderer| renderer.cache_used())
    }

    fn vertex_size(&self, vat: usize) -> usize {
        let (vcd_lo, vcd_hi) = (self.cp[0x50], self.cp[0x60]);
        let (a, b, c) = (self.cp[0x70 + vat], self.cp[0x80 + vat], self.cp[0x90 + vat]);
        let attr = |kind: u32, direct: usize| match kind {
            1 => direct,
            2 => 1,
            3 => 2,
            _ => 0,
        };

        let mut size = (vcd_lo & 0x1FF).count_ones() as usize;
        size += attr(
            (vcd_lo >> 9) & 3,
            (2 + (a & 1) as usize) * COMP[((a >> 1) & 7) as usize],
        );
        let nbt = (a >> 9) & 1 != 0;
        let nrm_direct = if nbt { 9 } else { 3 } * COMP[((a >> 10) & 7) as usize];
        let nrm_indices = if nbt && a >> 31 != 0 { 3 } else { 1 };
        size += match (vcd_lo >> 11) & 3 {
            1 => nrm_direct,
            k => attr(k, 0) * nrm_indices,
        };
        size += attr((vcd_lo >> 13) & 3, COLOR[((a >> 14) & 7) as usize]);
        size += attr((vcd_lo >> 15) & 3, COLOR[((a >> 18) & 7) as usize]);
        let tex = [
            (a >> 21, a >> 22),
            (b, b >> 1),
            (b >> 9, b >> 10),
            (b >> 18, b >> 19),
            (b >> 27, b >> 28),
            (c >> 5, c >> 6),
            (c >> 14, c >> 15),
            (c >> 23, c >> 24),
        ];
        for (i, (cnt, fmt)) in tex.into_iter().enumerate() {
            size += attr(
                (vcd_hi >> (i * 2)) & 3,
                (1 + (cnt & 1) as usize) * COMP[(fmt & 7) as usize],
            );
        }
        size
    }

    fn parse(&mut self, data: &[u8], c: &mut Ctx, depth: u32) -> usize {
        let mut i = 0;
        while i < data.len() {
            if depth == 0 {
                while let Some(&(_, widget)) = self
                    .hud_marks
                    .front()
                    .filter(|mark| mark.0 <= self.consumed + i as u64)
                {
                    self.hud = widget;
                    self.hud_marks.pop_front();
                }
            }
            let rest = &data[i..];
            let op = rest[0];
            let need = match op {
                0x00 | 0x48 => 1,
                0x08 => 6,
                0x10 if rest.len() >= 5 => 5 + 4 * ((be16(rest, 1) & 0xF) + 1),
                0x20 | 0x28 | 0x30 | 0x38 | 0x61 => 5,
                0x40 => 9,
                0x80..=0xBF if rest.len() >= 3 => {
                    3 + be16(rest, 1) * self.vertex_size((op & 7) as usize)
                }
                0x10 | 0x80..=0xBF => break,
                _ => return data.len(),
            };
            if rest.len() < need {
                break;
            }
            match op {
                0x08 => {
                    let t0 = unsafe { crate::switch_now_ns() };
                    let (register, value) = (rest[1] as usize, be32(rest, 2));
                    if self.cp[register] != value {
                        self.cp[register] = value;
                        if register == 0x30 {
                            let default_matrix = (value & 0x3F) as usize;
                            if self.template.is_some() {
                                let pos: [f32; 12] = core::array::from_fn(|k| self.xf_float((default_matrix * 4 + k) & 0xFF));
                                let norm: [f32; 9] = core::array::from_fn(|k| self.xf_float(0x400 + (default_matrix & 31) * 3 + k));
                                if let Some(t) = &mut self.template {
                                    t.vstate.default_matrix = default_matrix;
                                    t.vstate.matrices = match &mut t.vstate.matrices {
                                        Matrices::One(p, n) => {
                                            *p = pos;
                                            *n = norm;
                                            Matrices::One(*p, *n)
                                        }
                                        Matrices::All(all) => {
                                            all[..0x100].copy_from_slice(&self.xf[..0x100]);
                                            all[0x100..].copy_from_slice(&self.xf[0x400..0x480]);
                                            Matrices::All(*all)
                                        }
                                    };
                                    t.vparams = None;
                                    t.vparams_flat = None;
                                }
                            }
                        } else if (0xA0..0xBC).contains(&register) && self.template.is_some() {
                            if let Some(t) = &mut self.template {
                                if (0xA0..0xAC).contains(&register) {
                                    let n = register - 0xA0;
                                    t.vstate.arrays[n].0 = (value & 0x03FF_FFFF) | 0x8000_0000;
                                } else if (0xB0..0xBC).contains(&register) {
                                    let n = register - 0xB0;
                                    t.vstate.arrays[n].1 = value & 0xFF;
                                }
                                t.vparams = None;
                                t.vparams_flat = None;
                            }
                        } else {
                            self.template = None;
                        }
                        self.cp_sig = None;
                    }
                    self.stats.cp_loads += 1;
                    let dt = unsafe { crate::switch_now_ns().saturating_sub(t0) };
                    crate::PERF_PARSE_CP_NS.fetch_add(dt, core::sync::atomic::Ordering::Relaxed);
                }
                0x10 => {
                    let t0 = unsafe { crate::switch_now_ns() };
                    let base = be16(rest, 3);
                    let mut changed = false;
                    for (k, word) in rest[5..need].chunks_exact(4).enumerate() {
                        if let Some(slot) = self.xf.get_mut(base + k) {
                            let value = be32(word, 0);
                            changed |= *slot != value;
                            *slot = value;
                        }
                    }
                    if changed {
                        if base < 0x0500 && self.template.is_some() {
                            let default_matrix = self.template.as_ref().unwrap().vstate.default_matrix;
                            let pos: [f32; 12] = core::array::from_fn(|k| self.xf_float((default_matrix * 4 + k) & 0xFF));
                            let norm: [f32; 9] = core::array::from_fn(|k| self.xf_float(0x400 + (default_matrix & 31) * 3 + k));
                            if let Some(t) = &mut self.template {
                                t.vstate.matrices = match &mut t.vstate.matrices {
                                    Matrices::One(p, n) => {
                                        *p = pos;
                                        *n = norm;
                                        Matrices::One(*p, *n)
                                    }
                                    Matrices::All(all) => {
                                        all[..0x100].copy_from_slice(&self.xf[..0x100]);
                                        all[0x100..].copy_from_slice(&self.xf[0x400..0x480]);
                                        Matrices::All(*all)
                                    }
                                };
                                t.vparams = None;
                                t.vparams_flat = None;
                            }
                        } else if (0x100A..=0x1011).contains(&base) && self.template.is_some() {
                            let channels: [Channel; 2] = core::array::from_fn(|n| {
                                let color = |reg: u32| [24, 16, 8, 0].map(|s| ((reg >> s) & 0xFF) as f32 / 255.0);
                                Channel {
                                    material: color(self.xf[0x100C + n]),
                                    ambient: color(self.xf[0x100A + n]),
                                    ctrl: [self.xf[0x100E + n], self.xf[0x1010 + n]],
                                }
                            });
                            let lit_channels = (self.xf[0x1009] & 3).min(2) as usize;
                            let lit = |ctrl: u32| ctrl & 2 != 0 && ctrl & 0x783C != 0;
                            let lighting = channels[..lit_channels]
                                .iter()
                                .any(|ch| lit(ch.ctrl[0]) || lit(ch.ctrl[1]));
                            if let Some(t) = &mut self.template {
                                t.vstate.channels = channels;
                                t.vstate.lit_channels = lit_channels;
                                t.vstate.lighting = lighting;
                                t.vparams = None;
                                t.vparams_flat = None;
                            }
                        } else if (0x0600..0x0680).contains(&base) && self.template.is_some() {
                            let lighting = self.template.as_ref().map(|t| t.vstate.lighting).unwrap_or(false);
                            let lights: [Light; 8] = core::array::from_fn(|n| {
                                if !lighting {
                                    return Light::default();
                                }
                                let b = 0x600 + n * 16;
                                let vector = |at: usize| [0, 1, 2].map(|k| self.xf_float(b + at + k));
                                Light {
                                    color: [24, 16, 8, 0].map(|s| ((self.xf[b + 3] >> s) & 0xFF) as f32 / 255.0),
                                    cos_att: vector(4),
                                    dist_att: vector(7),
                                    pos: vector(10),
                                    dir: vector(13),
                                }
                            });
                            if let Some(t) = &mut self.template {
                                t.vstate.lights = lights;
                                t.vparams = None;
                                t.vparams_flat = None;
                            }
                        } else {
                            self.template = None;
                        }
                    }
                    self.stats.xf_loads += 1;
                    let dt = unsafe { crate::switch_now_ns().saturating_sub(t0) };
                    crate::PERF_PARSE_XF_NS.fetch_add(dt, core::sync::atomic::Ordering::Relaxed);
                }
                0x40 if depth < 4 => {
                    let t0 = unsafe { crate::switch_now_ns() };
                    let (addr, size) = (be32(rest, 1) | 0x8000_0000, be32(rest, 5));
                    self.stats.display_lists += 1;
                    let at = self.consumed + i as u64;
                    if depth != 0 || !self.cached_list(addr, size as usize, at, c) {
                        if in_ram(addr, size as usize) {
                            let list = unsafe { Mem(c.mem).slice(addr, size as usize) };
                            self.parse(list, c, depth + 1);
                        }
                    }
                    let dt = unsafe { crate::switch_now_ns().saturating_sub(t0) };
                    crate::PERF_CLIST_NS.fetch_add(dt, core::sync::atomic::Ordering::Relaxed);
                }
                0x61 => {
                    let word = be32(rest, 1);
                    let reg = (word >> 24) as usize;
                    if reg == BP_EFB_COPY {
                        self.bp_load(word, c);
                    } else {
                        let t0 = unsafe { crate::switch_now_ns() };
                        self.bp_load(word, c);
                        let dt = unsafe { crate::switch_now_ns().saturating_sub(t0) };
                        crate::PERF_PARSE_BP_NS.fetch_add(dt, core::sync::atomic::Ordering::Relaxed);
                    }
                }
                0x80..=0xBF => {
                    let t0 = unsafe { crate::switch_now_ns() };
                    self.stats.draws += 1;
                    let vcnt = be16(rest, 1);
                    self.stats.vertices += vcnt as u32;
                    if self.renderer.is_some() {
                        self.draw(op, vcnt, &rest[3..need], c);
                    }
                    let dt = unsafe { crate::switch_now_ns().saturating_sub(t0) };
                    crate::PERF_PARSE_DIRECT_DRAW_NS.fetch_add(dt, core::sync::atomic::Ordering::Relaxed);
                }
                _ => {}
            }
            i += need;
        }
        i
    }

    fn bp_load(&mut self, word: u32, c: &mut Ctx) {
        let reg = (word >> 24) as usize;
        let mask = if reg == 0xFE {
            0xFF_FFFF
        } else {
            core::mem::replace(&mut self.bp[0xFE], 0xFF_FFFF)
        };
        let value = (self.bp[reg] & !mask) | (word & mask & 0xFF_FFFF);
        let changed = self.bp[reg] != value;
        self.bp[reg] = value;
        self.stats.bp_loads += 1;

        if changed {
            let is_mat = matches!(
                reg,
                0x00 | 0x28..=0x2F | 0x80..=0xBF | 0xC0..=0xDF | 0xF1 | 0xF3 | 0xF6..=0xFF
            );
            if is_mat {
                self.forget_material();
            } else if let Some(t) = &mut self.template {
                if reg == 0x20 || reg == 0x21 {
                    let (tl, br) = (self.bp[0x20], self.bp[0x21]);
                    let edge = |v: u32, extra: u32| ((v & 0x7FF) + extra).saturating_sub(342);
                    t.scissor = [edge(tl >> 12, 0), edge(tl, 0), edge(br >> 12, 1), edge(br, 1)];
                } else if reg == 0x40 || reg == 0x41 {
                    let (zmode, cmode) = (self.bp[0x40], self.bp[0x41]);
                    t.state.depth_test = zmode & 1 != 0;
                    t.state.depth_func = ((zmode >> 1) & 7) as u8;
                    t.state.depth_write = zmode & 0x10 != 0;
                    t.state.blend = cmode & 1 != 0 || cmode & 0x800 != 0;
                    t.state.subtract = cmode & 0x800 != 0;
                    t.state.color_write = cmode & 8 != 0;
                    t.state.alpha_write = cmode & 0x10 != 0;
                    t.state.dst_factor = ((cmode >> 5) & 7) as u8;
                    t.state.src_factor = ((cmode >> 8) & 7) as u8;
                }
            }
        }

        match reg {
            0xE0..=0xE7 => {
                let part = |v: u32| (((v & 0x7FF) << 21) as i32 >> 21) as f32 / 255.0;
                let (low, high) = (part(value), part(value >> 12));
                let index = (reg - x0_e0()) / 2;
                let target = if value & 0x80_0000 != 0 {
                    &mut self.tev_konst[index]
                } else {
                    &mut self.tev_reg[index]
                };
                if reg & 1 == 0 {
                    (target[0], target[3]) = (low, high);
                } else {
                    (target[2], target[1]) = (low, high);
                }
                if self.template.is_some() {
                    let params = self.frame.push_params(&self.tev_params());
                    if let Some(t) = &mut self.template {
                        t.params = params;
                    }
                }
            }
            0x65 => {
                let source = ((self.bp[0x64] & 0xFF_FFFF) << 5) | 0x8000_0000;
                let (dest, len) = (
                    ((value & 0x3FF) << 9) as usize,
                    (((value >> 10) & 0x3FF) << 5) as usize,
                );
                if in_ram(source, len) && dest + len <= self.tmem.len() {
                    let src_bytes = unsafe { Mem(c.mem).slice(source, len) };
                    self.tmem[dest..dest + len].copy_from_slice(src_bytes);
                }
                if self.has_paletted_textures {
                    let mut any_pal = false;
                    self.texture_cache.retain(|&(_, image, _), _| {
                        let format = (image >> 20) & 0xF;
                        let pal = texture::palette_entries(format) != 0;
                        if pal {
                            any_pal = true;
                        }
                        !pal
                    });
                    self.has_paletted_textures = any_pal;
                    self.material_cache_len = 0;
                }
                self.forget_material();
            }
            BP_PE_TOKEN => self.token = value as u16,
            BP_PE_TOKEN_INT => {
                self.token = value as u16;
                self.raised.push(Raised::Token(value as u16));
            }
            BP_PE_DONE if value & 2 != 0 => {
                self.finish_requests += 1;
                self.raised.push(Raised::Finish);
            }
            BP_EFB_COPY if value & (1 << 14) != 0 => {
                let t0 = unsafe { crate::switch_now_ns() };
                self.end_frame(value);
                let dt = unsafe { crate::switch_now_ns().saturating_sub(t0) };
                crate::PERF_COPY_DISPLAY_NS.fetch_add(dt, core::sync::atomic::Ordering::Relaxed);
            }
            BP_EFB_COPY => {
                let t0 = unsafe { crate::switch_now_ns() };
                self.copy_to_texture(value);
                let dt = unsafe { crate::switch_now_ns().saturating_sub(t0) };
                crate::PERF_COPY_TEXTURE_NS.fetch_add(dt, core::sync::atomic::Ordering::Relaxed);
            }
            _ => {}
        }
    }

    fn copy_source(&self) -> [u32; 4] {
        let (xy, wh) = (self.bp[0x49], self.bp[0x4A]);
        [
            xy & 0x3FF,
            (xy >> 10) & 0x3FF,
            (wh & 0x3FF) + 1,
            ((wh >> 10) & 0x3FF) + 1,
        ]
    }

    fn clear_color(&self) -> [f64; 4] {
        let (ar, gb) = (self.bp[0x4F], self.bp[0x50]);
        [ar & 0xFF, (gb >> 8) & 0xFF, gb & 0xFF, (ar >> 8) & 0xFF].map(|v| v as f64 / 255.0)
    }

    fn copy_to_texture(&mut self, copy: u32) {
        self.finish_vertices();
        let mut frame = core::mem::take(&mut self.frame);
        frame.flat.clear();
        frame.raw_flat.clear();
        self.flat_started = false;
        let rect = self.copy_source();
        let dest = ((self.bp[0x4B] & 0xFF_FFFF) << 5) | 0x8000_0000;
        if let Some(renderer) = &mut self.renderer {
            renderer.flush(&frame, self.clear.take());
            renderer.copy_efb(dest, rect);
        }
        frame.clear();
        self.frame = frame;
        // Do not clear texture_cache or material on EFB copies; EFB textures are tracked independently in efb_copies
        if copy & (1 << 11) != 0 {
            if self.write_masks() == (true, true, true) {
                self.clear = Some(self.clear_color());
            } else {
                self.queue_clear(rect);
            }
        }
        crate::STAT_COPY_TEXTURE_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }

    fn write_masks(&self) -> (bool, bool, bool) {
        (
            self.bp[0x41] & 8 != 0,
            self.bp[0x41] & 0x10 != 0,
            self.bp[0x40] & 0x10 != 0,
        )
    }

    fn queue_clear(&mut self, rect: [u32; 4]) {
        let (color_write, alpha_write, depth_write) = self.write_masks();
        if !(color_write || alpha_write || depth_write) {
            return;
        }
        let color = self.clear_color().map(|v| v as f32);
        let corner = |x: f32, y: f32| Vertex {
            pos: [x, y, 1.0, 1.0],
            colors: [color; 2],
            uvs: [[0.0; 4]; 4],
        };
        let material = tev_gl::Config::passthrough();
        let shader = material.key();
        if let Some(renderer) = &mut self.renderer {
            renderer.ensure_shader(shader, || material.fragment_glsl());
        }
        let params = self.frame.push_params(&[0.0; PARAMS_FLOATS]);
        self.finish_vertices();
        let base = self.frame.vertices.len() as u32;
        let first = self.frame.indices.len() as u32;
        self.frame.vertices.extend_from_slice(&[
            corner(-1.0, 1.0),
            corner(1.0, 1.0),
            corner(1.0, -1.0),
            corner(-1.0, -1.0),
        ]);
        self.frame
            .indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        let state = DrawState {
            depth_test: depth_write,
            depth_func: 7,
            depth_write,
            blend: false,
            subtract: false,
            src_factor: 1,
            cull: 0,
            dst_factor: 0,
            color_write,
            alpha_write,
        };
        self.frame.push_draw(Draw {
            first,
            count: 6,
            state,
            shader,
            textures: [WHITE_TEXTURE; TEXTURE_SLOTS],
            samplers: [(0, 0, false); TEXTURE_SLOTS],
            params,
            geometry: Geometry::Transformed,
            vparams: 0,
            viewport: [0.0, 0.0, EFB_WIDTH as f32, EFB_HEIGHT as f32],
            scissor: [rect[0], rect[1], rect[0] + rect[2], rect[1] + rect[3]],
        });
        self.last_serial = 0;
    }

    fn end_frame(&mut self, copy: u32) {
        self.epoch = self.epoch.wrapping_add(1).max(1);
        self.finish_vertices();
        self.frames += 1;
        self.draw_index = 0;
        // Texture lookups key on address only; re-hash once per frame so textures the game
        // rewrites in place are noticed (matches desktop gx.rs end_frame).
        if self.texture_cache.len() > 2048 {
            self.texture_cache.clear();
            self.material_cache_len = 0;
            self.has_paletted_textures = false;
        }
        self.forget_material();
        self.copied_to_display = true;
        let to = ((self.bp[0x4B] & 0xFF_FFFF) << 5) | 0x8000_0000;
        if !self.display_copies.contains(&to) {
            self.display_copies.push(to);
        }

        let mut frame = core::mem::take(&mut self.frame);
        self.flat_started = false;
        let widen = crate::aspect::widen();
        if widen != 1.0 {
            let mut widgets: HashMap<u32, (f32, f32)> = HashMap::new();
            for flat in frame.flat.iter().filter(|flat| flat.hud != 0) {
                let extent = widgets.entry(flat.hud).or_insert((f32::MAX, f32::MIN));
                *extent = (extent.0.min(flat.extent.0), extent.1.max(flat.extent.1));
            }
            let margin = 1.0 - 1.0 / widen;
            for flat in &frame.flat {
                let side = match widgets.get(&flat.hud) {
                    Some(&(_, right)) if right < 0.0 => -margin,
                    Some(&(left, _)) if left > 0.0 => margin,
                    _ => 0.0,
                };
                for vertex in &mut frame.vertices[flat.from as usize..flat.to as usize] {
                    match flat.origin {
                        None => vertex.pos[0] /= widen,
                        Some(centre) => {
                            for (at, origin) in vertex.pos.iter_mut().zip(centre) {
                                *at = origin + (*at - origin) / widen;
                            }
                            vertex.pos[0] -= centre[0] / centre[3] * margin * vertex.pos[3];
                        }
                    }
                    vertex.pos[0] += side * vertex.pos[3];
                }
            }
            for (offset, centre) in core::mem::take(&mut frame.raw_flat) {
                frame.place_again(offset, 1.0 / widen, -centre[0] / centre[3] * margin);
            }
        }

        if let Some(renderer) = &mut self.renderer {
            let (xy, wh) = (self.bp[0x49], self.bp[0x4A]);
            let shown = [
                xy & 0x3FF,
                (xy >> 10) & 0x3FF,
                (wh & 0x3FF) + 1,
                ((wh >> 10) & 0x3FF) + 1,
            ];
            renderer.present(&frame, self.clear, shown, self.frames);
        }

        frame.clear();
        self.frame = frame;

        if core::mem::take(&mut self.cache_full) {
            self.shapes.clear();
            self.checks.clear();
            self.places.clear();
            for (_, ahead) in &mut self.ahead {
                if matches!(ahead, Ahead::Cached(..)) {
                    *ahead = Ahead::Late;
                }
            }
            self.contents.clear();
            self.seen.clear();
            if let Some(renderer) = &mut self.renderer {
                renderer.cache_reset();
            }
            self.churn.clear();
        }

        let rect = self.copy_source();
        self.clear = None;
        if copy & (1 << 11) != 0 {
            if self.write_masks() == (true, true, true) {
                self.clear = Some(self.clear_color());
            } else {
                self.queue_clear(rect);
            }
        }
        self.last_stats = core::mem::take(&mut self.stats);
        self.hud = 0;
        self.hud_marked = 0;
        self.hud_marks.clear();
        crate::STAT_COPY_DISPLAY_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn has_pending(&self, c: &Ctx) -> bool {
        c.fifo_len != 0 || !self.pending.is_empty()
    }

    pub fn process(&mut self, c: &mut Ctx) {
        let mut data = core::mem::take(&mut self.pending);
        if c.fifo_len != 0 {
            data.extend_from_slice(fifo_slice(c));
            c.fifo_len = 0;
        }
        let allowed = match self.limit {
            Some(limit) => (limit.saturating_sub(self.consumed) as usize).min(data.len()),
            None => data.len(),
        };
        if allowed > 0 {
            self.mem = Mem(c.mem);
            let t0 = unsafe { crate::switch_now_ns() };
            let used = self.parse(&data[..allowed], c, 0);
            self.finish_vertices();
            let dt = unsafe { crate::switch_now_ns().saturating_sub(t0) };
            crate::PERF_PARSE_NS.fetch_add(dt, core::sync::atomic::Ordering::Relaxed);
            data.drain(..used);
            self.consumed += used as u64;
        }
        self.pending = data;
        if self.limit == Some(self.consumed) && !self.break_signalled {
            self.break_signalled = true;
            self.raised.push(Raised::Break);
        }
    }

    pub fn absorb(&mut self, c: &mut Ctx) {
        if c.fifo_len != 0 {
            self.pending.extend_from_slice(fifo_slice(c));
            c.fifo_len = 0;
        }
    }

    pub fn mark_hud(&mut self, c: &Ctx, widget: bool) {
        if !widget && self.hud_marked == 0 {
            return;
        }
        if widget {
            self.hud_widgets = self.hud_widgets % 0xFFFF + 1;
        }
        self.hud_marked = if widget { self.hud_widgets } else { 0 };
        self.hud_marks.push_back((self.written(c), self.hud_marked));
    }

    #[inline(always)]
    fn written(&self, c: &Ctx) -> u64 {
        self.consumed + (self.pending.len() + c.fifo_len) as u64
    }

    fn fifo_range(c: &Ctx) -> (u32, u64) {
        let (base, top) = (
            c.r32(0xCC00_300C) & 0x03FF_FFE0,
            c.r32(0xCC00_3010) & 0x03FF_FFE0,
        );
        if top > base {
            (base | 0x8000_0000, (top - base + 4) as u64)
        } else {
            (0x8000_0000, 0x4_0000)
        }
    }

    pub fn fifo_ptrs(&mut self, c: &Ctx) -> (u32, u32) {
        let (base, size) = Self::fifo_range(c);
        let at = |pos: u64| base + (pos % size) as u32;
        let written = self.written(c);
        if self.marks.len() == 64 {
            self.marks.remove(0);
        }
        self.marks.push((at(written), written));
        (at(self.consumed), at(written))
    }

    pub fn set_breakpoint(&mut self, c: &Ctx, addr: u32) {
        let known = self
            .marks
            .iter()
            .filter(|m| m.0 == addr && m.1 >= self.consumed)
            .map(|m| m.1)
            .min();
        let limit = known.unwrap_or_else(|| {
            let (base, size) = Self::fifo_range(c);
            let written = self.written(c);
            let behind = (written + size - (addr.wrapping_sub(base) as u64 % size)) % size;
            written - behind.min(written)
        });
        self.limit = Some(limit);
        self.break_signalled = false;
    }

    pub fn clear_breakpoint(&mut self) {
        self.limit = None;
    }

    pub fn reset_cache(&mut self) {
        self.cache_full = true;
        self.material_cache_len = 0;
    }

    pub fn describe(&self) -> String {
        format!(
            "consumed={} pending={} limit={:?} token={:#06x} finishes={}",
            self.consumed,
            self.pending.len(),
            self.limit,
            self.token,
            self.finish_requests
        )
    }
}

#[inline(always)]
const fn x0_e0() -> usize {
    0xE0
}

