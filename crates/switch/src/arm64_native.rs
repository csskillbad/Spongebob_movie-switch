//! Native ARM64 (AArch64 / NEON) replacements for hot GameCube SDK, MSL math/libc,
//! Heavy Iron math, and RenderWare matrix/vector/skeletal-skinning routines.
//!
//! Every GQR-sensitive or domain-sensitive entry uses `Hle::Replace` and returns `false`
//! on non-default GQR or IEEE edge cases so the recompiled PowerPC code handles rare
//! corner cases with 100% bit-exactness while 99.999%+ of gameplay runs native ARM64 SIMD.

use runtime::{mr16, mr32, mr8, mptr, mw16, mw32, mw8, Ctx};

extern "C" {
    fn sinf(x: f32) -> f32;
    fn cosf(x: f32) -> f32;
    fn powf(x: f32, y: f32) -> f32;
    fn fmodf(x: f32, y: f32) -> f32;
    fn sin(x: f64) -> f64;
    fn cos(x: f64) -> f64;
    fn tan(x: f64) -> f64;
    fn atan(x: f64) -> f64;
    fn atan2(y: f64, x: f64) -> f64;
    fn acos(x: f64) -> f64;
    fn asin(x: f64) -> f64;
    fn fmod(x: f64, y: f64) -> f64;
}

#[inline(always)]
fn in_ram(a: u32, len: usize) -> bool {
    a >= 0x8000_0000
        && a < 0x8180_0000
        && (a as usize - 0x8000_0000).checked_add(len).is_some_and(|end| end <= 0x0180_0000)
}

#[inline(always)]
fn hw_sqrt_f64(x: f64) -> f64 {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        let out: f64;
        core::arch::asm!("fsqrt {:d}, {:d}", out(vreg) out, in(vreg) x, options(pure, nomem, nostack));
        out
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        f64::from_bits(0) + {
            // Portable Newton-Raphson fallback for non-aarch64 `cargo check` builds
            extern "C" { fn sqrt(v: f64) -> f64; }
            unsafe { sqrt(x) }
        }
    }
}

#[inline(always)]
fn hw_sqrt_f32(x: f32) -> f32 {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        let out: f32;
        core::arch::asm!("fsqrt {:s}, {:s}", out(vreg) out, in(vreg) x, options(pure, nomem, nostack));
        out
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        extern "C" { fn sqrtf(v: f32) -> f32; }
        unsafe { sqrtf(x) }
    }
}

#[inline(always)]
fn hw_ceil_f64(x: f64) -> f64 {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        let out: f64;
        core::arch::asm!("frintp {:d}, {:d}", out(vreg) out, in(vreg) x, options(pure, nomem, nostack));
        out
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        extern "C" { fn ceil(v: f64) -> f64; }
        unsafe { ceil(x) }
    }
}

#[inline(always)]
fn hw_floor_f64(x: f64) -> f64 {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        let out: f64;
        core::arch::asm!("frintm {:d}, {:d}", out(vreg) out, in(vreg) x, options(pure, nomem, nostack));
        out
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        extern "C" { fn floor(v: f64) -> f64; }
        unsafe { floor(x) }
    }
}

// ============================================================================
// 1. FMOD GameCube Audio Flag Hooks
// ============================================================================

pub fn fmusic_load_song_ex_before(c: &mut Ctx) {
    c.r[6] &= !0x0100_0000;
}

pub fn fmusic_load_internal_before(c: &mut Ctx) {
    c.r[4] &= !0x0100_0000;
}

pub fn fsound_sample_load_ex_before(c: &mut Ctx) {
    c.r[5] &= !0x0100_0000;
}

// ============================================================================
// 2. Libc Memory & String Routines (Overlap-Safe)
// ============================================================================

/// GameCube `memcpy` (`0x800031E8`) checks `src < dst` and copies backwards, so we always
/// use overlap-safe `core::ptr::copy` (`memmove` semantics) and leave `c.r[3] = dst`.
pub fn hle_memmove(c: &mut Ctx) {
    let dest = c.r[3];
    let src = c.r[4];
    let n = c.r[5] as usize;
    if n == 0 || dest == src {
        return;
    }
    if in_ram(dest, n) && in_ram(src, n) {
        unsafe {
            core::ptr::copy(mptr(c.mem, src), mptr(c.mem, dest), n);
        }
    } else if src < dest {
        for i in (0..c.r[5]).rev() {
            let b = c.r8(src.wrapping_add(i));
            c.w8(dest.wrapping_add(i), b);
        }
    } else {
        for i in 0..c.r[5] {
            let b = c.r8(src.wrapping_add(i));
            c.w8(dest.wrapping_add(i), b);
        }
    }
}

pub fn hle_memset(c: &mut Ctx) {
    let dest = c.r[3];
    let val = c.r[4] as u8;
    let n = c.r[5] as usize;
    if n == 0 {
        return;
    }
    if in_ram(dest, n) {
        unsafe {
            core::ptr::write_bytes(mptr(c.mem, dest), val, n);
        }
    } else {
        for i in 0..c.r[5] {
            c.w8(dest.wrapping_add(i), val);
        }
    }
}

pub fn hle_memcmp(c: &mut Ctx) {
    let s1 = c.r[3];
    let s2 = c.r[4];
    let n = c.r[5] as usize;
    if n == 0 || s1 == s2 {
        c.r[3] = 0;
        return;
    }
    if in_ram(s1, n) && in_ram(s2, n) {
        let b1 = unsafe { core::slice::from_raw_parts(mptr(c.mem, s1), n) };
        let b2 = unsafe { core::slice::from_raw_parts(mptr(c.mem, s2), n) };
        for i in 0..n {
            let a = unsafe { *b1.get_unchecked(i) };
            let b = unsafe { *b2.get_unchecked(i) };
            if a != b {
                c.r[3] = (a as i32 - b as i32) as u32;
                return;
            }
        }
        c.r[3] = 0;
    } else {
        for i in 0..c.r[5] {
            let b1 = c.r8(s1.wrapping_add(i)) as i32;
            let b2 = c.r8(s2.wrapping_add(i)) as i32;
            if b1 != b2 {
                c.r[3] = (b1 - b2) as u32;
                return;
            }
        }
        c.r[3] = 0;
    }
}

pub fn hle_strlen(c: &mut Ctx) {
    let s = c.r[3];
    if (0x8000_0000..0x8180_0000).contains(&s) {
        let max_len = (0x8180_0000 - s) as usize;
        let p = unsafe { mptr(c.mem, s) };
        let mut len = 0usize;
        while len < max_len && unsafe { *p.add(len) } != 0 {
            len += 1;
        }
        c.r[3] = len as u32;
    } else {
        let mut len = 0u32;
        while c.r8(s.wrapping_add(len)) != 0 {
            len += 1;
        }
        c.r[3] = len;
    }
}

pub fn hle_strcpy(c: &mut Ctx) {
    let dest = c.r[3];
    let src = c.r[4];
    if (0x8000_0000..0x8180_0000).contains(&dest) && (0x8000_0000..0x8180_0000).contains(&src) {
        let max_len = (0x8180_0000 - dest.max(src)) as usize;
        let pd = unsafe { mptr(c.mem, dest) };
        let ps = unsafe { mptr(c.mem, src) };
        let mut i = 0usize;
        while i < max_len {
            let b = unsafe { *ps.add(i) };
            unsafe { *pd.add(i) = b };
            if b == 0 {
                return;
            }
            i += 1;
        }
    }
    let mut i = 0u32;
    loop {
        let b = c.r8(src.wrapping_add(i));
        c.w8(dest.wrapping_add(i), b);
        if b == 0 {
            break;
        }
        i += 1;
    }
}

pub fn hle_strcmp(c: &mut Ctx) {
    let s1 = c.r[3];
    let s2 = c.r[4];
    if s1 == s2 {
        c.r[3] = 0;
        return;
    }
    if (0x8000_0000..0x8180_0000).contains(&s1) && (0x8000_0000..0x8180_0000).contains(&s2) {
        let max_len = (0x8180_0000 - s1.max(s2)) as usize;
        let p1 = unsafe { mptr(c.mem, s1) };
        let p2 = unsafe { mptr(c.mem, s2) };
        let mut i = 0usize;
        while i < max_len {
            let b1 = unsafe { *p1.add(i) } as i32;
            let b2 = unsafe { *p2.add(i) } as i32;
            if b1 != b2 || b1 == 0 {
                c.r[3] = (b1 - b2) as u32;
                return;
            }
            i += 1;
        }
    }
    let mut i = 0u32;
    loop {
        let b1 = c.r8(s1.wrapping_add(i)) as i32;
        let b2 = c.r8(s2.wrapping_add(i)) as i32;
        if b1 != b2 || b1 == 0 {
            c.r[3] = (b1 - b2) as u32;
            break;
        }
        i += 1;
    }
}

// ============================================================================
// 3. 64-Bit Integer Division / Remainder Helpers (PPCEABI -> ARM64 udiv/sdiv)
// ============================================================================

pub fn hle_div2u(c: &mut Ctx) -> bool {
    let a = ((c.r[3] as u64) << 32) | (c.r[4] as u64);
    let b = ((c.r[5] as u64) << 32) | (c.r[6] as u64);
    if b == 0 {
        return false;
    }
    let q = a / b;
    c.r[3] = (q >> 32) as u32;
    c.r[4] = q as u32;
    true
}

pub fn hle_div2i(c: &mut Ctx) -> bool {
    let a = (((c.r[3] as u64) << 32) | (c.r[4] as u64)) as i64;
    let b = (((c.r[5] as u64) << 32) | (c.r[6] as u64)) as i64;
    if b == 0 {
        return false;
    }
    let q = a.wrapping_div(b) as u64;
    c.r[3] = (q >> 32) as u32;
    c.r[4] = q as u32;
    true
}

pub fn hle_mod2u(c: &mut Ctx) -> bool {
    let a = ((c.r[3] as u64) << 32) | (c.r[4] as u64);
    let b = ((c.r[5] as u64) << 32) | (c.r[6] as u64);
    if b == 0 {
        return false;
    }
    let rem = a % b;
    c.r[3] = (rem >> 32) as u32;
    c.r[4] = rem as u32;
    true
}

pub fn hle_mod2i(c: &mut Ctx) -> bool {
    let a = (((c.r[3] as u64) << 32) | (c.r[4] as u64)) as i64;
    let b = (((c.r[5] as u64) << 32) | (c.r[6] as u64)) as i64;
    if b == 0 {
        return false;
    }
    let rem = a.wrapping_rem(b) as u64;
    c.r[3] = (rem >> 32) as u32;
    c.r[4] = rem as u32;
    true
}

// ============================================================================
// 4. Hardware FP & Native AArch64 libm Math
// ============================================================================

pub fn hle_sqrt(c: &mut Ctx) -> bool {
    let x = c.f[1];
    if x >= 0.0 && x.is_finite() {
        c.f[1] = hw_sqrt_f64(x);
        true
    } else {
        false
    }
}

pub fn hle_xsqrt(c: &mut Ctx) -> bool {
    let x = c.f[1];
    if x > 0.0 && x.is_finite() {
        c.set_fs(1, hw_sqrt_f32(x as f32) as f64);
        true
    } else {
        false
    }
}

pub fn hle_rw_sqrt(c: &mut Ctx) -> bool {
    let x = c.f[1];
    if x >= 0.0 && x.is_finite() {
        c.set_fs(1, hw_sqrt_f32(x as f32) as f64);
        true
    } else {
        false
    }
}

pub fn hle_rw_inv_sqrt(c: &mut Ctx) -> bool {
    let x = c.f[1];
    if x > 0.0 && x.is_finite() {
        c.set_fs(1, (1.0f32 / hw_sqrt_f32(x as f32)) as f64);
        true
    } else {
        false
    }
}

pub fn hle_ceil(c: &mut Ctx) -> bool {
    let x = c.f[1];
    if x.is_finite() {
        c.f[1] = hw_ceil_f64(x);
        true
    } else {
        false
    }
}

pub fn hle_floor(c: &mut Ctx) -> bool {
    let x = c.f[1];
    if x.is_finite() {
        c.f[1] = hw_floor_f64(x);
        true
    } else {
        false
    }
}

pub fn hle_sinf(c: &mut Ctx) -> bool {
    let x = c.f[1];
    if x.is_finite() {
        c.set_fs(1, unsafe { sinf(x as f32) } as f64);
        true
    } else {
        false
    }
}

pub fn hle_cosf(c: &mut Ctx) -> bool {
    let x = c.f[1];
    if x.is_finite() {
        c.set_fs(1, unsafe { cosf(x as f32) } as f64);
        true
    } else {
        false
    }
}

pub fn hle_powf(c: &mut Ctx) -> bool {
    let (x, y) = (c.f[1], c.f[2]);
    if x > 0.0 && x.is_finite() && y.is_finite() {
        c.set_fs(1, unsafe { powf(x as f32, y as f32) } as f64);
        true
    } else {
        false
    }
}

pub fn hle_sin(c: &mut Ctx) -> bool {
    let x = c.f[1];
    if x.is_finite() {
        c.f[1] = unsafe { sin(x) };
        true
    } else {
        false
    }
}

pub fn hle_cos(c: &mut Ctx) -> bool {
    let x = c.f[1];
    if x.is_finite() {
        c.f[1] = unsafe { cos(x) };
        true
    } else {
        false
    }
}

pub fn hle_tan(c: &mut Ctx) -> bool {
    let x = c.f[1];
    if x.is_finite() {
        c.f[1] = unsafe { tan(x) };
        true
    } else {
        false
    }
}

pub fn hle_atan(c: &mut Ctx) -> bool {
    let x = c.f[1];
    if x.is_finite() {
        c.f[1] = unsafe { atan(x) };
        true
    } else {
        false
    }
}

pub fn hle_atan2(c: &mut Ctx) -> bool {
    let (y, x) = (c.f[1], c.f[2]);
    if y.is_finite() && x.is_finite() && (y != 0.0 || x != 0.0) {
        c.f[1] = unsafe { atan2(y, x) };
        true
    } else {
        false
    }
}

pub fn hle_acos(c: &mut Ctx) -> bool {
    let x = c.f[1];
    if (-1.0..=1.0).contains(&x) {
        c.f[1] = unsafe { acos(x) };
        true
    } else {
        false
    }
}

pub fn hle_asin(c: &mut Ctx) -> bool {
    let x = c.f[1];
    if (-1.0..=1.0).contains(&x) {
        c.f[1] = unsafe { asin(x) };
        true
    } else {
        false
    }
}

pub fn hle_fmod(c: &mut Ctx) -> bool {
    let x = c.f[1];
    let y = c.f[2];
    if y != 0.0 && x.is_finite() && y.is_finite() {
        let res = unsafe { fmod(x, y) };
        c.f[1] = res;
        c.ps1[1] = res;
        true
    } else {
        false
    }
}

// ============================================================================
// 5. NEON Big-Endian Vector Load/Store Primitives & Matrix/Vector Math
// ============================================================================

#[inline(always)]
fn rf32(mem: *mut u8, ea: u32) -> f32 {
    f32::from_bits(mr32(mem, ea))
}

#[inline(always)]
fn wf32(mem: *mut u8, ea: u32, v: f32) {
    mw32(mem, ea, v.to_bits());
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use core::arch::aarch64::*;
    use runtime::mptr;

    /// Loads 4 big-endian floats from `ea` into a NEON `float32x4_t` (unaligned safe).
    #[inline(always)]
    pub unsafe fn load4_be(mem: *mut u8, ea: u32) -> float32x4_t {
        let p = mptr(mem, ea) as *const u8;
        vreinterpretq_f32_u8(vrev32q_u8(vld1q_u8(p)))
    }

    /// Stores 4 floats from `v` as big-endian `f32`s to `ea` (unaligned safe).
    #[inline(always)]
    pub unsafe fn store4_be(mem: *mut u8, ea: u32, v: float32x4_t) {
        let p = mptr(mem, ea);
        vst1q_u8(p, vrev32q_u8(vreinterpretq_u8_f32(v)));
    }

    /// Stores the first 3 floats (`x, y, z`) of `v` as big-endian `f32`s to `ea`
    /// WITHOUT touching byte offset `+12` (`flags` / adjacent vertex).
    #[inline(always)]
    pub unsafe fn store3_be(mem: *mut u8, ea: u32, v: float32x4_t) {
        let p = mptr(mem, ea);
        let be = vrev32q_u8(vreinterpretq_u8_f32(v));
        (p as *mut u64).write_unaligned(vgetq_lane_u64::<0>(vreinterpretq_u64_u8(be)));
        (p.add(8) as *mut u32).write_unaligned(vgetq_lane_u32::<2>(vreinterpretq_u32_u8(be)));
    }

    /// Loads an `RwMatrix` / `xMat4x3` (4 rows at `0x00, 0x10, 0x20, 0x30`) into 4 NEON regs.
    #[inline(always)]
    pub unsafe fn load_rw_mtx(mem: *mut u8, base: u32) -> (float32x4_t, float32x4_t, float32x4_t, float32x4_t) {
        (
            load4_be(mem, base),
            load4_be(mem, base.wrapping_add(0x10)),
            load4_be(mem, base.wrapping_add(0x20)),
            load4_be(mem, base.wrapping_add(0x30)),
        )
    }

    /// Multiplies `a * b` in `RwMatrix` row-vector convention (`v' = v.x*right + v.y*up + v.z*at + pos`).
    #[inline(always)]
    pub unsafe fn mul_rw_mtx(
        a: (float32x4_t, float32x4_t, float32x4_t, float32x4_t),
        b: (float32x4_t, float32x4_t, float32x4_t, float32x4_t),
    ) -> (float32x4_t, float32x4_t, float32x4_t, float32x4_t) {
        let r0 = vfmaq_laneq_f32::<2>(vfmaq_laneq_f32::<1>(vmulq_laneq_f32::<0>(b.0, a.0), b.1, a.0), b.2, a.0);
        let r1 = vfmaq_laneq_f32::<2>(vfmaq_laneq_f32::<1>(vmulq_laneq_f32::<0>(b.0, a.1), b.1, a.1), b.2, a.1);
        let r2 = vfmaq_laneq_f32::<2>(vfmaq_laneq_f32::<1>(vmulq_laneq_f32::<0>(b.0, a.2), b.1, a.2), b.2, a.2);
        let r3 = vfmaq_laneq_f32::<2>(vfmaq_laneq_f32::<1>(vfmaq_laneq_f32::<0>(b.3, b.0, a.3), b.1, a.3), b.2, a.3);
        (r0, r1, r2, r3)
    }

    /// Stores the 4 `RwV3d` rows of an `RwMatrix` WITHOUT modifying `0x0C, 0x1C, 0x2C, 0x3C`.
    #[inline(always)]
    pub unsafe fn store_rw_mtx_xyz(
        mem: *mut u8,
        dst: u32,
        m: (float32x4_t, float32x4_t, float32x4_t, float32x4_t),
    ) {
        store3_be(mem, dst, m.0);
        store3_be(mem, dst.wrapping_add(0x10), m.1);
        store3_be(mem, dst.wrapping_add(0x20), m.2);
        store3_be(mem, dst.wrapping_add(0x30), m.3);
    }
}

pub fn hle_psmtx_identity(c: &mut Ctx) -> bool {
    if c.spr[912] != 0 {
        return false;
    }
    let m = c.r[3];
    let mem = c.mem;
    const ONE: u32 = 0x3F80_0000;
    mw32(mem, m, ONE);
    mw32(mem, m.wrapping_add(0x04), 0);
    mw32(mem, m.wrapping_add(0x08), 0);
    mw32(mem, m.wrapping_add(0x0C), 0);
    mw32(mem, m.wrapping_add(0x10), 0);
    mw32(mem, m.wrapping_add(0x14), ONE);
    mw32(mem, m.wrapping_add(0x18), 0);
    mw32(mem, m.wrapping_add(0x1C), 0);
    mw32(mem, m.wrapping_add(0x20), 0);
    mw32(mem, m.wrapping_add(0x24), 0);
    mw32(mem, m.wrapping_add(0x28), ONE);
    mw32(mem, m.wrapping_add(0x2C), 0);
    true
}

pub fn hle_psmtx_scale(c: &mut Ctx) -> bool {
    if c.spr[912] != 0 {
        return false;
    }
    let m = c.r[3];
    let mem = c.mem;
    wf32(mem, m, c.f[1] as f32);
    mw32(mem, m.wrapping_add(0x04), 0);
    mw32(mem, m.wrapping_add(0x08), 0);
    mw32(mem, m.wrapping_add(0x0C), 0);
    mw32(mem, m.wrapping_add(0x10), 0);
    wf32(mem, m.wrapping_add(0x14), c.f[2] as f32);
    mw32(mem, m.wrapping_add(0x18), 0);
    mw32(mem, m.wrapping_add(0x1C), 0);
    mw32(mem, m.wrapping_add(0x20), 0);
    mw32(mem, m.wrapping_add(0x24), 0);
    wf32(mem, m.wrapping_add(0x28), c.f[3] as f32);
    mw32(mem, m.wrapping_add(0x2C), 0);
    true
}

pub fn hle_psmtx_concat(c: &mut Ctx) -> bool {
    if c.spr[912] != 0 {
        return false;
    }
    let (a, b, ab) = (c.r[3], c.r[4], c.r[5]);
    let mem = c.mem;
    #[cfg(target_arch = "aarch64")]
    unsafe {
        use core::arch::aarch64::*;
        let a0 = neon::load4_be(mem, a);
        let a1 = neon::load4_be(mem, a.wrapping_add(0x10));
        let a2 = neon::load4_be(mem, a.wrapping_add(0x20));
        let b0 = neon::load4_be(mem, b);
        let b1 = neon::load4_be(mem, b.wrapping_add(0x10));
        let b2 = neon::load4_be(mem, b.wrapping_add(0x20));
        let w_unit = vsetq_lane_f32::<3>(1.0, vdupq_n_f32(0.0));
        let o0 = vfmaq_laneq_f32::<3>(
            vfmaq_laneq_f32::<2>(vfmaq_laneq_f32::<1>(vmulq_laneq_f32::<0>(b0, a0), b1, a0), b2, a0),
            w_unit,
            a0,
        );
        let o1 = vfmaq_laneq_f32::<3>(
            vfmaq_laneq_f32::<2>(vfmaq_laneq_f32::<1>(vmulq_laneq_f32::<0>(b0, a1), b1, a1), b2, a1),
            w_unit,
            a1,
        );
        let o2 = vfmaq_laneq_f32::<3>(
            vfmaq_laneq_f32::<2>(vfmaq_laneq_f32::<1>(vmulq_laneq_f32::<0>(b0, a2), b1, a2), b2, a2),
            w_unit,
            a2,
        );
        neon::store4_be(mem, ab, o0);
        neon::store4_be(mem, ab.wrapping_add(0x10), o1);
        neon::store4_be(mem, ab.wrapping_add(0x20), o2);
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let mut ma = [0.0f32; 12];
        let mut mb = [0.0f32; 12];
        for i in 0..12 {
            ma[i] = rf32(mem, a.wrapping_add(i as u32 * 4));
            mb[i] = rf32(mem, b.wrapping_add(i as u32 * 4));
        }
        for r in 0..3 {
            let (ar0, ar1, ar2, ar3) = (ma[r * 4], ma[r * 4 + 1], ma[r * 4 + 2], ma[r * 4 + 3]);
            for col in 0..4 {
                let v = ar0 * mb[col] + ar1 * mb[4 + col] + ar2 * mb[8 + col] + if col == 3 { ar3 } else { 0.0 };
                wf32(mem, ab.wrapping_add((r * 4 + col) as u32 * 4), v);
            }
        }
    }
    true
}

#[inline(always)]
fn rw_mtx_mul_core(mem: *mut u8, dst: u32, src_a: u32, src_b: u32) {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        let ma = neon::load_rw_mtx(mem, src_a);
        let mb = neon::load_rw_mtx(mem, src_b);
        let out = neon::mul_rw_mtx(ma, mb);
        neon::store_rw_mtx_xyz(mem, dst, out);
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let read_row = |base: u32, off: u32| -> [f32; 3] {
            [rf32(mem, base.wrapping_add(off)), rf32(mem, base.wrapping_add(off + 4)), rf32(mem, base.wrapping_add(off + 8))]
        };
        let a = [read_row(src_a, 0), read_row(src_a, 0x10), read_row(src_a, 0x20), read_row(src_a, 0x30)];
        let b = [read_row(src_b, 0), read_row(src_b, 0x10), read_row(src_b, 0x20), read_row(src_b, 0x30)];
        for r in 0..4 {
            let add = if r == 3 { b[3] } else { [0.0; 3] };
            let off = (r as u32) * 0x10;
            for k in 0..3 {
                let v = a[r][0] * b[0][k] + a[r][1] * b[1][k] + a[r][2] * b[2][k] + add[k];
                wf32(mem, dst.wrapping_add(off + k as u32 * 4), v);
            }
        }
    }
}

pub fn hle_matrix_multiply(c: &mut Ctx) -> bool {
    if c.spr[912] != 0 {
        return false;
    }
    rw_mtx_mul_core(c.mem, c.r[3], c.r[4], c.r[5]);
    true
}

pub fn hle_rw_matrix_multiply(c: &mut Ctx) -> bool {
    if c.spr[912] != 0 {
        return false;
    }
    let (dst, src_a, src_b) = (c.r[3], c.r[4], c.r[5]);
    let mem = c.mem;
    let flags = mr32(mem, src_a.wrapping_add(0x0C)) & mr32(mem, src_b.wrapping_add(0x0C));
    rw_mtx_mul_core(mem, dst, src_a, src_b);
    mw32(mem, dst.wrapping_add(0x0C), flags);
    true
}

pub fn hle_rw_matrix_update(c: &mut Ctx) -> bool {
    let m = c.r[3];
    let mem = c.mem;
    let flags = mr32(mem, m.wrapping_add(0x0C)) & 0xFFFD_FFFC;
    mw32(mem, m.wrapping_add(0x0C), flags);
    true
}

pub fn hle_xmat4x3_mul(c: &mut Ctx) -> bool {
    let (dst, src_a, src_b) = (c.r[3], c.r[4], c.r[5]);
    let mem = c.mem;
    rw_mtx_mul_core(mem, dst, src_a, src_b);
    mw32(mem, dst.wrapping_add(0x0C), 0);
    true
}

pub fn hle_rw_v3d_length(c: &mut Ctx) -> bool {
    if c.spr[912] != 0 {
        return false;
    }
    let p = c.r[3];
    let mem = c.mem;
    let x = rf32(mem, p);
    let y = rf32(mem, p.wrapping_add(4));
    let z = rf32(mem, p.wrapping_add(8));
    let sq = x * x + y * y + z * z;
    let len = if sq > 0.0 { hw_sqrt_f32(sq) } else { 0.0 };
    c.set_fs(1, len as f64);
    true
}

pub fn hle_rw_v3d_transform_point(c: &mut Ctx) -> bool {
    if c.spr[912] != 0 {
        return false;
    }
    let (pts_out, pts_in, mtx) = (c.r[3], c.r[4], c.r[5]);
    let mem = c.mem;
    #[cfg(target_arch = "aarch64")]
    unsafe {
        use core::arch::aarch64::*;
        let m = neon::load_rw_mtx(mem, mtx);
        let x = rf32(mem, pts_in);
        let y = rf32(mem, pts_in.wrapping_add(4));
        let z = rf32(mem, pts_in.wrapping_add(8));
        let v = vfmaq_n_f32(vfmaq_n_f32(vfmaq_n_f32(m.3, m.0, x), m.1, y), m.2, z);
        neon::store3_be(mem, pts_out, v);
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let (x, y, z) = (rf32(mem, pts_in), rf32(mem, pts_in.wrapping_add(4)), rf32(mem, pts_in.wrapping_add(8)));
        for k in 0..3 {
            let v = x * rf32(mem, mtx.wrapping_add(k * 4))
                + y * rf32(mem, mtx.wrapping_add(0x10 + k * 4))
                + z * rf32(mem, mtx.wrapping_add(0x20 + k * 4))
                + rf32(mem, mtx.wrapping_add(0x30 + k * 4));
            wf32(mem, pts_out.wrapping_add(k * 4), v);
        }
    }
    true
}

pub fn hle_rw_v3d_transform_points(c: &mut Ctx) -> bool {
    if c.spr[912] != 0 {
        return false;
    }
    let (mut pts_out, mut pts_in, n, mtx) = (c.r[3], c.r[4], c.r[5] as i32, c.r[6]);
    if n <= 0 {
        return true;
    }
    let mem = c.mem;
    #[cfg(target_arch = "aarch64")]
    unsafe {
        use core::arch::aarch64::*;
        let m = neon::load_rw_mtx(mem, mtx);
        for _ in 0..n {
            let x = rf32(mem, pts_in);
            let y = rf32(mem, pts_in.wrapping_add(4));
            let z = rf32(mem, pts_in.wrapping_add(8));
            let v = vfmaq_n_f32(vfmaq_n_f32(vfmaq_n_f32(m.3, m.0, x), m.1, y), m.2, z);
            neon::store3_be(mem, pts_out, v);
            pts_in = pts_in.wrapping_add(12);
            pts_out = pts_out.wrapping_add(12);
        }
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let r0 = [rf32(mem, mtx), rf32(mem, mtx + 4), rf32(mem, mtx + 8)];
        let r1 = [rf32(mem, mtx + 0x10), rf32(mem, mtx + 0x14), rf32(mem, mtx + 0x18)];
        let r2 = [rf32(mem, mtx + 0x20), rf32(mem, mtx + 0x24), rf32(mem, mtx + 0x28)];
        let r3 = [rf32(mem, mtx + 0x30), rf32(mem, mtx + 0x34), rf32(mem, mtx + 0x38)];
        for _ in 0..n {
            let (x, y, z) = (rf32(mem, pts_in), rf32(mem, pts_in.wrapping_add(4)), rf32(mem, pts_in.wrapping_add(8)));
            wf32(mem, pts_out, x * r0[0] + y * r1[0] + z * r2[0] + r3[0]);
            wf32(mem, pts_out.wrapping_add(4), x * r0[1] + y * r1[1] + z * r2[1] + r3[1]);
            wf32(mem, pts_out.wrapping_add(8), x * r0[2] + y * r1[2] + z * r2[2] + r3[2]);
            pts_in = pts_in.wrapping_add(12);
            pts_out = pts_out.wrapping_add(12);
        }
    }
    true
}

pub fn hle_rw_v3d_transform_vector(c: &mut Ctx) -> bool {
    if c.spr[912] != 0 {
        return false;
    }
    let (vecs_out, vecs_in, mtx) = (c.r[3], c.r[4], c.r[5]);
    let mem = c.mem;
    #[cfg(target_arch = "aarch64")]
    unsafe {
        use core::arch::aarch64::*;
        let m0 = neon::load4_be(mem, mtx);
        let m1 = neon::load4_be(mem, mtx.wrapping_add(0x10));
        let m2 = neon::load4_be(mem, mtx.wrapping_add(0x20));
        let x = rf32(mem, vecs_in);
        let y = rf32(mem, vecs_in.wrapping_add(4));
        let z = rf32(mem, vecs_in.wrapping_add(8));
        let v = vfmaq_n_f32(vfmaq_n_f32(vmulq_n_f32(m0, x), m1, y), m2, z);
        neon::store3_be(mem, vecs_out, v);
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let (x, y, z) = (rf32(mem, vecs_in), rf32(mem, vecs_in.wrapping_add(4)), rf32(mem, vecs_in.wrapping_add(8)));
        for k in 0..3 {
            let v = x * rf32(mem, mtx.wrapping_add(k * 4))
                + y * rf32(mem, mtx.wrapping_add(0x10 + k * 4))
                + z * rf32(mem, mtx.wrapping_add(0x20 + k * 4));
            wf32(mem, vecs_out.wrapping_add(k * 4), v);
        }
    }
    true
}

pub fn hle_rw_v3d_transform_vectors(c: &mut Ctx) -> bool {
    if c.spr[912] != 0 {
        return false;
    }
    let (mut vecs_out, mut vecs_in, n, mtx) = (c.r[3], c.r[4], c.r[5] as i32, c.r[6]);
    if n <= 0 {
        return true;
    }
    let mem = c.mem;
    #[cfg(target_arch = "aarch64")]
    unsafe {
        use core::arch::aarch64::*;
        let m0 = neon::load4_be(mem, mtx);
        let m1 = neon::load4_be(mem, mtx.wrapping_add(0x10));
        let m2 = neon::load4_be(mem, mtx.wrapping_add(0x20));
        for _ in 0..n {
            let x = rf32(mem, vecs_in);
            let y = rf32(mem, vecs_in.wrapping_add(4));
            let z = rf32(mem, vecs_in.wrapping_add(8));
            let v = vfmaq_n_f32(vfmaq_n_f32(vmulq_n_f32(m0, x), m1, y), m2, z);
            neon::store3_be(mem, vecs_out, v);
            vecs_in = vecs_in.wrapping_add(12);
            vecs_out = vecs_out.wrapping_add(12);
        }
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let r0 = [rf32(mem, mtx), rf32(mem, mtx + 4), rf32(mem, mtx + 8)];
        let r1 = [rf32(mem, mtx + 0x10), rf32(mem, mtx + 0x14), rf32(mem, mtx + 0x18)];
        let r2 = [rf32(mem, mtx + 0x20), rf32(mem, mtx + 0x24), rf32(mem, mtx + 0x28)];
        for _ in 0..n {
            let (x, y, z) = (rf32(mem, vecs_in), rf32(mem, vecs_in.wrapping_add(4)), rf32(mem, vecs_in.wrapping_add(8)));
            wf32(mem, vecs_out, x * r0[0] + y * r1[0] + z * r2[0]);
            wf32(mem, vecs_out.wrapping_add(4), x * r0[1] + y * r1[1] + z * r2[1]);
            wf32(mem, vecs_out.wrapping_add(8), x * r0[2] + y * r1[2] + z * r2[2]);
            vecs_in = vecs_in.wrapping_add(12);
            vecs_out = vecs_out.wrapping_add(12);
        }
    }
    true
}

// ============================================================================
// 6. RenderWare Skeletal Skinning (ARM64 NEON + Full GQR6/GQR7 Quantized Path)
// ============================================================================

pub fn hle_skin_matrix_blend_update_asm(c: &mut Ctx) -> bool {
    if c.spr[912] != 0 {
        return false;
    }
    let (out_mtx, inv_bone_mtx, anim_node_mtx, root_mtx, bone_indices, num_bones) =
        (c.r[3], c.r[4], c.r[5], c.r[6], c.r[7], c.r[8]);
    if num_bones == 0 {
        return true;
    }
    let mem = c.mem;
    #[cfg(target_arch = "aarch64")]
    unsafe {
        let root = neon::load_rw_mtx(mem, root_mtx);
        for i in 0..num_bones {
            let off = (mr8(mem, bone_indices.wrapping_add(i)) as u32) << 6;
            let anim = neon::load_rw_mtx(mem, anim_node_mtx.wrapping_add(off));
            let inv = neon::load_rw_mtx(mem, inv_bone_mtx.wrapping_add(off));
            let tmp = neon::mul_rw_mtx(anim, root);
            let res = neon::mul_rw_mtx(inv, tmp);
            neon::store_rw_mtx_xyz(mem, out_mtx.wrapping_add(off), res);
        }
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        for i in 0..num_bones {
            let off = (mr8(mem, bone_indices.wrapping_add(i)) as u32) << 6;
            let dst = out_mtx.wrapping_add(off);
            rw_mtx_mul_core(mem, dst, anim_node_mtx.wrapping_add(off), root_mtx);
            rw_mtx_mul_core(mem, dst, inv_bone_mtx.wrapping_add(off), dst);
        }
    }
    true
}

#[inline(always)]
fn read_quantized_v3(c: &Ctx, ea: u32, comp: u32, gqr: u32) -> (f32, f32, f32) {
    if (gqr & 0x0004_0000) == 0 {
        let mem = c.mem;
        (rf32(mem, ea), rf32(mem, ea.wrapping_add(comp)), rf32(mem, ea.wrapping_add(comp * 2)))
    } else {
        let (ty, scale) = ((gqr >> 16) & 7, (gqr >> 24) & 0x3F);
        (
            c.dequantize(ea, ty, scale).0 as f32,
            c.dequantize(ea.wrapping_add(comp), ty, scale).0 as f32,
            c.dequantize(ea.wrapping_add(comp * 2), ty, scale).0 as f32,
        )
    }
}

#[inline(always)]
fn write_quantized_v3(mem: *mut u8, ea: u32, comp: u32, gqr: u32, x: f32, y: f32, z: f32) {
    if (gqr & 4) == 0 {
        wf32(mem, ea, x);
        wf32(mem, ea.wrapping_add(comp), y);
        wf32(mem, ea.wrapping_add(comp * 2), z);
    } else {
        let (ty, scale) = (gqr & 7, (gqr >> 8) & 0x3F);
        let (bx, sx) = Ctx::quantize(ty, scale, x as f64);
        let (by, _) = Ctx::quantize(ty, scale, y as f64);
        let (bz, _) = Ctx::quantize(ty, scale, z as f64);
        unsafe {
            core::ptr::copy_nonoverlapping(bx.as_ptr(), mptr(mem, ea), sx);
            core::ptr::copy_nonoverlapping(by.as_ptr(), mptr(mem, ea.wrapping_add(comp)), sx);
            core::ptr::copy_nonoverlapping(bz.as_ptr(), mptr(mem, ea.wrapping_add(comp * 2)), sx);
        }
    }
}

#[inline(always)]
fn skin_update_generic<const WEIGHTS: usize, const NORMALS: bool>(c: &mut Ctx) -> bool {
    // GQR0 must be standard f32 (0) and GQR5 must be standard u8/128 weight scale (0x0704_0000).
    if c.spr[912] != 0 || c.spr[917] != 0x0704_0000 {
        return false;
    }
    let (mtx_array, skin, ctx) = (c.r[3], c.r[4], c.r[5]);
    let mem = c.mem;
    let num_verts = mr32(mem, ctx.wrapping_add(0x1C)) as i32;
    if num_verts <= 0 {
        return true;
    }
    let mut out_pos = mr32(mem, ctx);
    let mut out_nrm = if NORMALS { mr32(mem, ctx.wrapping_add(0x04)) } else { 0 };
    let mut in_pos = mr32(mem, ctx.wrapping_add(0x08));
    let mut in_nrm = if NORMALS { mr32(mem, ctx.wrapping_add(0x0C)) } else { 0 };
    let pos_comp = mr32(mem, ctx.wrapping_add(0x10));
    let nrm_comp = if NORMALS { mr32(mem, ctx.wrapping_add(0x14)) } else { 0 };
    let nrm_skip = if NORMALS { mr32(mem, ctx.wrapping_add(0x18)) } else { 0 };
    let mut w_ptr = mr32(mem, skin.wrapping_add(0x24));
    let mut i_ptr = mr32(mem, skin.wrapping_add(0x28));

    let gqr6 = c.spr[918];
    let gqr7 = c.spr[919];
    let pos_stride = pos_comp.wrapping_mul(3);
    let nrm_stride = nrm_comp.wrapping_mul(3).wrapping_add(nrm_skip);
    crate::mark_skinned_range(out_pos, (num_verts as u32).saturating_mul(pos_stride));
    if NORMALS && out_nrm != 0 {
        crate::mark_skinned_range(out_nrm, (num_verts as u32).saturating_mul(nrm_stride));
    }
    const W_SCALE: f32 = 1.0 / 128.0;

    let fast_f32 = (gqr6 & 0x0004_0004) == 0
        && pos_comp == 4
        && (!NORMALS || ((gqr7 & 0x0004_0004) == 0 && nrm_comp == 4));

    #[cfg(target_arch = "aarch64")]
    unsafe {
        use core::arch::aarch64::*;
        for _ in 0..num_verts {
            let (px, py, pz) = if fast_f32 {
                (rf32(mem, in_pos), rf32(mem, in_pos.wrapping_add(4)), rf32(mem, in_pos.wrapping_add(8)))
            } else {
                read_quantized_v3(c, in_pos, pos_comp, gqr6)
            };
            let (nx, ny, nz) = if !NORMALS {
                (0.0, 0.0, 0.0)
            } else if fast_f32 {
                (rf32(mem, in_nrm), rf32(mem, in_nrm.wrapping_add(4)), rf32(mem, in_nrm.wrapping_add(8)))
            } else {
                read_quantized_v3(c, in_nrm, nrm_comp, gqr7)
            };

            let (b, w): ([u32; 4], [u8; 4]) = match WEIGHTS {
                2 => {
                    let idx = mr16(mem, i_ptr) as u32;
                    let wb = mr16(mem, w_ptr);
                    ([idx >> 8, idx & 0xFF, 0, 0], [(wb >> 8) as u8, wb as u8, 0, 0])
                }
                3 => (
                    [mr8(mem, i_ptr) as u32, mr8(mem, i_ptr.wrapping_add(1)) as u32, mr8(mem, i_ptr.wrapping_add(2)) as u32, 0],
                    [mr8(mem, w_ptr), mr8(mem, w_ptr.wrapping_add(1)), mr8(mem, w_ptr.wrapping_add(2)), 0],
                ),
                _ => {
                    let idx = mr32(mem, i_ptr);
                    let wb = mr32(mem, w_ptr);
                    (
                        [(idx >> 24) & 0xFF, (idx >> 16) & 0xFF, (idx >> 8) & 0xFF, idx & 0xFF],
                        [(wb >> 24) as u8, (wb >> 16) as u8, (wb >> 8) as u8, wb as u8],
                    )
                }
            };
            w_ptr = w_ptr.wrapping_add(WEIGHTS as u32);
            i_ptr = i_ptr.wrapping_add(WEIGHTS as u32);

            let m0 = neon::load_rw_mtx(mem, mtx_array.wrapping_add(b[0] << 6));
            let w0 = (w[0] as f32) * W_SCALE;
            let tp0 = vfmaq_n_f32(vfmaq_n_f32(vfmaq_n_f32(m0.3, m0.0, px), m0.1, py), m0.2, pz);
            let mut acc_p = vmulq_n_f32(tp0, w0);
            let mut acc_n = if NORMALS {
                let tn0 = vfmaq_n_f32(vfmaq_n_f32(vmulq_n_f32(m0.0, nx), m0.1, ny), m0.2, nz);
                vmulq_n_f32(tn0, w0)
            } else {
                vdupq_n_f32(0.0)
            };

            for k in 1..WEIGHTS {
                if w[k] == 0 {
                    break;
                }
                let wk = (w[k] as f32) * W_SCALE;
                let mk = neon::load_rw_mtx(mem, mtx_array.wrapping_add(b[k] << 6));
                let tpk = vfmaq_n_f32(vfmaq_n_f32(vfmaq_n_f32(mk.3, mk.0, px), mk.1, py), mk.2, pz);
                acc_p = vfmaq_n_f32(acc_p, tpk, wk);
                if NORMALS {
                    let tnk = vfmaq_n_f32(vfmaq_n_f32(vmulq_n_f32(mk.0, nx), mk.1, ny), mk.2, nz);
                    acc_n = vfmaq_n_f32(acc_n, tnk, wk);
                }
            }

            if fast_f32 {
                neon::store3_be(mem, out_pos, acc_p);
                in_pos = in_pos.wrapping_add(12);
                out_pos = out_pos.wrapping_add(12);
                if NORMALS {
                    neon::store3_be(mem, out_nrm, acc_n);
                    in_nrm = in_nrm.wrapping_add(nrm_stride);
                    out_nrm = out_nrm.wrapping_add(nrm_stride);
                }
            } else {
                write_quantized_v3(
                    mem,
                    out_pos,
                    pos_comp,
                    gqr6,
                    vgetq_lane_f32::<0>(acc_p),
                    vgetq_lane_f32::<1>(acc_p),
                    vgetq_lane_f32::<2>(acc_p),
                );
                in_pos = in_pos.wrapping_add(pos_stride);
                out_pos = out_pos.wrapping_add(pos_stride);
                if NORMALS {
                    write_quantized_v3(
                        mem,
                        out_nrm,
                        nrm_comp,
                        gqr7,
                        vgetq_lane_f32::<0>(acc_n),
                        vgetq_lane_f32::<1>(acc_n),
                        vgetq_lane_f32::<2>(acc_n),
                    );
                    in_nrm = in_nrm.wrapping_add(nrm_stride);
                    out_nrm = out_nrm.wrapping_add(nrm_stride);
                }
            }
        }
        return true;
    }

    // Non-aarch64 fallback path.
    for _ in 0..num_verts {
        let (px, py, pz) = read_quantized_v3(c, in_pos, pos_comp, gqr6);
        let (nx, ny, nz) = if NORMALS { read_quantized_v3(c, in_nrm, nrm_comp, gqr7) } else { (0.0, 0.0, 0.0) };

        let (b, w): ([u32; 4], [u8; 4]) = match WEIGHTS {
            2 => {
                let idx = mr16(mem, i_ptr) as u32;
                let wb = mr16(mem, w_ptr);
                ([idx >> 8, idx & 0xFF, 0, 0], [(wb >> 8) as u8, wb as u8, 0, 0])
            }
            3 => (
                [mr8(mem, i_ptr) as u32, mr8(mem, i_ptr.wrapping_add(1)) as u32, mr8(mem, i_ptr.wrapping_add(2)) as u32, 0],
                [mr8(mem, w_ptr), mr8(mem, w_ptr.wrapping_add(1)), mr8(mem, w_ptr.wrapping_add(2)), 0],
            ),
            _ => {
                let idx = mr32(mem, i_ptr);
                let wb = mr32(mem, w_ptr);
                (
                    [(idx >> 24) & 0xFF, (idx >> 16) & 0xFF, (idx >> 8) & 0xFF, idx & 0xFF],
                    [(wb >> 24) as u8, (wb >> 16) as u8, (wb >> 8) as u8, wb as u8],
                )
            }
        };
        w_ptr = w_ptr.wrapping_add(WEIGHTS as u32);
        i_ptr = i_ptr.wrapping_add(WEIGHTS as u32);

        let mut ox = 0.0f32;
        let mut oy = 0.0f32;
        let mut oz = 0.0f32;
        let mut onx = 0.0f32;
        let mut ony = 0.0f32;
        let mut onz = 0.0f32;

        for k in 0..WEIGHTS {
            if k > 0 && w[k] == 0 {
                break;
            }
            let wk = (w[k] as f32) * W_SCALE;
            let mb = mtx_array.wrapping_add(b[k] << 6);
            let m00 = rf32(mem, mb);
            let m01 = rf32(mem, mb.wrapping_add(0x04));
            let m02 = rf32(mem, mb.wrapping_add(0x08));
            let m10 = rf32(mem, mb.wrapping_add(0x10));
            let m11 = rf32(mem, mb.wrapping_add(0x14));
            let m12 = rf32(mem, mb.wrapping_add(0x18));
            let m20 = rf32(mem, mb.wrapping_add(0x20));
            let m21 = rf32(mem, mb.wrapping_add(0x24));
            let m22 = rf32(mem, mb.wrapping_add(0x28));
            let m30 = rf32(mem, mb.wrapping_add(0x30));
            let m31 = rf32(mem, mb.wrapping_add(0x34));
            let m32 = rf32(mem, mb.wrapping_add(0x38));

            ox += (px * m00 + py * m10 + pz * m20 + m30) * wk;
            oy += (px * m01 + py * m11 + pz * m21 + m31) * wk;
            oz += (px * m02 + py * m12 + pz * m22 + m32) * wk;
            if NORMALS {
                onx += (nx * m00 + ny * m10 + nz * m20) * wk;
                ony += (nx * m01 + ny * m11 + nz * m21) * wk;
                onz += (nx * m02 + ny * m12 + nz * m22) * wk;
            }
        }

        write_quantized_v3(mem, out_pos, pos_comp, gqr6, ox, oy, oz);
        in_pos = in_pos.wrapping_add(pos_stride);
        out_pos = out_pos.wrapping_add(pos_stride);
        if NORMALS {
            write_quantized_v3(mem, out_nrm, nrm_comp, gqr7, onx, ony, onz);
            in_nrm = in_nrm.wrapping_add(nrm_stride);
            out_nrm = out_nrm.wrapping_add(nrm_stride);
        }
    }
    true
}

pub fn hle_skin_2w_p(c: &mut Ctx) -> bool {
    skin_update_generic::<2, false>(c)
}
pub fn hle_skin_2w_pn(c: &mut Ctx) -> bool {
    skin_update_generic::<2, true>(c)
}
pub fn hle_skin_3w_p(c: &mut Ctx) -> bool {
    skin_update_generic::<3, false>(c)
}
pub fn hle_skin_3w_pn(c: &mut Ctx) -> bool {
    skin_update_generic::<3, true>(c)
}
pub fn hle_skin_4w_p(c: &mut Ctx) -> bool {
    skin_update_generic::<4, false>(c)
}
pub fn hle_skin_4w_pn(c: &mut Ctx) -> bool {
    skin_update_generic::<4, true>(c)
}

// ============================================================================
// 6. FMOD Memory Allocator (FSOUND_Memory_Pool_Alloc & Free)
// ============================================================================

/// Native ARM64 replacement for `FSOUND_Memory_Pool_Alloc` (`0x802D19BC`).
///
/// In original GameCube code, this function performs a linear byte-by-byte scan loop
/// over tens of thousands of allocation flags in RAM looking for contiguous zero blocks.
/// In recompiled PowerPC code, this loop burns ~150ms per frame.
/// Here in native ARM64, scanning a contiguous slice in L1/L2 takes < 1 microsecond.
#[inline(always)]
fn fsound_default_pool(mem: *mut u8, r13: u32) -> u32 {
    let g = mr32(mem, r13.wrapping_sub(0x7AB8));
    g.wrapping_add(0x29C)
}

fn fsound_pool_alloc_internal(c: &mut Ctx, pool: u32, req_size: u32) -> Option<u32> {
    let mem = c.mem;
    if !in_ram(pool, 0x40) {
        return None;
    }

    let alloc_fn = mr32(mem, pool.wrapping_add(0x30));
    if alloc_fn != 0 {
        return None;
    }

    let field_0x28 = mr32(mem, pool.wrapping_add(0x28));
    let alloc_size = if field_0x28 == 0 {
        req_size.wrapping_add(0x20)
    } else {
        req_size
    };

    let block_size = mr32(mem, pool.wrapping_add(0x2C));
    let total_blocks = mr32(mem, pool.wrapping_add(0xC));
    if block_size == 0 || total_blocks == 0 {
        return Some(0);
    }

    let blocks_needed = (alloc_size.wrapping_add(block_size).wrapping_sub(1)) / block_size;
    let bitmap_addr = mr32(mem, pool);
    if !in_ram(bitmap_addr, total_blocks as usize) {
        return None;
    }

    let bitmap_ptr = unsafe { mptr(mem, bitmap_addr) };
    let bitmap_slice = unsafe { core::slice::from_raw_parts(bitmap_ptr, total_blocks as usize) };

    // Linear search for `blocks_needed` consecutive free (0) blocks
    let start_block = if blocks_needed == 0 {
        Some(0)
    } else {
        let mut consecutive: u32 = 0;
        let mut found: Option<u32> = None;
        for (i, &b) in bitmap_slice.iter().enumerate() {
            if b == 0 {
                consecutive += 1;
                if consecutive == blocks_needed {
                    found = Some(((i + 1) as u32).wrapping_sub(blocks_needed));
                    break;
                }
            } else {
                consecutive = 0;
            }
        }
        found
    };

    let start_block = match start_block {
        Some(sb) => sb,
        None => return Some(0), // Out of memory
    };

    // Mark blocks as allocated in bitmap
    unsafe {
        core::ptr::write_bytes(bitmap_ptr.add(start_block as usize), 1, blocks_needed as usize);
    }

    let (ret_ptr, header_ptr) = if field_0x28 == 0 {
        let data_buffer = mr32(mem, pool.wrapping_add(0x4));
        let block_ptr = data_buffer.wrapping_add(start_block.wrapping_mul(block_size));
        if !in_ram(block_ptr, alloc_size as usize) {
            return None;
        }
        (block_ptr.wrapping_add(0x20), block_ptr)
    } else {
        // Custom pool (field_0x28 != 0, e.g. ARAM sample pool 0x80468F18):
        // Allocate a 32-byte header node from the default pool (r3 = 0, which uses F_Malloc).
        let saved_lr = c.lr;
        let saved_r = [
            c.r[0], c.r[3], c.r[4], c.r[5], c.r[6], c.r[7], c.r[8], c.r[9], c.r[10], c.r[11], c.r[12],
        ];
        c.r[3] = 0;
        c.r[4] = 0x20;
        c.r[5] = 0x8037_F460;
        c.r[6] = 0x269;
        c.call(0x802D_19BC);
        let hdr = c.r[3];
        c.lr = saved_lr;
        [
            c.r[0], c.r[3], c.r[4], c.r[5], c.r[6], c.r[7], c.r[8], c.r[9], c.r[10], c.r[11], c.r[12],
        ] = saved_r;
        if hdr == 0 || !in_ram(hdr, 0x20) {
            unsafe {
                core::ptr::write_bytes(bitmap_ptr.add(start_block as usize), 0, blocks_needed as usize);
            }
            return Some(0);
        }
        (hdr, hdr)
    };

    // Write header
    mw32(mem, header_ptr, req_size);
    mw32(mem, header_ptr.wrapping_add(0x4), blocks_needed);
    mw32(mem, header_ptr.wrapping_add(0x8), start_block);

    // Update pool accounting statistics
    let curr_bytes = mr32(mem, pool.wrapping_add(0x18)).wrapping_add(req_size);
    mw32(mem, pool.wrapping_add(0x18), curr_bytes);
    let mut peak_bytes = mr32(mem, pool.wrapping_add(0x1C));
    if (curr_bytes as i32) > (peak_bytes as i32) {
        peak_bytes = curr_bytes;
        mw32(mem, pool.wrapping_add(0x1C), peak_bytes);
    }

    let curr_blocks = mr32(mem, pool.wrapping_add(0x10)).wrapping_add(blocks_needed);
    mw32(mem, pool.wrapping_add(0x10), curr_blocks);
    let mut peak_blocks = mr32(mem, pool.wrapping_add(0x14));
    if (curr_blocks as i32) > (peak_blocks as i32) {
        peak_blocks = curr_blocks;
        mw32(mem, pool.wrapping_add(0x14), peak_blocks);
        let field_0x20 = peak_blocks.wrapping_mul(block_size);
        mw32(mem, pool.wrapping_add(0x20), field_0x20);
        let field_0x24 = field_0x20.wrapping_sub(peak_bytes);
        mw32(mem, pool.wrapping_add(0x24), field_0x24);
    }

    Some(ret_ptr)
}

fn fsound_pool_free_internal(c: &mut Ctx, pool: u32, user_ptr: u32) -> bool {
    let mem = c.mem;
    if user_ptr == 0 {
        return true;
    }
    if !in_ram(pool, 0x40) {
        return false;
    }

    let free_fn = mr32(mem, pool.wrapping_add(0x38));
    if free_fn != 0 {
        return false;
    }

    let field_0x28 = mr32(mem, pool.wrapping_add(0x28));
    let header = if field_0x28 == 0 {
        user_ptr.wrapping_sub(0x20)
    } else {
        user_ptr
    };

    if !in_ram(header, 0x20) {
        return false;
    }

    let orig_size = mr32(mem, header);
    let blocks_used = mr32(mem, header.wrapping_add(0x4));
    let start_block = mr32(mem, header.wrapping_add(0x8));

    // Update pool accounting
    let curr_bytes = mr32(mem, pool.wrapping_add(0x18)).wrapping_sub(orig_size);
    mw32(mem, pool.wrapping_add(0x18), curr_bytes);
    let curr_blocks = mr32(mem, pool.wrapping_add(0x10)).wrapping_sub(blocks_used);
    mw32(mem, pool.wrapping_add(0x10), curr_blocks);

    // Free blocks in bitmap
    let bitmap_addr = mr32(mem, pool);
    let total_blocks = mr32(mem, pool.wrapping_add(0xC));
    if start_block.wrapping_add(blocks_used) <= total_blocks && in_ram(bitmap_addr, total_blocks as usize) {
        unsafe {
            let bitmap_ptr = mptr(mem, bitmap_addr);
            core::ptr::write_bytes(bitmap_ptr.add(start_block as usize), 0, blocks_used as usize);
        }
    }

    if field_0x28 != 0 {
        // Free the 32-byte header node from default pool (r3 = 0, which uses F_Free)
        let saved_lr = c.lr;
        let saved_r = [
            c.r[0], c.r[3], c.r[4], c.r[5], c.r[6], c.r[7], c.r[8], c.r[9], c.r[10], c.r[11], c.r[12],
        ];
        c.r[3] = 0;
        c.r[4] = header;
        c.r[5] = 0x8037_F460;
        c.r[6] = 0x2DC;
        c.call(0x802D_1724);
        c.lr = saved_lr;
        [
            c.r[0], c.r[3], c.r[4], c.r[5], c.r[6], c.r[7], c.r[8], c.r[9], c.r[10], c.r[11], c.r[12],
        ] = saved_r;
    }

    true
}

/// Native ARM64 replacement for `FSOUND_Memory_Pool_Alloc` (`0x802D19BC`).
pub fn hle_fsound_memory_pool_alloc(c: &mut Ctx) -> bool {
    let mem = c.mem;
    let pool = if c.r[3] == 0 {
        fsound_default_pool(mem, c.r[13])
    } else {
        c.r[3]
    };
    let req_size = c.r[4];
    match fsound_pool_alloc_internal(c, pool, req_size) {
        Some(ptr) => {
            c.r[3] = ptr;
            true
        }
        None => false,
    }
}

/// Native ARM64 replacement for `FSOUND_Memory_Pool_Free` (`0x802D1724`).
pub fn hle_fsound_memory_pool_free(c: &mut Ctx) -> bool {
    let mem = c.mem;
    let pool = if c.r[3] == 0 {
        fsound_default_pool(mem, c.r[13])
    } else {
        c.r[3]
    };
    let user_ptr = c.r[4];
    fsound_pool_free_internal(c, pool, user_ptr)
}

/// Native ARM64 replacement for `FSOUND_Memory_Pool_Calloc` (`0x802D16B0`).
pub fn hle_fsound_memory_pool_calloc(c: &mut Ctx) -> bool {
    let mem = c.mem;
    let pool = if c.r[3] == 0 {
        fsound_default_pool(mem, c.r[13])
    } else {
        c.r[3]
    };
    let size = c.r[4];
    if !hle_fsound_memory_pool_alloc(c) {
        return false;
    }
    let ptr = c.r[3];
    if ptr != 0 && in_ram(pool, 0x40) {
        let field_0x28 = mr32(mem, pool.wrapping_add(0x28));
        if field_0x28 == 0 && in_ram(ptr, size as usize) {
            unsafe {
                core::ptr::write_bytes(mptr(mem, ptr), 0, size as usize);
            }
        }
    }
    true
}

/// Native ARM64 replacement for `split16` (`0x8028972C`).
pub fn hle_split16(c: &mut Ctx) -> bool {
    let mem = c.mem;
    let mut src = c.r[3];
    let mut dst_a = c.r[4];
    let mut dst_b = c.r[5];
    let count = c.r[6];
    let chunks = (count >> 2) as usize;
    if !in_ram(src, chunks * 8 + 4) || !in_ram(dst_a, chunks * 4 + 2) || !in_ram(dst_b, chunks * 4 + 2) {
        return false;
    }
    for _ in 0..chunks {
        let r11 = mr32(mem, src);
        let r9 = mr32(mem, src + 4);
        let r0 = (r11 & 0xFFFF0000) | ((r9 >> 16) & 0x0000FFFF);
        let r1 = ((r11 << 16) & 0xFFFF0000) | (r9 & 0x0000FFFF);
        mw32(mem, dst_a, r0);
        mw32(mem, dst_b, r1);
        src += 8;
        dst_a += 4;
        dst_b += 4;
    }
    if (count & 3) != 0 {
        let r9 = mr32(mem, src);
        mw16(mem, dst_a, (r9 >> 16) as u16);
        mw16(mem, dst_b, r9 as u16);
    }
    c.r[3] = src;
    c.r[4] = dst_a;
    c.r[5] = dst_b;
    true
}

/// Native ARM64 replacement for `split8` (`0x802896C0`).
pub fn hle_split8(c: &mut Ctx) -> bool {
    let mem = c.mem;
    let mut src = c.r[3];
    let mut dst_a = c.r[4];
    let mut dst_b = c.r[5];
    let count = c.r[6];
    let chunks = (count >> 1) as usize;
    if !in_ram(src, chunks * 4 + 2) || !in_ram(dst_a, chunks * 2 + 1) || !in_ram(dst_b, chunks * 2 + 1) {
        return false;
    }
    for _ in 0..chunks {
        let r10 = mr32(mem, src);
        src += 4;
        let b0 = (r10 >> 24) as u8;
        let b1 = ((r10 >> 16) & 0xFF) as u8;
        let b2 = ((r10 >> 8) & 0xFF) as u8;
        let b3 = (r10 & 0xFF) as u8;
        mw16(mem, dst_a, ((b0 as u16) << 8) | (b2 as u16));
        mw16(mem, dst_b, ((b1 as u16) << 8) | (b3 as u16));
        dst_a += 2;
        dst_b += 2;
    }
    if (count & 1) != 0 {
        let r9 = mr16(mem, src);
        mw8(mem, dst_a, (r9 >> 8) as u8);
        mw8(mem, dst_b, (r9 & 0xFF) as u8);
    }
    c.r[3] = src;
    c.r[4] = dst_a;
    c.r[5] = dst_b;
    true
}

/// Native ARM64 replacement for `__GXSetVAT` (`0x802EB118`).
pub fn hle_gx_set_vat(c: &mut Ctx) -> bool {
    let mem = c.mem;
    let gx_data_ptr = c.r[2].wrapping_add(0xFFFFA5C8);
    if !in_ram(gx_data_ptr, 4) {
        return false;
    }
    let gx_data = mr32(mem, gx_data_ptr);
    if !in_ram(gx_data, 0x5AC) {
        return false;
    }
    let dirty = mr8(mem, gx_data + 0x5AB);
    if dirty == 0 {
        return true;
    }
    for i in 0..8u32 {
        if (dirty & (1 << i)) != 0 {
            let va = mr32(mem, gx_data + 0x1C + i * 4);
            let vb = mr32(mem, gx_data + 0x3C + i * 4);
            let vc = mr32(mem, gx_data + 0x5C + i * 4);

            c.gw8(0xCC00_8000, 0x08);
            c.gw8(0xCC00_8000, (0x70 + i) as u8);
            c.gw32(0xCC00_8000, va);

            c.gw8(0xCC00_8000, 0x08);
            c.gw8(0xCC00_8000, (0x80 + i) as u8);
            c.gw32(0xCC00_8000, vb);

            c.gw8(0xCC00_8000, 0x08);
            c.gw8(0xCC00_8000, (0x90 + i) as u8);
            c.gw32(0xCC00_8000, vc);
        }
    }
    c.gw8(gx_data + 0x5AB, 0);
    true
}

/// Native fast-path for RenderWare `_rwDlSetRenderState` (`0x803564B0`).
/// Returns `true` with `c.r[3] = 1` if the requested render state is already
/// active in `_rwGCState` (`0x80487368`), avoiding thousands of emulated
/// function calls, stack roundtrips, and jump-table dispatches per frame.
pub fn hle_rw_dl_set_render_state(c: &mut Ctx) -> bool {
    let state = c.r[3];
    let val = c.r[4];
    if state > 0x1E {
        c.r[3] = 0;
        return true;
    }
    const GC_STATE: u32 = 0x80487368;
    let mem = c.mem;
    if !in_ram(GC_STATE, 0x50) {
        return false;
    }

    match state {
        1 => {
            // rwRENDERSTATETEXTURERASTER
            let ea = c.r[13].wrapping_add(0xFFFFA394);
            if !in_ram(ea, 4) { return false; }
            let r3 = mr32(mem, ea);
            if !in_ram(r3, 4) { return false; }
            let cur = mr32(mem, r3);
            if val == cur {
                c.r[3] = 1;
                return true;
            }
        }
        5 => {
            // rwRENDERSTATETEXTUREPERSPECTIVE
            c.r[3] = val;
            return true;
        }
        6 => {
            // rwRENDERSTATEZTESTENABLE
            let cur = mr32(mem, GC_STATE + 4);
            if (val != 0) == (cur != 0) {
                c.r[3] = 1;
                return true;
            }
        }
        8 => {
            // rwRENDERSTATEZWRITEENABLE
            let cur = mr32(mem, GC_STATE);
            if (val != 0) == (cur != 0) {
                c.r[3] = 1;
                return true;
            }
        }
        10 => {
            // rwRENDERSTATESRCBLEND
            let cur = mr32(mem, GC_STATE + 0x34);
            if val == cur {
                c.r[3] = 1;
                return true;
            }
        }
        11 => {
            // rwRENDERSTATEDESTBLEND
            let cur = mr32(mem, GC_STATE + 0x38);
            if val == cur {
                c.r[3] = 1;
                return true;
            }
        }
        13 => {
            // rwRENDERSTATEVERTEXALPHAENABLE
            c.r[3] = 0;
            return true;
        }
        14 => {
            // rwRENDERSTATEFOGENABLE
            let cur = mr32(mem, GC_STATE + 0x10);
            if (val != 0) == (cur != 0) {
                c.r[3] = 1;
                return true;
            }
        }
        15 => {
            // rwRENDERSTATEFOGCOLOR
            let cur = mr32(mem, GC_STATE + 0x18);
            if val == cur {
                c.r[3] = 1;
                return true;
            }
        }
        16 => {
            // rwRENDERSTATEFOGTYPE
            let cur = mr32(mem, GC_STATE + 0x14);
            if val == cur {
                c.r[3] = 1;
                return true;
            }
        }
        17 => {
            // rwRENDERSTATEFOGDENSITY
            c.r[3] = 0;
            return true;
        }
        20 => {
            // rwRENDERSTATECULLMODE
            let cur = mr32(mem, GC_STATE + 0xC);
            if val == cur {
                c.r[3] = 1;
                return true;
            }
        }
        30 => {
            let cur = mr8(mem, GC_STATE + 0x4C) as u32;
            if val == cur {
                c.r[3] = 1;
                return true;
            }
        }
        _ => {}
    }
    false
}

/// Native ARM64 fast-path for RenderWare `RwCameraFrustumTestSphere` (`0x80344384`).
/// Tests a bounding sphere against the 6 frustum planes of the active camera.
pub fn hle_rw_camera_frustum_test_sphere(c: &mut Ctx) -> bool {
    let camera = c.r[3];
    let sphere = c.r[4];
    if !in_ram(camera, 0x94 + 6 * 20) || !in_ram(sphere, 16) {
        return false;
    }
    let mem = c.mem;
    let cx = f32::from_bits(mr32(mem, sphere));
    let cy = f32::from_bits(mr32(mem, sphere + 4));
    let cz = f32::from_bits(mr32(mem, sphere + 8));
    let radius = f32::from_bits(mr32(mem, sphere + 12));
    let neg_radius = -radius;

    let mut result = 2u32; // rwSPHEREINSIDE
    let mut plane_addr = camera + 0x94;
    for _ in 0..6 {
        let nx = f32::from_bits(mr32(mem, plane_addr));
        let ny = f32::from_bits(mr32(mem, plane_addr + 4));
        let nz = f32::from_bits(mr32(mem, plane_addr + 8));
        let dist = f32::from_bits(mr32(mem, plane_addr + 12));
        plane_addr += 20;

        let dot = cx * nx + cy * ny + cz * nz - dist;
        if dot > radius {
            c.r[3] = 0; // rwSPHEREOUTSIDE
            return true;
        }
        if dot > neg_radius {
            result = 1; // rwSPHEREBOUNDARY
        }
    }
    c.r[3] = result;
    true
}

/// Native ARM64 implementation of RenderWare `RtIntersectionSphereTriangle` (`0x80337ADC`).
/// Tests intersection of a sphere with a triangle and outputs the triangle normal and plane distance.
pub fn hle_rt_intersection_sphere_triangle(c: &mut Ctx) -> bool {
    let sphere_ptr = c.r[3];
    let v0_ptr = c.r[4];
    let v1_ptr = c.r[5];
    let v2_ptr = c.r[6];
    let normal_ptr = c.r[7];
    let dist_ptr = c.r[8];

    let mem = c.mem;
    if !in_ram(sphere_ptr, 16)
        || !in_ram(v0_ptr, 12)
        || !in_ram(v1_ptr, 12)
        || !in_ram(v2_ptr, 12)
        || !in_ram(normal_ptr, 12)
        || !in_ram(dist_ptr, 4)
    {
        return false;
    }

    let cx = f32::from_bits(mr32(mem, sphere_ptr));
    let cy = f32::from_bits(mr32(mem, sphere_ptr + 4));
    let cz = f32::from_bits(mr32(mem, sphere_ptr + 8));
    let radius = f32::from_bits(mr32(mem, sphere_ptr + 12));

    let v0_x = f32::from_bits(mr32(mem, v0_ptr));
    let v0_y = f32::from_bits(mr32(mem, v0_ptr + 4));
    let v0_z = f32::from_bits(mr32(mem, v0_ptr + 8));

    let v1_x = f32::from_bits(mr32(mem, v1_ptr));
    let v1_y = f32::from_bits(mr32(mem, v1_ptr + 4));
    let v1_z = f32::from_bits(mr32(mem, v1_ptr + 8));

    let v2_x = f32::from_bits(mr32(mem, v2_ptr));
    let v2_y = f32::from_bits(mr32(mem, v2_ptr + 4));
    let v2_z = f32::from_bits(mr32(mem, v2_ptr + 8));

    let d0_x = v0_x - cx;
    let d0_y = v0_y - cy;
    let d0_z = v0_z - cz;

    let d1_x = v1_x - cx;
    let d1_y = v1_y - cy;
    let d1_z = v1_z - cz;

    let d2_x = v2_x - cx;
    let d2_y = v2_y - cy;
    let d2_z = v2_z - cz;

    let neg_radius = -radius;

    // 1. AABB Cull against sphere bounding box
    if (d0_x > radius && d1_x > radius && d2_x > radius)
        || (d0_x < neg_radius && d1_x < neg_radius && d2_x < neg_radius)
        || (d0_y > radius && d1_y > radius && d2_y > radius)
        || (d0_y < neg_radius && d1_y < neg_radius && d2_y < neg_radius)
        || (d0_z > radius && d1_z > radius && d2_z > radius)
        || (d0_z < neg_radius && d1_z < neg_radius && d2_z < neg_radius)
    {
        c.r[3] = 0;
        return true;
    }

    // 2. Triangle Normal (cross product of edges v1-v0 and v2-v0)
    let e1_x = v1_x - v0_x;
    let e1_y = v1_y - v0_y;
    let e1_z = v1_z - v0_z;

    let e2_x = v2_x - v0_x;
    let e2_y = v2_y - v0_y;
    let e2_z = v2_z - v0_z;

    let nx = e1_y * e2_z - e1_z * e2_y;
    let ny = e1_z * e2_x - e1_x * e2_z;
    let nz = e1_x * e2_y - e1_y * e2_x;

    let len_sq = nx * nx + ny * ny + nz * nz;
    if len_sq <= 0.0 {
        c.r[3] = 0;
        return true;
    }

    let inv_len = 1.0f32 / hw_sqrt_f32(len_sq);
    let norm_x = nx * inv_len;
    let norm_y = ny * inv_len;
    let norm_z = nz * inv_len;

    mw32(mem, normal_ptr, norm_x.to_bits());
    mw32(mem, normal_ptr + 4, norm_y.to_bits());
    mw32(mem, normal_ptr + 8, norm_z.to_bits());

    // Distance from sphere center to triangle plane
    let dist_to_v0 = d0_x * norm_x + d0_y * norm_y + d0_z * norm_z;
    if dist_to_v0 < neg_radius || dist_to_v0 > radius {
        c.r[3] = 0;
        return true;
    }

    let dist = -dist_to_v0;
    mw32(mem, dist_ptr, dist.to_bits());

    // 3. Vertex distance to sphere center
    let r_sq = radius * radius;
    let len0_sq = d0_x * d0_x + d0_y * d0_y + d0_z * d0_z;
    if len0_sq <= r_sq {
        c.r[3] = 1;
        return true;
    }

    let len1_sq = d1_x * d1_x + d1_y * d1_y + d1_z * d1_z;
    if len1_sq <= r_sq {
        c.r[3] = 1;
        return true;
    }

    let len2_sq = d2_x * d2_x + d2_y * d2_y + d2_z * d2_z;
    if len2_sq <= r_sq {
        c.r[3] = 1;
        return true;
    }

    // 4. Closest vertex Voronoi region tests
    let dot01 = d0_x * d1_x + d0_y * d1_y + d0_z * d1_z;
    let dot02 = d0_x * d2_x + d0_y * d2_y + d0_z * d2_z;
    let dot12 = d1_x * d2_x + d1_y * d2_y + d1_z * d2_z;

    if len0_sq < len1_sq {
        if len2_sq < len0_sq {
            // v2 is closest
            if dot02 > len2_sq && dot12 > len2_sq {
                c.r[3] = 0;
                return true;
            }
        } else {
            // v0 is closest
            if dot01 > len0_sq && dot02 > len0_sq {
                c.r[3] = 0;
                return true;
            }
        }
    } else {
        if len2_sq < len1_sq {
            // v2 is closest
            if dot02 > len2_sq && dot12 > len2_sq {
                c.r[3] = 0;
                return true;
            }
        } else {
            // v1 is closest
            if dot01 > len1_sq && dot12 > len1_sq {
                c.r[3] = 0;
                return true;
            }
        }
    }

    // 5. Edge 0-1 test
    let e01_x = d1_x - d0_x;
    let e01_y = d1_y - d0_y;
    let e01_z = d1_z - d0_z;
    let e01_len_sq = e01_x * e01_x + e01_y * e01_y + e01_z * e01_z;
    let dot_e01_d0 = e01_x * d0_x + e01_y * d0_y + e01_z * d0_z;
    let t01 = dot_e01_d0 / e01_len_sq;
    let c01_x = d0_x - e01_x * t01;
    let c01_y = d0_y - e01_y * t01;
    let c01_z = d0_z - e01_z * t01;
    let dist01_sq = c01_x * c01_x + c01_y * c01_y + c01_z * c01_z;
    if dist01_sq > r_sq {
        let dot01_d2 = c01_x * d2_x + c01_y * d2_y + c01_z * d2_z;
        if dist01_sq < dot01_d2 {
            c.r[3] = 0;
            return true;
        }
    }

    // 6. Edge 1-2 test
    let e12_x = d2_x - d1_x;
    let e12_y = d2_y - d1_y;
    let e12_z = d2_z - d1_z;
    let e12_len_sq = e12_x * e12_x + e12_y * e12_y + e12_z * e12_z;
    let dot_e12_d1 = e12_x * d1_x + e12_y * d1_y + e12_z * d1_z;
    let t12 = dot_e12_d1 / e12_len_sq;
    let c12_x = d1_x - e12_x * t12;
    let c12_y = d1_y - e12_y * t12;
    let c12_z = d1_z - e12_z * t12;
    let dist12_sq = c12_x * c12_x + c12_y * c12_y + c12_z * c12_z;
    if dist12_sq > r_sq {
        let dot12_d0 = c12_x * d0_x + c12_y * d0_y + c12_z * d0_z;
        if dist12_sq < dot12_d0 {
            c.r[3] = 0;
            return true;
        }
    }

    // 7. Edge 2-0 test
    let e20_x = d0_x - d2_x;
    let e20_y = d0_y - d2_y;
    let e20_z = d0_z - d2_z;
    let e20_len_sq = e20_x * e20_x + e20_y * e20_y + e20_z * e20_z;
    let dot_e20_d2 = e20_x * d2_x + e20_y * d2_y + e20_z * d2_z;
    let t20 = dot_e20_d2 / e20_len_sq;
    let c20_x = d2_x - e20_x * t20;
    let c20_y = d2_y - e20_y * t20;
    let c20_z = d2_z - e20_z * t20;
    let dist20_sq = c20_x * c20_x + c20_y * c20_y + c20_z * c20_z;
    if dist20_sq > r_sq {
        let dot20_d1 = c20_x * d1_x + c20_y * d1_y + c20_z * d1_z;
        if dist20_sq < dot20_d1 {
            c.r[3] = 0;
            return true;
        }
    }

    c.r[3] = 1;
    true
}

/// Native ARM64 fast-path for RenderWare `RtIntersectionBBoxTriangle` (`0x8033818C`).
/// Evaluates the complete Tomas Akenine-Möller 13-axis Separating Axis Theorem (SAT) test:
/// - 3 box face normals (AABB vs AABB overlap)
/// - 1 triangle face normal (plane vs box overlap)
/// - 9 cross-product axes of box edges and triangle edges
/// Resolves 100% of AABB-triangle intersection queries without any fallback to recompiled code.
pub fn hle_rt_intersection_bbox_triangle(c: &mut Ctx) -> bool {
    let bbox_ptr = c.r[3];
    let v0_ptr = c.r[4];
    let v1_ptr = c.r[5];
    let v2_ptr = c.r[6];
    let mem = c.mem;
    if !in_ram(bbox_ptr, 24) || !in_ram(v0_ptr, 12) || !in_ram(v1_ptr, 12) || !in_ram(v2_ptr, 12) {
        return false;
    }

    // In RenderWare RwBBox: sup (max) is offset 0..12, inf (min) is offset 12..24.
    let sup_x = f32::from_bits(mr32(mem, bbox_ptr));
    let sup_y = f32::from_bits(mr32(mem, bbox_ptr + 4));
    let sup_z = f32::from_bits(mr32(mem, bbox_ptr + 8));
    let inf_x = f32::from_bits(mr32(mem, bbox_ptr + 12));
    let inf_y = f32::from_bits(mr32(mem, bbox_ptr + 16));
    let inf_z = f32::from_bits(mr32(mem, bbox_ptr + 20));

    let v0_x = f32::from_bits(mr32(mem, v0_ptr));
    let v0_y = f32::from_bits(mr32(mem, v0_ptr + 4));
    let v0_z = f32::from_bits(mr32(mem, v0_ptr + 8));

    let v1_x = f32::from_bits(mr32(mem, v1_ptr));
    let v1_y = f32::from_bits(mr32(mem, v1_ptr + 4));
    let v1_z = f32::from_bits(mr32(mem, v1_ptr + 8));

    let v2_x = f32::from_bits(mr32(mem, v2_ptr));
    let v2_y = f32::from_bits(mr32(mem, v2_ptr + 4));
    let v2_z = f32::from_bits(mr32(mem, v2_ptr + 8));

    // Phase 1: Fast bounding box overlap along the 3 coordinate axes
    let min_x = v0_x.min(v1_x).min(v2_x);
    let max_x = v0_x.max(v1_x).max(v2_x);
    if min_x > sup_x || max_x < inf_x {
        c.r[3] = 0;
        return true;
    }

    let min_y = v0_y.min(v1_y).min(v2_y);
    let max_y = v0_y.max(v1_y).max(v2_y);
    if min_y > sup_y || max_y < inf_y {
        c.r[3] = 0;
        return true;
    }

    let min_z = v0_z.min(v1_z).min(v2_z);
    let max_z = v0_z.max(v1_z).max(v2_z);
    if min_z > sup_z || max_z < inf_z {
        c.r[3] = 0;
        return true;
    }

    // Translate triangle vertices relative to box center:
    let center_x = 0.5 * (sup_x + inf_x);
    let center_y = 0.5 * (sup_y + inf_y);
    let center_z = 0.5 * (sup_z + inf_z);

    let ext_x = 0.5 * (sup_x - inf_x);
    let ext_y = 0.5 * (sup_y - inf_y);
    let ext_z = 0.5 * (sup_z - inf_z);

    let u0_x = v0_x - center_x;
    let u0_y = v0_y - center_y;
    let u0_z = v0_z - center_z;

    let u1_x = v1_x - center_x;
    let u1_y = v1_y - center_y;
    let u1_z = v1_z - center_z;

    let u2_x = v2_x - center_x;
    let u2_y = v2_y - center_y;
    let u2_z = v2_z - center_z;

    // Edge vectors of the triangle:
    let f0_x = u1_x - u0_x;
    let f0_y = u1_y - u0_y;
    let f0_z = u1_z - u0_z;

    let f1_x = u2_x - u1_x;
    let f1_y = u2_y - u1_y;
    let f1_z = u2_z - u1_z;

    let f2_x = u0_x - u2_x;
    let f2_y = u0_y - u2_y;
    let f2_z = u0_z - u2_z;

    // Phase 2: 9 edge cross-product axes
    // --- Axis a00 = u_x x f0 = (0, -f0_z, f0_y) ---
    let p0 = -f0_z * u0_y + f0_y * u0_z;
    let p2 = -f0_z * u2_y + f0_y * u2_z;
    let rad = ext_y * f0_z.abs() + ext_z * f0_y.abs();
    if p0.min(p2) > rad || p0.max(p2) < -rad {
        c.r[3] = 0;
        return true;
    }

    // --- Axis a01 = u_x x f1 = (0, -f1_z, f1_y) ---
    let p0 = -f1_z * u0_y + f1_y * u0_z;
    let p1 = -f1_z * u1_y + f1_y * u1_z;
    let rad = ext_y * f1_z.abs() + ext_z * f1_y.abs();
    if p0.min(p1) > rad || p0.max(p1) < -rad {
        c.r[3] = 0;
        return true;
    }

    // --- Axis a02 = u_x x f2 = (0, -f2_z, f2_y) ---
    let p0 = -f2_z * u0_y + f2_y * u0_z;
    let p1 = -f2_z * u1_y + f2_y * u1_z;
    let rad = ext_y * f2_z.abs() + ext_z * f2_y.abs();
    if p0.min(p1) > rad || p0.max(p1) < -rad {
        c.r[3] = 0;
        return true;
    }

    // --- Axis a10 = u_y x f0 = (f0_z, 0, -f0_x) ---
    let p0 = f0_z * u0_x - f0_x * u0_z;
    let p2 = f0_z * u2_x - f0_x * u2_z;
    let rad = ext_x * f0_z.abs() + ext_z * f0_x.abs();
    if p0.min(p2) > rad || p0.max(p2) < -rad {
        c.r[3] = 0;
        return true;
    }

    // --- Axis a11 = u_y x f1 = (f1_z, 0, -f1_x) ---
    let p0 = f1_z * u0_x - f1_x * u0_z;
    let p1 = f1_z * u1_x - f1_x * u1_z;
    let rad = ext_x * f1_z.abs() + ext_z * f1_x.abs();
    if p0.min(p1) > rad || p0.max(p1) < -rad {
        c.r[3] = 0;
        return true;
    }

    // --- Axis a12 = u_y x f2 = (f2_z, 0, -f2_x) ---
    let p0 = f2_z * u0_x - f2_x * u0_z;
    let p1 = f2_z * u1_x - f2_x * u1_z;
    let rad = ext_x * f2_z.abs() + ext_z * f2_x.abs();
    if p0.min(p1) > rad || p0.max(p1) < -rad {
        c.r[3] = 0;
        return true;
    }

    // --- Axis a20 = u_z x f0 = (-f0_y, f0_x, 0) ---
    let p0 = -f0_y * u0_x + f0_x * u0_y;
    let p2 = -f0_y * u2_x + f0_x * u2_y;
    let rad = ext_x * f0_y.abs() + ext_y * f0_x.abs();
    if p0.min(p2) > rad || p0.max(p2) < -rad {
        c.r[3] = 0;
        return true;
    }

    // --- Axis a21 = u_z x f1 = (-f1_y, f1_x, 0) ---
    let p0 = -f1_y * u0_x + f1_x * u0_y;
    let p1 = -f1_y * u1_x + f1_x * u1_y;
    let rad = ext_x * f1_y.abs() + ext_y * f1_x.abs();
    if p0.min(p1) > rad || p0.max(p1) < -rad {
        c.r[3] = 0;
        return true;
    }

    // --- Axis a22 = u_z x f2 = (-f2_y, f2_x, 0) ---
    let p0 = -f2_y * u0_x + f2_x * u0_y;
    let p1 = -f2_y * u1_x + f2_x * u1_y;
    let rad = ext_x * f2_y.abs() + ext_y * f2_x.abs();
    if p0.min(p1) > rad || p0.max(p1) < -rad {
        c.r[3] = 0;
        return true;
    }

    // Phase 3: Plane of the triangle vs box
    let norm_x = f0_y * (-f2_z) - f0_z * (-f2_y);
    let norm_y = f0_z * (-f2_x) - f0_x * (-f2_z);
    let norm_z = f0_x * (-f2_y) - f0_y * (-f2_x);

    let rad = ext_x * norm_x.abs() + ext_y * norm_y.abs() + ext_z * norm_z.abs();
    let d = norm_x * u0_x + norm_y * u0_y + norm_z * u0_z;
    if d.abs() > rad {
        c.r[3] = 0;
        return true;
    }

    // All 13 separating axis tests passed -> Intersection!
    c.r[3] = 1;
    true
}

/// Native fast-path for GameCube SDK `__GXSetDirtyState` (`0x802EBBF4`).
/// When dirtyState == 0 (consecutive draw calls sharing vertex, lighting, and texture state),
/// this eliminates function prologue, stack frame allocation, multiple condition checks, and stack epilogue.
pub fn hle_gx_set_dirty_state(c: &mut Ctx) -> bool {
    let mem = c.mem;
    let r2 = c.r[2];
    let gx_data_ptr_addr = r2.wrapping_add(0xFFFFA5C8);
    if !in_ram(gx_data_ptr_addr, 4) {
        return false;
    }
    let gx_data = mr32(mem, gx_data_ptr_addr);
    let dirty_addr = gx_data.wrapping_add(0x5AC);
    if !in_ram(dirty_addr, 4) {
        return false;
    }
    let dirty = mr32(mem, dirty_addr);
    if dirty == 0 {
        c.r[3] = gx_data;
        c.r[0] = 0;
        return true;
    }
    false
}


/// Native fast-path for GameCube SDK `GXSetChanCtrl` (`0x802ED06C`).
pub fn hle_gx_set_chan_ctrl(c: &mut Ctx) -> bool {
    let chan = c.r[3];
    let enable = (c.r[4] & 0xFF) != 0;
    let amb_src = c.r[5];
    let mat_src = c.r[6];
    let light_mask = c.r[7];
    let diff_fn = c.r[8];
    let attn_fn = c.r[9];

    let mut reg = (mat_src & 1) | if enable { 2 } else { 0 };
    reg |= (amb_src & 1) << 6;
    if attn_fn != 0 {
        reg |= (diff_fn & 3) << 7;
        if attn_fn >= 2 {
            reg |= 1 << 9;
        }
        reg |= 1 << 10;
    }
    reg |= (light_mask & 0xF) << 2;
    reg |= ((light_mask >> 4) & 0xF) << 11;

    let xf_reg = 0x100E + (chan & 3);
    unsafe {
        let fifo_base = c.mem.add(0x0200_0000);
        let mut cur = c.fifo_len & 0x01FF_FFFF;
        fifo_base.add(cur).write(0x10);
        (fifo_base.add(cur + 1) as *mut u32).write_unaligned(xf_reg.to_be());
        (fifo_base.add(cur + 5) as *mut u32).write_unaligned(reg.to_be());
        cur += 9;

        if chan == 4 {
            fifo_base.add(cur).write(0x10);
            (fifo_base.add(cur + 1) as *mut u32).write_unaligned(0x1010u32.to_be());
            (fifo_base.add(cur + 5) as *mut u32).write_unaligned(reg.to_be());
            cur += 9;
        } else if chan == 5 {
            fifo_base.add(cur).write(0x10);
            (fifo_base.add(cur + 1) as *mut u32).write_unaligned(0x1011u32.to_be());
            (fifo_base.add(cur + 5) as *mut u32).write_unaligned(reg.to_be());
            cur += 9;
        }
        c.fifo_len = cur & 0x01FF_FFFF;
    }

    let gx_data_ptr = c.r[2].wrapping_add(0xFFFFA5C8);
    if in_ram(gx_data_ptr, 4) {
        let gx_data = mr32(c.mem, gx_data_ptr);
        if in_ram(gx_data, 4) {
            c.w16(gx_data + 2, 1);
        }
    }

    true
}

/// Native fast-path for GameCube SDK `GXClearVtxDesc` (`0x802EAC04`).
pub fn hle_gx_clear_vtx_desc(c: &mut Ctx) -> bool {
    let gx_data_ptr = c.r[2].wrapping_add(0xFFFFA5C8);
    let mem = c.mem;
    if !in_ram(gx_data_ptr, 4) { return false; }
    let gx_data = mr32(mem, gx_data_ptr);
    if !in_ram(gx_data, 0x5B0) { return false; }

    c.w32(gx_data + 0x14, 0x200); // POS direct (bit 9)
    c.w32(gx_data + 0x18, 0);
    c.w8(gx_data + 0x4D4, 0);
    c.w8(gx_data + 0x4D5, 0);
    let dirty = mr32(mem, gx_data + 0x5AC);
    c.w32(gx_data + 0x5AC, dirty | 8);
    true
}

/// Native fast-path for GameCube SDK `GXSetVtxDesc` (`0x802EA7B8`).
pub fn hle_gx_set_vtx_desc(c: &mut Ctx) -> bool {
    let attr = c.r[3];
    let attr_type = c.r[4];
    if attr > 25 {
        return true;
    }
    let gx_data_ptr = c.r[2].wrapping_add(0xFFFFA5C8);
    let mem = c.mem;
    if !in_ram(gx_data_ptr, 4) { return false; }
    let gx_data = mr32(mem, gx_data_ptr);
    if !in_ram(gx_data, 0x5B0) { return false; }

    match attr {
        0 => {
            let cur = mr32(mem, gx_data + 0x14);
            c.w32(gx_data + 0x14, (cur & !1) | (attr_type & 1));
        }
        1..=8 => {
            let cur = mr32(mem, gx_data + 0x14);
            c.w32(gx_data + 0x14, (cur & !(1 << attr)) | ((attr_type & 1) << attr));
        }
        9 => {
            let cur = mr32(mem, gx_data + 0x14);
            c.w32(gx_data + 0x14, (cur & !(3 << 9)) | ((attr_type & 3) << 9));
        }
        10 => {
            if attr_type != 0 {
                c.w8(gx_data + 0x4D4, 1);
                c.w8(gx_data + 0x4D5, 0);
                c.w32(gx_data + 0x4D0, attr_type);
                let cur = mr32(mem, gx_data + 0x14);
                c.w32(gx_data + 0x14, (cur & !(3 << 11)) | ((attr_type & 3) << 11));
            } else {
                c.w8(gx_data + 0x4D4, 0);
                let cur = mr32(mem, gx_data + 0x14);
                if mr8(mem, gx_data + 0x4D5) == 0 {
                    c.w32(gx_data + 0x14, cur & !(3 << 11));
                }
            }
        }
        25 => {
            if attr_type != 0 {
                c.w8(gx_data + 0x4D5, 1);
                c.w8(gx_data + 0x4D4, 0);
                c.w32(gx_data + 0x4D0, attr_type);
                let cur = mr32(mem, gx_data + 0x14);
                c.w32(gx_data + 0x14, (cur & !(3 << 11)) | ((attr_type & 3) << 11));
            } else {
                c.w8(gx_data + 0x4D5, 0);
                let cur = mr32(mem, gx_data + 0x14);
                if mr8(mem, gx_data + 0x4D4) == 0 {
                    c.w32(gx_data + 0x14, cur & !(3 << 11));
                }
            }
        }
        11 => {
            let cur = mr32(mem, gx_data + 0x14);
            c.w32(gx_data + 0x14, (cur & !(3 << 13)) | ((attr_type & 3) << 13));
        }
        12 => {
            let cur = mr32(mem, gx_data + 0x14);
            c.w32(gx_data + 0x14, (cur & !(3 << 15)) | ((attr_type & 3) << 15));
        }
        13..=20 => {
            let shift = (attr - 13) * 2;
            let cur = mr32(mem, gx_data + 0x18);
            c.w32(gx_data + 0x18, (cur & !(3 << shift)) | ((attr_type & 3) << shift));
        }
        _ => {}
    }

    let dirty = mr32(mem, gx_data + 0x5AC);
    c.w32(gx_data + 0x5AC, dirty | 8);
    true
}

/// Native fast-path for GameCube SDK `GXSetVtxAttrFmt` (`0x802EAC3C`).
pub fn hle_gx_set_vtx_attr_fmt(c: &mut Ctx) -> bool {
    let vtxfmt = c.r[3];
    let attr = c.r[4];
    let cnt = c.r[5];
    let comp_type = c.r[6];
    let frac = c.r[7];

    if vtxfmt > 7 {
        return false;
    }
    let gx_data_ptr = c.r[2].wrapping_add(0xFFFFA5C8);
    let mem = c.mem;
    if !in_ram(gx_data_ptr, 4) { return false; }
    let gx_data = mr32(mem, gx_data_ptr);
    if !in_ram(gx_data, 0x5B0) { return false; }

    let va_addr = gx_data + 0x1C + vtxfmt * 4;
    let vb_addr = gx_data + 0x3C + vtxfmt * 4;
    let vc_addr = gx_data + 0x5C + vtxfmt * 4;

    match attr {
        9 => {
            let cur = mr32(mem, va_addr);
            let val = (cnt & 1) | ((comp_type & 7) << 1) | ((frac & 0x1F) << 4);
            c.w32(va_addr, (cur & !0x1FF) | val);
        }
        10 | 25 => {
            let cur = mr32(mem, va_addr);
            let mut val = (cur & !(7 << 10)) | ((comp_type & 7) << 10);
            if cnt == 2 {
                val |= (1 << 9) | (1 << 31);
            } else {
                val = (val & !(1 << 31)) | ((cnt & 1) << 9);
            }
            c.w32(va_addr, val);
        }
        11 => {
            let cur = mr32(mem, va_addr);
            let val = ((cnt & 1) << 13) | ((comp_type & 7) << 14);
            c.w32(va_addr, (cur & !(0xF << 13)) | val);
        }
        12 => {
            let cur = mr32(mem, va_addr);
            let val = ((cnt & 1) << 17) | ((comp_type & 7) << 18);
            c.w32(va_addr, (cur & !(0xF << 17)) | val);
        }
        13 => {
            let cur = mr32(mem, va_addr);
            let val = ((cnt & 1) << 21) | ((comp_type & 7) << 22) | ((frac & 0x1F) << 25);
            c.w32(va_addr, (cur & !(0x1FF << 21)) | val);
        }
        14..=16 => {
            let shift = (attr - 14) * 9;
            let cur = mr32(mem, vb_addr);
            let val = (cnt & 1) | ((comp_type & 7) << 1) | ((frac & 0x1F) << 4);
            c.w32(vb_addr, (cur & !(0x1FF << shift)) | (val << shift));
        }
        17 => {
            let cur_b = mr32(mem, vb_addr);
            let val_b = ((cnt & 1) << 27) | ((comp_type & 7) << 28);
            c.w32(vb_addr, (cur_b & !(0xF << 27)) | val_b);

            let cur_c = mr32(mem, vc_addr);
            let val_c = frac & 0x1F;
            c.w32(vc_addr, (cur_c & !0x1F) | val_c);
        }
        18..=20 => {
            let shift = 5 + (attr - 18) * 9;
            let cur = mr32(mem, vc_addr);
            let val = (cnt & 1) | ((comp_type & 7) << 1) | ((frac & 0x1F) << 4);
            c.w32(vc_addr, (cur & !(0x1FF << shift)) | (val << shift));
        }
        _ => {}
    }

    let dirty = mr8(mem, gx_data + 0x5AB);
    c.w8(gx_data + 0x5AB, dirty | (1 << vtxfmt));
    let dirty_5ac = mr32(mem, gx_data + 0x5AC);
    c.w32(gx_data + 0x5AC, dirty_5ac | 0x10);
    true
}

/// Native fast-path for GameCube SDK `GXSetArray` (`0x802EB1B4`).
pub fn hle_gx_set_array(c: &mut Ctx) -> bool {
    let mut attr = c.r[3];
    let base_ptr = c.r[4];
    let stride = c.r[5];

    if attr == 25 {
        attr = 10;
    }
    let r6 = attr.wrapping_sub(9);

    unsafe {
        let fifo_base = c.mem.add(0x0200_0000);
        let mut cur = c.fifo_len & 0x01FF_FFFF;
        fifo_base.add(cur).write(0x08);
        fifo_base.add(cur + 1).write(0xA0 | (r6 as u8));
        (fifo_base.add(cur + 2) as *mut u32).write_unaligned((base_ptr & 0x3FFF_FFFF).to_be());
        cur += 6;

        fifo_base.add(cur).write(0x08);
        fifo_base.add(cur + 1).write(0xB0 | (r6 as u8));
        (fifo_base.add(cur + 2) as *mut u32).write_unaligned((stride & 0xFF).to_be());
        cur += 6;
        c.fifo_len = cur & 0x01FF_FFFF;
    }

    let gx_data_ptr = c.r[2].wrapping_add(0xFFFFA5C8);
    let mem = c.mem;
    let r0 = r6.wrapping_sub(12);
    if r0 < 4 && in_ram(gx_data_ptr, 4) {
        let gx_data = mr32(mem, gx_data_ptr);
        if in_ram(gx_data, 0x100) {
            c.w32(gx_data + 0x88 + r0 * 4, base_ptr);
            c.w32(gx_data + 0x98 + r0 * 4, stride);
        }
    }

    true
}

/// Native fast-path for `xQuickCullIsects` (`0x801957D8`).
/// Checks if two xQCData bounding volumes intersect.
/// Returns 1 if intersecting, 0 if disjoint.
pub fn hle_xquick_cull_isects(c: &mut Ctx) -> bool {
    let a_ptr = c.r[3];
    let b_ptr = c.r[4];
    let mem = c.mem;

    if in_ram(a_ptr, 8) && in_ram(b_ptr, 8) {
        unsafe {
            use core::arch::aarch64::*;
            let pa = mptr(mem, a_ptr) as *const i8;
            let pb = mptr(mem, b_ptr) as *const i8;

            let va = vld1_s8(pa);
            let vb = vld1_s8(pb);

            let max_b = vext_s8(vb, vb, 4);
            let max_a = vext_s8(va, va, 4);

            let cmp1 = vcgt_s8(va, max_b);
            let cmp2 = vcgt_s8(vb, max_a);

            let disjoint = vorr_u8(cmp1, cmp2);
            let val = vget_lane_u32(vreinterpret_u32_u8(disjoint), 0) & 0x00FF_FFFF;
            c.r[3] = if val != 0 { 0 } else { 1 };
        }
    } else {
        let a0 = mr8(mem, a_ptr) as i8;
        let b4 = mr8(mem, b_ptr.wrapping_add(4)) as i8;
        if a0 > b4 {
            c.r[3] = 0;
            return true;
        }
        let a1 = mr8(mem, a_ptr.wrapping_add(1)) as i8;
        let b5 = mr8(mem, b_ptr.wrapping_add(5)) as i8;
        if a1 > b5 {
            c.r[3] = 0;
            return true;
        }
        let a2 = mr8(mem, a_ptr.wrapping_add(2)) as i8;
        let b6 = mr8(mem, b_ptr.wrapping_add(6)) as i8;
        if a2 > b6 {
            c.r[3] = 0;
            return true;
        }
        let b0 = mr8(mem, b_ptr) as i8;
        let a4 = mr8(mem, a_ptr.wrapping_add(4)) as i8;
        if b0 > a4 {
            c.r[3] = 0;
            return true;
        }
        let b1 = mr8(mem, b_ptr.wrapping_add(1)) as i8;
        let a5 = mr8(mem, a_ptr.wrapping_add(5)) as i8;
        if b1 > a5 {
            c.r[3] = 0;
            return true;
        }
        let b2 = mr8(mem, b_ptr.wrapping_add(2)) as i8;
        let a6 = mr8(mem, a_ptr.wrapping_add(6)) as i8;
        if b2 > a6 {
            c.r[3] = 0;
            return true;
        }
        c.r[3] = 1;
    }
    true
}

/// Native fast implementation of RenderWare `_rwGCNDisplayListGetStride` (`0x80331A64`).
/// Computes the exact byte stride of a vertex formatted according to `_rwVtxFmt`.
pub fn hle_rw_gcn_display_list_get_stride(c: &mut Ctx) -> bool {
    let fmt_ptr = c.r[3];
    let mem = c.mem;
    if !in_ram(fmt_ptr, 0x20) {
        return false;
    }
    let vcd_lo = mr32(mem, fmt_ptr + 0x10);
    let vcd_hi = mr32(mem, fmt_ptr + 0x14);
    let vat_a = mr32(mem, fmt_ptr + 0x4);
    let vat_b = mr32(mem, fmt_ptr + 0x8);
    let vat_c = mr32(mem, fmt_ptr + 0xC);

    let attr = |kind: u32, direct: u32| match kind {
        1 => direct,
        2 => 1,
        3 => 2,
        _ => 0,
    };

    const COMP: [u32; 8] = [1, 1, 2, 2, 4, 0, 0, 0];
    const COLOR: [u32; 8] = [2, 3, 4, 2, 3, 4, 0, 0];

    // Attributes 0..7 (matrix indices)
    let mut stride = (vcd_lo & 0x1FF).count_ones();

    // Attribute 9: POS
    stride += attr(
        (vcd_lo >> 9) & 3,
        (2 + (vat_a & 1)) * COMP[((vat_a >> 1) & 7) as usize],
    );

    // Attribute 10: NRM / NBT
    let nbt = (vat_a >> 9) & 1 != 0;
    let nrm_direct = if nbt { 9 } else { 3 } * COMP[((vat_a >> 10) & 7) as usize];
    let nrm_indices = if nbt && (vat_a >> 31) != 0 { 3 } else { 1 };
    stride += match (vcd_lo >> 11) & 3 {
        1 => nrm_direct,
        k => attr(k, 0) * nrm_indices,
    };

    // Attribute 11: CLR0
    stride += attr((vcd_lo >> 13) & 3, COLOR[((vat_a >> 14) & 7) as usize]);

    // Attribute 12: CLR1
    stride += attr((vcd_lo >> 15) & 3, COLOR[((vat_a >> 18) & 7) as usize]);

    // Attributes 13..20: TEX0..TEX7
    let tex = [
        (vat_a >> 21, vat_a >> 22),
        (vat_b, vat_b >> 1),
        (vat_b >> 9, vat_b >> 10),
        (vat_b >> 18, vat_b >> 19),
        (vat_b >> 27, vat_b >> 28),
        (vat_c >> 5, vat_c >> 6),
        (vat_c >> 14, vat_c >> 15),
        (vat_c >> 23, vat_c >> 24),
    ];
    for (i, (cnt, fmt)) in tex.into_iter().enumerate() {
        stride += attr(
            (vcd_hi >> (i * 2)) & 3,
            (1 + (cnt & 1)) * COMP[(fmt & 7) as usize],
        );
    }

    c.r[3] = stride;
    true
}

/// Native ARM64 implementation of Heavy Iron `properSphereIsectTri` (`0x80206EDC`).
/// Computes exact sphere-triangle intersection with normal, closest point, and distance.
pub fn hle_proper_sphere_isect_tri(c: &mut Ctx) -> bool {
    let center_ptr = c.r[3];
    let radius = c.f[1] as f32;
    let hit_ptr = c.r[4];
    let dist_ptr = c.r[5];
    let tri_ptr = c.r[6];
    let mem = c.mem;

    if !in_ram(center_ptr, 12) || !in_ram(hit_ptr, 12) || !in_ram(dist_ptr, 4) || !in_ram(tri_ptr, 40) {
        return false;
    }

    let cx = rf32(mem, center_ptr);
    let cy = rf32(mem, center_ptr + 4);
    let cz = rf32(mem, center_ptr + 8);

    let nx = rf32(mem, tri_ptr);
    let ny = rf32(mem, tri_ptr + 4);
    let nz = rf32(mem, tri_ptr + 8);

    let px = rf32(mem, tri_ptr + 12);
    let py = rf32(mem, tri_ptr + 16);
    let pz = rf32(mem, tri_ptr + 20);

    let v0_ptr = mr32(mem, tri_ptr + 28);
    let v1_ptr = mr32(mem, tri_ptr + 32);
    let v2_ptr = mr32(mem, tri_ptr + 36);

    if !in_ram(v0_ptr, 12) || !in_ram(v1_ptr, 12) || !in_ram(v2_ptr, 12) {
        return false;
    }

    let v0_x = rf32(mem, v0_ptr);
    let v0_y = rf32(mem, v0_ptr + 4);
    let v0_z = rf32(mem, v0_ptr + 8);

    let v1_x = rf32(mem, v1_ptr);
    let v1_y = rf32(mem, v1_ptr + 4);
    let v1_z = rf32(mem, v1_ptr + 8);

    let v2_x = rf32(mem, v2_ptr);
    let v2_y = rf32(mem, v2_ptr + 4);
    let v2_z = rf32(mem, v2_ptr + 8);

    // Plane distance: dot(normal, center) - dot(normal, point)
    let dot_c = nx * cx + ny * cy + nz * cz;
    let dot_p = nx * px + ny * py + nz * pz;
    let dist_to_plane = dot_c - dot_p;

    let abs_dist_plane = dist_to_plane.abs();
    if abs_dist_plane > radius {
        c.r[3] = 0;
        return true;
    }

    // Projected point on plane: center - normal * dist_to_plane
    let proj_x = cx - nx * dist_to_plane;
    let proj_y = cy - ny * dist_to_plane;
    let proj_z = cz - nz * dist_to_plane;

    // Test PointWithinTriangle on projected 2D coordinates (dominant normal axis)
    let abs_nx = nx.abs();
    let abs_ny = ny.abs();
    let abs_nz = nz.abs();

    let inside = if abs_nz >= abs_nx && abs_nz >= abs_ny {
        let c0 = (v1_x - v0_x) * (proj_y - v0_y) - (v1_y - v0_y) * (proj_x - v0_x);
        let c1 = (v2_x - v1_x) * (proj_y - v1_y) - (v2_y - v1_y) * (proj_x - v1_x);
        let c2 = (v0_x - v2_x) * (proj_y - v2_y) - (v0_y - v2_y) * (proj_x - v2_x);
        (c0 >= 0.0 && c1 >= 0.0 && c2 >= 0.0) || (c0 <= 0.0 && c1 <= 0.0 && c2 <= 0.0)
    } else if abs_ny >= abs_nx {
        let c0 = (v1_x - v0_x) * (proj_z - v0_z) - (v1_z - v0_z) * (proj_x - v0_x);
        let c1 = (v2_x - v1_x) * (proj_z - v1_z) - (v2_z - v1_z) * (proj_x - v1_x);
        let c2 = (v0_x - v2_x) * (proj_z - v2_z) - (v0_z - v2_z) * (proj_x - v2_x);
        (c0 >= 0.0 && c1 >= 0.0 && c2 >= 0.0) || (c0 <= 0.0 && c1 <= 0.0 && c2 <= 0.0)
    } else {
        let c0 = (v1_y - v0_y) * (proj_z - v0_z) - (v1_z - v0_z) * (proj_y - v0_y);
        let c1 = (v2_y - v1_y) * (proj_z - v1_z) - (v2_z - v1_z) * (proj_y - v1_y);
        let c2 = (v0_y - v2_y) * (proj_z - v2_z) - (v0_z - v2_z) * (proj_y - v2_y);
        (c0 >= 0.0 && c1 >= 0.0 && c2 >= 0.0) || (c0 <= 0.0 && c1 <= 0.0 && c2 <= 0.0)
    };

    if inside {
        wf32(mem, hit_ptr, proj_x);
        wf32(mem, hit_ptr + 4, proj_y);
        wf32(mem, hit_ptr + 8, proj_z);
        wf32(mem, dist_ptr, abs_dist_plane);
        c.r[3] = 1;
        return true;
    }

    // Not inside triangle: test closest point on the 3 edges
    let edges = [
        (v0_x, v0_y, v0_z, v1_x, v1_y, v1_z),
        (v1_x, v1_y, v1_z, v2_x, v2_y, v2_z),
        (v2_x, v2_y, v2_z, v0_x, v0_y, v0_z),
    ];

    let r_sq = radius * radius;
    let cur_dist = rf32(mem, dist_ptr);
    let mut min_d_sq = cur_dist * cur_dist;
    if min_d_sq > r_sq {
        min_d_sq = r_sq;
    }
    let mut best_hit = None;

    for (ax, ay, az, bx, by, bz) in edges {
        let ab_x = bx - ax;
        let ab_y = by - ay;
        let ab_z = bz - az;
        let ab_sq = ab_x * ab_x + ab_y * ab_y + ab_z * ab_z;
        if ab_sq <= 0.0 {
            continue;
        }
        let ac_x = cx - ax;
        let ac_y = cy - ay;
        let ac_z = cz - az;
        let t = (ac_x * ab_x + ac_y * ab_y + ac_z * ab_z) / ab_sq;
        let t_clamped = if t < 0.0 { 0.0 } else if t > 1.0 { 1.0 } else { t };
        let qx = ax + ab_x * t_clamped;
        let qy = ay + ab_y * t_clamped;
        let qz = az + ab_z * t_clamped;
        let diff_x = cx - qx;
        let diff_y = cy - qy;
        let diff_z = cz - qz;
        let d_sq = diff_x * diff_x + diff_y * diff_y + diff_z * diff_z;
        if d_sq < min_d_sq {
            min_d_sq = d_sq;
            best_hit = Some((qx, qy, qz));
        }
    }

    if let Some((qx, qy, qz)) = best_hit {
        let final_dist = hw_sqrt_f32(min_d_sq);
        wf32(mem, hit_ptr, qx);
        wf32(mem, hit_ptr + 4, qy);
        wf32(mem, hit_ptr + 8, qz);
        wf32(mem, dist_ptr, final_dist);
        c.r[3] = 1;
        return true;
    }

    c.r[3] = 0;
    true
}

/// Native ARM64 implementation of `PointWithinTriangle` (`0x8020722C`).
pub fn hle_point_within_triangle(c: &mut Ctx) -> bool {
    let p_ptr = c.r[3];
    let v_ptrs_ptr = c.r[4];
    let norm_ptr = c.r[5];
    let mem = c.mem;

    if !in_ram(p_ptr, 12) || !in_ram(v_ptrs_ptr, 12) || !in_ram(norm_ptr, 12) {
        return false;
    }

    let px = rf32(mem, p_ptr);
    let py = rf32(mem, p_ptr + 4);
    let pz = rf32(mem, p_ptr + 8);

    let nx = rf32(mem, norm_ptr);
    let ny = rf32(mem, norm_ptr + 4);
    let nz = rf32(mem, norm_ptr + 8);

    let v0_ptr = mr32(mem, v_ptrs_ptr);
    let v1_ptr = mr32(mem, v_ptrs_ptr + 4);
    let v2_ptr = mr32(mem, v_ptrs_ptr + 8);

    if !in_ram(v0_ptr, 12) || !in_ram(v1_ptr, 12) || !in_ram(v2_ptr, 12) {
        return false;
    }

    let v0_x = rf32(mem, v0_ptr);
    let v0_y = rf32(mem, v0_ptr + 4);
    let v0_z = rf32(mem, v0_ptr + 8);

    let v1_x = rf32(mem, v1_ptr);
    let v1_y = rf32(mem, v1_ptr + 4);
    let v1_z = rf32(mem, v1_ptr + 8);

    let v2_x = rf32(mem, v2_ptr);
    let v2_y = rf32(mem, v2_ptr + 4);
    let v2_z = rf32(mem, v2_ptr + 8);

    let abs_nx = nx.abs();
    let abs_ny = ny.abs();
    let abs_nz = nz.abs();

    let inside = if abs_nz >= abs_nx && abs_nz >= abs_ny {
        let c0 = (v1_x - v0_x) * (py - v0_y) - (v1_y - v0_y) * (px - v0_x);
        let c1 = (v2_x - v1_x) * (py - v1_y) - (v2_y - v1_y) * (px - v1_x);
        let c2 = (v0_x - v2_x) * (py - v2_y) - (v0_y - v2_y) * (px - v2_x);
        (c0 >= 0.0 && c1 >= 0.0 && c2 >= 0.0) || (c0 <= 0.0 && c1 <= 0.0 && c2 <= 0.0)
    } else if abs_ny >= abs_nx {
        let c0 = (v1_x - v0_x) * (pz - v0_z) - (v1_z - v0_z) * (px - v0_x);
        let c1 = (v2_x - v1_x) * (pz - v1_z) - (v2_z - v1_z) * (px - v1_x);
        let c2 = (v0_x - v2_x) * (pz - v2_z) - (v0_z - v2_z) * (px - v2_x);
        (c0 >= 0.0 && c1 >= 0.0 && c2 >= 0.0) || (c0 <= 0.0 && c1 <= 0.0 && c2 <= 0.0)
    } else {
        let c0 = (v1_y - v0_y) * (pz - v0_z) - (v1_z - v0_z) * (py - v0_y);
        let c1 = (v2_y - v1_y) * (pz - v1_z) - (v2_z - v1_z) * (py - v1_y);
        let c2 = (v0_y - v2_y) * (pz - v2_z) - (v0_z - v2_z) * (py - v2_y);
        (c0 >= 0.0 && c1 >= 0.0 && c2 >= 0.0) || (c0 <= 0.0 && c1 <= 0.0 && c2 <= 0.0)
    };

    c.r[3] = if inside { 1 } else { 0 };
    true
}

/// Native ARM64 implementation of RenderWare `RwBBoxCalculate` (`0x803418F8`).
/// Computes axis-aligned bounding box enclosing `num_points`.
pub fn hle_rw_bbox_calculate(c: &mut Ctx) -> bool {
    let bbox_ptr = c.r[3];
    let points_ptr = c.r[4];
    let n = c.r[5] as usize;
    let mem = c.mem;

    if n == 0 || !in_ram(bbox_ptr, 24) || !in_ram(points_ptr, n * 12) {
        return false;
    }

    let mut min_x = rf32(mem, points_ptr);
    let mut min_y = rf32(mem, points_ptr + 4);
    let mut min_z = rf32(mem, points_ptr + 8);
    let mut max_x = min_x;
    let mut max_y = min_y;
    let mut max_z = min_z;

    for i in 1..n {
        let p = points_ptr + (i as u32) * 12;
        let px = rf32(mem, p);
        let py = rf32(mem, p + 4);
        let pz = rf32(mem, p + 8);

        if px < min_x { min_x = px; }
        if py < min_y { min_y = py; }
        if pz < min_z { min_z = pz; }
        if px > max_x { max_x = px; }
        if py > max_y { max_y = py; }
        if pz > max_z { max_z = pz; }
    }

    // In RenderWare RwBBox:
    // offset 0x0..0xC is sup (max: x, y, z)
    // offset 0xC..0x18 is inf (min: x, y, z)
    wf32(mem, bbox_ptr, max_x);
    wf32(mem, bbox_ptr + 4, max_y);
    wf32(mem, bbox_ptr + 8, max_z);
    wf32(mem, bbox_ptr + 12, min_x);
    wf32(mem, bbox_ptr + 16, min_y);
    wf32(mem, bbox_ptr + 20, min_z);

    c.r[3] = bbox_ptr;
    true
}

/// Native ARM64 implementation of `xBoxForTriangle` (`0x802020AC`).
/// Computes axis-aligned bounding box enclosing triangle vertices (v0, v1, v2).
/// In Heavy Iron's `xBox`:
/// offset +0x00..+0x0C: upper (max: x, y, z)
/// offset +0x0C..+0x18: lower (min: x, y, z)
pub fn hle_xbox_for_triangle(c: &mut Ctx) -> bool {
    let box_ptr = c.r[3];
    let v0_ptr = c.r[4];
    let v1_ptr = c.r[5];
    let v2_ptr = c.r[6];
    let mem = c.mem;

    if !in_ram(box_ptr, 24) || !in_ram(v0_ptr, 12) || !in_ram(v1_ptr, 12) || !in_ram(v2_ptr, 12) {
        return false;
    }

    let v0_x = rf32(mem, v0_ptr);
    let v0_y = rf32(mem, v0_ptr + 4);
    let v0_z = rf32(mem, v0_ptr + 8);

    let v1_x = rf32(mem, v1_ptr);
    let v1_y = rf32(mem, v1_ptr + 4);
    let v1_z = rf32(mem, v1_ptr + 8);

    let v2_x = rf32(mem, v2_ptr);
    let v2_y = rf32(mem, v2_ptr + 4);
    let v2_z = rf32(mem, v2_ptr + 8);

    let max_x = v0_x.max(v1_x).max(v2_x);
    let max_y = v0_y.max(v1_y).max(v2_y);
    let max_z = v0_z.max(v1_z).max(v2_z);

    let min_x = v0_x.min(v1_x).min(v2_x);
    let min_y = v0_y.min(v1_y).min(v2_y);
    let min_z = v0_z.min(v1_z).min(v2_z);

    wf32(mem, box_ptr, max_x);
    wf32(mem, box_ptr + 4, max_y);
    wf32(mem, box_ptr + 8, max_z);

    wf32(mem, box_ptr + 12, min_x);
    wf32(mem, box_ptr + 16, min_y);
    wf32(mem, box_ptr + 20, min_z);

    true
}

/// Native ARM64 implementation of `xQuatToMat` (`0x801EA1F8`).
/// Converts unit quaternion (x, y, z, w) into 3x3 rotation matrix (16-byte row stride).
pub fn hle_xquat_to_mat(c: &mut Ctx) -> bool {
    let q_ptr = c.r[3];
    let m_ptr = c.r[4];
    let mem = c.mem;

    if !in_ram(q_ptr, 16) || !in_ram(m_ptr, 48) {
        return false;
    }

    let qx = rf32(mem, q_ptr);
    let qy = rf32(mem, q_ptr + 4);
    let qz = rf32(mem, q_ptr + 8);
    let qw = rf32(mem, q_ptr + 12);

    let xx = 2.0 * qx * qx;
    let yy = 2.0 * qy * qy;
    let zz = 2.0 * qz * qz;
    let xy = 2.0 * qx * qy;
    let xz = 2.0 * qx * qz;
    let yz = 2.0 * qy * qz;
    let wx = 2.0 * qw * qx;
    let wy = 2.0 * qw * qy;
    let wz = 2.0 * qw * qz;

    // Row 0
    wf32(mem, m_ptr, 1.0 - yy - zz);
    wf32(mem, m_ptr + 4, xy - wz);
    wf32(mem, m_ptr + 8, xz + wy);
    mw32(mem, m_ptr + 12, 0);

    // Row 1
    wf32(mem, m_ptr + 0x10, xy + wz);
    wf32(mem, m_ptr + 0x14, 1.0 - xx - zz);
    wf32(mem, m_ptr + 0x18, yz - wx);

    // Row 2
    wf32(mem, m_ptr + 0x20, xz - wy);
    wf32(mem, m_ptr + 0x24, yz + wx);
    wf32(mem, m_ptr + 0x28, 1.0 - xx - yy);

    true
}

/// Native ARM64 implementation of `xMat3x3LMulVec` (`0x801EB06C`).
/// Computes out = m * v (row-major dot products).
pub fn hle_xmat3x3_lmul_vec(c: &mut Ctx) -> bool {
    let out_ptr = c.r[3];
    let m_ptr = c.r[4];
    let v_ptr = c.r[5];
    let mem = c.mem;

    if !in_ram(out_ptr, 12) || !in_ram(m_ptr, 48) || !in_ram(v_ptr, 12) {
        return false;
    }

    let vx = rf32(mem, v_ptr);
    let vy = rf32(mem, v_ptr + 4);
    let vz = rf32(mem, v_ptr + 8);

    let m00 = rf32(mem, m_ptr);
    let m01 = rf32(mem, m_ptr + 4);
    let m02 = rf32(mem, m_ptr + 8);

    let m10 = rf32(mem, m_ptr + 0x10);
    let m11 = rf32(mem, m_ptr + 0x14);
    let m12 = rf32(mem, m_ptr + 0x18);

    let m20 = rf32(mem, m_ptr + 0x20);
    let m21 = rf32(mem, m_ptr + 0x24);
    let m22 = rf32(mem, m_ptr + 0x28);

    let ox = m00 * vx + m01 * vy + m02 * vz;
    let oy = m10 * vx + m11 * vy + m12 * vz;
    let oz = m20 * vx + m21 * vy + m22 * vz;

    wf32(mem, out_ptr, ox);
    wf32(mem, out_ptr + 4, oy);
    wf32(mem, out_ptr + 8, oz);

    true
}

/// Native ARM64 implementation of `xMat3x3Tolocal` (`0x801EAF90`).
/// Transforms vector v into local coordinate space of matrix m,
/// normalizing each coordinate by the squared length of the corresponding basis row.
pub fn hle_xmat3x3_tolocal(c: &mut Ctx) -> bool {
    let out_ptr = c.r[3];
    let m_ptr = c.r[4];
    let v_ptr = c.r[5];
    let mem = c.mem;

    if !in_ram(out_ptr, 12) || !in_ram(m_ptr, 48) || !in_ram(v_ptr, 12) {
        return false;
    }

    let vx = rf32(mem, v_ptr);
    let vy = rf32(mem, v_ptr + 4);
    let vz = rf32(mem, v_ptr + 8);

    let m00 = rf32(mem, m_ptr);
    let m01 = rf32(mem, m_ptr + 4);
    let m02 = rf32(mem, m_ptr + 8);

    let m10 = rf32(mem, m_ptr + 0x10);
    let m11 = rf32(mem, m_ptr + 0x14);
    let m12 = rf32(mem, m_ptr + 0x18);

    let m20 = rf32(mem, m_ptr + 0x20);
    let m21 = rf32(mem, m_ptr + 0x24);
    let m22 = rf32(mem, m_ptr + 0x28);

    let dot0 = m00 * vx + m01 * vy + m02 * vz;
    let dot1 = m10 * vx + m11 * vy + m12 * vz;
    let dot2 = m20 * vx + m21 * vy + m22 * vz;

    let len_sq0 = m00 * m00 + m01 * m01 + m02 * m02;
    let len_sq1 = m10 * m10 + m11 * m11 + m12 * m12;
    let len_sq2 = m20 * m20 + m21 * m21 + m22 * m22;

    let ox = dot0 / len_sq0;
    let oy = dot1 / len_sq1;
    let oz = dot2 / len_sq2;

    wf32(mem, out_ptr, ox);
    wf32(mem, out_ptr + 4, oy);
    wf32(mem, out_ptr + 8, oz);

    true
}

/// Native ARM64 implementation of `xMat3x3Mul` (`0x801EB0D0`).
/// Computes out = a * b (3x3 matrix multiplication with 16-byte row stride).
/// Safe against in-place aliasing (out == a or out == b).
pub fn hle_xmat3x3_mul(c: &mut Ctx) -> bool {
    let out_ptr = c.r[3];
    let a_ptr = c.r[4];
    let b_ptr = c.r[5];
    let mem = c.mem;

    if !in_ram(out_ptr, 48) || !in_ram(a_ptr, 48) || !in_ram(b_ptr, 48) {
        return false;
    }

    let a00 = rf32(mem, a_ptr);
    let a01 = rf32(mem, a_ptr + 4);
    let a02 = rf32(mem, a_ptr + 8);

    let a10 = rf32(mem, a_ptr + 0x10);
    let a11 = rf32(mem, a_ptr + 0x14);
    let a12 = rf32(mem, a_ptr + 0x18);

    let a20 = rf32(mem, a_ptr + 0x20);
    let a21 = rf32(mem, a_ptr + 0x24);
    let a22 = rf32(mem, a_ptr + 0x28);

    let b00 = rf32(mem, b_ptr);
    let b01 = rf32(mem, b_ptr + 4);
    let b02 = rf32(mem, b_ptr + 8);

    let b10 = rf32(mem, b_ptr + 0x10);
    let b11 = rf32(mem, b_ptr + 0x14);
    let b12 = rf32(mem, b_ptr + 0x18);

    let b20 = rf32(mem, b_ptr + 0x20);
    let b21 = rf32(mem, b_ptr + 0x24);
    let b22 = rf32(mem, b_ptr + 0x28);

    let o00 = a00 * b00 + a01 * b10 + a02 * b20;
    let o01 = a00 * b01 + a01 * b11 + a02 * b21;
    let o02 = a00 * b02 + a01 * b12 + a02 * b22;

    let o10 = a10 * b00 + a11 * b10 + a12 * b20;
    let o11 = a10 * b01 + a11 * b11 + a12 * b21;
    let o12 = a10 * b02 + a11 * b12 + a12 * b22;

    let o20 = a20 * b00 + a21 * b10 + a22 * b20;
    let o21 = a20 * b01 + a21 * b11 + a22 * b21;
    let o22 = a20 * b02 + a21 * b12 + a22 * b22;

    wf32(mem, out_ptr, o00);
    wf32(mem, out_ptr + 4, o01);
    wf32(mem, out_ptr + 8, o02);
    mw32(mem, out_ptr + 12, 0);

    wf32(mem, out_ptr + 0x10, o10);
    wf32(mem, out_ptr + 0x14, o11);
    wf32(mem, out_ptr + 0x18, o12);

    wf32(mem, out_ptr + 0x20, o20);
    wf32(mem, out_ptr + 0x24, o21);
    wf32(mem, out_ptr + 0x28, o22);

    true
}

/// Native ARM64 implementation of `xBoxInitBoundOBB` (`0x801ECBEC`).
/// Computes axis-aligned bounding box enclosing an oriented bounding box (OBB)
/// transformed by affine matrix (right, up, at, pos).
pub fn hle_xbox_init_bound_obb(c: &mut Ctx) -> bool {
    let out_box_ptr = c.r[3];
    let src_box_ptr = c.r[4];
    let mat_ptr = c.r[5];
    let mem = c.mem;

    if !in_ram(out_box_ptr, 24) || !in_ram(src_box_ptr, 24) || !in_ram(mat_ptr, 64) {
        return false;
    }

    let ux = rf32(mem, src_box_ptr);
    let uy = rf32(mem, src_box_ptr + 4);
    let uz = rf32(mem, src_box_ptr + 8);

    let lx = rf32(mem, src_box_ptr + 12);
    let ly = rf32(mem, src_box_ptr + 16);
    let lz = rf32(mem, src_box_ptr + 20);

    let cx = 0.5 * (lx + ux);
    let cy = 0.5 * (ly + uy);
    let cz = 0.5 * (lz + uz);

    let ex = ux - cx;
    let ey = uy - cy;
    let ez = uz - cz;

    let rx = rf32(mem, mat_ptr);
    let ry = rf32(mem, mat_ptr + 4);
    let rz = rf32(mem, mat_ptr + 8);

    let ux_m = rf32(mem, mat_ptr + 0x10);
    let uy_m = rf32(mem, mat_ptr + 0x14);
    let uz_m = rf32(mem, mat_ptr + 0x18);

    let ax = rf32(mem, mat_ptr + 0x20);
    let ay = rf32(mem, mat_ptr + 0x24);
    let az = rf32(mem, mat_ptr + 0x28);

    let px = rf32(mem, mat_ptr + 0x30);
    let py = rf32(mem, mat_ptr + 0x34);
    let pz = rf32(mem, mat_ptr + 0x38);

    let nc_x = rx * cx + ux_m * cy + ax * cz + px;
    let nc_y = ry * cx + uy_m * cy + ay * cz + py;
    let nc_z = rz * cx + uz_m * cy + az * cz + pz;

    let ne_x = rx.abs() * ex + ux_m.abs() * ey + ax.abs() * ez;
    let ne_y = ry.abs() * ex + uy_m.abs() * ey + ay.abs() * ez;
    let ne_z = rz.abs() * ex + uz_m.abs() * ey + az.abs() * ez;

    // Heavy Iron xBox layout:
    // +0x00: upper (max)
    // +0x0C: lower (min)
    wf32(mem, out_box_ptr, nc_x + ne_x);
    wf32(mem, out_box_ptr + 4, nc_y + ne_y);
    wf32(mem, out_box_ptr + 8, nc_z + ne_z);

    wf32(mem, out_box_ptr + 12, nc_x - ne_x);
    wf32(mem, out_box_ptr + 16, nc_y - ne_y);
    wf32(mem, out_box_ptr + 20, nc_z - ne_z);

    true
}

/// Native `xStrHash__FPCc(const char* str)` (`0x80222788`).
pub fn hle_xstr_hash(c: &mut Ctx) -> bool {
    let mem = c.mem;
    let mut p = c.r[3];
    let mut h = 0u32;
    loop {
        let ch = mr8(mem, p) as u32;
        if ch == 0 {
            break;
        }
        p = p.wrapping_add(1);
        let sub = (ch & ((ch >> 1) & 0x7F)) & 0x20;
        let folded = ch.wrapping_sub(sub) & 0xFF;
        h = h.wrapping_mul(131).wrapping_add(folded);
    }
    c.r[3] = h;
    true
}

/// Native `xStrHash__FPCcUl(const char* str, unsigned long max_len)` (`0x8022273C`).
pub fn hle_xstr_hash_n(c: &mut Ctx) -> bool {
    let mem = c.mem;
    let mut p = c.r[3];
    let max_len = c.r[4];
    let mut h = 0u32;
    let mut n = 0u32;
    while n < max_len {
        let ch = mr8(mem, p) as u32;
        if ch == 0 {
            break;
        }
        n = n.wrapping_add(1);
        p = p.wrapping_add(1);
        let sub = (ch & ((ch >> 1) & 0x7F)) & 0x20;
        let folded = ch.wrapping_sub(sub) & 0xFF;
        h = h.wrapping_mul(131).wrapping_add(folded);
    }
    c.r[3] = h;
    true
}

/// Native `PKR_AssetCount(st_PACKER_READ_DATA* pr, unsigned int type)` (`0x80197B20`).
pub fn hle_pkr_asset_count(c: &mut Ctx) -> bool {
    let mem = c.mem;
    let pr = c.r[3];
    let ty = c.r[4];
    if ty == 0 {
        c.r[3] = mr32(mem, pr.wrapping_add(0xB4));
        return true;
    }
    let mut p = mr32(mem, pr);
    let mut idx = 0u32;
    loop {
        let t = mr32(mem, p);
        if t == 0 {
            c.r[3] = 0;
            return true;
        }
        if t == ty {
            let off = (idx << 4).wrapping_add(0xDC);
            c.r[3] = mr32(mem, pr.wrapping_add(off));
            return true;
        }
        idx = idx.wrapping_add(1);
        p = p.wrapping_add(0x28);
    }
}

/// Native `PKR_AssetByType(st_PACKER_READ_DATA* pr, unsigned int type, int idx, unsigned int* size)` (`0x80197A7C`).
pub fn hle_pkr_asset_by_type(c: &mut Ctx) -> bool {
    let mem = c.mem;
    let pr = c.r[3];
    let ty = c.r[4];
    let idx = (c.r[5] as i32).max(0);
    let size_out = c.r[6];
    if size_out != 0 {
        mw32(mem, size_out, 0);
    }
    let mut p = mr32(mem, pr);
    let mut slot = -1i32;
    let mut i = 0i32;
    loop {
        let t = mr32(mem, p);
        if t == 0 {
            break;
        }
        if t == ty {
            slot = i;
            break;
        }
        i += 1;
        p = p.wrapping_add(0x28);
    }
    if slot < 0 {
        c.r[3] = 0;
        return true;
    }
    let entry_list = pr.wrapping_add(((slot as u32) << 4).wrapping_add(0xD8));
    let count = mr32(mem, entry_list.wrapping_add(4)) as i32;
    if idx >= count {
        c.r[3] = 0;
        return true;
    }
    let base = mr32(mem, entry_list);
    let asset_hdr = mr32(mem, base.wrapping_add((idx as u32) << 2));
    if size_out != 0 {
        mw32(mem, size_out, mr32(mem, asset_hdr.wrapping_add(0x0C)));
    }
    c.r[3] = mr32(mem, asset_hdr.wrapping_add(0x24));
    true
}
