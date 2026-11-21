//! Konami-code debug menu (`Up Up Down Down Left Right Left Right B A`).
//!
//! Styled cleanly in the game's UI overlay (`gl_renderer::build_debug_overlay`).
//! While open, the game receives no controller input:
//! - `L` / `R`: switch tabs (`WARP` | `ENEMIES` | `TOOLS`)
//! - `D-Pad Up` / `Down`: move selection
//! - `D-Pad Left` / `Right`: cycle values (levels / enemy slot; `X` / `Y` jumps by 5 levels)
//! - `A`: activate / toggle item
//! - `B` or `Start`: close menu

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use runtime::Ctx;

use crate::gl_renderer::{CURRENT_SCENE_ID, FPS_ENABLED};

extern "C" {
    fn switch_now_ns() -> u64;
}

const LEFT: u32 = 0x0001;
const RIGHT: u32 = 0x0002;
const DOWN: u32 = 0x0004;
const UP: u32 = 0x0008;
const R_BTN: u32 = 0x0020;
const L_BTN: u32 = 0x0040;
const A: u32 = 0x0100;
const B: u32 = 0x0200;
const X: u32 = 0x0400;
const Y: u32 = 0x0800;
const START: u32 = 0x1000;
const KEYS: u32 = LEFT | RIGHT | DOWN | UP | A | B;
const KONAMI: [u32; 10] = [UP, UP, DOWN, DOWN, LEFT, RIGHT, LEFT, RIGHT, B, A];

/// All 34 real playable `.hip` scenes verified on disc (`disc/files/`).
pub const LEVELS: [(&str, &str); 34] = [
    ("BB02", "NO CHEESE!"),
    ("BB03", "IM READY DEPRESSION"),
    ("BB01", "SANDWICH DRIVING 101"),
    ("DE01", "3000 MILES TO SHELL CITY"),
    ("DE02", "NOW THAT WERE MEN"),
    ("TT01", "RUB A DUB DUB"),
    ("TT02", "BUBBLE BLOWING BABY HUNT"),
    ("B101", "FROGFISH BOSS"),
    ("TR01", "TRENCH SLIDE"),
    ("TR02", "WELCOME TO PLANKTOPOLIS"),
    ("TR03", "TRENCH OF ADVANCED DARK"),
    ("JK01", "JUNKYARD 1"),
    ("JK02", "JUNKYARD 2"),
    ("B201", "DENNIS BOSS 1"),
    ("GG02", "GOOFY GOOBER DRIVE"),
    ("SC02", "SHELL CITY DEAD AHEAD"),
    ("B301", "DENNIS BOSS 2"),
    ("PT01", "PLANKTOPOLIS 1"),
    ("PT02", "PLANKTOPOLIS DRIVE"),
    ("PT03", "PLANKTOPOLIS 3"),
    ("B401", "KING NEPTUNE BOSS"),
    ("B402", "100 PERCENT REWARD"),
    ("AM01", "COMBAT ARENA 1"),
    ("AM02", "COMBAT ARENA 2"),
    ("AM03", "COMBAT ARENA 3"),
    ("AM04", "COMBAT ARENA 4"),
    ("BL01", "SPONGEBALL ARENA 1"),
    ("BL02", "SPONGEBALL ARENA 2"),
    ("BL03", "SPONGEBALL ARENA 3"),
    ("BL04", "SPONGEBALL ARENA 4"),
    ("FB01", "FLOATING BLOCK 1"),
    ("FB02", "FLOATING BLOCK 2"),
    ("FB03", "FLOATING BLOCK 3"),
    ("MNUS", "MAIN MENU"),
];

pub const TAB_NAMES: [&str; 3] = ["WARP", "ENEMIES", "TOOLS"];
const NUM_TABS: usize = 3;
const WARP_ITEMS: usize = 8;
const ENEMY_ITEMS: usize = 7;
const TOOL_ITEMS: usize = 10;

/// Game globals (`zGlobals` at `0x804ADA80`), verified from recompiled guest code (`gen/m*.rs`).
const GLOBALS: u32 = 0x804A_DA80;
/// Free Switch host scratch region (`0x817E_0A00`, clear of PORTAL `0x817E_0900` and options `0x817F_0000`).
const SCRATCH_VEC3: u32 = 0x817E_0A00;

/// Verified guest function addresses (`crates/game/src/gen/mod.rs`).
const FN_NME_RESPAWN: u32 = 0x8005_0BD8; // Respawn__10zNMECommonFPC5xVec3P10zMovePointP10zMovePoint
const FN_NME_BUPDATE: u32 = 0x8000_84E4; // BUpdate__10zNMECommonFP5xVec3
const FN_ENT_SHOW: u32 = 0x801B_5578; // xEntShow__FP4xEnt
const FN_ENT_HIDE: u32 = 0x801B_553C; // xEntHide__FP4xEnt
const FN_ALL_UPGRADED_MOVES: u32 = 0x800E_81A4; // AllUpgradedMovesCB__Fv
const FN_ALL_MOVES: u32 = 0x800E_8330; // AllMovesCB__Fv
const FN_MAX_HEALTH_SLOTS: u32 = 0x800E_8540; // MaxHealthSlotsCB__Fv
const FN_CHA_CHING: u32 = 0x800E_86C0; // ChaChingCB__Fv
const FN_INC_MANLINESS: u32 = 0x8011_6E24; // IncManlinessPoints__11zRewardsMgrFUib

static OPEN: AtomicBool = AtomicBool::new(false);
static SWALLOW: AtomicBool = AtomicBool::new(false);
static TAB: AtomicUsize = AtomicUsize::new(0);
static CURSOR: AtomicUsize = AtomicUsize::new(0);
static LEVEL: AtomicUsize = AtomicUsize::new(0);
static ENEMY_SLOT: AtomicUsize = AtomicUsize::new(0);

// Debug toggles
static GOD: AtomicBool = AtomicBool::new(false);
static FLY: AtomicBool = AtomicBool::new(false);
static FREEZE_ENEMIES: AtomicBool = AtomicBool::new(false);
static CHAR_SWITCH: AtomicBool = AtomicBool::new(false);

const L_STICK_PRESS: i32 = 18_000;
const L_STICK_RELEASE: i32 = 10_000;
const MENU_KEYS: u32 = LEFT | RIGHT | DOWN | UP | L_BTN | R_BTN | A | B | X | Y | START;

// Pad edge detector & Konami progress (runs at 125 Hz from main.c host loop)
static HOST_LAST: AtomicU32 = AtomicU32::new(0);
static HOST_STICK_DIR: AtomicU32 = AtomicU32::new(0);
static HOST_EXTRA_LAST: AtomicU32 = AtomicU32::new(0);
static PROGRESS: AtomicUsize = AtomicUsize::new(0);
static SWAP_BA: AtomicBool = AtomicBool::new(false);
static MENU_LATCH: AtomicU32 = AtomicU32::new(0);
static NEED_SYNC_OPEN: AtomicBool = AtomicBool::new(false);
static OPENED_AT_NS: AtomicU64 = AtomicU64::new(0);
static REPEAT_DIR: AtomicU32 = AtomicU32::new(0);
static REPEAT_NEXT_NS: AtomicU64 = AtomicU64::new(0);

// Live telemetry snapshot (written on Core 1 in filter_pad, read lock-free on Core 0/2 overlay)
static TELEM_X: AtomicU32 = AtomicU32::new(0);
static TELEM_Y: AtomicU32 = AtomicU32::new(0);
static TELEM_Z: AtomicU32 = AtomicU32::new(0);
static TELEM_HP: AtomicU32 = AtomicU32::new(0);
static TELEM_MAX_HP: AtomicU32 = AtomicU32::new(0);
static TELEM_MANLINESS: AtomicU32 = AtomicU32::new(0);
static TELEM_TOKENS: AtomicU32 = AtomicU32::new(0);
static TELEM_NPC_TOTAL: AtomicU32 = AtomicU32::new(0);
static TELEM_NPC_ALIVE: AtomicU32 = AtomicU32::new(0);
static TELEM_SLOT_ALIVE: AtomicBool = AtomicBool::new(false);

// Lock-free seqlock toast message (up to 48 ASCII bytes) shown for ~2.5s
const TOAST_CAP: usize = 48;
static TOAST_SEQ: AtomicU32 = AtomicU32::new(0);
static TOAST_LEN: AtomicUsize = AtomicUsize::new(0);
static TOAST_UNTIL_NS: AtomicU64 = AtomicU64::new(0);
static TOAST_BUF: [AtomicU8; TOAST_CAP] = [const { AtomicU8::new(0) }; TOAST_CAP];

// Core 1 only: frozen enemy positions snapshot (`(npc_ptr, [x, y, z])`)
const MAX_FROZEN: usize = 96;
static mut FROZEN_COUNT: usize = 0;
static mut FROZEN_POS: [(u32, [f32; 3]); MAX_FROZEN] = [(0, [0.0; 3]); MAX_FROZEN];

pub struct MenuView {
    pub scene: String,
    pub tab: usize,
    pub cursor: usize,
    pub telemetry: String,
    pub items: Vec<String>,
    pub hint: &'static str,
}

#[inline(always)]
fn valid_ptr(addr: u32) -> bool {
    (0x8000_0000..0x817F_0000).contains(&addr) && (addr & 3) == 0
}

#[inline(always)]
fn valid_byte_ptr(addr: u32) -> bool {
    (0x8000_0000..0x817F_0000).contains(&addr)
}

#[inline(always)]
fn read_f32(c: &Ctx, addr: u32) -> f32 {
    f32::from_bits(c.r32(addr))
}

#[inline(always)]
fn write_f32(c: &mut Ctx, addr: u32, v: f32) {
    c.w32(addr, v.to_bits());
}

fn tab_item_count(tab: usize) -> usize {
    match tab {
        0 => WARP_ITEMS,
        1 => ENEMY_ITEMS,
        _ => TOOL_ITEMS,
    }
}

fn set_toast(msg: &str) {
    let bytes = msg.as_bytes();
    let len = bytes.len().min(TOAST_CAP);
    let s = TOAST_SEQ.load(Ordering::Relaxed);
    TOAST_SEQ.store(s.wrapping_add(1) | 1, Ordering::Release);
    for i in 0..len {
        TOAST_BUF[i].store(bytes[i], Ordering::Relaxed);
    }
    TOAST_LEN.store(len, Ordering::Relaxed);
    let now = unsafe { switch_now_ns() };
    TOAST_UNTIL_NS.store(now.saturating_add(2_600_000_000), Ordering::Relaxed);
    TOAST_SEQ.store((s.wrapping_add(1) | 1).wrapping_add(1), Ordering::Release);
}

/// Returns active toast message if still within its display window.
pub fn active_toast() -> Option<String> {
    let until = TOAST_UNTIL_NS.load(Ordering::Relaxed);
    if until == 0 {
        return None;
    }
    let now = unsafe { switch_now_ns() };
    if now >= until {
        return None;
    }
    for _ in 0..4 {
        let s0 = TOAST_SEQ.load(Ordering::Acquire);
        if (s0 & 1) != 0 {
            continue;
        }
        let len = TOAST_LEN.load(Ordering::Relaxed).min(TOAST_CAP);
        let mut out = String::with_capacity(len);
        for i in 0..len {
            let b = TOAST_BUF[i].load(Ordering::Relaxed);
            if (0x20..0x7F).contains(&b) {
                out.push(b as char);
            }
        }
        let s1 = TOAST_SEQ.load(Ordering::Acquire);
        if s0 == s1 {
            return if out.is_empty() { None } else { Some(out) };
        }
    }
    None
}

pub fn is_open() -> bool {
    OPEN.load(Ordering::Relaxed)
}

/// Builds the full UI view for `gl_renderer::build_debug_overlay`.
pub fn menu_view() -> MenuView {
    let on = |b: bool| if b { "ON" } else { "OFF" };
    let scene_bytes = CURRENT_SCENE_ID.load(Ordering::Relaxed).to_be_bytes();
    let scene: String = scene_bytes
        .iter()
        .map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '?' })
        .collect();

    let px = f32::from_bits(TELEM_X.load(Ordering::Relaxed));
    let py = f32::from_bits(TELEM_Y.load(Ordering::Relaxed));
    let pz = f32::from_bits(TELEM_Z.load(Ordering::Relaxed));
    let hp = TELEM_HP.load(Ordering::Relaxed);
    let max_hp = TELEM_MAX_HP.load(Ordering::Relaxed);
    let pts = TELEM_MANLINESS.load(Ordering::Relaxed);
    let toks = TELEM_TOKENS.load(Ordering::Relaxed);
    let npc_alive = TELEM_NPC_ALIVE.load(Ordering::Relaxed);
    let npc_total = TELEM_NPC_TOTAL.load(Ordering::Relaxed);

    let telemetry = format!(
        "POS:{:.0},{:.0},{:.0}  HP:{}/{}  NPC:{}/{}  PTS:{}  GG:{}",
        px, py, pz, hp, max_hp, npc_alive, npc_total, pts, toks
    );

    let tab = TAB.load(Ordering::Relaxed) % NUM_TABS;
    let cursor = CURSOR.load(Ordering::Relaxed).min(tab_item_count(tab) - 1);

    let items = match tab {
        0 => {
            let idx = LEVEL.load(Ordering::Relaxed) % LEVELS.len();
            let (id, name) = LEVELS[idx];
            alloc::vec![
                format!("LEVEL < {:02}/{} {} {} >", idx + 1, LEVELS.len(), id, name),
                String::from("WARP TO SELECTED LEVEL"),
                String::from("QUICK: BB02 NO CHEESE!"),
                String::from("QUICK: BB01 SANDWICH DRIVING 101"),
                String::from("QUICK: B401 KING NEPTUNE BOSS"),
                String::from("QUICK: AM01 COMBAT ARENA 1"),
                String::from("RESET CURRENT LEVEL"),
                String::from("WARP TO MAIN MENU (MNUS)"),
            ]
        }
        1 => {
            let slot_str = if npc_total == 0 {
                String::from("ENEMY SLOT < NONE IN SCENE >")
            } else {
                let s = ENEMY_SLOT.load(Ordering::Relaxed) % (npc_total as usize);
                let st = if TELEM_SLOT_ALIVE.load(Ordering::Relaxed) {
                    "ALIVE"
                } else {
                    "HIDDEN"
                };
                format!("ENEMY SLOT < {:02}/{:02} [{}] >", s + 1, npc_total, st)
            };
            alloc::vec![
                slot_str,
                String::from("SPAWN SELECTED ENEMY IN FRONT"),
                String::from("SPAWN 5 ENEMIES AROUND PLAYER"),
                String::from("BRING ALL ENEMIES HERE (MAX 12)"),
                String::from("DESPAWN / HIDE ALL ENEMIES"),
                format!("FREEZE ENEMIES [{}]", on(FREEZE_ENEMIES.load(Ordering::Relaxed))),
                String::from("RESPAWN ALL ENEMIES AT HOME"),
            ]
        }
        _ => alloc::vec![
            format!("GOD MODE (INF HEALTH) [{}]", on(GOD.load(Ordering::Relaxed))),
            String::from("REFILL HEALTH + MAX SLOTS (6)"),
            format!("FLY / NOCLIP MODE [{}]", on(FLY.load(Ordering::Relaxed))),
            String::from("UNLOCK ALL MOVES + UPGRADES"),
            String::from("+1000 MANLINESS POINTS"),
            String::from("2X MANLINESS (CHA-CHING)"),
            String::from("+5 GOOFY GOOBER TOKENS"),
            format!("CHAR SWITCH (D-PAD) [{}]", on(CHAR_SWITCH.load(Ordering::Relaxed))),
            String::from("TELEPORT UP (+10M)"),
            format!("FPS COUNTER [{}]", on(FPS_ENABLED.load(Ordering::Relaxed))),
        ],
    };

    let hint = match tab {
        0 => "L/R:TAB  U/D:SEL  L/R:LVL  X/Y:+-5  A:WARP  B:EXIT",
        1 => "L/R:TAB  U/D:SEL  LEFT/RIGHT:SLOT  A:SPAWN  B:EXIT",
        _ => "L/R:TAB  U/D:SEL  A:TOGGLE/APPLY  B/START:CLOSE",
    };

    MenuView {
        scene,
        tab,
        cursor,
        telemetry,
        items,
        hint,
    }
}

fn warp(id: u32, label: &str) {
    // Clear frozen snapshot and fly flag cleanly when leaving a scene.
    FREEZE_ENEMIES.store(false, Ordering::Relaxed);
    FLY.store(false, Ordering::Relaxed);
    unsafe {
        FROZEN_COUNT = 0;
    }
    crate::WARP_TARGET.store(id, Ordering::Relaxed);
    crate::TARGET_SCENE.store(id, Ordering::Relaxed);
    CURRENT_SCENE_ID.store(id, Ordering::Relaxed);
    set_toast(&format!("WARPING TO {}", label));
}

fn warp_code(code: &str) {
    if let Ok(bytes) = <[u8; 4]>::try_from(code.as_bytes()) {
        warp(u32::from_be_bytes(bytes), code);
        close();
    }
}

fn player_pos_and_fwd(c: &Ctx) -> Option<([f32; 3], [f32; 3])> {
    let player = c.r32(GLOBALS + 0xAF0);
    if !valid_ptr(player) {
        return None;
    }
    let model = c.r32(player + 0x28);
    if valid_ptr(model) {
        let mat = c.r32(model + 0x54);
        if valid_ptr(mat) {
            let pos = [read_f32(c, mat + 0x30), read_f32(c, mat + 0x34), read_f32(c, mat + 0x38)];
            let mut fx = read_f32(c, mat + 0x20);
            let mut fz = read_f32(c, mat + 0x28);
            let len = libm::sqrtf(fx * fx + fz * fz);
            if len > 1e-4 {
                fx /= len;
                fz /= len;
            } else {
                fx = 0.0;
                fz = 1.0;
            }
            return Some((pos, [fx, 0.0, fz]));
        }
    }
    let frame = c.r32(player + 0x48);
    if valid_ptr(frame) {
        let pos = [read_f32(c, frame + 0x30), read_f32(c, frame + 0x34), read_f32(c, frame + 0x38)];
        return Some((pos, [0.0, 0.0, 1.0]));
    }
    None
}

fn npc_list(c: &Ctx) -> Option<(u32, usize)> {
    let mgr = c.r32(c.r[13].wrapping_add(0xFFFF_8DBC));
    if !valid_ptr(mgr) {
        return None;
    }
    let list = c.r32(mgr);
    let cnt = c.r32(mgr + 4) as i32;
    if !valid_ptr(list) || cnt <= 0 || cnt > 256 {
        return None;
    }
    Some((list, cnt as usize))
}

#[inline(always)]
fn npc_is_visible(c: &Ctx, npc: u32) -> bool {
    let flags = c.r8(npc + 0x16);
    (flags & 0x01) != 0 && (flags & 0x84) == 0
}

fn set_npc_pos(c: &mut Ctx, npc: u32, pos: [f32; 3]) {
    let model = c.r32(npc + 0x28);
    if valid_ptr(model) {
        let mat = c.r32(model + 0x54);
        if valid_ptr(mat) {
            write_f32(c, mat + 0x30, pos[0]);
            write_f32(c, mat + 0x34, pos[1]);
            write_f32(c, mat + 0x38, pos[2]);
        }
    }
    let frame = c.r32(npc + 0x48);
    if valid_ptr(frame) {
        write_f32(c, frame + 0x30, pos[0]);
        write_f32(c, frame + 0x34, pos[1]);
        write_f32(c, frame + 0x38, pos[2]);
        write_f32(c, frame + 0x70, pos[0]);
        write_f32(c, frame + 0x74, pos[1]);
        write_f32(c, frame + 0x78, pos[2]);
        c.w32(frame + 0xC8, 0);
        c.w32(frame + 0xCC, 0);
        c.w32(frame + 0xD0, 0);
    }
}

fn spawn_npc_at(c: &mut Ctx, npc: u32, pos: [f32; 3]) -> bool {
    if !valid_ptr(npc) {
        return false;
    }
    write_f32(c, SCRATCH_VEC3, pos[0]);
    write_f32(c, SCRATCH_VEC3 + 4, pos[1]);
    write_f32(c, SCRATCH_VEC3 + 8, pos[2]);
    // 1. Official engine respawn message (`NPC_MSG_SPAWN`) restores HP, psyche, flags, and animations.
    crate::call_guest_addr(c, FN_NME_RESPAWN, &[npc, SCRATCH_VEC3, 0, 0]);
    // 2. Ensure entity is visible & immediately relocated even if already alive.
    crate::call_guest_addr(c, FN_ENT_SHOW, &[npc]);
    set_npc_pos(c, npc, pos);
    // 3. Refresh collision bound & spatial grid at the new position.
    crate::call_guest_addr(c, FN_NME_BUPDATE, &[npc, SCRATCH_VEC3]);
    // Keep freeze table updated if Freeze Enemies is active.
    if FREEZE_ENEMIES.load(Ordering::Relaxed) {
        unsafe {
            let n = FROZEN_COUNT;
            for i in 0..n {
                if FROZEN_POS[i].0 == npc {
                    FROZEN_POS[i].1 = pos;
                    return true;
                }
            }
            if n < MAX_FROZEN {
                FROZEN_POS[n] = (npc, pos);
                FROZEN_COUNT = n + 1;
            }
        }
    }
    true
}

fn spawn_selected_enemy(c: &mut Ctx) {
    let Some((list, cnt)) = npc_list(c) else {
        set_toast("NO NPCS LOADED IN SCENE");
        return;
    };
    let Some((ppos, pfwd)) = player_pos_and_fwd(c) else {
        set_toast("PLAYER NOT ACTIVE");
        return;
    };
    let slot = ENEMY_SLOT.load(Ordering::Relaxed) % cnt;
    let npc = c.r32(list + (slot as u32) * 4);
    let target = [
        ppos[0] + pfwd[0] * 6.0,
        ppos[1] + 1.0,
        ppos[2] + pfwd[2] * 6.0,
    ];
    if spawn_npc_at(c, npc, target) {
        ENEMY_SLOT.store((slot + 1) % cnt, Ordering::Relaxed);
        set_toast(&format!("SPAWNED ENEMY #{} IN FRONT!", slot + 1));
    } else {
        set_toast("INVALID ENEMY SLOT");
    }
}

fn spawn_many_enemies(c: &mut Ctx, max_n: usize) {
    let Some((list, cnt)) = npc_list(c) else {
        set_toast("NO NPCS LOADED IN SCENE");
        return;
    };
    let Some((ppos, pfwd)) = player_pos_and_fwd(c) else {
        set_toast("PLAYER NOT ACTIVE");
        return;
    };
    let to_spawn = cnt.min(max_n);
    let start_slot = ENEMY_SLOT.load(Ordering::Relaxed) % cnt;
    let right = [pfwd[2], 0.0, -pfwd[0]];
    let mut spawned = 0usize;
    for i in 0..to_spawn {
        let idx = (start_slot + i) % cnt;
        let npc = c.r32(list + (idx as u32) * 4);
        let frac = if to_spawn > 1 {
            (i as f32) / ((to_spawn - 1) as f32) - 0.5
        } else {
            0.0
        };
        let angle = frac * 2.2; // ~126 degree arc in front of player
        let dist = 6.5 + ((i % 2) as f32) * 2.0;
        let ca = libm::cosf(angle);
        let sa = libm::sinf(angle);
        let dx = (pfwd[0] * ca + right[0] * sa) * dist;
        let dz = (pfwd[2] * ca + right[2] * sa) * dist;
        if spawn_npc_at(c, npc, [ppos[0] + dx, ppos[1] + 1.0, ppos[2] + dz]) {
            spawned += 1;
        }
    }
    ENEMY_SLOT.store((start_slot + to_spawn) % cnt, Ordering::Relaxed);
    set_toast(&format!("SPAWNED {} ENEMIES AROUND!", spawned));
}

fn despawn_all_enemies(c: &mut Ctx) {
    let Some((list, cnt)) = npc_list(c) else {
        set_toast("NO NPCS LOADED IN SCENE");
        return;
    };
    let mut hidden = 0usize;
    for i in 0..cnt {
        let npc = c.r32(list + (i as u32) * 4);
        if valid_ptr(npc) {
            crate::call_guest_addr(c, FN_ENT_HIDE, &[npc]);
            hidden += 1;
        }
    }
    set_toast(&format!("DESPAWNED {} ENEMIES", hidden));
}

fn respawn_all_home(c: &mut Ctx) {
    let Some((list, cnt)) = npc_list(c) else {
        set_toast("NO NPCS LOADED IN SCENE");
        return;
    };
    let mut n = 0usize;
    for i in 0..cnt {
        let npc = c.r32(list + (i as u32) * 4);
        if valid_ptr(npc) {
            crate::call_guest_addr(c, FN_NME_RESPAWN, &[npc, 0, 0, 0]);
            crate::call_guest_addr(c, FN_ENT_SHOW, &[npc]);
            n += 1;
        }
    }
    if FREEZE_ENEMIES.load(Ordering::Relaxed) {
        snapshot_frozen_enemies(c);
    }
    set_toast(&format!("RESPAWNED {} ENEMIES AT HOME", n));
}

fn snapshot_frozen_enemies(c: &Ctx) {
    unsafe {
        FROZEN_COUNT = 0;
        let Some((list, cnt)) = npc_list(c) else {
            return;
        };
        let mut out = 0usize;
        for i in 0..cnt.min(MAX_FROZEN) {
            let npc = c.r32(list + (i as u32) * 4);
            if !valid_ptr(npc) {
                continue;
            }
            let model = c.r32(npc + 0x28);
            if valid_ptr(model) {
                let mat = c.r32(model + 0x54);
                if valid_ptr(mat) {
                    FROZEN_POS[out] = (
                        npc,
                        [read_f32(c, mat + 0x30), read_f32(c, mat + 0x34), read_f32(c, mat + 0x38)],
                    );
                    out += 1;
                }
            }
        }
        FROZEN_COUNT = out;
    }
}

fn enforce_frozen_enemies(c: &mut Ctx) {
    if !FREEZE_ENEMIES.load(Ordering::Relaxed) {
        return;
    }
    let Some((list, cnt)) = npc_list(c) else {
        unsafe {
            FROZEN_COUNT = 0;
        }
        return;
    };
    unsafe {
        if FROZEN_COUNT == 0 {
            snapshot_frozen_enemies(c);
        }
        let n = FROZEN_COUNT;
        for i in 0..n {
            let (npc, pos) = FROZEN_POS[i];
            if valid_ptr(npc) {
                // Verify pointer still belongs to current scene's NPC list before writing.
                let mut found = false;
                for j in 0..cnt {
                    if c.r32(list + (j as u32) * 4) == npc {
                        found = true;
                        break;
                    }
                }
                if found {
                    set_npc_pos(c, npc, pos);
                }
            }
        }
    }
}

fn refill_health(c: &mut Ctx) {
    let player = c.r32(GLOBALS + 0xAF0);
    if valid_ptr(player) {
        let slots = c.r32(GLOBALS + 0xF64).clamp(1, 12);
        c.w32(player + 0x460, slots);
    }
}

fn set_fly_mode(c: &mut Ctx, enable: bool) {
    FLY.store(enable, Ordering::Relaxed);
    c.w32(GLOBALS + 0xA24, 1); // G.CheatFlyToggle
    c.w32(GLOBALS + 0x1DA8, if enable { 1 } else { 0 });
    let player = c.r32(GLOBALS + 0xAF0);
    if valid_ptr(player) {
        let frame = c.r32(player + 0x48);
        if valid_ptr(frame) {
            c.w32(frame + 0xC8, 0);
            c.w32(frame + 0xCC, 0);
            c.w32(frame + 0xD0, 0);
        }
        let coll_ptr = c.r32(player + 0x4C);
        if valid_byte_ptr(coll_ptr) {
            c.w8(coll_ptr, if enable { 0x00 } else { 0x2E });
        }
    }
    set_toast(if enable { "FLY / NOCLIP MODE ON!" } else { "FLY / NOCLIP MODE OFF" });
}

fn teleport_up(c: &mut Ctx, dy: f32) {
    let player = c.r32(GLOBALS + 0xAF0);
    if !valid_ptr(player) {
        set_toast("PLAYER NOT ACTIVE");
        return;
    }
    let model = c.r32(player + 0x28);
    if valid_ptr(model) {
        let mat = c.r32(model + 0x54);
        if valid_ptr(mat) {
            let y = read_f32(c, mat + 0x34) + dy;
            write_f32(c, mat + 0x34, y);
        }
    }
    let frame = c.r32(player + 0x48);
    if valid_ptr(frame) {
        let y = read_f32(c, frame + 0x34) + dy;
        write_f32(c, frame + 0x34, y);
        write_f32(c, frame + 0x74, y);
        c.w32(frame + 0xCC, 0);
    }
    set_toast("TELEPORTED +10M UP!");
}

fn update_telemetry(c: &Ctx) {
    if let Some((pos, _)) = player_pos_and_fwd(c) {
        TELEM_X.store(pos[0].to_bits(), Ordering::Relaxed);
        TELEM_Y.store(pos[1].to_bits(), Ordering::Relaxed);
        TELEM_Z.store(pos[2].to_bits(), Ordering::Relaxed);
    }
    let player = c.r32(GLOBALS + 0xAF0);
    if valid_ptr(player) {
        TELEM_HP.store(c.r32(player + 0x460).min(99), Ordering::Relaxed);
    } else {
        TELEM_HP.store(0, Ordering::Relaxed);
    }
    TELEM_MAX_HP.store(c.r32(GLOBALS + 0xF64).min(99), Ordering::Relaxed);
    TELEM_MANLINESS.store(c.r32(GLOBALS + 0x1DB4), Ordering::Relaxed);
    TELEM_TOKENS.store(c.r32(GLOBALS + 0x1DBC), Ordering::Relaxed);
    FLY.store(c.r32(GLOBALS + 0x1DA8) != 0, Ordering::Relaxed);
    CHAR_SWITCH.store(c.r32(GLOBALS + 0xA1C) != 0, Ordering::Relaxed);

    if let Some((list, cnt)) = npc_list(c) {
        let mut alive = 0u32;
        for i in 0..cnt {
            let npc = c.r32(list + (i as u32) * 4);
            if valid_ptr(npc) && npc_is_visible(c, npc) {
                alive += 1;
            }
        }
        TELEM_NPC_TOTAL.store(cnt as u32, Ordering::Relaxed);
        TELEM_NPC_ALIVE.store(alive, Ordering::Relaxed);
        let slot = ENEMY_SLOT.load(Ordering::Relaxed) % cnt;
        let sel = c.r32(list + (slot as u32) * 4);
        TELEM_SLOT_ALIVE.store(valid_ptr(sel) && npc_is_visible(c, sel), Ordering::Relaxed);
    } else {
        TELEM_NPC_TOTAL.store(0, Ordering::Relaxed);
        TELEM_NPC_ALIVE.store(0, Ordering::Relaxed);
        TELEM_SLOT_ALIVE.store(false, Ordering::Relaxed);
    }
}

fn activate(c: &mut Ctx) {
    let tab = TAB.load(Ordering::Relaxed) % NUM_TABS;
    let cursor = CURSOR.load(Ordering::Relaxed);
    match tab {
        0 => match cursor {
            0 | 1 => {
                let (id, _) = LEVELS[LEVEL.load(Ordering::Relaxed) % LEVELS.len()];
                warp_code(id);
            }
            2 => warp_code("BB02"),
            3 => warp_code("BB01"),
            4 => warp_code("B401"),
            5 => warp_code("AM01"),
            6 => {
                let scene = CURRENT_SCENE_ID.load(Ordering::Relaxed);
                if scene != 0 {
                    let b = scene.to_be_bytes();
                    let s: String = b.iter().map(|&ch| ch as char).collect();
                    warp(scene, &s);
                    close();
                }
            }
            _ => warp_code("MNUS"),
        },
        1 => match cursor {
            0 | 1 => spawn_selected_enemy(c),
            2 => spawn_many_enemies(c, 5),
            3 => spawn_many_enemies(c, 12),
            4 => despawn_all_enemies(c),
            5 => {
                let next = !FREEZE_ENEMIES.load(Ordering::Relaxed);
                FREEZE_ENEMIES.store(next, Ordering::Relaxed);
                if next {
                    snapshot_frozen_enemies(c);
                    set_toast("ENEMIES FROZEN!");
                } else {
                    unsafe {
                        FROZEN_COUNT = 0;
                    }
                    set_toast("ENEMIES UNFROZEN");
                }
            }
            _ => respawn_all_home(c),
        },
        _ => match cursor {
            0 => {
                let next = !GOD.load(Ordering::Relaxed);
                GOD.store(next, Ordering::Relaxed);
                if next {
                    refill_health(c);
                }
                set_toast(if next { "GOD MODE ON!" } else { "GOD MODE OFF" });
            }
            1 => {
                crate::call_guest_addr(c, FN_MAX_HEALTH_SLOTS, &[]);
                refill_health(c);
                set_toast("MAX HEALTH SLOTS (6) + REFILLED!");
            }
            2 => {
                let next = c.r32(GLOBALS + 0x1DA8) == 0;
                set_fly_mode(c, next);
            }
            3 => {
                crate::call_guest_addr(c, FN_ALL_MOVES, &[]);
                crate::call_guest_addr(c, FN_ALL_UPGRADED_MOVES, &[]);
                set_toast("ALL MOVES + UPGRADES UNLOCKED!");
            }
            4 => {
                crate::call_guest_addr(c, FN_INC_MANLINESS, &[0, 1000, 0]);
                set_toast("+1000 MANLINESS POINTS!");
            }
            5 => {
                crate::call_guest_addr(c, FN_CHA_CHING, &[]);
                set_toast("2X MANLINESS (CHA-CHING) ON!");
            }
            6 => {
                let cur = c.r32(GLOBALS + 0x1DBC);
                c.w32(GLOBALS + 0x1DBC, cur.saturating_add(5).min(999));
                set_toast("+5 GOOFY GOOBER TOKENS!");
            }
            7 => {
                let next = c.r32(GLOBALS + 0xA1C) == 0;
                c.w32(GLOBALS + 0xA1C, if next { 1 } else { 0 });
                CHAR_SWITCH.store(next, Ordering::Relaxed);
                set_toast(if next {
                    "CHAR SWITCH CHEAT ON (D-PAD)!"
                } else {
                    "CHAR SWITCH CHEAT OFF"
                });
            }
            8 => teleport_up(c, 10.0),
            _ => {
                let next = !FPS_ENABLED.load(Ordering::Relaxed);
                FPS_ENABLED.store(next, Ordering::Relaxed);
                set_toast(if next { "FPS COUNTER ON" } else { "FPS COUNTER OFF" });
            }
        },
    }
    update_telemetry(c);
}

fn open_menu(via_konami: bool) {
    PROGRESS.store(0, Ordering::Relaxed);
    SWAP_BA.store(false, Ordering::Relaxed);
    CURSOR.store(0, Ordering::Relaxed);
    MENU_LATCH.store(0, Ordering::Relaxed);
    REPEAT_DIR.store(0, Ordering::Relaxed);
    let now = unsafe { switch_now_ns() };
    OPENED_AT_NS.store(now, Ordering::Relaxed);
    NEED_SYNC_OPEN.store(true, Ordering::Relaxed);
    SWALLOW.store(true, Ordering::Relaxed);
    OPEN.store(true, Ordering::Release);
    if via_konami {
        set_toast("KONAMI DEBUG MENU UNLOCKED!");
        crate::log_c("[debug] Konami debug menu OPENED!");
    } else {
        set_toast("DEBUG MENU OPENED (L3+R3)!");
        crate::log_c("[debug] Debug menu OPENED via L3+R3!");
    }
}

fn close() {
    OPEN.store(false, Ordering::Release);
    SWALLOW.store(true, Ordering::Relaxed);
    MENU_LATCH.store(0, Ordering::Relaxed);
    REPEAT_DIR.store(0, Ordering::Relaxed);
}

static STICK_CENTERED: AtomicBool = AtomicBool::new(true);
static LAST_KEY_TIME_NS: AtomicU64 = AtomicU64::new(0);

#[inline]
fn stick_to_cardinal(lx: i32, ly: i32, prev: u32) -> u32 {
    let ax = lx.abs();
    let ay = ly.abs();
    // Return to center required between distinct stick pushes
    if ax < 8_000 && ay < 8_000 {
        STICK_CENTERED.store(true, Ordering::Relaxed);
        return 0;
    }
    // Hysteresis: if a cardinal direction was already held, hold until below L_STICK_RELEASE.
    if prev != 0 {
        let still = match prev {
            UP => ly > L_STICK_RELEASE && ay >= ax,
            DOWN => ly < -L_STICK_RELEASE && ay >= ax,
            RIGHT => lx > L_STICK_RELEASE && ax >= ay,
            LEFT => lx < -L_STICK_RELEASE && ax >= ay,
            _ => false,
        };
        if still {
            return prev;
        }
    }
    // Only register a new push if stick previously returned to center
    if !STICK_CENTERED.load(Ordering::Relaxed) {
        return 0;
    }
    let dir = if ay >= ax {
        if ly > L_STICK_PRESS {
            UP
        } else if ly < -L_STICK_PRESS {
            DOWN
        } else {
            0
        }
    } else if lx > L_STICK_PRESS {
        RIGHT
    } else if lx < -L_STICK_PRESS {
        LEFT
    } else {
        0
    };
    if dir != 0 {
        STICK_CENTERED.store(false, Ordering::Relaxed);
    }
    dir
}

pub fn feed_konami_key(key: u32) {
    if key == 0 || OPEN.load(Ordering::Acquire) {
        return;
    }
    let now = unsafe { switch_now_ns() };
    let last = LAST_KEY_TIME_NS.swap(now, Ordering::Relaxed);
    // 3.0s timeout: reset progress if too much time has elapsed between taps
    if last != 0 && now.saturating_sub(last) > 3_000_000_000 {
        PROGRESS.store(0, Ordering::Relaxed);
    }

    let at = PROGRESS.load(Ordering::Relaxed);
    let next = if at < 8 {
        let expected = KONAMI[at];
        if (key & expected) != 0 {
            at + 1
        } else if at > 0 && (key & KONAMI[at - 1]) != 0 {
            // Repeated tap of the same direction (e.g. 3rd UP or 3rd DOWN) stays in place
            at
        } else if (key & KONAMI[0]) != 0 {
            1
        } else {
            0
        }
    } else if at == 8 {
        // After all 8 directions (U U D D L R L R), accept native B->A or swapped A->B
        if (key & B) != 0 && (key & A) != 0 {
            // Rolled B+A in the same tick: complete immediately
            10
        } else if (key & B) != 0 {
            SWAP_BA.store(false, Ordering::Relaxed);
            9
        } else if (key & A) != 0 {
            SWAP_BA.store(true, Ordering::Relaxed);
            9
        } else if (key & RIGHT) != 0 {
            8
        } else if (key & UP) != 0 {
            1
        } else {
            0
        }
    } else {
        // Step 9: expect A (normal) or B (if swapped layout)
        let expected = if SWAP_BA.load(Ordering::Relaxed) { B } else { A };
        if (key & expected) != 0 {
            10
        } else if (key & UP) != 0 {
            1
        } else {
            0
        }
    };

    if next >= KONAMI.len() {
        open_menu(true);
    } else {
        if next != at && next > 0 {
            crate::log_c(&alloc::format!("[debug] Konami progress: {}/10 (key=0x{:X})", next, key));
        }
        PROGRESS.store(next, Ordering::Relaxed);
    }
}

/// Runs at 125 Hz from the Switch host input loop (`main.c`) so rapid Konami taps
/// and menu presses are never dropped even during heavy frames or loading screens.
/// Accepts BOTH D-Pad and Left Stick for directions, tolerates diagonals / extra leading UPs,
/// accepts both `B A` and swapped `A B` after the 8-direction prefix, and also toggles on `L3+R3`.
pub fn on_host_input(gc_buttons: u32, lx: i32, ly: i32, host_extra: u32) {
    let prev_stick = HOST_STICK_DIR.load(Ordering::Relaxed);
    let stick_dir = stick_to_cardinal(lx, ly, prev_stick);
    HOST_STICK_DIR.store(stick_dir, Ordering::Relaxed);

    let combined = gc_buttons | stick_dir;
    let prev = HOST_LAST.swap(combined, Ordering::Relaxed);
    let pressed = combined & !prev;

    // Instant toggle shortcut: L3 + R3 (both analog stick clicks simultaneously; unused on GC).
    let prev_extra = HOST_EXTRA_LAST.swap(host_extra, Ordering::Relaxed);
    let both_sticks_now = (host_extra & 0x03) == 0x03;
    let both_sticks_prev = (prev_extra & 0x03) == 0x03;
    if both_sticks_now && !both_sticks_prev {
        if OPEN.load(Ordering::Acquire) {
            close();
        } else {
            open_menu(false);
        }
        return;
    }

    if !OPEN.load(Ordering::Acquire) {
        let key = pressed & KEYS;
        feed_konami_key(key);
        return;
    }

    // Menu is open: latch rising edges for Core 1 `filter_pad` plus smooth directional auto-repeat.
    let mut latch = pressed & MENU_KEYS;
    let dir_held = combined & (UP | DOWN | LEFT | RIGHT);
    let now = unsafe { switch_now_ns() };
    let cur_rep = REPEAT_DIR.load(Ordering::Relaxed);
    if dir_held != 0 && dir_held.is_power_of_two() {
        if dir_held != cur_rep {
            REPEAT_DIR.store(dir_held, Ordering::Relaxed);
            REPEAT_NEXT_NS.store(now.saturating_add(320_000_000), Ordering::Relaxed);
        } else if now >= REPEAT_NEXT_NS.load(Ordering::Relaxed) {
            latch |= dir_held;
            REPEAT_NEXT_NS.store(now.saturating_add(85_000_000), Ordering::Relaxed);
        }
    } else if cur_rep != 0 {
        REPEAT_DIR.store(0, Ordering::Relaxed);
    }

    if latch != 0 {
        MENU_LATCH.fetch_or(latch, Ordering::Release);
    }
}

/// Runs on controller 1's state (12 bytes at `status`) before the game reads it.
/// Returns `true` when the debug menu took the input (the pad state was cleared).
pub fn filter_pad(c: &mut Ctx, status: u32) -> bool {
    let buttons = c.r16(status) as u32;

    if GOD.load(Ordering::Relaxed) {
        refill_health(c);
    }
    enforce_frozen_enemies(c);

    let clear = |c: &mut Ctx| c.slice_mut(status, 12).fill(0);

    if !OPEN.load(Ordering::Acquire) {
        if SWALLOW.load(Ordering::Relaxed) {
            if buttons == 0 {
                SWALLOW.store(false, Ordering::Relaxed);
            } else {
                clear(c);
                return true;
            }
        }
        return false;
    }

    if NEED_SYNC_OPEN.swap(false, Ordering::Relaxed) {
        let cur_scene = CURRENT_SCENE_ID.load(Ordering::Relaxed).to_be_bytes();
        if let Some(pos) = LEVELS.iter().position(|(id, _)| id.as_bytes() == &cur_scene) {
            LEVEL.store(pos, Ordering::Relaxed);
        }
    }

    update_telemetry(c);

    let pressed = MENU_LATCH.swap(0, Ordering::AcqRel);
    let now = unsafe { switch_now_ns() };
    // 700ms grace period after opening so classic `... B A Start` does not immediately close the menu.
    let can_close = now.saturating_sub(OPENED_AT_NS.load(Ordering::Relaxed)) > 700_000_000;

    let tab = TAB.load(Ordering::Relaxed) % NUM_TABS;
    let items = tab_item_count(tab);
    let cursor = CURSOR.load(Ordering::Relaxed).min(items - 1);

    if pressed & L_BTN != 0 {
        let next_tab = (tab + NUM_TABS - 1) % NUM_TABS;
        TAB.store(next_tab, Ordering::Relaxed);
        CURSOR.store(0, Ordering::Relaxed);
    } else if pressed & R_BTN != 0 {
        let next_tab = (tab + 1) % NUM_TABS;
        TAB.store(next_tab, Ordering::Relaxed);
        CURSOR.store(0, Ordering::Relaxed);
    } else if pressed & UP != 0 {
        CURSOR.store((cursor + items - 1) % items, Ordering::Relaxed);
    } else if pressed & DOWN != 0 {
        CURSOR.store((cursor + 1) % items, Ordering::Relaxed);
    } else if tab == 0 && cursor == 0 && pressed & (LEFT | RIGHT | X | Y) != 0 {
        let n = LEVELS.len();
        let l = LEVEL.load(Ordering::Relaxed) % n;
        let next = if pressed & RIGHT != 0 {
            (l + 1) % n
        } else if pressed & LEFT != 0 {
            (l + n - 1) % n
        } else if pressed & X != 0 {
            (l + 5) % n
        } else {
            (l + n - 5) % n
        };
        LEVEL.store(next, Ordering::Relaxed);
    } else if tab == 1 && cursor == 0 && pressed & (LEFT | RIGHT) != 0 {
        let n = TELEM_NPC_TOTAL.load(Ordering::Relaxed) as usize;
        if n > 0 {
            let s = ENEMY_SLOT.load(Ordering::Relaxed) % n;
            ENEMY_SLOT.store(
                if pressed & RIGHT != 0 { (s + 1) % n } else { (s + n - 1) % n },
                Ordering::Relaxed,
            );
            update_telemetry(c);
        }
    } else if pressed & A != 0 {
        activate(c);
    } else if can_close && pressed & (B | START) != 0 {
        close();
    }

    clear(c);
    true
}
