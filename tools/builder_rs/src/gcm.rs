//! GameCube ISO / GCM / NKit parser for The SpongeBob SquarePants Movie (GGVE78 / GGVP78).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct IsoFile {
    pub name: String,
    pub offset: u64,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct IsoDir {
    pub name: String,
    pub subdirs: Vec<IsoDir>,
    pub files: Vec<IsoFile>,
}

impl IsoDir {
    pub fn total_files(&self) -> usize {
        self.files.len() + self.subdirs.iter().map(|d| d.total_files()).sum::<usize>()
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct GameCubeIso {
    pub game_id: String,
    pub revision: u8,
    pub game_title: String,
    pub region_label: String,
    pub file_size: u64,
    pub root: IsoDir,
}

#[allow(dead_code)]
impl GameCubeIso {
    pub fn is_uk_pal(&self) -> bool {
        self.game_id == "GGVP78"
    }

    pub fn is_usa(&self) -> bool {
        self.game_id == "GGVE78"
    }
}

pub fn parse_iso(path: &Path) -> Result<GameCubeIso, String> {
    let mut f = File::open(path)
        .map_err(|e| format!("Cannot open ISO file '{}': {e}", path.display()))?;
    let file_size = f
        .metadata()
        .map_err(|e| format!("Cannot read metadata of '{}': {e}", path.display()))?
        .len();

    if file_size < 0x440 {
        return Err(format!(
            "File '{}' is too small ({} bytes) to be a GameCube ISO.",
            path.display(),
            file_size
        ));
    }

    let mut hdr = [0u8; 0x440];
    f.read_exact(&mut hdr)
        .map_err(|e| format!("Failed reading ISO header: {e}"))?;

    // Give helpful messages for commonly dragged compressed formats.
    if &hdr[0..4] == b"RVZ\x01" || &hdr[0..4] == b"WIA\x01" {
        return Err(
            "This is a compressed Dolphin .rvz/.wia file!\n\
             In Dolphin Emulator: right-click the game -> Convert File -> Format: ISO,\n\
             then select or drag-and-drop the uncompressed .iso file."
                .into(),
        );
    }
    if &hdr[0..4] == b"CISO" || &hdr[0..4] == b"\x01\xc0\x0b\xb1" {
        return Err(
            "This is a compressed .ciso/.gcz image!\n\
             Please convert it to a standard uncompressed .iso in Dolphin first."
                .into(),
        );
    }
    if &hdr[0..4] == b"PK\x03\x04" || &hdr[0..4] == b"7z\xbc\xaf" || &hdr[0..4] == b"Rar!" {
        return Err(
            "This is a compressed archive (.zip / .7z / .rar)!\n\
             Please extract the .iso file from the archive first."
                .into(),
        );
    }

    let gc_magic = u32::from_be_bytes([hdr[0x1c], hdr[0x1d], hdr[0x1e], hdr[0x1f]]);
    if gc_magic != 0xC233_9F3D {
        return Err(format!(
            "Not a valid GameCube ISO/GCM file (expected magic 0xC2339F3D at 0x1C, found 0x{gc_magic:08X}).\n\
             Make sure you selected an uncompressed GameCube .iso or .gcm disc image."
        ));
    }

    let game_id = String::from_utf8_lossy(&hdr[0..6]).trim().to_string();
    let revision = hdr[7];
    let title_end = hdr[0x20..0x60]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(0x40);
    let game_title = String::from_utf8_lossy(&hdr[0x20..0x20 + title_end])
        .trim()
        .to_string();

    let region_label = match game_id.as_str() {
        "GGVE78" => format!("USA (NTSC-U, Rev {revision})"),
        "GGVP78" => format!("UK / Europe (PAL, Rev {revision})"),
        id if id.starts_with("GGV") && id.ends_with("78") => {
            format!("Regional PAL/Other ({id}, Rev {revision})")
        }
        other => {
            return Err(format!(
                "Wrong GameCube game: found Game ID '{other}' ('{game_title}').\n\
                 Expected 'GGVE78' (USA) or 'GGVP78' (UK/PAL) for The SpongeBob SquarePants Movie."
            ));
        }
    };

    let fst_offset = u32::from_be_bytes([hdr[0x424], hdr[0x425], hdr[0x426], hdr[0x427]]) as u64;
    let fst_size = u32::from_be_bytes([hdr[0x428], hdr[0x429], hdr[0x42a], hdr[0x42b]]) as u64;

    if fst_offset == 0 || fst_size < 12 || fst_offset.saturating_add(fst_size) > file_size {
        return Err(format!(
            "Invalid or truncated GameCube FST (offset=0x{fst_offset:X}, size=0x{fst_size:X}, file_size=0x{file_size:X})."
        ));
    }
    if fst_size > 64 * 1024 * 1024 {
        return Err(format!("FST size unreasonably large ({fst_size} bytes)."));
    }

    let mut fst = vec![0u8; fst_size as usize];
    f.seek(SeekFrom::Start(fst_offset))
        .map_err(|e| format!("Failed seeking to FST: {e}"))?;
    f.read_exact(&mut fst)
        .map_err(|e| format!("Failed reading FST: {e}"))?;

    let num_entries = u32::from_be_bytes([fst[8], fst[9], fst[10], fst[11]]) as usize;
    let str_table_off = num_entries
        .checked_mul(12)
        .ok_or_else(|| "FST entry count overflow".to_string())?;
    if num_entries == 0 || str_table_off > fst.len() {
        return Err(format!(
            "Corrupt FST header (entries={num_entries}, fst_size={}).",
            fst.len()
        ));
    }

    let (root, next_idx) = parse_fst_dir(&fst, 0, num_entries, str_table_off, "", file_size)?;
    if next_idx != num_entries {
        return Err(format!(
            "Unexpected FST root end ({next_idx} != {num_entries})."
        ));
    }

    Ok(GameCubeIso {
        game_id,
        revision,
        game_title,
        region_label,
        file_size,
        root,
    })
}

fn read_fst_name(fst: &[u8], str_table_off: usize, name_off: usize) -> Result<String, String> {
    let start = str_table_off
        .checked_add(name_off)
        .ok_or_else(|| "FST string offset overflow".to_string())?;
    if start >= fst.len() {
        return Err(format!("FST string offset out of bounds: {start}"));
    }
    let end = fst[start..]
        .iter()
        .position(|&b| b == 0)
        .map(|rel| start + rel)
        .unwrap_or(fst.len());
    Ok(String::from_utf8_lossy(&fst[start..end]).to_string())
}

fn parse_fst_dir(
    fst: &[u8],
    dir_idx: usize,
    end_idx: usize,
    str_table_off: usize,
    dir_name: &str,
    iso_file_size: u64,
) -> Result<(IsoDir, usize), String> {
    let mut dir = IsoDir {
        name: dir_name.to_string(),
        subdirs: Vec::new(),
        files: Vec::new(),
    };

    let mut idx = dir_idx + 1;
    while idx < end_idx {
        let base = idx * 12;
        let w0 = u32::from_be_bytes([fst[base], fst[base + 1], fst[base + 2], fst[base + 3]]);
        let w1 = u32::from_be_bytes([fst[base + 4], fst[base + 5], fst[base + 6], fst[base + 7]]);
        let w2 = u32::from_be_bytes([fst[base + 8], fst[base + 9], fst[base + 10], fst[base + 11]]);

        let is_dir = (w0 >> 24) != 0;
        let name_off = (w0 & 0x00FF_FFFF) as usize;
        let name = read_fst_name(fst, str_table_off, name_off)?;

        if is_dir {
            let sub_end = w2 as usize;
            if sub_end <= idx || sub_end > end_idx {
                return Err(format!(
                    "Corrupt FST directory '{name}' (idx={idx}, next={sub_end}, end={end_idx})"
                ));
            }
            let (subdir, next) =
                parse_fst_dir(fst, idx, sub_end, str_table_off, &name, iso_file_size)?;
            dir.subdirs.push(subdir);
            idx = next;
        } else {
            let offset = w1 as u64;
            let size = w2 as u64;
            if offset.saturating_add(size) > iso_file_size {
                return Err(format!(
                    "ISO appears truncated: file '{name}' at 0x{offset:X}+0x{size:X} exceeds ISO size 0x{iso_file_size:X}."
                ));
            }
            dir.files.push(IsoFile { name, offset, size });
            idx += 1;
        }
    }

    Ok((dir, end_idx))
}
