//! GameCube DSP/AX 64-voice hardware audio mixer & ARAM emulation for Switch (`#![no_std]` + `alloc`).
//! Mixes 5 ms frames (160 stereo samples @ 32 kHz) into a lock-free SPSC ring buffer drained by SDL2.

#![allow(static_mut_refs)]

use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use runtime::Ctx;

pub const ARAM_SIZE: usize = 16 << 20;
const FRAME_SAMPLES: usize = 160;
const BLOCK_COUNT: u32 = 64;
const BLOCK_SIZE: u32 = 244;

/// Lock-free SPSC ring buffer holding stereo 16-bit PCM frames (power-of-two capacity).
const RING_CAP: usize = 8192;
const RING_MASK: usize = RING_CAP - 1;

static mut RING_BUF: [[i16; 2]; RING_CAP] = [[0; 2]; RING_CAP];
static RING_WRITE: AtomicUsize = AtomicUsize::new(0);
static RING_READ: AtomicUsize = AtomicUsize::new(0);

pub static MIXED_FRAMES: AtomicU32 = AtomicU32::new(0);
pub static DRAINED_FRAMES: AtomicU32 = AtomicU32::new(0);
pub static ACTIVE_VOICES: AtomicU32 = AtomicU32::new(0);
pub static PEAK_LEVEL: AtomicU32 = AtomicU32::new(0);
pub static ARAM_TRANSFERS: AtomicU32 = AtomicU32::new(0);

static mut ARAM: Option<Vec<u8>> = None;
static mut AX_BLOCKS: u32 = 0;

pub fn init() {
    unsafe {
        if ARAM.is_none() {
            ARAM = Some(vec![0u8; ARAM_SIZE]);
        }
        AX_BLOCKS = 0;
    }
    RING_WRITE.store(0, Ordering::Relaxed);
    RING_READ.store(0, Ordering::Relaxed);
    MIXED_FRAMES.store(0, Ordering::Relaxed);
    DRAINED_FRAMES.store(0, Ordering::Relaxed);
    ACTIVE_VOICES.store(0, Ordering::Relaxed);
    PEAK_LEVEL.store(0, Ordering::Relaxed);
    ARAM_TRANSFERS.store(0, Ordering::Relaxed);
}

pub fn transfer(c: &mut Ctx, to_aram: bool, main: u32, aram_addr: u32, len: u32) {
    let (aram_off, len) = (aram_addr as usize, len as usize);
    let main_ok = main >= runtime::mem::RAM_BASE
        && main as u64 + len as u64 <= (runtime::mem::RAM_BASE + runtime::mem::RAM_SIZE) as u64;
    if !main_ok || aram_off.saturating_add(len) > ARAM_SIZE || len == 0 {
        return;
    }
    ARAM_TRANSFERS.fetch_add(1, Ordering::Relaxed);
    let memory = c.slice_mut(main, len);
    unsafe {
        let Some(aram) = ARAM.as_mut() else { return };
        let stored = &mut aram[aram_off..aram_off + len];
        if to_aram {
            stored.copy_from_slice(memory);
        } else {
            memory.copy_from_slice(stored);
        }
    }
}

mod pb {
    pub const SRC_TYPE: u32 = 0x08;
    pub const MIXER_CONTROL: u32 = 0x0C;
    pub const RUNNING: u32 = 0x0E;
    pub const IS_STREAM: u32 = 0x10;
    pub const MIX_LEFT: u32 = 0x12;
    pub const VOLUME: u32 = 0x64;
    pub const LOOPING: u32 = 0x6E;
    pub const FORMAT: u32 = 0x70;
    pub const LOOP_ADDR: u32 = 0x72;
    pub const END_ADDR: u32 = 0x76;
    pub const CUR_ADDR: u32 = 0x7A;
    pub const COEFS: u32 = 0x7E;
    pub const PRED_SCALE: u32 = 0xA0;
    pub const RATIO: u32 = 0xA6;
    pub const FRACTION: u32 = 0xAA;
    pub const LAST_SAMPLES: u32 = 0xAC;
    pub const LOOP_PRED_SCALE: u32 = 0xB4;
}

struct Source<'a> {
    aram: &'a [u8],
    format: u16,
    looping: bool,
    is_stream: bool,
    loop_addr: u32,
    end_addr: u32,
    cur: u32,
    coefs: [i32; 16],
    pred_scale: u16,
    history: [i32; 2],
    loop_state: (u16, i32, i32),
    running: bool,
}

impl Source<'_> {
    #[inline(always)]
    fn byte(&self, at: u32) -> u8 {
        let idx = at as usize;
        if idx < ARAM_SIZE {
            unsafe { *self.aram.get_unchecked(idx) }
        } else {
            0
        }
    }

    #[inline(always)]
    fn next(&mut self) -> i32 {
        if !self.running {
            return 0;
        }
        let sample = match self.format {
            0x0A => {
                let at = self.cur * 2;
                i16::from_be_bytes([self.byte(at), self.byte(at + 1)]) as i32
            }
            0x19 => (self.byte(self.cur) as i8 as i32) << 8,
            _ => {
                if self.cur & 15 == 0 {
                    self.pred_scale = self.byte(self.cur >> 1) as u16;
                    self.cur += 2;
                }
                let byte = self.byte(self.cur >> 1);
                let nibble = if self.cur & 1 == 0 { byte >> 4 } else { byte & 15 } as i32;
                let nibble = if nibble >= 8 { nibble - 16 } else { nibble };
                let scale = 1 << (self.pred_scale & 15);
                let index = ((self.pred_scale >> 4) & 7) as usize * 2;
                let predicted = (0x400
                    + self.coefs[index] as i64 * self.history[0] as i64
                    + self.coefs[index + 1] as i64 * self.history[1] as i64)
                    >> 11;
                let value = ((scale * nibble) as i64 + predicted).clamp(-32768, 32767) as i32;
                self.history = [value, self.history[0]];
                value
            }
        };
        self.cur += 1;
        if self.cur > self.end_addr {
            if self.looping {
                self.cur = self.loop_addr;
                if self.format == 0 && !self.is_stream {
                    (self.pred_scale, self.history[0], self.history[1]) = self.loop_state;
                }
            } else {
                self.running = false;
            }
        }
        sample
    }
}

fn mix_voice(c: &mut Ctx, aram: &[u8], block: u32, out: &mut [i32; FRAME_SAMPLES * 2]) {
    let mem = c.mem;
    let u = |at: u32| runtime::mr16(mem, block + at);
    let s = |at: u32| runtime::mr16(mem, block + at) as i16 as i32;
    let mut source = Source {
        aram,
        format: u(pb::FORMAT),
        looping: u(pb::LOOPING) != 0,
        is_stream: u(pb::IS_STREAM) != 0,
        loop_addr: runtime::mr32(mem, block + pb::LOOP_ADDR),
        end_addr: runtime::mr32(mem, block + pb::END_ADDR),
        cur: runtime::mr32(mem, block + pb::CUR_ADDR),
        coefs: core::array::from_fn(|k| s(pb::COEFS + k as u32 * 2)),
        pred_scale: u(pb::PRED_SCALE),
        history: [s(pb::PRED_SCALE + 2), s(pb::PRED_SCALE + 4)],
        loop_state: (
            u(pb::LOOP_PRED_SCALE),
            s(pb::LOOP_PRED_SCALE + 2),
            s(pb::LOOP_PRED_SCALE + 4),
        ),
        running: true,
    };
    let resample = u(pb::SRC_TYPE) != 2;
    let ratio = runtime::mr32(mem, block + pb::RATIO);
    let mut fraction = u(pb::FRACTION) as u32;
    let (mut older, mut newer) = (s(pb::LAST_SAMPLES + 4), s(pb::LAST_SAMPLES + 6));
    let (mut volume, volume_delta) = (u(pb::VOLUME) as i32, s(pb::VOLUME + 2));
    let (mut left, left_delta) = (u(pb::MIX_LEFT) as i32, s(pb::MIX_LEFT + 2));
    let (mut right, right_delta) = (u(pb::MIX_LEFT + 4) as i32, s(pb::MIX_LEFT + 6));
    let ramp = (u(pb::MIXER_CONTROL) & 4 != 0) && (left_delta != 0 || right_delta != 0);

    for frame in out.chunks_exact_mut(2) {
        let sample = if resample {
            let position = fraction as u64 + ratio as u64;
            for _ in 0..(position >> 16).min(16) {
                older = newer;
                newer = source.next();
            }
            fraction = (position & 0xFFFF) as u32;
            older + (((newer - older) as i64 * fraction as i64) >> 16) as i32
        } else {
            source.next()
        };
        let scaled = (sample as i64 * volume as i64) >> 15;
        let mixed = |level: i32| {
            ((scaled * level as i64) >> 15).clamp(i32::MIN as i64 / 2, i32::MAX as i64 / 2) as i32
        };
        frame[0] = frame[0].saturating_add(mixed(left));
        frame[1] = frame[1].saturating_add(mixed(right));
        if volume_delta != 0 {
            volume = (volume + volume_delta).clamp(0, 0xFFFF);
        }
        if ramp {
            left = (left + left_delta).clamp(0, 0xFFFF);
            right = (right + right_delta).clamp(0, 0xFFFF);
        }
    }

    runtime::mw32(mem, block + pb::CUR_ADDR, source.cur);
    runtime::mw16(mem, block + pb::PRED_SCALE, source.pred_scale);
    runtime::mw16(mem, block + pb::PRED_SCALE + 2, source.history[0] as u16);
    runtime::mw16(mem, block + pb::PRED_SCALE + 4, source.history[1] as u16);
    runtime::mw16(mem, block + pb::FRACTION, fraction as u16);
    runtime::mw16(mem, block + pb::LAST_SAMPLES + 4, older as u16);
    runtime::mw16(mem, block + pb::LAST_SAMPLES + 6, newer as u16);
    runtime::mw16(mem, block + pb::VOLUME, volume as u16);
    runtime::mw16(mem, block + pb::MIX_LEFT, left as u16);
    runtime::mw16(mem, block + pb::MIX_LEFT + 4, right as u16);
    if !source.running {
        runtime::mw16(mem, block + pb::RUNNING, 0);
    }
}

pub fn mix_frame(c: &mut Ctx) {
    let mut blocks = unsafe { AX_BLOCKS };
    if blocks == 0 {
        let Some(get_blocks) = c.func_by_name("__AXGetPBs") else { return };
        let saved_r3 = c.r[3];
        let saved_lr = c.lr;
        get_blocks(c);
        blocks = c.r[3];
        c.r[3] = saved_r3;
        c.lr = saved_lr;
        if blocks < runtime::mem::RAM_BASE {
            return;
        }
        unsafe {
            AX_BLOCKS = blocks;
        }
    }

    let Some(aram) = (unsafe { ARAM.as_deref() }) else { return };
    let mut mixed = [0i32; FRAME_SAMPLES * 2];
    let mut active = 0u32;
    for i in 0..BLOCK_COUNT {
        let block = blocks + i * BLOCK_SIZE;
        if c.r16(block + pb::RUNNING) == 1 {
            active += 1;
            mix_voice(c, aram, block, &mut mixed);
        }
    }
    ACTIVE_VOICES.store(active, Ordering::Relaxed);
    MIXED_FRAMES.fetch_add(1, Ordering::Relaxed);

    let mut peak = 0u32;
    let mut w = RING_WRITE.load(Ordering::Relaxed);
    let r = RING_READ.load(Ordering::Acquire);
    for pair in mixed.chunks_exact(2) {
        if w.wrapping_sub(r) >= RING_CAP {
            break;
        }
        let l = pair[0].clamp(-32768, 32767);
        let rv = pair[1].clamp(-32768, 32767);
        peak = peak.max(l.unsigned_abs()).max(rv.unsigned_abs());
        unsafe {
            RING_BUF[w & RING_MASK] = [l as i16, rv as i16];
        }
        w = w.wrapping_add(1);
    }
    RING_WRITE.store(w, Ordering::Release);
    if peak > 0 {
        PEAK_LEVEL.fetch_max(peak, Ordering::Relaxed);
    }
}

/// Drains up to `max_frames` stereo `i16` frames into `dst`. Called from SDL2's audio callback.
pub unsafe fn drain(dst: *mut i16, max_frames: usize) -> usize {
    if dst.is_null() || max_frames == 0 {
        return 0;
    }
    let w = RING_WRITE.load(Ordering::Acquire);
    let mut r = RING_READ.load(Ordering::Relaxed);
    let avail = w.wrapping_sub(r).min(max_frames);
    for i in 0..avail {
        let s = RING_BUF[r & RING_MASK];
        *dst.add(i * 2) = s[0];
        *dst.add(i * 2 + 1) = s[1];
        r = r.wrapping_add(1);
    }
    RING_READ.store(r, Ordering::Release);
    DRAINED_FRAMES.fetch_add(avail as u32, Ordering::Relaxed);
    avail
}
