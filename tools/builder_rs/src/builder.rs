//! Core Switch .NRO and .NSP builder engine.
//! - .NRO: Dynamic RomFS + tssm_engine.nro (works for BOTH USA GGVE78 and UK GGVP78, zero keys!)
//! - .NSP:
//!   * Fast zero-keys path for UK GGVP78 (cryptographically verified via IVFC master hash).
//!   * Universal pure-Rust NCA3 + IVFC + CNMT + PFS0 path for USA GGVE78 (and any ISO) using prod.keys.

use std::env;
use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::crypto::{hex_decode, hex_encode, nintendo_xts_crypt, Sha256, SwitchAesCtr};
use crate::gcm::{parse_iso, GameCubeIso};
use crate::romfs::RomFsPlan;

pub const TITLE_ID: &str = "0100780012340000";
pub const UK_TEMPLATE_ROMFS_SIZE: u64 = 751_669_536;
pub const UK_TEMPLATE_MASTER_HASH_HEX: &str =
    "523d72b6b633ca48fb80083c0548aee472e94ae6f898eae737644c63a1552bee";
const NCA_SECTION_KEY: [u8; 16] = [0x04; 16];
const IVFC_BLOCK_SIZE: usize = 0x4000;

#[derive(Debug, Clone)]
pub struct ProjectPaths {
    pub root: PathBuf,
    pub engine_nro: PathBuf,
    pub nsp_template: PathBuf,
    pub out_dir: PathBuf,
    pub out_nro: PathBuf,
    pub out_nsp_dir: PathBuf,
    pub out_nsp: PathBuf,
    #[allow(dead_code)]
    pub templates_present: bool,
}

impl ProjectPaths {
    pub fn discover() -> Self {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(exe) = env::current_exe() {
            if let Some(mut dir) = exe.parent().map(|p| p.to_path_buf()) {
                for _ in 0..5 {
                    candidates.push(dir.clone());
                    if let Some(parent) = dir.parent() {
                        dir = parent.to_path_buf();
                    } else {
                        break;
                    }
                }
            }
        }
        if let Ok(cwd) = env::current_dir() {
            candidates.push(cwd);
        }

        let found_root = candidates
            .iter()
            .find(|c| c.join("tools").join("switch").join("tssm_engine.nro").is_file())
            .cloned();

        let root = found_root.unwrap_or_else(|| {
            env::current_exe()
                .ok()
                .and_then(|e| e.parent().map(|p| p.to_path_buf()))
                .or_else(|| env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("."))
        });

        let engine_nro = root.join("tools").join("switch").join("tssm_engine.nro");
        let nsp_template = root
            .join("tools")
            .join("switch")
            .join("tssm_nsp_template.bin");
        let out_dir = root.join("target").join("switch");
        let out_nro = out_dir.join("tssm.nro");
        let out_nsp_dir = out_dir.join("nsp");
        let out_nsp = out_nsp_dir.join(format!("{TITLE_ID}.nsp"));
        let templates_present = engine_nro.is_file() && nsp_template.is_file();

        Self {
            root,
            engine_nro,
            nsp_template,
            out_dir,
            out_nro,
            out_nsp_dir,
            out_nsp,
            templates_present,
        }
    }
}

/// Searches the project root and CWD for a valid SpongeBob Movie GameCube ISO/GCM.
pub fn auto_find_iso(root: &Path) -> Option<(PathBuf, GameCubeIso)> {
    let mut dirs = vec![root.to_path_buf()];
    if let Ok(cwd) = env::current_dir() {
        if cwd != root {
            dirs.push(cwd);
        }
    }
    for dir in dirs {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        let mut files: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .and_then(|ext| ext.to_str())
                        .map(|s| s.eq_ignore_ascii_case("iso") || s.eq_ignore_ascii_case("gcm"))
                        .unwrap_or(false)
            })
            .collect();
        files.sort();
        for p in files {
            if let Ok(info) = parse_iso(&p) {
                return Some((p, info));
            }
        }
    }
    None
}

/// Searches standard Switch / emulator locations for `prod.keys`.
pub fn auto_find_prod_keys(root: &Path) -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = vec![
        root.join("prod.keys"),
        root.join("keys.dat"),
    ];
    if let Ok(cwd) = env::current_dir() {
        cands.push(cwd.join("prod.keys"));
    }
    if let Ok(appdata) = env::var("APPDATA") {
        let ap = PathBuf::from(appdata);
        cands.push(ap.join("Ryujinx").join("system").join("prod.keys"));
        cands.push(ap.join("yuzu").join("keys").join("prod.keys"));
        cands.push(ap.join("suyu").join("keys").join("prod.keys"));
        cands.push(ap.join("citron").join("keys").join("prod.keys"));
        cands.push(ap.join("eden").join("keys").join("prod.keys"));
    }
    if let Ok(profile) = env::var("USERPROFILE") {
        cands.push(PathBuf::from(profile).join(".switch").join("prod.keys"));
    }
    if let Ok(home) = env::var("HOME") {
        let h = PathBuf::from(home);
        cands.push(h.join(".config").join("Ryujinx").join("system").join("prod.keys"));
        cands.push(h.join(".switch").join("prod.keys"));
        cands.push(h.join(".local").join("share").join("yuzu").join("keys").join("prod.keys"));
    }

    cands.into_iter().find(|p| p.is_file())
}

/// Parses `header_key` (32 bytes) from a Switch `prod.keys` file.
pub fn parse_header_key(path: &Path) -> Result<[u8; 32], String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("Cannot read prod.keys '{}': {e}", path.display()))?;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if k.trim().eq_ignore_ascii_case("header_key") {
                let mut val = v.trim();
                if val.len() == 66 && val.ends_with("00") {
                    val = &val[..64];
                }
                let bytes = hex_decode(val)
                    .map_err(|e| format!("Malformed header_key in prod.keys: {e}"))?;
                if bytes.len() != 32 {
                    return Err(format!(
                        "Expected 32-byte header_key in prod.keys, got {} bytes.",
                        bytes.len()
                    ));
                }
                let mut out = [0u8; 32];
                out.copy_from_slice(&bytes);
                return Ok(out);
            }
        }
    }
    Err(format!(
        "No 'header_key' entry found in '{}'. Please select a valid Switch prod.keys file.",
        path.display()
    ))
}

#[derive(Debug, Clone)]
pub struct BuildResult {
    pub nro_path: Option<PathBuf>,
    pub nsp_path: Option<PathBuf>,
    pub nsp_skipped_reason: Option<String>,
    pub iso_summary: String,
}

/// Builds standalone Homebrew Switch `.NRO` (`target/switch/tssm.nro`).
/// Works for BOTH USA (`GGVE78`) and UK (`GGVP78`) ISOs with ZERO `prod.keys` required!
pub fn build_nro<P>(
    paths: &ProjectPaths,
    iso_path: &Path,
    mut on_progress: P,
) -> Result<PathBuf, String>
where
    P: FnMut(u32, &str),
{
    if !paths.engine_nro.is_file() {
        return Err(format!(
            "Missing Switch engine template:\n{}\n\n\
             Keep TSSM-Builder inside the downloaded repository folder (do not move the .exe by itself).",
            paths.engine_nro.display()
        ));
    }

    on_progress(2, "Validating GameCube ISO header & filesystem...");
    let iso = parse_iso(iso_path)?;
    on_progress(
        5,
        &format!(
            "Detected {} ({}, {} game files). Planning Switch RomFS...",
            iso.game_id,
            iso.region_label,
            iso.root.total_files()
        ),
    );

    let plan = RomFsPlan::build_for_tssm(&iso.root);
    on_progress(
        8,
        &format!(
            "RomFS planned: {} files, {} dirs ({:.1} MB). Loading engine template...",
            plan.total_files,
            plan.total_dirs,
            plan.total_size as f64 / (1024.0 * 1024.0)
        ),
    );

    let mut engine = fs::read(&paths.engine_nro)
        .map_err(|e| format!("Failed reading '{}': {e}", paths.engine_nro.display()))?;
    if engine.len() < 0x100 {
        return Err("tssm_engine.nro template is corrupted (too small).".into());
    }
    let nro_size = u32::from_le_bytes([engine[0x18], engine[0x19], engine[0x1a], engine[0x1b]]) as usize;
    if nro_size + 56 > engine.len() || &engine[nro_size..nro_size + 4] != b"ASET" {
        return Err("tssm_engine.nro template has invalid NRO0/ASET header.".into());
    }
    // Patch ASET.romfs_size (offset +0x30 within the 56-byte ASET header) for dynamic USA/UK RomFS!
    engine[nro_size + 0x30..nro_size + 0x38].copy_from_slice(&plan.total_size.to_le_bytes());

    fs::create_dir_all(&paths.out_dir)
        .map_err(|e| format!("Cannot create output directory '{}': {e}", paths.out_dir.display()))?;
    let tmp_path = paths.out_nro.with_extension("nro.tmp");
    let out_file = File::create(&tmp_path)
        .map_err(|e| format!("Cannot create '{}': {e}", tmp_path.display()))?;
    let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, out_file);

    on_progress(12, "Writing Switch engine + icon + NACP...");
    writer
        .write_all(&engine)
        .map_err(|e| format!("Write error writing NRO engine: {e}"))?;

    on_progress(15, "Streaming GameCube game assets into tssm.nro RomFS...");
    let mut iso_file = File::open(iso_path)
        .map_err(|e| format!("Cannot reopen ISO '{}': {e}", iso_path.display()))?;

    let stream_res = plan.stream(
        &mut iso_file,
        |chunk| {
            writer
                .write_all(chunk)
                .map_err(|e| format!("Disk write error building tssm.nro: {e}"))
        },
        |done, total| {
            let pct = 15 + ((done as f64 / total.max(1) as f64) * 82.0) as u32;
            on_progress(
                pct.min(97),
                &format!(
                    "Building tssm.nro... {:.0} / {:.0} MB",
                    done as f64 / (1024.0 * 1024.0),
                    total as f64 / (1024.0 * 1024.0)
                ),
            );
        },
    );

    if let Err(e) = stream_res {
        drop(writer);
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }

    writer
        .flush()
        .map_err(|e| format!("Failed flushing tssm.nro: {e}"))?;
    drop(writer);

    if paths.out_nro.exists() {
        let _ = fs::remove_file(&paths.out_nro);
    }
    fs::rename(&tmp_path, &paths.out_nro).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        format!(
            "Failed finalizing '{}': {e}",
            paths.out_nro.display()
        )
    })?;

    on_progress(
        100,
        &format!("Built {} successfully!", paths.out_nro.display()),
    );
    Ok(paths.out_nro.clone())
}

/// Computes IVFC levels 0..4 from level 4 hashes (every 16 KiB block of RomFS).
struct IvfcTree {
    lvl0: Vec<u8>,
    lvl1: Vec<u8>,
    lvl2: Vec<u8>,
    lvl3: Vec<u8>,
    lvl4: Vec<u8>,
    master_hash: [u8; 32],
}

fn align_up_u64(v: u64, align: u64) -> u64 {
    (v + align - 1) & !(align - 1)
}

fn align_up_usize(v: usize, align: usize) -> usize {
    (v + align - 1) & !(align - 1)
}

fn hash_ivfc_level(data: &[u8]) -> Vec<u8> {
    assert!(data.len() % IVFC_BLOCK_SIZE == 0);
    let num_blocks = data.len() / IVFC_BLOCK_SIZE;
    let out_len = align_up_usize(num_blocks * 32, IVFC_BLOCK_SIZE);
    let mut out = vec![0u8; out_len];
    for i in 0..num_blocks {
        let h = Sha256::digest(&data[i * IVFC_BLOCK_SIZE..(i + 1) * IVFC_BLOCK_SIZE]);
        out[i * 32..(i + 1) * 32].copy_from_slice(&h);
    }
    out
}

fn finish_ivfc_from_lvl4(mut lvl4: Vec<u8>) -> Result<IvfcTree, String> {
    let padded_len = align_up_usize(lvl4.len(), IVFC_BLOCK_SIZE);
    lvl4.resize(padded_len, 0);
    let lvl3 = hash_ivfc_level(&lvl4);
    if lvl3.len() != IVFC_BLOCK_SIZE {
        return Err("RomFS exceeds single-block IVFC Level 3 capacity (>4 GiB).".into());
    }
    let lvl2 = hash_ivfc_level(&lvl3);
    let lvl1 = hash_ivfc_level(&lvl2);
    let lvl0 = hash_ivfc_level(&lvl1);
    let master_hash = Sha256::digest(&lvl0);
    Ok(IvfcTree {
        lvl0,
        lvl1,
        lvl2,
        lvl3,
        lvl4,
        master_hash,
    })
}

/// Incrementally hashes incoming bytes in 16 KiB IVFC Level 5 blocks to construct `lvl4`.
struct IvfcBlockHasher {
    buf: Vec<u8>,
    lvl4: Vec<u8>,
}

impl IvfcBlockHasher {
    fn new(romfs_size: u64) -> Self {
        let num_blocks = ((romfs_size + IVFC_BLOCK_SIZE as u64 - 1) / IVFC_BLOCK_SIZE as u64) as usize;
        Self {
            buf: Vec::with_capacity(IVFC_BLOCK_SIZE),
            lvl4: Vec::with_capacity(align_up_usize(num_blocks * 32, IVFC_BLOCK_SIZE)),
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        if !self.buf.is_empty() {
            let take = (IVFC_BLOCK_SIZE - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.buf.len() == IVFC_BLOCK_SIZE {
                let h = Sha256::digest(&self.buf);
                self.lvl4.extend_from_slice(&h);
                self.buf.clear();
            }
        }
        while data.len() >= IVFC_BLOCK_SIZE {
            let h = Sha256::digest(&data[..IVFC_BLOCK_SIZE]);
            self.lvl4.extend_from_slice(&h);
            data = &data[IVFC_BLOCK_SIZE..];
        }
        if !data.is_empty() {
            self.buf.extend_from_slice(data);
        }
    }

    fn finalize(mut self) -> Result<IvfcTree, String> {
        if !self.buf.is_empty() {
            self.buf.resize(IVFC_BLOCK_SIZE, 0);
            let h = Sha256::digest(&self.buf);
            self.lvl4.extend_from_slice(&h);
            self.buf.clear();
        }
        finish_ivfc_from_lvl4(self.lvl4)
    }
}

/// Builds installable Switch Home Menu `.NSP` (`target/switch/nsp/0100780012340000.nsp`).
/// - UK `GGVP78` ISO: Zero `prod.keys` needed (uses pre-signed template, cryptographically verified!).
/// - USA `GGVE78` (or modified) ISO: Dynamically packages NCA3 + IVFC + CNMT + PFS0 using `prod.keys`.
pub fn build_nsp<P>(
    paths: &ProjectPaths,
    iso_path: &Path,
    prod_keys_path: Option<&Path>,
    force_universal: bool,
    mut on_progress: P,
) -> Result<PathBuf, String>
where
    P: FnMut(u32, &str),
{
    if !paths.nsp_template.is_file() {
        return Err(format!(
            "Missing Switch NSP template:\n{}\n\n\
             Keep TSSM-Builder inside the downloaded repository folder.",
            paths.nsp_template.display()
        ));
    }

    on_progress(2, "Validating GameCube ISO header & filesystem...");
    let iso = parse_iso(iso_path)?;
    let plan = RomFsPlan::build_for_tssm(&iso.root);
    on_progress(
        5,
        &format!(
            "Detected {} ({}, {} files, RomFS {:.1} MB).",
            iso.game_id,
            iso.region_label,
            plan.total_files,
            plan.total_size as f64 / (1024.0 * 1024.0)
        ),
    );

    let tpl_len = fs::metadata(&paths.nsp_template)
        .map_err(|e| format!("Cannot stat NSP template: {e}"))?
        .len() as usize;
    if tpl_len < 0x10000 {
        return Err("tssm_nsp_template.bin is corrupted (too small).".into());
    }

    fs::create_dir_all(&paths.out_nsp_dir).map_err(|e| {
        format!(
            "Cannot create NSP output directory '{}': {e}",
            paths.out_nsp_dir.display()
        )
    })?;

    // Fast Zero-Keys Path: UK GGVP78 matching tssm_nsp_template.bin!
    if !force_universal && plan.total_size == UK_TEMPLATE_ROMFS_SIZE {
        on_progress(
            8,
            "UK PAL ISO matches zero-keys NSP template! Encrypting & verifying RomFS...",
        );
        let fast_res = try_build_nsp_zero_keys(paths, iso_path, &plan, &mut on_progress)?;
        if let Some(out) = fast_res {
            return Ok(out);
        }
        on_progress(
            10,
            "ISO content differs from UK template; switching to universal prod.keys NSP packager...",
        );
    }

    // Universal Path: USA GGVE78 or modified ISO -> requires header_key from prod.keys!
    let keys_path = match prod_keys_path {
        Some(p) if p.is_file() => p.to_path_buf(),
        _ => auto_find_prod_keys(&paths.root).ok_or_else(|| {
            format!(
                "Building an installable .NSP for {} ({}) requires your Nintendo Switch prod.keys!\n\n\
                 Good news: .NRO (Homebrew) works with ZERO keys — click 'Build .NRO' to play right away!\n\n\
                 To also build a Home Menu .NSP for this ISO, click 'Browse Keys...' and select your prod.keys\n\
                 (e.g. %APPDATA%\\Ryujinx\\system\\prod.keys or ~/.switch/prod.keys).",
                iso.game_id, iso.region_label
            )
        })?,
    };

    on_progress(
        10,
        &format!("Loading header_key from {}...", keys_path.display()),
    );
    let header_key = parse_header_key(&keys_path)?;

    build_nsp_universal(paths, iso_path, &plan, &header_key, on_progress)
}

/// Single-pass zero-keys NSP builder for standard UK `GGVP78` ISOs.
/// Computes `prefix_len` and `suffix_len` dynamically from `tssm_nsp_template.bin`'s PFS0 header
/// so updates to the Switch ARM64 engine size work automatically.
/// Returns `Ok(Some(path))` if IVFC master hash matches template (100% verified!),
/// or `Ok(None)` if the ISO bytes differ and universal `prod.keys` repacking is needed.
fn try_build_nsp_zero_keys<P>(
    paths: &ProjectPaths,
    iso_path: &Path,
    plan: &RomFsPlan,
    on_progress: &mut P,
) -> Result<Option<PathBuf>, String>
where
    P: FnMut(u32, &str),
{
    let mut tpl_file = File::open(&paths.nsp_template)
        .map_err(|e| format!("Cannot open NSP template: {e}"))?;
    let tpl_len = tpl_file
        .metadata()
        .map_err(|e| format!("Cannot stat NSP template: {e}"))?
        .len() as usize;

    let mut hdr = [0u8; 216];
    tpl_file
        .read_exact(&mut hdr)
        .map_err(|e| format!("Failed reading NSP template PFS0 header: {e}"))?;
    if &hdr[0..4] != b"PFS0" {
        return Err("Invalid tssm_nsp_template.bin (missing PFS0 header).".into());
    }
    let prog_sz = u64::from_le_bytes(hdr[16 + 8..16 + 16].try_into().unwrap());
    let ctrl_sz = u64::from_le_bytes(hdr[16 + 24 + 8..16 + 24 + 16].try_into().unwrap());
    let meta_sz = u64::from_le_bytes(hdr[16 + 48 + 8..16 + 48 + 16].try_into().unwrap());
    let tail_pad = align_up_u64(plan.total_size, IVFC_BLOCK_SIZE as u64) - plan.total_size;
    let suffix_len = (tail_pad + ctrl_sz + meta_sz) as usize;
    if tpl_len <= suffix_len + 216 {
        return Ok(None);
    }
    let prefix_len = tpl_len - suffix_len;
    let romfs_nca_off = (prefix_len - 216) as u64;
    // Verify template PFS0 program.nca size matches prefix + RomFS + tail_pad!
    if romfs_nca_off + plan.total_size + tail_pad != prog_sz || (romfs_nca_off % 16) != 0 {
        return Ok(None);
    }

    tpl_file
        .seek(SeekFrom::Start(0))
        .map_err(|e| format!("Failed seeking NSP template: {e}"))?;
    let mut prefix = vec![0u8; prefix_len];
    tpl_file
        .read_exact(&mut prefix)
        .map_err(|e| format!("Failed reading NSP template prefix: {e}"))?;
    let mut suffix = vec![0u8; suffix_len];
    tpl_file
        .read_exact(&mut suffix)
        .map_err(|e| format!("Failed reading NSP template suffix: {e}"))?;
    drop(tpl_file);

    let tmp_path = paths.out_nsp.with_extension("nsp.tmp");
    let out_file = File::create(&tmp_path)
        .map_err(|e| format!("Cannot create '{}': {e}", tmp_path.display()))?;
    let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, out_file);

    writer
        .write_all(&prefix)
        .map_err(|e| format!("Failed writing NSP prefix: {e}"))?;

    let mut ctr = SwitchAesCtr::new(&NCA_SECTION_KEY, 0, romfs_nca_off);
    let mut ivfc_hasher = IvfcBlockHasher::new(plan.total_size);
    let mut enc_buf: Vec<u8> = Vec::with_capacity(1024 * 1024);

    let mut iso_file = File::open(iso_path)
        .map_err(|e| format!("Cannot open ISO '{}': {e}", iso_path.display()))?;

    let stream_res = plan.stream(
        &mut iso_file,
        |chunk| {
            ivfc_hasher.update(chunk);
            enc_buf.clear();
            enc_buf.extend_from_slice(chunk);
            ctr.apply(&mut enc_buf);
            writer
                .write_all(&enc_buf)
                .map_err(|e| format!("Disk write error building NSP: {e}"))
        },
        |done, total| {
            let pct = 12 + ((done as f64 / total.max(1) as f64) * 82.0) as u32;
            on_progress(
                pct.min(94),
                &format!(
                    "Encrypting & packaging .NSP... {:.0} / {:.0} MB",
                    done as f64 / (1024.0 * 1024.0),
                    total as f64 / (1024.0 * 1024.0)
                ),
            );
        },
    );

    if let Err(e) = stream_res {
        drop(writer);
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }

    writer
        .write_all(&suffix)
        .map_err(|e| format!("Failed writing NSP suffix: {e}"))?;
    writer
        .flush()
        .map_err(|e| format!("Failed flushing NSP: {e}"))?;
    drop(writer);

    on_progress(96, "Verifying IVFC SHA-256 hash tree integrity...");
    let tree = ivfc_hasher.finalize()?;
    if hex_encode(&tree.master_hash) != UK_TEMPLATE_MASTER_HASH_HEX {
        let _ = fs::remove_file(&tmp_path);
        return Ok(None);
    }

    if paths.out_nsp.exists() {
        let _ = fs::remove_file(&paths.out_nsp);
    }
    fs::rename(&tmp_path, &paths.out_nsp).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        format!("Failed finalizing '{}': {e}", paths.out_nsp.display())
    })?;

    on_progress(
        100,
        &format!("Built {} successfully (zero-keys verified)!", paths.out_nsp.display()),
    );
    Ok(Some(paths.out_nsp.clone()))
}

/// Full pure-Rust Switch NCA3 + IVFC + CNMT + PFS0 NSP packager.
/// Works for ANY SpongeBob Movie GameCube ISO (USA `GGVE78`, UK `GGVP78`, or modded).
fn build_nsp_universal<P>(
    paths: &ProjectPaths,
    iso_path: &Path,
    plan: &RomFsPlan,
    header_key: &[u8; 32],
    mut on_progress: P,
) -> Result<PathBuf, String>
where
    P: FnMut(u32, &str),
{
    let tpl_data = fs::read(&paths.nsp_template)
        .map_err(|e| format!("Cannot read NSP template: {e}"))?;

    // Parse template PFS0 header to locate control.nca & meta.cnmt.nca dynamically.
    if tpl_data.len() < 216 + 0xC00 || &tpl_data[0..4] != b"PFS0" {
        return Err("Invalid tssm_nsp_template.bin (missing PFS0 header).".into());
    }
    let num_files = u32::from_le_bytes(tpl_data[4..8].try_into().unwrap()) as usize;
    let str_sz = u32::from_le_bytes(tpl_data[8..12].try_into().unwrap()) as usize;
    let pfs_hdr_len = 16 + num_files * 24 + str_sz;
    if num_files != 3 || pfs_hdr_len != 216 {
        return Err(format!(
            "Unexpected template PFS0 layout (files={num_files}, hdr_len={pfs_hdr_len})."
        ));
    }
    let ctrl_size = u64::from_le_bytes(tpl_data[16 + 24 + 8..16 + 24 + 16].try_into().unwrap()) as usize;
    let meta_size = u64::from_le_bytes(tpl_data[16 + 48 + 8..16 + 48 + 16].try_into().unwrap()) as usize;
    if meta_size != 4096 || ctrl_size + meta_size > tpl_data.len() {
        return Err("Corrupt control/meta NCA sizes in tssm_nsp_template.bin.".into());
    }
    let meta_tpl = &tpl_data[tpl_data.len() - meta_size..];
    let ctrl_nca = &tpl_data[tpl_data.len() - meta_size - ctrl_size..tpl_data.len() - meta_size];

    // Decrypt template program.nca header (0xC00) and verify NCA3 magic.
    let mut prog_hdr = tpl_data[pfs_hdr_len..pfs_hdr_len + 0xC00].to_vec();
    nintendo_xts_crypt(header_key, &mut prog_hdr, false);
    if &prog_hdr[0x200..0x204] != b"NCA3" {
        return Err(
            "Your prod.keys 'header_key' failed to decrypt the NCA3 header!\n\
             Make sure you selected valid retail Switch prod.keys (not title.keys or dev.keys)."
                .into(),
        );
    }

    // Section 0 (ExeFS: main + main.npdm) is already encrypted inside template!
    let sec0_start = u32::from_le_bytes(prog_hdr[0x240..0x244].try_into().unwrap()) as usize * 0x200;
    let sec0_end = u32::from_le_bytes(prog_hdr[0x244..0x248].try_into().unwrap()) as usize * 0x200;
    if sec0_start != 0xC00 || pfs_hdr_len + sec0_end > tpl_data.len() {
        return Err("Corrupt Section 0 bounds in tssm_nsp_template.bin.".into());
    }
    let sec0_enc = &tpl_data[pfs_hdr_len + sec0_start..pfs_hdr_len + sec0_end];
    let sec1_start = sec0_end as u64;

    // Pass 1: Compute IVFC hash tree (Levels 4 -> 0 -> master_hash) over the dynamic RomFS!
    on_progress(12, "Pass 1/2: Computing IVFC SHA-256 hash tree for RomFS...");
    let mut iso_file = File::open(iso_path)
        .map_err(|e| format!("Cannot open ISO '{}': {e}", iso_path.display()))?;
    let mut ivfc_hasher = IvfcBlockHasher::new(plan.total_size);
    plan.stream(
        &mut iso_file,
        |chunk| {
            ivfc_hasher.update(chunk);
            Ok(())
        },
        |done, total| {
            let pct = 12 + ((done as f64 / total.max(1) as f64) * 35.0) as u32;
            on_progress(
                pct.min(47),
                &format!(
                    "Pass 1/2: Hashing RomFS IVFC... {:.0} / {:.0} MB",
                    done as f64 / (1024.0 * 1024.0),
                    total as f64 / (1024.0 * 1024.0)
                ),
            );
        },
    )?;
    let tree = ivfc_hasher.finalize()?;

    // Layout Section 1 (IVFC Levels 0..5)
    let lvl4_off: u64 = 0x10000;
    let lvl4_sz = tree.lvl4.len() as u64;
    let lvl5_off = lvl4_off + lvl4_sz;
    let lvl5_padded_sz = align_up_u64(plan.total_size, IVFC_BLOCK_SIZE as u64);
    let sec1_total_sz = lvl5_off + lvl5_padded_sz;
    let prog_nca_size = sec1_start + sec1_total_sz;
    assert_eq!(prog_nca_size % 0x200, 0);

    // Patch decrypted program.nca header!
    prog_hdr[0x208..0x210].copy_from_slice(&prog_nca_size.to_le_bytes());
    prog_hdr[0x250..0x254].copy_from_slice(&((sec1_start / 0x200) as u32).to_le_bytes());
    prog_hdr[0x254..0x258].copy_from_slice(&((prog_nca_size / 0x200) as u32).to_le_bytes());

    {
        let fs1 = &mut prog_hdr[0x600..0x800];
        // IVFC level entries start at fs1[0x18], 24 bytes each: (off: u64, sz: u64, blk_log2: u32, res: u32)
        let write_lvl = |fs: &mut [u8], idx: usize, off: u64, sz: u64| {
            let base = 0x18 + idx * 24;
            fs[base..base + 8].copy_from_slice(&off.to_le_bytes());
            fs[base + 8..base + 16].copy_from_slice(&sz.to_le_bytes());
            fs[base + 16..base + 20].copy_from_slice(&14u32.to_le_bytes());
            fs[base + 20..base + 24].copy_from_slice(&0u32.to_le_bytes());
        };
        write_lvl(fs1, 0, 0x0000, 0x4000);
        write_lvl(fs1, 1, 0x4000, 0x4000);
        write_lvl(fs1, 2, 0x8000, 0x4000);
        write_lvl(fs1, 3, 0xC000, 0x4000);
        write_lvl(fs1, 4, lvl4_off, lvl4_sz);
        write_lvl(fs1, 5, lvl5_off, plan.total_size);
        fs1[0xC8..0xE8].copy_from_slice(&tree.master_hash);
    }
    let fs1_hash = Sha256::digest(&prog_hdr[0x600..0x800]);
    prog_hdr[0x2A0..0x2C0].copy_from_slice(&fs1_hash);
    nintendo_xts_crypt(header_key, &mut prog_hdr, true);

    // Pass 2: Write PFS0 placeholder + encrypted program.nca (hashing every byte) + control.nca + meta.cnmt.nca!
    on_progress(50, "Pass 2/2: Encrypting & writing program.nca...");
    let tmp_path = paths.out_nsp.with_extension("nsp.tmp");
    let out_file = File::create(&tmp_path)
        .map_err(|e| format!("Cannot create '{}': {e}", tmp_path.display()))?;
    let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, out_file);

    // Placeholder 216-byte PFS0 header (rewritten at end once NCA hashes/IDs are known).
    writer
        .write_all(&[0u8; 216])
        .map_err(|e| format!("Write error: {e}"))?;

    let mut prog_sha = Sha256::new();
    let write_prog = |writer: &mut BufWriter<File>, sha: &mut Sha256, data: &[u8]| -> Result<(), String> {
        sha.update(data);
        writer
            .write_all(data)
            .map_err(|e| format!("Disk write error writing program.nca: {e}"))
    };

    write_prog(&mut writer, &mut prog_sha, &prog_hdr)?;
    write_prog(&mut writer, &mut prog_sha, sec0_enc)?;

    // Encrypt & write IVFC Levels 0..4
    let mut ctr = SwitchAesCtr::new(&NCA_SECTION_KEY, 0, sec1_start);
    for lvl in [&tree.lvl0, &tree.lvl1, &tree.lvl2, &tree.lvl3, &tree.lvl4] {
        let mut buf = lvl.clone();
        ctr.apply(&mut buf);
        write_prog(&mut writer, &mut prog_sha, &buf)?;
    }

    // Encrypt & stream IVFC Level 5 (RomFS)
    let mut enc_buf: Vec<u8> = Vec::with_capacity(1024 * 1024);
    let stream_res = plan.stream(
        &mut iso_file,
        |chunk| {
            enc_buf.clear();
            enc_buf.extend_from_slice(chunk);
            ctr.apply(&mut enc_buf);
            prog_sha.update(&enc_buf);
            writer
                .write_all(&enc_buf)
                .map_err(|e| format!("Disk write error writing NSP RomFS: {e}"))
        },
        |done, total| {
            let pct = 52 + ((done as f64 / total.max(1) as f64) * 42.0) as u32;
            on_progress(
                pct.min(94),
                &format!(
                    "Pass 2/2: Encrypting & writing .NSP... {:.0} / {:.0} MB",
                    done as f64 / (1024.0 * 1024.0),
                    total as f64 / (1024.0 * 1024.0)
                ),
            );
        },
    );
    if let Err(e) = stream_res {
        drop(writer);
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }

    // Encrypt & write trailing zero padding up to 16 KiB IVFC block boundary
    let tail_pad_len = (lvl5_padded_sz - plan.total_size) as usize;
    if tail_pad_len > 0 {
        let mut pad = vec![0u8; tail_pad_len];
        ctr.apply(&mut pad);
        write_prog(&mut writer, &mut prog_sha, &pad)?;
    }

    let prog_hash = prog_sha.finalize();
    let prog_id = &prog_hash[..16];

    // Write Control NCA verbatim & compute its NCA ID
    on_progress(96, "Finalizing control.nca, meta.cnmt.nca, and NSP PFS0 header...");
    let ctrl_hash = Sha256::digest(ctrl_nca);
    let ctrl_id = &ctrl_hash[..16];
    writer
        .write_all(ctrl_nca)
        .map_err(|e| format!("Write error writing control.nca: {e}"))?;

    // Build updated meta.cnmt.nca (4096 bytes)
    let mut meta_hdr = meta_tpl[0..0xC00].to_vec();
    let mut meta_sec0 = meta_tpl[0xC00..0x1000].to_vec();
    nintendo_xts_crypt(header_key, &mut meta_hdr, false);
    if &meta_hdr[0x200..0x204] != b"NCA3" {
        return Err("Failed decrypting meta.cnmt.nca header.".into());
    }
    let mut meta_ctr = SwitchAesCtr::new(&NCA_SECTION_KEY, 0, 0xC00);
    meta_ctr.apply(&mut meta_sec0);

    // Patch PackagedContentInfo #0 (Program NCA) inside Application_0100780012340000.cnmt!
    // Inside meta_sec0: PFS0 starts at 0x200 (length 0x128), CNMT data starts at 0x268 (length 0xC0).
    let cnmt_off = 0x268;
    meta_sec0[cnmt_off + 0x30..cnmt_off + 0x50].copy_from_slice(&prog_hash);
    meta_sec0[cnmt_off + 0x50..cnmt_off + 0x60].copy_from_slice(prog_id);
    meta_sec0[cnmt_off + 0x60..cnmt_off + 0x66].copy_from_slice(&prog_nca_size.to_le_bytes()[..6]);
    // Also refresh Control NCA info just in case
    meta_sec0[cnmt_off + 0x68..cnmt_off + 0x88].copy_from_slice(&ctrl_hash);
    meta_sec0[cnmt_off + 0x88..cnmt_off + 0x98].copy_from_slice(ctrl_id);
    meta_sec0[cnmt_off + 0x98..cnmt_off + 0x9E]
        .copy_from_slice(&(ctrl_nca.len() as u64).to_le_bytes()[..6]);

    // Re-hash meta PFS0 -> hash table -> fs0 master hash -> section[0] hash
    let meta_pfs_hash = Sha256::digest(&meta_sec0[0x200..0x328]);
    meta_sec0[0x00..0x20].copy_from_slice(&meta_pfs_hash);
    let meta_master_hash = Sha256::digest(&meta_sec0[0x00..0x20]);
    meta_hdr[0x408..0x428].copy_from_slice(&meta_master_hash);
    let meta_fs0_hash = Sha256::digest(&meta_hdr[0x400..0x600]);
    meta_hdr[0x280..0x2A0].copy_from_slice(&meta_fs0_hash);

    // Re-encrypt meta_sec0 & meta_hdr
    let mut meta_ctr_enc = SwitchAesCtr::new(&NCA_SECTION_KEY, 0, 0xC00);
    meta_ctr_enc.apply(&mut meta_sec0);
    nintendo_xts_crypt(header_key, &mut meta_hdr, true);

    let mut meta_full = Vec::with_capacity(4096);
    meta_full.extend_from_slice(&meta_hdr);
    meta_full.extend_from_slice(&meta_sec0);
    let meta_hash = Sha256::digest(&meta_full);
    let meta_id = &meta_hash[..16];

    writer
        .write_all(&meta_full)
        .map_err(|e| format!("Write error writing meta.cnmt.nca: {e}"))?;

    // Construct final 216-byte NSP PFS0 header
    let prog_name = format!("{}.nca\0", hex_encode(prog_id));
    let ctrl_name = format!("{}.nca\0", hex_encode(ctrl_id));
    let meta_name = format!("{}.cnmt.nca\0", hex_encode(meta_id));
    let mut str_table = [0u8; 128];
    let n0 = 0usize;
    let n1 = prog_name.len();
    let n2 = n1 + ctrl_name.len();
    str_table[n0..n1].copy_from_slice(prog_name.as_bytes());
    str_table[n1..n2].copy_from_slice(ctrl_name.as_bytes());
    str_table[n2..n2 + meta_name.len()].copy_from_slice(meta_name.as_bytes());

    let mut pfs0 = [0u8; 216];
    pfs0[0..4].copy_from_slice(b"PFS0");
    pfs0[4..8].copy_from_slice(&3u32.to_le_bytes());
    pfs0[8..12].copy_from_slice(&128u32.to_le_bytes());
    pfs0[12..16].copy_from_slice(&0u32.to_le_bytes());

    let entries: [(u64, u64, u32); 3] = [
        (0, prog_nca_size, n0 as u32),
        (prog_nca_size, ctrl_nca.len() as u64, n1 as u32),
        (
            prog_nca_size + ctrl_nca.len() as u64,
            meta_full.len() as u64,
            n2 as u32,
        ),
    ];
    for (i, (off, sz, noff)) in entries.iter().enumerate() {
        let b = 16 + i * 24;
        pfs0[b..b + 8].copy_from_slice(&off.to_le_bytes());
        pfs0[b + 8..b + 16].copy_from_slice(&sz.to_le_bytes());
        pfs0[b + 16..b + 20].copy_from_slice(&noff.to_le_bytes());
        pfs0[b + 20..b + 24].copy_from_slice(&0u32.to_le_bytes());
    }
    pfs0[16 + 72..216].copy_from_slice(&str_table);

    writer
        .flush()
        .map_err(|e| format!("Flush error before patching PFS0 header: {e}"))?;
    let mut raw_file = writer
        .into_inner()
        .map_err(|e| format!("Failed finalizing file buffer: {e}"))?;
    raw_file
        .seek(SeekFrom::Start(0))
        .map_err(|e| format!("Failed seeking to NSP header: {e}"))?;
    raw_file
        .write_all(&pfs0)
        .map_err(|e| format!("Failed writing final PFS0 header: {e}"))?;
    raw_file
        .flush()
        .map_err(|e| format!("Failed flushing final NSP: {e}"))?;
    drop(raw_file);

    if paths.out_nsp.exists() {
        let _ = fs::remove_file(&paths.out_nsp);
    }
    fs::rename(&tmp_path, &paths.out_nsp).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        format!("Failed finalizing '{}': {e}", paths.out_nsp.display())
    })?;

    on_progress(
        100,
        &format!("Built {} successfully!", paths.out_nsp.display()),
    );
    Ok(paths.out_nsp.clone())
}

/// Builds both `.NRO` and `.NSP`. If USA ISO has no `prod.keys`, `.NRO` still succeeds and `nsp_skipped_reason` explains how to also build `.NSP`.
pub fn build_both<P>(
    paths: &ProjectPaths,
    iso_path: &Path,
    prod_keys_path: Option<&Path>,
    allow_skip_nsp_without_keys: bool,
    mut on_progress: P,
) -> Result<BuildResult, String>
where
    P: FnMut(u32, &str),
{
    let iso = parse_iso(iso_path)?;
    let iso_summary = format!("{} ({})", iso.game_id, iso.region_label);

    let nro_path = build_nro(paths, iso_path, |p, msg| {
        on_progress(p / 2, msg);
    })?;

    let mut nsp_skipped_reason = None;
    let nsp_path = match build_nsp(paths, iso_path, prod_keys_path, false, |p, msg| {
        on_progress(50 + p / 2, msg);
    }) {
        Ok(p) => Some(p),
        Err(e) if allow_skip_nsp_without_keys && e.contains("prod.keys") => {
            nsp_skipped_reason = Some(e);
            on_progress(
                100,
                "Built tssm.nro! (Skipped .NSP because USA ISO needs prod.keys)",
            );
            None
        }
        Err(e) => return Err(e),
    };

    Ok(BuildResult {
        nro_path: Some(nro_path),
        nsp_path,
        nsp_skipped_reason,
        iso_summary,
    })
}
