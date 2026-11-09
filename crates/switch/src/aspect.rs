//! Native 16:9 widescreen aspect ratio support for Nintendo Switch (`#![no_std]` + `alloc`).
//! Widens 3D world cameras (`RwCameraSetViewWindow` / `RwCameraBeginUpdate`) to fill the Switch's
//! 16:9 display while preserving unstretched 4:3 UI/menus and anchoring HUD widgets to screen edges.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use hashbrown::HashMap;
use runtime::Ctx;

const NATIVE: f32 = 4.0 / 3.0;
const MAX_WIDEN: f32 = 2.0;
/// Reserved inside Switch host scratch region (`0x817F_8000 + 0x800`).
const VIEW_WINDOW_SCRATCH: u32 = 0x817F_8800;

/// Switch display is always 16:9 (1280x720 Handheld, 1920x1080 Docked).
static WINDOW: AtomicU32 = AtomicU32::new((16.0f32 / 9.0f32).to_bits());
static LOCK: AtomicBool = AtomicBool::new(false);
static mut CAMERAS: Option<HashMap<u32, (f32, f32, f32)>> = None;

#[inline]
#[allow(static_mut_refs)]
fn with_cameras<R>(f: impl FnOnce(&mut HashMap<u32, (f32, f32, f32)>) -> R) -> R {
    while LOCK.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        core::hint::spin_loop();
    }
    let res = unsafe {
        let map = CAMERAS.get_or_insert_with(HashMap::new);
        f(map)
    };
    LOCK.store(false, Ordering::Release);
    res
}

pub fn set_window(width: u32, height: u32) {
    if width > 0 && height > 0 {
        WINDOW.store((width as f32 / height as f32).to_bits(), Ordering::Relaxed);
    }
}

#[inline]
pub fn widen() -> f32 {
    let window = f32::from_bits(WINDOW.load(Ordering::Relaxed));
    let ratio = if window > 0.0 { window } else { 16.0 / 9.0 };
    (ratio / NATIVE).clamp(1.0, MAX_WIDEN)
}

#[inline]
pub fn picture() -> f32 {
    NATIVE * widen()
}

pub fn before_set_view_window(c: &mut Ctx) {
    let (x, y) = (f32::from_bits(c.r32(c.r[4])), f32::from_bits(c.r32(c.r[4] + 4)));
    let w = widen();
    let fills_screen = y > 0.0 && (x / y >= 1.05 && x / y <= 1.45);
    crate::log_c(&alloc::format!(
        "[aspect] view window {x} x {y} (ratio {:.3}) fills={fills_screen} widen={w:.3}",
        if y > 0.0 { x / y } else { 0.0 }
    ));
    with_cameras(|cameras| {
        if fills_screen {
            cameras.insert(c.r[3], (x, y, w));
        } else {
            cameras.remove(&c.r[3]);
        }
    });
    if !fills_screen || w == 1.0 {
        return;
    }
    let copy = VIEW_WINDOW_SCRATCH;
    c.w32(copy, (x * w).to_bits());
    c.w32(copy + 4, y.to_bits());
    c.r[4] = copy;
}

pub fn before_begin_update(c: &mut Ctx) {
    let camera = c.r[3];
    let w = widen();
    let stale = with_cameras(|cameras| cameras.get(&camera).copied().filter(|entry| entry.2 != w));
    let Some((x, y, _)) = stale else { return };
    let saved = (c.r, c.f, c.ps1, c.cr, c.lr, c.ctr, c.ca, c.xer_rest, c.fpscr);
    let window = VIEW_WINDOW_SCRATCH + 8;
    c.w32(window, x.to_bits());
    c.w32(window + 4, y.to_bits());
    c.r[4] = window;
    if let Some(set_view_window) = c.func_by_name("RwCameraSetViewWindow") {
        set_view_window(c);
    }
    (c.r, c.f, c.ps1, c.cr, c.lr, c.ctr, c.ca, c.xer_rest, c.fpscr) = saved;
}
