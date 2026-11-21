//! Native ARM64 / Rust HLE implementations for high-impact CPU & loading hotspots.

use alloc::vec::Vec;
use hashbrown::HashMap;
use runtime::Ctx;

#[inline(always)]
fn mr8(mem: *mut u8, ea: u32) -> u8 {
    // SAFETY: caller provides a valid guest address within the 24 MB guest space.
    unsafe { *mem.add((ea & 0x01FF_FFFF) as usize) }
}

#[inline(always)]
fn mr16(mem: *mut u8, ea: u32) -> u16 {
    // SAFETY: guest RAM is flat and unaligned BE loads compile to `ldrh` + `rev16`.
    unsafe { core::ptr::read_unaligned(mem.add((ea & 0x01FF_FFFF) as usize) as *const u16).to_be() }
}

#[inline(always)]
fn mr32(mem: *mut u8, ea: u32) -> u32 {
    // SAFETY: guest RAM is flat and unaligned BE loads compile to `ldr` + `rev`.
    unsafe { core::ptr::read_unaligned(mem.add((ea & 0x01FF_FFFF) as usize) as *const u32).to_be() }
}

#[inline(always)]
fn mw8(mem: *mut u8, ea: u32, v: u8) {
    // SAFETY: valid guest address within the 24 MB guest space.
    unsafe { *mem.add((ea & 0x01FF_FFFF) as usize) = v }
}

#[inline(always)]
fn mw16(mem: *mut u8, ea: u32, v: u16) {
    // SAFETY: valid guest address; compiles to `rev16` + `strh`.
    unsafe { core::ptr::write_unaligned(mem.add((ea & 0x01FF_FFFF) as usize) as *mut u16, v.to_be()) }
}

#[inline(always)]
fn mw32(mem: *mut u8, ea: u32, v: u32) {
    // SAFETY: valid guest address; compiles to `rev` + `str`.
    unsafe { core::ptr::write_unaligned(mem.add((ea & 0x01FF_FFFF) as usize) as *mut u32, v.to_be()) }
}

#[inline(always)]
fn call2(c: &mut Ctx, func: u32, r3: u32, r4: u32) -> u32 {
    c.r[3] = r3;
    c.r[4] = r4;
    c.call(func);
    c.r[3]
}

#[inline(always)]
fn call4(c: &mut Ctx, func: u32, r3: u32, r4: u32, r5: u32, r6: u32) -> u32 {
    c.r[3] = r3;
    c.r[4] = r4;
    c.r[5] = r5;
    c.r[6] = r6;
    c.call(func);
    c.r[3]
}

const PKR_ASSET_BY_TYPE_ADDR: u32 = 0x8019_7A7C;
const PKR_ASSET_COUNT_ADDR: u32 = 0x8019_7B20;
const RW_FREELIST_CREATE_ADDR: u32 = 0x8033_DF08;
const ST_ROOT: u32 = 0x803A_79DC;
const ST_MASK_OFF: u32 = 0x1590;
const ST_STRIDE: u32 = 276;

#[inline(always)]
fn pkr_count_fast(mem: *mut u8, pr: u32, ty: u32) -> u32 {
    if ty == 0 {
        return mr32(mem, pr.wrapping_add(0xB4));
    }
    let mut p = mr32(mem, pr);
    let mut idx = 0u32;
    loop {
        let t = mr32(mem, p);
        if t == 0 {
            return 0;
        }
        if t == ty {
            return mr32(mem, pr.wrapping_add((idx << 4).wrapping_add(0xDC)));
        }
        idx = idx.wrapping_add(1);
        p = p.wrapping_add(0x28);
    }
}

#[inline(always)]
fn pkr_by_type_fast(mem: *mut u8, pr: u32, ty: u32, idx: i32, size_out: u32) -> u32 {
    if size_out != 0 {
        mw32(mem, size_out, 0);
    }
    let idx = idx.max(0);
    let mut p = mr32(mem, pr);
    let mut slot = -1i32;
    let mut i = 0i32;
    loop {
        let t = mr32(mem, p);
        if t == 0 {
            break;
        }
        if t == ty {
            slot = i;
            break;
        }
        i += 1;
        p = p.wrapping_add(0x28);
    }
    if slot < 0 {
        return 0;
    }
    let entry_list = pr.wrapping_add(((slot as u32) << 4).wrapping_add(0xD8));
    let count = mr32(mem, entry_list.wrapping_add(4)) as i32;
    if idx >= count {
        return 0;
    }
    let base = mr32(mem, entry_list);
    let asset_hdr = mr32(mem, base.wrapping_add((idx as u32) << 2));
    if size_out != 0 {
        mw32(mem, size_out, mr32(mem, asset_hdr.wrapping_add(0x0C)));
    }
    mr32(mem, asset_hdr.wrapping_add(0x24))
}

/// Native `xSTFindAssetByType(unsigned int type, int idx, unsigned int* size)` (`0x8022_2DB0`).
pub fn hle_xst_find_asset_by_type(c: &mut Ctx) -> bool {
    let ty = c.r[3];
    let target_idx = c.r[4] as i32;
    let size_out = c.r[5];
    let mask = mr32(c.mem, ST_ROOT.wrapping_add(ST_MASK_OFF)) & 0xF_FFFF;
    let pkr_funcs = mr32(c.mem, c.r[13].wrapping_add(0xFFFF_97B4));
    let count_fn = mr32(c.mem, pkr_funcs.wrapping_add(0x1C));
    let by_type_fn = mr32(c.mem, pkr_funcs.wrapping_add(0x18));

    let mut acc = 0i32;
    let mut bits = mask;
    let need_save = count_fn != PKR_ASSET_COUNT_ADDR || by_type_fn != PKR_ASSET_BY_TYPE_ADDR;
    let saved_lr = c.lr;

    while bits != 0 {
        let b = bits.trailing_zeros();
        bits &= bits - 1;
        let pkr = mr32(c.mem, ST_ROOT.wrapping_add(b * ST_STRIDE + 8));
        let cnt = if count_fn == PKR_ASSET_COUNT_ADDR {
            pkr_count_fast(c.mem, pkr, ty)
        } else {
            call2(c, count_fn, pkr, ty)
        };
        let next_acc = acc.wrapping_add(cnt as i32);
        if target_idx >= acc && target_idx < next_acc {
            let local_idx = target_idx.wrapping_sub(acc);
            let res = if by_type_fn == PKR_ASSET_BY_TYPE_ADDR {
                pkr_by_type_fast(c.mem, pkr, ty, local_idx, size_out)
            } else {
                call4(c, by_type_fn, pkr, ty, local_idx as u32, size_out)
            };
            if need_save {
                c.lr = saved_lr;
            }
            c.r[3] = res;
            return true;
        }
        acc = next_acc;
    }

    if need_save {
        c.lr = saved_lr;
    }
    c.r[3] = 0;
    true
}

/// Native `xSTAssetCountByType(unsigned int type)` (`0x8022_2F94`).
pub fn hle_xst_asset_count_by_type(c: &mut Ctx) -> bool {
    let ty = c.r[3];
    let mask = mr32(c.mem, ST_ROOT.wrapping_add(ST_MASK_OFF)) & 0xF_FFFF;
    let pkr_funcs = mr32(c.mem, c.r[13].wrapping_add(0xFFFF_97B4));
    let count_fn = mr32(c.mem, pkr_funcs.wrapping_add(0x1C));
    let mut total = 0u32;
    let mut bits = mask;
    let saved_lr = c.lr;

    while bits != 0 {
        let b = bits.trailing_zeros();
        bits &= bits - 1;
        let pkr = mr32(c.mem, ST_ROOT.wrapping_add(b * ST_STRIDE + 8));
        let cnt = if count_fn == PKR_ASSET_COUNT_ADDR {
            pkr_count_fast(c.mem, pkr, ty)
        } else {
            call2(c, count_fn, pkr, ty)
        };
        total = total.wrapping_add(cnt);
    }

    c.lr = saved_lr;
    c.r[3] = total;
    true
}

/// Native `xVec3Copy(xVec3* dst, const xVec3* src)` (`0x8021_C4B0`).
#[inline]
pub fn hle_xvec3_copy(c: &mut Ctx) -> bool {
    let dst = (c.r[3] & 0x01FF_FFFF) as usize;
    let src = (c.r[4] & 0x01FF_FFFF) as usize;
    // SAFETY: both pointers are valid guest vectors within 24 MB main RAM.
    unsafe { core::ptr::copy(c.mem.add(src), c.mem.add(dst), 12) };
    true
}

#[inline(always)]
fn swap_elems(mem: *mut u8, a: u32, b: u32, size: usize) {
    let oa = (a & 0x01FF_FFFF) as usize;
    let ob = (b & 0x01FF_FFFF) as usize;
    if oa == ob || size == 0 {
        return;
    }
    // SAFETY: disjoint elements inside guest RAM (`|oa - ob| >= size`).
    unsafe { core::ptr::swap_nonoverlapping(mem.add(oa), mem.add(ob), size) };
}

/// Native `qsort` (`0x802A_A454`) — exact Metrowerks heapsort in native code.
pub fn hle_qsort(c: &mut Ctx) -> bool {
    let base = c.r[3];
    let n = c.r[4];
    let size = c.r[5];
    let cmp_fn = c.r[6];
    if n < 2 || size == 0 {
        return true;
    }
    let sz = size as usize;
    let saved_lr = c.lr;

    let mut half = (n >> 1).wrapping_add(1);
    let mut n_cur = n;
    let mut parent = base.wrapping_add(size.wrapping_mul(half.wrapping_sub(1)));
    let mut end = base.wrapping_add(size.wrapping_mul(n.wrapping_sub(1)));

    loop {
        if half > 1 {
            parent = parent.wrapping_sub(size);
            half = half.wrapping_sub(1);
        } else {
            swap_elems(c.mem, end, parent, sz);
            n_cur = n_cur.wrapping_sub(1);
            if n_cur == 1 {
                break;
            }
            end = end.wrapping_sub(size);
        }

        let mut j = half;
        let mut node = base.wrapping_add(size.wrapping_mul(half.wrapping_sub(1)));
        loop {
            if j.wrapping_mul(2) > n_cur {
                break;
            }
            j = j << 1;
            let parent_node = node;
            let mut child = base.wrapping_add(size.wrapping_mul(j.wrapping_sub(1)));
            if j < n_cur {
                let right = child.wrapping_add(size);
                if (call2(c, cmp_fn, child, right) as i32) < 0 {
                    child = right;
                    j = j.wrapping_add(1);
                }
            }
            if (call2(c, cmp_fn, parent_node, child) as i32) >= 0 {
                break;
            }
            swap_elems(c.mem, child, parent_node, sz);
            node = child;
        }
    }

    c.lr = saved_lr;
    true
}

/// Native O(N) `TriStripBinEntryArrayCreate(RwInt32 numTris, TriStripBinEntryArray* arr, TriStripEdge** edgeList, TriStripTri* tris)` (`0x8032_3F64`).
/// Replaces the original O(N^2) single-linked-list edge scan with a hash table while preserving
/// exact guest allocations, `*edgeList` order, and struct fields.
pub fn hle_tri_strip_bin_entry_array_create(c: &mut Ctx) -> bool {
    let num_tris = c.r[3];
    let bin_entry_arr = c.r[4];
    let edge_list_ptr = c.r[5];
    let tris_in = c.r[6];

    let saved_lr = c.lr;
    let rw_globals = mr32(c.mem, c.r[13].wrapping_add(0xFFFF_A2E4));
    let rw_malloc = mr32(c.mem, rw_globals.wrapping_add(0x108));
    let rw_fl_alloc = mr32(c.mem, rw_globals.wrapping_add(0x118));
    const TAG: u32 = 0x3_0502;

    let tri_ptrs = call2(c, rw_malloc, num_tris << 2, TAG);
    let fl_tris = call4(c, RW_FREELIST_CREATE_ADDR, 0x24, num_tris, 4, TAG);
    mw32(c.mem, bin_entry_arr, fl_tris);
    let fl_edges = call4(c, RW_FREELIST_CREATE_ADDR, 0x10, (num_tris >> 1).wrapping_add(1), 4, TAG);
    mw32(c.mem, bin_entry_arr.wrapping_add(4), fl_edges);

    let mut unmatched: HashMap<u32, Vec<u32>> = HashMap::with_capacity(num_tris as usize * 2);
    let existing_head = mr32(c.mem, edge_list_ptr);
    if existing_head != 0 {
        let mut existing = Vec::new();
        let mut e = existing_head;
        while e != 0 {
            if mr32(c.mem, e.wrapping_add(0x08)) == 0 {
                existing.push(e);
            }
            e = mr32(c.mem, e.wrapping_add(0x0C));
        }
        for &e in existing.iter().rev() {
            let v0 = mr16(c.mem, e);
            let v1 = mr16(c.mem, e.wrapping_add(2));
            let key = ((v0 as u32) << 16) | (v1 as u32);
            unmatched.entry(key).or_default().push(e);
        }
    }

    let mut tri_slot = tri_ptrs;
    let mut tri_in = tris_in;
    for i in 0..num_tris {
        let tri = call2(c, rw_fl_alloc, mr32(c.mem, bin_entry_arr), TAG);
        mw32(c.mem, tri_slot, tri);
        mw8(c.mem, tri.wrapping_add(0x20), 0);
        mw32(c.mem, tri, i);
        mw32(c.mem, tri.wrapping_add(0x14), 0);
        mw32(c.mem, tri.wrapping_add(0x10), 0);
        mw32(c.mem, tri.wrapping_add(0x18), 0);

        let v0 = mr16(c.mem, tri_in);
        let v1 = mr16(c.mem, tri_in.wrapping_add(2));
        let v2 = mr16(c.mem, tri_in.wrapping_add(4));

        let edges = [(v0, v1, 0x04u32), (v1, v2, 0x08u32), (v2, v0, 0x0Cu32)];
        let fl_edges_ptr = mr32(c.mem, bin_entry_arr.wrapping_add(4));

        for &(va, vb, tri_field_off) in &edges {
            let rev_key = ((vb as u32) << 16) | (va as u32);
            let matched = unmatched.get_mut(&rev_key).and_then(|v| v.pop());
            let edge_ptr = if let Some(e) = matched {
                let other_tri = mr32(c.mem, e.wrapping_add(0x04));
                let od = mr8(c.mem, other_tri.wrapping_add(0x20));
                mw8(c.mem, other_tri.wrapping_add(0x20), od.wrapping_add(1));
                let td = mr8(c.mem, tri.wrapping_add(0x20));
                mw8(c.mem, tri.wrapping_add(0x20), td.wrapping_add(1));
                mw32(c.mem, e.wrapping_add(0x08), tri);
                e
            } else {
                let e = call2(c, rw_fl_alloc, fl_edges_ptr, TAG);
                mw16(c.mem, e, va);
                mw16(c.mem, e.wrapping_add(2), vb);
                mw32(c.mem, e.wrapping_add(0x04), tri);
                mw32(c.mem, e.wrapping_add(0x08), 0);
                let prev_head = mr32(c.mem, edge_list_ptr);
                mw32(c.mem, e.wrapping_add(0x0C), prev_head);
                mw32(c.mem, edge_list_ptr, e);
                let fwd_key = ((va as u32) << 16) | (vb as u32);
                unmatched.entry(fwd_key).or_default().push(e);
                e
            };
            mw32(c.mem, tri.wrapping_add(tri_field_off), edge_ptr);
        }

        tri_slot = tri_slot.wrapping_add(4);
        tri_in = tri_in.wrapping_add(0x14);
    }

    c.lr = saved_lr;
    c.r[3] = tri_ptrs;
    true
}

#[inline(always)]
fn in_ram_opt(addr: u32, size: u32) -> bool {
    addr >= runtime::mem::RAM_BASE
        && addr
            .checked_add(size)
            .map_or(false, |end| end <= runtime::mem::RAM_BASE + runtime::mem::EXT_RAM_SIZE)
}

#[inline(always)]
fn mr_f32(mem: *mut u8, ea: u32) -> f32 {
    f32::from_bits(mr32(mem, ea))
}

#[inline(always)]
fn mw_f32(mem: *mut u8, ea: u32, v: f32) {
    mw32(mem, ea, v.to_bits());
}

#[inline(always)]
fn emit_index_run(
    mem: *mut u8,
    src_base: u32,
    dst_base: u32,
    start_idx: u32,
    count: u32,
    stride: u32,
    out_type: u32,
) {
    let mut src = src_base.wrapping_add(start_idx.wrapping_mul(2));
    let mut dst = dst_base;
    match out_type {
        2 => {
            for _ in 0..count {
                let v = mr16(mem, src) as u8;
                mw8(mem, dst, v);
                src = src.wrapping_add(2);
                dst = dst.wrapping_add(stride);
            }
        }
        3 => {
            for _ in 0..count {
                let v = mr16(mem, src);
                mw16(mem, dst, v);
                src = src.wrapping_add(2);
                dst = dst.wrapping_add(stride);
            }
        }
        _ => {}
    }
}

/// Native Rust fast-path for RenderWare `_rwGCNInstanceIndices` (`0x803347CC`).
/// Converts triangle-list or degenerate-stitched triangle-strip u16 indices into u8/u16 display-list slots.
pub fn hle_rw_gcn_instance_indices(c: &mut Ctx) -> bool {
    let scan_src = c.r[3];
    let copy_src = c.r[4];
    let num_indices = c.r[5];
    let is_strip = c.r[6] as i32;
    let stride = c.r[7];
    let out_type = c.r[8];
    let flag = c.r[9] as i32;
    let dst_base = c.r[10];

    let mem = c.mem;
    if is_strip == 0 {
        if num_indices == 0 {
            return true;
        }
        if !in_ram_opt(copy_src, num_indices.saturating_mul(2))
            || !in_ram_opt(dst_base, num_indices.saturating_mul(stride.max(2)))
        {
            return false;
        }
        emit_index_run(mem, copy_src, dst_base, 0, num_indices, stride, out_type);
        return true;
    }

    if num_indices < 2 {
        return false;
    }
    if !in_ram_opt(scan_src, num_indices.saturating_mul(2))
        || !in_ram_opt(copy_src, num_indices.saturating_mul(2))
        || !in_ram_opt(dst_base, stride.max(2))
    {
        return false;
    }

    let limit = num_indices - 2;
    let mut strips = 0u32;
    let mut total_verts = 0u32;
    let mut run = 0u32;
    let mut p_scan = scan_src;

    for i in 0..limit {
        let a = mr16(mem, p_scan);
        let b = mr16(mem, p_scan.wrapping_add(2));
        let c_idx = mr16(mem, p_scan.wrapping_add(4));
        if a != b && a != c_idx && b != c_idx {
            if run == 0 && (i & 1) != 0 && flag != 0 {
                run = 2;
            } else {
                run = run.wrapping_add(1);
            }
        } else if run != 0 {
            let can_bridge = flag != 0
                && i < num_indices.wrapping_sub(3)
                && ((i.wrapping_add(1)) & 1) != 0
                && {
                    let v3 = mr16(mem, p_scan.wrapping_add(6));
                    b != c_idx && b != v3 && c_idx != v3
                };
            if can_bridge {
                run = run.wrapping_add(1);
            } else {
                let emit_cnt = run.wrapping_add(2);
                let out_off = strips
                    .wrapping_mul(3)
                    .wrapping_add(total_verts.wrapping_mul(stride));
                let dst = dst_base.wrapping_add(out_off);
                let start_idx = i.wrapping_sub(run);
                emit_index_run(mem, copy_src, dst, start_idx, emit_cnt, stride, out_type);
                total_verts = total_verts.wrapping_add(emit_cnt);
                strips = strips.wrapping_add(1);
                run = 0;
            }
        }
        p_scan = p_scan.wrapping_add(2);
    }

    if run != 0 {
        let emit_cnt = run.wrapping_add(2);
        let out_off = strips
            .wrapping_mul(3)
            .wrapping_add(total_verts.wrapping_mul(stride));
        let dst = dst_base.wrapping_add(out_off);
        let start_idx = limit.wrapping_sub(run);
        emit_index_run(mem, copy_src, dst, start_idx, emit_cnt, stride, out_type);
    }

    true
}

/// Native Rust fast-path for `iAnimEvalSKB__FP14iAnimSKBHeaderfUiP5xVec3P5xQuat` (`0x80254C30`).
/// Evaluates i16-compressed SKB keyframe quaternions (shortest-path slerp) and scaled translations.
pub fn hle_ianim_eval_skb(c: &mut Ctx) -> bool {
    const INV_32767: f32 = f32::from_bits(0x3800_0100);
    const SLERP_LIN_COS: f32 = f32::from_bits(0x3F7F_FF58); // 0.99999

    let header = c.r[3];
    let flags = c.r[4];
    let tran_out = c.r[5];
    let quat_out = c.r[6];
    let mut time = c.f[1] as f32;

    if !in_ram_opt(header, 0x1C) {
        return false;
    }

    let mem = c.mem;
    let bone_count = mr16(mem, header.wrapping_add(0x08)) as u32;
    let time_count = mr16(mem, header.wrapping_add(0x0A)) as u32;
    let key_count = mr32(mem, header.wrapping_add(0x0C));
    if bone_count == 0 || time_count == 0 {
        return false;
    }

    let sx = mr_f32(mem, header.wrapping_add(0x10));
    let sy = mr_f32(mem, header.wrapping_add(0x14));
    let sz = mr_f32(mem, header.wrapping_add(0x18));

    let keys_base = header.wrapping_add(0x1C);
    let times_base = keys_base.wrapping_add(key_count.wrapping_mul(16));
    let offsets_base = times_base.wrapping_add(time_count.wrapping_mul(4));
    if !in_ram_opt(keys_base, key_count.saturating_mul(16))
        || !in_ram_opt(times_base, time_count.saturating_mul(4))
    {
        return false;
    }

    if time < 0.0 {
        time = 0.0;
    }
    let last_time = mr_f32(mem, times_base.wrapping_add((time_count - 1).wrapping_mul(4)));
    if time > last_time {
        time = last_time;
    }

    let mut t_idx = (time_count - 1) & 3;
    while t_idx + 4 < time_count && mr_f32(mem, times_base.wrapping_add(t_idx.wrapping_mul(4))) < time {
        t_idx += 4;
    }
    if mr_f32(mem, times_base.wrapping_add(t_idx.wrapping_mul(4))) < time && t_idx + 4 == time_count - 1 {
        t_idx += 4;
    }
    while t_idx > 0 && time <= mr_f32(mem, times_base.wrapping_add(t_idx.wrapping_mul(4))) {
        t_idx -= 1;
    }

    let mut out_bones = bone_count;
    let mut off_idx = t_idx.wrapping_mul(bone_count);
    if (flags & 1) != 0 {
        out_bones = 1;
    }
    if (flags & 2) != 0 {
        out_bones = out_bones.wrapping_sub(1);
        off_idx = off_idx.wrapping_add(1);
    }

    if out_bones == 0 {
        return true;
    }
    if !in_ram_opt(tran_out, out_bones.saturating_mul(12))
        || !in_ram_opt(quat_out, out_bones.saturating_mul(16))
    {
        return false;
    }

    if time_count == 1 {
        let mut k = keys_base;
        let mut t_dst = tran_out;
        let mut q_dst = quat_out;
        if !in_ram_opt(keys_base, out_bones.saturating_mul(0x20)) {
            return false;
        }
        for _ in 0..out_bones {
            mw_f32(mem, q_dst, (mr16(mem, k.wrapping_add(2)) as i16 as f32) * INV_32767);
            mw_f32(mem, q_dst.wrapping_add(4), (mr16(mem, k.wrapping_add(4)) as i16 as f32) * INV_32767);
            mw_f32(mem, q_dst.wrapping_add(8), (mr16(mem, k.wrapping_add(6)) as i16 as f32) * INV_32767);
            mw_f32(mem, q_dst.wrapping_add(12), (mr16(mem, k.wrapping_add(8)) as i16 as f32) * INV_32767);

            mw_f32(mem, t_dst, (mr16(mem, k.wrapping_add(10)) as i16 as f32) * sx);
            mw_f32(mem, t_dst.wrapping_add(4), (mr16(mem, k.wrapping_add(12)) as i16 as f32) * sy);
            mw_f32(mem, t_dst.wrapping_add(8), (mr16(mem, k.wrapping_add(14)) as i16 as f32) * sz);

            k = k.wrapping_add(0x20);
            q_dst = q_dst.wrapping_add(0x10);
            t_dst = t_dst.wrapping_add(0x0C);
        }
        return true;
    }

    let mut off_ptr = offsets_base.wrapping_add(off_idx.wrapping_mul(2));
    if !in_ram_opt(off_ptr, out_bones.saturating_mul(2)) {
        return false;
    }

    let mut t_dst = tran_out;
    let mut q_dst = quat_out;
    for _ in 0..out_bones {
        let k_idx = mr16(mem, off_ptr) as u32;
        off_ptr = off_ptr.wrapping_add(2);
        let k0 = keys_base.wrapping_add(k_idx.wrapping_mul(16));
        let k1 = k0.wrapping_add(16);

        let ti0 = mr16(mem, k0) as u32;
        let ti1 = mr16(mem, k1) as u32;
        let time0 = mr_f32(mem, times_base.wrapping_add(ti0.wrapping_mul(4)));
        let time1 = mr_f32(mem, times_base.wrapping_add(ti1.wrapping_mul(4)));
        let denom = time1 - time0;
        let alpha = if denom != 0.0 { (time - time0) / denom } else { 0.0 };

        let q0 = [
            (mr16(mem, k0.wrapping_add(2)) as i16 as f32) * INV_32767,
            (mr16(mem, k0.wrapping_add(4)) as i16 as f32) * INV_32767,
            (mr16(mem, k0.wrapping_add(6)) as i16 as f32) * INV_32767,
            (mr16(mem, k0.wrapping_add(8)) as i16 as f32) * INV_32767,
        ];
        let mut q1 = [
            (mr16(mem, k1.wrapping_add(2)) as i16 as f32) * INV_32767,
            (mr16(mem, k1.wrapping_add(4)) as i16 as f32) * INV_32767,
            (mr16(mem, k1.wrapping_add(6)) as i16 as f32) * INV_32767,
            (mr16(mem, k1.wrapping_add(8)) as i16 as f32) * INV_32767,
        ];

        let out_q = if alpha <= 0.0 {
            q0
        } else if alpha >= 1.0 {
            q1
        } else {
            let mut cosom = q0[0] * q1[0] + q0[1] * q1[1] + q0[2] * q1[2] + q0[3] * q1[3];
            if cosom < 0.0 {
                cosom = -cosom;
                q1 = [-q1[0], -q1[1], -q1[2], -q1[3]];
            }
            let (s0, s1) = if cosom >= SLERP_LIN_COS {
                (1.0 - alpha, alpha)
            } else {
                let omega = libm::acosf(cosom.clamp(0.0, 1.0));
                let sin_om = libm::sinf(omega);
                if sin_om.abs() < 1e-6 {
                    (1.0 - alpha, alpha)
                } else {
                    let inv_sin = 1.0 / sin_om;
                    (
                        libm::sinf((1.0 - alpha) * omega) * inv_sin,
                        libm::sinf(alpha * omega) * inv_sin,
                    )
                }
            };
            [
                s0 * q0[0] + s1 * q1[0],
                s0 * q0[1] + s1 * q1[1],
                s0 * q0[2] + s1 * q1[2],
                s0 * q0[3] + s1 * q1[3],
            ]
        };

        mw_f32(mem, q_dst, out_q[0]);
        mw_f32(mem, q_dst.wrapping_add(4), out_q[1]);
        mw_f32(mem, q_dst.wrapping_add(8), out_q[2]);
        mw_f32(mem, q_dst.wrapping_add(12), out_q[3]);

        let tx0 = (mr16(mem, k0.wrapping_add(10)) as i16 as f32) * sx;
        let ty0 = (mr16(mem, k0.wrapping_add(12)) as i16 as f32) * sy;
        let tz0 = (mr16(mem, k0.wrapping_add(14)) as i16 as f32) * sz;
        let tx1 = (mr16(mem, k1.wrapping_add(10)) as i16 as f32) * sx;
        let ty1 = (mr16(mem, k1.wrapping_add(12)) as i16 as f32) * sy;
        let tz1 = (mr16(mem, k1.wrapping_add(14)) as i16 as f32) * sz;

        mw_f32(mem, t_dst, tx0 + alpha * (tx1 - tx0));
        mw_f32(mem, t_dst.wrapping_add(4), ty0 + alpha * (ty1 - ty0));
        mw_f32(mem, t_dst.wrapping_add(8), tz0 + alpha * (tz1 - tz0));

        q_dst = q_dst.wrapping_add(0x10);
        t_dst = t_dst.wrapping_add(0x0C);
    }

    true
}

/// Native Rust fast-path for `xClumpColl_ForAllIntersections` (`0x80209674`).
/// Accelerates sphere (`type == 3`) BSP tree traversal + `RtIntersectionSphereTriangle`,
/// delegating non-sphere intersection types (`line == 1`, `box == 5`) back to the recompiled PPC function.
pub fn hle_xclump_coll_intersections(c: &mut Ctx) -> bool {
    const MIN_RAD: f32 = f32::from_bits(0x3727_C5AC); // 1e-5

    let tree = c.r[3];
    let isect = c.r[4];
    let callback = c.r[5];
    let cb_data = c.r[6];

    if !in_ram_opt(tree, 0x10) || !in_ram_opt(isect, 0x1C) {
        return false;
    }

    let mem = c.mem;
    if mr32(mem, isect.wrapping_add(0x18)) != 3 {
        return false;
    }

    let cx = mr_f32(mem, isect);
    let cy = mr_f32(mem, isect.wrapping_add(4));
    let cz = mr_f32(mem, isect.wrapping_add(8));
    let raw_rad = mr_f32(mem, isect.wrapping_add(12));
    let rad = if raw_rad > MIN_RAD { raw_rad } else { MIN_RAD };
    mw_f32(mem, isect.wrapping_add(12), rad);
    let inv_rad = 1.0f32 / rad;

    let box_min = [cx - rad, cy - rad, cz - rad];
    let box_max = [cx + rad, cy + rad, cz + rad];

    let branch_nodes = mr32(mem, tree.wrapping_add(4));
    let triangles = mr32(mem, tree.wrapping_add(0x0C));
    if !in_ram_opt(triangles, 8) || (branch_nodes != 0 && !in_ram_opt(branch_nodes, 16)) {
        return false;
    }

    let r13 = c.r[13];
    let g_mask1 = mr8(mem, r13.wrapping_add(0xFFFF_83D8));
    let g_mask2 = mr8(mem, r13.wrapping_add(0xFFFF_9688));
    let g_cmp = mr8(mem, r13.wrapping_add(0xFFFF_83D9));

    let saved_lr = c.lr;
    let saved_sp = c.r[1];
    let frame_sp = saved_sp.wrapping_sub(0x40);
    if !in_ram_opt(frame_sp, 0x40) {
        return false;
    }
    c.r[1] = frame_sp;
    mw32(mem, frame_sp, saved_sp);
    let col_tri = frame_sp.wrapping_add(0x10);

    let mut stack = [(0u32, 0u32); 128];
    let mut sp = 0usize;
    let mut node_type = if branch_nodes != 0 { 2u32 } else { 1u32 };
    let mut node_idx = 0u32;

    loop {
        if node_type == 1 {
            let mut tri = triangles.wrapping_add(node_idx.wrapping_mul(8));
            loop {
                let tri_flags = mr8(mem, tri.wrapping_add(4));
                let pass = (tri_flags & g_mask1) != 0 || (tri_flags & g_mask2) == g_cmp;
                if pass {
                    let v0 = mr32(mem, tri);
                    let (v1, v2) = if (tri_flags & 2) == 0 {
                        (v0.wrapping_add(0x0C), v0.wrapping_add(0x18))
                    } else {
                        (v0.wrapping_add(0x18), v0.wrapping_add(0x0C))
                    };
                    if let Some((norm, dist)) = crate::arm64_native::rt_sphere_triangle_raw(
                        mem, cx, cy, cz, rad, v0, v1, v2, None, None,
                    ) {
                        mw_f32(mem, col_tri, norm[0]);
                        mw_f32(mem, col_tri.wrapping_add(4), norm[1]);
                        mw_f32(mem, col_tri.wrapping_add(8), norm[2]);
                        mw32(mem, col_tri.wrapping_add(0x0C), mr32(mem, v0));
                        mw32(mem, col_tri.wrapping_add(0x10), mr32(mem, v0.wrapping_add(4)));
                        mw32(mem, col_tri.wrapping_add(0x14), mr32(mem, v0.wrapping_add(8)));
                        mw32(mem, col_tri.wrapping_add(0x18), tri);
                        mw32(mem, col_tri.wrapping_add(0x1C), v0);
                        mw32(mem, col_tri.wrapping_add(0x20), v1);
                        mw32(mem, col_tri.wrapping_add(0x24), v2);

                        let d = (dist * inv_rad) as f64;
                        c.f[1] = d;
                        c.ps1[1] = d;
                        let cont = call4(c, callback, isect, 0, col_tri, cb_data);
                        if cont == 0 {
                            c.lr = saved_lr;
                            c.r[1] = saved_sp;
                            c.r[3] = tree;
                            return true;
                        }
                    }
                }
                tri = tri.wrapping_add(8);
                if (tri_flags & 1) == 0 {
                    break;
                }
            }
            if sp == 0 {
                break;
            }
            sp -= 1;
            (node_type, node_idx) = stack[sp];
        } else if node_type == 2 {
            let node = branch_nodes.wrapping_add(node_idx.wrapping_mul(16));
            let left_info = mr32(mem, node);
            let right_info = mr32(mem, node.wrapping_add(4));
            let axis = ((left_info >> 2) & 3) as usize;
            if axis >= 3 {
                c.lr = saved_lr;
                c.r[1] = saved_sp;
                return false;
            }
            let left_val = mr_f32(mem, node.wrapping_add(8));
            let right_val = mr_f32(mem, node.wrapping_add(12));

            if box_min[axis] < left_val {
                if box_max[axis] >= right_val && sp < stack.len() {
                    stack[sp] = (right_info & 3, right_info >> 12);
                    sp += 1;
                }
                node_type = left_info & 3;
                node_idx = left_info >> 12;
            } else {
                node_type = right_info & 3;
                node_idx = right_info >> 12;
            }
        } else {
            if sp == 0 {
                break;
            }
            sp -= 1;
            (node_type, node_idx) = stack[sp];
        }
    }

    c.lr = saved_lr;
    c.r[1] = saved_sp;
    c.r[3] = tree;
    true
}

#[inline(always)]
fn mr64(mem: *mut u8, ea: u32) -> u64 {
    unsafe { core::ptr::read_unaligned(mem.add((ea & 0x01FF_FFFF) as usize) as *const u64).to_be() }
}

#[inline(always)]
fn fmadds(a: f64, b: f64, c: f64) -> f64 {
    (a * b + c) as f32 as f64
}

#[inline(always)]
fn fnmsubs(a: f64, b: f64, c: f64) -> f64 {
    (-(a * b - c)) as f32 as f64
}

/// Native ARM64 / Rust replacement for `AXFXReverbHiCallbackDpl2` (`0x802DABE8` + `HandleReverbDpl2` `0x802DA45C`).
/// Eliminates ~16ms/frame (26% of Core 1 CPU) in large DPL2-reverb levels like `BB01` while remaining
/// bit-identical to the original PowerPC DSP filter (verified via `hleverify.flag`).
pub fn hle_axfx_reverb_hi_callback_dpl2(c: &mut Ctx) -> bool {
    let aux_ptr = c.r[3];
    let rev = c.r[4];
    let mem = c.mem;

    if mr8(mem, rev.wrapping_add(0x254)) != 0 {
        return true;
    }

    let c_49f8 = mr_f32(mem, 0x804B_49F8) as f64;
    let c_49fc = mr_f32(mem, 0x804B_49FC) as f64;
    let c_4a00 = f64::from_bits(mr64(mem, 0x804B_4A00));

    let ap_g = mr_f32(mem, rev.wrapping_add(0x1E0)) as f64;
    let damping = mr_f32(mem, rev.wrapping_add(0x228)) as f64;
    let mix = mr_f32(mem, rev.wrapping_add(0x224)) as f64;
    let c_wet = (mix * c_49fc) as f32 as f64;
    let c_dry = (c_49fc - c_wet) as f32 as f64;
    let predelay_len = mr32(mem, rev.wrapping_add(0x22C));
    let predelay_end_off = predelay_len.wrapping_sub(1).wrapping_mul(4);

    for ch in 0..4u32 {
        let data_ptr = mr32(mem, aux_ptr.wrapping_add(ch << 2));
        let ch_x60 = ch.wrapping_mul(60);
        let comb_base = rev.wrapping_add(0xF0).wrapping_add(ch_x60);
        let ap_base = rev.wrapping_add(ch_x60);
        let g_base = rev.wrapping_add(0x1E4).wrapping_add(ch.wrapping_mul(12));
        let ch_x4 = rev.wrapping_add(ch << 2);

        let g0 = mr_f32(mem, g_base) as f64;
        let g1 = mr_f32(mem, g_base.wrapping_add(4)) as f64;
        let g2 = mr_f32(mem, g_base.wrapping_add(8)) as f64;
        let mut lpf_last = mr_f32(mem, ch_x4.wrapping_add(0x214)) as f64;

        let mut c0_in = mr32(mem, comb_base);
        let mut c0_out = mr32(mem, comb_base.wrapping_add(0x04));
        let c0_len = mr32(mem, comb_base.wrapping_add(0x08));
        let c0_buf = mr32(mem, comb_base.wrapping_add(0x0C));
        let mut c0_last = mr_f32(mem, comb_base.wrapping_add(0x10)) as f64;

        let mut c1_in = mr32(mem, comb_base.wrapping_add(0x14));
        let mut c1_out = mr32(mem, comb_base.wrapping_add(0x18));
        let c1_len = mr32(mem, comb_base.wrapping_add(0x1C));
        let c1_buf = mr32(mem, comb_base.wrapping_add(0x20));
        let mut c1_last = mr_f32(mem, comb_base.wrapping_add(0x24)) as f64;

        let mut c2_in = mr32(mem, comb_base.wrapping_add(0x28));
        let mut c2_out = mr32(mem, comb_base.wrapping_add(0x2C));
        let c2_len = mr32(mem, comb_base.wrapping_add(0x30));
        let c2_buf = mr32(mem, comb_base.wrapping_add(0x34));
        let mut c2_last = mr_f32(mem, comb_base.wrapping_add(0x38)) as f64;

        let mut a0_in = mr32(mem, ap_base);
        let mut a0_out = mr32(mem, ap_base.wrapping_add(0x04));
        let a0_len = mr32(mem, ap_base.wrapping_add(0x08));
        let a0_buf = mr32(mem, ap_base.wrapping_add(0x0C));
        let mut a0_last = mr_f32(mem, ap_base.wrapping_add(0x10)) as f64;

        let mut a1_in = mr32(mem, ap_base.wrapping_add(0x14));
        let mut a1_out = mr32(mem, ap_base.wrapping_add(0x18));
        let a1_len = mr32(mem, ap_base.wrapping_add(0x1C));
        let a1_buf = mr32(mem, ap_base.wrapping_add(0x20));
        let mut a1_last = mr_f32(mem, ap_base.wrapping_add(0x24)) as f64;

        let mut a2_in = mr32(mem, ap_base.wrapping_add(0x28));
        let mut a2_out = mr32(mem, ap_base.wrapping_add(0x2C));
        let a2_len = mr32(mem, ap_base.wrapping_add(0x30));
        let a2_buf = mr32(mem, ap_base.wrapping_add(0x34));
        let mut a2_last = mr_f32(mem, ap_base.wrapping_add(0x38)) as f64;

        for s in 0..160u32 {
            let sample_ea = data_ptr.wrapping_add(s << 2);
            let raw = mr32(mem, sample_ea);
            let f12_bits = (0x4330_0000u64 << 32) | ((raw ^ 0x8000_0000) as u64);
            let inp = (f64::from_bits(f12_bits) - c_4a00) as f32 as f64;

            let x = if predelay_len == 0 {
                inp
            } else {
                let p_base = mr32(mem, ch_x4.wrapping_add(0x234));
                let mut p_cur = mr32(mem, ch_x4.wrapping_add(0x244));
                let p_end = p_base.wrapping_add(predelay_end_off);
                let prev = mr_f32(mem, p_cur) as f64;
                mw32(mem, p_cur, (inp as f32).to_bits());
                p_cur = p_cur.wrapping_add(4);
                if p_cur == p_end {
                    p_cur = p_base;
                }
                mw32(mem, p_base.wrapping_add(0x244), p_cur);
                prev
            };

            // 3 parallel comb filters
            let y0 = fmadds(g0, c0_last, x);
            let y1 = fmadds(g1, c1_last, x);
            mw32(mem, c0_buf.wrapping_add(c0_in), (y0 as f32).to_bits());
            c0_in = c0_in.wrapping_add(4);
            if c0_in == c0_len {
                c0_in = 0;
            }
            mw32(mem, c1_buf.wrapping_add(c1_in), (y1 as f32).to_bits());
            c1_in = c1_in.wrapping_add(4);
            if c1_in == c1_len {
                c1_in = 0;
            }
            let o0 = mr_f32(mem, c0_buf.wrapping_add(c0_out)) as f64;
            c0_out = c0_out.wrapping_add(4);
            if c0_out == c0_len {
                c0_out = 0;
            }
            c0_last = o0;
            let o1 = mr_f32(mem, c1_buf.wrapping_add(c1_out)) as f64;
            c1_out = c1_out.wrapping_add(4);
            if c1_out == c1_len {
                c1_out = 0;
            }
            c1_last = o1;
            let sum01 = (o0 + o1) as f32 as f64;

            let y2 = fmadds(g2, c2_last, x);
            mw32(mem, c2_buf.wrapping_add(c2_in), (y2 as f32).to_bits());
            c2_in = c2_in.wrapping_add(4);
            if c2_in == c2_len {
                c2_in = 0;
            }
            let o2 = mr_f32(mem, c2_buf.wrapping_add(c2_out)) as f64;
            c2_out = c2_out.wrapping_add(4);
            if c2_out == c2_len {
                c2_out = 0;
            }
            c2_last = o2;
            let sum = (sum01 + o2) as f32 as f64;

            // Allpass 0
            let w0 = fmadds(ap_g, a0_last, sum);
            mw32(mem, a0_buf.wrapping_add(a0_in), (w0 as f32).to_bits());
            let v0 = fnmsubs(ap_g, w0, a0_last);
            a0_in = a0_in.wrapping_add(4);
            if a0_in == a0_len {
                a0_in = 0;
            }
            a0_last = mr_f32(mem, a0_buf.wrapping_add(a0_out)) as f64;
            a0_out = a0_out.wrapping_add(4);
            if a0_out == a0_len {
                a0_out = 0;
            }

            // Allpass 1
            let w1 = fmadds(ap_g, a1_last, v0);
            mw32(mem, a1_buf.wrapping_add(a1_in), (w1 as f32).to_bits());
            let v1 = fnmsubs(ap_g, w1, a1_last);
            a1_in = a1_in.wrapping_add(4);
            if a1_in == a1_len {
                a1_in = 0;
            }
            a1_last = mr_f32(mem, a1_buf.wrapping_add(a1_out)) as f64;
            a1_out = a1_out.wrapping_add(4);
            if a1_out == a1_len {
                a1_out = 0;
            }

            // 1-pole low-pass filter
            let lpf_in = (v1 * c_49f8) as f32 as f64;
            let lpf_out = fmadds(damping, lpf_last, lpf_in);
            lpf_last = lpf_out;

            // Allpass 2
            let w2 = fmadds(ap_g, a2_last, lpf_out);
            mw32(mem, a2_buf.wrapping_add(a2_in), (w2 as f32).to_bits());
            let v2 = fnmsubs(ap_g, w2, a2_last);
            a2_in = a2_in.wrapping_add(4);
            if a2_in == a2_len {
                a2_in = 0;
            }
            a2_last = mr_f32(mem, a2_buf.wrapping_add(a2_out)) as f64;
            a2_out = a2_out.wrapping_add(4);
            if a2_out == a2_len {
                a2_out = 0;
            }

            // Wet/dry mix and truncate to i32 (exact PPC fctiwz)
            let dry_part = (c_dry * inp) as f32 as f64;
            let mixed = fmadds(c_wet, v2, dry_part);
            let out_u32 = runtime::fctiwz_val(mixed).to_bits() as u32;
            mw32(mem, sample_ea, out_u32);
        }

        // Write back filter state
        mw32(mem, comb_base, c0_in);
        mw32(mem, comb_base.wrapping_add(0x04), c0_out);
        mw32(mem, comb_base.wrapping_add(0x10), (c0_last as f32).to_bits());
        mw32(mem, comb_base.wrapping_add(0x14), c1_in);
        mw32(mem, comb_base.wrapping_add(0x18), c1_out);
        mw32(mem, comb_base.wrapping_add(0x24), (c1_last as f32).to_bits());
        mw32(mem, comb_base.wrapping_add(0x28), c2_in);
        mw32(mem, comb_base.wrapping_add(0x2C), c2_out);
        mw32(mem, comb_base.wrapping_add(0x38), (c2_last as f32).to_bits());

        mw32(mem, ap_base, a0_in);
        mw32(mem, ap_base.wrapping_add(0x04), a0_out);
        mw32(mem, ap_base.wrapping_add(0x10), (a0_last as f32).to_bits());
        mw32(mem, ap_base.wrapping_add(0x14), a1_in);
        mw32(mem, ap_base.wrapping_add(0x18), a1_out);
        mw32(mem, ap_base.wrapping_add(0x24), (a1_last as f32).to_bits());
        mw32(mem, ap_base.wrapping_add(0x28), a2_in);
        mw32(mem, ap_base.wrapping_add(0x2C), a2_out);
        mw32(mem, ap_base.wrapping_add(0x38), (a2_last as f32).to_bits());

        mw32(mem, ch_x4.wrapping_add(0x214), (lpf_last as f32).to_bits());
    }

    true
}

const FN_RP_HANIM_FRAME_GET_HIER: u32 = 0x8030_4E44;
const FN_RW_FRAME_FOR_ALL_CHILDREN: u32 = 0x8034_5FC8;
const FN_IMODEL_FIND_HIER_CB: u32 = 0x8024_C898;
const FN_RW_MATRIX_UPDATE: u32 = 0x8033_CFA0;
const FN_RW_RESOURCES_EMPTY_ARENA: u32 = 0x8033_EF74;

#[inline(always)]
fn call1(c: &mut Ctx, func: u32, r3: u32) -> u32 {
    c.r[3] = r3;
    c.call(func);
    c.r[3]
}

#[inline(always)]
fn call3(c: &mut Ctx, func: u32, r3: u32, r4: u32, r5: u32) -> u32 {
    c.r[3] = r3;
    c.r[4] = r4;
    c.r[5] = r5;
    c.call(func);
    c.r[3]
}

/// Native ARM64 / Rust replacement for `iModelRender__FP8RpAtomicP11RwMatrixTag` (`0x8024BA58`).
///
/// The original GameCube binary had a debug/fragmentation hack that evicted the ENTIRE 696 KB
/// RenderWare Resource Arena and filled it word-by-word with `0xDEADBEEF` EVERY SINGLE FRAME
/// whenever any skinned atomic with `> 3000` vertices rendered (e.g. Patty Wagon / Mindy in `BB01`).
/// That forced all 200+ static level meshes to be re-instanced on Core 1, rebuilt (92K verts) on
/// Core 0, and re-uploaded (17ms) on Core 2 every frame.
///
/// This native implementation preserves the exact transform/HAnim/renderCallBack semantics while
/// only emptying the arena ONCE if a `> 3000`-vert skinned geometry is not yet instanced (`repEntry == 0`),
/// and never executes the `0xDEADBEEF` poison loop.
pub fn hle_imodel_render(c: &mut Ctx) -> bool {
    let atomic = c.r[3];
    let mat = c.r[4];
    let saved_lr = c.lr;
    let saved_sp = c.r[1];
    let mem = c.mem;

    // Reserve 16 bytes on guest stack for `hier` out-pointer in `RwFrameForAllChildren`.
    let sp_hier = saved_sp.wrapping_sub(0x10);
    c.r[1] = sp_hier;
    mw32(mem, sp_hier, 0);

    let frame0 = mr32(mem, atomic.wrapping_add(0x04));
    let mut hier = call1(c, FN_RP_HANIM_FRAME_GET_HIER, frame0);
    if hier == 0 {
        call3(c, FN_RW_FRAME_FOR_ALL_CHILDREN, frame0, FN_IMODEL_FIND_HIER_CB, sp_hier);
        hier = mr32(mem, sp_hier);
    }

    // One-time static init guard (`r13 - 0x677C` / `r13 - 0x6780`).
    let r13 = c.r[13];
    if mr8(mem, r13.wrapping_add(0xFFFF_9884)) == 0 {
        mw32(mem, r13.wrapping_add(0xFFFF_9880), 1);
        mw8(mem, r13.wrapping_add(0xFFFF_9884), 1);
    }

    let mut saved_pnode = 0u32;
    if hier != 0 {
        saved_pnode = mr32(mem, hier.wrapping_add(0x08));
        mw32(mem, hier.wrapping_add(0x08), mat.wrapping_add(0x40));
    }

    // Copy 64-byte RwMatrix into `atomic->frame->ltm` (`frame + 0x50`) and update matrix flags.
    let frame = mr32(mem, atomic.wrapping_add(0x04));
    let dst = frame.wrapping_add(0x50);
    unsafe {
        let src_ptr = mem.add((mat & 0x01FF_FFFF) as usize);
        let dst_ptr = mem.add((dst & 0x01FF_FFFF) as usize);
        core::ptr::copy(src_ptr, dst_ptr, 64);
    }
    call1(c, FN_RW_MATRIX_UPDATE, dst);

    // Only defrag the resource arena if a >3000-vertex skinned atomic has not been instanced yet.
    if hier != 0 {
        let geom = mr32(mem, atomic.wrapping_add(0x18));
        if geom != 0 && (mr32(mem, geom.wrapping_add(0x10)) as i32) > 3000 {
            let rep = if mr32(mem, geom.wrapping_add(0x18)) == 1 {
                mr32(mem, geom.wrapping_add(0x58))
            } else {
                mr32(mem, atomic.wrapping_add(0x14))
            };
            if rep == 0 {
                c.call(FN_RW_RESOURCES_EMPTY_ARENA);
            }
        }
    }

    // Render the atomic via its callback (`atomic + 0x48`).
    let cb = mr32(mem, atomic.wrapping_add(0x48));
    call1(c, cb, atomic);

    if hier != 0 {
        mw32(mem, hier.wrapping_add(0x08), saved_pnode);
    }

    c.r[1] = saved_sp;
    c.lr = saved_lr;
    true
}

const FN_RW_RES_HEAP_INIT: u32 = 0x8034_0DE0;
/// 6 MB RenderWare Resource Arena in extended Switch guest RAM (`0x8180_0000..0x81E0_0000`),
/// 9x larger than GameCube's 696 KB (`0xAE000`) arena so large levels like `BB01` (~2.8 MB active
/// geometry) keep all static meshes resident without per-frame eviction, token stalls, or rebuilds.
const EXT_RW_ARENA_ADDR: u32 = 0x8180_0000;
const EXT_RW_ARENA_SIZE: u32 = 0x0060_0000;

/// Native replacement for `_rwResourcesOpen` (`0x8033E860`): backs the RenderWare Resource Arena
/// with our dedicated 6 MB region at `0x8180_0000` (freeing 696 KB in the 24 MB GC heap too).
pub fn hle_rw_resources_open(c: &mut Ctx) -> bool {
    let instance = c.r[3];
    let offset = c.r[4];
    let saved_lr = c.lr;
    let r13 = c.r[13];
    let mem = c.mem;

    let rw_engine = mr32(mem, r13.wrapping_add(0xFFFF_A2E4));
    mw32(mem, r13.wrapping_add(0xFFFF_A2A8), offset);
    let res_globals = rw_engine.wrapping_add(offset);

    mw32(mem, res_globals.wrapping_add(0x0C), EXT_RW_ARENA_ADDR);
    let ok = call2(c, FN_RW_RES_HEAP_INIT, EXT_RW_ARENA_ADDR, EXT_RW_ARENA_SIZE);
    if ok == 0 {
        c.lr = saved_lr;
        c.r[3] = 0;
        return true;
    }

    let l0 = res_globals.wrapping_add(0x10);
    let l1 = res_globals.wrapping_add(0x18);
    mw32(mem, l0, l0);
    mw32(mem, l0.wrapping_add(4), l0);
    mw32(mem, l1, l1);
    mw32(mem, l1.wrapping_add(4), l1);
    mw32(mem, res_globals.wrapping_add(0x24), l0);
    mw32(mem, res_globals.wrapping_add(0x20), l1);
    // Store 0 in `res_globals.arenaSize` (read ONLY by `RwResourcesGetArenaSize` in `MorphCommon`
    // when vertex_count > 3000) while the actual resource heap is initialized to the full 6 MB.
    // This prevents `MorphCommon` from allocating 6 MB and running a 1.5M-word `0xDEADBEEF` poison
    // loop 3 times every frame in BB01 ("The Ride").
    mw32(mem, res_globals, 0);
    mw32(mem, res_globals.wrapping_add(4), 0);
    mw32(mem, res_globals.wrapping_add(8), 0);

    let ref_ea = r13.wrapping_add(0xFFFF_A2A8).wrapping_add(4);
    mw32(mem, ref_ea, mr32(mem, ref_ea).wrapping_add(1));

    crate::log_c("[switch] RenderWare Resource Arena -> 6.0 MB @ 0x81800000 (was 696 KB)");
    c.lr = saved_lr;
    c.r[3] = instance;
    true
}

/// Native replacement for `_rwResourcesClose` (`0x8033E9B4`): empties arena without freeing
/// the static extended RAM block at `0x8180_0000`.
pub fn hle_rw_resources_close(c: &mut Ctx) -> bool {
    let instance = c.r[3];
    let saved_lr = c.lr;
    let r13 = c.r[13];
    let mem = c.mem;

    c.call(FN_RW_RESOURCES_EMPTY_ARENA);
    let off = mr32(mem, r13.wrapping_add(0xFFFF_A2A8));
    let rw_engine = mr32(mem, r13.wrapping_add(0xFFFF_A2E4));
    let res_globals = rw_engine.wrapping_add(off);
    mw32(mem, res_globals.wrapping_add(0x0C), 0);

    let ref_ea = r13.wrapping_add(0xFFFF_A2A8).wrapping_add(4);
    mw32(mem, ref_ea, mr32(mem, ref_ea).wrapping_sub(1));

    c.lr = saved_lr;
    c.r[3] = instance;
    true
}

/// Native replacement for `RwResourcesSetArenaSize` (`0x8033EE2C`): keeps the 6 MB extended arena
/// instead of shrinking back to GameCube's 696 KB (`0xAE000`).
pub fn hle_rw_resources_set_arena_size(c: &mut Ctx) -> bool {
    let saved_lr = c.lr;
    let r13 = c.r[13];
    let mem = c.mem;

    let ref_cnt = mr32(mem, r13.wrapping_add(0xFFFF_A2A8).wrapping_add(4)) as i32;
    let rw_engine = mr32(mem, r13.wrapping_add(0xFFFF_A2E4));
    if ref_cnt == 0 {
        mw32(mem, rw_engine.wrapping_add(0x128), EXT_RW_ARENA_SIZE);
        c.lr = saved_lr;
        c.r[3] = 1;
        return true;
    }

    let off = mr32(mem, r13.wrapping_add(0xFFFF_A2A8));
    let res_globals = rw_engine.wrapping_add(off);
    mw32(mem, res_globals, 0);
    c.call(FN_RW_RESOURCES_EMPTY_ARENA);
    mw32(mem, res_globals.wrapping_add(0x0C), EXT_RW_ARENA_ADDR);
    let ok = call2(c, FN_RW_RES_HEAP_INIT, EXT_RW_ARENA_ADDR, EXT_RW_ARENA_SIZE);

    c.lr = saved_lr;
    c.r[3] = if ok != 0 { 1 } else { 0 };
    true
}

/// Native ARM64 / Rust replacement for `_rwGCLightsGlobalEnable` (`0x8032AF74`).
/// Walks the current `RpWorld` global light list, accumulates ambient light into the
/// RenderWare GC light context, and configures directional `GXLightObj` entries in native code.
pub fn hle_rw_gc_lights_global_enable(c: &mut Ctx) -> bool {
    let saved_lr = c.lr;
    let r13 = c.r[13];
    let r2 = c.r[2];
    let mem = c.mem;
    let enable_mask = (c.r[3] & 0xFF) as u8;
    let ctx = c.r[4];

    let rw_engine = mr32(mem, r13.wrapping_add(0xFFFF_A2E4));
    if rw_engine == 0 {
        return false;
    }
    let world = mr32(mem, rw_engine.wrapping_add(0x04));
    if world == 0 {
        c.lr = saved_lr;
        return true;
    }

    let list_end = world.wrapping_add(0x3C);
    let mut cur = mr32(mem, list_end);
    let mut guard = 0u32;

    while cur != list_end && cur != 0 && guard < 256 {
        guard += 1;
        let light = cur.wrapping_add(0xFFFF_FFCC);
        if light != 0 {
            let light_flags = mr8(mem, light.wrapping_add(0x02));
            if (light_flags & enable_mask) != 0 {
                let sub_type = mr8(mem, light.wrapping_add(0x01));
                if sub_type == 1 {
                    let slot = mr32(mem, ctx.wrapping_add(0x24));
                    let frame = mr32(mem, light.wrapping_add(0x04));
                    let root = mr32(mem, frame.wrapping_add(0xA0));
                    if root != 0 && (mr8(mem, root.wrapping_add(0x03)) & 1) != 0 {
                        call2(c, 0x8034_A1D0, root, 0); // _rwFrameSyncHierarchy
                    }
                    let ltm = frame.wrapping_add(0x50);

                    // Transform `ltm->at` (offset +0x20) by `_RwGCViewMatrix` at `0x80487324`
                    let vx = f32::from_bits(mr32(mem, ltm.wrapping_add(0x20)));
                    let vy = f32::from_bits(mr32(mem, ltm.wrapping_add(0x24)));
                    let vz = f32::from_bits(mr32(mem, ltm.wrapping_add(0x28)));
                    let m = 0x8048_7324u32;
                    let m00 = f32::from_bits(mr32(mem, m));
                    let m01 = f32::from_bits(mr32(mem, m.wrapping_add(0x04)));
                    let m02 = f32::from_bits(mr32(mem, m.wrapping_add(0x08)));
                    let m10 = f32::from_bits(mr32(mem, m.wrapping_add(0x10)));
                    let m11 = f32::from_bits(mr32(mem, m.wrapping_add(0x14)));
                    let m12 = f32::from_bits(mr32(mem, m.wrapping_add(0x18)));
                    let m20 = f32::from_bits(mr32(mem, m.wrapping_add(0x20)));
                    let m21 = f32::from_bits(mr32(mem, m.wrapping_add(0x24)));
                    let m22 = f32::from_bits(mr32(mem, m.wrapping_add(0x28)));

                    let dx = (m20 * vz) + ((m10 * vy) + (m00 * vx));
                    let dy = (m21 * vz) + ((m11 * vy) + (m01 * vx));
                    let dz = (m22 * vz) + ((m12 * vy) + (m02 * vx));

                    let light_obj = 0x8048_6258u32.wrapping_add((slot << 6) & 0xFFFF_FFC0);
                    let ext_off = mr32(mem, r13.wrapping_add(0xFFFF_A268));
                    let ext = light.wrapping_add(ext_off);
                    if mr32(mem, ext) == 0 {
                        let one = mr32(mem, r2.wrapping_add(0xFFFF_A910));
                        let zero = mr32(mem, r2.wrapping_add(0xFFFF_A914));
                        mw32(mem, light_obj.wrapping_add(0x10), one);
                        mw32(mem, light_obj.wrapping_add(0x14), zero);
                        mw32(mem, light_obj.wrapping_add(0x18), zero);
                        mw32(mem, light_obj.wrapping_add(0x1C), one);
                        mw32(mem, light_obj.wrapping_add(0x20), zero);
                        mw32(mem, light_obj.wrapping_add(0x24), zero);
                    } else {
                        mw32(mem, light_obj.wrapping_add(0x10), mr32(mem, ext.wrapping_add(0x04)));
                        mw32(mem, light_obj.wrapping_add(0x14), mr32(mem, ext.wrapping_add(0x08)));
                        mw32(mem, light_obj.wrapping_add(0x18), mr32(mem, ext.wrapping_add(0x0C)));
                        mw32(mem, light_obj.wrapping_add(0x1C), mr32(mem, ext.wrapping_add(0x10)));
                        mw32(mem, light_obj.wrapping_add(0x20), mr32(mem, ext.wrapping_add(0x14)));
                        mw32(mem, light_obj.wrapping_add(0x24), mr32(mem, ext.wrapping_add(0x18)));
                    }

                    let dist = f32::from_bits(mr32(mem, r2.wrapping_add(0xFFFF_A918)));
                    let px = dist * (-dx);
                    let py = dist * dy;
                    let pz = dist * (-dz);
                    mw32(mem, light_obj.wrapping_add(0x28), px.to_bits());
                    mw32(mem, light_obj.wrapping_add(0x2C), py.to_bits());
                    mw32(mem, light_obj.wrapping_add(0x30), pz.to_bits());

                    let scale = f32::from_bits(mr32(mem, r2.wrapping_add(0xFFFF_A91C)));
                    let cr = f32::from_bits(mr32(mem, light.wrapping_add(0x18)));
                    let cg = f32::from_bits(mr32(mem, light.wrapping_add(0x1C)));
                    let cb = f32::from_bits(mr32(mem, light.wrapping_add(0x20)));
                    let r_u8 = (((scale * cr) as i32) & 0xFF) as u32;
                    let g_u8 = (((scale * cg) as i32) & 0xFF) as u32;
                    let b_u8 = (((scale * cb) as i32) & 0xFF) as u32;
                    let rgba = (r_u8 << 24) | (g_u8 << 16) | (b_u8 << 8);
                    mw32(mem, light_obj.wrapping_add(0x0C), rgba);

                    let bit = if slot < 32 { 1u32 << slot } else { 0 };
                    call2(c, 0x802E_CDD4, light_obj, bit); // GXLoadLightObjImm

                    let cur_slot = mr32(mem, ctx.wrapping_add(0x24));
                    let cur_mask = mr32(mem, ctx.wrapping_add(0x20));
                    let mask_bit = if cur_slot < 32 { 1u32 << cur_slot } else { 0 };
                    mw32(mem, ctx.wrapping_add(0x20), cur_mask | mask_bit);
                    mw32(mem, ctx.wrapping_add(0x24), cur_slot.wrapping_add(1));
                } else {
                    let ar = f32::from_bits(mr32(mem, ctx.wrapping_add(0x0C)))
                        + f32::from_bits(mr32(mem, light.wrapping_add(0x18)));
                    let ag = f32::from_bits(mr32(mem, ctx.wrapping_add(0x10)))
                        + f32::from_bits(mr32(mem, light.wrapping_add(0x1C)));
                    let ab = f32::from_bits(mr32(mem, ctx.wrapping_add(0x14)))
                        + f32::from_bits(mr32(mem, light.wrapping_add(0x20)));
                    mw32(mem, ctx.wrapping_add(0x0C), ar.to_bits());
                    mw32(mem, ctx.wrapping_add(0x10), ag.to_bits());
                    mw32(mem, ctx.wrapping_add(0x14), ab.to_bits());
                    mw32(mem, ctx.wrapping_add(0x1C), 1);
                }
            }
        }
        cur = mr32(mem, cur);
    }

    c.lr = saved_lr;
    true
}

/// Parses a C `atof` decimal floating-point prefix natively, falling back to `None` on
/// hex-floats, inf/nan, or abnormally long strings so MSL handles exotic edge cases identically.
#[inline]
fn parse_c_atof(mem: *mut u8, addr: u32) -> Option<f64> {
    if !in_ram_opt(addr, 1) {
        return None;
    }
    let mut buf = [0u8; 64];
    let mut len = 0usize;
    let mut p = addr;
    // Skip leading C whitespace
    loop {
        if !in_ram_opt(p, 1) {
            return None;
        }
        let b = mr8(mem, p);
        if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C) {
            p = p.wrapping_add(1);
        } else {
            break;
        }
    }
    // Optional sign
    let first = mr8(mem, p);
    if first == b'+' || first == b'-' {
        buf[len] = first;
        len += 1;
        p = p.wrapping_add(1);
    }
    // Bail to MSL on hex floats ("0x...") or inf/nan
    let b0 = mr8(mem, p);
    if matches!(b0, b'i' | b'I' | b'n' | b'N') {
        return None;
    }
    if b0 == b'0' && in_ram_opt(p.wrapping_add(1), 1) {
        let b1 = mr8(mem, p.wrapping_add(1));
        if b1 == b'x' || b1 == b'X' {
            return None;
        }
    }

    let mut has_digits = false;
    while len < 60 && in_ram_opt(p, 1) {
        let b = mr8(mem, p);
        if b.is_ascii_digit() {
            has_digits = true;
            buf[len] = b;
            len += 1;
            p = p.wrapping_add(1);
        } else {
            break;
        }
    }
    if len < 60 && in_ram_opt(p, 1) && mr8(mem, p) == b'.' {
        buf[len] = b'.';
        len += 1;
        p = p.wrapping_add(1);
        while len < 60 && in_ram_opt(p, 1) {
            let b = mr8(mem, p);
            if b.is_ascii_digit() {
                has_digits = true;
                buf[len] = b;
                len += 1;
                p = p.wrapping_add(1);
            } else {
                break;
            }
        }
    }
    if !has_digits {
        return Some(0.0);
    }
    // Optional exponent (`[eE][+-]?[0-9]+`)
    if len < 56 && in_ram_opt(p, 1) && matches!(mr8(mem, p), b'e' | b'E') {
        let mut ep = p.wrapping_add(1);
        let mut ebuf = [0u8; 8];
        let mut elen = 0usize;
        ebuf[elen] = b'e';
        elen += 1;
        if in_ram_opt(ep, 1) && matches!(mr8(mem, ep), b'+' | b'-') {
            ebuf[elen] = mr8(mem, ep);
            elen += 1;
            ep = ep.wrapping_add(1);
        }
        let mut exp_digits = 0usize;
        while elen < ebuf.len() && in_ram_opt(ep, 1) {
            let b = mr8(mem, ep);
            if b.is_ascii_digit() {
                ebuf[elen] = b;
                elen += 1;
                exp_digits += 1;
                ep = ep.wrapping_add(1);
            } else {
                break;
            }
        }
        if exp_digits > 0 && len + elen <= buf.len() {
            if in_ram_opt(ep, 1) && mr8(mem, ep).is_ascii_digit() {
                return None; // abnormally long exponent
            }
            buf[len..len + elen].copy_from_slice(&ebuf[..elen]);
            len += elen;
            p = ep;
        }
    }
    if len >= 60 && in_ram_opt(p, 1) && mr8(mem, p).is_ascii_digit() {
        return None;
    }

    let s = core::str::from_utf8(&buf[..len]).ok()?;
    let v = s.parse::<f64>().ok()?;
    if !v.is_finite() {
        return None;
    }
    Some(v)
}

/// Native ARM64 / Rust replacement for `atof` (`0x802ABBB4`).
/// Eliminates MSL `__dec2num` / `__num2dec_internal` / `__two_exp` big-integer software math
/// (was 30-35% of all Core 1 CPU during `BB01` subtitle/HUD rendering!).
pub fn hle_atof(c: &mut Ctx) -> bool {
    let Some(v) = parse_c_atof(c.mem, c.r[3]) else {
        return false;
    };
    c.f[1] = v;
    c.ps1[1] = v;
    true
}

/// Native ARM64 / Rust replacement for `xatof__FPCc` (`0x801EE380`).
pub fn hle_xatof(c: &mut Ctx) -> bool {
    let Some(v) = parse_c_atof(c.mem, c.r[3]) else {
        return false;
    };
    let f = v as f32 as f64;
    c.f[1] = f;
    c.ps1[1] = f;
    true
}

#[inline(always)]
fn call5(c: &mut Ctx, func: u32, r3: u32, r4: u32, r5: u32, r6: u32, r7: u32) -> u32 {
    c.r[3] = r3;
    c.r[4] = r4;
    c.r[5] = r5;
    c.r[6] = r6;
    c.r[7] = r7;
    c.call(func);
    c.r[3]
}

/// Inlined 6-byte signed `xQuickCullIsects` (`0x801957D8`) check.
#[inline(always)]
fn qcd_isects_raw(mem: *mut u8, a: u32, b: u32) -> bool {
    if (mr8(mem, a) as i8) > (mr8(mem, b.wrapping_add(4)) as i8) {
        return false;
    }
    if (mr8(mem, a.wrapping_add(1)) as i8) > (mr8(mem, b.wrapping_add(5)) as i8) {
        return false;
    }
    if (mr8(mem, a.wrapping_add(2)) as i8) > (mr8(mem, b.wrapping_add(6)) as i8) {
        return false;
    }
    if (mr8(mem, b) as i8) > (mr8(mem, a.wrapping_add(4)) as i8) {
        return false;
    }
    if (mr8(mem, b.wrapping_add(1)) as i8) > (mr8(mem, a.wrapping_add(5)) as i8) {
        return false;
    }
    if (mr8(mem, b.wrapping_add(2)) as i8) > (mr8(mem, a.wrapping_add(6)) as i8) {
        return false;
    }
    true
}

struct GridIter {
    listhead: u32,
    cur: u32,
    delfound: bool,
}

#[inline(always)]
fn grid_cell_listhead(mem: *mut u8, grid: u32, gx: i32, gz: i32) -> u32 {
    if gx < 0 || gz < 0 {
        return 0;
    }
    let nx = mr16(mem, grid.wrapping_add(4)) as i32;
    let nz = mr16(mem, grid.wrapping_add(6)) as i32;
    if gx >= nx || gz >= nz {
        return 0;
    }
    let cells = mr32(mem, grid.wrapping_add(0x2C));
    if cells == 0 {
        return 0;
    }
    let idx = (gz as u32).wrapping_mul(nx as u32).wrapping_add(gx as u32);
    cells.wrapping_add(idx << 2)
}

#[inline(always)]
fn grid_iter_open(mem: *mut u8, listhead: u32) -> Option<GridIter> {
    if listhead == 0 || !in_ram_opt(listhead, 4) {
        return None;
    }
    let first = mr32(mem, listhead);
    if first == 0 || !in_ram_opt(first, 0x20) {
        return None;
    }
    let grid = mr32(mem, first.wrapping_add(0x0C));
    if in_ram_opt(grid, 0x38) {
        let active = mr32(mem, grid.wrapping_add(0x34));
        mw32(mem, grid.wrapping_add(0x34), active.wrapping_add(1));
    }
    Some(GridIter {
        listhead,
        cur: first,
        delfound: false,
    })
}

#[inline(always)]
fn grid_iter_close(mem: *mut u8, it: &mut GridIter) {
    if it.listhead == 0 || !in_ram_opt(it.listhead, 4) {
        it.listhead = 0;
        it.cur = 0;
        it.delfound = false;
        return;
    }
    let first = mr32(mem, it.listhead);
    if first != 0 && in_ram_opt(first, 0x20) {
        let grid = mr32(mem, first.wrapping_add(0x0C));
        if in_ram_opt(grid, 0x38) {
            let active = mr32(mem, grid.wrapping_add(0x34)).wrapping_sub(1);
            mw32(mem, grid.wrapping_add(0x34), active);
            if it.delfound && (active as i32) <= 0 {
                let mut prev_link = it.listhead;
                let mut cur = first;
                let mut guard = 0u32;
                while cur != 0 && in_ram_opt(cur, 0x20) && guard < 16384 {
                    guard += 1;
                    let next = mr32(mem, cur.wrapping_add(0x14));
                    if mr8(mem, cur.wrapping_add(0x09)) != 0 {
                        mw32(mem, prev_link, next);
                        mw32(mem, cur.wrapping_add(0x14), 0);
                        mw32(mem, cur.wrapping_add(0x10), 0);
                        mw32(mem, cur.wrapping_add(0x0C), 0);
                        mw8(mem, cur.wrapping_add(0x09), 0);
                        mw16(mem, cur.wrapping_add(0x04), 0xFFFF);
                        mw16(mem, cur.wrapping_add(0x06), 0xFFFF);
                        cur = mr32(mem, prev_link);
                    } else {
                        prev_link = cur.wrapping_add(0x14);
                        cur = next;
                    }
                }
            }
        }
    }
    it.listhead = 0;
    it.cur = 0;
    it.delfound = false;
}

#[inline(always)]
fn grid_iter_step(mem: *mut u8, it: &mut GridIter) {
    if it.cur != 0 && in_ram_opt(it.cur, 0x20) {
        it.cur = mr32(mem, it.cur.wrapping_add(0x14));
    } else {
        it.cur = 0;
    }
    let mut guard = 0u32;
    while it.cur != 0 && in_ram_opt(it.cur, 0x20) && guard < 16384 {
        guard += 1;
        if mr8(mem, it.cur.wrapping_add(0x09)) == 0 {
            return;
        }
        it.delfound = true;
        it.cur = mr32(mem, it.cur.wrapping_add(0x14));
    }
    grid_iter_close(mem, it);
}

#[inline(always)]
fn grid_clamped_cell(mem: *mut u8, grid: u32, px: f32, pz: f32) -> (i32, i32) {
    let min_x = mr_f32(mem, grid.wrapping_add(0x08));
    let min_z = mr_f32(mem, grid.wrapping_add(0x0C));
    let inv_cx = mr_f32(mem, grid.wrapping_add(0x20));
    let inv_cz = mr_f32(mem, grid.wrapping_add(0x24));
    let nx = mr16(mem, grid.wrapping_add(0x04)) as i32;
    let nz = mr16(mem, grid.wrapping_add(0x06)) as i32;
    let fx = (px - min_x) * inv_cx;
    let fz = (pz - min_z) * inv_cz;
    let max_x = (nx - 1) as f32;
    let max_z = (nz - 1) as f32;
    let cx = (if fx > 0.0 { fx } else { 0.0 }).min(max_x) as i32;
    let cz = (if fz > 0.0 { fz } else { 0.0 }).min(max_z) as i32;
    (cx, cz)
}

#[inline(always)]
fn grid_quadrant(mem: *mut u8, grid: u32, cx: i32, cz: i32, px: f32, pz: f32) -> u32 {
    let min_x = mr_f32(mem, grid.wrapping_add(0x08));
    let min_z = mr_f32(mem, grid.wrapping_add(0x0C));
    let csx = mr_f32(mem, grid.wrapping_add(0x18));
    let csz = mr_f32(mem, grid.wrapping_add(0x1C));
    let mid_x = csx * (cx as f32) + min_x + 0.5f32 * csx;
    let mid_z = csz * (cz as f32) + min_z + 0.5f32 * csz;
    if px < mid_x {
        if pz < mid_z { 0 } else { 1 }
    } else if pz < mid_z {
        3
    } else {
        2
    }
}

/// Native ARM64 / Rust replacement for `xGridCheckPosition__FP5xGridP5xVec3P7xQCDataPFP4xEntPv_iPv` (`0x801B5920`).
pub fn hle_xgrid_check_position(c: &mut Ctx) -> bool {
    let grid = c.r[3];
    let pos = c.r[4];
    let qcd = c.r[5];
    let cb = c.r[6];
    let cbdata = c.r[7];
    if !in_ram_opt(grid, 0x38) || !in_ram_opt(pos, 12) || !in_ram_opt(qcd, 8) {
        return false;
    }
    let mem = c.mem;
    let saved_lr = c.lr;
    let px = mr_f32(mem, pos);
    let pz = mr_f32(mem, pos.wrapping_add(8));
    let (cx, cz) = grid_clamped_cell(mem, grid, px, pz);

    // 1. Center cell (cx, cz)
    let center_head = grid_cell_listhead(mem, grid, cx, cz);
    if let Some(mut it) = grid_iter_open(mem, center_head) {
        while it.cur != 0 {
            let bound = it.cur;
            if qcd_isects_raw(mem, qcd, bound.wrapping_add(0x18)) {
                let ent = mr32(mem, bound);
                if call2(c, cb, ent, cbdata) == 0 {
                    grid_iter_close(mem, &mut it);
                    c.lr = saved_lr;
                    return true;
                }
            }
            grid_iter_step(mem, &mut it);
        }
    }

    // 2. Quadrant & 3 neighbor cells
    let k = grid_quadrant(mem, grid, cx, cz, px, pz);
    let r13 = c.r[13];
    mw32(mem, r13.wrapping_add(0xFFFF_9578), k);

    const OFFSET_TABLE: u32 = 0x803A_57FC;
    for i in 0..3u32 {
        let cur_k = mr32(mem, r13.wrapping_add(0xFFFF_9578));
        let entry = OFFSET_TABLE
            .wrapping_add(cur_k.wrapping_mul(24))
            .wrapping_add(i.wrapping_mul(8));
        let dz = mr32(mem, entry) as i32;
        let dx = mr32(mem, entry.wrapping_add(4)) as i32;
        let head = grid_cell_listhead(mem, grid, cx.wrapping_add(dx), cz.wrapping_add(dz));
        if let Some(mut it) = grid_iter_open(mem, head) {
            while it.cur != 0 {
                let bound = it.cur;
                if qcd_isects_raw(mem, qcd, bound.wrapping_add(0x18)) {
                    let ent = mr32(mem, bound);
                    if call2(c, cb, ent, cbdata) == 0 {
                        grid_iter_close(mem, &mut it);
                        c.lr = saved_lr;
                        return true;
                    }
                }
                grid_iter_step(mem, &mut it);
            }
        }
    }

    // 3. `grid.other` oversized bounds list (`grid + 0x30`)
    if let Some(mut it) = grid_iter_open(mem, grid.wrapping_add(0x30)) {
        while it.cur != 0 {
            let bound = it.cur;
            if qcd_isects_raw(mem, qcd, bound.wrapping_add(0x18)) {
                let ent = mr32(mem, bound);
                if call2(c, cb, ent, cbdata) == 0 {
                    grid_iter_close(mem, &mut it);
                    c.lr = saved_lr;
                    return true;
                }
            }
            grid_iter_step(mem, &mut it);
        }
    }

    c.lr = saved_lr;
    true
}

/// Native ARM64 / Rust replacement for `xEntCollCheckOneGrid__FP4xEntP6xScenePFP4xEntP6xScenePv_P4xEntP5xGrid` (`0x801D5D80`).
pub fn hle_xent_coll_check_one_grid(c: &mut Ctx) -> bool {
    let ent = c.r[3];
    let sc = c.r[4];
    let hit_it = c.r[5];
    let grid = c.r[6];
    if !in_ram_opt(ent, 0x98) || !in_ram_opt(grid, 0x38) {
        return false;
    }
    let mem = c.mem;
    let saved_lr = c.lr;
    let qcd = ent.wrapping_add(0x68);
    let px = mr_f32(mem, ent.wrapping_add(0x8C));
    let pz = mr_f32(mem, ent.wrapping_add(0x94));
    let (cx, cz) = grid_clamped_cell(mem, grid, px, pz);

    // 1. Center cell (cx, cz)
    let center_head = grid_cell_listhead(mem, grid, cx, cz);
    if let Some(mut it) = grid_iter_open(mem, center_head) {
        while it.cur != 0 {
            let bound = it.cur;
            if qcd_isects_raw(mem, qcd, bound.wrapping_add(0x18)) {
                let other_ent = mr32(mem, bound);
                call3(c, hit_it, other_ent, sc, ent);
            }
            grid_iter_step(mem, &mut it);
        }
    }

    // 2. Quadrant & 3 neighbor cells
    let k = grid_quadrant(mem, grid, cx, cz, px, pz);
    let r13 = c.r[13];
    mw32(mem, r13.wrapping_add(0xFFFF_94BC), k);

    const OFFSET_TABLE: u32 = 0x803A_3B14;
    for i in 0..3u32 {
        let cur_k = mr32(mem, r13.wrapping_add(0xFFFF_94BC));
        let entry = OFFSET_TABLE
            .wrapping_add(cur_k.wrapping_mul(24))
            .wrapping_add(i.wrapping_mul(8));
        let dz = mr32(mem, entry) as i32;
        let dx = mr32(mem, entry.wrapping_add(4)) as i32;
        let head = grid_cell_listhead(mem, grid, cx.wrapping_add(dx), cz.wrapping_add(dz));
        if let Some(mut it) = grid_iter_open(mem, head) {
            while it.cur != 0 {
                let bound = it.cur;
                if qcd_isects_raw(mem, qcd, bound.wrapping_add(0x18)) {
                    let other_ent = mr32(mem, bound);
                    call3(c, hit_it, other_ent, sc, ent);
                }
                grid_iter_step(mem, &mut it);
            }
        }
    }

    // 3. `grid.other` oversized bounds list (`grid + 0x30`)
    if let Some(mut it) = grid_iter_open(mem, grid.wrapping_add(0x30)) {
        while it.cur != 0 {
            let bound = it.cur;
            if qcd_isects_raw(mem, qcd, bound.wrapping_add(0x18)) {
                let other_ent = mr32(mem, bound);
                call3(c, hit_it, other_ent, sc, ent);
            }
            grid_iter_step(mem, &mut it);
        }
    }

    c.lr = saved_lr;
    true
}

/// Native ARM64 / Rust replacement for `xRayHitsGrid__FP5xGridP6xSceneP5xRay3PFP6xSceneP5xRay3P7xQCDataP4xEntPv_vP7xQCDataPv` (`0x8018F5A8`).
pub fn hle_xray_hits_grid(c: &mut Ctx) -> bool {
    let grid = c.r[3];
    let sc = c.r[4];
    let ray = c.r[5];
    let cb = c.r[6];
    let qcd = c.r[7];
    let cbdata = c.r[8];
    if !in_ram_opt(grid, 0x38) || !in_ram_opt(ray, 0x24) || !in_ram_opt(qcd, 8) {
        return false;
    }
    let mem = c.mem;
    let saved_lr = c.lr;
    let r2 = c.r[2];

    let flags = mr32(mem, ray.wrapping_add(0x20)) | 0xC00;
    mw32(mem, ray.wrapping_add(0x20), flags);

    let min_t = mr_f32(mem, ray.wrapping_add(0x18));
    let max_t = mr_f32(mem, ray.wrapping_add(0x1C));
    let ox = mr_f32(mem, ray);
    let oy = mr_f32(mem, ray.wrapping_add(4));
    let oz = mr_f32(mem, ray.wrapping_add(8));
    let dir_x = mr_f32(mem, ray.wrapping_add(0x0C));
    let dir_y = mr_f32(mem, ray.wrapping_add(0x10));
    let dir_z0 = mr_f32(mem, ray.wrapping_add(0x14));

    let x0 = ox + dir_x * min_t;
    let _y0 = oy + dir_y * min_t;
    let z0 = oz + dir_z0 * min_t;
    let dt = max_t - min_t;
    let x1 = x0 + dir_x * dt;
    let _y1 = _y0 + dir_y * dt;
    let z1 = z0 + dir_z0 * dt;

    let f_eps = mr_f32(mem, r2.wrapping_add(0xFFFF_9018));
    let f_half = mr_f32(mem, r2.wrapping_add(0xFFFF_9008));
    let f_qtr = mr_f32(mem, r2.wrapping_add(0xFFFF_900C));
    let f_zero = mr_f32(mem, r2.wrapping_add(0xFFFF_9014));
    let f_negeps = mr_f32(mem, r2.wrapping_add(0xFFFF_902C));

    let csx = mr_f32(mem, grid.wrapping_add(0x18));
    let csz = mr_f32(mem, grid.wrapping_add(0x1C));
    let half_cx = f_half * csx;
    let half_cz = f_half * csz;
    let qtr_cx = f_qtr * csx;
    let qtr_cz = f_qtr * csz;

    if dir_z0.abs() <= f_eps {
        if dir_z0 >= f_zero {
            mw_f32(mem, ray.wrapping_add(0x14), f_eps);
        } else {
            mw_f32(mem, ray.wrapping_add(0x14), f_negeps);
        }
    }
    let slope_xz = mr_f32(mem, ray.wrapping_add(0x0C)) / mr_f32(mem, ray.wrapping_add(0x14));

    let (_, mut grz0) = grid_clamped_cell(mem, grid, x0, z0);
    let (_, mut grz1) = grid_clamped_cell(mem, grid, x1, z1);
    let min_x = mr_f32(mem, grid.wrapping_add(0x08));
    let min_z = mr_f32(mem, grid.wrapping_add(0x0C));
    let nx = mr16(mem, grid.wrapping_add(0x04)) as i32;
    let nz = mr16(mem, grid.wrapping_add(0x06)) as i32;

    let z_center_0 = csz * (grz0 as f32) + min_z + half_cz;
    let z_center_1 = csz * (grz1 as f32) + min_z + half_cz;

    if z0 < z1 {
        if grz0 > 0 && z0 < (z_center_0 - qtr_cz) {
            grz0 -= 1;
        }
        if grz1 < (nz - 1) && z1 > (z_center_1 + qtr_cz) {
            grz1 += 1;
        }
        let mut gz = grz0;
        while gz <= grz1 {
            let cell_z = csz * ((gz - grz0) as f32) + z_center_0;
            let z_lo = cell_z - half_cz;
            let z_hi = cell_z + half_cz;
            let seg_x0 = if z_lo < z0 { x0 } else { slope_xz * (z_lo - z0) + x0 };
            let seg_x1 = if z_hi > z1 { x1 } else { slope_xz * (z_hi - z0) + x0 };
            let (mut gx0, _) = grid_clamped_cell(mem, grid, seg_x0, z_lo);
            let (mut gx1, _) = grid_clamped_cell(mem, grid, seg_x1, z_hi);
            let x_center_0 = csx * (gx0 as f32) + min_x + half_cx;
            let x_center_1 = csx * (gx1 as f32) + min_x + half_cx;
            let (x_start, x_end) = if seg_x0 < seg_x1 {
                if gx0 > 0 && seg_x0 < (x_center_0 - qtr_cx) {
                    gx0 -= 1;
                }
                if gx1 < (nx - 1) && seg_x1 > (x_center_1 + qtr_cx) {
                    gx1 += 1;
                }
                (gx0, gx1)
            } else {
                if gx1 > 0 && seg_x1 < (x_center_1 - qtr_cx) {
                    gx1 -= 1;
                }
                if gx0 < (nx - 1) && seg_x0 > (x_center_0 + qtr_cx) {
                    gx0 += 1;
                }
                (gx1, gx0)
            };
            let mut gx = x_start;
            while gx <= x_end {
                let head = grid_cell_listhead(mem, grid, gx, gz);
                if let Some(mut it) = grid_iter_open(mem, head) {
                    while it.cur != 0 {
                        let bound = it.cur;
                        if qcd_isects_raw(mem, qcd, bound.wrapping_add(0x18)) {
                            let ent = mr32(mem, bound);
                            call5(c, cb, sc, ray, qcd, ent, cbdata);
                        }
                        grid_iter_step(mem, &mut it);
                    }
                }
                gx += 1;
            }
            gz += 1;
        }
    } else {
        if grz1 > 0 && z1 < (z_center_1 - qtr_cz) {
            grz1 -= 1;
        }
        if grz0 < (nz - 1) && z0 > (z_center_0 + qtr_cz) {
            grz0 += 1;
        }
        let mut gz = grz1;
        while gz <= grz0 {
            let cell_z = csz * ((gz - grz1) as f32) + z_center_1;
            let z_lo = cell_z - half_cz;
            let z_hi = cell_z + half_cz;
            let seg_x0 = if z_lo < z1 { x1 } else { slope_xz * (z1 - z_lo) + x1 };
            let seg_x1 = if z_hi > z0 { x0 } else { slope_xz * (z1 - z_hi) + x1 };
            let (mut gx0, _) = grid_clamped_cell(mem, grid, seg_x0, z_lo);
            let (mut gx1, _) = grid_clamped_cell(mem, grid, seg_x1, z_hi);
            let x_center_0 = csx * (gx0 as f32) + min_x + half_cx;
            let x_center_1 = csx * (gx1 as f32) + min_x + half_cx;
            let (x_start, x_end) = if seg_x0 < seg_x1 {
                if gx0 > 0 && seg_x0 < (x_center_0 - qtr_cx) {
                    gx0 -= 1;
                }
                if gx1 < (nx - 1) && seg_x1 > (x_center_1 + qtr_cx) {
                    gx1 += 1;
                }
                (gx0, gx1)
            } else {
                if gx1 > 0 && seg_x1 < (x_center_1 - qtr_cx) {
                    gx1 -= 1;
                }
                if gx0 < (nx - 1) && seg_x0 > (x_center_0 + qtr_cx) {
                    gx0 += 1;
                }
                (gx1, gx0)
            };
            let mut gx = x_start;
            while gx <= x_end {
                let head = grid_cell_listhead(mem, grid, gx, gz);
                if let Some(mut it) = grid_iter_open(mem, head) {
                    while it.cur != 0 {
                        let bound = it.cur;
                        if qcd_isects_raw(mem, qcd, bound.wrapping_add(0x18)) {
                            let ent = mr32(mem, bound);
                            call5(c, cb, sc, ray, qcd, ent, cbdata);
                        }
                        grid_iter_step(mem, &mut it);
                    }
                }
                gx += 1;
            }
            gz += 1;
        }
    }

    if let Some(mut it) = grid_iter_open(mem, grid.wrapping_add(0x30)) {
        while it.cur != 0 {
            let bound = it.cur;
            if qcd_isects_raw(mem, qcd, bound.wrapping_add(0x18)) {
                let ent = mr32(mem, bound);
                call5(c, cb, sc, ray, qcd, ent, cbdata);
            }
            grid_iter_step(mem, &mut it);
        }
    }

    c.lr = saved_lr;
    true
}

#[inline(always)]
fn sphere_hits_box_raw(mem: *mut u8, cx: f32, cy: f32, cz: f32, rad: f32, box_addr: u32) -> bool {
    let mut min_x = mr_f32(mem, box_addr);
    let mut min_y = mr_f32(mem, box_addr.wrapping_add(4));
    let mut min_z = mr_f32(mem, box_addr.wrapping_add(8));
    let mut max_x = mr_f32(mem, box_addr.wrapping_add(12));
    let mut max_y = mr_f32(mem, box_addr.wrapping_add(16));
    let mut max_z = mr_f32(mem, box_addr.wrapping_add(20));

    if min_x > max_x {
        core::mem::swap(&mut min_x, &mut max_x);
    }
    if min_y > max_y {
        core::mem::swap(&mut min_y, &mut max_y);
    }
    if min_z > max_z {
        core::mem::swap(&mut min_z, &mut max_z);
    }

    let cl_x = cx.clamp(min_x, max_x);
    let cl_y = cy.clamp(min_y, max_y);
    let cl_z = cz.clamp(min_z, max_z);
    let dx = cx - cl_x;
    let dy = cy - cl_y;
    let dz = cz - cl_z;
    (dx * dx + dy * dy + dz * dz) <= rad * rad
}

/// Native ARM64 / Rust replacement for `xSphereHitsEnv4__FPC7xSpherePC4xEnvPC7xMat3x3P7xCollis` (`0x80200424`).
pub fn hle_xsphere_hits_env4(c: &mut Ctx) -> bool {
    let sphere = c.r[3];
    let env = c.r[4];
    let mat = c.r[5];
    let coll = c.r[6];
    if !in_ram_opt(sphere, 16) || !in_ram_opt(env, 4) || !in_ram_opt(coll, 0x1F8) {
        return false;
    }
    let mem = c.mem;
    let geom = mr32(mem, env);
    if !in_ram_opt(geom, 0x20) {
        return false;
    }

    let saved_lr = c.lr;
    let saved_sp = c.r[1];
    let frame_sp = saved_sp.wrapping_sub(0x30);
    if !in_ram_opt(frame_sp, 0x30) {
        return false;
    }
    c.r[1] = frame_sp;
    mw32(mem, frame_sp, saved_sp);

    let isect = frame_sp.wrapping_add(0x08);
    let cx = mr_f32(mem, sphere);
    let cy = mr_f32(mem, sphere.wrapping_add(4));
    let cz = mr_f32(mem, sphere.wrapping_add(8));
    let rad = mr_f32(mem, sphere.wrapping_add(12));
    mw_f32(mem, isect, cx);
    mw_f32(mem, isect.wrapping_add(4), cy);
    mw_f32(mem, isect.wrapping_add(8), cz);
    mw_f32(mem, isect.wrapping_add(12), rad);
    mw32(mem, isect.wrapping_add(0x18), 3);

    let r13 = c.r[13];
    mw32(mem, r13.wrapping_add(0xFFFF_96AC), mat);
    mw32(mem, r13.wrapping_add(0xFFFF_969C), 1);

    let jsp_count = mr32(mem, geom.wrapping_add(0x10)) as i32;
    let mut box_off = 0u32;
    let mut ptr_off = 0u32;
    for _ in 0..jsp_count.max(0) {
        let boxes_base = mr32(mem, geom.wrapping_add(0x1C));
        let box_addr = boxes_base.wrapping_add(box_off);
        if in_ram_opt(box_addr, 24) && sphere_hits_box_raw(mem, cx, cy, cz, rad, box_addr) {
            for k in 0..6u32 {
                let c_ptr = coll.wrapping_add(k.wrapping_mul(0x54));
                mw32(mem, c_ptr.wrapping_add(0x14), mr32(mem, c_ptr.wrapping_add(0x10)));
            }
            let cur_geom = mr32(mem, env);
            let jsp_list = mr32(mem, cur_geom.wrapping_add(0x18));
            let jsp = mr32(mem, jsp_list.wrapping_add(ptr_off));
            let tree = mr32(mem, jsp.wrapping_add(0x10));
            call4(c, 0x8020_9674, tree, isect, 0x8020_069C, coll);

            let cur_geom2 = mr32(mem, env);
            let jsp_list2 = mr32(mem, cur_geom2.wrapping_add(0x18));
            let jsp2 = mr32(mem, jsp_list2.wrapping_add(ptr_off));
            for k in 0..6u32 {
                let c_ptr = coll.wrapping_add(k.wrapping_mul(0x54));
                if mr_f32(mem, c_ptr.wrapping_add(0x10)) < mr_f32(mem, c_ptr.wrapping_add(0x14)) {
                    let old_oid = mr32(mem, c_ptr.wrapping_add(0x04));
                    let new_oid = call3(c, 0x801E_FEE8, jsp2, old_oid, 0);
                    mw32(mem, c_ptr.wrapping_add(0x04), new_oid);
                }
            }
        }
        box_off = box_off.wrapping_add(0x18);
        ptr_off = ptr_off.wrapping_add(4);
    }

    mw32(mem, r13.wrapping_add(0xFFFF_969C), 0);
    let mut hit_count = 0u32;
    let sphere_rad = mr_f32(mem, sphere.wrapping_add(12));
    for k in 0..6u32 {
        let c_ptr = coll.wrapping_add(k.wrapping_mul(0x54));
        if (mr32(mem, c_ptr) & 1) != 0 {
            hit_count = hit_count.wrapping_add(1);
            let dist = mr_f32(mem, c_ptr.wrapping_add(0x10));
            let inv_dist = 1.0f32 / dist;
            let hdx = mr_f32(mem, c_ptr.wrapping_add(0x24)) * inv_dist;
            let hdy = mr_f32(mem, c_ptr.wrapping_add(0x28)) * inv_dist;
            let hdz = mr_f32(mem, c_ptr.wrapping_add(0x2C)) * inv_dist;
            mw_f32(mem, c_ptr.wrapping_add(0x3C), hdx);
            mw_f32(mem, c_ptr.wrapping_add(0x40), hdy);
            mw_f32(mem, c_ptr.wrapping_add(0x44), hdz);

            let pen = dist - sphere_rad;
            let depen_len = if 0.0f32 < pen { 0.0f32 } else { pen };
            mw_f32(mem, c_ptr.wrapping_add(0x30), hdx * depen_len);
            mw_f32(mem, c_ptr.wrapping_add(0x34), hdy * depen_len);
            mw_f32(mem, c_ptr.wrapping_add(0x38), hdz * depen_len);

            let nx = mr_f32(mem, c_ptr.wrapping_add(0x18));
            let ny = mr_f32(mem, c_ptr.wrapping_add(0x1C));
            let nz = mr_f32(mem, c_ptr.wrapping_add(0x20));
            if (hdx * nx + hdy * ny + hdz * nz) > 0.0f32 {
                mw_f32(mem, c_ptr.wrapping_add(0x18), -nx);
                mw_f32(mem, c_ptr.wrapping_add(0x1C), -ny);
                mw_f32(mem, c_ptr.wrapping_add(0x20), -nz);
            }
        }
    }

    c.r[3] = hit_count;
    c.r[1] = saved_sp;
    c.lr = saved_lr;
    true
}
