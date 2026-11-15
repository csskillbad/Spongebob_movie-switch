// Switch C++ Acceleration Engine for TSSM
// High-performance routines for asset scanning, byte manipulation, and bone blending

#include <stdint.h>
#include <stddef.h>
#include <string.h>
#include <arm_neon.h>

extern "C" {

// Fast bit-scan asset finder: replaces 32-step software loop with single-cycle instruction
int32_t switch_find_asset_by_type(uint32_t mask, int32_t target_idx) {
    if (mask == 0 || target_idx < 0) return -1;
    int32_t count = 0;
    uint32_t m = mask;
    while (m != 0) {
        int bit = __builtin_ctz(m);
        if (count == target_idx) {
            return bit;
        }
        count++;
        m &= m - 1; // clear lowest set bit
    }
    return -1;
}

// Bulk big-endian 32-bit swap for vertex / index arrays using 128-bit NEON
void switch_bswap32_stream(uint32_t* dst, const uint32_t* src, uint32_t count) {
    uint32_t i = 0;
    // Process 4 uint32s (16 bytes) per iteration
    for (; i + 4 <= count; i += 4) {
        uint8x16_t v = vld1q_u8(reinterpret_cast<const uint8_t*>(src + i));
        // Reverse bytes in each 32-bit element
        uint8x16_t rev = vrev32q_u8(v);
        vst1q_u8(reinterpret_cast<uint8_t*>(dst + i), rev);
    }
    // Remainder
    for (; i < count; i++) {
        dst[i] = __builtin_bswap32(src[i]);
    }
}

// Bulk big-endian 16-bit swap for index arrays using 128-bit NEON
void switch_bswap16_stream(uint16_t* dst, const uint16_t* src, uint32_t count) {
    uint32_t i = 0;
    // Process 8 uint16s (16 bytes) per iteration
    for (; i + 8 <= count; i += 8) {
        uint8x16_t v = vld1q_u8(reinterpret_cast<const uint8_t*>(src + i));
        uint8x16_t rev = vrev16q_u8(v);
        vst1q_u8(reinterpret_cast<uint8_t*>(dst + i), rev);
    }
    for (; i < count; i++) {
        dst[i] = __builtin_bswap16(src[i]);
    }
}

// Optimized skeletal bone matrix blending (_rpSkinMatrixBlendUpdateASM)
// Blends an array of weighted bone transform matrices into a single world matrix
void switch_skin_blend_matrices(
    float* dst_mtx,              // 4x3 row-major output matrix
    const float* bone_matrices,  // array of 4x3 bone matrices
    const uint8_t* bone_indices, // bone index per weight
    const float* weights,        // normalized float weights
    uint32_t num_weights
) {
    float32x4_t r0 = vdupq_n_f32(0.0f);
    float32x4_t r1 = vdupq_n_f32(0.0f);
    float32x4_t r2 = vdupq_n_f32(0.0f);

    for (uint32_t w = 0; w < num_weights; w++) {
        float weight = weights[w];
        if (weight <= 0.0f) continue;

        uint32_t bone_idx = bone_indices[w];
        const float* bm = bone_matrices + bone_idx * 12;

        float32x4_t b0 = vld1q_f32(bm);
        float32x4_t b1 = vld1q_f32(bm + 4);
        float32x4_t b2 = vld1q_f32(bm + 8);

        float32x4_t wvec = vdupq_n_f32(weight);
        r0 = vmlaq_f32(r0, b0, wvec);
        r1 = vmlaq_f32(r1, b1, wvec);
        r2 = vmlaq_f32(r2, b2, wvec);
    }

    vst1q_f32(dst_mtx, r0);
    vst1q_f32(dst_mtx + 4, r1);
    vst1q_f32(dst_mtx + 8, r2);
}

// Vectorized 3D AABB bounding calculation
void switch_calc_aabb(
    float* out_min,
    float* out_max,
    const float* positions,
    uint32_t count,
    uint32_t stride_bytes
) {
    if (count == 0) return;

    float32x4_t vmin = {positions[0], positions[1], positions[2], 0.0f};
    float32x4_t vmax = vmin;

    const uint8_t* ptr = reinterpret_cast<const uint8_t*>(positions);
    for (uint32_t i = 0; i < count; i++) {
        const float* p = reinterpret_cast<const float*>(ptr);
        float32x4_t v = {p[0], p[1], p[2], 0.0f};
        vmin = vminq_f32(vmin, v);
        vmax = vmaxq_f32(vmax, v);
        ptr += stride_bytes;
    }

    out_min[0] = vgetq_lane_f32(vmin, 0);
    out_min[1] = vgetq_lane_f32(vmin, 1);
    out_min[2] = vgetq_lane_f32(vmin, 2);

    out_max[0] = vgetq_lane_f32(vmax, 0);
    out_max[1] = vgetq_lane_f32(vmax, 1);
    out_max[2] = vgetq_lane_f32(vmax, 2);
}

} // extern "C"
