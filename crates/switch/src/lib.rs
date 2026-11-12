#![no_std]
#![allow(static_mut_refs)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::ffi::c_void;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use runtime::hle::Hle;
use runtime::Ctx;

mod arm64_native;
mod aspect;
mod audio;
mod card;
mod gl_renderer;
mod gx_gpu;
mod tev_gl;
mod texture;

extern "C" {
    fn malloc(size: usize) -> *mut u8;
    fn memalign(align: usize, size: usize) -> *mut u8;
    fn switch_file_exists(path: *const u8) -> i32;
    fn free(ptr: *mut u8);
    fn switch_log(msg: *const u8);
    fn switch_read_file(path: *const u8, offset: u32, len: u32, out: *mut u8) -> i32;
    fn switch_yield();
    fn switch_sleep_ms(ms: u32);
    fn switch_sleep_ns(ns: u64);
    fn switch_now_ns() -> u64;
    fn switch_exit_guest_thread() -> !;
    fn switch_exit_worker_thread() -> !;
    fn switch_sched_lock();
    fn switch_sched_unlock();
    fn switch_sched_wait();
    fn switch_sched_wake_all();
    fn switch_get_thread_index() -> u32;
    fn switch_set_thread_index(idx: u32);
    fn switch_get_guest_thread() -> u32;
    fn switch_set_guest_thread(gt: u32);
    fn switch_get_thread_prio() -> i32;
    fn switch_set_thread_prio(prio: i32);
    fn switch_create_worker_thread(
        entry: extern "C" fn(*mut c_void),
        arg: *mut c_void,
        ctx_to_free: *mut c_void,
        index: u32,
        stack_size: u32,
    ) -> i32;
    fn switch_start_gx_worker(entry: extern "C" fn(*mut c_void), arg: *mut c_void) -> i32;
    fn switch_start_prof_sampler(tick: extern "C" fn(), period_ns: u64) -> i32;
    fn switch_stop_gx_worker();
    fn switch_gx_lock();
    fn switch_gx_unlock();
    fn switch_gx_wait_work();
    fn switch_gx_wake_work();
    fn switch_gx_wait_done();
    fn switch_gx_wake_done();
    fn switch_gl_shutdown();
    fn switch_gl_get_perf_stats(
        out_ring_upload_ns: *mut u64,
        out_state_apply_ns: *mut u64,
        out_draw_calls_ns: *mut u64,
        out_present_blit_ns: *mut u64,
        out_swap_buffers_ns: *mut u64,
        out_draw_count: *mut u32,
        out_batch_count: *mut u32,
    );
}

pub(crate) fn log_c(msg: &str) {
    let mut buf = Vec::with_capacity(msg.len() + 1);
    buf.extend_from_slice(msg.as_bytes());
    buf.push(0);
    unsafe {
        switch_log(buf.as_ptr());
    }
}

static ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);

struct SwitchAlloc;

unsafe impl GlobalAlloc for SwitchAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        // newlib's malloc only guarantees 16-byte alignment.
        if layout.align() > 16 {
            memalign(layout.align(), layout.size())
        } else {
            malloc(layout.size())
        }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        ALLOC_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        free(ptr);
    }
}

#[global_allocator]
static ALLOC: SwitchAlloc = SwitchAlloc;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let msg = format!("[RUST PANIC] {:?}", info);
    log_c(&msg);
    loop {
        unsafe {
            switch_sleep_ms(100);
        }
    }
}

#[derive(Clone, Copy)]
struct SavedRegs {
    r: [u32; 32],
    f: [f64; 32],
    ps1: [f64; 32],
    cr: [u8; 8],
    lr: u32,
    ctr: u32,
    ca: u32,
    xer_rest: u32,
    fpscr: u32,
}

impl SavedRegs {
    #[inline(always)]
    fn save(c: &Ctx) -> Self {
        Self {
            r: c.r,
            f: c.f,
            ps1: c.ps1,
            cr: c.cr,
            lr: c.lr,
            ctr: c.ctr,
            ca: c.ca,
            xer_rest: c.xer_rest,
            fpscr: c.fpscr,
        }
    }

    #[inline(always)]
    fn restore(self, c: &mut Ctx) {
        c.r = self.r;
        c.f = self.f;
        c.ps1 = self.ps1;
        c.cr = self.cr;
        c.lr = self.lr;
        c.ctr = self.ctr;
        c.ca = self.ca;
        c.xer_rest = self.xer_rest;
        c.fpscr = self.fpscr;
    }
}

// Global Switch platform state
static IS_DOCKED: AtomicBool = AtomicBool::new(true);
static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Core 0 (GX worker) phase profiler: the worker records what it is doing, the Core 2 sampler
/// counts it. Phase 0 = idle (waiting for work); see `GX_PHASE_NAMES`.
/// Vertices sent down the CPU transform path, by reason: ortho, pos/nrm matrix index,
/// tex matrix index, tex coords 4-7, texgen from coords 4-7.
pub(crate) static CPUV_WHY: [AtomicU32; 5] = [const { AtomicU32::new(0) }; 5];
/// Clock for the per-command timing breakdowns in `[gx-core0]`. Thousands of calls per frame,
/// so it only reads the clock in profile builds; release builds report those parts as 0.
#[inline(always)]
pub(crate) fn prof_now() -> u64 {
    if runtime::PROFILE_FN {
        unsafe { switch_now_ns() }
    } else {
        0
    }
}

/// Core 1 time spent waiting for Core 0, by what asked for the wait (`[sync]` perf line).
static SYNC_NS: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];
static SYNC_N: [AtomicU32; 6] = [const { AtomicU32::new(0) }; 6];
const SYNC_NAMES: [&str; 6] = ["retrace", "draw_done", "read_sync", "fifo_ptrs", "break_pt", "settle"];
#[inline(always)]
fn timed_sync<R>(source: usize, c: &mut Ctx, f: impl FnOnce(&mut Ctx) -> R) -> R {
    let t0 = unsafe { switch_now_ns() };
    let r = f(c);
    SYNC_NS[source].fetch_add(unsafe { switch_now_ns() }.saturating_sub(t0), Ordering::Relaxed);
    SYNC_N[source].fetch_add(1, Ordering::Relaxed);
    r
}

/// Display-list cache counters per frame (`[dlstat]` perf line).
pub(crate) static DLSTAT: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
const DLSTAT_NAMES: [&str; 8] = ["resolve", "check_hit", "exact_hit", "builds", "built_verts", "owned", "hashed_kb", "list_kb"];
#[inline(always)]
pub(crate) fn dlstat(k: usize, n: u64) {
    DLSTAT[k].fetch_add(n, Ordering::Relaxed);
}

/// `sdmc:/strictsync.flag`: wait for Core 0 at every retrace, as the original port did.
pub(crate) static STRICT_SYNC: AtomicBool = AtomicBool::new(false);

pub(crate) static GX_PHASE: AtomicU32 = AtomicU32::new(0);
static GX_PHASE_COUNT: [AtomicU32; 16] = [const { AtomicU32::new(0) }; 16];
const GX_PHASE_NAMES: [&str; 16] = [
    "idle", "parse", "dl_call", "template", "dl_resolve", "vparams", "cpu_vertices", "imm_draw",
    "bp", "blk_hash", "copy_tex", "present_enc", "texture", "dl_build", "end_frame", "worker_other",
];

/// Sets the GX worker's current phase for the profiler and restores the previous one on drop.
pub(crate) struct GxPhase(u32);
impl GxPhase {
    #[inline(always)]
    pub(crate) fn enter(phase: u32) -> Option<GxPhase> {
        if runtime::PROFILE_FN {
            Some(GxPhase(GX_PHASE.swap(phase, Ordering::Relaxed)))
        } else {
            None
        }
    }
}
impl Drop for GxPhase {
    #[inline(always)]
    fn drop(&mut self) {
        GX_PHASE.store(self.0, Ordering::Relaxed);
    }
}

/// Function-level profiler samples (see `runtime::PROFILE_FN`): open-addressed (addr, count).
const PROF_SLOTS: usize = 8192;
static PROF_ADDR: [AtomicU32; PROF_SLOTS] = [const { AtomicU32::new(0) }; PROF_SLOTS];
static PROF_COUNT: [AtomicU32; PROF_SLOTS] = [const { AtomicU32::new(0) }; PROF_SLOTS];
static PROF_TOTAL: AtomicU32 = AtomicU32::new(0);

/// Runs on the sampler thread: charges one sample to the guest function executing right now.
extern "C" fn prof_tick() {
    GX_PHASE_COUNT[(GX_PHASE.load(Ordering::Relaxed) & 15) as usize].fetch_add(1, Ordering::Relaxed);
    let a = runtime::CUR_FN.load(Ordering::Relaxed);
    if a == 0 {
        return;
    }
    PROF_TOTAL.fetch_add(1, Ordering::Relaxed);
    let mut i = ((a >> 2).wrapping_mul(0x9E37_79B1) >> 19) as usize & (PROF_SLOTS - 1);
    for _ in 0..64 {
        let cur = PROF_ADDR[i].load(Ordering::Relaxed);
        if cur == a || (cur == 0 && PROF_ADDR[i].compare_exchange(0, a, Ordering::Relaxed, Ordering::Relaxed).is_ok()) {
            PROF_COUNT[i].fetch_add(1, Ordering::Relaxed);
            return;
        }
        if PROF_ADDR[i].load(Ordering::Relaxed) == a {
            PROF_COUNT[i].fetch_add(1, Ordering::Relaxed);
            return;
        }
        i = (i + 1) & (PROF_SLOTS - 1);
    }
}

/// Top functions by self time since the last call, as (addr, samples), plus the sample total.
fn drain_prof() -> (u32, Vec<(u32, u32)>) {
    let total = PROF_TOTAL.swap(0, Ordering::Relaxed);
    let mut v = Vec::new();
    for i in 0..PROF_SLOTS {
        let n = PROF_COUNT[i].swap(0, Ordering::Relaxed);
        if n != 0 {
            v.push((PROF_ADDR[i].load(Ordering::Relaxed), n));
        }
    }
    v.sort_unstable_by(|a, b| b.1.cmp(&a.1));
    (total, v)
}
static PAD_BUTTONS: AtomicU32 = AtomicU32::new(0);
static PAD_STICK_X: AtomicI32 = AtomicI32::new(0);
static PAD_STICK_Y: AtomicI32 = AtomicI32::new(0);
static PAD_SUBSTICK_X: AtomicI32 = AtomicI32::new(0);
static PAD_SUBSTICK_Y: AtomicI32 = AtomicI32::new(0);
static RETRACE_COUNT: AtomicU32 = AtomicU32::new(0);
pub static CURRENT_XFB: AtomicU32 = AtomicU32::new(0);
pub static FRAME_COUNT: AtomicU32 = AtomicU32::new(0);
pub static XFB_WIDTH: AtomicU32 = AtomicU32::new(640);
pub static XFB_HEIGHT: AtomicU32 = AtomicU32::new(480);
static PRE_RETRACE: AtomicU32 = AtomicU32::new(0);
static POST_RETRACE: AtomicU32 = AtomicU32::new(0);
static DRAW_DONE_CB: AtomicU32 = AtomicU32::new(0);
static DRAW_SYNC_CB: AtomicU32 = AtomicU32::new(0);
static BREAK_PT_CB: AtomicU32 = AtomicU32::new(0);
static GUEST_MEM_PTR: AtomicUsize = AtomicUsize::new(0);
static NEXT_RETRACE_TIME: AtomicU64 = AtomicU64::new(0);
static NEXT_AUDIO_TIME: AtomicU64 = AtomicU64::new(0);
static INTERRUPTS_ENABLED: AtomicBool = AtomicBool::new(true);
static IN_HANDLER: AtomicBool = AtomicBool::new(false);
static AUDIO_DMA_RUNNING: AtomicBool = AtomicBool::new(false);
static AI_DMA_CALLBACK: AtomicU32 = AtomicU32::new(0);
static AR_DMA_CALLBACK: AtomicU32 = AtomicU32::new(0);
static DSP_TASK: AtomicU32 = AtomicU32::new(0);
static TARGET_SCENE: AtomicU32 = AtomicU32::new(0);
static WARP_TARGET: AtomicU32 = AtomicU32::new(0);
static CURRENT_GAME_MODE: AtomicU32 = AtomicU32::new(0);

static HAS_EVENTS: AtomicBool = AtomicBool::new(false);
static NEXT_DUE_TIME: AtomicU64 = AtomicU64::new(0);
static BEST_READY_PRIO: AtomicI32 = AtomicI32::new(i32::MAX);
static ACTIVE_THREAD_IDX: AtomicUsize = AtomicUsize::new(0);
static ACTIVE_GUEST_THREAD: AtomicU32 = AtomicU32::new(0x817E_0000);
static ACTIVE_THREAD_PRIO: AtomicI32 = AtomicI32::new(16);

#[inline(always)]
fn update_next_due() {
    let r = NEXT_RETRACE_TIME.load(Ordering::Relaxed);
    let r = if r == 0 { u64::MAX } else { r };
    let a = if AUDIO_DMA_RUNNING.load(Ordering::Relaxed) && AI_DMA_CALLBACK.load(Ordering::Relaxed) != 0 {
        let t = NEXT_AUDIO_TIME.load(Ordering::Relaxed);
        if t == 0 { u64::MAX } else { t }
    } else {
        u64::MAX
    };
    let al = next_alarm_due();
    NEXT_DUE_TIME.store(r.min(a).min(al), Ordering::Relaxed);
}

// Low-overhead performance accumulators (nanoseconds)
pub static PERF_GX_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_GX_WAIT_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_AUDIO_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_SLEEP_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_PARSE_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_GL_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_PUMP_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_WAIT_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_LOOKAHEAD_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_CLIST_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_RESOLVE_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_FINISH_VERTS_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_SPANS_HASH_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_BUILD_LIST_NS: AtomicU64 = AtomicU64::new(0);

// Extreme profiler sub-timers (nanoseconds)
pub static PERF_PUMP_RETRACE_GX_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_PUMP_RETRACE_CB_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_PUMP_AUDIO_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_PUMP_ALARMS_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_PUMP_SCHED_NS: AtomicU64 = AtomicU64::new(0);

pub static PERF_PARSE_CP_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_PARSE_XF_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_PARSE_BP_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_PARSE_DIRECT_DRAW_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_COPY_TEXTURE_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_COPY_DISPLAY_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_HUD_ADJUST_NS: AtomicU64 = AtomicU64::new(0);
pub static PERF_ENCODE_PASS_NS: AtomicU64 = AtomicU64::new(0);
pub static STAT_COPY_TEXTURE_COUNT: AtomicU32 = AtomicU32::new(0);
pub static STAT_COPY_DISPLAY_COUNT: AtomicU32 = AtomicU32::new(0);

pub static STAT_CLIST_CALLS: AtomicU32 = AtomicU32::new(0);
pub static STAT_CLIST_HITS: AtomicU32 = AtomicU32::new(0);
pub static STAT_CLIST_MISSES: AtomicU32 = AtomicU32::new(0);
pub static STAT_AHEAD_HITS: AtomicU32 = AtomicU32::new(0);
pub static STAT_AHEAD_MISSES: AtomicU32 = AtomicU32::new(0);
pub static STAT_CACHED_DRAWS: AtomicU32 = AtomicU32::new(0);
pub static STAT_RAW_DRAWS: AtomicU32 = AtomicU32::new(0);
pub static STAT_XFORM_DRAWS: AtomicU32 = AtomicU32::new(0);
pub static STAT_CLIST_RAW_FAIL: AtomicU32 = AtomicU32::new(0);
pub static STAT_CLIST_HUD_FAIL: AtomicU32 = AtomicU32::new(0);
pub static STAT_CLIST_RES_FAIL: AtomicU32 = AtomicU32::new(0);
static PERF_LAST_NS: AtomicU64 = AtomicU64::new(0);
static PERF_LAST_FRAMES: AtomicU64 = AtomicU64::new(0);

const TICKS_PER_SECOND: u64 = 40_500_000;
const RETRACE_INTERVAL: u64 = 675_000; // 40.5 MHz / 60.000 Hz (16.6667 ms)
const AUDIO_INTERVAL: u64 = 202_500;   // 40.5 MHz / 200.000 Hz (5.0000 ms)

#[inline(always)]
fn check_exit(c: &mut Ctx) {
    if EXIT_REQUESTED.load(Ordering::Relaxed) {
        if unsafe { switch_get_thread_index() } == 0 {
            log_c("[switch] Exit requested: stopping Core 0 GX & Core 2 OpenGL and exiting guest thread.");
            unsafe {
                switch_exit_guest_thread();
            }
        } else {
            sched::exit_current(c);
        }
    }
}

static BOOT_INIT_DONE: AtomicBool = AtomicBool::new(false);

fn check_warp(c: &mut Ctx) {
    if !BOOT_INIT_DONE.load(Ordering::Relaxed) {
        return;
    }
    let target = WARP_TARGET.swap(0, Ordering::Relaxed);
    if target != 0 {
        TARGET_SCENE.store(target, Ordering::Relaxed);
        gl_renderer::CURRENT_SCENE_ID.store(target, Ordering::Relaxed);
        const PORTAL: u32 = 0x817E_0900;
        let asset = PORTAL + 0x40;
        c.slice_mut(PORTAL, 0x80).fill(0);
        c.w32(PORTAL + 0x10, asset);
        c.w32(asset + 0x14, target);
        let id = target.to_be_bytes();
        let name: String = id
            .iter()
            .map(|&b| (b as char).escape_default().to_string())
            .collect();
        log_c(&format!("[scene] Warping to {}", name));
        guest_call(c, "zSceneSwitch__FP8_zPortali", &[PORTAL, 0]);
    }
}

// ---- Cooperative Single-Baton Guest Thread Scheduler (ported from crates/tssm/src/sched.rs) ----
//
// Exactly ONE guest thread runs at a time, matching the GameCube's single PowerPC Gekko core.
// Non-running threads sleep on a Horizon OS CondVar (~0.00% CPU). Core 0 owns the GX Frontend
// (`Gx`), Core 2 owns the OpenGL 4.3 hardware renderer (`s_gl_thread`), and Core 1 runs the
// guest game engine loop and streams GX FIFO buffers to Core 0.
mod sched {
    use alloc::boxed::Box;
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::ffi::c_void;
    use core::sync::atomic::Ordering;
    use runtime::Ctx;

    use super::{
        absorb_gx, check_exit, idle, switch_create_worker_thread,
        switch_exit_guest_thread, switch_exit_worker_thread, switch_get_thread_index,
        switch_set_thread_index, switch_sched_lock, switch_sched_unlock, switch_sched_wait,
        switch_sched_wake_all, EXIT_REQUESTED, IN_HANDLER, INTERRUPTS_ENABLED,
    };

    /// Pseudo thread queue woken on every vertical retrace.
    pub const RETRACE_QUEUE: u32 = 1;
    const MAIN_PRIORITY: i32 = 16;

    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Ready,
        Waiting(u32),
        Exited,
    }

    struct Thread {
        guest: u32,
        state: State,
        suspend: i32,
        priority: i32,
    }

    struct Sched {
        threads: Vec<Thread>,
        current: usize,
    }

    static mut SCHED: Option<Sched> = None;

    #[inline(always)]
    pub fn me() -> usize {
        super::ACTIVE_THREAD_IDX.load(Ordering::Relaxed)
    }

    #[inline(always)]
    fn lock() {
        unsafe { switch_sched_lock() };
    }

    #[inline(always)]
    fn unlock() {
        unsafe { switch_sched_unlock() };
    }

    #[inline(always)]
    fn wait_cond() {
        unsafe { switch_sched_wait() };
    }

    #[inline(always)]
    pub fn wake_all() {
        unsafe { switch_sched_wake_all() };
    }

    #[allow(static_mut_refs)]
    unsafe fn state() -> &'static mut Sched {
        SCHED.get_or_insert_with(|| Sched {
            threads: Vec::with_capacity(8),
            current: 0,
        })
    }

    impl Sched {
        fn runnable(&self, i: usize) -> bool {
            self.threads[i].state == State::Ready && self.threads[i].suspend == 0
        }

        fn pick(&self, caller: usize, yielding: bool) -> Option<usize> {
            let n = self.threads.len();
            if n == 0 {
                return None;
            }
            if yielding {
                if let Some(next) = (1..n)
                    .map(|k| (caller + k) % n)
                    .find(|&i| self.runnable(i))
                {
                    return Some(next);
                }
            }
            let best = (0..n)
                .filter(|&i| self.runnable(i))
                .map(|i| self.threads[i].priority)
                .min()?;
            (0..n)
                .map(|k| (caller + yielding as usize + k) % n)
                .find(|&i| self.runnable(i) && self.threads[i].priority == best)
        }

        fn find(&self, guest: u32) -> Option<usize> {
            self.threads
                .iter()
                .position(|t| t.guest == guest && t.state != State::Exited)
        }
    }

    fn update_best_ready_prio(s: &Sched) {
        let cur = s.current;
        super::ACTIVE_THREAD_IDX.store(cur, Ordering::Relaxed);
        let (gt, prio) = s
            .threads
            .get(cur)
            .map_or((0x817E_0000, MAIN_PRIORITY), |t| (t.guest, t.priority));
        super::ACTIVE_GUEST_THREAD.store(gt, Ordering::Relaxed);
        super::ACTIVE_THREAD_PRIO.store(prio, Ordering::Relaxed);
        let best = (0..s.threads.len())
            .filter(|&i| i != cur && s.runnable(i))
            .map(|i| s.threads[i].priority)
            .min()
            .unwrap_or(i32::MAX);
        super::BEST_READY_PRIO.store(best, Ordering::Relaxed);
    }

    pub fn init(main_guest: u32) {
        lock();
        let s = unsafe { state() };
        if s.threads.is_empty() {
            s.threads.push(Thread {
                guest: main_guest,
                state: State::Ready,
                suspend: 0,
                priority: MAIN_PRIORITY,
            });
        } else {
            s.threads[0].guest = main_guest;
            s.threads[0].state = State::Ready;
            s.threads[0].suspend = 0;
            s.threads[0].priority = MAIN_PRIORITY;
        }
        s.current = 0;
        update_best_ready_prio(s);
        unlock();
    }

    pub fn describe() -> String {
        lock();
        let s = unsafe { state() };
        let mut out = format!("cur={}", s.current);
        for (i, t) in s.threads.iter().enumerate() {
            if t.state == State::Exited {
                continue;
            }
            let st = match t.state {
                State::Ready => "ready".to_string(),
                State::Waiting(q) => format!("wait({:#x})", q),
                State::Exited => "exited".to_string(),
            };
            out += &format!(" [t{}:{:#x} p={} sus={} {}]", i, t.guest, t.priority, t.suspend, st);
        }
        unlock();
        out
    }

    /// Waits with `sched` lock held until `s.current == caller`.
    fn wait_until_turn(caller: usize) {
        while unsafe { state().current } != caller && !EXIT_REQUESTED.load(Ordering::Relaxed) {
            wait_cond();
        }
    }

    pub fn reschedule(c: &mut Ctx, yielding: bool) {
        if IN_HANDLER.load(Ordering::Relaxed) {
            return;
        }
        let caller = me();
        absorb_gx(c);
        let saved_intr = INTERRUPTS_ENABLED.swap(true, Ordering::Relaxed);
        let mut yielding = yielding;

        loop {
            if EXIT_REQUESTED.load(Ordering::Relaxed) {
                break;
            }
            lock();
            let picked = unsafe { state().pick(caller, yielding) };
            match picked {
                Some(next) if next == caller => {
                    let s = unsafe { state() };
                    update_best_ready_prio(s);
                    unlock();
                    break;
                }
                Some(next) => {
                    unsafe {
                        state().current = next;
                        update_best_ready_prio(state());
                    }
                    wake_all();
                    wait_until_turn(caller);
                    update_best_ready_prio(unsafe { state() });
                    unlock();
                    yielding = false;
                }
                None => {
                    if caller != 0 {
                        // Hand idle duty to Core 1 (thread 0) so timers & VI retrace callbacks
                        // run on the main guest thread.
                        unsafe {
                            state().current = 0;
                            update_best_ready_prio(state());
                        }
                        wake_all();
                        wait_until_turn(caller);
                        update_best_ready_prio(unsafe { state() });
                        unlock();
                        yielding = false;
                    } else {
                        let s = unsafe { state() };
                        update_best_ready_prio(s);
                        unlock();
                        idle(c);
                    }
                }
            }
        }

        INTERRUPTS_ENABLED.store(saved_intr, Ordering::Relaxed);
        check_exit(c);
    }

    #[inline(always)]
    pub fn preempt(c: &mut Ctx) {
        if IN_HANDLER.load(Ordering::Relaxed) {
            return;
        }
        let my_prio = super::ACTIVE_THREAD_PRIO.load(Ordering::Relaxed);
        if super::BEST_READY_PRIO.load(Ordering::Relaxed) < my_prio {
            reschedule(c, false);
        }
    }

    #[inline(always)]
    pub fn current_guest() -> u32 {
        super::ACTIVE_GUEST_THREAD.load(Ordering::Relaxed)
    }

    pub fn priority(guest: u32) -> i32 {
        if guest == current_guest() {
            return super::ACTIVE_THREAD_PRIO.load(Ordering::Relaxed);
        }
        lock();
        let s = unsafe { state() };
        let p = s.find(guest).map_or(MAIN_PRIORITY, |i| s.threads[i].priority);
        unlock();
        p
    }

    pub fn sleep(c: &mut Ctx, queue: u32) {
        if IN_HANDLER.load(Ordering::Relaxed) {
            return;
        }
        let caller = me();
        lock();
        let s = unsafe { state() };
        if let Some(t) = s.threads.get_mut(caller) {
            t.state = State::Waiting(queue);
        }
        update_best_ready_prio(s);
        unlock();
        reschedule(c, false);
    }

    #[inline(always)]
    pub fn has_sleepers(queue: u32) -> bool {
        lock();
        let has = unsafe { state().threads.iter().any(|t| t.state == State::Waiting(queue)) };
        unlock();
        has
    }

    pub fn wake(queue: u32) {
        lock();
        let s = unsafe { state() };
        for t in s.threads.iter_mut().filter(|t| t.state == State::Waiting(queue)) {
            t.state = State::Ready;
        }
        update_best_ready_prio(s);
        unlock();
    }

    pub fn resume(guest: u32) -> i32 {
        lock();
        let s = unsafe { state() };
        let Some(i) = s.find(guest) else {
            unlock();
            return 0;
        };
        let old = s.threads[i].suspend;
        s.threads[i].suspend = (old - 1).max(0);
        update_best_ready_prio(s);
        unlock();
        old
    }

    pub fn suspend(c: &mut Ctx, guest: u32) -> i32 {
        let caller = me();
        let (old, is_me) = {
            lock();
            let s = unsafe { state() };
            let Some(i) = s.find(guest) else {
                unlock();
                return 0;
            };
            s.threads[i].suspend += 1;
            let res = (s.threads[i].suspend - 1, i == caller);
            update_best_ready_prio(s);
            unlock();
            res
        };
        if is_me {
            reschedule(c, false);
        }
        old
    }

    pub fn is_suspended(guest: u32) -> bool {
        lock();
        let s = unsafe { state() };
        let res = s.find(guest).is_some_and(|i| s.threads[i].suspend > 0);
        unlock();
        res
    }

    fn finish(c: &mut Ctx) {
        let caller = unsafe { switch_get_thread_index() as usize };
        let guest = {
            lock();
            let s = unsafe { state() };
            let mut g = c.guest_thread;
            if let Some(t) = s.threads.get_mut(caller) {
                t.state = State::Exited;
                g = t.guest;
            }
            unlock();
            g
        };
        if (0x8000_0000..0x8180_0000).contains(&guest) {
            c.w16(guest + 0x2C8, 8); // OS_THREAD_STATE_DEAD
        }
        wake(guest);
        absorb_gx(c);
        INTERRUPTS_ENABLED.store(true, Ordering::Relaxed);

        lock();
        let s = unsafe { state() };
        s.current = s.pick(caller, false).unwrap_or(0);
        update_best_ready_prio(s);
        wake_all();
        unlock();
    }

    pub fn exit_current(c: &mut Ctx) -> ! {
        if unsafe { switch_get_thread_index() } == 0 {
            unsafe { switch_exit_guest_thread() };
        }
        finish(c);
        unsafe { switch_exit_worker_thread() };
    }

    struct WorkerPayload {
        index: usize,
        func: u32,
        ctx_raw: *mut Ctx,
        guest: u32,
        priority: i32,
    }

    extern "C" fn worker_trampoline(arg: *mut c_void) {
        let payload = unsafe { Box::from_raw(arg as *mut WorkerPayload) };
        let WorkerPayload {
            index,
            func,
            ctx_raw,
            guest,
            priority,
        } = *payload;
        drop(payload);

        unsafe {
            switch_set_thread_index(index as u32);
            super::switch_set_guest_thread(guest);
            super::switch_set_thread_prio(priority);
        }

        lock();
        while unsafe { state().current } != index && !EXIT_REQUESTED.load(Ordering::Relaxed) {
            wait_cond();
        }
        update_best_ready_prio(unsafe { state() });
        unlock();

        let c = unsafe { &mut *ctx_raw };
        if !EXIT_REQUESTED.load(Ordering::Relaxed) {
            INTERRUPTS_ENABLED.store(true, Ordering::Relaxed);
            c.call(func);
        }
        finish(c);
    }

    pub fn create(c: &mut Ctx) {
        let (guest, func, param, stack_top, stack_size, raw_prio) = (
            c.r[3],
            c.r[4],
            c.r[5],
            c.r[6],
            c.r[7],
            c.r[8] as i32,
        );
        // Clamp ONLY the FMOD software mix threads (priority 8) to priority 18 (below Main Thread 16).
        // DVD thread (priority 0) and Async asset unpacker (priority 10) MUST retain high priority so level loading completes!
        let priority = if raw_prio == 8 { 18 } else { raw_prio };

        c.slice_mut(guest, 0x318).fill(0);
        c.w16(guest + 0x2C8, 4); // OS_THREAD_STATE_WAITING
        c.w32(guest + 0x2CC, 1); // initial suspend count = 1
        c.w32(guest + 0x2D0, priority as u32);

        let index = {
            lock();
            let s = unsafe { state() };
            if let Some(existing) = s.threads.iter().position(|t| t.state == State::Exited) {
                s.threads[existing] = Thread {
                    guest,
                    state: State::Ready,
                    suspend: 1,
                    priority,
                };
                update_best_ready_prio(s);
                unlock();
                existing
            } else {
                s.threads.push(Thread {
                    guest,
                    state: State::Ready,
                    suspend: 1,
                    priority,
                });
                let idx = s.threads.len() - 1;
                update_best_ready_prio(s);
                unlock();
                idx
            }
        };

        let mut child = c.fork();
        child.guest_thread = guest;
        child.r[1] = (stack_top - 16) & !7;
        child.w32(child.r[1], 0);
        child.r[2] = c.r[2];
        child.r[13] = c.r[13];
        child.r[3] = param;

        let ctx_raw = Box::into_raw(child);
        let payload = Box::new(WorkerPayload {
            index,
            func,
            ctx_raw,
            guest,
            priority,
        });
        let payload_raw = Box::into_raw(payload) as *mut c_void;

        let rc = unsafe {
            switch_create_worker_thread(
                worker_trampoline,
                payload_raw,
                ctx_raw as *mut c_void,
                index as u32,
                stack_size,
            )
        };
        if rc < 0 {
            super::log_c("[sched] Failed to spawn Horizon worker thread!");
            unsafe {
                drop(Box::from_raw(payload_raw as *mut WorkerPayload));
                drop(Box::from_raw(ctx_raw));
            }
            lock();
            unsafe {
                state().threads[index].state = State::Exited;
            }
            unlock();
            c.r[3] = 0;
            return;
        }
        c.r[3] = 1;
    }
}

// ---- 60 FPS driving-level zCar fixed-step & matrix interpolation (from crates/tssm/src/car.rs) ----

mod car {
    use core::sync::atomic::{AtomicBool, Ordering};
    use runtime::Ctx;

    const CAR_UPDATE: u32 = 0x800C_8724;
    const STEP: f32 = 1.0 / 60.0;
    const MOST_STEPS: u32 = 6;
    const MATRIX: u32 = 0x50;
    const PREVIOUS: u32 = 0x10;

    type Matrix = [u32; 16];

    struct State {
        now: Matrix,
        before: Matrix,
        shown: Matrix,
        left: f32,
        last_seen: u64,
    }

    static mut STATE: Option<State> = None;
    static INSIDE: AtomicBool = AtomicBool::new(false);

    fn read(c: &Ctx, at: u32) -> Matrix {
        core::array::from_fn(|i| c.r32(at + 4 * i as u32))
    }

    fn write(c: &mut Ctx, at: u32, m: &Matrix) {
        for (i, &w) in m.iter().enumerate() {
            c.w32(at + 4 * i as u32, w);
        }
    }

    pub fn car_update(c: &mut Ctx) -> bool {
        if INSIDE.load(Ordering::Relaxed) {
            return false;
        }
        let car = c.r[3];
        let args = (c.r[4], c.r[5], c.r[6], c.f[2], c.lr);
        let dt = c.f[1] as f32;
        let now_tb = c.timebase();
        let current = read(c, car + MATRIX);

        #[allow(static_mut_refs)]
        let s = unsafe {
            let fresh = match STATE.as_ref() {
                None => true,
                Some(s) => {
                    current != s.shown
                        || now_tb.saturating_sub(s.last_seen) > super::TICKS_PER_SECOND / 4
                }
            };
            if fresh {
                STATE = Some(State {
                    now: current,
                    before: current,
                    shown: current,
                    left: STEP,
                    last_seen: now_tb,
                });
            }
            STATE.as_mut().unwrap()
        };

        s.last_seen = now_tb;
        s.left = (s.left + dt).min(STEP * MOST_STEPS as f32);

        let shown_before = s.shown;
        write(c, car + MATRIX, &s.now);
        while s.left >= STEP {
            s.left -= STEP;
            s.before = s.now;
            write(c, car + PREVIOUS, &s.now);
            c.r[3] = car;
            (c.r[4], c.r[5], c.r[6], c.f[2]) = (args.0, args.1, args.2, args.3);
            c.f[1] = STEP as f64;
            INSIDE.store(true, Ordering::Relaxed);
            c.call(CAR_UPDATE);
            INSIDE.store(false, Ordering::Relaxed);
            s.now = read(c, car + MATRIX);
        }
        c.lr = args.4;

        s.shown = blend(&s.before, &s.now, s.left / STEP);
        write(c, car + PREVIOUS, &shown_before);
        write(c, car + MATRIX, &s.shown);
        true
    }

    fn blend(a: &Matrix, b: &Matrix, t: f32) -> Matrix {
        let f = |m: &Matrix, i: usize| f32::from_bits(m[i]);
        let row = |r: usize| -> [f32; 3] {
            core::array::from_fn(|k| f(a, r * 4 + k) + (f(b, r * 4 + k) - f(a, r * 4 + k)) * t)
        };
        let dot = |x: [f32; 3], y: [f32; 3]| x[0] * y[0] + x[1] * y[1] + x[2] * y[2];
        let sub =
            |x: [f32; 3], y: [f32; 3], s: f32| [x[0] - y[0] * s, x[1] - y[1] * s, x[2] - y[2] * s];
        let scale = |x: [f32; 3], s: f32| [x[0] * s, x[1] * s, x[2] * s];
        let length = |x: [f32; 3]| libm::sqrtf(dot(x, x));
        let target = |r: usize| length(core::array::from_fn(|k| f(b, r * 4 + k)));

        let x = row(0);
        let x = scale(x, 1.0 / length(x).max(1e-6));
        let y = row(1);
        let y = sub(y, x, dot(y, x));
        let y = scale(y, 1.0 / length(y).max(1e-6));
        let z = row(2);
        let z = sub(sub(z, x, dot(z, x)), y, dot(z, y));
        let z = scale(z, 1.0 / length(z).max(1e-6));
        let rows = [
            scale(x, target(0)),
            scale(y, target(1)),
            scale(z, target(2)),
            row(3),
        ];

        let mut out = *b;
        for (r, v) in rows.iter().enumerate() {
            for k in 0..3 {
                out[r * 4 + k] = v[k].to_bits();
            }
        }
        out
    }
}

#[derive(Clone, Copy)]
struct Alarm {
    ptr: u32,
    due: u64,
    period: u64,
    handler: u32,
}

static mut ALARMS: Vec<Alarm> = Vec::new();

// Embedded core GameCube assets
static MAIN_DOL: &[u8] = include_bytes!("../../../disc/sys/main.dol");
static BOOT_BIN: &[u8] = include_bytes!("../../../disc/sys/boot.bin");
static FST_BIN: &[u8] = include_bytes!("../../../disc/sys/fst.bin");

#[derive(Clone)]
struct DvdEntry {
    path: String,
    offset: u32,
    size: u32,
    is_dir: bool,
}

static mut DVD_ENTRIES: Option<Vec<DvdEntry>> = None;

fn init_dvd_entries() {
    let fst = FST_BIN;
    if fst.len() < 12 {
        return;
    }
    let be = |o: usize| u32::from_be_bytes(fst[o..o + 4].try_into().unwrap());
    let count = be(8) as usize;
    let names = count * 12;

    let mut entries = Vec::with_capacity(count);
    entries.push(DvdEntry {
        path: String::new(),
        offset: 0,
        size: 0,
        is_dir: true,
    });

    let mut dirs: Vec<(usize, String)> = vec![(count, String::new())];
    for i in 1..count {
        while !dirs.is_empty() && i >= dirs.last().unwrap().0 {
            dirs.pop();
        }
        let o = names + (be(i * 12) & 0xFF_FFFF) as usize;
        let mut len = 0;
        while o + len < fst.len() && fst[o + len] != 0 {
            len += 1;
        }
        let name: String = fst[o..o + len].iter().map(|&b| b as char).collect();
        let prefix = dirs.last().map(|d| d.1.as_str()).unwrap_or("");
        let path = format!("{}{}", prefix, name);
        let is_dir = fst[i * 12] == 1;

        if is_dir {
            dirs.push((be(i * 12 + 8) as usize, format!("{}/", path)));
        }

        entries.push(DvdEntry {
            path,
            offset: be(i * 12 + 4),
            size: be(i * 12 + 8),
            is_dir,
        });
    }

    unsafe {
        DVD_ENTRIES = Some(entries);
    }
}

fn dvd_lookup(path: &str) -> Option<usize> {
    let clean = path
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_matches('/')
        .to_ascii_lowercase();
    #[allow(static_mut_refs)]
    unsafe {
        DVD_ENTRIES
            .as_ref()?
            .iter()
            .position(|e| e.path.to_ascii_lowercase() == clean)
    }
}

fn dvd_find_by_offset(start: u32) -> Option<usize> {
    #[allow(static_mut_refs)]
    unsafe {
        DVD_ENTRIES
            .as_ref()?
            .iter()
            .position(|e| !e.is_dir && e.offset == start)
    }
}

fn boot_override(ini: &mut [u8]) {
    let (from, to) = (b"ShowMenuOnBoot = 1", b"ShowMenuOnBoot = 0");
    if let Some(at) = ini.windows(from.len()).position(|w| w == from) {
        ini[at..at + to.len()].copy_from_slice(to);
        log_c("[dvd] Patched SB04.ini: ShowMenuOnBoot = 0 (Direct Boot Enabled)");
    }
}

fn autosave_notice(piece: &mut [u8]) {
    const REST: &str = "{n}{n}Would you like to create a game file to save your progress?";
    const OLD: &str = "The game uses an autosave feature. While playing, Do not remove the Memory Card or power off the \
        Nintendo GameCube.";
    const NEW: &str =
        "The game uses an autosave feature. While playing, do not power off your device or close the game.";
    let old = format!("{OLD}{REST}").into_bytes();
    let new = format!("{NEW}{REST}").into_bytes();
    if let Some(pos) = piece.windows(old.len()).position(|w| w == old.as_slice()) {
        let mut patch = new;
        patch.resize(old.len(), 0);
        piece[pos..pos + patch.len()].copy_from_slice(&patch);
    }
}

pub(crate) enum Event {
    DvdDone { block: u32, callback: u32, result: i32 },
    GxToken(u16),
    GxBreak,
    GxFinish,
    Call(u32, u32),
    CardDone { chan: u32, callback: u32, result: i32 },
}

static mut EVENT_QUEUE: Option<VecDeque<Event>> = None;
static EVENTS_LOCK: AtomicBool = AtomicBool::new(false);
static ALARMS_LOCK: AtomicBool = AtomicBool::new(false);

#[inline(always)]
fn lock_events() {
    while EVENTS_LOCK
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
}

#[inline(always)]
fn unlock_events() {
    EVENTS_LOCK.store(false, Ordering::Release);
}

pub(crate) fn push_event(ev: Event) {
    lock_events();
    #[allow(static_mut_refs)]
    unsafe {
        EVENT_QUEUE
            .get_or_insert_with(|| VecDeque::with_capacity(64))
            .push_back(ev);
    }
    HAS_EVENTS.store(true, Ordering::Release);
    unlock_events();
}

#[inline(always)]
fn pop_event() -> Option<Event> {
    if !HAS_EVENTS.load(Ordering::Relaxed) {
        return None;
    }
    lock_events();
    #[allow(static_mut_refs)]
    let (ev, empty) = unsafe {
        if let Some(q) = EVENT_QUEUE.as_mut() {
            let ev = q.pop_front();
            (ev, q.is_empty())
        } else {
            (None, true)
        }
    };
    if empty {
        HAS_EVENTS.store(false, Ordering::Release);
    }
    unlock_events();
    ev
}

#[inline(always)]
fn has_pending_events() -> bool {
    HAS_EVENTS.load(Ordering::Relaxed)
}

#[inline(always)]
fn lock_alarms() {
    while ALARMS_LOCK
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
}

#[inline(always)]
fn unlock_alarms() {
    ALARMS_LOCK.store(false, Ordering::Release);
}

#[inline(always)]
fn next_alarm_due() -> u64 {
    lock_alarms();
    #[allow(static_mut_refs)]
    let due = unsafe { ALARMS.iter().map(|a| a.due).min().unwrap_or(u64::MAX) };
    unlock_alarms();
    due
}

mod gx_worker {
    use alloc::collections::VecDeque;
    use alloc::vec::Vec;
    use core::ffi::c_void;
    use core::sync::atomic::Ordering;
    use runtime::Ctx;

    use crate::gl_renderer::Renderer;
    use crate::gx_gpu::{FrameStats, Gx, Raised};
    use crate::{
        push_event, switch_gx_lock, switch_gx_unlock, switch_gx_wait_done, switch_gx_wait_work,
        switch_gx_wake_done, switch_gx_wake_work, switch_now_ns, switch_start_gx_worker, Event,
        EXIT_REQUESTED, FRAME_COUNT, GUEST_MEM_PTR, PERF_GX_NS, PERF_GX_WAIT_NS,
    };

    #[derive(Clone, Copy)]
    pub enum CtrlCmd {
        ResetCache,
        EnableBreakPt(u32),
        /// Breakpoint at a FIFO stream position (bytes since boot), resolved on Core 1.
        EnableBreakPtAt(u64),
        DisableBreakPt,
        QueryFifoPtrs,
    }

    struct Shared {
        initialized: bool,
        staging: Vec<u8>,
        hud_marks: VecDeque<(u64, u32)>,
        cmds: VecDeque<CtrlCmd>,
        written_bytes: u64,
        written_seq: u64,
        completed_ram_seq: u64,
        completed_full_seq: u64,
        /// Core 0's FIFO state (Gx::describe), for the hang report.
        gx_state: alloc::string::String,
        token: u16,
        frames: u64,
        last_stats: FrameStats,
        cache_used: u64,
        made: (u32, u32, u32, u64),
        fifo_ptrs: (u32, u32),
        hud_widgets: u32,
        hud_marked: u32,
    }

    static mut SHARED: Option<Shared> = None;

    #[inline(always)]
    #[allow(static_mut_refs)]
    unsafe fn shared() -> &'static mut Shared {
        SHARED.as_mut().expect("gx_worker::SHARED initialized")
    }

    extern "C" fn worker_entry(_arg: *mut c_void) {
        let mut gx = Gx::new();
        gx.renderer = Some(Renderer::new());

        unsafe {
            switch_gx_lock();
            shared().initialized = true;
            switch_gx_wake_done();
            switch_gx_unlock();
        }

        let mut work_buf: Vec<u8> = Vec::with_capacity(256 * 1024);
        let mut work_hud: VecDeque<(u64, u32)> = VecDeque::with_capacity(64);
        let mut work_cmds: VecDeque<CtrlCmd> = VecDeque::with_capacity(16);

        loop {
            unsafe {
                switch_gx_lock();
            }
            loop {
                if EXIT_REQUESTED.load(Ordering::Relaxed) {
                    unsafe {
                        switch_gx_unlock();
                    }
                    return;
                }
                let s = unsafe { shared() };
                if !s.staging.is_empty()
                    || !s.cmds.is_empty()
                    || !s.hud_marks.is_empty()
                    || gx.has_ready_presents()
                {
                    break;
                }
                unsafe {
                    switch_gx_wait_work();
                }
            }

            let s = unsafe { shared() };
            core::mem::swap(&mut s.staging, &mut work_buf);
            while let Some(hm) = s.hud_marks.pop_front() {
                work_hud.push_back(hm);
            }
            while let Some(cmd) = s.cmds.pop_front() {
                work_cmds.push_back(cmd);
            }
            let seq_at_pickup = s.written_seq;
            unsafe {
                switch_gx_unlock();
            }

            let guest_mem = GUEST_MEM_PTR.load(Ordering::Relaxed) as *const u8;
            let t0 = unsafe { switch_now_ns() };
            let _busy = crate::GxPhase::enter(15);

            while let Some(hm) = work_hud.pop_front() {
                gx.hud_marks.push_back(hm);
            }

            let mut need_reset_after = false;
            if !work_cmds.is_empty() && !work_buf.is_empty() {
                gx.pending.extend_from_slice(&work_buf);
                work_buf.clear();
            }
            while let Some(cmd) = work_cmds.pop_front() {
                match cmd {
                    CtrlCmd::ResetCache => need_reset_after = true,
                    CtrlCmd::EnableBreakPt(addr) => gx.set_breakpoint(guest_mem, addr),
                    CtrlCmd::EnableBreakPtAt(limit) => gx.set_breakpoint_at(limit),
                    CtrlCmd::DisableBreakPt => gx.clear_breakpoint(),
                    CtrlCmd::QueryFifoPtrs => {
                        let ptrs = gx.fifo_ptrs(guest_mem);
                        unsafe {
                            switch_gx_lock();
                            shared().fifo_ptrs = ptrs;
                            switch_gx_unlock();
                        }
                    }
                }
            }

            if !work_buf.is_empty() || gx.has_pending_local() {
                gx.process_chunk(guest_mem, &work_buf);
                work_buf.clear();
            }

            if need_reset_after {
                gx.reset_cache();
            }

            gx.display_copies.clear();
            gx.copied_to_display = false;
            FRAME_COUNT.store(gx.frames as u32, Ordering::Relaxed);

            for raised in gx.raised.drain(..) {
                push_event(match raised {
                    Raised::Token(token) => Event::GxToken(token),
                    Raised::Break => Event::GxBreak,
                    Raised::Finish => Event::GxFinish,
                });
            }

            let frames = gx.frames;
            let last_stats = gx.last_stats;
            let cache_used = gx.cache_room_used();
            let token = gx.token;
            let gx_state = gx.describe();

            // Wake Core 1 immediately once all guest RAM reads and raised tokens for seq_at_pickup complete!
            unsafe {
                switch_gx_lock();
                let s = shared();
                s.frames = frames;
                s.last_stats = last_stats;
                s.cache_used = cache_used;
                s.token = token;
                s.gx_state.clear();
                s.gx_state.push_str(&gx_state);
                if s.completed_ram_seq < seq_at_pickup {
                    s.completed_ram_seq = seq_at_pickup;
                }
                if !gx.has_ready_presents() && s.completed_full_seq < seq_at_pickup {
                    s.completed_full_seq = seq_at_pickup;
                }
                switch_gx_wake_done();
                switch_gx_unlock();
            }

            // Finish pure-host HUD widescreen adjustment and pass encoding to Core 2 GL backend
            // concurrently while Core 1 simulates the next game frame!
            if gx.has_ready_presents() {
                gx.flush_ready_presents();
            }

            let made_delta = gx
                .renderer
                .as_mut()
                .map_or((0, 0, 0, 0), |r| core::mem::take(&mut r.made));
            let cache_used_after = gx.cache_room_used();
            let dt = unsafe { switch_now_ns().saturating_sub(t0) };
            PERF_GX_NS.fetch_add(dt, Ordering::Relaxed);

            unsafe {
                switch_gx_lock();
                let s = shared();
                s.made.0 += made_delta.0;
                s.made.1 += made_delta.1;
                s.made.2 += made_delta.2;
                s.made.3 += made_delta.3;
                s.cache_used = cache_used_after;
                if s.completed_full_seq < seq_at_pickup {
                    s.completed_full_seq = seq_at_pickup;
                }
                switch_gx_wake_done();
                switch_gx_unlock();
            }
        }
    }

    pub fn init() {
        unsafe {
            SHARED = Some(Shared {
                initialized: false,
                staging: Vec::with_capacity(256 * 1024),
                hud_marks: VecDeque::with_capacity(64),
                cmds: VecDeque::with_capacity(16),
                written_bytes: 0,
                written_seq: 0,
                completed_ram_seq: 0,
                completed_full_seq: 0,
                token: 0,
                frames: 0,
                last_stats: FrameStats::default(),
                cache_used: 0,
                made: (0, 0, 0, 0),
                fifo_ptrs: (0x8000_0000, 0x8000_0000),
                hud_widgets: 0,
                hud_marked: 0,
                gx_state: alloc::string::String::new(),
            });
            switch_start_gx_worker(worker_entry, core::ptr::null_mut());
            switch_gx_lock();
            while !shared().initialized && !EXIT_REQUESTED.load(Ordering::Relaxed) {
                switch_gx_wait_done();
            }
            switch_gx_unlock();
        }
    }

    #[inline(always)]
    pub fn push_fifo(c: &mut Ctx, min_bytes: usize) {
        if c.fifo_len == 0 || c.fifo_len < min_bytes {
            return;
        }
        let slice = unsafe { core::slice::from_raw_parts(c.mem.add(0x0200_0000), c.fifo_len) };
        unsafe {
            switch_gx_lock();
            let s = shared();
            s.staging.extend_from_slice(slice);
            s.written_bytes += slice.len() as u64;
            s.written_seq += 1;
            switch_gx_wake_work();
            switch_gx_unlock();
        }
        c.fifo_len = 0;
    }

    /// Waits for Core 0 GX worker to finish all guest RAM reads & raise GX sync tokens.
    /// Does NOT stall Core 1 on Core 0's host-only HUD/present pass encoding or Core 2's GL draws.
    pub fn sync_ram(c: &mut Ctx) {
        let t0 = unsafe { switch_now_ns() };
        unsafe {
            switch_gx_lock();
            let s = shared();
            if c.fifo_len != 0 {
                let slice = core::slice::from_raw_parts(c.mem.add(0x0200_0000), c.fifo_len);
                s.staging.extend_from_slice(slice);
                s.written_bytes += slice.len() as u64;
                s.written_seq += 1;
                c.fifo_len = 0;
                switch_gx_wake_work();
            }
            let target = s.written_seq;
            while s.completed_ram_seq < target && !EXIT_REQUESTED.load(Ordering::Relaxed) {
                switch_gx_wait_done();
            }
            switch_gx_unlock();
        }
        let dt = unsafe { switch_now_ns().saturating_sub(t0) };
        PERF_GX_WAIT_NS.fetch_add(dt, Ordering::Relaxed);
    }

    /// Strict full sync (used for GXDrawDone, GXReadDrawSync, breakpoints, scene init reset).
    pub fn sync_full(c: &mut Ctx, cmd: Option<CtrlCmd>) {
        let t0 = unsafe { switch_now_ns() };
        unsafe {
            switch_gx_lock();
            let s = shared();
            if c.fifo_len != 0 {
                let slice = core::slice::from_raw_parts(c.mem.add(0x0200_0000), c.fifo_len);
                s.staging.extend_from_slice(slice);
                s.written_bytes += slice.len() as u64;
                s.written_seq += 1;
                c.fifo_len = 0;
            }
            if let Some(cmd) = cmd {
                s.cmds.push_back(cmd);
                s.written_seq += 1;
            }
            let target = s.written_seq;
            switch_gx_wake_work();
            while s.completed_full_seq < target && !EXIT_REQUESTED.load(Ordering::Relaxed) {
                switch_gx_wait_done();
            }
            switch_gx_unlock();
        }
        let dt = unsafe { switch_now_ns().saturating_sub(t0) };
        PERF_GX_WAIT_NS.fetch_add(dt, Ordering::Relaxed);
    }

    pub fn mark_hud(c: &Ctx, widget: bool) {
        unsafe {
            switch_gx_lock();
            let s = shared();
            if !widget && s.hud_marked == 0 {
                switch_gx_unlock();
                return;
            }
            if widget {
                s.hud_widgets = s.hud_widgets % 0xFFFF + 1;
            }
            s.hud_marked = if widget { s.hud_widgets } else { 0 };
            let at = s.written_bytes + c.fifo_len as u64;
            s.hud_marks.push_back((at, s.hud_marked));
            switch_gx_unlock();
        }
    }

    pub fn read_token() -> u16 {
        unsafe {
            switch_gx_lock();
            let tok = shared().token;
            switch_gx_unlock();
            tok
        }
    }

    /// Lazy end-of-frame sync (opt-in: `sdmc:/lazysync.flag`): GXGetFifoPtrs and the
    /// frame breakpoints no longer wait for Core 0; instead Core 1 waits right before it
    /// first rewrites dynamic vertex data in the next frame (CPU skinning), so Core 0 finishes
    /// the previous frame while Core 1 runs the next frame's game logic. Without the wait before
    /// skinning, characters' vertex buffers were rewritten while Core 0 still read them (huge
    /// stray polygons on some frames). BB02 Ryujinx: per-frame wait 8-14 ms -> ~4-5 ms.
    pub static LAZY_SYNC: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    /// A frame ended without Core 1 waiting for Core 0 (see LAZY_SYNC).
    pub static FRAME_DIRTY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

    fn fifo_ring(c: &Ctx) -> (u32, u64) {
        // Same mapping as Gx::fifo_range on Core 0.
        let (base, top) = (c.r32(0xCC00_300C) & 0x03FF_FFE0, c.r32(0xCC00_3010) & 0x03FF_FFE0);
        if top > base {
            (base | 0x8000_0000, (top - base + 4) as u64)
        } else {
            (0x8000_0000, 0x4_0000)
        }
    }

    fn written_now(c: &Ctx) -> u64 {
        unsafe {
            switch_gx_lock();
            let written = shared().written_bytes + c.fifo_len as u64;
            switch_gx_unlock();
            written
        }
    }

    /// Queues a control command for Core 0 behind everything written so far, without waiting.
    pub fn post_cmd(c: &mut Ctx, cmd: CtrlCmd) {
        unsafe {
            switch_gx_lock();
            let s = shared();
            if c.fifo_len != 0 {
                let slice = core::slice::from_raw_parts(c.mem.add(0x0200_0000), c.fifo_len);
                s.staging.extend_from_slice(slice);
                s.written_bytes += slice.len() as u64;
                c.fifo_len = 0;
            }
            s.cmds.push_back(cmd);
            s.written_seq += 1;
            switch_gx_wake_work();
            switch_gx_unlock();
        }
    }

    /// GXEnableBreakPt. In lazy mode the address is resolved to a stream position here,
    /// against Core 1's exact byte count: Core 0 may read the command after Core 1 has written
    /// more than a whole FIFO ring further, and resolving it there lands a lap too far.
    pub fn enable_breakpoint(c: &mut Ctx, addr: u32) {
        if !LAZY_SYNC.load(Ordering::Relaxed) {
            sync_full(c, Some(CtrlCmd::EnableBreakPt(addr)));
            return;
        }
        let written = written_now(c);
        let (base, size) = fifo_ring(c);
        let behind = (written + size - (addr.wrapping_sub(base) as u64 % size)) % size;
        post_cmd(c, CtrlCmd::EnableBreakPtAt(written - behind.min(written)));
    }

    pub fn disable_breakpoint(c: &mut Ctx) {
        if LAZY_SYNC.load(Ordering::Relaxed) {
            post_cmd(c, CtrlCmd::DisableBreakPt);
        } else {
            sync_full(c, Some(CtrlCmd::DisableBreakPt));
        }
    }

    /// Called before Core 1 rewrites dynamic vertex data: settles a lazily ended frame.
    #[inline(always)]
    pub fn settle_frame(c: &mut Ctx) {
        if FRAME_DIRTY.load(Ordering::Relaxed) {
            FRAME_DIRTY.store(false, Ordering::Relaxed);
            super::timed_sync(5, c, sync_ram);
        }
    }

    pub fn read_fifo_ptrs(c: &mut Ctx) -> (u32, u32) {
        if LAZY_SYNC.load(Ordering::Relaxed) {
            // The only caller (_rwDlRasterShowRaster) uses just the write pointer.
            let written = written_now(c);
            let (base, size) = fifo_ring(c);
            let at = base + (written % size) as u32;
            FRAME_DIRTY.store(true, Ordering::Relaxed);
            return (at, at);
        }
        // Full sync on purpose: _rwDlRasterShowRaster calls this once per frame, at the end of
        // the frame, and it is what keeps Core 1 from starting to rewrite the next frame's
        // dynamic vertex buffers while Core 0 still reads this frame's. Answering from Core 1's
        // own byte count without waiting (tried 2026-11-12) gained ~10% FPS but drew huge
        // stray polygons on some frames.
        sync_full(c, Some(CtrlCmd::QueryFifoPtrs));
        unsafe {
            switch_gx_lock();
            let ptrs = shared().fifo_ptrs;
            switch_gx_unlock();
            ptrs
        }
    }

    /// (written_seq, completed_ram_seq, completed_full_seq, Core 0 state, queued commands,
    /// staged bytes). Caller holds the GX lock.
    pub unsafe fn shared_for_report() -> (u64, u64, u64, &'static alloc::string::String, usize, usize) {
        let s = shared();
        (s.written_seq, s.completed_ram_seq, s.completed_full_seq, &s.gx_state, s.cmds.len(), s.staging.len())
    }

    pub fn drain_perf_snapshot() -> (u64, FrameStats, u64, (u32, u32, u32, u64)) {
        unsafe {
            switch_gx_lock();
            let s = shared();
            let made = core::mem::take(&mut s.made);
            let res = (s.frames, s.last_stats, s.cache_used, made);
            switch_gx_unlock();
            res
        }
    }
}

#[inline(always)]
fn absorb_gx(c: &mut Ctx) {
    gx_worker::push_fifo(c, 1);
}

fn gx_process(c: &mut Ctx) {
    gx_worker::sync_ram(c);
}

#[inline(always)]
fn advance_due(due: u64, period: u64, now: u64) -> u64 {
    if period == 0 {
        return now;
    }
    if now >= due {
        due + period * ((now - due) / period + 1)
    } else {
        due + period
    }
}

#[inline(always)]
fn uncap_frame_rate(c: &mut Ctx) {
    // Keep RenderWare's _RwDlRetraceMinCount at 1 so driving & heavy scenes never lock to 30 FPS.
    const RETRACE_MIN_COUNT: u32 = 0x804B_0889;
    if c.r8(RETRACE_MIN_COUNT) > 1 {
        c.w8(RETRACE_MIN_COUNT, 1);
    }
}

fn log_perf_summary(c: &Ctx, retrace: u32) {
    let now_ns = unsafe { switch_now_ns() };
    let prev_ns = PERF_LAST_NS.swap(now_ns, Ordering::Relaxed);
    let (frames, st, cached_v, _made) = gx_worker::drain_perf_snapshot();
    let (draws, verts, dls, bps, cps, xfs) = (
        st.draws,
        st.vertices,
        st.display_lists,
        st.bp_loads,
        st.cp_loads,
        st.xf_loads,
    );
    let prev_frames = PERF_LAST_FRAMES.swap(frames, Ordering::Relaxed);
    {
        // Hang report: no frame for ~5 s of retraces -> write what every core is waiting on
        // to the log (sdmc:/tssm.log), once per stall.
        static STALLED: AtomicU32 = AtomicU32::new(0);
        if frames == prev_frames && frames > 0 {
            if STALLED.fetch_add(1, Ordering::Relaxed) + 1 == 15 {
                let (written, ram, full, gx_state, cmds, staged) = unsafe {
                    switch_gx_lock();
                    let s = gx_worker::shared_for_report();
                    let r = (s.0, s.1, s.2, s.3.clone(), s.4, s.5);
                    switch_gx_unlock();
                    r
                };
                log_c(&format!(
                    "[hang] frames={} retrace={} gx_phase={} written_seq={} ram_done={} full_done={} cmds={} staged={} core0: {} | lazy={} lr={:#010x} ({}) | {}",
                    frames, retrace, GX_PHASE.load(Ordering::Relaxed), written, ram, full, cmds, staged,
                    gx_state, gx_worker::LAZY_SYNC.load(Ordering::Relaxed), c.lr, c.func_name(c.lr), sched::describe()
                ));
            }
        } else {
            STALLED.store(0, Ordering::Relaxed);
        }
    }
    let gx_ns = PERF_GX_NS.swap(0, Ordering::Relaxed);
    let gx_wait_ns = PERF_GX_WAIT_NS.swap(0, Ordering::Relaxed);
    let aud_ns = PERF_AUDIO_NS.swap(0, Ordering::Relaxed);
    let slp_ns = PERF_SLEEP_NS.swap(0, Ordering::Relaxed);
    let parse_ns = PERF_PARSE_NS.swap(0, Ordering::Relaxed);
    let gl_ns = PERF_GL_NS.swap(0, Ordering::Relaxed);
    let pump_ns = PERF_PUMP_NS.swap(0, Ordering::Relaxed);
    let wait_ns = PERF_WAIT_NS.swap(0, Ordering::Relaxed);

    let lookahead_ns = PERF_LOOKAHEAD_NS.swap(0, Ordering::Relaxed);
    let clist_ns = PERF_CLIST_NS.swap(0, Ordering::Relaxed);
    let resolve_ns = PERF_RESOLVE_NS.swap(0, Ordering::Relaxed);
    let finish_v_ns = PERF_FINISH_VERTS_NS.swap(0, Ordering::Relaxed);
    let spans_h_ns = PERF_SPANS_HASH_NS.swap(0, Ordering::Relaxed);
    let build_l_ns = PERF_BUILD_LIST_NS.swap(0, Ordering::Relaxed);

    let p_retrace_gx_ns = PERF_PUMP_RETRACE_GX_NS.swap(0, Ordering::Relaxed);
    let p_retrace_cb_ns = PERF_PUMP_RETRACE_CB_NS.swap(0, Ordering::Relaxed);
    let p_audio_ns = PERF_PUMP_AUDIO_NS.swap(0, Ordering::Relaxed);
    let p_alarms_ns = PERF_PUMP_ALARMS_NS.swap(0, Ordering::Relaxed);
    let p_sched_ns = PERF_PUMP_SCHED_NS.swap(0, Ordering::Relaxed);

    let parse_cp_ns = PERF_PARSE_CP_NS.swap(0, Ordering::Relaxed);
    let parse_xf_ns = PERF_PARSE_XF_NS.swap(0, Ordering::Relaxed);
    let parse_bp_ns = PERF_PARSE_BP_NS.swap(0, Ordering::Relaxed);
    let parse_draw_ns = PERF_PARSE_DIRECT_DRAW_NS.swap(0, Ordering::Relaxed);
    let copy_tex_ns = PERF_COPY_TEXTURE_NS.swap(0, Ordering::Relaxed);
    let copy_disp_ns = PERF_COPY_DISPLAY_NS.swap(0, Ordering::Relaxed);
    let hud_adj_ns = PERF_HUD_ADJUST_NS.swap(0, Ordering::Relaxed);
    let encode_pass_ns = PERF_ENCODE_PASS_NS.swap(0, Ordering::Relaxed);
    let copy_tex_cnt = STAT_COPY_TEXTURE_COUNT.swap(0, Ordering::Relaxed);
    let copy_disp_cnt = STAT_COPY_DISPLAY_COUNT.swap(0, Ordering::Relaxed);

    let _clist_calls = STAT_CLIST_CALLS.swap(0, Ordering::Relaxed);
    let _clist_hits = STAT_CLIST_HITS.swap(0, Ordering::Relaxed);
    let _clist_misses = STAT_CLIST_MISSES.swap(0, Ordering::Relaxed);
    let _ahead_hits = STAT_AHEAD_HITS.swap(0, Ordering::Relaxed);
    let _ahead_misses = STAT_AHEAD_MISSES.swap(0, Ordering::Relaxed);
    let cached_draws = STAT_CACHED_DRAWS.swap(0, Ordering::Relaxed);
    let raw_draws = STAT_RAW_DRAWS.swap(0, Ordering::Relaxed);
    let xform_draws = STAT_XFORM_DRAWS.swap(0, Ordering::Relaxed);

    let mut gl_ring_up_ns = 0u64;
    let mut gl_state_app_ns = 0u64;
    let mut gl_draws_ns = 0u64;
    let mut gl_present_bl_ns = 0u64;
    let mut gl_swap_buf_ns = 0u64;
    let mut gl_draws_cnt = 0u32;
    let mut gl_batches_cnt = 0u32;
    unsafe {
        switch_gl_get_perf_stats(
            &mut gl_ring_up_ns,
            &mut gl_state_app_ns,
            &mut gl_draws_ns,
            &mut gl_present_bl_ns,
            &mut gl_swap_buf_ns,
            &mut gl_draws_cnt,
            &mut gl_batches_cnt,
        );
    }

    let (sample_total, guest_samples) = c.drain_sample_table();
    let hle_stats = c.drain_hle_stats();

    if prev_ns == 0 || now_ns <= prev_ns {
        return;
    }
    let wall_ms = (now_ns - prev_ns) as f64 / 1_000_000.0;
    let df = frames.saturating_sub(prev_frames);
    let fps = (df as f64) * 1000.0 / wall_ms;
    let shown = df.max(1) as f64;
    let frame_ms = wall_ms / shown;
    let gx_ms = (gx_ns as f64 / 1_000_000.0) / shown;
    let wait_gx_ms = (gx_wait_ns as f64 / 1_000_000.0) / shown;
    let aud_ms = (aud_ns as f64 / 1_000_000.0) / shown;
    let slp_ms = (slp_ns as f64 / 1_000_000.0) / shown;
    let parse_ms = (parse_ns as f64 / 1_000_000.0) / shown;
    let gl_ms = (gl_ns as f64 / 1_000_000.0) / shown;
    let pump_ms = (pump_ns as f64 / 1_000_000.0) / shown;
    let wait_ms = (wait_ns as f64 / 1_000_000.0) / shown;
    let cpu_game_ms = (frame_ms - wait_gx_ms - aud_ms - slp_ms).max(0.0);
    let la_ms = (lookahead_ns as f64 / 1_000_000.0) / shown;
    let pure_guest_ms = (cpu_game_ms - pump_ms - la_ms).max(0.0);
    let heap_mb = ALLOC_BYTES.load(Ordering::Relaxed) / (1024 * 1024);
    let _cl_ms = (clist_ns as f64 / 1_000_000.0) / shown;
    let _res_ms = (resolve_ns as f64 / 1_000_000.0) / shown;
    let _fv_ms = (finish_v_ns as f64 / 1_000_000.0) / shown;
    let _sh_ms = (spans_h_ns as f64 / 1_000_000.0) / shown;
    let _bl_ms = (build_l_ns as f64 / 1_000_000.0) / shown;

    let p_retrace_gx_ms = (p_retrace_gx_ns as f64 / 1_000_000.0) / shown;
    let p_retrace_cb_ms = (p_retrace_cb_ns as f64 / 1_000_000.0) / shown;
    let p_audio_ms = (p_audio_ns as f64 / 1_000_000.0) / shown;
    let p_alarms_ms = (p_alarms_ns as f64 / 1_000_000.0) / shown;
    let p_sched_ms = (p_sched_ns as f64 / 1_000_000.0) / shown;

    let _cp_ms = (parse_cp_ns as f64 / 1_000_000.0) / shown;
    let _xf_ms = (parse_xf_ns as f64 / 1_000_000.0) / shown;
    let _bp_ms = (parse_bp_ns as f64 / 1_000_000.0) / shown;
    let _dd_ms = (parse_draw_ns as f64 / 1_000_000.0) / shown;
    let ct_ms = (copy_tex_ns as f64 / 1_000_000.0) / shown;
    let cd_ms = (copy_disp_ns as f64 / 1_000_000.0) / shown;
    let _hud_ms = (hud_adj_ns as f64 / 1_000_000.0) / shown;
    let _enc_ms = (encode_pass_ns as f64 / 1_000_000.0) / shown;

    let gl_up_ms = (gl_ring_up_ns as f64 / 1_000_000.0) / shown;
    let gl_st_ms = (gl_state_app_ns as f64 / 1_000_000.0) / shown;
    let gl_dc_ms = (gl_draws_ns as f64 / 1_000_000.0) / shown;
    let gl_bl_ms = (gl_present_bl_ns as f64 / 1_000_000.0) / shown;
    let gl_sw_ms = (gl_swap_buf_ns as f64 / 1_000_000.0) / shown;

    if df == 0 {
        let hle_id = runtime::LAST_HLE.load(Ordering::Relaxed);
        let hle_name = game::HLE_NAMES.get(hle_id).copied().unwrap_or("?");
        let hle_lr = runtime::LAST_HLE_CALLER.load(Ordering::Relaxed);
        log_c(&format!(
            "[perf] vi={} frames={} (+0) | lr={:#010x} ({}) last_hle={} from {:#010x} ({}) | {}",
            retrace,
            frames,
            c.lr,
            c.func_name(c.lr),
            hle_name,
            hle_lr,
            c.func_name(hle_lr),
            sched::describe()
        ));
        return;
    }

    let gmode = CURRENT_GAME_MODE.load(Ordering::Relaxed);
    log_c(&format!(
        "[perf] vi={} frames={} (+{}) | mode={} | {:.1} FPS ({:.2} ms/f) | draws={} [c:{} r:{} x:{}] verts={} dl_cache_v={} heap={}MB | {}",
        retrace, frames, df, gmode, fps, frame_ms, draws, cached_draws / df.max(1) as u32, raw_draws / df.max(1) as u32, xform_draws / df.max(1) as u32, verts, cached_v, heap_mb, sched::describe()
    ));
    log_c(&format!(
        "  [cpu-core1] game={:.2}ms (pure_guest={:.2}ms wait_gx={:.2}ms pump={:.2}ms [retrace_gx={:.2}ms cb={:.2}ms aud={:.2}ms alm={:.2}ms sc={:.2}ms] wait={:.2}ms) | aud={:.2}ms slp={:.2}ms",
        cpu_game_ms, pure_guest_ms, wait_gx_ms, pump_ms, p_retrace_gx_ms, p_retrace_cb_ms, p_audio_ms, p_alarms_ms, p_sched_ms, wait_ms, aud_ms, slp_ms
    ));
    {
        let frames = df.max(1) as f64;
        let mut dl = String::from("  [dlstat]");
        for k in 0..DLSTAT_NAMES.len() {
            let v = DLSTAT[k].swap(0, Ordering::Relaxed);
            let v = if k >= 6 { v / 1024 } else { v };
            dl += &format!(" {}={:.0}", DLSTAT_NAMES[k], v as f64 / frames);
        }
        log_c(&dl);
        let mut line = String::from("  [sync]");
        for k in 0..SYNC_NAMES.len() {
            let ns = SYNC_NS[k].swap(0, Ordering::Relaxed);
            let n = SYNC_N[k].swap(0, Ordering::Relaxed);
            line += &format!(" {}={:.2}ms/{:.1}x", SYNC_NAMES[k], ns as f64 / 1e6 / frames, n as f64 / frames);
        }
        log_c(&line);
    }
    let cl_raw_fail = STAT_CLIST_RAW_FAIL.swap(0, Ordering::Relaxed) / df.max(1) as u32;
    let cl_hud_fail = STAT_CLIST_HUD_FAIL.swap(0, Ordering::Relaxed) / df.max(1) as u32;
    let cl_res_fail = STAT_CLIST_RES_FAIL.swap(0, Ordering::Relaxed) / df.max(1) as u32;
    log_c(&format!(
        "  [gx-core0]  gx={:.2}ms (parse_cpu={:.2}ms [dls:{} bps:{} cps:{} xfs:{}] | cl={:.2}ms (res={:.2}ms fails:[raw:{} hud:{} res:{}]) bp={:.2}ms cp={:.2}ms xf={:.2}ms dd={:.2}ms fv={:.2}ms | copy_tex={:.2}ms [{}x] copy_disp={:.2}ms [{}x] gl_enc={:.2}ms)",
        gx_ms, parse_ms, dls / df.max(1) as u32, bps / df.max(1) as u32, cps / df.max(1) as u32, xfs / df.max(1) as u32,
        _cl_ms, _res_ms, cl_raw_fail, cl_hud_fail, cl_res_fail, _bp_ms, _cp_ms, _xf_ms, _dd_ms, _fv_ms, ct_ms, copy_tex_cnt / df.max(1) as u32, cd_ms, copy_disp_cnt / df.max(1) as u32, gl_ms
    ));
    log_c(&format!(
        "  [gl-core2]  upload={:.2}ms state={:.2}ms draw_calls={:.2}ms [{} draws in {} batches] blit={:.2}ms swap_buffers={:.2}ms",
        gl_up_ms, gl_st_ms, gl_dc_ms, gl_draws_cnt / df.max(1) as u32, gl_batches_cnt / df.max(1) as u32, gl_bl_ms, gl_sw_ms
    ));

    // Print Top 6 Sampled Guest Functions
    if sample_total > 0 && !guest_samples.is_empty() {
        let mut msg = String::from("  [hot-guest] ");
        for (rank, &(lr, count)) in guest_samples.iter().take(6).enumerate() {
            let pct = (count as f64 * 100.0) / (sample_total as f64);
            let name = c.func_name(lr);
            msg += &format!("{}. {:.1}% ({}) {:#010x} {} | ", rank + 1, pct, count, lr, name);
        }
        log_c(&msg);
    }

    if runtime::PROFILE_FN {
        let (total, top) = drain_prof();
        if total > 0 {
            let mut msg = format!("  [prof] {} samples | ", total);
            for &(addr, n) in top.iter().take(15) {
                msg += &format!("{:.1}% {} | ", n as f64 * 100.0 / total as f64, c.func_name(addr));
            }
            log_c(&msg);
        }
        let counts: [u32; 16] = core::array::from_fn(|i| GX_PHASE_COUNT[i].swap(0, Ordering::Relaxed));
        let all: u32 = counts.iter().sum();
        if all > 0 {
            let w: [u32; 5] = core::array::from_fn(|i| CPUV_WHY[i].swap(0, Ordering::Relaxed));
            let f = df.max(1) as u32;
            log_c(&format!("  [cpuv] per frame: ortho={} pnmtxidx={} texmtxidx={} tex4_7={} texgen4_7={}", w[0] / f, w[1] / f, w[2] / f, w[3] / f, w[4] / f));
            let mut msg = String::from("  [gxprof] ");
            for (i, &n) in counts.iter().enumerate().filter(|(_, &n)| n > 0) {
                msg += &format!("{} {:.1}% | ", GX_PHASE_NAMES[i], n as f64 * 100.0 / all as f64);
            }
            log_c(&msg);
        }
    }

    // Print Top 6 Most Expensive / Frequent HLE Calls
    if !hle_stats.is_empty() {
        let mut msg = String::from("  [hot-hle] ");
        for (rank, &(name, count, ticks)) in hle_stats.iter().take(6).enumerate() {
            if ticks > 0 {
                let hle_ms = (ticks as f64 * 125.0 / 24.0) / 1_000_000.0 / shown;
                msg += &format!("{}. {}: {}c ({:.2}ms) | ", rank + 1, name, count / df.max(1) as u32, hle_ms);
            } else {
                msg += &format!("{}. {}: {}c | ", rank + 1, name, count / df.max(1) as u32);
            }
        }
        log_c(&msg);
    }
}

#[inline(always)]
fn events_or_timer_due(now: u64) -> bool {
    if HAS_EVENTS.load(Ordering::Relaxed) {
        return true;
    }
    let due = NEXT_DUE_TIME.load(Ordering::Relaxed);
    due == 0 || now >= due
}

#[inline(always)]
fn deliver_events(c: &mut Ctx) {
    check_exit(c);
    if !INTERRUPTS_ENABLED.load(Ordering::Relaxed) || IN_HANDLER.load(Ordering::Relaxed) {
        return;
    }
    let now = c.timebase();
    if !events_or_timer_due(now) {
        return;
    }
    deliver_events_slow(c);
}

fn deliver_events_slow(c: &mut Ctx) {
    IN_HANDLER.store(true, Ordering::Relaxed);
    let saved = SavedRegs::save(c);

    loop {
        if let Some(event) = pop_event() {
            match event {
                Event::DvdDone {
                    block,
                    callback,
                    result,
                } => {
                    c.w32(block + 0x0C, 0); // DVD_STATE_END
                    if callback != 0 {
                        c.r[3] = result as u32;
                        c.r[4] = block;
                        c.call(callback);
                        saved.restore(c);
                    }
                }
                Event::GxToken(token) => {
                    let cb = DRAW_SYNC_CB.load(Ordering::Relaxed);
                    if cb != 0 {
                        c.r[3] = token as u32;
                        c.call(cb);
                        saved.restore(c);
                    }
                }
                Event::GxBreak => {
                    let cb = BREAK_PT_CB.load(Ordering::Relaxed);
                    if cb != 0 {
                        c.call(cb);
                        saved.restore(c);
                    }
                }
                Event::GxFinish => {
                    let cb = DRAW_DONE_CB.load(Ordering::Relaxed);
                    if cb != 0 {
                        c.call(cb);
                        saved.restore(c);
                    }
                }
                Event::Call(callback, arg) => {
                    if callback != 0 {
                        c.r[3] = arg;
                        c.call(callback);
                        saved.restore(c);
                    }
                }
                Event::CardDone {
                    chan,
                    callback,
                    result,
                } => {
                    card::completed(result);
                    if callback != 0 {
                        c.r[3] = chan;
                        c.r[4] = result as u32;
                        c.call(callback);
                        saved.restore(c);
                    }
                }
            }
            continue;
        }

        let now = c.timebase();

        // 1. Phase-locked 60.000 Hz VI Retrace tick
        let next_retrace = NEXT_RETRACE_TIME.load(Ordering::Relaxed);
        if next_retrace == 0 {
            NEXT_RETRACE_TIME.store(now + RETRACE_INTERVAL, Ordering::Relaxed);
        } else if now >= next_retrace {
            NEXT_RETRACE_TIME.store(
                advance_due(next_retrace, RETRACE_INTERVAL, now),
                Ordering::Relaxed,
            );
            let count = RETRACE_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
            uncap_frame_rate(c);

            // The vertical interrupt: let the GPU catch up before the retrace callbacks
            // advance RenderWare's shown-frame counter (matches desktop host.rs), so the
            // game never reuses dynamic vertex/light buffers that are still undecoded.
            //
            // Only a retrace that shows a frame moves that counter, so Core 1 waits for Core 0
            // only when a finished frame is waiting to be shown (the same test as
            // replace_get_xfbs). Retraces in the middle of building a frame (two or three per
            // frame below 60 FPS) used to stall Core 1 behind Core 0's backlog for nothing.
            let t_retrace_gx = unsafe { switch_now_ns() };
            let frame_pending = c.r32(0x804B_23AC) == 1 || c.r32(0x804B_23A0) != c.r32(0x804B_23A4);
            if frame_pending || STRICT_SYNC.load(Ordering::Relaxed) {
                timed_sync(0, c, gx_process);
            } else {
                gx_worker::push_fifo(c, 1);
            }
            PERF_PUMP_RETRACE_GX_NS.fetch_add(
                unsafe { switch_now_ns().saturating_sub(t_retrace_gx) },
                Ordering::Relaxed,
            );

            let t_retrace_cb = unsafe { switch_now_ns() };
            // Deliver any GPU sync tokens raised by gx_process before running VI retrace callbacks
            while let Some(ev) = pop_event() {
                match ev {
                    Event::GxToken(token) => {
                        let cb = DRAW_SYNC_CB.load(Ordering::Relaxed);
                        if cb != 0 {
                            c.r[3] = token as u32;
                            c.call(cb);
                            saved.restore(c);
                        }
                    }
                    Event::GxFinish => {
                        let cb = DRAW_DONE_CB.load(Ordering::Relaxed);
                        if cb != 0 {
                            c.call(cb);
                            saved.restore(c);
                        }
                    }
                    Event::GxBreak => {
                        let cb = BREAK_PT_CB.load(Ordering::Relaxed);
                        if cb != 0 {
                            c.call(cb);
                            saved.restore(c);
                        }
                    }
                    other => {
                        push_event(other);
                        break;
                    }
                }
            }

            let pre = PRE_RETRACE.load(Ordering::Relaxed);
            if pre != 0 {
                c.r[3] = count;
                c.call(pre);
                saved.restore(c);
            }

            let post = POST_RETRACE.load(Ordering::Relaxed);
            if post != 0 {
                c.r[3] = count;
                c.call(post);
                saved.restore(c);
            }
            PERF_PUMP_RETRACE_CB_NS.fetch_add(
                unsafe { switch_now_ns().saturating_sub(t_retrace_cb) },
                Ordering::Relaxed,
            );

            sched::wake(sched::RETRACE_QUEUE);

            if count % 20 == 0 {
                log_perf_summary(c, count);
            }
            continue;
        }

        // 2. Phase-locked 5.000 ms Audio / DSP frame tick
        let next_audio = NEXT_AUDIO_TIME.load(Ordering::Relaxed);
        let dma_cb = AI_DMA_CALLBACK.load(Ordering::Relaxed);
        if next_audio == 0 {
            NEXT_AUDIO_TIME.store(now + AUDIO_INTERVAL, Ordering::Relaxed);
        } else if AUDIO_DMA_RUNNING.load(Ordering::Relaxed) && dma_cb != 0 && now >= next_audio {
            NEXT_AUDIO_TIME.store(now + AUDIO_INTERVAL, Ordering::Relaxed);
            let t0 = unsafe { switch_now_ns() };
            audio::mix_frame(c);
            saved.restore(c);

            let task = DSP_TASK.load(Ordering::Relaxed);
            let resume = if task != 0 { c.r32(task + 0x2C) } else { 0 };
            if resume != 0 {
                c.r[3] = task;
                c.call(resume);
                saved.restore(c);
            }
            c.call(dma_cb);
            saved.restore(c);
            let dt = unsafe { switch_now_ns().saturating_sub(t0) };
            PERF_AUDIO_NS.fetch_add(dt, Ordering::Relaxed);
            PERF_PUMP_AUDIO_NS.fetch_add(dt, Ordering::Relaxed);
            continue;
        }

        // 3. OS Alarms
        let due_alarm = {
            lock_alarms();
            #[allow(static_mut_refs)]
            let found = unsafe {
                ALARMS.iter().position(|a| a.due <= now).map(|i| {
                    let (ptr, handler) = (ALARMS[i].ptr, ALARMS[i].handler);
                    if ALARMS[i].period != 0 {
                        let a = &mut ALARMS[i];
                        a.due += a.period * ((now - a.due) / a.period + 1);
                    } else {
                        ALARMS.swap_remove(i);
                    }
                    (handler, ptr)
                })
            };
            unlock_alarms();
            found
        };
        if let Some((handler, ptr)) = due_alarm {
            if handler != 0 {
                let t_alm = unsafe { switch_now_ns() };
                c.r[3] = ptr;
                c.r[4] = 0x817E_0400; // alarm OSContext scratch
                c.call(handler);
                saved.restore(c);
                PERF_PUMP_ALARMS_NS.fetch_add(
                    unsafe { switch_now_ns().saturating_sub(t_alm) },
                    Ordering::Relaxed,
                );
            }
            continue;
        }

        break;
    }

    saved.restore(c);
    update_next_due();
    IN_HANDLER.store(false, Ordering::Relaxed);
}

#[inline(always)]
fn pump(c: &mut Ctx) {
    deliver_events(c);
    sched::preempt(c);
}

/// Called by `sched::reschedule` on Core 1 when every guest thread is blocked: sleeps in the
/// Horizon OS kernel via `svcSleepThread` until the exact nanosecond of the next VI retrace,
/// 5 ms audio frame, or OSAlarm (~0.00% CPU while waiting).
fn idle(c: &mut Ctx) {
    check_exit(c);
    gx_worker::push_fifo(c, 1);
    let now = c.timebase();
    let due = if has_pending_events() {
        now
    } else {
        let next_retrace = {
            let r = NEXT_RETRACE_TIME.load(Ordering::Relaxed);
            if r == 0 {
                now + RETRACE_INTERVAL
            } else {
                r
            }
        };
        let next_audio = if AUDIO_DMA_RUNNING.load(Ordering::Relaxed)
            && AI_DMA_CALLBACK.load(Ordering::Relaxed) != 0
        {
            let a = NEXT_AUDIO_TIME.load(Ordering::Relaxed);
            if a == 0 {
                now + AUDIO_INTERVAL
            } else {
                a
            }
        } else {
            u64::MAX
        };
        next_retrace.min(next_audio).min(next_alarm_due())
    };

    if due > now {
        // 40.5 MHz ticks -> nanoseconds: ticks * 1_000_000_000 / 40_500_000 = ticks * 200 / 81
        let wait_ns = (due - now).saturating_mul(200) / 81;
        if wait_ns > 0 {
            let t0 = unsafe { switch_now_ns() };
            unsafe {
                switch_sleep_ns(wait_ns);
            }
            let slept = unsafe { switch_now_ns().saturating_sub(t0) };
            PERF_SLEEP_NS.fetch_add(slept, Ordering::Relaxed);
        }
    }
    deliver_events(c);
}

/// Serializes RenderWare's 3-slot frame queue across the loading-screen animation thread and
/// the main thread (ported from crates/tssm/src/hle.rs::before_show_raster).
fn before_show_raster(c: &mut Ctx) {
    if sched::me() != 0 {
        let saved = SavedRegs::save(c);
        gx_worker::push_fifo(c, 1);
        sched::sleep(c, sched::RETRACE_QUEUE);
        saved.restore(c);
        return;
    }
    const FRAME_QUEUE_SLEEPERS: u32 = 0x804B_23C0;
    if !sched::has_sleepers(FRAME_QUEUE_SLEEPERS) || IN_HANDLER.load(Ordering::Relaxed) {
        return;
    }
    let t0 = unsafe { switch_now_ns() };
    let saved = SavedRegs::save(c);
    while sched::has_sleepers(FRAME_QUEUE_SLEEPERS) && !EXIT_REQUESTED.load(Ordering::Relaxed) {
        gx_worker::push_fifo(c, 1);
        sched::sleep(c, sched::RETRACE_QUEUE);
    }
    saved.restore(c);
    let dt = unsafe { switch_now_ns().saturating_sub(t0) };
    PERF_WAIT_NS.fetch_add(dt, Ordering::Relaxed);
}

/// Sleeps cleanly in the kernel instead of guest busy-waiting for submitted XFBs to be shown,
/// and directly assigns XFB1 and XFB0 pointers to avoid any guest spin loop.
fn replace_get_xfbs(c: &mut Ctx) -> bool {
    const HANDING_OVER: u32 = 0x804B_23AC;
    const SUBMITTED: u32 = 0x804B_23A0;
    const SHOWN: u32 = 0x804B_23A4;
    const XFB1: u32 = 0x804B_2394;
    const XFB0: u32 = 0x804B_2390;
    let waiting = |c: &Ctx| c.r32(HANDING_OVER) == 1 || c.r32(SUBMITTED) != c.r32(SHOWN);
    if waiting(c) && !IN_HANDLER.load(Ordering::Relaxed) {
        let t0 = unsafe { switch_now_ns() };
        let saved = SavedRegs::save(c);
        while waiting(c) && !EXIT_REQUESTED.load(Ordering::Relaxed) {
            gx_worker::push_fifo(c, 1);
            sched::sleep(c, sched::RETRACE_QUEUE);
        }
        saved.restore(c);
        let dt = unsafe { switch_now_ns().saturating_sub(t0) };
        PERF_WAIT_NS.fetch_add(dt, Ordering::Relaxed);
    }
    let p1 = c.r[3];
    let p0 = c.r[4];
    if p1 != 0 {
        let v1 = c.r32(XFB1);
        c.w32(p1, v1);
    }
    if p0 != 0 {
        let v0 = c.r32(XFB0);
        c.w32(p0, v0);
    }
    true
}

fn call_display_list(c: &mut Ctx) -> bool {
    const GX_CALL_DISPLAY_LIST: u32 = 0x802E_F450;
    static INSIDE: AtomicBool = AtomicBool::new(false);
    if INSIDE.swap(true, Ordering::Relaxed) {
        return false;
    }
    c.call(GX_CALL_DISPLAY_LIST);
    INSIDE.store(false, Ordering::Relaxed);
    gx_worker::push_fifo(c, 4096);
    true
}

fn flush_hook(c: &mut Ctx) -> bool {
    const GX_FLUSH: u32 = 0x802E_B5A0;
    static INSIDE: AtomicBool = AtomicBool::new(false);
    if INSIDE.swap(true, Ordering::Relaxed) {
        return false;
    }
    c.call(GX_FLUSH);
    INSIDE.store(false, Ordering::Relaxed);
    gx_worker::push_fifo(c, 1);
    true
}

fn copy_disp_hook(c: &mut Ctx) -> bool {
    const GX_COPY_DISP: u32 = 0x802E_C7E4;
    static INSIDE: AtomicBool = AtomicBool::new(false);
    if INSIDE.swap(true, Ordering::Relaxed) {
        return false;
    }
    c.call(GX_COPY_DISP);
    INSIDE.store(false, Ordering::Relaxed);
    gx_worker::push_fifo(c, 1);
    true
}

fn copy_tex_hook(c: &mut Ctx) -> bool {
    const GX_COPY_TEX: u32 = 0x802E_C94C;
    static INSIDE: AtomicBool = AtomicBool::new(false);
    if INSIDE.swap(true, Ordering::Relaxed) {
        return false;
    }
    c.call(GX_COPY_TEX);
    INSIDE.store(false, Ordering::Relaxed);
    gx_worker::push_fifo(c, 1);
    true
}

fn guest_poll_hook(c: &mut Ctx) {
    runtime::sample_guest_pc(c.lr);
    gx_worker::push_fifo(c, 4096);
    let saved = SavedRegs::save(c);
    pump(c);
    if sched::me() != 0 {
        sched::reschedule(c, true);
    }
    saved.restore(c);
}

fn set_interrupts(c: &mut Ctx, enabled: bool) {
    let prev = INTERRUPTS_ENABLED.swap(enabled, Ordering::Relaxed);
    if enabled {
        if HAS_EVENTS.load(Ordering::Relaxed) {
            deliver_events(c);
        } else {
            static EVENT_COUNTDOWN: AtomicU32 = AtomicU32::new(0);
            if (EVENT_COUNTDOWN.fetch_add(1, Ordering::Relaxed) & 63) == 0 {
                deliver_events(c);
            }
        }
        sched::preempt(c);
    }
    c.r[3] = prev as u32;
}

fn guest_text(c: &Ctx, mut addr: u32) -> String {
    let mut bytes = Vec::new();
    while (0x8000_0000..0x8180_0000).contains(&addr) && c.r8(addr) != 0 && bytes.len() < 1024 {
        bytes.push(c.r8(addr));
        addr += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn guest_format(c: &Ctx, format_reg: usize) -> String {
    let format = guest_text(c, c.r[format_reg]);
    let (mut next_int, mut next_float) = (format_reg + 1, 1usize);
    let mut int = |c: &Ctx| {
        next_int += 1;
        match next_int - 1 {
            reg @ 0..=10 => c.r[reg],
            n => c.r32(c.r[1] + 8 + 4 * (n as u32 - 11)),
        }
    };
    let mut out = String::new();
    let mut chars = format.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        while chars.peek().is_some_and(|c| "-+ #0123456789.lh".contains(*c)) {
            chars.next();
        }
        match chars.next() {
            Some('d' | 'i') => out += &(int(c) as i32).to_string(),
            Some('u') => out += &int(c).to_string(),
            Some('x' | 'p') => out += &format!("{:x}", int(c)),
            Some('X') => out += &format!("{:X}", int(c)),
            Some('c') => out.push(int(c) as u8 as char),
            Some('s') => {
                let addr = int(c);
                out += &guest_text(c, addr);
            }
            Some('f' | 'g' | 'e') => {
                out += &c
                    .f
                    .get(next_float)
                    .map_or(String::from("?"), |v| format!("{:.2}", v));
                next_float += 1;
            }
            Some('%') => out.push('%'),
            Some(other) => out += &format!("%{other}"),
            None => {}
        }
    }
    out
}

fn linker_sym(name: &str) -> u32 {
    game::LINKER_SYMS
        .iter()
        .find(|s| s.0 == name)
        .map_or(0, |s| s.1)
}

fn guest_call(c: &mut Ctx, name: &str, args: &[u32]) {
    if let Some(f) = c.func_by_name(name) {
        c.r[3..3 + args.len()].copy_from_slice(args);
        f(c);
    }
}

/// CPU skinning (`_rwDlSkinUpdate*`): the native NEON versions in arm64_native by default (they
/// matched the original bit for bit over ~12k calls in BB02). `sdmc:/skinoriginal.flag` runs the
/// recompiled original instead; with
/// `sdmc:/skinverify.flag` every call runs both, keeps the original's output and logs how far the
/// native output was from it (`[skinverify]` lines).
mod skin_verify {
    use super::*;

    /// 0 = original, 1 = native, 2 = verify.
    static MODE: AtomicU32 = AtomicU32::new(0);
    static BUSY: AtomicBool = AtomicBool::new(false);
    static CALLS: [AtomicU32; 6] = [const { AtomicU32::new(0) }; 6];
    static BAD_CALLS: [AtomicU32; 6] = [const { AtomicU32::new(0) }; 6];
    static MAX_ERR_E6: [AtomicU32; 6] = [const { AtomicU32::new(0) }; 6];
    const NAMES: [&str; 6] = ["2P", "2PN", "3P", "3PN", "4P", "4PN"];
    const GUEST: [&str; 6] = [
        "_rwDlSkinUpdate2WeightsP",
        "_rwDlSkinUpdate2WeightsPN",
        "_rwDlSkinUpdate3WeightsP",
        "_rwDlSkinUpdate3WeightsPN",
        "_rwDlSkinUpdate4WeightsP",
        "_rwDlSkinUpdate4WeightsPN",
    ];
    const NATIVE: [fn(&mut Ctx) -> bool; 6] = [
        arm64_native::hle_skin_2w_p,
        arm64_native::hle_skin_2w_pn,
        arm64_native::hle_skin_3w_p,
        arm64_native::hle_skin_3w_pn,
        arm64_native::hle_skin_4w_p,
        arm64_native::hle_skin_4w_pn,
    ];

    pub fn init() {
        let mode = if unsafe { switch_file_exists(b"sdmc:/skinverify.flag\0".as_ptr()) } != 0 {
            2
        } else if unsafe { switch_file_exists(b"sdmc:/skinoriginal.flag\0".as_ptr()) } != 0 {
            0
        } else {
            1
        };
        MODE.store(mode, Ordering::Relaxed);
        // Lazy end-of-frame sync is opt-in until it has run on real hardware: a Switch hung
        // at the first screens with it on by default (Ryujinx did not).
        let lazy = unsafe { switch_file_exists(b"sdmc:/lazysync.flag\0".as_ptr()) } != 0;
        let strict = unsafe { switch_file_exists(b"sdmc:/strictsync.flag\0".as_ptr()) } != 0;
        gx_worker::LAZY_SYNC.store(lazy && !strict, Ordering::Relaxed);
        STRICT_SYNC.store(strict, Ordering::Relaxed);
        log_c(&format!("[switch] sync mode: lazy={} strict={} (sdmc:/lazysync.flag, sdmc:/strictsync.flag)", lazy && !strict, strict));
        log_c(&format!("[skinverify] skinning mode {}", ["original", "native", "verify"][mode as usize]));
    }

    pub fn policy(k: usize) -> Hle {
        match MODE.load(Ordering::Relaxed) {
            1 => Hle::Replace([native::<0>, native::<1>, native::<2>, native::<3>, native::<4>, native::<5>][k]),
            2 => Hle::Replace([verify::<0>, verify::<1>, verify::<2>, verify::<3>, verify::<4>, verify::<5>][k]),
            _ => Hle::Before(gx_worker::settle_frame),
        }
    }

    fn native<const K: usize>(c: &mut Ctx) -> bool {
        gx_worker::settle_frame(c);
        NATIVE[K](c)
    }

    fn verify<const K: usize>(c: &mut Ctx) -> bool {
        if BUSY.load(Ordering::Relaxed) {
            return false; // the original, called from below
        }
        let normals = K % 2 == 1;
        let ctx = c.r[5];
        let n = c.r32(ctx.wrapping_add(0x1C)) as i32;
        if n <= 0 || n > 1 << 16 {
            return false;
        }
        let n = n as u32;
        let out_pos = c.r32(ctx);
        let out_nrm = if normals { c.r32(ctx.wrapping_add(4)) } else { 0 };
        let nrm_stride = c.r32(ctx.wrapping_add(0x14)).wrapping_mul(3).wrapping_add(c.r32(ctx.wrapping_add(0x18)));
        let pos_len = (n * 12) as usize;
        let nrm_len = if normals { (n * nrm_stride) as usize } else { 0 };
        let pos_before = c.read_bytes(out_pos, pos_len);
        let nrm_before = if normals { c.read_bytes(out_nrm, nrm_len) } else { Vec::new() };
        let (r, f, ps1, cr, lr, ctr) = (c.r, c.f, c.ps1, c.cr, c.lr, c.ctr);

        if !NATIVE[K](c) {
            return false;
        }
        let pos_native = c.read_bytes(out_pos, pos_len);
        let nrm_native = if normals { c.read_bytes(out_nrm, nrm_len) } else { Vec::new() };
        c.slice_mut(out_pos, pos_len).copy_from_slice(&pos_before);
        if normals {
            c.slice_mut(out_nrm, nrm_len).copy_from_slice(&nrm_before);
        }
        (c.r, c.f, c.ps1, c.cr, c.lr, c.ctr) = (r, f, ps1, cr, lr, ctr);

        let Some(original) = c.func_by_name(GUEST[K]) else {
            return false;
        };
        BUSY.store(true, Ordering::Relaxed);
        original(c);
        BUSY.store(false, Ordering::Relaxed);

        let pos_orig = c.read_bytes(out_pos, pos_len);
        let mut err = rel_err(&pos_native, &pos_orig, 12, 12);
        if normals {
            let nrm_orig = c.read_bytes(out_nrm, nrm_len);
            err = err.max(rel_err(&nrm_native, &nrm_orig, nrm_stride as usize, 12));
        }
        let calls = CALLS[K].fetch_add(1, Ordering::Relaxed) + 1;
        if err > 1e-3 {
            BAD_CALLS[K].fetch_add(1, Ordering::Relaxed);
        }
        MAX_ERR_E6[K].fetch_max((err * 1e6).min(4e9) as u32, Ordering::Relaxed);
        if calls % 2000 == 0 {
            let mut line = String::from("[skinverify]");
            for k in 0..6 {
                line += &format!(
                    " {}: {} calls {} bad max_err={:.6} |",
                    NAMES[k],
                    CALLS[k].load(Ordering::Relaxed),
                    BAD_CALLS[k].load(Ordering::Relaxed),
                    MAX_ERR_E6[k].load(Ordering::Relaxed) as f64 / 1e6
                );
            }
            log_c(&line);
        }
        true
    }

    /// Largest relative difference between the big-endian f32 triples of two vertex streams.
    fn rel_err(a: &[u8], b: &[u8], stride: usize, used: usize) -> f64 {
        let mut worst = 0f64;
        for v in (0..a.len()).step_by(stride.max(1)) {
            for k in (0..used).step_by(4) {
                let at = v + k;
                if at + 4 > a.len() {
                    break;
                }
                let x = f32::from_be_bytes(a[at..at + 4].try_into().unwrap()) as f64;
                let y = f32::from_be_bytes(b[at..at + 4].try_into().unwrap()) as f64;
                let e = if x == y { 0.0 } else { (x - y).abs() / y.abs().max(1.0) };
                if !(e <= worst) {
                    worst = if e.is_nan() { 1e9 } else { e };
                }
            }
        }
        worst
    }
}

fn switch_policy(name: &str) -> Hle {
    match name {
        "OSReport" => Hle::Native(|c| {
            let msg = guest_format(c, 3);
            log_c(&format!("[game] {}", msg.trim_end()));
        }),
        "OSPanic" => Hle::Native(|c| {
            let msg = guest_format(c, 5);
            log_c(&format!("[game OSPanic] {}", msg.trim_end()));
        }),
        "OSInit" => Hle::Native(|c| {
            c.w32(0x8000_0028, runtime::mem::RAM_SIZE);
            c.w32(0x8000_002C, 3);
            c.w32(0x8000_00CC, 0);
            c.w32(0x8000_00F0, runtime::mem::RAM_SIZE);
            c.w32(0x8000_00F4, 486_000_000);
            c.w32(0x8000_00F8, 162_000_000);
            c.w32(0x8000_00FC, 486_000_000);

            let top = runtime::mem::RAM_BASE + runtime::mem::RAM_SIZE;
            let scratch = top - 0x20000; // 0x817E_0000 (view windows live at 0x817F_8800)
            sched::init(scratch);
            let lo = (linker_sym("_stack_addr") + 31) & !31;
            guest_call(c, "OSSetArenaLo", &[lo]);
            guest_call(c, "OSSetArenaHi", &[scratch]);

            c.slice_mut(scratch, 0x318).fill(0);
            c.w16(scratch + 0x2C8, 2); // OS_THREAD_STATE_RUNNING
            c.w32(scratch + 0x2D0, 16);
            c.w32(0x8000_00E4, scratch);
            c.guest_thread = scratch;
        }),

        "OSGetArenaLo" | "OSGetArenaHi" | "OSSetArenaLo" | "OSSetArenaHi" | "OSInitAlloc"
        | "OSCreateHeap" | "OSDestroyHeap" | "OSSetCurrentHeap" | "OSAllocFromHeap"
        | "OSFreeToHeap" | "OSAllocFixed" | "OSAddToHeap" | "OSCheckHeap" | "OSReferentSize"
        | "OSDumpHeap" | "OSAllocFromArenaLo" | "OSAllocFromArenaHi" | "OSCreateAlarm"
        | "OSInitThreadQueue" | "OSGetConsoleType" => Hle::Original,

        "OSCheckActiveThreads" => Hle::Native(|c| c.r[3] = 0),
        "__OSSetInterruptHandler" | "__OSGetInterruptHandler" => Hle::Native(|c| c.r[3] = 0),
        "OSDisableInterrupts" => Hle::Native(|c| set_interrupts(c, false)),
        "OSEnableInterrupts" => Hle::Native(|c| set_interrupts(c, true)),
        "OSRestoreInterrupts" => Hle::Native(|c| {
            let enabled = c.r[3] != 0;
            set_interrupts(c, enabled);
        }),
        "__OSUnmaskInterrupts" | "__OSMaskInterrupts" => Hle::Native(|c| c.r[3] = 0),
        "OSSetErrorHandler" | "OSSetResetCallback" => Hle::Native(|c| c.r[3] = 0),
        "OSGetResetButtonState" | "OSGetResetSwitchState" => Hle::Native(|c| c.r[3] = 0),

        "OSSetAlarm" => Hle::Native(|c| {
            let ptr = c.r[3];
            let ticks = ((c.r[5] as u64) << 32) | (c.r[6] as u64);
            let handler = c.r[7];
            let due = c.timebase() + ticks;
            c.w32(ptr, handler);
            lock_alarms();
            #[allow(static_mut_refs)]
            unsafe {
                ALARMS.retain(|a| a.ptr != ptr);
                ALARMS.push(Alarm {
                    ptr,
                    due,
                    period: 0,
                    handler,
                });
            }
            unlock_alarms();
            update_next_due();
        }),
        "OSSetPeriodicAlarm" => Hle::Native(|c| {
            let ptr = c.r[3];
            let start = ((c.r[5] as u64) << 32) | (c.r[6] as u64);
            let period = (((c.r[7] as u64) << 32) | (c.r[8] as u64)).max(1);
            let handler = c.r[9];
            let now = c.timebase();
            let due = now + start.saturating_sub(now);
            c.w32(ptr, handler);
            lock_alarms();
            #[allow(static_mut_refs)]
            unsafe {
                ALARMS.retain(|a| a.ptr != ptr);
                ALARMS.push(Alarm {
                    ptr,
                    due,
                    period,
                    handler,
                });
            }
            unlock_alarms();
            update_next_due();
        }),
        "OSCancelAlarm" => Hle::Native(|c| {
            let ptr = c.r[3];
            c.w32(ptr, 0);
            lock_alarms();
            #[allow(static_mut_refs)]
            unsafe {
                ALARMS.retain(|a| a.ptr != ptr);
            }
            unlock_alarms();
            update_next_due();
        }),

        // Cooperative single-baton threads, semaphores, and mutexes
        "OSCreateThread" => Hle::Native(sched::create),
        "OSGetCurrentThread" => Hle::Native(|c| c.r[3] = sched::current_guest()),
        "OSGetThreadPriority" => Hle::Native(|c| c.r[3] = sched::priority(c.r[3]) as u32),
        "OSResumeThread" => Hle::Native(|c| {
            let old = sched::resume(c.r[3]);
            sched::preempt(c);
            c.r[3] = old as u32;
        }),
        "OSSuspendThread" => Hle::Native(|c| {
            let thread = c.r[3];
            c.r[3] = sched::suspend(c, thread) as u32;
        }),
        "OSYieldThread" => Hle::Native(|c| sched::reschedule(c, true)),
        "OSSleepThread" => Hle::Native(|c| {
            let queue = c.r[3];
            sched::sleep(c, queue);
        }),
        "OSWakeupThread" => Hle::Native(|c| {
            sched::wake(c.r[3]);
            sched::preempt(c);
        }),
        "OSExitThread" => Hle::Native(|c| sched::exit_current(c)),
        "OSIsThreadSuspended" => Hle::Native(|c| c.r[3] = sched::is_suspended(c.r[3]) as u32),

        "OSInitSemaphore" => Hle::Native(|c| {
            let (sem, count) = (c.r[3], c.r[4]);
            c.w32(sem, count);
            c.slice_mut(sem + 4, 8).fill(0);
        }),
        "OSWaitSemaphore" => Hle::Native(|c| {
            let sem = c.r[3];
            let q = sem + 4;
            while (c.r32(sem) as i32) <= 0 && !EXIT_REQUESTED.load(Ordering::Relaxed) {
                sched::sleep(c, q);
            }
            let old = c.r32(sem) as i32;
            c.w32(sem, (old - 1) as u32);
            c.r[3] = old as u32;
        }),
        "OSTryWaitSemaphore" => Hle::Native(|c| {
            let sem = c.r[3];
            let count = c.r32(sem) as i32;
            if count > 0 {
                c.w32(sem, (count - 1) as u32);
                c.r[3] = 1;
            } else {
                c.r[3] = 0;
            }
        }),
        "OSSignalSemaphore" => Hle::Native(|c| {
            let sem = c.r[3];
            let q = sem + 4;
            let old = c.r32(sem) as i32;
            c.w32(sem, (old + 1) as u32);
            sched::wake(q);
            sched::preempt(c);
            c.r[3] = old as u32;
        }),
        "OSGetSemaphoreCount" => Hle::Native(|c| {
            c.r[3] = c.r32(c.r[3]);
        }),

        "OSInitMutex" => Hle::Native(|c| {
            let mutex = c.r[3];
            c.slice_mut(mutex, 0x18).fill(0);
        }),
        "OSLockMutex" => Hle::Native(|c| {
            let (mutex, me) = (c.r[3], sched::current_guest());
            while c.r32(mutex + 8) != 0
                && c.r32(mutex + 8) != me
                && !EXIT_REQUESTED.load(Ordering::Relaxed)
            {
                sched::sleep(c, mutex);
            }
            c.w32(mutex + 8, me);
            c.w32(mutex + 0xC, c.r32(mutex + 0xC) + 1);
        }),
        "OSTryLockMutex" => Hle::Native(|c| {
            let (mutex, me) = (c.r[3], sched::current_guest());
            let free = c.r32(mutex + 8) == 0 || c.r32(mutex + 8) == me;
            if free {
                c.w32(mutex + 8, me);
                c.w32(mutex + 0xC, c.r32(mutex + 0xC) + 1);
            }
            c.r[3] = free as u32;
        }),
        "OSUnlockMutex" => Hle::Native(|c| {
            let mutex = c.r[3];
            let count = c.r32(mutex + 0xC).saturating_sub(1);
            c.w32(mutex + 0xC, count);
            if count == 0 {
                c.w32(mutex + 8, 0);
                sched::wake(mutex);
                sched::preempt(c);
            }
        }),

        "OSGetTime" | "__OSGetSystemTime" => Hle::Native(|c| {
            let now = c.timebase();
            let next_due = NEXT_DUE_TIME.load(Ordering::Relaxed);
            if has_pending_events() || (next_due != 0 && now >= next_due) {
                pump(c);
            }
            let t = c.timebase();
            c.r[3] = (t >> 32) as u32;
            c.r[4] = t as u32;
        }),
        "OSGetTick" => Hle::Native(|c| {
            let now = c.timebase();
            let next_due = NEXT_DUE_TIME.load(Ordering::Relaxed);
            if has_pending_events() || (next_due != 0 && now >= next_due) {
                pump(c);
            }
            c.r[3] = c.timebase() as u32;
        }),
        "OSTicksToCalendarTime" => Hle::Native(|c| {
            let ticks = ((c.r[3] as u64) << 32 | (c.r[4] as u64)) as i64;
            let out = c.r[5];
            let per_second = TICKS_PER_SECOND as i64;
            let (seconds, fraction) = (ticks.div_euclid(per_second), ticks.rem_euclid(per_second));
            let (days, time) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
            let fields = [
                time % 60,
                time / 60 % 60,
                time / 3600,
                days % 31 + 1,
                (days / 31) % 12,
                2000 + days / 365,
                (days + 6).rem_euclid(7),
                days % 365,
                (fraction * 1000) / per_second,
                (fraction * 1_000_000) / per_second,
            ];
            for (i, val) in fields.into_iter().enumerate() {
                c.w32(out + 4 * i as u32, val as u32);
            }
        }),
        "OSGetResetCode" => Hle::Native(|c| c.r[3] = 0),
        "OSGetSoundMode" => Hle::Native(|c| c.r[3] = 1),
        "OSGetFontEncode" | "OSInitFont" | "PPCMfhid2" => Hle::Native(|c| c.r[3] = 0),
        "OSSetSoundMode" | "OSRegisterVersion" | "OSRegisterResetFunction" | "PPCMthid2"
        | "PPCMtwpar" => Hle::Native(|_| {}),
        "DCFlushRange" | "DCFlushRangeNoSync" | "DCStoreRange" | "DCStoreRangeNoSync"
        | "DCInvalidateRange" | "DCTouchRange" | "DCEnable" | "ICEnable" | "ICInvalidateRange"
        | "ICFlashInvalidate" | "ICSync" | "DBInit" | "__DBExceptionDestination" | "DBPrintf"
        | "__flush_cache" | "PPCSync" | "PPCHalt" => Hle::Native(|_| {}),
        "DCZeroRange" => Hle::Original,

        // Controller input
        "PADInit" | "PADReset" | "PADRecalibrate" => Hle::Native(|c| c.r[3] = 1),
        "PADSetSpec" | "PADClamp" => Hle::Native(|_| {}),
        "PADRead" => Hle::Native(|c| {
            let status = c.r[3];
            let mode = c.r32(0x804B_0D54);
            CURRENT_GAME_MODE.store(mode, Ordering::Relaxed);
            let btn = PAD_BUTTONS.load(Ordering::Relaxed);
            let sx = (PAD_STICK_X.load(Ordering::Relaxed) / 256).clamp(-128, 127) as i8;
            let sy = (PAD_STICK_Y.load(Ordering::Relaxed) / 256).clamp(-128, 127) as i8;
            let cx = (PAD_SUBSTICK_X.load(Ordering::Relaxed) / 256).clamp(-128, 127) as i8;
            let cy = (PAD_SUBSTICK_Y.load(Ordering::Relaxed) / 256).clamp(-128, 127) as i8;
            c.slice_mut(status, 12).fill(0);
            c.w16(status, btn as u16);
            c.w8(status + 2, sx as u8);
            c.w8(status + 3, sy as u8);
            c.w8(status + 4, cx as u8);
            c.w8(status + 5, cy as u8);
            for port in 1..4 {
                let s = c.slice_mut(status + port * 12, 12);
                s.fill(0);
                s[10] = 0xFF;
            }
            check_warp(c);
            c.r[3] = 0;
        }),
        "PADControlMotor" => Hle::Native(|_| {}),
        "PADSetSamplingCallback" => Hle::Native(|c| c.r[3] = 0),

        // Video & display
        "VIInit" | "VIFlush" | "VISetBlack" | "VIConfigurePan" => Hle::Native(|_| {}),
        "VIConfigure" => Hle::Native(|c| {
            let mode = c.r[3];
            let fb_w = c.r16(mode + 4) as u32;
            let fb_h = c.r16(mode + 8) as u32;
            if fb_w > 0 && fb_h > 0 {
                let old_w = XFB_WIDTH.swap(fb_w, Ordering::Relaxed);
                let old_h = XFB_HEIGHT.swap(fb_h, Ordering::Relaxed);
                if old_w != fb_w || old_h != fb_h {
                    log_c(&format!("[vi] VIConfigure display buffer: {}x{}", fb_w, fb_h));
                }
            }
        }),
        "VIGetTvFormat" | "VIGetDTVStatus" => Hle::Native(|c| c.r[3] = 0),
        "VIGetNextField" => Hle::Native(|c| c.r[3] = RETRACE_COUNT.load(Ordering::Relaxed) & 1),
        "VIGetRetraceCount" => Hle::Native(|c| c.r[3] = RETRACE_COUNT.load(Ordering::Relaxed)),
        "VISetNextFrameBuffer" => Hle::Native(|c| {
            let xfb = c.r[3] | 0x8000_0000;
            CURRENT_XFB.store(xfb, Ordering::Relaxed);
        }),
        "VISetPreRetraceCallback" => Hle::Native(|c| {
            let new = c.r[3];
            c.r[3] = PRE_RETRACE.swap(new, Ordering::Relaxed);
        }),
        "VISetPostRetraceCallback" => Hle::Native(|c| {
            let new = c.r[3];
            c.r[3] = POST_RETRACE.swap(new, Ordering::Relaxed);
        }),
        "VIWaitForRetrace" => Hle::Native(|c| {
            let t0 = unsafe { switch_now_ns() };
            gx_worker::push_fifo(c, 1);
            sched::sleep(c, sched::RETRACE_QUEUE);
            let dt = unsafe { switch_now_ns().saturating_sub(t0) };
            PERF_WAIT_NS.fetch_add(dt, Ordering::Relaxed);
        }),

        // Hardware GX pipeline (Core 1 streams FIFO to Core 0 GX Frontend -> Core 2 OpenGL Backend)
        "GXDrawDone" | "GXWaitDrawDone" => Hle::Native(|c| {
            timed_sync(1, c, gx_worker::sync_ram);
        }),
        "GXCallDisplayList" => Hle::Replace(call_display_list),
        "GXFlush" => Hle::Replace(flush_hook),
        "GXCopyDisp" => Hle::Replace(copy_disp_hook),
        "GXCopyTex" => Hle::Replace(copy_tex_hook),
        "GXReadDrawSync" => Hle::Native(|c| {
            timed_sync(2, c, gx_worker::sync_ram);
            c.r[3] = gx_worker::read_token() as u32;
        }),
        "GXGetFifoPtrs" => Hle::Native(|c| {
            let (read_ptr, write_ptr) = timed_sync(3, c, gx_worker::read_fifo_ptrs);
            let (read_out, write_out) = (c.r[4], c.r[5]);
            if read_out != 0 {
                c.w32(read_out, read_ptr);
            }
            if write_out != 0 {
                c.w32(write_out, write_ptr);
            }
        }),
        "GXEnableBreakPt" => Hle::Native(|c| {
            let addr = c.r[3];
            timed_sync(4, c, |c| gx_worker::enable_breakpoint(c, addr));
        }),
        "GXDisableBreakPt" => Hle::Native(|c| {
            timed_sync(4, c, gx_worker::disable_breakpoint);
        }),
        "GXSetDrawSyncCallback" => Hle::Native(|c| {
            let new = c.r[3];
            c.r[3] = DRAW_SYNC_CB.swap(new, Ordering::Relaxed);
        }),
        "GXSetBreakPtCallback" => Hle::Native(|c| {
            let new = c.r[3];
            c.r[3] = BREAK_PT_CB.swap(new, Ordering::Relaxed);
        }),
        "GXSetDrawDoneCallback" => Hle::Native(|c| {
            let new = c.r[3];
            c.r[3] = DRAW_DONE_CB.swap(new, Ordering::Relaxed);
        }),
        "GXSetChanCtrl" => Hle::Replace(arm64_native::hle_gx_set_chan_ctrl),
        "GXClearVtxDesc" => Hle::Replace(arm64_native::hle_gx_clear_vtx_desc),
        "GXSetVtxDesc" => Hle::Replace(arm64_native::hle_gx_set_vtx_desc),
        "GXSetVtxAttrFmt" => Hle::Replace(arm64_native::hle_gx_set_vtx_attr_fmt),
        "GXSetArray" => Hle::Replace(arm64_native::hle_gx_set_array),
        "__GXSetVAT" => Hle::Replace(arm64_native::hle_gx_set_vat),
        _ if name.starts_with("GX") || name.starts_with("__GX") => Hle::Original,

        // DVD file system
        "DVDInit" | "DVDReset" | "DVDResume" | "DVDSetAutoInvalidation" => Hle::Native(|_| {}),
        "DVDConvertPathToEntrynum" => Hle::Native(|c| {
            let path = guest_text(c, c.r[3]);
            let found = dvd_lookup(&path);
            if found.is_none() {
                log_c(&format!("[dvd] not found: {}", path));
            }
            c.r[3] = found.map_or(u32::MAX, |i| i as u32);
        }),
        "DVDFastOpen" => Hle::Native(|c| {
            let (entry_raw, info) = (c.r[3], c.r[4]);
            #[allow(static_mut_refs)]
            let entry = (entry_raw != u32::MAX)
                .then_some(entry_raw as usize)
                .and_then(|idx| unsafe { DVD_ENTRIES.as_ref()?.get(idx) });
            match entry {
                Some(e) if !e.is_dir => {
                    c.w32(info + 0x0C, 0);
                    c.w32(info + 0x30, e.offset);
                    c.w32(info + 0x34, e.size);
                    c.w32(info + 0x38, 0);
                    c.r[3] = 1;
                }
                _ => c.r[3] = 0,
            }
        }),
        "DVDOpen" => Hle::Native(|c| {
            let (path_str, info) = (guest_text(c, c.r[3]), c.r[4]);
            #[allow(static_mut_refs)]
            let entry = dvd_lookup(&path_str).and_then(|i| unsafe { DVD_ENTRIES.as_ref()?.get(i) });
            match entry {
                Some(e) if !e.is_dir => {
                    c.w32(info + 0x0C, 0);
                    c.w32(info + 0x30, e.offset);
                    c.w32(info + 0x34, e.size);
                    c.w32(info + 0x38, 0);
                    c.r[3] = 1;
                }
                _ => {
                    log_c(&format!("[dvd] DVDOpen not found: {}", path_str));
                    c.r[3] = 0;
                }
            }
        }),
        "DVDClose" => Hle::Native(|c| c.r[3] = 1),
        "DVDReadAsyncPrio" => Hle::Native(|c| {
            let (info, addr, length, offset, callback) =
                (c.r[3], c.r[4], c.r[5], c.r[6], c.r[7]);
            let start = c.r32(info + 0x30);
            #[allow(static_mut_refs)]
            let entry =
                dvd_find_by_offset(start).and_then(|i| unsafe { DVD_ENTRIES.as_ref()?.get(i) });
            let buf = c.slice_mut(addr, length as usize);
            buf.fill(0);
            if let Some(e) = entry {
                let avail = e.size.saturating_sub(offset).min(length);
                if avail > 0 {
                    let mut path_c = Vec::with_capacity(e.path.len() + 1);
                    path_c.extend_from_slice(e.path.as_bytes());
                    path_c.push(0);

                    let read_bytes = unsafe {
                        switch_read_file(path_c.as_ptr(), offset, avail, buf.as_mut_ptr())
                    };
                    if read_bytes < 0 {
                        log_c(&format!(
                            "[dvd] failed reading {} (offset: {}, len: {})",
                            e.path, offset, avail
                        ));
                    } else {
                        let lower = e.path.to_ascii_lowercase();
                        if lower.ends_with("sb04.ini") {
                            boot_override(&mut buf[..avail as usize]);
                        } else if lower.ends_with("mnus_us.hip") {
                            autosave_notice(&mut buf[..avail as usize]);
                        }
                    }
                }
            }
            push_event(Event::DvdDone {
                block: info,
                callback,
                result: length as i32,
            });
            c.w32(info + 0x0C, 1);
            c.w32(info + 0x20, length);
            c.w32(info + 0x38, callback);
            c.r[3] = 1;
        }),
        "DVDSeekAsyncPrio" => Hle::Native(|c| {
            let (info, callback) = (c.r[3], c.r[5]);
            push_event(Event::DvdDone {
                block: info,
                callback,
                result: 0,
            });
            c.w32(info + 0x0C, 1);
            c.r[3] = 1;
        }),
        "DVDGetCommandBlockStatus" => Hle::Native(|c| {
            let block = c.r[3];
            pump(c);
            c.r[3] = c.r32(block + 0x0C);
        }),
        "DVDGetDriveStatus" => Hle::Native(|c| {
            pump(c);
            c.r[3] = 0;
        }),
        "DVDCheckDisk" => Hle::Native(|c| {
            pump(c);
            c.r[3] = 1;
        }),
        "DVDGetCurrentDiskID" => Hle::Native(|c| c.r[3] = 0x8000_0000),
        "DVDCancel" | "DVDCancelAsync" | "DVDCancelAllAsync" => Hle::Native(|c| c.r[3] = 0),
        "DVDReadCallback" => Hle::Original,

        // Audio interface (AI)
        "AIInit" | "AISetDSPSampleRate" | "AISetStreamPlayState" | "AISetStreamVolLeft"
        | "AISetStreamVolRight" | "AIInitDMA" => Hle::Native(|_| {}),
        "AIStartDMA" => Hle::Native(|c| {
            NEXT_AUDIO_TIME.store(c.timebase() + AUDIO_INTERVAL, Ordering::Relaxed);
            AUDIO_DMA_RUNNING.store(true, Ordering::Relaxed);
            update_next_due();
        }),
        "AIStopDMA" => Hle::Native(|_| {
            AUDIO_DMA_RUNNING.store(false, Ordering::Relaxed);
            update_next_due();
        }),
        "AICheckInit" => Hle::Native(|c| c.r[3] = 1),
        "AIGetDSPSampleRate" | "AIGetStreamPlayState" | "AIGetStreamSampleRate"
        | "AIGetStreamVolLeft" | "AIGetStreamVolRight" => Hle::Native(|c| c.r[3] = 0),
        "AIRegisterDMACallback" => Hle::Native(|c| {
            let new = c.r[3];
            c.r[3] = AI_DMA_CALLBACK.swap(new, Ordering::Relaxed);
        }),

        // DSP
        "DSPInit" | "DSPCancelTask" | "DSPSendMailToDSP" | "DSPAssertTask" => Hle::Native(|_| {}),
        "DSPCheckInit" => Hle::Native(|c| c.r[3] = 1),
        "DSPCheckMailToDSP" | "DSPCheckMailFromDSP" | "DSPReadMailFromDSP" => {
            Hle::Native(|c| c.r[3] = 0)
        }
        "DSPAddTask" => Hle::Native(|c| {
            let task = c.r[3];
            DSP_TASK.store(task, Ordering::Relaxed);
            let init = c.r32(task + 0x28);
            if init != 0 {
                let saved = SavedRegs::save(c);
                c.r[3] = task;
                c.call(init);
                saved.restore(c);
            }
            c.r[3] = task;
        }),

        // Audio RAM (ARAM)
        "ARInit" | "ARGetBaseAddress" => Hle::Native(|c| c.r[3] = 0x4000),
        "ARCheckInit" => Hle::Native(|c| c.r[3] = 1),
        "ARGetDMAStatus" => Hle::Native(|c| c.r[3] = 0),
        "ARRegisterDMACallback" => Hle::Native(|c| {
            let new = c.r[3];
            c.r[3] = AR_DMA_CALLBACK.swap(new, Ordering::Relaxed);
        }),
        "ARStartDMA" => Hle::Native(|c| {
            let (kind, main, aram, len) = (c.r[3], c.r[4], c.r[5], c.r[6]);
            audio::transfer(c, kind == 0, main, aram, len);
            let cb = AR_DMA_CALLBACK.load(Ordering::Relaxed);
            if cb != 0 {
                push_event(Event::Call(cb, 0));
            }
        }),
        "ARQInit" => Hle::Native(|_| {}),
        "ARQPostRequest" => Hle::Native(|c| {
            let (request, kind, source, dest, len, callback) =
                (c.r[3], c.r[5], c.r[7], c.r[8], c.r[9], c.r[10]);
            for (i, val) in
                [0, c.r[4], kind, c.r[6], source, dest, len, callback].into_iter().enumerate()
            {
                c.w32(request + 4 * i as u32, val);
            }
            if kind == 0 {
                audio::transfer(c, true, source, dest, len);
            } else {
                audio::transfer(c, false, dest, source, len);
            }
            if callback != 0 {
                push_event(Event::Call(callback, request));
            }
        }),

        // AX mixer & effects: stub dead DSP aux reverb and command list processing
        "__AXProcessAux" | "AXFXReverbHiCallback" | "AXFXReverbHiCallbackDpl2" | "__AXNextFrame"
        | "__AXPrintStudio" => Hle::Native(|_| {}),
        _ if ["AX", "__AX", "MIX", "__MIX"].iter().any(|p| name.starts_with(p)) => Hle::Original,

        // Memory card
        _ if name.starts_with("CARD") || name.starts_with("__CARD") => card::policy(name),

        // Serial Interface & Expansion Interface
        _ if name.starts_with("SI") || name.starts_with("EXI") => Hle::Native(|c| c.r[3] = 0),

        // RenderWare frame queue serialization & zero-CPU vsync sleep
        "_rwDlRasterShowRaster" => Hle::Before(before_show_raster),
        "RwGameCubeGetXFBs" => Hle::Replace(replace_get_xfbs),

        // Skip Bink FMV cutscenes cleanly when any button is pressed (from crates/tssm/src/menu.rs::before_movie)
        "iFMVPlay__FPCcUifUibbi" | "iFMVPlay__FPCcUifUibb" => Hle::Before(|c| {
            if c.r[4] != 0 {
                c.r[4] = u32::MAX;
            }
        }),

        "zSceneInit__FUii" => Hle::Before(|c| {
            static FIRST: AtomicBool = AtomicBool::new(true);
            let target = TARGET_SCENE.load(Ordering::Relaxed);
            if FIRST.swap(false, Ordering::Relaxed) && target != 0 {
                c.r[3] = target;
            }
            BOOT_INIT_DONE.store(true, Ordering::Relaxed);
            let scene = c.r[3];
            TARGET_SCENE.store(scene, Ordering::Relaxed);
            gl_renderer::CURRENT_SCENE_ID.store(scene, Ordering::Relaxed);
            let id = scene.to_be_bytes();
            let s: String = id
                .iter()
                .map(|&b| (b as char).escape_default().to_string())
                .collect();
            log_c(&format!("[scene] zSceneInit({} / {:#010x})", s, scene));
            gx_worker::sync_full(c, Some(gx_worker::CtrlCmd::ResetCache));
        }),

        // Native 16:9 widescreen camera & HUD edge anchoring
        "RwCameraSetViewWindow" => Hle::Before(aspect::before_set_view_window),
        "RwCameraBeginUpdate" => Hle::Before(aspect::before_begin_update),
        "render__Q24xhud11text_widgetFv"
        | "render__Q24xhud12image_widgetFv"
        | "render__Q24xhud12model_widgetFv"
        | "render__Q24xhud17font_meter_widgetFv"
        | "render__Q24xhud17unit_meter_widgetFv" => Hle::Before(|c| {
            gx_worker::mark_hud(c, true);
        }),
        "zGameModeGet__Fv"
        | "zSceneRender__Fv"
        | "zSceneRenderPreFX__Fv"
        | "zSceneRenderPostFX__Fv"
        | "_rxGCDefaultRenderCallback" => Hle::Before(|c| {
            gx_worker::mark_hud(c, false);
        }),

        // Fixed 60 Hz car physics step + smooth matrix interpolation
        "CarUpdate__4zCarFffbbb" => Hle::Replace(car::car_update),

        "zCameraUpdate__FP7xCameraf"
        | "xCameraMove__FP7xCameraUiffffff"
        | "xCameraLookYPR__FP7xCameraUiffffff"
        | "zCameraTweakGlobal_Add__FUiffff"
        | "zUIMgrUpdate__Ff"
        | "DoRender__7zUITextCFv"
        | "DoRender__8zUIModelCFv"
        | "DoRender__8zUIImageCFv" => Hle::Original,

        // The save and load menus go straight to the three save slots.
        "zSaveLoad_SaveLoop__Fv" | "zSaveLoad_LoadLoop__Fv" => Hle::Before(card::before_menu),
        "zSaveLoad_Tick__Fb" => Hle::Replace(card::pick_slot_a),
        "iSGTgtHaveRoomStartup__FP13st_ISGSESSIONiiPCcPCcPiPiPi" => Hle::Before(card::before_room_check),

        "BinkDoFrame"
        | "__start"
        | "__init_registers"
        | "__init_data"
        | "__init_hardware"
        | "__init_user"
        | "__fini_cpp" => Hle::Original,

        "exit" | "_ExitProcess" => Hle::Native(|_| log_c("[switch] Guest called exit()")),
        "InitMetroTRK" | "InitMetroTRK_BBA" | "EnableMetroTRKInterrupts" => Hle::Native(|_| {}),

        // FMOD GameCube ARAM flag hooks
        "FMUSIC_LoadSongEx" => Hle::Before(arm64_native::fmusic_load_song_ex_before),
        "FMUSIC_LoadInternal" => Hle::Before(arm64_native::fmusic_load_internal_before),
        "FSOUND_Sample_LoadEx" => Hle::Before(arm64_native::fsound_sample_load_ex_before),

        // Native ARM64 FMOD memory pool allocator
        "FSOUND_Memory_Pool_Alloc" => Hle::Replace(arm64_native::hle_fsound_memory_pool_alloc),
        "FSOUND_Memory_Pool_Free" => Hle::Replace(arm64_native::hle_fsound_memory_pool_free),
        "FSOUND_Memory_Pool_Calloc" => Hle::Replace(arm64_native::hle_fsound_memory_pool_calloc),

        // Native de-interleaving routines
        "split16" => Hle::Replace(arm64_native::hle_split16),
        "split8" => Hle::Replace(arm64_native::hle_split8),

        // Native ARM64 overlap-safe Libc memory & string routines
        "memcpy" | "TRK_memcpy" | "TRK_ppc_memcpy" | "memmove" | "F_memmove" => {
            Hle::Native(arm64_native::hle_memmove)
        }
        "memset" | "TRK_memset" | "__fill_mem" => Hle::Native(arm64_native::hle_memset),
        "memcmp" => Hle::Native(arm64_native::hle_memcmp),
        "strlen" | "F_strlen" => Hle::Native(arm64_native::hle_strlen),
        "strcpy" | "F_strcpy" => Hle::Native(arm64_native::hle_strcpy),
        "strcmp" => Hle::Native(arm64_native::hle_strcmp),

        // Native ARM64 64-bit PPCEABI integer division/modulo (single hardware udiv/sdiv)
        "__div2u" => Hle::Replace(arm64_native::hle_div2u),
        "__div2i" => Hle::Replace(arm64_native::hle_div2i),
        "__mod2u" => Hle::Replace(arm64_native::hle_mod2u),
        "__mod2i" => Hle::Replace(arm64_native::hle_mod2i),

        // Math functions (native ARM64 / IEEE-754 hardware acceleration)
        "sqrt" | "__ieee754_sqrt" => Hle::Replace(arm64_native::hle_sqrt),
        "xsqrt__Ff" => Hle::Replace(arm64_native::hle_xsqrt),
        "_rwSqrt" => Hle::Replace(arm64_native::hle_rw_sqrt),
        "_rwInvSqrt" => Hle::Replace(arm64_native::hle_rw_inv_sqrt),
        "ceil" => Hle::Replace(arm64_native::hle_ceil),
        "floor" => Hle::Replace(arm64_native::hle_floor),
        "sinf" | "isin__Ff" => Hle::Replace(arm64_native::hle_sinf),
        "cosf" | "icos__Ff" => Hle::Replace(arm64_native::hle_cosf),
        "powf" => Hle::Replace(arm64_native::hle_powf),
        "sin" => Hle::Replace(arm64_native::hle_sin),
        "cos" => Hle::Replace(arm64_native::hle_cos),
        "tan" => Hle::Replace(arm64_native::hle_tan),
        "atan" => Hle::Replace(arm64_native::hle_atan),
        "atan2" => Hle::Replace(arm64_native::hle_atan2),
        "acos" => Hle::Replace(arm64_native::hle_acos),
        "asin" => Hle::Replace(arm64_native::hle_asin),
        "fmod" | "__ieee754_fmod" => Hle::Replace(arm64_native::hle_fmod),

        // Matrix identity/scale & scene culling/collision acceleration
        "PSMTXIdentity" => Hle::Replace(arm64_native::hle_psmtx_identity),
        "PSMTXScale" => Hle::Replace(arm64_native::hle_psmtx_scale),
        "RwCameraFrustumTestSphere" => Hle::Replace(arm64_native::hle_rw_camera_frustum_test_sphere),
        "RtIntersectionSphereTriangle" => Hle::Replace(arm64_native::hle_rt_intersection_sphere_triangle),
        "RtIntersectionBBoxTriangle" => Hle::Replace(arm64_native::hle_rt_intersection_bbox_triangle),
        "xQuickCullIsects__FPC7xQCDataPC7xQCData" => Hle::Replace(arm64_native::hle_xquick_cull_isects),
        "properSphereIsectTri__FPC5xVec3fP5xVec3PfP19RpCollisionTriangle" => Hle::Replace(arm64_native::hle_proper_sphere_isect_tri),
        "PointWithinTriangle__FP5xVec3PP5xVec3P5xVec3" => Hle::Replace(arm64_native::hle_point_within_triangle),
        "RwBBoxCalculate" => Hle::Replace(arm64_native::hle_rw_bbox_calculate),
        "_rwGCNDisplayListGetStride" => Hle::Replace(arm64_native::hle_rw_gcn_display_list_get_stride),
        "xBoxForTriangle__FR4xBoxRC5xVec3RC5xVec3RC5xVec3" => {
            Hle::Replace(arm64_native::hle_xbox_for_triangle)
        }
        "xBoxInitBoundOBB__FP4xBoxPC4xBoxPC7xMat4x3" => {
            Hle::Replace(arm64_native::hle_xbox_init_bound_obb)
        }

        // RenderWare matrix, vector transforms, and concatenation acceleration
        "PSMTXConcat"
        | "MatrixMultiply"
        | "RwMatrixMultiply"
        | "RwMatrixUpdate"
        | "xMat4x3Mul__FP7xMat4x3PC7xMat4x3PC7xMat4x3"
        | "xQuatToMat__FPC5xQuatP7xMat3x3"
        | "xMat3x3LMulVec__FP5xVec3PC7xMat3x3PC5xVec3"
        | "xMat3x3Tolocal__FP5xVec3PC7xMat3x3PC5xVec3"
        | "xMat3x3Mul__FP7xMat3x3PC7xMat3x3PC7xMat3x3"
        | "RwV3dLength"
        | "RwV3dTransformPoint"
        | "RwV3dTransformPoints"
        | "RwV3dTransformVector"
        | "RwV3dTransformVectors" => Hle::Original,

        // Skeletal skinning vertex updates (use 100% faithful original GameCube recompiled code)
        "_rwDlSkinUpdate2WeightsP" => skin_verify::policy(0),
        "_rwDlSkinUpdate2WeightsPN" => skin_verify::policy(1),
        "_rwDlSkinUpdate3WeightsP" => skin_verify::policy(2),
        "_rwDlSkinUpdate3WeightsPN" => skin_verify::policy(3),
        "_rwDlSkinUpdate4WeightsP" => skin_verify::policy(4),
        "_rwDlSkinUpdate4WeightsPN" => skin_verify::policy(5),
        "_rpSkinMatrixBlendUpdateASM" | "_rwDlSetRenderState" => Hle::Original,
        "_rxGCResEntryWaitDone" => Hle::Native(|_| {}),
        "_rwDlTokenQueryDone" => Hle::Native(|c| c.r[3] = 1),

        _ => Hle::Native(|c| c.r[3] = 0),
    }
}

#[no_mangle]
pub extern "C" fn tssm_switch_free_ctx(ctx: *mut c_void) {
    if !ctx.is_null() {
        unsafe {
            drop(Box::from_raw(ctx as *mut Ctx));
        }
    }
}

#[no_mangle]
pub extern "C" fn tssm_switch_init() -> i32 {
    log_c("[switch] Initializing DVD filesystem & 64-voice DSP/AX audio...");
    init_dvd_entries();
    audio::init();
    gl_renderer::CURRENT_SCENE_ID.store(TARGET_SCENE.load(Ordering::Relaxed), Ordering::Relaxed);
    let num_funcs = game::FUNCS.len();
    log_c(&format!("[switch] Recompiled symbols: {}", num_funcs));
    num_funcs as i32
}

#[no_mangle]
pub extern "C" fn tssm_switch_guest_main() {
    unsafe {
        switch_set_thread_index(0);
    }

    skin_verify::init();
    let mut c = Ctx::new(game::FUNCS, game::HLE_NAMES, switch_policy);
    c.survey = false;
    c.poll_hook = Some(guest_poll_hook);
    if runtime::PROFILE_FN {
        unsafe { switch_start_prof_sampler(prof_tick, 200_000) };
    }
    sched::init(0x817E_0000);
    GUEST_MEM_PTR.store(c.mem as usize, Ordering::Relaxed);
    log_c("[switch] Starting Core 0 GX Frontend & Core 2 OpenGL 4.3 hardware renderer...");
    gx_worker::init();
    NEXT_RETRACE_TIME.store(c.timebase() + RETRACE_INTERVAL, Ordering::Relaxed);
    NEXT_AUDIO_TIME.store(c.timebase() + AUDIO_INTERVAL, Ordering::Relaxed);
    PERF_LAST_NS.store(unsafe { switch_now_ns() }, Ordering::Relaxed);

    // 1. Initialize GameCube low memory OS constants
    c.w32(0x8000_0028, runtime::mem::RAM_SIZE);
    c.w32(0x8000_002C, 3);
    c.w32(0x8000_00CC, 0);
    c.w32(0x8000_00F0, runtime::mem::RAM_SIZE);
    c.w32(0x8000_00F4, 486_000_000);
    c.w32(0x8000_00F8, 162_000_000);
    c.w32(0x8000_00FC, 486_000_000);

    // 2. Copy disc boot header
    let boot_len = BOOT_BIN.len().min(0x440);
    c.slice_mut(0x8000_0000, boot_len)
        .copy_from_slice(&BOOT_BIN[..boot_len]);

    // 3. Load DOL executable image
    log_c("[switch] Loading main.dol into 24MB guest memory...");
    c.load_dol(MAIN_DOL);

    // Re-verify OS constants
    c.w32(0x8000_0028, runtime::mem::RAM_SIZE);
    c.w32(0x8000_002C, 3);
    c.w32(0x8000_00CC, 0);
    c.w32(0x8000_00F0, runtime::mem::RAM_SIZE);
    c.w32(0x8000_00F4, 486_000_000);
    c.w32(0x8000_00F8, 162_000_000);
    c.w32(0x8000_00FC, 486_000_000);

    // 4. Setup PowerPC registers
    c.r[1] = linker_sym("_stack_addr");
    c.r[2] = linker_sym("_SDA2_BASE_");
    c.r[13] = linker_sym("_SDA_BASE_");

    log_c("[switch] Jumping to game main(&mut c)...");
    if let Some(main_fn) = c.func_by_name("main") {
        main_fn(&mut c);
        log_c("[switch] Game main returned!");
    } else {
        log_c("[switch] Error: main symbol not found in game!");
    }

    EXIT_REQUESTED.store(true, Ordering::Relaxed);
    unsafe {
        switch_stop_gx_worker();
        switch_gl_shutdown();
    }
}

#[no_mangle]
pub extern "C" fn tssm_switch_request_exit() {
    EXIT_REQUESTED.store(true, Ordering::Relaxed);
    sched::wake_all();
    unsafe {
        switch_gx_lock();
        switch_gx_wake_work();
        switch_gx_wake_done();
        switch_gx_unlock();
    }
}

#[no_mangle]
pub extern "C" fn tssm_switch_set_docked(docked: i32) {
    IS_DOCKED.store(docked != 0, Ordering::Relaxed);
}

#[no_mangle]
pub extern "C" fn tssm_switch_set_pad(buttons: u32) {
    PAD_BUTTONS.store(buttons, Ordering::Relaxed);
}

#[no_mangle]
pub extern "C" fn tssm_switch_set_sticks(lx: i32, ly: i32, rx: i32, ry: i32) {
    PAD_STICK_X.store(lx, Ordering::Relaxed);
    PAD_STICK_Y.store(ly, Ordering::Relaxed);
    PAD_SUBSTICK_X.store(rx, Ordering::Relaxed);
    PAD_SUBSTICK_Y.store(ry, Ordering::Relaxed);
}

#[no_mangle]
pub extern "C" fn tssm_switch_get_xfb() -> u32 {
    CURRENT_XFB.load(Ordering::Relaxed)
}

#[no_mangle]
pub extern "C" fn tssm_switch_get_frame_count() -> u32 {
    FRAME_COUNT.load(Ordering::Relaxed)
}

#[no_mangle]
pub extern "C" fn tssm_switch_get_guest_mem() -> *mut u8 {
    GUEST_MEM_PTR.load(Ordering::Relaxed) as *mut u8
}

#[no_mangle]
pub extern "C" fn tssm_switch_get_xfb_width() -> u32 {
    XFB_WIDTH.load(Ordering::Relaxed)
}

#[no_mangle]
pub extern "C" fn tssm_switch_get_xfb_height() -> u32 {
    XFB_HEIGHT.load(Ordering::Relaxed)
}

#[no_mangle]
pub extern "C" fn tssm_switch_drain_audio(out: *mut i16, max_frames: usize) -> usize {
    unsafe { audio::drain(out, max_frames) }
}

#[no_mangle]
pub extern "C" fn tssm_switch_set_boot_scene(scene: *const u8) {
    if !scene.is_null() {
        let bytes = unsafe { core::slice::from_raw_parts(scene, 4) };
        if let Ok(arr) = bytes.try_into() {
            let val = u32::from_be_bytes(arr);
            TARGET_SCENE.store(val, Ordering::Relaxed);
            gl_renderer::CURRENT_SCENE_ID.store(val, Ordering::Relaxed);
        }
    }
}

#[no_mangle]
pub extern "C" fn tssm_switch_warp(scene: *const u8) {
    if !scene.is_null() {
        let bytes = unsafe { core::slice::from_raw_parts(scene, 4) };
        if let Ok(arr) = bytes.try_into() {
            let val = u32::from_be_bytes(arr);
            WARP_TARGET.store(val, Ordering::Relaxed);
            TARGET_SCENE.store(val, Ordering::Relaxed);
            gl_renderer::CURRENT_SCENE_ID.store(val, Ordering::Relaxed);
        }
    }
}

#[no_mangle]
pub extern "C" fn tssm_switch_get_fps_enabled() -> i32 {
    gl_renderer::FPS_ENABLED.load(Ordering::Relaxed) as i32
}

#[no_mangle]
pub extern "C" fn tssm_switch_set_fps_enabled(val: i32) {
    gl_renderer::FPS_ENABLED.store(val != 0, Ordering::Relaxed);
}

#[no_mangle]
pub extern "C" fn tssm_switch_get_current_scene() -> u32 {
    TARGET_SCENE.load(Ordering::Relaxed)
}

#[no_mangle]
pub extern "C" fn tssm_switch_set_scale_override(scale: u32) {
    gl_renderer::SCALE_OVERRIDE.store(scale, Ordering::Relaxed);
}

#[no_mangle]
pub extern "C" fn tssm_switch_get_game_mode() -> u32 {
    CURRENT_GAME_MODE.load(Ordering::Relaxed)
}

#[no_mangle]
pub extern "C" fn tssm_switch_is_in_gameplay() -> i32 {
    if CURRENT_GAME_MODE.load(Ordering::Relaxed) == 12 {
        1
    } else {
        0
    }
}
