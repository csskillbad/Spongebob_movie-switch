//! Runtime for the recompiled game: guest CPU state, guest memory and SDK replacements.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec, vec::Vec};

pub mod hle;
pub mod mem;

use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use hle::Hle;

/// A recompiled guest function.
pub type GuestFn = fn(&mut Ctx);

/// Loop iterations between chances for the host to deliver interrupts.
pub const POLL_INTERVAL: u32 = 1 << 16;

/// Id of the replaceable SDK function entered most recently on any thread; a breadcrumb for
/// diagnosing hangs.
pub static LAST_HLE: AtomicUsize = AtomicUsize::new(0);
/// Guest return address of that call.
pub static LAST_HLE_CALLER: AtomicU32 = AtomicU32::new(0);

pub struct SampleEntry {
    pub lr: AtomicU32,
    pub count: AtomicU32,
}

pub const SAMPLE_SLOTS: usize = 256;
pub static SAMPLE_TABLE: [SampleEntry; SAMPLE_SLOTS] = [const { SampleEntry { lr: AtomicU32::new(0), count: AtomicU32::new(0) } }; SAMPLE_SLOTS];
pub static SAMPLE_TOTAL: AtomicU32 = AtomicU32::new(0);

pub const PROFILE_DETAILED: bool = false;
pub const MAX_HLE_FUNCS: usize = 1024;
pub static HLE_CALL_COUNTS: [AtomicU32; MAX_HLE_FUNCS] = [const { AtomicU32::new(0) }; MAX_HLE_FUNCS];
pub static HLE_CALL_TICKS: [AtomicU64; MAX_HLE_FUNCS] = [const { AtomicU64::new(0) }; MAX_HLE_FUNCS];

#[inline(always)]
pub fn in_ram(a: u32, len: usize) -> bool {
    a >= mem::RAM_BASE && a.wrapping_sub(mem::RAM_BASE) <= mem::RAM_SIZE.saturating_sub(len as u32)
}

#[inline(always)]
pub unsafe fn mptr(mem: *mut u8, a: u32) -> *mut u8 {
    #[cfg(target_os = "none")]
    {
        mem.add((a & 0x07FF_FFFF) as usize)
    }
    #[cfg(not(target_os = "none"))]
    {
        mem.add(a as usize)
    }
}

#[inline(always)]
pub fn mr8(mem: *mut u8, a: u32) -> u8 {
    unsafe { mptr(mem, a).read() }
}
#[inline(always)]
pub fn mr16(mem: *mut u8, a: u32) -> u16 {
    unsafe { u16::from_be((mptr(mem, a) as *const u16).read_unaligned()) }
}
#[inline(always)]
pub fn mr32(mem: *mut u8, a: u32) -> u32 {
    unsafe { u32::from_be((mptr(mem, a) as *const u32).read_unaligned()) }
}
#[inline(always)]
pub fn mr64(mem: *mut u8, a: u32) -> u64 {
    unsafe { u64::from_be((mptr(mem, a) as *const u64).read_unaligned()) }
}
#[inline(always)]
pub fn mw8(mem: *mut u8, a: u32, v: u8) {
    unsafe { mptr(mem, a).write(v) }
}
#[inline(always)]
pub fn mw16(mem: *mut u8, a: u32, v: u16) {
    unsafe { (mptr(mem, a) as *mut u16).write_unaligned(v.to_be()) }
}
#[inline(always)]
pub fn mw32(mem: *mut u8, a: u32, v: u32) {
    unsafe { (mptr(mem, a) as *mut u32).write_unaligned(v.to_be()) }
}
#[inline(always)]
pub fn mw64(mem: *mut u8, a: u32, v: u64) {
    unsafe { (mptr(mem, a) as *mut u64).write_unaligned(v.to_be()) }
}
#[inline(always)]
pub fn dcbz_mem(mem: *mut u8, ea: u32) {
    let line = ea & !31;
    unsafe {
        let p = mptr(mem, line) as *mut u64;
        p.write_unaligned(0);
        p.add(1).write_unaligned(0);
        p.add(2).write_unaligned(0);
        p.add(3).write_unaligned(0);
    }
}

#[inline(always)]
pub fn add3_val(a: u32, b: u32, carry: u32) -> (u32, u32) {
    let sum = a as u64 + b as u64 + carry as u64;
    (sum as u32, (sum >> 32) as u32)
}
#[inline(always)]
pub fn slw_val(v: u32, n: u32) -> u32 {
    if n & 32 != 0 { 0 } else { v << (n & 31) }
}
#[inline(always)]
pub fn srw_val(v: u32, n: u32) -> u32 {
    if n & 32 != 0 { 0 } else { v >> (n & 31) }
}
#[inline(always)]
pub fn sraw_val(v: u32, n: u32) -> (u32, u32) {
    let s = v as i32;
    if n & 32 != 0 {
        ((s >> 31) as u32, (s < 0) as u32)
    } else {
        let n = n & 31;
        ((s >> n) as u32, (s < 0 && v & ((1u32 << n) - 1) != 0) as u32)
    }
}
#[inline(always)]
pub fn cmps_val(a: i32, b: i32) -> u8 {
    if a < b { 8 } else if a > b { 4 } else { 2 }
}
#[inline(always)]
pub fn cmpu_val(a: u32, b: u32) -> u8 {
    if a < b { 8 } else if a > b { 4 } else { 2 }
}
#[inline(always)]
pub fn fcmp_val(a: f64, b: f64) -> u8 {
    if a < b { 8 } else if a > b { 4 } else if a == b { 2 } else { 1 }
}
#[inline(always)]
pub fn fctiwz_val(v: f64) -> f64 {
    f64::from_bits(v as i32 as u32 as u64)
}
#[inline(always)]
pub fn fctiw_val(v: f64) -> f64 {
    #[cfg(feature = "std")]
    {
        f64::from_bits(v.round_ties_even() as i32 as u32 as u64)
    }
    #[cfg(not(feature = "std"))]
    {
        let rounded: i32;
        unsafe {
            core::arch::asm!("fcvtns {:w}, {:d}", out(reg) rounded, in(vreg) v);
        }
        f64::from_bits(rounded as u32 as u64)
    }
}

#[inline(always)]
pub fn sample_guest_pc(lr: u32) {
    if lr == 0 { return; }
    SAMPLE_TOTAL.fetch_add(1, Ordering::Relaxed);
    let hash = ((lr >> 2) ^ (lr >> 6) ^ (lr >> 10)) as usize;
    for offset in 0..8 {
        let idx = (hash + offset) & (SAMPLE_SLOTS - 1);
        let entry = &SAMPLE_TABLE[idx];
        let curr = entry.lr.load(Ordering::Relaxed);
        if curr == lr {
            entry.count.fetch_add(1, Ordering::Relaxed);
            return;
        } else if curr == 0 {
            if entry.lr.compare_exchange(0, lr, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                entry.count.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
    }
}

/// Guest CPU state plus everything recompiled code needs to reach.
pub struct Ctx {
    pub r: [u32; 32],
    /// FPRs: `f` is the double / paired-single slot 0, `ps1` is paired-single slot 1.
    pub f: [f64; 32],
    pub ps1: [f64; 32],
    /// One entry per CR field: LT=8, GT=4, EQ=2, SO=1.
    pub cr: [u8; 8],
    pub lr: u32,
    pub ctr: u32,
    /// XER carry bit (0 or 1).
    pub ca: u32,
    pub xer_rest: u32,
    pub msr: u32,
    pub fpscr: u32,
    /// Every other special-purpose register, including the GQRs at 912..920.
    pub spr: [u32; 1024],
    /// Base of the 4 GB guest address space.
    pub mem: *mut u8,
    /// Bytes the guest has written to the GX write-gather pipe and the host has not consumed.
    pub fifo: Vec<u8>,
    /// Number of bytes written to the branchless WGPIPE buffer at `mem + 0x0200_0000`.
    pub fifo_len: usize,
    /// Loop iterations left before `poll_hook` runs.
    pub budget: u32,
    /// Called every `POLL_INTERVAL` loop iterations so the host can deliver interrupts to
    /// code that loops without calling anything. It must leave the registers as it found them.
    pub poll_hook: Option<fn(&mut Ctx)>,

    table_base: u32,
    table_ptr: *const Option<GuestFn>,
    table_len: usize,
    table: Vec<Option<GuestFn>>,
    hle_ptr: *const Hle,
    hle: Vec<Hle>,
    hle_names: &'static [&'static str],
    hle_logged: Vec<bool>,
    /// Survey mode: unimplemented SDK calls are logged once and return 0 instead of panicking.
    pub survey: bool,
    funcs: &'static [(u32, GuestFn, &'static str)],
    pub guest_thread: u32,
    pub sample_countdown: u32,
    #[cfg(feature = "std")]
    start: std::time::Instant,
}

unsafe impl Send for Ctx {}

impl Ctx {
    pub fn new(
        funcs: &'static [(u32, GuestFn, &'static str)],
        hle_names: &'static [&'static str],
        policy: fn(&str) -> Hle,
    ) -> Box<Ctx> {
        let table_base = funcs.first().map_or(0, |f| f.0);
        let table_end = funcs.last().map_or(0, |f| f.0 + 4);
        let mut table = vec![None; ((table_end - table_base) / 4) as usize];
        for &(addr, f, _) in funcs {
            table[((addr - table_base) / 4) as usize] = Some(f);
        }
        let table_ptr = table.as_ptr();
        let table_len = table.len();
        let hle: Vec<Hle> = hle_names.iter().map(|n| policy(n)).collect();
        let hle_ptr = hle.as_ptr();
        Box::new(Ctx {
            r: [0; 32],
            f: [0.0; 32],
            ps1: [0.0; 32],
            cr: [0; 8],
            lr: 0,
            ctr: 0,
            ca: 0,
            xer_rest: 0,
            msr: 0x2000, // FP available
            fpscr: 0,
            spr: [0; 1024],
            mem: mem::alloc_guest_memory(),
            fifo: Vec::new(),
            fifo_len: 0,
            budget: POLL_INTERVAL,
            poll_hook: None,
            table_base,
            table_ptr,
            table_len,
            table,
            hle_ptr,
            hle,
            hle_names,
            hle_logged: vec![false; hle_names.len()],
            survey: false,
            funcs,
            guest_thread: 0x817E_0000,
            sample_countdown: 128,
            #[cfg(feature = "std")]
            start: std::time::Instant::now(),
        })
    }

    /// A context for another guest thread: fresh registers, but the same memory, function
    /// tables, clock and special-purpose registers (the quantization registers among them).
    pub fn fork(&self) -> Box<Ctx> {
        let table = self.table.clone();
        let table_ptr = table.as_ptr();
        let table_len = table.len();
        let hle = self.hle.clone();
        let hle_ptr = hle.as_ptr();
        Box::new(Ctx {
            r: [0; 32],
            f: [0.0; 32],
            ps1: [0.0; 32],
            cr: [0; 8],
            lr: 0,
            ctr: 0,
            ca: 0,
            xer_rest: 0,
            msr: self.msr,
            fpscr: 0,
            spr: self.spr,
            mem: self.mem,
            fifo: Vec::new(),
            fifo_len: 0,
            budget: POLL_INTERVAL,
            poll_hook: self.poll_hook,
            table_base: self.table_base,
            table_ptr,
            table_len,
            table,
            hle_ptr,
            hle,
            hle_names: self.hle_names,
            hle_logged: self.hle_logged.clone(),
            survey: self.survey,
            funcs: self.funcs,
            guest_thread: self.guest_thread,
            sample_countdown: 128,
            #[cfg(feature = "std")]
            start: self.start,
        })
    }

    /// Copies a DOL executable's sections into guest memory and returns its entry point.
    pub fn load_dol(&mut self, dol: &[u8]) -> u32 {
        let be = |o: usize| u32::from_be_bytes([dol[o], dol[o + 1], dol[o + 2], dol[o + 3]]);
        for i in 0..18 {
            let (off, addr, size) = (be(i * 4) as usize, be(0x48 + i * 4), be(0x90 + i * 4) as usize);
            if size != 0 {
                self.slice_mut(addr, size).copy_from_slice(&dol[off..off + size]);
            }
        }
        be(0xE0)
    }

    pub fn func_by_name(&self, name: &str) -> Option<GuestFn> {
        self.funcs.iter().find(|f| f.2 == name).map(|f| f.1)
    }

    /// Symbol name of the function containing `addr`, for diagnostics.
    pub fn func_name(&self, addr: u32) -> &'static str {
        let i = self.funcs.partition_point(|f| f.0 <= addr);
        if i == 0 {
            "?"
        } else {
            self.funcs[i - 1].2
        }
    }

    // ---- memory ------------------------------------------------------------------------

    /// Copies `len` bytes out of guest RAM.
    pub fn read_bytes(&mut self, addr: u32, len: usize) -> Vec<u8> {
        self.slice_mut(addr, len).to_vec()
    }

    /// Translates a 32-bit GameCube guest address to host memory.
    #[inline(always)]
    pub unsafe fn guest_ptr(&self, a: u32) -> *mut u8 {
        #[cfg(target_os = "none")]
        {
            self.mem.add((a & 0x07FF_FFFF) as usize)
        }
        #[cfg(not(target_os = "none"))]
        {
            self.mem.add(a as usize)
        }
    }

    pub fn slice_mut(&mut self, addr: u32, len: usize) -> &mut [u8] {
        #[cfg(target_os = "none")]
        unsafe {
            core::slice::from_raw_parts_mut(self.guest_ptr(addr), len)
        }
        #[cfg(not(target_os = "none"))]
        {
            let (lo, hi) = (addr as u64, addr as u64 + len as u64);
            let ram = mem::RAM_BASE as u64..=(mem::RAM_BASE + mem::RAM_SIZE) as u64;
            assert!(ram.contains(&lo) && ram.contains(&hi), "guest range is outside RAM");
            unsafe { core::slice::from_raw_parts_mut(self.mem.add(addr as usize), len) }
        }
    }

    #[inline(always)]
    pub fn r8(&self, a: u32) -> u8 {
        unsafe { self.guest_ptr(a).read() }
    }
    #[inline(always)]
    pub fn r16(&self, a: u32) -> u16 {
        unsafe { u16::from_be((self.guest_ptr(a) as *const u16).read_unaligned()) }
    }
    #[inline(always)]
    pub fn r32(&self, a: u32) -> u32 {
        unsafe { u32::from_be((self.guest_ptr(a) as *const u32).read_unaligned()) }
    }
    #[inline(always)]
    pub fn r64(&self, a: u32) -> u64 {
        unsafe { u64::from_be((self.guest_ptr(a) as *const u64).read_unaligned()) }
    }
    #[inline(always)]
    pub fn w8(&mut self, a: u32, v: u8) {
        unsafe { self.guest_ptr(a).write(v) }
    }
    #[inline(always)]
    pub fn w16(&mut self, a: u32, v: u16) {
        unsafe { (self.guest_ptr(a) as *mut u16).write_unaligned(v.to_be()) }
    }
    #[inline(always)]
    pub fn w32(&mut self, a: u32, v: u32) {
        unsafe { (self.guest_ptr(a) as *mut u32).write_unaligned(v.to_be()) }
    }
    #[inline(always)]
    pub fn w64(&mut self, a: u32, v: u64) {
        unsafe { (self.guest_ptr(a) as *mut u64).write_unaligned(v.to_be()) }
    }

    // Stores that may target the GX write-gather pipe.
    #[cfg(target_os = "none")]
    #[inline(always)]
    pub unsafe fn gw_ptr(&mut self, a: u32, size: usize) -> *mut u8 {
        let is_wg = (a == mem::WGPIPE) as usize;
        let mask = is_wg.wrapping_neg();
        let ram_off = (a & 0x07FF_FFFF) as usize;
        let fifo_off = 0x0200_0000usize.wrapping_add(self.fifo_len & 0x01FF_FFFF);
        let off = ram_off ^ ((ram_off ^ fifo_off) & mask);
        self.fifo_len = (self.fifo_len.wrapping_add(size & mask)) & 0x01FF_FFFF;
        self.mem.add(off)
    }

    #[inline(always)]
    pub fn gw8(&mut self, a: u32, v: u8) {
        #[cfg(target_os = "none")]
        unsafe {
            self.gw_ptr(a, 1).write(v);
        }
        #[cfg(not(target_os = "none"))]
        if a == mem::WGPIPE {
            self.fifo.push(v)
        } else {
            self.w8(a, v)
        }
    }
    #[inline(always)]
    pub fn gw16(&mut self, a: u32, v: u16) {
        #[cfg(target_os = "none")]
        unsafe {
            (self.gw_ptr(a, 2) as *mut u16).write_unaligned(v.to_be());
        }
        #[cfg(not(target_os = "none"))]
        if a == mem::WGPIPE {
            self.fifo.extend_from_slice(&v.to_be_bytes())
        } else {
            self.w16(a, v)
        }
    }
    #[inline(always)]
    pub fn gw32(&mut self, a: u32, v: u32) {
        #[cfg(target_os = "none")]
        unsafe {
            (self.gw_ptr(a, 4) as *mut u32).write_unaligned(v.to_be());
        }
        #[cfg(not(target_os = "none"))]
        if a == mem::WGPIPE {
            self.fifo.extend_from_slice(&v.to_be_bytes())
        } else {
            self.w32(a, v)
        }
    }
    #[inline(always)]
    pub fn gw64(&mut self, a: u32, v: u64) {
        #[cfg(target_os = "none")]
        unsafe {
            (self.gw_ptr(a, 8) as *mut u64).write_unaligned(v.to_be());
        }
        #[cfg(not(target_os = "none"))]
        if a == mem::WGPIPE {
            self.fifo.extend_from_slice(&v.to_be_bytes())
        } else {
            self.w64(a, v)
        }
    }
    #[inline(always)]
    pub fn gstfs(&mut self, s: usize, ea: u32) {
        self.gw32(ea, (self.f[s] as f32).to_bits());
    }
    #[inline(always)]
    pub fn gstfd(&mut self, s: usize, ea: u32) {
        self.gw64(ea, self.f[s].to_bits());
    }

    #[inline(always)]
    pub fn lmw(&mut self, first: usize, ea: u32) {
        for (i, reg) in (first..32).enumerate() {
            self.r[reg] = self.r32(ea.wrapping_add(i as u32 * 4));
        }
    }
    #[inline(always)]
    pub fn stmw(&mut self, first: usize, ea: u32) {
        for (i, reg) in (first..32).enumerate() {
            self.w32(ea.wrapping_add(i as u32 * 4), self.r[reg]);
        }
    }
    #[inline(always)]
    pub fn dcbz(&mut self, _reg: usize, ea: u32) {
        let line = ea & !31;
        for i in 0..4 {
            self.w64(line + i * 8, 0);
        }
    }

    // ---- integer -----------------------------------------------------------------------

    /// a + b + carry-in, recording carry-out in XER[CA].
    #[inline(always)]
    pub fn add3(&mut self, a: u32, b: u32, carry: u32) -> u32 {
        let sum = a as u64 + b as u64 + carry as u64;
        self.ca = (sum >> 32) as u32;
        sum as u32
    }
    #[inline(always)]
    pub fn slw(&self, v: u32, n: u32) -> u32 {
        if n & 32 != 0 {
            0
        } else {
            v << (n & 31)
        }
    }
    #[inline(always)]
    pub fn srw(&self, v: u32, n: u32) -> u32 {
        if n & 32 != 0 {
            0
        } else {
            v >> (n & 31)
        }
    }
    #[inline(always)]
    pub fn sraw(&mut self, v: u32, n: u32) -> u32 {
        let s = v as i32;
        if n & 32 != 0 {
            self.ca = (s < 0) as u32;
            (s >> 31) as u32
        } else {
            let n = n & 31;
            self.ca = (s < 0 && v & ((1u32 << n) - 1) != 0) as u32;
            (s >> n) as u32
        }
    }

    // ---- condition register ------------------------------------------------------------

    #[inline(always)]
    pub fn cmps(&mut self, crf: usize, a: i32, b: i32) {
        self.cr[crf] = if a < b { 8 } else if a > b { 4 } else { 2 };
    }
    #[inline(always)]
    pub fn cmpu(&mut self, crf: usize, a: u32, b: u32) {
        self.cr[crf] = if a < b { 8 } else if a > b { 4 } else { 2 };
    }
    #[inline(always)]
    pub fn fcmp(&mut self, crf: usize, a: f64, b: f64) {
        self.cr[crf] = if a < b { 8 } else if a > b { 4 } else if a == b { 2 } else { 1 };
    }
    /// CR0 update for record-form (`.`) instructions.
    #[inline(always)]
    pub fn rc(&mut self, v: u32) {
        self.cmps(0, v as i32, 0);
    }
    #[inline(always)]
    pub fn crbit(&self, crf: usize, mask: u8) -> bool {
        self.cr[crf] & mask != 0
    }
    #[inline(always)]
    pub fn crset(&mut self, crf: usize, mask: u8, v: bool) {
        self.cr[crf] = if v { self.cr[crf] | mask } else { self.cr[crf] & !mask };
    }
    pub fn get_cr(&self) -> u32 {
        self.cr.iter().fold(0, |acc, &f| (acc << 4) | f as u32)
    }
    /// mtcrf: `mask` has one bit per CR field, most significant = CR0.
    pub fn set_cr(&mut self, mask: u32, v: u32) {
        for i in 0..8 {
            if mask & (0x80 >> i) != 0 {
                self.cr[i] = ((v >> (28 - 4 * i)) & 0xF) as u8;
            }
        }
    }
    pub fn get_xer(&self) -> u32 {
        self.xer_rest | (self.ca << 29)
    }
    pub fn set_xer(&mut self, v: u32) {
        self.ca = (v >> 29) & 1;
        self.xer_rest = v & !(1 << 29);
    }

    // ---- floating point ----------------------------------------------------------------

    /// Result of a single-precision operation: rounded to f32 and mirrored into both slots.
    #[inline(always)]
    pub fn set_fs(&mut self, d: usize, v: f64) {
        let v = v as f32 as f64;
        self.f[d] = v;
        self.ps1[d] = v;
    }
    #[inline(always)]
    pub fn set_ps(&mut self, d: usize, v0: f64, v1: f64) {
        self.f[d] = v0 as f32 as f64;
        self.ps1[d] = v1 as f32 as f64;
    }
    #[inline(always)]
    pub fn lfs(&mut self, d: usize, ea: u32) {
        let v = f32::from_bits(self.r32(ea)) as f64;
        self.f[d] = v;
        self.ps1[d] = v;
    }
    #[inline(always)]
    pub fn lfd(&mut self, d: usize, ea: u32) {
        self.f[d] = f64::from_bits(self.r64(ea));
    }
    #[inline(always)]
    pub fn stfs(&mut self, s: usize, ea: u32) {
        self.w32(ea, (self.f[s] as f32).to_bits());
    }
    #[inline(always)]
    pub fn stfd(&mut self, s: usize, ea: u32) {
        self.w64(ea, self.f[s].to_bits());
    }
    #[inline(always)]
    pub fn stfiw(&mut self, s: usize, ea: u32) {
        self.w32(ea, self.f[s].to_bits() as u32);
    }
    /// Float to integer, truncating; the integer lands in the low word of the register.
    #[inline(always)]
    pub fn fctiwz(&self, v: f64) -> f64 {
        fctiwz_val(v)
    }
    #[inline(always)]
    pub fn fctiw(&self, v: f64) -> f64 {
        fctiw_val(v)
    }

    /// Value-returning quantized paired-single load using unaliased `mem`.
    #[inline(always)]
    pub fn psq_l_val(&self, mem: *mut u8, ea: u32, single: bool, i: usize) -> (f64, f64) {
        let gqr = self.spr[912 + i];
        if (gqr & 0x0004_0000) == 0 {
            if single {
                (f32::from_bits(mr32(mem, ea)) as f64, 1.0)
            } else {
                let pair = mr64(mem, ea);
                (f32::from_bits((pair >> 32) as u32) as f64, f32::from_bits(pair as u32) as f64)
            }
        } else {
            self.psq_l_val_slow(ea, single, gqr)
        }
    }

    #[cold]
    fn psq_l_val_slow(&self, ea: u32, single: bool, gqr: u32) -> (f64, f64) {
        let (ty, scale) = ((gqr >> 16) & 7, (gqr >> 24) & 0x3F);
        let (v0, size) = self.dequantize(ea, ty, scale);
        let v1 = if single { 1.0 } else { self.dequantize(ea.wrapping_add(size), ty, scale).0 };
        (v0, v1)
    }

    #[inline(always)]
    pub fn psq_st_val(&mut self, mem: *mut u8, ea: u32, f0: f64, f1: f64, single: bool, i: usize) {
        let _ = mem;
        let gqr = self.spr[912 + i];
        if (gqr & 4) == 0 {
            let w0 = (f0 as f32).to_bits();
            if single {
                self.gw32(ea, w0);
            } else {
                let w1 = (f1 as f32).to_bits();
                let pair = ((w0 as u64) << 32) | (w1 as u64);
                #[cfg(target_os = "none")]
                unsafe {
                    (self.gw_ptr(ea, 8) as *mut u64).write_unaligned(pair.to_be());
                }
                #[cfg(not(target_os = "none"))]
                if ea == mem::WGPIPE {
                    self.fifo.extend_from_slice(&pair.to_be_bytes());
                } else {
                    mw64(mem, ea, pair);
                }
            }
        } else {
            self.psq_st_val_slow(ea, f0, f1, single, gqr);
        }
    }

    #[cold]
    fn psq_st_val_slow(&mut self, ea: u32, f0: f64, f1: f64, single: bool, gqr: u32) {
        let (ty, scale) = (gqr & 7, (gqr >> 8) & 0x3F);
        let (b0, size) = Self::quantize(ty, scale, f0);
        let (b1, _) = Self::quantize(ty, scale, f1);
        let count = if single { 1 } else { 2 };
        for (i, bytes) in [b0, b1].iter().take(count).enumerate() {
            #[cfg(target_os = "none")]
            unsafe {
                let is_wg = (ea == mem::WGPIPE) as u32;
                let at = ea.wrapping_add(((i * size) as u32) * (1 - is_wg));
                let p = self.gw_ptr(at, size);
                core::ptr::copy_nonoverlapping(bytes.as_ptr(), p, size);
            }
            #[cfg(not(target_os = "none"))]
            if ea == mem::WGPIPE {
                self.fifo.extend_from_slice(&bytes[..size]);
            } else {
                let at = ea.wrapping_add((i * size) as u32);
                self.slice_unchecked(at, size).copy_from_slice(&bytes[..size]);
            }
        }
    }

    /// Quantized paired-single load: element type and scale come from GQR[i].
    #[inline(always)]
    pub fn psq_l(&mut self, d: usize, ea: u32, single: bool, i: usize) {
        let (v0, v1) = self.psq_l_val(self.mem, ea, single, i);
        self.f[d] = v0;
        self.ps1[d] = v1;
    }

    #[inline(always)]
    pub fn psq_st(&mut self, s: usize, ea: u32, single: bool, i: usize) {
        self.psq_st_val(self.mem, ea, self.f[s], self.ps1[s], single, i);
    }

    #[inline(always)]
    pub fn pow2(exp: i32) -> f64 {
        if exp < -1022 {
            0.0
        } else if exp > 1023 {
            f64::INFINITY
        } else {
            f64::from_bits(((1023 + exp) as u64) << 52)
        }
    }

    #[inline(always)]
    pub fn dequantize(&self, ea: u32, ty: u32, scale: u32) -> (f64, u32) {
        // scale is a signed 6-bit exponent
        let k = Self::pow2(-(((scale ^ 32) as i32) - 32));
        match ty {
            4 => (self.r8(ea) as f64 * k, 1),
            5 => (self.r16(ea) as f64 * k, 2),
            6 => (self.r8(ea) as i8 as f64 * k, 1),
            7 => (self.r16(ea) as i16 as f64 * k, 2),
            _ => (f32::from_bits(self.r32(ea)) as f64, 4),
        }
    }
    /// Big-endian encoding of `v` in quantization type `ty`, and its length in bytes.
    #[inline(always)]
    pub fn quantize(ty: u32, scale: u32, v: f64) -> ([u8; 4], usize) {
        let k = Self::pow2(((scale ^ 32) as i32) - 32);
        let mut out = [0u8; 4];
        let size = match ty {
            4 => {
                out[0] = (v * k) as u8;
                1
            }
            5 => {
                out[..2].copy_from_slice(&((v * k) as u16).to_be_bytes());
                2
            }
            6 => {
                out[0] = (v * k) as i8 as u8;
                1
            }
            7 => {
                out[..2].copy_from_slice(&((v * k) as i16).to_be_bytes());
                2
            }
            _ => {
                out = (v as f32).to_bits().to_be_bytes();
                4
            }
        };
        (out, size)
    }

    /// Raw view of guest memory with no range check; faults like any other guest access.
    #[inline(always)]
    fn slice_unchecked(&mut self, addr: u32, len: usize) -> &mut [u8] {
        #[cfg(target_os = "none")]
        unsafe {
            core::slice::from_raw_parts_mut(self.guest_ptr(addr), len)
        }
        #[cfg(not(target_os = "none"))]
        unsafe { core::slice::from_raw_parts_mut(self.mem.add(addr as usize), len) }
    }

    /// Time base: ticks at 1/4 of the 162 MHz bus clock.
    #[inline(always)]
    pub fn timebase(&self) -> u64 {
        #[cfg(feature = "std")]
        {
            (self.start.elapsed().as_nanos() * 40_500_000 / 1_000_000_000) as u64
        }
        #[cfg(not(feature = "std"))]
        {
            let ticks: u64;
            unsafe {
                core::arch::asm!("mrs {}, cntvct_el0", out(reg) ticks, options(pure, nomem, nostack));
            }
            ticks.wrapping_mul(135) / 64
        }
    }

    #[inline(always)]
    pub fn now_ticks(&self) -> u64 {
        #[cfg(feature = "std")]
        {
            self.start.elapsed().as_nanos() as u64
        }
        #[cfg(not(feature = "std"))]
        {
            let ticks: u64;
            unsafe {
                core::arch::asm!("mrs {}, cntvct_el0", out(reg) ticks, options(pure, nomem, nostack));
            }
            ticks
        }
    }

    // ---- control flow ------------------------------------------------------------------

    #[inline(always)]
    pub fn sample_tick(&mut self) {
        self.sample_countdown = self.sample_countdown.wrapping_sub(1);
        if self.sample_countdown == 0 {
            self.sample_countdown = 16;
            sample_guest_pc(self.lr);
        }
    }

    #[inline(always)]
    pub fn sample_tick_pc(&mut self, pc: u32) {
        self.sample_countdown = self.sample_countdown.wrapping_sub(1);
        if self.sample_countdown == 0 {
            self.sample_countdown = 16;
            sample_guest_pc(pc);
        }
    }

    /// Counts one loop iteration (a backward branch) in recompiled code.
    #[inline(always)]
    pub fn back_edge(&mut self) {
        if PROFILE_DETAILED {
            self.sample_tick();
        }
        self.budget -= 1;
        if self.budget == 0 {
            self.poll();
        }
    }

    #[cold]
    #[inline(never)]
    pub fn poll(&mut self) {
        self.budget = POLL_INTERVAL;
        if let Some(hook) = self.poll_hook {
            hook(self);
        }
    }

    /// Indirect call through a guest address (function pointers, virtual calls).
    #[inline(always)]
    pub fn call(&mut self, addr: u32) {
        if PROFILE_DETAILED {
            self.sample_tick_pc(addr);
        }
        let idx = (addr.wrapping_sub(self.table_base) >> 2) as usize;
        if (addr & 3) == 0 && idx < self.table_len {
            if let Some(f) = unsafe { *self.table_ptr.add(idx) } {
                f(self);
                return;
            }
        }
        self.call_bad(addr);
    }

    #[cold]
    fn call_bad(&self, addr: u32) {
        let _ = addr;
        #[cfg(feature = "std")]
        eprintln!("indirect call to {addr:#010x}, which is not a function (lr = {:#010x})", self.lr);
    }

    /// Entry check for a replaceable SDK function; true means a native version handled the call.
    #[inline(always)]
    pub fn hle(&mut self, id: usize) -> bool {
        if !PROFILE_DETAILED {
            return match unsafe { *self.hle_ptr.add(id) } {
                Hle::Original => false,
                Hle::Native(f) => {
                    f(self);
                    true
                }
                Hle::Before(f) => {
                    f(self);
                    false
                }
                Hle::Replace(f) => f(self),
                Hle::Unimplemented => self.hle_unimplemented(id),
            };
        }
        self.hle_profiled(id)
    }

    #[cold]
    fn hle_unimplemented(&mut self, id: usize) -> bool {
        if self.survey {
            if !core::mem::replace(&mut self.hle_logged[id], true) {
                #[cfg(feature = "std")]
                eprintln!("[survey] {} (first called from {})", self.hle_names[id], self.func_name(self.lr));
            }
            self.r[3] = 0;
            true
        } else {
            panic!(
                "SDK function {} has no native implementation yet (called from {}, lr = {:#010x})",
                self.hle_names[id],
                self.func_name(self.lr),
                self.lr
            )
        }
    }

    #[cold]
    fn hle_profiled(&mut self, id: usize) -> bool {
        LAST_HLE.store(id, Ordering::Relaxed);
        LAST_HLE_CALLER.store(self.lr, Ordering::Relaxed);
        let t0 = self.now_ticks();
        let ret = match self.hle[id] {
            Hle::Native(f) => {
                f(self);
                true
            }
            Hle::Original => false,
            Hle::Before(f) => {
                f(self);
                false
            }
            Hle::Replace(f) => f(self),
            Hle::Unimplemented => self.hle_unimplemented(id),
        };
        let dt = self.now_ticks().saturating_sub(t0);
        if id < MAX_HLE_FUNCS {
            HLE_CALL_TICKS[id].fetch_add(dt, Ordering::Relaxed);
            HLE_CALL_COUNTS[id].fetch_add(1, Ordering::Relaxed);
        }
        ret
    }

    pub fn drain_sample_table(&self) -> (u32, Vec<(u32, u32)>) {
        let total = SAMPLE_TOTAL.swap(0, Ordering::Relaxed);
        let mut samples = Vec::new();
        for entry in &SAMPLE_TABLE {
            let count = entry.count.swap(0, Ordering::Relaxed);
            let lr = entry.lr.swap(0, Ordering::Relaxed);
            if count > 0 && lr != 0 {
                samples.push((lr, count));
            }
        }
        samples.sort_unstable_by(|a, b| b.1.cmp(&a.1));
        (total, samples)
    }

    pub fn drain_hle_stats(&self) -> Vec<(&'static str, u32, u64)> {
        let mut stats = Vec::new();
        for (id, &name) in self.hle_names.iter().enumerate() {
            if id < MAX_HLE_FUNCS {
                let count = HLE_CALL_COUNTS[id].swap(0, Ordering::Relaxed);
                let ticks = HLE_CALL_TICKS[id].swap(0, Ordering::Relaxed);
                if count > 0 {
                    stats.push((name, count, ticks));
                }
            }
        }
        stats.sort_unstable_by(|a, b| b.2.cmp(&a.2).then_with(|| b.1.cmp(&a.1)));
        stats
    }

    #[cold]
    pub fn bad_branch(&mut self, pc: u32, target: u32) {
        panic!("{pc:#010x} ({}) branches into the middle of {} at {target:#010x}", self.func_name(pc), self.func_name(target))
    }
    #[cold]
    pub fn bad_jump(&mut self, func: u32, pc: u32) {
        panic!("computed jump in {} ({func:#010x}) to {pc:#010x}, which is not a known block", self.func_name(func))
    }
    #[cold]
    pub fn invalid(&mut self, pc: u32, word: u32) {
        panic!("untranslated instruction {word:#010x} at {pc:#010x} ({})", self.func_name(pc))
    }
    #[cold]
    pub fn trap(&mut self, what: &str) {
        panic!("privileged instruction `{what}` executed; its SDK function needs a native replacement")
    }
}
