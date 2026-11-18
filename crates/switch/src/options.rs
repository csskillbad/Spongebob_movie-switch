//! The port's own settings (FPS Counter, Render Resolution, Field of View) in the game's
//! settings screen (Options, Settings), below its own three: Rumble Feature, Music Volume and
//! SFX Volume. Modeled on the desktop port's `options.rs`, gamepad only.
//!
//! The extra rows are not widgets of the game's: once the last of the game's rows has drawn
//! itself it is drawn again, lower down, with the port's names (`draw`). Down on SFX Volume
//! (which the game ignores) takes the focus off it and gives it to the first port row. While a
//! port row has the focus the game sees no arrows, stick or A (`filter_pad`): Up/Down move
//! between rows (Up from the first gives SFX Volume the focus back) and Left/Right change the
//! value. Every setting has eleven places, one per bar of the game's volume sliders. Values are
//! saved to `sdmc:/switch/tssm/settings.bin` as soon as they change.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use runtime::Ctx;

use crate::gl_renderer::{FPS_ENABLED, SCALE_OVERRIDE};

extern "C" {
    fn switch_cfg_load(buf: *mut u8, cap: u32) -> i32;
    fn switch_cfg_store(buf: *const u8, len: u32) -> i32;
}

const UP: u32 = 0x0008;
const DOWN: u32 = 0x0004;
const LEFT: u32 = 0x0001;
const RIGHT: u32 = 0x0002;
const A: u32 = 0x0100;

const WIDGETS: u32 = 0x8040_72C0;
const WIDGET_COUNT: u32 = 0xFFFF_93D0;
const TEXT_WIDGET: u8 = 0x50;
const IMAGE_WIDGET: u8 = 0x51;
const WIDGET_TEXT: u32 = 0x70;
const TEXT_SHADOW: u32 = 0x6F;
const SEND_EVENT: u32 = 0x801C_9550;
const RENDER_TEXT: u32 = 0x8018_53DC;
const RENDER_IMAGE: u32 = 0x8018_6D6C;
const FOCUS_ON_SELECT: u32 = 94;
const FOCUS_OFF_UNSELECT: u32 = 95;
const DIMMED: u8 = 100;

const SETTING_COUNT: usize = 3;
const NAMES: [&str; SETTING_COUNT] = ["FPS Counter", "Resolution", "Field of View"];
const FOV_VALUES: [u32; 11] = [60, 65, 70, 75, 80, 85, 90, 95, 100, 105, 110];
/// Bar place of each resolution level (Auto, Low, 720p, 1080p).
const RES_PLACES: [usize; 4] = [0, 3, 7, 10];
const RES_NAMES: [&str; 4] = ["Auto", "Low", "720p", "1080p"];

const GAME_SETTINGS: [&str; 3] = ["rumble feature", "music volume", "sfx volume"];
const ROWS_LEFT: f32 = 80.0;
const ROW_TOPS: [f32; 3] = [190.0, 230.0, 270.0];
const LAYOUT_TOPS: [f32; 6] = [158.0, 192.0, 226.0, 260.0, 294.0, 328.0];
const BOUNCE_PERIOD: u32 = 30;
const BOUNCE_UP: u32 = 18;
const BOUNCE_HEIGHT: f32 = 5.0;
const TITLE: [f32; 4] = [190.0, 100.0, 260.0, 40.0];
const WORDS_TOP: f32 = 210.0;
const WORD_LEFT: f32 = 295.0;
const WORD_RIGHT: f32 = 455.0;
const RANGE_TOP: f32 = 238.0;
const VALUE_TOP: f32 = 188.0;
const BARS_CENTER_X: f32 = 415.0;
const TOGGLE_TOP: f32 = 228.0;
const DARK_BARS: (u32, u32) = (0x5F72_9DFE, 0xD7A6_D9AD);
const LIT_BARS: (u32, u32) = (0x5AF6_E971, 0x8C59_7586);

/// Text copies drawn by the port, in the host scratch block.
const TEXT_SCRATCH: u32 = 0x817F_0000;
const TEXT_SLOT: u32 = 0x40;
const TEXT_SLOTS: u32 = 8;
const REPEAT_AFTER: u32 = 24;
const REPEAT_EVERY: u32 = 5;
const STICK_PUSH: i8 = 40;
const ON_SHOW_TICKS: u32 = 15;

fn bar_id(bars: (u32, u32), place: usize) -> u32 {
    if place < 10 { bars.0 + place as u32 } else { bars.1 }
}

struct State {
    /// Pad polls (`tick`) when the settings screen was last seen.
    seen: u32,
    tick: u32,
    rows: [u32; 3],
    focused: [bool; 3],
    focus: Option<usize>,
    /// Events to send when the game next updates its menus.
    actions: Vec<(u32, u32)>,
    held: u8,
    repeat_at: u32,
    swallow: bool,
    texts: u32,
    offset: (f32, f32),
    focus_since: u32,
    loaded: bool,
}

// SAFETY: only the guest thread touches this.
static mut STATE: State = State {
    seen: 0,
    tick: 1,
    rows: [0; 3],
    focused: [false; 3],
    focus: None,
    actions: Vec::new(),
    held: 0,
    repeat_at: 0,
    swallow: false,
    texts: 0,
    offset: (0.0, 0.0),
    focus_since: 0,
    loaded: false,
};
static RENDERING: AtomicBool = AtomicBool::new(false);

#[allow(static_mut_refs)]
fn state() -> &'static mut State {
    unsafe { &mut STATE }
}

fn on_show(s: &State) -> bool {
    s.seen != 0 && s.tick.wrapping_sub(s.seen) < ON_SHOW_TICKS
}

fn slid_away(s: &State) -> bool {
    s.offset.1.abs() > 100.0
}

/// Applies the saved settings (once).
pub fn ensure_loaded() {
    let s = state();
    if s.loaded {
        return;
    }
    s.loaded = true;
    let mut buf = [0u8; 8];
    // SAFETY: buf is valid for 8 bytes.
    let len = unsafe { switch_cfg_load(buf.as_mut_ptr(), 8) };
    if len >= 5 && buf[0] == b'S' && buf[1] == b'1' {
        FPS_ENABLED.store(buf[2] != 0, Ordering::Relaxed);
        SCALE_OVERRIDE.store((buf[3] as u32).min(3), Ordering::Relaxed);
        crate::aspect::set_fov(buf[4] as f32);
    }
}

fn save() {
    let buf = [
        b'S',
        b'1',
        FPS_ENABLED.load(Ordering::Relaxed) as u8,
        SCALE_OVERRIDE.load(Ordering::Relaxed).min(3) as u8,
        crate::aspect::fov().clamp(30.0, 150.0) as u8,
    ];
    // SAFETY: buf is valid for its length.
    unsafe { switch_cfg_store(buf.as_ptr(), buf.len() as u32) };
}

fn res_level() -> usize {
    SCALE_OVERRIDE.load(Ordering::Relaxed).min(3) as usize
}

fn fov_index() -> usize {
    let now = crate::aspect::fov();
    (0..11)
        .min_by(|&a, &b| {
            let (da, db) = ((FOV_VALUES[a] as f32 - now).abs(), (FOV_VALUES[b] as f32 - now).abs());
            da.partial_cmp(&db).unwrap_or(core::cmp::Ordering::Equal)
        })
        .unwrap_or(3)
}

fn bar_place(setting: usize) -> usize {
    match setting {
        0 => if FPS_ENABLED.load(Ordering::Relaxed) { 10 } else { 0 },
        1 => RES_PLACES[res_level()],
        _ => fov_index(),
    }
}

fn step(setting: usize, delta: i32) {
    match setting {
        0 => {
            let next = if delta > 0 { true } else if delta < 0 { false } else { return };
            if FPS_ENABLED.swap(next, Ordering::Relaxed) == next {
                return;
            }
        }
        1 => {
            let cur = res_level() as i32;
            let next = (cur + delta).clamp(0, 3) as usize;
            if next == cur as usize {
                return;
            }
            SCALE_OVERRIDE.store(next as u32, Ordering::Relaxed);
        }
        _ => {
            let cur = fov_index() as i32;
            let next = (cur + delta).clamp(0, 10) as usize;
            if next == cur as usize {
                return;
            }
            crate::aspect::set_fov(FOV_VALUES[next] as f32);
        }
    }
    save();
}

fn call(c: &mut Ctx, function: u32, arguments: &[u32]) {
    let saved = (c.r, c.f, c.ps1, c.cr, c.lr, c.ctr, c.ca, c.xer_rest, c.fpscr);
    for (i, &argument) in arguments.iter().enumerate() {
        c.r[3 + i] = argument;
    }
    c.call(function);
    (c.r, c.f, c.ps1, c.cr, c.lr, c.ctr, c.ca, c.xer_rest, c.fpscr) = saved;
}

fn render_text_unhooked(c: &mut Ctx, widget: u32) {
    RENDERING.store(true, Ordering::Relaxed);
    call(c, RENDER_TEXT, &[widget]);
    RENDERING.store(false, Ordering::Relaxed);
}

fn send(c: &mut Ctx, to: u32, event: u32) {
    let saved = (c.r, c.f, c.ps1, c.cr, c.lr, c.ctr, c.ca, c.xer_rest, c.fpscr);
    (c.r[3], c.r[4], c.r[5], c.r[6]) = (0, 0, to, event);
    (c.r[7], c.r[8], c.r[9], c.r[10]) = (0, 0, 0, 1);
    c.call(SEND_EVENT);
    (c.r, c.f, c.ps1, c.cr, c.lr, c.ctr, c.ca, c.xer_rest, c.fpscr) = saved;
}

fn characters(c: &Ctx, addr: u32) -> String {
    let mut out = String::new();
    if !(0x8000_0000..0x8180_0000 - 64).contains(&addr) {
        return out;
    }
    for i in 0..64 {
        let b = c.r8(addr + i);
        if b == 0 {
            break;
        }
        out.push(b.to_ascii_lowercase() as char);
    }
    out
}

struct Widget {
    addr: u32,
    focused: bool,
    rect: [f32; 4],
}

/// The widgets of a kind that are enabled, visible and not fully transparent.
fn on_show_widgets(c: &Ctx, kind: u8) -> Vec<Widget> {
    let count = c.r32(c.r[13].wrapping_add(WIDGET_COUNT)).min(1024);
    let f = |addr: u32| f32::from_bits(c.r32(addr));
    (0..count)
        .map(|i| c.r32(WIDGETS + i * 4))
        .filter(|&ui| (0x8000_0000..0x8180_0000).contains(&ui) && c.r8(ui + 4) == kind)
        .filter(|&ui| c.r16(ui + 6) & 1 != 0 && c.r8(ui + 0x54) != 0 && c.r8(ui + 0x27) != 0)
        .map(|ui| Widget { addr: ui, focused: c.r8(ui + 0x55) != 0, rect: [f(ui + 0x14), f(ui + 0x18), f(ui + 0x1C), f(ui + 0x20)] })
        .collect()
}

/// All of a kind of the game's widgets, shown or not: address, id and place.
fn all_widgets(c: &Ctx, kind: u8) -> Vec<(u32, u32, [f32; 4])> {
    let count = c.r32(c.r[13].wrapping_add(WIDGET_COUNT)).min(1024);
    let f = |addr: u32| f32::from_bits(c.r32(addr));
    (0..count)
        .map(|i| c.r32(WIDGETS + i * 4))
        .filter(|&ui| (0x8000_0000..0x8180_0000).contains(&ui) && c.r8(ui + 4) == kind)
        .map(|ui| (ui, c.r32(ui), [f(ui + 0x14), f(ui + 0x18), f(ui + 0x1C), f(ui + 0x20)]))
        .collect()
}

/// Runs in place of zUIText::DoRender(): draws one of the game's settings where `row_shift`
/// puts it, and after the last of them the port's. False for any other widget.
pub fn render_row(c: &mut Ctx) -> bool {
    if RENDERING.load(Ordering::Relaxed) {
        return false;
    }
    let widget = c.r[3];
    let (shift, last) = {
        let s = state();
        match s.rows.iter().position(|&row| row == widget) {
            Some(i) if on_show(s) => (LAYOUT_TOPS[i] - ROW_TOPS[i], i == 2),
            _ => return false,
        }
    };
    let y = c.r32(widget + 0x18);
    c.w32(widget + 0x18, (f32::from_bits(y) + shift).to_bits());
    render_text_unhooked(c, widget);
    c.w32(widget + 0x18, y);
    if last {
        draw(c, widget);
    }
    true
}

/// Runs before the game updates its menus: finds the settings screen and sends the focus
/// events asked for by `filter_pad`.
pub fn before_update(c: &mut Ctx) {
    ensure_loaded();
    let s = state();
    s.texts = 0;
    let mut rows = [None, None, None];
    for w in on_show_widgets(c, TEXT_WIDGET).into_iter().filter(|w| w.rect[2] == 240.0) {
        let text = characters(c, c.r32(w.addr + WIDGET_TEXT));
        if let Some(i) = GAME_SETTINGS.iter().position(|name| *name == text) {
            rows[i] = Some(w);
        }
    }
    let [Some(rumble), Some(music), Some(sfx)] = rows else {
        s.focus = None;
        return;
    };
    let rows = [rumble, music, sfx];
    // The screen's slide: the middle one of the rows' offsets, so not a bouncing row's.
    let mut offsets = [0, 1, 2].map(|i| rows[i].rect[1] - ROW_TOPS[i]);
    offsets.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
    s.offset = (rows[2].rect[0] - ROWS_LEFT, offsets[1]);
    if !on_show(s) || slid_away(s) {
        s.focus = None;
    }
    s.seen = s.tick;
    s.rows = [rows[0].addr, rows[1].addr, rows[2].addr];
    s.focused = [rows[0].focused, rows[1].focused, rows[2].focused];
    // The game gave one of its own the focus: not while the focus is still to be taken from it.
    if s.focus.is_some() && s.actions.is_empty() && s.focused.iter().any(|&f| f) {
        s.focus = None;
    }
    for (widget, event) in core::mem::take(&mut s.actions) {
        send(c, widget, event);
    }
}

/// Runs on controller 1's state just before the game reads it (12 bytes: buttons, stick, ...).
/// While the settings screen is on show: Down on SFX Volume moves to the port's rows, and
/// while one of those has the focus the arrows, stick and A do their work here.
pub fn filter_pad(status: &mut [u8]) {
    let s = state();
    s.tick = s.tick.wrapping_add(1).max(1);
    if !on_show(s) {
        (s.focus, s.swallow, s.held) = (None, false, 0);
        return;
    }
    let buttons = u16::from_be_bytes([status[0], status[1]]) as u32;
    let (x, y) = (status[2] as i8, status[3] as i8);
    let held = (buttons & UP != 0 || y > STICK_PUSH) as u8
        | ((buttons & DOWN != 0 || y < -STICK_PUSH) as u8) << 1
        | ((buttons & LEFT != 0 || x < -STICK_PUSH) as u8) << 2
        | ((buttons & RIGHT != 0 || x > STICK_PUSH) as u8) << 3;
    let now = s.tick;
    let pressed = held & !s.held;
    let mut act = pressed;
    if pressed != 0 {
        s.repeat_at = now.wrapping_add(REPEAT_AFTER);
    } else if held != 0 && held == s.held && now.wrapping_sub(s.repeat_at) < 0x8000_0000 {
        s.repeat_at = now.wrapping_add(REPEAT_EVERY);
        act = held & 0b1100;
    }
    s.held = held;
    let strip = |status: &mut [u8], also: u32| {
        let kept = buttons & !(UP | DOWN | LEFT | RIGHT | also);
        status[0..2].copy_from_slice(&(kept as u16).to_be_bytes());
        (status[2], status[3]) = (0, 0);
    };

    let Some(focus) = s.focus else {
        if s.swallow {
            if held == 0 {
                s.swallow = false;
            } else {
                strip(status, 0);
            }
        } else if act & 2 != 0 && s.focused[2] {
            s.focus = Some(0);
            s.focus_since = s.tick;
            let sfx = s.rows[2];
            s.actions.push((sfx, FOCUS_OFF_UNSELECT));
            strip(status, 0);
        }
        return;
    };
    strip(status, A);
    if act & 1 != 0 {
        if focus == 0 {
            s.focus = None;
            s.swallow = true;
            let sfx = s.rows[2];
            s.actions.push((sfx, FOCUS_ON_SELECT));
        } else {
            s.focus = Some(focus - 1);
            s.focus_since = s.tick;
        }
    } else if act & 2 != 0 && focus + 1 < SETTING_COUNT {
        s.focus = Some(focus + 1);
        s.focus_since = s.tick;
    }
    if act & 4 != 0 {
        step(focus, -1);
    } else if act & 8 != 0 {
        step(focus, 1);
    }
}

/// Has a widget draw itself at another place, at an opacity, shown whether or not the game
/// shows it, and (a text widget) with another text.
fn draw_widget(c: &mut Ctx, texts: &mut u32, widget: u32, text: Option<&str>, at: (f32, f32), opacity: u8) {
    let saved = (c.r32(widget + 0x14), c.r32(widget + 0x18), c.r8(widget + 0x27), c.r8(widget + 0x54), c.r16(widget + 6), c.r32(widget + WIDGET_TEXT));
    let shadow = c.r8(widget + TEXT_SHADOW);
    if let Some(text) = text {
        let slot = *texts % TEXT_SLOTS;
        *texts += 1;
        let copy = TEXT_SCRATCH + slot * TEXT_SLOT;
        for (i, byte) in text.bytes().take(TEXT_SLOT as usize - 1).chain(core::iter::once(0)).enumerate() {
            c.w8(copy + i as u32, byte);
        }
        c.w32(widget + WIDGET_TEXT, copy);
    }
    c.w32(widget + 0x14, at.0.to_bits());
    c.w32(widget + 0x18, at.1.to_bits());
    c.w8(widget + 0x27, opacity);
    c.w8(widget + 0x54, 1);
    c.w16(widget + 6, saved.4 | 1);
    if text.is_some() {
        c.w8(widget + TEXT_SHADOW, opacity);
        render_text_unhooked(c, widget);
    } else {
        call(c, RENDER_IMAGE, &[widget]);
    }
    c.w32(widget + 0x14, saved.0);
    c.w32(widget + 0x18, saved.1);
    c.w8(widget + 0x27, saved.2);
    c.w8(widget + 0x54, saved.3);
    c.w16(widget + 6, saved.4);
    c.w32(widget + WIDGET_TEXT, saved.5);
    if text.is_some() {
        c.w8(widget + TEXT_SHADOW, shadow);
    }
}

fn active_value(setting: usize) -> String {
    use alloc::string::ToString;
    match setting {
        0 => if FPS_ENABLED.load(Ordering::Relaxed) { "On".into() } else { "Off".into() },
        1 => RES_NAMES[res_level()].into(),
        _ => FOV_VALUES[fov_index()].to_string(),
    }
}

/// Runs after the last of the game's settings (`row`) has drawn itself: draws the port's rows
/// under it, and while one of them has the focus, its title, its range and its bars.
fn draw(c: &mut Ctx, row: u32) {
    let (dx, dy) = state().offset;
    let (focus, since) = {
        let s = state();
        (s.focus, s.tick.wrapping_sub(s.focus_since) % BOUNCE_PERIOD)
    };
    let bounce = if since < BOUNCE_UP {
        let t = since as f32 / BOUNCE_UP as f32 * 2.0 - 1.0;
        BOUNCE_HEIGHT * (1.0 - if t < 0.0 { -t } else { t })
    } else {
        0.0
    };
    let mut texts = state().texts;
    for (i, name) in NAMES.iter().enumerate() {
        let focused = focus == Some(i);
        let top = LAYOUT_TOPS[3 + i] + dy - if focused { bounce } else { 0.0 };
        draw_widget(c, &mut texts, row, Some(name), (ROWS_LEFT + dx, top), if focused { 255 } else { DIMMED });
    }
    let Some(focus) = focus else {
        state().texts = texts;
        return;
    };

    let all_texts = all_widgets(c, TEXT_WIDGET);
    let title = all_texts.iter().find(|(.., rect)| *rect == TITLE).map(|w| w.0);
    if let Some(title) = title {
        draw_widget(c, &mut texts, title, Some(NAMES[focus]), (TITLE[0] + dx, TITLE[1] + dy), 255);
    }

    let word = |x: f32| all_texts.iter().find(|(.., rect)| rect[0] == x && rect[1] == WORDS_TOP && rect[2] == 100.0).map(|w| w.0);
    let words = (word(WORD_LEFT), word(WORD_RIGHT));

    if focus == 0 {
        let on = FPS_ENABLED.load(Ordering::Relaxed);
        if let (Some(low), Some(high)) = words {
            for (label, widget, x, active) in [("Off", low, WORD_LEFT, !on), ("On", high, WORD_RIGHT, on)] {
                match (active, title) {
                    (true, Some(t)) => draw_widget(c, &mut texts, t, Some(label), (x + 50.0 - TITLE[2] * 0.5 + dx, TOGGLE_TOP - 4.0 + dy), 255),
                    _ => draw_widget(c, &mut texts, widget, Some(label), (x + dx, TOGGLE_TOP + dy), if active { 255 } else { DIMMED }),
                }
            }
        }
        state().texts = texts;
        return;
    }

    if let (Some(low), Some(high)) = words {
        let (lo, hi) = match focus {
            1 => (RES_NAMES[0], RES_NAMES[3]),
            _ => ("60", "110"),
        };
        draw_widget(c, &mut texts, low, Some(lo), (266.0 + dx, RANGE_TOP + dy), 165);
        draw_widget(c, &mut texts, high, Some(hi), (464.0 + dx, RANGE_TOP + dy), 165);
    }
    if let Some(title) = title {
        let val = active_value(focus);
        draw_widget(c, &mut texts, title, Some(&val), (BARS_CENTER_X - TITLE[2] * 0.5 + dx, VALUE_TOP + dy), 255);
    }

    let place = bar_place(focus);
    let images = all_widgets(c, IMAGE_WIDGET);
    let by_id = |id: u32| images.iter().find(|w| w.1 == id).map(|w| (w.0, w.2));
    for p in 0..11 {
        if let Some((bar, rect)) = by_id(bar_id(DARK_BARS, p)) {
            draw_widget(c, &mut texts, bar, None, (rect[0] + dx, rect[1] + dy), 255);
        }
    }
    if let Some((lit, rect)) = by_id(bar_id(LIT_BARS, place)) {
        draw_widget(c, &mut texts, lit, None, (rect[0] + dx, rect[1] + dy), 255);
    }
    state().texts = texts;
}
