//! Pure-Rust Nintendo Switch RomFS planner and streaming writer.
//! Produces bit-for-bit identical RomFS images to devkitPro `elf2nro` and `hacbrewpack`.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use crate::gcm::{IsoDir, IsoFile};

pub const ROMFS_EMPTY: u32 = 0xFFFF_FFFF;
pub const ROMFS_DATA_OFFSET: u64 = 0x200;

#[derive(Debug, Clone)]
pub enum FileSource {
    Embedded(Vec<u8>),
    Iso { offset: u64, size: u64 },
}

impl FileSource {
    pub fn size(&self) -> u64 {
        match self {
            FileSource::Embedded(v) => v.len() as u64,
            FileSource::Iso { size, .. } => *size,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RomFsDataItem {
    pub data_off: u64, // relative to ROMFS_DATA_OFFSET (0x200)
    pub source: FileSource,
}

pub struct RomFsPlan {
    pub header_padded: Vec<u8>, // 0x200 bytes (0x50 header + zero pad)
    pub items: Vec<RomFsDataItem>,
    pub data_end_rel: u64,      // relative to 0x200
    pub tail_tables: Vec<u8>,   // 0..3 align pad + dir_hash + dir_meta + file_hash + file_meta
    pub total_size: u64,
    pub total_files: usize,
    pub total_dirs: usize,
}

#[derive(Clone)]
struct VDir {
    name: String,
    subdirs: Vec<VDir>,
    files: Vec<(String, FileSource)>,
}

fn align_up_u64(val: u64, align: u64) -> u64 {
    (val + align - 1) & !(align - 1)
}

fn align_up_usize(val: usize, align: usize) -> usize {
    (val + align - 1) & !(align - 1)
}

pub fn romfs_hash_table_buckets(num_entries: usize) -> usize {
    if num_entries < 3 {
        3
    } else if num_entries < 19 {
        num_entries | 1
    } else {
        let mut count = num_entries;
        while count % 2 == 0
            || count % 3 == 0
            || count % 5 == 0
            || count % 7 == 0
            || count % 11 == 0
            || count % 13 == 0
        {
            count += 1;
        }
        count
    }
}

pub fn romfs_path_hash(parent_off: u32, name: &[u8]) -> u32 {
    let mut hash = parent_off ^ 123456789u32;
    for &b in name {
        hash = hash.rotate_right(5) ^ (b as u32);
    }
    hash
}

fn iso_dir_to_vdir(dir: &IsoDir) -> VDir {
    VDir {
        name: dir.name.clone(),
        subdirs: dir.subdirs.iter().map(iso_dir_to_vdir).collect(),
        files: dir
            .files
            .iter()
            .map(|f: &IsoFile| {
                (
                    f.name.clone(),
                    FileSource::Iso {
                        offset: f.offset,
                        size: f.size,
                    },
                )
            })
            .collect(),
    }
}

struct DirRecord {
    meta_off: u32,
    parent_off: u32,
    next_sib: u32,
    first_child: u32,
    first_file: u32,
    hash_next: u32,
    name: String,
}

struct FileRecord {
    meta_off: u32,
    parent_off: u32,
    next_sib: u32,
    data_off: u64,
    data_size: u64,
    hash_next: u32,
    name: String,
}

impl RomFsPlan {
    /// Builds the Switch RomFS layout for The SpongeBob SquarePants Movie from any parsed GameCube ISO.
    pub fn build_for_tssm(iso_root: &IsoDir) -> Self {
        let mut files_vdir = iso_dir_to_vdir(iso_root);
        files_vdir.name = "files".to_string();

        let config_vdir = VDir {
            name: "config".to_string(),
            subdirs: Vec::new(),
            files: vec![(
                "fps_counter.txt".to_string(),
                FileSource::Embedded(b"1\n".to_vec()),
            )],
        };

        let root_vdir = VDir {
            name: String::new(),
            subdirs: vec![config_vdir, files_vdir],
            files: vec![(
                "boot.txt".to_string(),
                FileSource::Embedded(b"TSSM Switch RomFS Root\n".to_vec()),
            )],
        };

        Self::from_vtree(&root_vdir)
    }

    fn from_vtree(root: &VDir) -> Self {
        let mut dir_records: Vec<DirRecord> = Vec::new();

        // Pass 1: Assign directory metadata offsets & parent/child/sibling links.
        // Direct subdirs of a directory are allocated before recursing (matching elf2nro / hacbrewpack).
        fn collect_dirs_bfs_children(
            node: &VDir,
            my_idx: usize,
            dir_records: &mut Vec<DirRecord>,
            cursor: &mut u32,
        ) {
            let my_off = dir_records[my_idx].meta_off;
            let mut subdirs: Vec<&VDir> = node.subdirs.iter().collect();
            subdirs.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));

            let mut child_pairs: Vec<(usize, &VDir)> = Vec::with_capacity(subdirs.len());
            let mut prev_child_idx: Option<usize> = None;
            for sd in subdirs {
                let child_idx = dir_records.len();
                let child_off = *cursor;
                let entry_sz = 24 + align_up_usize(sd.name.len(), 4);
                *cursor += entry_sz as u32;
                dir_records.push(DirRecord {
                    meta_off: child_off,
                    parent_off: my_off,
                    next_sib: ROMFS_EMPTY,
                    first_child: ROMFS_EMPTY,
                    first_file: ROMFS_EMPTY,
                    hash_next: ROMFS_EMPTY,
                    name: sd.name.clone(),
                });
                if let Some(prev) = prev_child_idx {
                    dir_records[prev].next_sib = child_off;
                } else {
                    dir_records[my_idx].first_child = child_off;
                }
                prev_child_idx = Some(child_idx);
                child_pairs.push((child_idx, sd));
            }

            for (child_idx, sd) in child_pairs {
                collect_dirs_bfs_children(sd, child_idx, dir_records, cursor);
            }
        }

        dir_records.push(DirRecord {
            meta_off: 0,
            parent_off: 0,
            next_sib: ROMFS_EMPTY,
            first_child: ROMFS_EMPTY,
            first_file: ROMFS_EMPTY,
            hash_next: ROMFS_EMPTY,
            name: root.name.clone(),
        });
        let mut dir_meta_cursor: u32 = 24 + align_up_usize(root.name.len(), 4) as u32;
        collect_dirs_bfs_children(root, 0, &mut dir_records, &mut dir_meta_cursor);

        // Build lookup map (parent_off, child_name) -> dir_records index
        let find_child_dir_idx = |dir_records: &[DirRecord], parent_off: u32, name: &str| -> usize {
            dir_records
                .iter()
                .position(|d| d.meta_off != 0 && d.parent_off == parent_off && d.name == name)
                .expect("child dir must exist in dir_records")
        };

        // Pass 2: Walk merged sorted entries (files + subdirs by UTF-8 byte order) to lay out
        // file metadata and 16-byte-aligned file data identically to elf2nro / hacbrewpack.
        let mut file_records: Vec<FileRecord> = Vec::new();
        let mut items: Vec<RomFsDataItem> = Vec::new();
        let mut file_meta_cursor: u32 = 0;
        let mut data_cursor: u64 = 0;

        enum EntryRef<'a> {
            Dir(&'a VDir),
            File(&'a str, &'a FileSource),
        }
        impl<'a> EntryRef<'a> {
            fn name(&self) -> &'a str {
                match self {
                    EntryRef::Dir(d) => &d.name,
                    EntryRef::File(n, _) => n,
                }
            }
        }

        #[allow(clippy::too_many_arguments)]
        fn collect_files(
            node: &VDir,
            dir_idx: usize,
            dir_records: &mut [DirRecord],
            file_records: &mut Vec<FileRecord>,
            items: &mut Vec<RomFsDataItem>,
            file_meta_cursor: &mut u32,
            data_cursor: &mut u64,
            find_child_dir_idx: &dyn Fn(&[DirRecord], u32, &str) -> usize,
        ) {
            let my_off = dir_records[dir_idx].meta_off;
            let mut entries: Vec<EntryRef> =
                Vec::with_capacity(node.subdirs.len() + node.files.len());
            for sd in &node.subdirs {
                entries.push(EntryRef::Dir(sd));
            }
            for (name, src) in &node.files {
                entries.push(EntryRef::File(name.as_str(), src));
            }
            entries.sort_by(|a, b| a.name().as_bytes().cmp(b.name().as_bytes()));

            let mut prev_file_idx: Option<usize> = None;
            for entry in entries {
                match entry {
                    EntryRef::File(name, src) => {
                        let f_idx = file_records.len();
                        let f_off = *file_meta_cursor;
                        let entry_sz = 32 + align_up_usize(name.len(), 4);
                        *file_meta_cursor += entry_sz as u32;

                        *data_cursor = align_up_u64(*data_cursor, 0x10);
                        let d_off = *data_cursor;
                        let d_sz = src.size();
                        *data_cursor += d_sz;

                        file_records.push(FileRecord {
                            meta_off: f_off,
                            parent_off: my_off,
                            next_sib: ROMFS_EMPTY,
                            data_off: d_off,
                            data_size: d_sz,
                            hash_next: ROMFS_EMPTY,
                            name: name.to_string(),
                        });
                        items.push(RomFsDataItem {
                            data_off: d_off,
                            source: src.clone(),
                        });

                        if let Some(prev) = prev_file_idx {
                            file_records[prev].next_sib = f_off;
                        } else {
                            dir_records[dir_idx].first_file = f_off;
                        }
                        prev_file_idx = Some(f_idx);
                    }
                    EntryRef::Dir(sd) => {
                        let child_idx = find_child_dir_idx(dir_records, my_off, &sd.name);
                        collect_files(
                            sd,
                            child_idx,
                            dir_records,
                            file_records,
                            items,
                            file_meta_cursor,
                            data_cursor,
                            find_child_dir_idx,
                        );
                    }
                }
            }
        }

        collect_files(
            root,
            0,
            &mut dir_records,
            &mut file_records,
            &mut items,
            &mut file_meta_cursor,
            &mut data_cursor,
            &find_child_dir_idx,
        );

        // Pass 3: Populate directory & file hash tables (sequential head-insert).
        let dir_buckets = romfs_hash_table_buckets(dir_records.len());
        let mut dir_hash_table = vec![ROMFS_EMPTY; dir_buckets];
        for d in &mut dir_records {
            let h = romfs_path_hash(d.parent_off, d.name.as_bytes());
            let b = (h as usize) % dir_buckets;
            d.hash_next = dir_hash_table[b];
            dir_hash_table[b] = d.meta_off;
        }

        let file_buckets = romfs_hash_table_buckets(file_records.len());
        let mut file_hash_table = vec![ROMFS_EMPTY; file_buckets];
        for f in &mut file_records {
            let h = romfs_path_hash(f.parent_off, f.name.as_bytes());
            let b = (h as usize) % file_buckets;
            f.hash_next = file_hash_table[b];
            file_hash_table[b] = f.meta_off;
        }

        // Compute table offsets & serialize tail_tables.
        let data_end_abs = ROMFS_DATA_OFFSET + data_cursor;
        let dir_hash_off = align_up_u64(data_end_abs, 4);
        let pad_before_tables = (dir_hash_off - data_end_abs) as usize;

        let dir_hash_sz = (dir_buckets * 4) as u64;
        let dir_meta_off = dir_hash_off + dir_hash_sz;
        let dir_meta_sz = dir_meta_cursor as u64;

        let file_hash_off = dir_meta_off + dir_meta_sz;
        let file_hash_sz = (file_buckets * 4) as u64;
        let file_meta_off = file_hash_off + file_hash_sz;
        let file_meta_sz = file_meta_cursor as u64;

        let total_size = file_meta_off + file_meta_sz;

        let mut header_padded = vec![0u8; ROMFS_DATA_OFFSET as usize];
        let hdr_vals: [u64; 10] = [
            0x50,
            dir_hash_off,
            dir_hash_sz,
            dir_meta_off,
            dir_meta_sz,
            file_hash_off,
            file_hash_sz,
            file_meta_off,
            file_meta_sz,
            ROMFS_DATA_OFFSET,
        ];
        for (i, v) in hdr_vals.iter().enumerate() {
            header_padded[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
        }

        let tail_len = pad_before_tables + (total_size - dir_hash_off) as usize;
        let mut tail_tables = Vec::with_capacity(tail_len);
        tail_tables.resize(pad_before_tables, 0);

        for &entry in &dir_hash_table {
            tail_tables.extend_from_slice(&entry.to_le_bytes());
        }
        for d in &dir_records {
            tail_tables.extend_from_slice(&d.parent_off.to_le_bytes());
            tail_tables.extend_from_slice(&d.next_sib.to_le_bytes());
            tail_tables.extend_from_slice(&d.first_child.to_le_bytes());
            tail_tables.extend_from_slice(&d.first_file.to_le_bytes());
            tail_tables.extend_from_slice(&d.hash_next.to_le_bytes());
            tail_tables.extend_from_slice(&(d.name.len() as u32).to_le_bytes());
            tail_tables.extend_from_slice(d.name.as_bytes());
            let pad = align_up_usize(d.name.len(), 4) - d.name.len();
            tail_tables.resize(tail_tables.len() + pad, 0);
        }
        for &entry in &file_hash_table {
            tail_tables.extend_from_slice(&entry.to_le_bytes());
        }
        for f in &file_records {
            tail_tables.extend_from_slice(&f.parent_off.to_le_bytes());
            tail_tables.extend_from_slice(&f.next_sib.to_le_bytes());
            tail_tables.extend_from_slice(&f.data_off.to_le_bytes());
            tail_tables.extend_from_slice(&f.data_size.to_le_bytes());
            tail_tables.extend_from_slice(&f.hash_next.to_le_bytes());
            tail_tables.extend_from_slice(&(f.name.len() as u32).to_le_bytes());
            tail_tables.extend_from_slice(f.name.as_bytes());
            let pad = align_up_usize(f.name.len(), 4) - f.name.len();
            tail_tables.resize(tail_tables.len() + pad, 0);
        }

        debug_assert_eq!(
            ROMFS_DATA_OFFSET + data_cursor + tail_tables.len() as u64,
            total_size
        );

        Self {
            header_padded,
            items,
            data_end_rel: data_cursor,
            tail_tables,
            total_size,
            total_files: file_records.len(),
            total_dirs: dir_records.len(),
        }
    }

    /// Streams the complete RomFS image byte-for-byte into `sink`, reporting bytes streamed to `on_progress`.
    pub fn stream<F, P>(
        &self,
        iso_file: &mut File,
        mut sink: F,
        mut on_progress: P,
    ) -> Result<(), String>
    where
        F: FnMut(&[u8]) -> Result<(), String>,
        P: FnMut(u64, u64),
    {
        let mut streamed: u64 = 0;
        sink(&self.header_padded)?;
        streamed += self.header_padded.len() as u64;

        let zeros = [0u8; 64];
        let mut cur_rel: u64 = 0;
        let mut buf = vec![0u8; 1024 * 1024]; // 1 MiB I/O buffer
        let mut last_report: u64 = 0;

        for item in &self.items {
            if item.data_off > cur_rel {
                let mut pad = (item.data_off - cur_rel) as usize;
                while pad > 0 {
                    let take = pad.min(zeros.len());
                    sink(&zeros[..take])?;
                    streamed += take as u64;
                    pad -= take;
                }
                cur_rel = item.data_off;
            }

            match &item.source {
                FileSource::Embedded(bytes) => {
                    sink(bytes)?;
                    let sz = bytes.len() as u64;
                    cur_rel += sz;
                    streamed += sz;
                }
                FileSource::Iso { offset, size } => {
                    iso_file
                        .seek(SeekFrom::Start(*offset))
                        .map_err(|e| format!("Failed seeking ISO to 0x{offset:X}: {e}"))?;
                    let mut rem = *size;
                    while rem > 0 {
                        let to_read = (rem as usize).min(buf.len());
                        iso_file
                            .read_exact(&mut buf[..to_read])
                            .map_err(|e| format!("Failed reading ISO data at 0x{offset:X}: {e}"))?;
                        sink(&buf[..to_read])?;
                        rem -= to_read as u64;
                        cur_rel += to_read as u64;
                        streamed += to_read as u64;

                        if streamed - last_report >= 8 * 1024 * 1024 {
                            on_progress(streamed, self.total_size);
                            last_report = streamed;
                        }
                    }
                }
            }
        }

        debug_assert_eq!(cur_rel, self.data_end_rel);
        sink(&self.tail_tables)?;
        streamed += self.tail_tables.len() as u64;
        debug_assert_eq!(streamed, self.total_size);
        on_progress(self.total_size, self.total_size);
        Ok(())
    }
}
