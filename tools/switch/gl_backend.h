#ifndef TSSM_SWITCH_GL_BACKEND_H
#define TSSM_SWITCH_GL_BACKEND_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    uint32_t program;
    uint32_t state_bits;
    uint32_t first_index;
    uint32_t index_count;
    int32_t  base_vertex;
    uint8_t  geom_kind; // 0 = Transformed, 1 = Raw, 2 = Cached
    uint8_t  pad[3];
    uint32_t params_offset;
    uint32_t vparams_offset;
    uint32_t textures[8];
    uint32_t samplers[8];
    float    viewport[4];
    int32_t  scissor[4];
} GlDrawCmd;

typedef struct {
    const void*      vertices;
    uint32_t         vertices_bytes;
    const void*      raw_vertices;
    uint32_t         raw_bytes;
    const void*      indices;
    uint32_t         indices_bytes;
    const void*      params;
    uint32_t         params_bytes;
    const void*      vparams;
    uint32_t         vparams_bytes;
    const GlDrawCmd* cmds;
    uint32_t         cmd_count;
    uint32_t         has_clear;
    float            clear_color[4];
} GlPassDesc;

int      switch_gl_init(uint32_t efb_w, uint32_t efb_h, uint32_t cache_verts, uint32_t cache_indices);
void     switch_gl_shutdown(void);
void     switch_gl_resize_efb(uint32_t efb_w, uint32_t efb_h);
void     switch_gl_get_window_size(uint32_t* out_w, uint32_t* out_h);

uint32_t switch_gl_compile_fs(const char* src_ptr, uint32_t src_len);
uint32_t switch_gl_link_program(uint32_t fs_id, uint32_t is_raw);

uint32_t switch_gl_upload_texture(uint32_t existing_tex, uint32_t w, uint32_t h, const uint8_t* rgba);
uint32_t switch_gl_create_sampler(uint8_t wrap_s, uint8_t wrap_t, uint8_t linear);

void     switch_gl_cache_upload(
    uint32_t vert_offset_elems,
    const void* verts,
    uint32_t vert_bytes,
    uint32_t idx_offset_elems,
    const void* indices,
    uint32_t idx_bytes
);

void     switch_gl_execute_pass(const GlPassDesc* pass);
uint32_t switch_gl_copy_efb(uint32_t existing_tex, uint32_t alloc_new_size, uint32_t x, uint32_t y, uint32_t w, uint32_t h);
uint32_t switch_gl_efb_color_tex(void);

void     switch_gl_present(
    uint32_t src_tex,
    const float uv_rect[4],
    float picture_aspect,
    const void* overlay_verts,
    uint32_t overlay_vert_count
);

void     switch_gl_get_perf_stats(
    uint64_t* out_ring_upload_ns,
    uint64_t* out_state_apply_ns,
    uint64_t* out_draw_calls_ns,
    uint64_t* out_present_blit_ns,
    uint64_t* out_swap_buffers_ns,
    uint32_t* out_draw_count,
    uint32_t* out_batch_count
);

#ifdef __cplusplus
}
#endif

#endif // TSSM_SWITCH_GL_BACKEND_H
