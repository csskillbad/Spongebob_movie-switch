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
