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
