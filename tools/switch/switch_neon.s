// Switch AArch64 NEON Assembly Acceleration for TSSM
// Optimized for Cortex-A57 (ARMv8-A, 128-bit NEON SIMD)
// Directly handles GameCube big-endian guest memory with rev32

.text
.align 4

// void switch_neon_matrix_concat_be(void* mem, uint32_t a, uint32_t b, uint32_t ab)
// x0 = mem, w1 = a, w2 = b, w3 = ab
.global switch_neon_matrix_concat_be
.type switch_neon_matrix_concat_be, %function
switch_neon_matrix_concat_be:
    and w1, w1, #0x07FFFFFF
    add x1, x0, x1
    and w2, w2, #0x07FFFFFF
    add x2, x0, x2
    and w3, w3, #0x07FFFFFF
    add x3, x0, x3

    // Load 3 rows of b (16 bytes each) and byte-reverse to host f32
    ld1 {v4.16b}, [x2], #16
    ld1 {v5.16b}, [x2], #16
    ld1 {v6.16b}, [x2]
    rev32 v4.16b, v4.16b
    rev32 v5.16b, v5.16b
    rev32 v6.16b, v6.16b

    // --- Row 0 of a ---
    ld1 {v0.16b}, [x1], #16
    rev32 v0.16b, v0.16b
    fmul v1.4s, v4.4s, v0.s[0]
    fmla v1.4s, v5.4s, v0.s[1]
    fmla v1.4s, v6.4s, v0.s[2]
    mov  s2, v0.s[3]
    mov  s3, v1.s[3]
    fadd s2, s2, s3
    ins  v1.s[3], v2.s[0]
    rev32 v1.16b, v1.16b
    st1 {v1.16b}, [x3], #16

    // --- Row 1 of a ---
    ld1 {v0.16b}, [x1], #16
    rev32 v0.16b, v0.16b
    fmul v1.4s, v4.4s, v0.s[0]
    fmla v1.4s, v5.4s, v0.s[1]
    fmla v1.4s, v6.4s, v0.s[2]
    mov  s2, v0.s[3]
    mov  s3, v1.s[3]
    fadd s2, s2, s3
    ins  v1.s[3], v2.s[0]
    rev32 v1.16b, v1.16b
    st1 {v1.16b}, [x3], #16

    // --- Row 2 of a ---
    ld1 {v0.16b}, [x1]
    rev32 v0.16b, v0.16b
    fmul v1.4s, v4.4s, v0.s[0]
    fmla v1.4s, v5.4s, v0.s[1]
    fmla v1.4s, v6.4s, v0.s[2]
    mov  s2, v0.s[3]
    mov  s3, v1.s[3]
    fadd s2, s2, s3
    ins  v1.s[3], v2.s[0]
    rev32 v1.16b, v1.16b
    st1 {v1.16b}, [x3]

    ret

// void switch_neon_matrix_mult_be(void* mem, uint32_t dst, uint32_t a, uint32_t b)
// x0 = mem, w1 = dst, w2 = a, w3 = b
.global switch_neon_matrix_mult_be
.type switch_neon_matrix_mult_be, %function
switch_neon_matrix_mult_be:
    and w1, w1, #0x07FFFFFF
    add x1, x0, x1
    and w2, w2, #0x07FFFFFF
    add x2, x0, x2
    and w3, w3, #0x07FFFFFF
    add x3, x0, x3

    // Load b: 4 rows of 16 bytes
    ld1 {v4.16b}, [x3], #16
    ld1 {v5.16b}, [x3], #16
    ld1 {v6.16b}, [x3], #16
    ld1 {v7.16b}, [x3]
    rev32 v4.16b, v4.16b
    rev32 v5.16b, v5.16b
    rev32 v6.16b, v6.16b
    rev32 v7.16b, v7.16b

    // Load a: 4 rows of 16 bytes
    ld1 {v0.16b}, [x2], #16
    ld1 {v1.16b}, [x2], #16
    ld1 {v2.16b}, [x2], #16
    ld1 {v3.16b}, [x2]
    rev32 v0.16b, v0.16b
    rev32 v1.16b, v1.16b
    rev32 v2.16b, v2.16b
    rev32 v3.16b, v3.16b

    // out0 = a0[0]*b0 + a0[1]*b1 + a0[2]*b2
    fmul v16.4s, v4.4s, v0.s[0]
    fmla v16.4s, v5.4s, v0.s[1]
    fmla v16.4s, v6.4s, v0.s[2]

    // out1 = a1[0]*b0 + a1[1]*b1 + a1[2]*b2
    fmul v17.4s, v4.4s, v1.s[0]
    fmla v17.4s, v5.4s, v1.s[1]
    fmla v17.4s, v6.4s, v1.s[2]

    // out2 = a2[0]*b0 + a2[1]*b1 + a2[2]*b2
    fmul v18.4s, v4.4s, v2.s[0]
    fmla v18.4s, v5.4s, v2.s[1]
    fmla v18.4s, v6.4s, v2.s[2]

    // out3 = a3[0]*b0 + a3[1]*b1 + a3[2]*b2 + b3
    fmul v19.4s, v4.4s, v3.s[0]
    fmla v19.4s, v5.4s, v3.s[1]
    fmla v19.4s, v6.4s, v3.s[2]
    fadd v19.4s, v19.4s, v7.4s

    // Write back 3 floats (xyz) for rows 0, 1, 2
    rev32 v16.16b, v16.16b
    rev32 v17.16b, v17.16b
    rev32 v18.16b, v18.16b
    rev32 v19.16b, v19.16b

    // Store row 0 (x, y, z) without overwriting flags at +12
    st1 {v16.s}[0], [x1], #4
    st1 {v16.s}[1], [x1], #4
    st1 {v16.s}[2], [x1], #4
    add x1, x1, #4

    // Store row 1
    st1 {v17.s}[0], [x1], #4
    st1 {v17.s}[1], [x1], #4
    st1 {v17.s}[2], [x1], #4
    add x1, x1, #4

    // Store row 2
    st1 {v18.s}[0], [x1], #4
    st1 {v18.s}[1], [x1], #4
    st1 {v18.s}[2], [x1], #4
    add x1, x1, #4

    // Store row 3 (pos)
    st1 {v19.s}[0], [x1], #4
    st1 {v19.s}[1], [x1], #4
    st1 {v19.s}[2], [x1]

    ret

// uint32_t switch_str_hash_be(const void* mem, uint32_t str_ea)
// x0 = mem, w1 = str_ea
.global switch_str_hash_be
.type switch_str_hash_be, %function
switch_str_hash_be:
    and w1, w1, #0x07FFFFFF
    add x0, x0, x1
    mov w1, #0              // h = 0
    mov w4, #131
.Lhash_loop:
    ldrb w2, [x0], #1       // c = *str++
    cbz  w2, .Lhash_done
    lsr  w3, w2, #1          // c >> 1
    and  w3, w2, w3          // c & (c >> 1)
    and  w3, w3, #0x20       // sub = (c & (c >> 1)) & 0x20
    sub  w2, w2, w3          // folded = c - sub
    and  w2, w2, #0xFF
    madd w1, w1, w4, w2      // h = h * 131 + folded
    b .Lhash_loop
.Lhash_done:
    mov w0, w1
    ret

// uint32_t switch_str_hash_n_be(const void* mem, uint32_t str_ea, uint32_t max_len)
// x0 = mem, w1 = str_ea, w2 = max_len
.global switch_str_hash_n_be
.type switch_str_hash_n_be, %function
switch_str_hash_n_be:
    cbz w2, .Lhashn_zero
    and w1, w1, #0x07FFFFFF
    add x0, x0, x1
    mov w3, #0              // h = 0
    mov w5, #131
.Lhashn_loop:
    ldrb w4, [x0], #1
    cbz  w4, .Lhashn_done
    lsr  w6, w4, #1
    and  w6, w4, w6
    and  w6, w6, #0x20
    sub  w4, w4, w6
    and  w4, w4, #0xFF
    madd w3, w3, w5, w4
    subs w2, w2, #1
    bne .Lhashn_loop
.Lhashn_done:
    mov w0, w3
    ret
.Lhashn_zero:
    mov w0, #0
    ret

// void switch_neon_skin_blend_update_be(void* mem, uint32_t out_mtx, uint32_t inv_bone_mtx, uint32_t anim_node_mtx, uint32_t root_mtx, uint32_t bone_indices, uint32_t num_bones)
// x0 = mem, w1 = out_mtx, w2 = inv_bone_mtx, w3 = anim_node_mtx, w4 = root_mtx, w5 = bone_indices, w6 = num_bones
.global switch_neon_skin_blend_update_be
.type switch_neon_skin_blend_update_be, %function
switch_neon_skin_blend_update_be:
    cbz  w6, .Lskin_done

    and  w1, w1, #0x07FFFFFF
    add  x1, x0, x1
    and  w2, w2, #0x07FFFFFF
    add  x2, x0, x2
    and  w3, w3, #0x07FFFFFF
    add  x3, x0, x3
    and  w4, w4, #0x07FFFFFF
    add  x4, x0, x4
    and  w5, w5, #0x07FFFFFF
    add  x5, x0, x5

    // Load root matrix (4 rows = 64 bytes) once, keep in v4-v7 throughout
    ld1 {v4.16b}, [x4], #16
    ld1 {v5.16b}, [x4], #16
    ld1 {v6.16b}, [x4], #16
    ld1 {v7.16b}, [x4]
    rev32 v4.16b, v4.16b
    rev32 v5.16b, v5.16b
    rev32 v6.16b, v6.16b
    rev32 v7.16b, v7.16b

.Lskin_loop:
    ldrb w8, [x5], #1               // bone index
    lsl  w8, w8, #6                 // off = bone_index * 64
    and  x8, x8, #0x07FFFFFF

    // Load anim matrix (4 rows)
    add  x9, x3, x8
    ld1 {v0.16b}, [x9], #16
    ld1 {v1.16b}, [x9], #16
    ld1 {v2.16b}, [x9], #16
    ld1 {v3.16b}, [x9]
    rev32 v0.16b, v0.16b
    rev32 v1.16b, v1.16b
    rev32 v2.16b, v2.16b
    rev32 v3.16b, v3.16b

    // tmp = anim * root
    // row 0:
    fmul v16.4s, v4.4s, v0.s[0]
    fmla v16.4s, v5.4s, v0.s[1]
    fmla v16.4s, v6.4s, v0.s[2]

    // row 1:
    fmul v17.4s, v4.4s, v1.s[0]
    fmla v17.4s, v5.4s, v1.s[1]
    fmla v17.4s, v6.4s, v1.s[2]

    // row 2:
    fmul v18.4s, v4.4s, v2.s[0]
    fmla v18.4s, v5.4s, v2.s[1]
    fmla v18.4s, v6.4s, v2.s[2]

    // row 3:
    fmul v19.4s, v4.4s, v3.s[0]
    fmla v19.4s, v5.4s, v3.s[1]
    fmla v19.4s, v6.4s, v3.s[2]
    fadd v19.4s, v19.4s, v7.4s

    // Load inv matrix (4 rows)
    add  x9, x2, x8
    ld1 {v0.16b}, [x9], #16
    ld1 {v1.16b}, [x9], #16
    ld1 {v2.16b}, [x9], #16
    ld1 {v3.16b}, [x9]
    rev32 v0.16b, v0.16b
    rev32 v1.16b, v1.16b
    rev32 v2.16b, v2.16b
    rev32 v3.16b, v3.16b

    // res = inv * tmp
    // row 0:
    fmul v20.4s, v16.4s, v0.s[0]
    fmla v20.4s, v17.4s, v0.s[1]
    fmla v20.4s, v18.4s, v0.s[2]

    // row 1:
    fmul v21.4s, v16.4s, v1.s[0]
    fmla v21.4s, v17.4s, v1.s[1]
    fmla v21.4s, v18.4s, v1.s[2]

    // row 2:
    fmul v22.4s, v16.4s, v2.s[0]
    fmla v22.4s, v17.4s, v2.s[1]
    fmla v22.4s, v18.4s, v2.s[2]

    // row 3:
    fmul v23.4s, v16.4s, v3.s[0]
    fmla v23.4s, v17.4s, v3.s[1]
    fmla v23.4s, v18.4s, v3.s[2]
    fadd v23.4s, v23.4s, v19.4s

    // rev32 to big-endian
    rev32 v20.16b, v20.16b
    rev32 v21.16b, v21.16b
    rev32 v22.16b, v22.16b
    rev32 v23.16b, v23.16b

    // Store xyz (12 bytes) of rows 0..3 to out_mtx + off
    add  x9, x1, x8
    st1 {v20.s}[0], [x9], #4
    st1 {v20.s}[1], [x9], #4
    st1 {v20.s}[2], [x9], #4
    add  x9, x9, #4

    st1 {v21.s}[0], [x9], #4
    st1 {v21.s}[1], [x9], #4
    st1 {v21.s}[2], [x9], #4
    add  x9, x9, #4

    st1 {v22.s}[0], [x9], #4
    st1 {v22.s}[1], [x9], #4
    st1 {v22.s}[2], [x9], #4
    add  x9, x9, #4

    st1 {v23.s}[0], [x9], #4
    st1 {v23.s}[1], [x9], #4
    st1 {v23.s}[2], [x9]

    subs w6, w6, #1
    bne  .Lskin_loop

.Lskin_done:
    ret

// void switch_neon_transform_points_be(void* mem, uint32_t pts_out, uint32_t pts_in, int32_t n, uint32_t mtx)
// x0 = mem, w1 = pts_out, w2 = pts_in, w3 = n, w4 = mtx
.global switch_neon_transform_points_be
.type switch_neon_transform_points_be, %function
switch_neon_transform_points_be:
    cmp  w3, #0
    ble  .Lpts_done

    and  w1, w1, #0x07FFFFFF
    add  x1, x0, x1
    and  w2, w2, #0x07FFFFFF
    add  x2, x0, x2
    and  w4, w4, #0x07FFFFFF
    add  x4, x0, x4

    // Load matrix: m0 (right), m1 (up), m2 (at), m3 (pos)
    ld1 {v4.16b}, [x4], #16
    ld1 {v5.16b}, [x4], #16
    ld1 {v6.16b}, [x4], #16
    ld1 {v7.16b}, [x4]
    rev32 v4.16b, v4.16b
    rev32 v5.16b, v5.16b
    rev32 v6.16b, v6.16b
    rev32 v7.16b, v7.16b

    // Unroll 2 vertices per iteration if n >= 2
    subs w3, w3, #2
    blt  .Lpts_tail

.Lpts_loop2:
    // Vertex 0: load 3 floats
    ld1 {v0.s}[0], [x2], #4
    ld1 {v0.s}[1], [x2], #4
    ld1 {v0.s}[2], [x2], #4

    // Vertex 1: load 3 floats
    ld1 {v2.s}[0], [x2], #4
    ld1 {v2.s}[1], [x2], #4
    ld1 {v2.s}[2], [x2], #4

    rev32 v0.16b, v0.16b
    rev32 v2.16b, v2.16b

    // Dual-pipeline transform
    fmul v1.4s, v4.4s, v0.s[0]
    fmul v3.4s, v4.4s, v2.s[0]
    fmla v1.4s, v5.4s, v0.s[1]
    fmla v3.4s, v5.4s, v2.s[1]
    fmla v1.4s, v6.4s, v0.s[2]
    fmla v3.4s, v6.4s, v2.s[2]
    fadd v1.4s, v1.4s, v7.4s
    fadd v3.4s, v3.4s, v7.4s

    rev32 v1.16b, v1.16b
    rev32 v3.16b, v3.16b

    // Store Vertex 0
    st1 {v1.s}[0], [x1], #4
    st1 {v1.s}[1], [x1], #4
    st1 {v1.s}[2], [x1], #4

    // Store Vertex 1
    st1 {v3.s}[0], [x1], #4
    st1 {v3.s}[1], [x1], #4
    st1 {v3.s}[2], [x1], #4

    subs w3, w3, #2
    bge  .Lpts_loop2

.Lpts_tail:
    adds w3, w3, #2
    ble  .Lpts_done

    // Single remainder vertex
    ld1 {v0.s}[0], [x2], #4
    ld1 {v0.s}[1], [x2], #4
    ld1 {v0.s}[2], [x2], #4
    rev32 v0.16b, v0.16b

    fmul v1.4s, v4.4s, v0.s[0]
    fmla v1.4s, v5.4s, v0.s[1]
    fmla v1.4s, v6.4s, v0.s[2]
    fadd v1.4s, v1.4s, v7.4s

    rev32 v1.16b, v1.16b
    st1 {v1.s}[0], [x1], #4
    st1 {v1.s}[1], [x1], #4
    st1 {v1.s}[2], [x1], #4

.Lpts_done:
    ret

// void switch_neon_transform_vectors_be(void* mem, uint32_t vecs_out, uint32_t vecs_in, int32_t n, uint32_t mtx)
// x0 = mem, w1 = vecs_out, w2 = vecs_in, w3 = n, w4 = mtx
.global switch_neon_transform_vectors_be
.type switch_neon_transform_vectors_be, %function
switch_neon_transform_vectors_be:
    cmp  w3, #0
    ble  .Lvecs_done

    and  w1, w1, #0x07FFFFFF
    add  x1, x0, x1
    and  w2, w2, #0x07FFFFFF
    add  x2, x0, x2
    and  w4, w4, #0x07FFFFFF
    add  x4, x0, x4

    // Load matrix rows: m0, m1, m2 (no translation needed)
    ld1 {v4.16b}, [x4], #16
    ld1 {v5.16b}, [x4], #16
    ld1 {v6.16b}, [x4]
    rev32 v4.16b, v4.16b
    rev32 v5.16b, v5.16b
    rev32 v6.16b, v6.16b

    subs w3, w3, #2
    blt  .Lvecs_tail

.Lvecs_loop2:
    ld1 {v0.s}[0], [x2], #4
    ld1 {v0.s}[1], [x2], #4
    ld1 {v0.s}[2], [x2], #4

    ld1 {v2.s}[0], [x2], #4
    ld1 {v2.s}[1], [x2], #4
    ld1 {v2.s}[2], [x2], #4

    rev32 v0.16b, v0.16b
    rev32 v2.16b, v2.16b

    fmul v1.4s, v4.4s, v0.s[0]
    fmul v3.4s, v4.4s, v2.s[0]
    fmla v1.4s, v5.4s, v0.s[1]
    fmla v3.4s, v5.4s, v2.s[1]
    fmla v1.4s, v6.4s, v0.s[2]
    fmla v3.4s, v6.4s, v2.s[2]

    rev32 v1.16b, v1.16b
    rev32 v3.16b, v3.16b

    st1 {v1.s}[0], [x1], #4
    st1 {v1.s}[1], [x1], #4
    st1 {v1.s}[2], [x1], #4

    st1 {v3.s}[0], [x1], #4
    st1 {v3.s}[1], [x1], #4
    st1 {v3.s}[2], [x1], #4

    subs w3, w3, #2
    bge  .Lvecs_loop2

.Lvecs_tail:
    adds w3, w3, #2
    ble  .Lvecs_done

    ld1 {v0.s}[0], [x2], #4
    ld1 {v0.s}[1], [x2], #4
    ld1 {v0.s}[2], [x2], #4
    rev32 v0.16b, v0.16b

    fmul v1.4s, v4.4s, v0.s[0]
    fmla v1.4s, v5.4s, v0.s[1]
    fmla v1.4s, v6.4s, v0.s[2]

    rev32 v1.16b, v1.16b
    st1 {v1.s}[0], [x1], #4
    st1 {v1.s}[1], [x1], #4
    st1 {v1.s}[2], [x1], #4

.Lvecs_done:
    ret
