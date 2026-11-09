//! Memory card library for the Switch build, ported from crates/tssm/src/card.rs.
//!
//! Slot A holds a formatted "Memory Card 251" whose files live in memory for the session
//! (there is no host file system in this no_std build yet); slot B is empty. The SDK's CARD
//! functions are answered directly. Asynchronous operations finish at once, but report it as
//! the console does: the call returns, the slot reads as busy, and the completion callback
//! runs from the event pump.

use alloc::vec;
use alloc::vec::Vec;

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use runtime::hle::Hle;
use runtime::Ctx;

use crate::{Event, push_event};

const READY: i32 = 0;
const BUSY: i32 = -1;
const NOCARD: i32 = -3;
const NOFILE: i32 = -4;
const EXIST: i32 = -7;
const NOENT: i32 = -8;
const INSSPACE: i32 = -9;
const LIMIT: i32 = -11;
const NAMETOOLONG: i32 = -12;
const FATAL_ERROR: i32 = -128;

const SIZE_MEGABITS: u32 = 16;
const SECTOR: u32 = 8192;
const BLOCKS: u32 = 251;
const MAX_FILES: usize = 127;
const SEGMENT: u32 = 512;
const NAME_LENGTH: usize = 32;
const NONE: u32 = 0xFFFF_FFFF;

const MENU_SLOT: u32 = 0xFFFF_8234;
const MENU_ANSWER: u32 = 0xFFFF_8E30;
const ANSWER_BACK: u32 = 4;
const MENU_DISPATCH: u32 = 0x8010_B3BC;
const EVENT_SLOT_CHOSEN: u32 = 171;
const SLOT_SCREEN_WAIT: u32 = 0x8010_F510;

static ROOM_CHECK_OPENS: AtomicU32 = AtomicU32::new(0);
static SLOT_CHOSEN: AtomicBool = AtomicBool::new(false);

pub fn before_room_check(_c: &mut Ctx) {
    ROOM_CHECK_OPENS.store(3, Ordering::Relaxed);
}

pub fn before_menu(_c: &mut Ctx) {
    SLOT_CHOSEN.store(false, Ordering::Relaxed);
}

pub fn pick_slot_a(c: &mut Ctx) -> bool {
    let small = |c: &Ctx, offset: u32| c.r32(c.r[13].wrapping_add(offset));
    if c.lr != SLOT_SCREEN_WAIT || !present(0) || small(c, MENU_SLOT) != NONE || small(c, MENU_ANSWER) != NONE {
        return false;
    }
    if SLOT_CHOSEN.swap(true, Ordering::Relaxed) {
        c.w32(c.r[13].wrapping_add(MENU_ANSWER), ANSWER_BACK);
        return true;
    }
    let (lr, sp) = (c.lr, c.r[1]);
    c.r[1] = sp - 0x20;
    c.w32(c.r[1], sp);
    c.w32(c.r[1] + 8, 0f32.to_bits());
    c.r[3] = EVENT_SLOT_CHOSEN;
    c.r[4] = c.r[1] + 8;
    c.call(MENU_DISPATCH);
    c.r[1] = sp;
    c.lr = lr;
    true
}

/// The card's 64-byte directory entry (GCI header layout).
const ENTRY: usize = 64;
const E_GAME: usize = 0;
const E_BANNER_FORMAT: usize = 7;
const E_NAME: usize = 8;
const E_TIME: usize = 40;
const E_ICON_ADDR: usize = 44;
const E_ICON_FORMAT: usize = 48;
const E_ICON_SPEED: usize = 50;
const E_PERMISSION: usize = 52;
const E_BLOCKS: usize = 56;
const E_COMMENT_ADDR: usize = 60;

struct File {
    entry: [u8; ENTRY],
    data: Vec<u8>,
}

impl File {
    fn be16(&self, at: usize) -> u16 {
        u16::from_be_bytes([self.entry[at], self.entry[at + 1]])
    }
    fn be32(&self, at: usize) -> u32 {
        u32::from_be_bytes([self.entry[at], self.entry[at + 1], self.entry[at + 2], self.entry[at + 3]])
    }
    fn name(&self) -> &[u8] {
        let name = &self.entry[E_NAME..E_NAME + NAME_LENGTH];
        &name[..name.iter().position(|&b| b == 0).unwrap_or(NAME_LENGTH)]
    }
}

struct Card {
    files: Vec<Option<File>>,
    result: i32,
    transferred: u32,
}

static mut CARD: Option<Card> = None;

fn with<R>(f: impl FnOnce(&mut Card) -> R) -> R {
    // SAFETY: the guest runs on a single thread.
    #[allow(static_mut_refs)]
    let card = unsafe {
        CARD.get_or_insert_with(|| {
            let mut files = Vec::new();
            files.resize_with(MAX_FILES, || None);
            Card { files, result: READY, transferred: 0 }
        })
    };
    f(card)
}

fn present(chan: u32) -> bool {
    chan == 0
}

fn now(c: &Ctx) -> u32 {
    (c.timebase() / 40_500_000) as u32
}

fn done(c: &mut Ctx, result: i32) {
    with(|card| card.result = result);
    c.r[3] = result as u32;
}

fn done_async(c: &mut Ctx, chan: u32, callback: u32, result: i32) {
    if result < 0 {
        return done(c, result);
    }
    with(|card| card.result = BUSY);
    push_event(Event::CardDone { chan, callback, result });
    c.r[3] = READY as u32;
}

/// Called by the event pump as it delivers a completion.
pub fn completed(result: i32) {
    with(|card| card.result = result);
}

fn guest_name(c: &Ctx, addr: u32) -> Vec<u8> {
    (0..=NAME_LENGTH as u32).map(|i| c.r8(addr + i)).take_while(|&b| b != 0).collect()
}

fn find(card: &Card, c: &Ctx, name: &[u8]) -> Option<usize> {
    let game = [0u32, 1, 2, 3, 4, 5].map(|i| c.r8(0x8000_0000 + i));
    card.files
        .iter()
        .position(|file| file.as_ref().is_some_and(|f| f.name() == name && f.entry[E_GAME..E_GAME + 6] == game))
}

fn fill_file_info(c: &mut Ctx, info: u32, chan: u32, number: usize) {
    c.w32(info, chan);
    c.w32(info + 4, number as u32);
    c.w32(info + 8, 0);
    c.w32(info + 0xC, 0);
    c.w16(info + 0x10, 5 + number as u16);
}

fn opened(c: &Ctx, info: u32) -> Result<(u32, usize), i32> {
    let (chan, number) = (c.r32(info), c.r32(info + 4) as usize);
    if !present(chan) {
        return Err(NOCARD);
    }
    if number >= MAX_FILES || with(|card| card.files[number].is_none()) {
        return Err(NOFILE);
    }
    Ok((chan, number))
}

fn probe(c: &mut Ctx) {
    let chan = c.r[3];
    done(c, if present(chan) { READY } else { NOCARD });
}

fn probe_ex(c: &mut Ctx) {
    let (chan, size, sector) = (c.r[3], c.r[4], c.r[5]);
    if !present(chan) {
        c.r[3] = NOCARD as u32;
        return;
    }
    if size != 0 {
        c.w32(size, SIZE_MEGABITS);
    }
    if sector != 0 {
        c.w32(sector, SECTOR);
    }
    c.r[3] = READY as u32;
}

fn mount_async(c: &mut Ctx) {
    let (chan, callback) = (c.r[3], c.r[6]);
    done_async(c, chan, callback, if present(chan) { READY } else { NOCARD });
}

fn check_ex(c: &mut Ctx) {
    let (chan, transferred) = (c.r[3], c.r[4]);
    if present(chan) && transferred != 0 {
        c.w32(transferred, 0);
    }
    done(c, if present(chan) { READY } else { NOCARD });
}

fn check_ex_async(c: &mut Ctx) {
    let (chan, transferred, callback) = (c.r[3], c.r[4], c.r[5]);
    if present(chan) && transferred != 0 {
        c.w32(transferred, 0);
    }
    done_async(c, chan, callback, if present(chan) { READY } else { NOCARD });
}

fn format(chan: u32) -> i32 {
    if !present(chan) {
        return NOCARD;
    }
    with(|card| card.files.iter_mut().for_each(|f| *f = None));
    READY
}

fn used_blocks(card: &Card) -> u32 {
    card.files.iter().flatten().map(|f| f.be16(E_BLOCKS) as u32).sum()
}

fn free_blocks(c: &mut Ctx) {
    ROOM_CHECK_OPENS.store(0, Ordering::Relaxed);
    let (chan, bytes, files) = (c.r[3], c.r[4], c.r[5]);
    if !present(chan) {
        return done(c, NOCARD);
    }
    let (used, count) = with(|card| (used_blocks(card), card.files.iter().flatten().count() as u32));
    if bytes != 0 {
        c.w32(bytes, BLOCKS.saturating_sub(used) * SECTOR);
    }
    if files != 0 {
        c.w32(files, MAX_FILES as u32 - count);
    }
    done(c, READY);
}

fn get_encoding(c: &mut Ctx) {
    let (chan, out) = (c.r[3], c.r[4]);
    if present(chan) && out != 0 {
        c.w16(out, 0);
    }
    c.r[3] = if present(chan) { READY } else { NOCARD } as u32;
}

fn get_sector_size(c: &mut Ctx) {
    let (chan, out) = (c.r[3], c.r[4]);
    if present(chan) && out != 0 {
        c.w32(out, SECTOR);
    }
    c.r[3] = if present(chan) { READY } else { NOCARD } as u32;
}

fn get_result_code(c: &mut Ctx) {
    let chan = c.r[3];
    crate::deliver_events(c);
    c.r[3] = if present(chan) { with(|card| card.result) } else { NOCARD } as u32;
}

fn open(c: &mut Ctx) {
    let (chan, name, info) = (c.r[3], guest_name(c, c.r[4]), c.r[5]);
    c.w32(info, NONE);
    if !present(chan) {
        return done(c, NOCARD);
    }
    let prev = ROOM_CHECK_OPENS.load(Ordering::Relaxed);
    if prev > 0 {
        ROOM_CHECK_OPENS.store(prev - 1, Ordering::Relaxed);
        if prev == 1 {
            return done(c, NOFILE);
        }
    }
    match with(|card| find(card, c, &name)) {
        Some(number) => {
            fill_file_info(c, info, chan, number);
            done(c, READY)
        }
        None => done(c, NOFILE),
    }
}

fn fast_open(c: &mut Ctx) {
    let (chan, number, info) = (c.r[3], c.r[4] as usize, c.r[5]);
    c.w32(info, NONE);
    if !present(chan) {
        return done(c, NOCARD);
    }
    if number >= MAX_FILES {
        return done(c, FATAL_ERROR);
    }
    if with(|card| card.files[number].is_none()) {
        return done(c, NOFILE);
    }
    fill_file_info(c, info, chan, number);
    done(c, READY);
}

fn close(c: &mut Ctx) {
    let info = c.r[3];
    let chan = c.r32(info);
    c.w32(info, NONE);
    c.r[3] = if present(chan) { READY } else { NOCARD } as u32;
}

fn create_async(c: &mut Ctx) {
    let (chan, name, size, info, callback) = (c.r[3], guest_name(c, c.r[4]), c.r[5], c.r[6], c.r[7]);
    if !present(chan) {
        return done(c, NOCARD);
    }
    if name.len() > NAME_LENGTH {
        return done(c, NAMETOOLONG);
    }
    if size == 0 || size % SECTOR != 0 {
        return done(c, FATAL_ERROR);
    }
    let time = now(c);
    let game: [u8; 6] = core::array::from_fn(|i| c.r8(0x8000_0000 + i as u32));
    let result = with(|card| {
        if find(card, c, &name).is_some() {
            return Err(EXIST);
        }
        let number = card.files.iter().position(Option::is_none).ok_or(NOENT)?;
        if used_blocks(card) + size / SECTOR > BLOCKS {
            return Err(INSSPACE);
        }
        let mut entry = [0u8; ENTRY];
        entry[E_GAME..E_GAME + 6].copy_from_slice(&game);
        entry[6] = 0xFF;
        entry[E_NAME..E_NAME + name.len()].copy_from_slice(&name);
        entry[E_TIME..E_TIME + 4].copy_from_slice(&time.to_be_bytes());
        entry[E_ICON_ADDR..E_ICON_ADDR + 4].copy_from_slice(&NONE.to_be_bytes());
        entry[E_PERMISSION] = 4;
        entry[54..56].copy_from_slice(&(5 + number as u16).to_be_bytes());
        entry[E_BLOCKS..E_BLOCKS + 2].copy_from_slice(&((size / SECTOR) as u16).to_be_bytes());
        entry[58..60].fill(0xFF);
        entry[E_COMMENT_ADDR..].copy_from_slice(&NONE.to_be_bytes());
        card.files[number] = Some(File { entry, data: vec![0; size as usize] });
        card.transferred = 0;
        Ok(number)
    });
    match result {
        Ok(number) => {
            fill_file_info(c, info, chan, number);
            done_async(c, chan, callback, READY)
        }
        Err(error) => done(c, error),
    }
}

fn delete_async(c: &mut Ctx) {
    let (chan, name, callback) = (c.r[3], guest_name(c, c.r[4]), c.r[5]);
    if !present(chan) {
        return done(c, NOCARD);
    }
    let result = with(|card| match find(card, c, &name) {
        Some(number) => {
            card.files[number] = None;
            READY
        }
        None => NOFILE,
    });
    done_async(c, chan, callback, result);
}

fn range(number: usize, length: u32, offset: u32, unit: u32) -> Result<core::ops::Range<usize>, i32> {
    if offset % unit != 0 || length % unit != 0 {
        return Err(FATAL_ERROR);
    }
    let size = with(|card| card.files[number].as_ref().map_or(0, |f| f.data.len()));
    let end = offset as usize + length as usize;
    if end > size {
        return Err(LIMIT);
    }
    Ok(offset as usize..end)
}

fn read(c: &mut Ctx) -> (u32, i32) {
    let (info, buffer, length, offset) = (c.r[3], c.r[4], c.r[5], c.r[6]);
    let result = opened(c, info).and_then(|(chan, number)| {
        let range = range(number, length, offset, SEGMENT)?;
        let bytes = with(|card| {
            card.transferred = length;
            card.files[number].as_ref().unwrap().data[range].to_vec()
        });
        c.slice_mut(buffer, length as usize).copy_from_slice(&bytes);
        Ok(chan)
    });
    match result {
        Ok(chan) => (chan, READY),
        Err(error) => (c.r32(info), error),
    }
}

fn write_async(c: &mut Ctx) {
    let (info, buffer, length, offset, callback) = (c.r[3], c.r[4], c.r[5], c.r[6], c.r[7]);
    let time = now(c);
    let result = opened(c, info).and_then(|(chan, number)| {
        let range = range(number, length, offset, SECTOR)?;
        let source = c.read_bytes(buffer, length as usize);
        with(|card| {
            let file = card.files[number].as_mut().unwrap();
            file.data[range].copy_from_slice(&source);
            file.entry[E_TIME..E_TIME + 4].copy_from_slice(&time.to_be_bytes());
            card.transferred = length;
        });
        Ok(chan)
    });
    match result {
        Ok(chan) => done_async(c, chan, callback, READY),
        Err(error) => done(c, error),
    }
}

fn get_status(c: &mut Ctx) {
    let (chan, number, stat) = (c.r[3], c.r[4] as usize, c.r[5]);
    if !present(chan) {
        return done(c, NOCARD);
    }
    if number >= MAX_FILES {
        return done(c, FATAL_ERROR);
    }
    let Some(entry) = with(|card| card.files[number].as_ref().map(|f| f.entry)) else { return done(c, NOFILE) };
    let file = File { entry, data: Vec::new() };
    c.slice_mut(stat, 0x6C).fill(0);
    c.slice_mut(stat, NAME_LENGTH).copy_from_slice(&entry[E_NAME..E_NAME + NAME_LENGTH]);
    c.w32(stat + 0x20, file.be16(E_BLOCKS) as u32 * SECTOR);
    c.w32(stat + 0x24, file.be32(E_TIME));
    c.slice_mut(stat + 0x28, 6).copy_from_slice(&entry[E_GAME..E_GAME + 6]);
    c.w8(stat + 0x2E, entry[E_BANNER_FORMAT]);
    c.w32(stat + 0x30, file.be32(E_ICON_ADDR));
    c.w16(stat + 0x34, file.be16(E_ICON_FORMAT));
    c.w16(stat + 0x36, file.be16(E_ICON_SPEED));
    c.w32(stat + 0x38, file.be32(E_COMMENT_ADDR));
    update_icon_offsets(c, stat);
    done(c, READY);
}

fn update_icon_offsets(c: &mut Ctx, stat: u32) {
    let mut offset = c.r32(stat + 0x30);
    if offset == NONE {
        c.w8(stat + 0x2E, 0);
        c.w16(stat + 0x34, 0);
        c.w16(stat + 0x36, 0);
        offset = 0;
    }
    let (banner, banner_palette) = match c.r8(stat + 0x2E) & 3 {
        1 => {
            let at = offset;
            offset += 96 * 32 + 2 * 256;
            (at, at + 96 * 32)
        }
        2 => {
            let at = offset;
            offset += 2 * 96 * 32;
            (at, NONE)
        }
        _ => (NONE, NONE),
    };
    c.w32(stat + 0x3C, banner);
    c.w32(stat + 0x40, banner_palette);
    let formats = c.r16(stat + 0x34);
    let mut palette = false;
    for i in 0..8 {
        let at = match (formats >> (2 * i)) & 3 {
            1 => {
                palette = true;
                offset += 32 * 32;
                offset - 32 * 32
            }
            2 => {
                offset += 2 * 32 * 32;
                offset - 2 * 32 * 32
            }
            _ => NONE,
        };
        c.w32(stat + 0x44 + 4 * i, at);
    }
    c.w32(stat + 0x64, if palette { offset } else { NONE });
    if palette {
        offset += 2 * 256;
    }
    c.w32(stat + 0x68, offset);
}

fn set_status(c: &mut Ctx) -> i32 {
    let (chan, number, stat) = (c.r[3], c.r[4] as usize, c.r[5]);
    if !present(chan) {
        return NOCARD;
    }
    let (icon, comment) = (c.r32(stat + 0x30), c.r32(stat + 0x38));
    if number >= MAX_FILES || (icon != NONE && icon >= SEGMENT) || (comment != NONE && comment % SECTOR > SECTOR - 64) {
        return FATAL_ERROR;
    }
    update_icon_offsets(c, stat);
    if c.r32(stat + 0x30) == NONE {
        let speed = c.r16(stat + 0x36);
        c.w16(stat + 0x36, speed & !3 | 1);
    }
    let (banner_format, icon_format, icon_speed) = (c.r8(stat + 0x2E), c.r16(stat + 0x34), c.r16(stat + 0x36));
    let time = now(c);
    with(|card| {
        let Some(file) = card.files[number].as_mut() else { return NOFILE };
        file.entry[E_BANNER_FORMAT] = banner_format;
        file.entry[E_ICON_ADDR..E_ICON_ADDR + 4].copy_from_slice(&icon.to_be_bytes());
        file.entry[E_ICON_FORMAT..E_ICON_FORMAT + 2].copy_from_slice(&icon_format.to_be_bytes());
        file.entry[E_ICON_SPEED..E_ICON_SPEED + 2].copy_from_slice(&icon_speed.to_be_bytes());
        file.entry[E_COMMENT_ADDR..].copy_from_slice(&comment.to_be_bytes());
        file.entry[E_TIME..E_TIME + 4].copy_from_slice(&time.to_be_bytes());
        READY
    })
}

fn set_attributes(c: &mut Ctx) {
    let (chan, number, attributes) = (c.r[3], c.r[4] as usize, c.r[5] as u8);
    if !present(chan) {
        return done(c, NOCARD);
    }
    if number >= MAX_FILES {
        return done(c, FATAL_ERROR);
    }
    let result = with(|card| match card.files[number].as_mut() {
        Some(file) => {
            file.entry[E_PERMISSION] = attributes;
            READY
        }
        None => NOFILE,
    });
    done(c, result);
}

pub fn policy(name: &str) -> Hle {
    Hle::Native(match name {
        "CARDInit" => |_| {},
        "CARDMount" | "CARDCheck" | "CARDUnmount" => probe,
        "CARDProbe" => |c| c.r[3] = present(c.r[3]) as u32,
        "CARDProbeEx" => probe_ex,
        "CARDMountAsync" => mount_async,
        "CARDCheckEx" => check_ex,
        "CARDCheckExAsync" => check_ex_async,
        "CARDCheckAsync" => |c| {
            let (chan, callback) = (c.r[3], c.r[4]);
            done_async(c, chan, callback, if present(chan) { READY } else { NOCARD })
        },
        "CARDFormat" => |c| {
            let result = format(c.r[3]);
            done(c, result)
        },
        "CARDFormatAsync" => |c| {
            let (chan, callback) = (c.r[3], c.r[4]);
            done_async(c, chan, callback, format(chan))
        },
        "CARDFreeBlocks" => free_blocks,
        "CARDGetEncoding" => get_encoding,
        "CARDGetSectorSize" => get_sector_size,
        "CARDGetResultCode" => get_result_code,
        "CARDGetXferredBytes" => |c| c.r[3] = if present(c.r[3]) { with(|card| card.transferred) } else { 0 },
        "CARDOpen" => open,
        "CARDFastOpen" => fast_open,
        "CARDClose" => close,
        "CARDCreateAsync" => create_async,
        "CARDDeleteAsync" => delete_async,
        "CARDRead" => |c| {
            let (_, result) = read(c);
            done(c, result)
        },
        "CARDReadAsync" => |c| {
            let callback = c.r[7];
            let (chan, result) = read(c);
            done_async(c, chan, callback, result)
        },
        "CARDWriteAsync" => write_async,
        "CARDGetStatus" => get_status,
        "CARDSetStatus" => |c| {
            let result = set_status(c);
            done(c, result)
        },
        "CARDSetStatusAsync" => |c| {
            let (chan, callback) = (c.r[3], c.r[6]);
            let result = set_status(c);
            done_async(c, chan, callback, result)
        },
        "CARDSetAttributes" => set_attributes,
        // Anything else of the card library is internal to it and never reached.
        _ => |c| c.r[3] = NOCARD as u32,
    })
}
