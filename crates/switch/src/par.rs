//! Two-way split helper: the GX worker (Core 0) hands half of a big job to a helper thread on
//! Core 2 and runs the other half itself; if the helper has not started by then, the caller
//! simply runs both (see `switch_par_*` in tools/switch/main.c).

use core::ffi::c_void;

/// Below this many vertices the hand-off costs more than it saves.
pub const MIN_PARALLEL_VERTS: usize = 768;

extern "C" {
    fn switch_par_post(f: extern "C" fn(*mut c_void), ctx: *mut c_void) -> u32;
    fn switch_par_try_steal() -> u32;
    fn switch_par_stolen_done();
    fn switch_par_wait();
}

extern "C" fn tramp(ctx: *mut c_void) {
    let job = unsafe { &mut *(ctx as *mut &mut dyn FnMut()) };
    job();
}

/// Runs `a` here and `b` on the helper (or here if the helper is unavailable or busy).
pub fn run2(a: impl FnOnce(), b: &mut dyn FnMut()) {
    let mut bref: &mut dyn FnMut() = b;
    let ctx = &mut bref as *mut &mut dyn FnMut() as *mut c_void;
    if unsafe { switch_par_post(tramp, ctx) } == 0 {
        a();
        tramp(ctx);
        return;
    }
    a();
    if unsafe { switch_par_try_steal() } != 0 {
        tramp(ctx);
        unsafe { switch_par_stolen_done() };
    } else {
        unsafe { switch_par_wait() };
    }
}
