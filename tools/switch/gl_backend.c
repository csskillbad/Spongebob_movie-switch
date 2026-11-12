#include "gl_backend.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdbool.h>
#include <switch.h>
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <glad/glad.h>

extern void switch_log(const char* msg);
extern uint32_t switch_is_docked_mode(void);

#define VERTEX_STRIDE     112u
#define RAW_VERTEX_STRIDE  96u
#define OVERLAY_STRIDE     24u

#define MAX_PAYLOADS       16u
#define GL_QUEUE_CAPACITY 256u

// --- Dynamic Pass Payload ---
typedef struct {
    uint8_t* vert_buf;
    uint32_t vert_cap;
    uint32_t vert_bytes;

    uint8_t* raw_buf;
    uint32_t raw_cap;
    uint32_t raw_bytes;

    uint8_t* idx_buf;
    uint32_t idx_cap;
    uint32_t idx_bytes;

    uint8_t* params_buf;
    uint32_t params_cap;
    uint32_t params_bytes;

    uint8_t* vparams_buf;
    uint32_t vparams_cap;
    uint32_t vparams_bytes;

    GlDrawCmd* cmds;
    uint32_t   cmds_cap;
    uint32_t   cmd_count;

    uint32_t  has_clear;
    float     clear_color[4];
} PassPayload;

static PassPayload s_pass_payloads[MAX_PAYLOADS];
static uint32_t    s_pass_head = 0;
static uint32_t    s_pass_tail = 0;

static uint8_t  s_overlay_buf[2][4096 * OVERLAY_STRIDE];
static uint32_t s_overlay_slot = 0;

// --- Command Queue ---
typedef enum {
    GL_CMD_NONE = 0,
    GL_CMD_COMPILE_FS,
    GL_CMD_LINK_PROGRAM,
    GL_CMD_UPLOAD_TEXTURE,
    GL_CMD_CREATE_SAMPLER,
    GL_CMD_CACHE_UPLOAD,
    GL_CMD_RESIZE_EFB,
    GL_CMD_EXECUTE_PASS,
    GL_CMD_COPY_EFB,
    GL_CMD_PRESENT
} GlCmdType;

typedef struct {
    GlCmdType type;
    union {
        struct {
            const char* src_ptr;
            uint32_t    src_len;
        } compile_fs;
        struct {
            uint32_t fs_id;
            uint32_t is_raw;
        } link_prog;
        struct {
            uint32_t existing_tex;
            uint32_t w;
            uint32_t h;
            const uint8_t* rgba;
        } upload_tex;
        struct {
            uint8_t wrap_s;
            uint8_t wrap_t;
            uint8_t linear;
        } create_sampler;
        struct {
            uint32_t vert_offset_elems;
            const void* verts;
            uint32_t vert_bytes;
            uint32_t idx_offset_elems;
            const void* indices;
            uint32_t idx_bytes;
        } cache_upload;
        struct {
            uint32_t efb_w;
            uint32_t efb_h;
        } resize_efb;
        struct {
            uint32_t payload_idx;
        } pass;
        struct {
            uint32_t tex;
            uint32_t alloc_new_size;
            uint32_t x;
            uint32_t y;
            uint32_t w;
            uint32_t h;
        } copy_efb;
        struct {
            uint32_t src_tex;
            float    uv_rect[4];
            float    picture_aspect;
            uint8_t* overlay_verts;
            uint32_t overlay_vert_count;
        } present;
    };
    uint32_t* out_u32;
} GlCommand;

static GlCommand s_queue[GL_QUEUE_CAPACITY];
static uint32_t  s_queue_head = 0;
static uint32_t  s_queue_tail = 0;

static Mutex   s_gl_mutex;
static CondVar s_gl_cond_work;
static CondVar s_gl_cond_queue_free;
static CondVar s_gl_cond_payload_free;
static CondVar s_gl_cond_frame_done;
static CondVar s_gl_cond_sync_done;

static Thread   s_gl_thread;
static bool     s_gl_thread_initialized = false;
static bool     s_gl_exit = false;
static int      s_gl_init_result = 0;
static uint32_t s_frames_in_flight = 0;

static uint32_t s_init_efb_w = 640;
static uint32_t s_init_efb_h = 528;
static uint32_t s_cache_verts = 2000000;
static uint32_t s_cache_indices = 8000000;

// Pre-generated texture pool for zero-stall EFB copy
static GLuint   s_pregen_tex[256];
static uint32_t s_pregen_tex_count = 0;

// EGL and GL context (owned by s_gl_thread on Core 2)
static EGLDisplay s_display = EGL_NO_DISPLAY;
static EGLContext s_context = EGL_NO_CONTEXT;
static EGLSurface s_surface = EGL_NO_SURFACE;
static EGLConfig  s_config  = NULL;

static uint64_t s_gl_perf_ring_upload_ns  = 0;
static uint64_t s_gl_perf_state_apply_ns  = 0;
static uint64_t s_gl_perf_draw_calls_ns   = 0;
static uint64_t s_gl_perf_present_blit_ns = 0;
static uint64_t s_gl_perf_swap_buffers_ns = 0;
static uint32_t s_gl_perf_draw_count      = 0;
static uint32_t s_gl_perf_batch_count     = 0;

static GLuint s_efb_fbo   = 0;
static GLuint s_efb_color = 0;
static GLuint s_efb_depth = 0;
static uint32_t s_efb_w   = 0;
static uint32_t s_efb_h   = 0;

static GLuint s_vs_transformed = 0;
static GLuint s_vs_gx          = 0;
static GLuint s_prog_blit      = 0;
static GLint  s_blit_rect_loc  = -1;
static GLuint s_prog_overlay   = 0;
static GLuint s_linear_sampler = 0;

static GLuint s_vao_transformed = 0;
static GLuint s_vao_raw         = 0;
static GLuint s_vao_cached      = 0;
static GLuint s_vao_empty       = 0;
static GLuint s_vao_overlay     = 0;

typedef struct {
    GLuint   id;
    GLenum   target;
    uint32_t capacity;
    uint32_t cursor;
} RingBuffer;

static RingBuffer s_ring_verts;
static RingBuffer s_ring_raw;
static RingBuffer s_ring_indices;
static RingBuffer s_ring_params;
static RingBuffer s_ring_vparams;

static GLuint s_vbo_cache   = 0;
static GLuint s_ibo_cache   = 0;
static RingBuffer s_ring_overlay;

// --- Shaders ---
static const char* k_vs_transformed_src =
    "#version 430 core\n"
    "invariant gl_Position;\n"
    "layout(location = 0) in vec4 a_pos;\n"
    "layout(location = 1) in vec4 a_c0;\n"
    "layout(location = 2) in vec4 a_c1;\n"
    "layout(location = 3) in vec4 a_uv01;\n"
    "layout(location = 4) in vec4 a_uv23;\n"
    "layout(location = 5) in vec4 a_uv45;\n"
    "layout(location = 6) in vec4 a_uv67;\n"
    "layout(location = 0) out vec4 v_c0;\n"
    "layout(location = 1) out vec4 v_c1;\n"
    "layout(location = 2) out vec4 v_uv01;\n"
    "layout(location = 3) out vec4 v_uv23;\n"
    "layout(location = 4) out vec4 v_uv45;\n"
    "layout(location = 5) out vec4 v_uv67;\n"
    "void main() {\n"
    "    gl_Position = vec4(a_pos.x, -a_pos.y, a_pos.z * 2.0 - a_pos.w, a_pos.w);\n"
    "    v_c0 = a_c0;\n"
    "    v_c1 = a_c1;\n"
    "    v_uv01 = a_uv01;\n"
    "    v_uv23 = a_uv23;\n"
    "    v_uv45 = a_uv45;\n"
    "    v_uv67 = a_uv67;\n"
    "}\n";

static const char* k_vs_gx_src =
    "#version 430 core\n"
    "invariant gl_Position;\n"
    "layout(location = 0) in vec4 a_pos;\n"
    "layout(location = 1) in vec3 a_nrm;\n"
    "layout(location = 2) in vec4 a_vc0;\n"
    "layout(location = 3) in vec4 a_vc1;\n"
    "layout(location = 4) in vec4 a_t01;\n"
    "layout(location = 5) in vec4 a_t23;\n"
    "layout(location = 0) out vec4 v_c0;\n"
    "layout(location = 1) out vec4 v_c1;\n"
    "layout(location = 2) out vec4 v_uv01;\n"
    "layout(location = 3) out vec4 v_uv23;\n"
    "layout(location = 4) out vec4 v_uv45;\n"
    "layout(location = 5) out vec4 v_uv67;\n"
    "struct Gen { vec4 r0; vec4 r1; vec4 r2; vec4 info; };\n"
    "struct Chan { vec4 material; vec4 ambient; uvec4 ctrl; };\n"
    "struct Light { vec4 color; vec4 cos_att; vec4 dist_att; vec4 pos; vec4 dir; };\n"
    "layout(std140, binding = 1) uniform VParams {\n"
    "    vec4 proj0; vec4 proj1;\n"
    "    vec4 m0; vec4 m1; vec4 m2;\n"
    "    vec4 n0; vec4 n1; vec4 n2;\n"
    "    Gen gens[8];\n"
    "    Chan chans[2];\n"
    "    Light lights[8];\n"
    "    vec4 origin;\n"
    "    vec4 place;\n"
    "} v;\n"
    "layout(std140, binding = 2) uniform XfMem {\n"
    "    vec4 pm[64];\n"
    "    vec4 nm[32];\n"
    "} xf;\n"
    "float xf_normal(int i) { i = min(i, 127); return xf.nm[i >> 2][i & 3]; }\n"
    "vec3 unit_vec(vec3 a) {\n"
    "    float squared = dot(a, a);\n"
    "    if (squared > 1e-30 && squared < 3.0e38) { return a * inversesqrt(squared); }\n"
    "    float largest = max(abs(a.x), max(abs(a.y), abs(a.z)));\n"
    "    if (!(largest > 0.0) || !(largest < 3.0e38)) { return vec3(0.0); }\n"
    "    vec3 b = a / largest;\n"
    "    return b * inversesqrt(dot(b, b));\n"
    "}\n"
    "float ratio_fn(float above, float below) {\n"
    "    if (below == 0.0) { return 0.0; }\n"
    "    return max(above, 0.0) / below;\n"
    "}\n"
    "float light_amount(uint i, uint ctrl, vec3 pos, vec3 normal) {\n"
    "    vec3 to_light = v.lights[i].pos.xyz - pos;\n"
    "    vec3 direction = unit_vec(to_light);\n"
    "    uint diffuse = (ctrl >> 7u) & 3u;\n"
    "    uint kind = (ctrl >> 9u) & 3u;\n"
    "    float attenuation = 1.0;\n"
    "    if (kind == 1u) {\n"
    "        float c = 0.0;\n"
    "        if (dot(direction, normal) >= 0.0) { c = max(dot(v.lights[i].dir.xyz, normal), 0.0); }\n"
    "        vec3 powers = vec3(1.0, c, c * c);\n"
    "        vec3 dist_att = v.lights[i].dist_att.xyz;\n"
    "        if (diffuse != 0u) { dist_att = unit_vec(dist_att); }\n"
    "        attenuation = ratio_fn(dot(powers, v.lights[i].cos_att.xyz), dot(powers, dist_att));\n"
    "    } else if (kind == 3u) {\n"
    "        float dist = sqrt(dot(to_light, to_light));\n"
    "        float c = max(dot(direction, v.lights[i].dir.xyz), 0.0);\n"
    "        attenuation = ratio_fn(\n"
    "            dot(vec3(1.0, c, c * c), v.lights[i].cos_att.xyz),\n"
    "            dot(vec3(1.0, dist, dist * dist), v.lights[i].dist_att.xyz)\n"
    "        );\n"
    "    } else if (direction.x == 0.0 && direction.y == 0.0 && direction.z == 0.0) {\n"
    "        direction = normal;\n"
    "    }\n"
    "    if (diffuse == 0u) { return attenuation; }\n"
    "    if (diffuse == 1u) { return attenuation * dot(direction, normal); }\n"
    "    return attenuation * max(dot(direction, normal), 0.0);\n"
    "}\n"
    "vec4 shade(uint n, uint ctrl, vec4 vertex, vec3 pos, vec3 normal) {\n"
    "    uint flags = v.chans[n].ctrl.z;\n"
    "    bool has_color = (flags & 1u) != 0u;\n"
    "    vec4 material = v.chans[n].material;\n"
    "    if (has_color && (ctrl & 1u) != 0u) { material = vertex; }\n"
    "    if ((ctrl & 2u) == 0u || (flags & 2u) == 0u) { return material; }\n"
    "    vec4 sum = v.chans[n].ambient;\n"
    "    if (has_color && (ctrl & 0x40u) != 0u) { sum = vertex; }\n"
    "    uint lights = ((ctrl >> 2u) & 0xFu) | (((ctrl >> 11u) & 0xFu) << 4u);\n"
    "    for (uint i = 0u; i < 8u; i++) {\n"
    "        if (((lights >> i) & 1u) != 0u) { sum += v.lights[i].color * light_amount(i, ctrl, pos, normal); }\n"
    "    }\n"
    "    return material * clamp(sum, vec4(0.0), vec4(1.0));\n"
    "}\n"
    "vec4 channel_fn(uint n, vec4 vertex, vec3 pos, vec3 normal) {\n"
    "    uvec4 ctrl = v.chans[n].ctrl;\n"
    "    vec4 color = shade(n, ctrl.x, vertex, pos, normal);\n"
    "    if (ctrl.x == ctrl.y) { return color; }\n"
    "    return vec4(color.rgb, shade(n, ctrl.y, vertex, pos, normal).a);\n"
    "}\n"
    "vec2 coordinate_fn(uint n, vec3 pos, vec2 t0, vec2 t1, vec2 t2, vec2 t3) {\n"
    "    vec4 info = v.gens[n].info;\n"
    "    if (info.x < 0.0) { return vec2(0.0); }\n"
    "    vec4 inp = vec4(0.0);\n"
    "    if (info.x == 0.0) { inp = vec4(pos, 1.0); }\n"
    "    else if (info.x == 1.0) { inp = vec4(t0, 1.0, 1.0); }\n"
    "    else if (info.x == 2.0) { inp = vec4(t1, 1.0, 1.0); }\n"
    "    else if (info.x == 3.0) { inp = vec4(t2, 1.0, 1.0); }\n"
    "    else if (info.x == 4.0) { inp = vec4(t3, 1.0, 1.0); }\n"
    "    vec2 st = vec2(dot(v.gens[n].r0, inp), dot(v.gens[n].r1, inp));\n"
    "    if (info.y != 0.0) {\n"
    "        float q = dot(v.gens[n].r2, inp);\n"
    "        if (q != 0.0) { st = st / q; }\n"
    "    }\n"
    "    return st - info.zw;\n"
    "}\n"
    "void main() {\n"
    "    vec4 pos4 = vec4(a_pos.xyz, 1.0);\n"
    "    vec4 m0 = v.m0, m1 = v.m1, m2 = v.m2;\n"
    "    vec3 n0 = v.n0.xyz, n1 = v.n1.xyz, n2 = v.n2.xyz;\n"
    "    if (v.proj1.w != 0.0) {\n"
    "        int mi = int(a_pos.w + 0.5);\n"
    "        m0 = xf.pm[mi & 63]; m1 = xf.pm[(mi + 1) & 63]; m2 = xf.pm[(mi + 2) & 63];\n"
    "        int nb = (mi & 31) * 3;\n"
    "        n0 = vec3(xf_normal(nb), xf_normal(nb + 1), xf_normal(nb + 2));\n"
    "        n1 = vec3(xf_normal(nb + 3), xf_normal(nb + 4), xf_normal(nb + 5));\n"
    "        n2 = vec3(xf_normal(nb + 6), xf_normal(nb + 7), xf_normal(nb + 8));\n"
    "    }\n"
    "    vec3 at = vec3(dot(m0, pos4), dot(m1, pos4), dot(m2, pos4));\n"
    "    vec3 normal = a_nrm;\n"
    "    if (v.proj1.z != 0.0) {\n"
    "        normal = unit_vec(vec3(dot(n0, a_nrm), dot(n1, a_nrm), dot(n2, a_nrm)));\n"
    "    }\n"
    "    vec4 o_pos = vec4(\n"
    "        v.proj0.x * at.x + v.proj0.y * at.z,\n"
    "        v.proj0.z * at.y + v.proj0.w * at.z,\n"
    "        v.proj1.x * at.z + v.proj1.y - at.z,\n"
    "        -at.z\n"
    "    );\n"
    "    if (v.place.z != 0.0) {\n"
    "        o_pos = v.origin + (o_pos - v.origin) * v.place.x;\n"
    "        o_pos.x += v.place.y * o_pos.w;\n"
    "    }\n"
    "    gl_Position = vec4(o_pos.x, -o_pos.y, o_pos.z * 2.0 - o_pos.w, o_pos.w);\n"
    "    v_c0 = channel_fn(0u, a_vc0, at, normal);\n"
    "    v_c1 = channel_fn(1u, a_vc1, at, normal);\n"
    "    v_uv01 = vec4(\n"
    "        coordinate_fn(0u, a_pos.xyz, a_t01.xy, a_t01.zw, a_t23.xy, a_t23.zw),\n"
    "        coordinate_fn(1u, a_pos.xyz, a_t01.xy, a_t01.zw, a_t23.xy, a_t23.zw)\n"
    "    );\n"
    "    v_uv23 = vec4(\n"
    "        coordinate_fn(2u, a_pos.xyz, a_t01.xy, a_t01.zw, a_t23.xy, a_t23.zw),\n"
    "        coordinate_fn(3u, a_pos.xyz, a_t01.xy, a_t01.zw, a_t23.xy, a_t23.zw)\n"
    "    );\n"
    "    v_uv45 = vec4(\n"
    "        coordinate_fn(4u, a_pos.xyz, a_t01.xy, a_t01.zw, a_t23.xy, a_t23.zw),\n"
    "        coordinate_fn(5u, a_pos.xyz, a_t01.xy, a_t01.zw, a_t23.xy, a_t23.zw)\n"
    "    );\n"
    "    v_uv67 = vec4(\n"
    "        coordinate_fn(6u, a_pos.xyz, a_t01.xy, a_t01.zw, a_t23.xy, a_t23.zw),\n"
    "        coordinate_fn(7u, a_pos.xyz, a_t01.xy, a_t01.zw, a_t23.xy, a_t23.zw)\n"
    "    );\n"
    "}\n";

static const char* k_vs_blit_src =
    "#version 430 core\n"
    "uniform vec4 u_rect;\n"
    "layout(location = 0) out vec2 v_uv;\n"
    "void main() {\n"
    "    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));\n"
    "    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);\n"
    "    v_uv = u_rect.xy + vec2(p.x, 1.0 - p.y) * u_rect.zw;\n"
    "}\n";

static const char* k_fs_blit_src =
    "#version 430 core\n"
    "layout(binding = 0) uniform sampler2D u_tex;\n"
    "uniform vec4 u_rect;\n"
    "layout(location = 0) in vec2 v_uv;\n"
    "layout(location = 0) out vec4 f_out;\n"
    "void main() {\n"
    "    vec2 tex_sz = vec2(textureSize(u_tex, 0));\n"
    "    vec2 half_texel = 0.5 / tex_sz;\n"
    "    vec2 min_uv = min(u_rect.xy, u_rect.xy + u_rect.zw) + half_texel;\n"
    "    vec2 max_uv = max(u_rect.xy, u_rect.xy + u_rect.zw) - half_texel;\n"
    "    vec2 uv_clamped = clamp(v_uv, min_uv, max_uv);\n"
    "    f_out = vec4(texture(u_tex, uv_clamped).rgb, 1.0);\n"
    "}\n";

static const char* k_vs_overlay_src =
    "#version 430 core\n"
    "layout(location = 0) in vec2 a_pos;\n"
    "layout(location = 1) in vec4 a_col;\n"
    "layout(location = 0) out vec4 v_col;\n"
    "void main() {\n"
    "    gl_Position = vec4(a_pos, 0.0, 1.0);\n"
    "    v_col = a_col;\n"
    "}\n";

static const char* k_fs_overlay_src =
    "#version 430 core\n"
    "layout(location = 0) in vec4 v_col;\n"
    "layout(location = 0) out vec4 f_out;\n"
    "void main() {\n"
    "    f_out = v_col;\n"
    "}\n";

static GLuint compile_shader(GLenum type, const char* src, GLint len) {
    GLuint sh = glCreateShader(type);
    glShaderSource(sh, 1, &src, len > 0 ? &len : NULL);
    glCompileShader(sh);
    GLint ok = GL_FALSE;
    glGetShaderiv(sh, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        char log[512];
        glGetShaderInfoLog(sh, sizeof(log), NULL, log);
        char msg[600];
        snprintf(msg, sizeof(msg), "[gl] shader compile error: %s", log);
        switch_log(msg);
    }
    return sh;
}

static GLuint link_two(GLuint vs, GLuint fs, int delete_both) {
    GLuint prog = glCreateProgram();
    glAttachShader(prog, vs);
    glAttachShader(prog, fs);
    glLinkProgram(prog);
    glDetachShader(prog, vs);
    glDetachShader(prog, fs);
    GLint ok = GL_FALSE;
    glGetProgramiv(prog, GL_LINK_STATUS, &ok);
    if (!ok) {
        char log[512];
        glGetProgramInfoLog(prog, sizeof(log), NULL, log);
        char msg[600];
        snprintf(msg, sizeof(msg), "[gl] program link error: %s", log);
        switch_log(msg);
    }
    if (delete_both) {
        glDeleteShader(vs);
        glDeleteShader(fs);
    }
    return prog;
}

static void setup_raw_vao_attribs(void) {
    glEnableVertexAttribArray(0);
    glVertexAttribPointer(0, 4, GL_FLOAT, GL_FALSE, RAW_VERTEX_STRIDE, (const void*)0); /* xyz + matrix index */
    glEnableVertexAttribArray(1);
    glVertexAttribPointer(1, 3, GL_FLOAT, GL_FALSE, RAW_VERTEX_STRIDE, (const void*)16);
    glEnableVertexAttribArray(2);
    glVertexAttribPointer(2, 4, GL_FLOAT, GL_FALSE, RAW_VERTEX_STRIDE, (const void*)32);
    glEnableVertexAttribArray(3);
    glVertexAttribPointer(3, 4, GL_FLOAT, GL_FALSE, RAW_VERTEX_STRIDE, (const void*)48);
    glEnableVertexAttribArray(4);
    glVertexAttribPointer(4, 4, GL_FLOAT, GL_FALSE, RAW_VERTEX_STRIDE, (const void*)64);
    glEnableVertexAttribArray(5);
    glVertexAttribPointer(5, 4, GL_FLOAT, GL_FALSE, RAW_VERTEX_STRIDE, (const void*)80);
}

static void setup_transformed_vao_attribs(void) {
    for (GLuint i = 0; i < 7; i++) {
        glEnableVertexAttribArray(i);
        glVertexAttribPointer(i, 4, GL_FLOAT, GL_FALSE, VERTEX_STRIDE, (const void*)(uintptr_t)(i * 16));
    }
}

static void init_ring(RingBuffer* rb, GLenum target, uint32_t cap) {
    rb->target = target;
    rb->capacity = cap;
    rb->cursor = 0;
    glGenBuffers(1, &rb->id);
    glBindBuffer(GL_COPY_WRITE_BUFFER, rb->id);
    glBufferData(GL_COPY_WRITE_BUFFER, cap, NULL, GL_DYNAMIC_DRAW);
}

static void rebind_streaming_vaos(void) {
    if (s_vao_transformed != 0) {
        glBindVertexArray(s_vao_transformed);
        glBindBuffer(GL_ARRAY_BUFFER, s_ring_verts.id);
        setup_transformed_vao_attribs();
        glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, s_ring_indices.id);
    }

    if (s_vao_raw != 0) {
        glBindVertexArray(s_vao_raw);
        glBindBuffer(GL_ARRAY_BUFFER, s_ring_raw.id);
        setup_raw_vao_attribs();
        glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, s_ring_indices.id);
    }

    if (s_vao_overlay != 0) {
        glBindVertexArray(s_vao_overlay);
        glBindBuffer(GL_ARRAY_BUFFER, s_ring_overlay.id);
        glEnableVertexAttribArray(0);
        glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, OVERLAY_STRIDE, (const void*)0);
        glEnableVertexAttribArray(1);
        glVertexAttribPointer(1, 4, GL_FLOAT, GL_FALSE, OVERLAY_STRIDE, (const void*)8);
    }

    glBindVertexArray(0);
}

static uint32_t ring_upload(RingBuffer* rb, const void* data, uint32_t bytes, uint32_t align) {
    if (bytes == 0 || data == NULL) {
        return 0;
    }
    uint32_t rem = rb->cursor % align;
    uint32_t aligned = rem == 0 ? rb->cursor : (rb->cursor + (align - rem));
    if (bytes > rb->capacity) {
        uint32_t new_cap = rb->capacity;
        while (new_cap < bytes * 2) {
            new_cap *= 2;
        }
        new_cap = ((new_cap + align - 1) / align) * align;
        rb->capacity = new_cap;
        glBindBuffer(GL_COPY_WRITE_BUFFER, rb->id);
        glBufferData(GL_COPY_WRITE_BUFFER, new_cap, NULL, GL_DYNAMIC_DRAW);
        if (rb == &s_ring_verts || rb == &s_ring_raw || rb == &s_ring_indices || rb == &s_ring_overlay) {
            rebind_streaming_vaos();
        }
        aligned = 0;
    } else if (aligned + bytes > rb->capacity) {
        glBindBuffer(GL_COPY_WRITE_BUFFER, rb->id);
        glBufferData(GL_COPY_WRITE_BUFFER, rb->capacity, NULL, GL_DYNAMIC_DRAW);
        if (rb == &s_ring_verts || rb == &s_ring_raw || rb == &s_ring_indices || rb == &s_ring_overlay) {
            rebind_streaming_vaos();
        }
        aligned = 0;
    }
    glBindBuffer(GL_COPY_WRITE_BUFFER, rb->id);
    glBufferSubData(GL_COPY_WRITE_BUFFER, (GLintptr)aligned, (GLsizeiptr)bytes, data);
    rb->cursor = aligned + bytes;
    return aligned;
}

static void create_or_resize_efb(uint32_t w, uint32_t h) {
    if (w == s_efb_w && h == s_efb_h && s_efb_fbo != 0) {
        return;
    }
    s_efb_w = w;
    s_efb_h = h;

    if (s_efb_fbo == 0) {
        glGenFramebuffers(1, &s_efb_fbo);
        glGenTextures(1, &s_efb_color);
        glGenTextures(1, &s_efb_depth);
    }

    glBindTexture(GL_TEXTURE_2D, s_efb_color);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_BASE_LEVEL, 0);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAX_LEVEL, 0);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA8, (GLsizei)w, (GLsizei)h, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);

    glBindTexture(GL_TEXTURE_2D, s_efb_depth);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_BASE_LEVEL, 0);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAX_LEVEL, 0);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_DEPTH_COMPONENT24, (GLsizei)w, (GLsizei)h, 0, GL_DEPTH_COMPONENT, GL_UNSIGNED_INT, NULL);

    glBindFramebuffer(GL_FRAMEBUFFER, s_efb_fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, s_efb_color, 0);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_DEPTH_ATTACHMENT, GL_TEXTURE_2D, s_efb_depth, 0);
    glBindFramebuffer(GL_FRAMEBUFFER, 0);
}

static GLenum map_blend_factor(uint32_t code, int is_source) {
    switch (code & 7u) {
        case 0: return GL_ZERO;
        case 1: return GL_ONE;
        case 2: return is_source ? GL_DST_COLOR : GL_SRC_COLOR;
        case 3: return is_source ? GL_ONE_MINUS_DST_COLOR : GL_ONE_MINUS_SRC_COLOR;
        case 4: return GL_SRC_ALPHA;
        case 5: return GL_ONE_MINUS_SRC_ALPHA;
        case 6: return GL_DST_ALPHA;
        default: return GL_ONE_MINUS_DST_ALPHA;
    }
}

static void apply_draw_state(uint32_t bits) {
    static const GLenum k_depth_funcs[8] = {
        GL_NEVER, GL_LESS, GL_EQUAL, GL_LEQUAL, GL_GREATER, GL_NOTEQUAL, GL_GEQUAL, GL_ALWAYS
    };
    int depth_test  = (bits & (1u << 0)) != 0;
    int depth_write = (bits & (1u << 1)) != 0;
    uint32_t dfunc  = (bits >> 2) & 7u;
    int blend       = (bits & (1u << 5)) != 0;
    int subtract    = (bits & (1u << 6)) != 0;
    uint32_t src_f  = (bits >> 7) & 7u;
    uint32_t dst_f  = (bits >> 10) & 7u;
    int color_w     = (bits & (1u << 13)) != 0;
    int alpha_w     = (bits & (1u << 14)) != 0;
    uint32_t cull   = (bits >> 15) & 3u;

    if (depth_test) {
        glEnable(GL_DEPTH_TEST);
        glDepthFunc(k_depth_funcs[dfunc]);
        glDepthMask(depth_write ? GL_TRUE : GL_FALSE);
    } else {
        glDisable(GL_DEPTH_TEST);
        glDepthMask(GL_FALSE);
    }

    if (blend) {
        glEnable(GL_BLEND);
        if (subtract) {
            glBlendEquation(GL_FUNC_REVERSE_SUBTRACT);
            glBlendFunc(GL_ONE, GL_ONE);
        } else {
            glBlendEquation(GL_FUNC_ADD);
            glBlendFunc(map_blend_factor(src_f, 1), map_blend_factor(dst_f, 0));
        }
    } else {
        glDisable(GL_BLEND);
    }

    glColorMask(
        color_w ? GL_TRUE : GL_FALSE,
        color_w ? GL_TRUE : GL_FALSE,
        color_w ? GL_TRUE : GL_FALSE,
        alpha_w ? GL_TRUE : GL_FALSE
    );

    if (cull == 1u) {
        glEnable(GL_CULL_FACE);
        glCullFace(GL_FRONT);
    } else if (cull == 2u) {
        glEnable(GL_CULL_FACE);
        glCullFace(GL_BACK);
    } else {
        glDisable(GL_CULL_FACE);
    }
}

static void execute_pass_internal(const PassPayload* pass) {
    if (pass->cmd_count == 0 && !pass->has_clear) {
        return;
    }
    uint64_t t_ring_0 = armTicksToNs(armGetSystemTick());
    uint32_t vert_byte_off    = ring_upload(&s_ring_verts,   pass->vert_buf,    pass->vert_bytes,    VERTEX_STRIDE);
    uint32_t raw_byte_off     = ring_upload(&s_ring_raw,     pass->raw_buf,     pass->raw_bytes,     RAW_VERTEX_STRIDE);
    uint32_t idx_byte_off     = ring_upload(&s_ring_indices, pass->idx_buf,     pass->idx_bytes,     4u);
    uint32_t params_byte_off  = ring_upload(&s_ring_params,  pass->params_buf,  pass->params_bytes,  256u);
    uint32_t vparams_byte_off = ring_upload(&s_ring_vparams, pass->vparams_buf, pass->vparams_bytes, 1536u);
    uint64_t t_ring_1 = armTicksToNs(armGetSystemTick());
    s_gl_perf_ring_upload_ns += (t_ring_1 - t_ring_0);

    int32_t  ring_vert_base = (int32_t)(vert_byte_off / VERTEX_STRIDE);
    int32_t  ring_raw_base  = (int32_t)(raw_byte_off / RAW_VERTEX_STRIDE);
    uint32_t ring_idx_base  = idx_byte_off / 4u;

    glBindFramebuffer(GL_FRAMEBUFFER, s_efb_fbo);
    glFrontFace(GL_CW);

    if (pass->has_clear) {
        glDisable(GL_SCISSOR_TEST);
        glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
        glDepthMask(GL_TRUE);
        glClearColor(pass->clear_color[0], pass->clear_color[1], pass->clear_color[2], pass->clear_color[3]);
        glClearDepthf(1.0f);
        glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
    }

    if (pass->cmd_count == 0) {
        return;
    }

    glEnable(GL_SCISSOR_TEST);

    uint32_t cur_prog    = 0xFFFFFFFFu;
    uint32_t cur_state   = 0xFFFFFFFFu;
    uint32_t cur_params  = 0xFFFFFFFFu;
    uint32_t cur_vparams = 0xFFFFFFFFu;
    uint8_t  cur_geom    = 0xFFu;
    uint32_t cur_tex[8]  = {0};
    uint32_t cur_smp[8]  = {0};
    float    cur_vp[4]   = {-1.0f, -1.0f, -1.0f, -1.0f};
    int32_t  cur_sc[4]   = {-1, -1, -1, -1};

    const GLuint vaos[3] = { s_vao_transformed, s_vao_raw, s_vao_cached };
    uint64_t t_pass_0 = armTicksToNs(armGetSystemTick());

    for (uint32_t i = 0; i < pass->cmd_count; i++) {
        const GlDrawCmd* cmd = &pass->cmds[i];
        if (cmd->program == 0 || cmd->index_count == 0) {
            continue;
        }

        if (cmd->program != cur_prog) {
            cur_prog = cmd->program;
            glUseProgram(cur_prog);
        }
        if (cmd->state_bits != cur_state) {
            cur_state = cmd->state_bits;
            apply_draw_state(cur_state);
        }

        uint32_t p_off = params_byte_off + cmd->params_offset;
        if (p_off != cur_params) {
            cur_params = p_off;
            glBindBufferRange(GL_UNIFORM_BUFFER, 0, s_ring_params.id, (GLintptr)p_off, 176);
        }

        if (cmd->geom_kind != 0) {
            uint32_t vp_off = vparams_byte_off + cmd->vparams_offset;
            if (vp_off != cur_vparams) {
                cur_vparams = vp_off;
                glBindBufferRange(GL_UNIFORM_BUFFER, 1, s_ring_vparams.id, (GLintptr)vp_off, 1408);
                /* XfMem: matrix-indexed draws store it in the next 1536-byte slot. Other draws
                   never read it, so any in-range slot will do. */
                uint32_t xf_off = vp_off + 1536u;
                if (xf_off + 1536u > s_ring_vparams.capacity) xf_off = vp_off;
                glBindBufferRange(GL_UNIFORM_BUFFER, 2, s_ring_vparams.id, (GLintptr)xf_off, 1536);
            }
        }

        const uint64_t* cmd_tex64 = (const uint64_t*)cmd->textures;
        uint64_t* cur_tex64 = (uint64_t*)cur_tex;
        if (((cmd_tex64[0] ^ cur_tex64[0]) | (cmd_tex64[1] ^ cur_tex64[1]) |
             (cmd_tex64[2] ^ cur_tex64[2]) | (cmd_tex64[3] ^ cur_tex64[3])) != 0) {
            for (uint32_t slot = 0; slot < 8; slot++) {
                if (cmd->textures[slot] != cur_tex[slot]) {
                    cur_tex[slot] = cmd->textures[slot];
                    glActiveTexture(GL_TEXTURE0 + slot);
                    glBindTexture(GL_TEXTURE_2D, cur_tex[slot]);
                }
            }
        }
        const uint64_t* cmd_smp64 = (const uint64_t*)cmd->samplers;
        uint64_t* cur_smp64 = (uint64_t*)cur_smp;
        if (((cmd_smp64[0] ^ cur_smp64[0]) | (cmd_smp64[1] ^ cur_smp64[1]) |
             (cmd_smp64[2] ^ cur_smp64[2]) | (cmd_smp64[3] ^ cur_smp64[3])) != 0) {
            for (uint32_t slot = 0; slot < 8; slot++) {
                if (cmd->samplers[slot] != cur_smp[slot]) {
                    cur_smp[slot] = cmd->samplers[slot];
                    glBindSampler(slot, cur_smp[slot]);
                }
            }
        }

        const uint64_t* cmd_vp64 = (const uint64_t*)cmd->viewport;
        uint64_t* cur_vp64 = (uint64_t*)cur_vp;
        if (((cmd_vp64[0] ^ cur_vp64[0]) | (cmd_vp64[1] ^ cur_vp64[1])) != 0) {
            cur_vp64[0] = cmd_vp64[0];
            cur_vp64[1] = cmd_vp64[1];
            glViewport((GLint)cur_vp[0], (GLint)cur_vp[1], (GLsizei)cur_vp[2], (GLsizei)cur_vp[3]);
        }
        const uint64_t* cmd_sc64 = (const uint64_t*)cmd->scissor;
        uint64_t* cur_sc64 = (uint64_t*)cur_sc;
        if (((cmd_sc64[0] ^ cur_sc64[0]) | (cmd_sc64[1] ^ cur_sc64[1])) != 0) {
            cur_sc64[0] = cmd_sc64[0];
            cur_sc64[1] = cmd_sc64[1];
            glScissor(cur_sc[0], cur_sc[1], cur_sc[2], cur_sc[3]);
        }

        if (cmd->geom_kind != cur_geom) {
            cur_geom = cmd->geom_kind;
            glBindVertexArray(vaos[cur_geom <= 2 ? cur_geom : 0]);
        }

        uint32_t first_idx = cmd->first_index;
        int32_t  base_vtx  = cmd->base_vertex;
        if (cur_geom == 0) {
            first_idx += ring_idx_base;
            base_vtx   = ring_vert_base;
        } else if (cur_geom == 1) {
            first_idx += ring_idx_base;
            base_vtx   = ring_raw_base;
        }

        uint32_t total_indices = cmd->index_count;
        uint32_t merged_cmds = 1;
        while (i + 1 < pass->cmd_count) {
            const GlDrawCmd* next = &pass->cmds[i + 1];
            if (next->program == cmd->program &&
                next->state_bits == cmd->state_bits &&
                next->geom_kind == cmd->geom_kind &&
                next->geom_kind != 2 &&
                next->base_vertex == cmd->base_vertex &&
                next->first_index == cmd->first_index + total_indices &&
                next->params_offset == cmd->params_offset &&
                next->vparams_offset == cmd->vparams_offset)
            {
                const uint64_t* vp_a = (const uint64_t*)next->viewport;
                const uint64_t* vp_b = (const uint64_t*)cmd->viewport;
                if (vp_a[0] != vp_b[0] || vp_a[1] != vp_b[1]) break;

                const uint64_t* sc_a = (const uint64_t*)next->scissor;
                const uint64_t* sc_b = (const uint64_t*)cmd->scissor;
                if (sc_a[0] != sc_b[0] || sc_a[1] != sc_b[1]) break;

                const uint64_t* tx_a = (const uint64_t*)next->textures;
                const uint64_t* tx_b = (const uint64_t*)cmd->textures;
                if (tx_a[0] != tx_b[0] || tx_a[1] != tx_b[1] || tx_a[2] != tx_b[2] || tx_a[3] != tx_b[3]) break;

                const uint64_t* sm_a = (const uint64_t*)next->samplers;
                const uint64_t* sm_b = (const uint64_t*)cmd->samplers;
                if (sm_a[0] != sm_b[0] || sm_a[1] != sm_b[1] || sm_a[2] != sm_b[2] || sm_a[3] != sm_b[3]) break;

                total_indices += next->index_count;
                merged_cmds++;
                i++;
            } else {
                break;
            }
        }

        glDrawElementsBaseVertex(
            GL_TRIANGLES,
            (GLsizei)total_indices,
            GL_UNSIGNED_INT,
            (const void*)(uintptr_t)(first_idx * 4u),
            base_vtx
        );

        s_gl_perf_draw_count += merged_cmds;
        s_gl_perf_batch_count += 1;
    }
    uint64_t t_pass_1 = armTicksToNs(armGetSystemTick());
    s_gl_perf_draw_calls_ns += (t_pass_1 - t_pass_0);
    glBindVertexArray(0);
}

static void copy_efb_internal(uint32_t tex, uint32_t alloc_new_size, uint32_t x, uint32_t y, uint32_t w, uint32_t h) {
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, tex);
    if (alloc_new_size) {
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_BASE_LEVEL, 0);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAX_LEVEL, 0);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA8, (GLsizei)w, (GLsizei)h, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);
    }
    glBindFramebuffer(GL_READ_FRAMEBUFFER, s_efb_fbo);
    glCopyTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, (GLint)x, (GLint)y, (GLsizei)w, (GLsizei)h);
}

static void present_internal(
    uint32_t src_tex,
    const float uv_rect[4],
    float picture_aspect,
    const void* overlay_verts,
    uint32_t overlay_vert_count
) {
    uint32_t is_docked = switch_is_docked_mode();
    uint32_t target_w = is_docked ? 1920 : 1280;
    uint32_t target_h = is_docked ? 1080 : 720;

    static uint32_t s_crop_w = 0;
    static uint32_t s_crop_h = 0;
    if (target_w != s_crop_w || target_h != s_crop_h) {
        s_crop_w = target_w;
        s_crop_h = target_h;
        NWindow* win = nwindowGetDefault();
        if (is_docked) {
            nwindowSetCrop(win, 0, 0, 0, 0);
        } else {
            nwindowSetCrop(win, 0, 0, 1280, 720);
        }
    }

    uint32_t win_w = target_w;
    uint32_t win_h = target_h;

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glFrontFace(GL_CCW);
    glDisable(GL_SCISSOR_TEST);
    glDisable(GL_DEPTH_TEST);
    glDisable(GL_CULL_FACE);
    glDisable(GL_BLEND);
    glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);

    GLint vp_base_y = (GLint)(1080 - target_h);
    glViewport(0, vp_base_y, (GLsizei)win_w, (GLsizei)win_h);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);

    uint64_t t_bl_0 = armTicksToNs(armGetSystemTick());
    glUseProgram(s_prog_blit);
    glUniform4fv(s_blit_rect_loc, 1, uv_rect);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, src_tex);
    glBindSampler(0, s_linear_sampler);
    glBindVertexArray(s_vao_empty);
    glDrawArrays(GL_TRIANGLES, 0, 3);
    glBindSampler(0, 0);
    glBindVertexArray(0);

    if (overlay_verts != NULL && overlay_vert_count > 0) {
        uint32_t count = overlay_vert_count > 4096 ? 4096 : overlay_vert_count;
        uint32_t ov_byte_off = ring_upload(&s_ring_overlay, overlay_verts, count * OVERLAY_STRIDE, OVERLAY_STRIDE);
        glViewport(0, vp_base_y, (GLsizei)win_w, (GLsizei)win_h);
        glEnable(GL_BLEND);
        glBlendEquation(GL_FUNC_ADD);
        glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
        glUseProgram(s_prog_overlay);
        glBindVertexArray(s_vao_overlay);
        glDrawArrays(GL_TRIANGLES, (GLint)(ov_byte_off / OVERLAY_STRIDE), (GLsizei)count);
        glBindVertexArray(0);
    }
    uint64_t t_bl_1 = armTicksToNs(armGetSystemTick());
    s_gl_perf_present_blit_ns += (t_bl_1 - t_bl_0);

    eglSwapBuffers(s_display, s_surface);
    uint64_t t_bl_2 = armTicksToNs(armGetSystemTick());
    s_gl_perf_swap_buffers_ns += (t_bl_2 - t_bl_1);
}

// --- GL Thread Main Loop (Runs on Core 2) ---
static void gl_thread_entry(void* arg) {
    (void)arg;
    mutexLock(&s_gl_mutex);

    setenv("MESA_NO_ERROR", "1", 1);

    uint32_t is_docked = switch_is_docked_mode();
    uint32_t win_w = is_docked ? 1920 : 1280;
    uint32_t win_h = is_docked ? 1080 : 720;

    NWindow* win = nwindowGetDefault();
    nwindowSetDimensions(win, 1920, 1080);
    if (is_docked) {
        nwindowSetCrop(win, 0, 0, 0, 0);
    } else {
        nwindowSetCrop(win, 0, 0, 1280, 720);
    }

    s_display = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    if (!s_display) {
        switch_log("[gl] eglGetDisplay failed");
        s_gl_init_result = -1;
        condvarWakeAll(&s_gl_cond_sync_done);
        mutexUnlock(&s_gl_mutex);
        return;
    }
    eglInitialize(s_display, NULL, NULL);
    if (eglBindAPI(EGL_OPENGL_API) == EGL_FALSE) {
        switch_log("[gl] eglBindAPI(EGL_OPENGL_API) failed");
        s_gl_init_result = -2;
        condvarWakeAll(&s_gl_cond_sync_done);
        mutexUnlock(&s_gl_mutex);
        return;
    }

    EGLConfig config;
    EGLint num_configs = 0;
    static const EGLint fb_attribs[] = {
        EGL_RENDERABLE_TYPE, EGL_OPENGL_BIT,
        EGL_RED_SIZE,     8,
        EGL_GREEN_SIZE,   8,
        EGL_BLUE_SIZE,    8,
        EGL_ALPHA_SIZE,   8,
        EGL_NONE
    };
    eglChooseConfig(s_display, fb_attribs, &config, 1, &num_configs);
    if (num_configs == 0) {
        switch_log("[gl] eglChooseConfig returned 0 configs");
        s_gl_init_result = -3;
        condvarWakeAll(&s_gl_cond_sync_done);
        mutexUnlock(&s_gl_mutex);
        return;
    }
    s_config = config;

    s_surface = eglCreateWindowSurface(s_display, s_config, (EGLNativeWindowType)win, NULL);
    if (!s_surface) {
        switch_log("[gl] eglCreateWindowSurface failed");
        s_gl_init_result = -4;
        condvarWakeAll(&s_gl_cond_sync_done);
        mutexUnlock(&s_gl_mutex);
        return;
    }

    static const EGLint ctx_attribs[] = {
        EGL_CONTEXT_OPENGL_PROFILE_MASK_KHR, EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT_KHR,
        EGL_CONTEXT_MAJOR_VERSION_KHR, 4,
        EGL_CONTEXT_MINOR_VERSION_KHR, 3,
        EGL_NONE
    };
    s_context = eglCreateContext(s_display, config, EGL_NO_CONTEXT, ctx_attribs);
    if (!s_context) {
        switch_log("[gl] eglCreateContext failed");
        s_gl_init_result = -5;
        condvarWakeAll(&s_gl_cond_sync_done);
        mutexUnlock(&s_gl_mutex);
        return;
    }

    eglMakeCurrent(s_display, s_surface, s_surface, s_context);
    gladLoadGL();
    eglSwapInterval(s_display, 0);

    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);

    char info[256];
    snprintf(
        info,
        sizeof(info),
        "[gl] Dedicated Core 2 Render Thread initialized: %s | %s",
        (const char*)glGetString(GL_RENDERER),
        (const char*)glGetString(GL_VERSION)
    );
    switch_log(info);

    s_vs_transformed = compile_shader(GL_VERTEX_SHADER, k_vs_transformed_src, 0);
    s_vs_gx          = compile_shader(GL_VERTEX_SHADER, k_vs_gx_src, 0);

    GLuint vs_blit = compile_shader(GL_VERTEX_SHADER, k_vs_blit_src, 0);
    GLuint fs_blit = compile_shader(GL_FRAGMENT_SHADER, k_fs_blit_src, 0);
    s_prog_blit    = link_two(vs_blit, fs_blit, 1);
    s_blit_rect_loc = glGetUniformLocation(s_prog_blit, "u_rect");

    GLuint vs_ov = compile_shader(GL_VERTEX_SHADER, k_vs_overlay_src, 0);
    GLuint fs_ov = compile_shader(GL_FRAGMENT_SHADER, k_fs_overlay_src, 0);
    s_prog_overlay = link_two(vs_ov, fs_ov, 1);

    glGenSamplers(1, &s_linear_sampler);
    glSamplerParameteri(s_linear_sampler, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glSamplerParameteri(s_linear_sampler, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glSamplerParameteri(s_linear_sampler, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glSamplerParameteri(s_linear_sampler, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);

    // Circular streaming buffers
    init_ring(&s_ring_verts,   GL_ARRAY_BUFFER,         65536u * VERTEX_STRIDE);      // ~7.3 MB
    init_ring(&s_ring_raw,     GL_ARRAY_BUFFER,         131072u * RAW_VERTEX_STRIDE); // ~12.5 MB
    init_ring(&s_ring_indices, GL_ELEMENT_ARRAY_BUFFER, 1048576u * 4u);               // ~4.1 MB
    init_ring(&s_ring_params,  GL_UNIFORM_BUFFER,       4u * 1024u * 1024u);          //  4.0 MB
    init_ring(&s_ring_vparams, GL_UNIFORM_BUFFER,       8u * 1024u * 1024u);          //  8.0 MB
    init_ring(&s_ring_overlay, GL_ARRAY_BUFFER,         32768u * OVERLAY_STRIDE);     // ~0.8 MB

    // Persistent GPU Display List Cache buffers
    glGenBuffers(1, &s_vbo_cache);
    glBindBuffer(GL_COPY_WRITE_BUFFER, s_vbo_cache);
    glBufferData(GL_COPY_WRITE_BUFFER, (GLsizeiptr)s_cache_verts * RAW_VERTEX_STRIDE, NULL, GL_STATIC_DRAW);

    glGenBuffers(1, &s_ibo_cache);
    glBindBuffer(GL_COPY_WRITE_BUFFER, s_ibo_cache);
    glBufferData(GL_COPY_WRITE_BUFFER, (GLsizeiptr)s_cache_indices * 4, NULL, GL_STATIC_DRAW);

    // Pre-configured VAOs
    glGenVertexArrays(1, &s_vao_transformed);
    glGenVertexArrays(1, &s_vao_raw);
    glGenVertexArrays(1, &s_vao_cached);
    glGenVertexArrays(1, &s_vao_empty);
    glGenVertexArrays(1, &s_vao_overlay);

    rebind_streaming_vaos();

    glBindVertexArray(s_vao_cached);
    glBindBuffer(GL_ARRAY_BUFFER, s_vbo_cache);
    setup_raw_vao_attribs();
    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, s_ibo_cache);
    glBindVertexArray(0);

    create_or_resize_efb(s_init_efb_w, s_init_efb_h);

    // Pre-generate pool of texture handles for zero-wait EFB copy
    glGenTextures(256, s_pregen_tex);
    s_pregen_tex_count = 256;

    s_gl_init_result = 0;
    condvarWakeAll(&s_gl_cond_sync_done);

    // Main Core 2 Rendering Loop
    while (!s_gl_exit) {
        while (s_queue_head == s_queue_tail && !s_gl_exit) {
            condvarWait(&s_gl_cond_work, &s_gl_mutex);
        }
        if (s_gl_exit) {
            break;
        }

        GlCommand cmd = s_queue[s_queue_tail % GL_QUEUE_CAPACITY];
        s_queue_tail++;
        condvarWakeAll(&s_gl_cond_queue_free);
        mutexUnlock(&s_gl_mutex);

        // Execute command with OpenGL Context current
        switch (cmd.type) {
            case GL_CMD_COMPILE_FS: {
                GLuint fs = compile_shader(GL_FRAGMENT_SHADER, cmd.compile_fs.src_ptr, (GLint)cmd.compile_fs.src_len);
                mutexLock(&s_gl_mutex);
                if (cmd.out_u32) *cmd.out_u32 = fs;
                condvarWakeAll(&s_gl_cond_sync_done);
                mutexUnlock(&s_gl_mutex);
                break;
            }
            case GL_CMD_LINK_PROGRAM: {
                GLuint vs = cmd.link_prog.is_raw ? s_vs_gx : s_vs_transformed;
                GLuint prog = link_two(vs, cmd.link_prog.fs_id, 0);
                mutexLock(&s_gl_mutex);
                if (cmd.out_u32) *cmd.out_u32 = prog;
                condvarWakeAll(&s_gl_cond_sync_done);
                mutexUnlock(&s_gl_mutex);
                break;
            }
            case GL_CMD_UPLOAD_TEXTURE: {
                GLuint tex = cmd.upload_tex.existing_tex;
                if (tex == 0) {
                    glGenTextures(1, &tex);
                }
                glActiveTexture(GL_TEXTURE0);
                glBindTexture(GL_TEXTURE_2D, tex);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_BASE_LEVEL, 0);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAX_LEVEL, 0);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
                glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA8, (GLsizei)cmd.upload_tex.w, (GLsizei)cmd.upload_tex.h, 0, GL_RGBA, GL_UNSIGNED_BYTE, cmd.upload_tex.rgba);
                mutexLock(&s_gl_mutex);
                if (cmd.out_u32) *cmd.out_u32 = tex;
                condvarWakeAll(&s_gl_cond_sync_done);
                mutexUnlock(&s_gl_mutex);
                break;
            }
            case GL_CMD_CREATE_SAMPLER: {
                static const GLint k_wrap[3] = { GL_CLAMP_TO_EDGE, GL_REPEAT, GL_MIRRORED_REPEAT };
                GLuint smp = 0;
                glGenSamplers(1, &smp);
                GLint ws = k_wrap[cmd.create_sampler.wrap_s <= 2 ? cmd.create_sampler.wrap_s : 0];
                GLint wt = k_wrap[cmd.create_sampler.wrap_t <= 2 ? cmd.create_sampler.wrap_t : 0];
                GLint flt = cmd.create_sampler.linear ? GL_LINEAR : GL_NEAREST;
                glSamplerParameteri(smp, GL_TEXTURE_WRAP_S, ws);
                glSamplerParameteri(smp, GL_TEXTURE_WRAP_T, wt);
                glSamplerParameteri(smp, GL_TEXTURE_MIN_FILTER, flt);
                glSamplerParameteri(smp, GL_TEXTURE_MAG_FILTER, flt);
                mutexLock(&s_gl_mutex);
                if (cmd.out_u32) *cmd.out_u32 = smp;
                condvarWakeAll(&s_gl_cond_sync_done);
                mutexUnlock(&s_gl_mutex);
                break;
            }
            case GL_CMD_CACHE_UPLOAD: {
                if (cmd.cache_upload.vert_bytes > 0 && cmd.cache_upload.verts != NULL) {
                    glBindBuffer(GL_COPY_WRITE_BUFFER, s_vbo_cache);
                    glBufferSubData(GL_COPY_WRITE_BUFFER, (GLintptr)cmd.cache_upload.vert_offset_elems * RAW_VERTEX_STRIDE, cmd.cache_upload.vert_bytes, cmd.cache_upload.verts);
                }
                if (cmd.cache_upload.idx_bytes > 0 && cmd.cache_upload.indices != NULL) {
                    glBindBuffer(GL_COPY_WRITE_BUFFER, s_ibo_cache);
                    glBufferSubData(GL_COPY_WRITE_BUFFER, (GLintptr)cmd.cache_upload.idx_offset_elems * 4, cmd.cache_upload.idx_bytes, cmd.cache_upload.indices);
                }
                mutexLock(&s_gl_mutex);
                condvarWakeAll(&s_gl_cond_sync_done);
                mutexUnlock(&s_gl_mutex);
                break;
            }
            case GL_CMD_RESIZE_EFB: {
                create_or_resize_efb(cmd.resize_efb.efb_w, cmd.resize_efb.efb_h);
                mutexLock(&s_gl_mutex);
                condvarWakeAll(&s_gl_cond_sync_done);
                mutexUnlock(&s_gl_mutex);
                break;
            }
            case GL_CMD_EXECUTE_PASS: {
                PassPayload* payload = &s_pass_payloads[cmd.pass.payload_idx];
                execute_pass_internal(payload);
                mutexLock(&s_gl_mutex);
                s_pass_tail++;
                condvarWakeAll(&s_gl_cond_payload_free);
                mutexUnlock(&s_gl_mutex);
                break;
            }
            case GL_CMD_COPY_EFB: {
                copy_efb_internal(
                    cmd.copy_efb.tex,
                    cmd.copy_efb.alloc_new_size,
                    cmd.copy_efb.x,
                    cmd.copy_efb.y,
                    cmd.copy_efb.w,
                    cmd.copy_efb.h
                );
                break;
            }
            case GL_CMD_PRESENT: {
                present_internal(
                    cmd.present.src_tex,
                    cmd.present.uv_rect,
                    cmd.present.picture_aspect,
                    cmd.present.overlay_verts,
                    cmd.present.overlay_vert_count
                );
                mutexLock(&s_gl_mutex);
                if (s_frames_in_flight > 0) {
                    s_frames_in_flight--;
                }
                condvarWakeAll(&s_gl_cond_frame_done);
                mutexUnlock(&s_gl_mutex);
                break;
            }
            default:
                break;
        }

        mutexLock(&s_gl_mutex);
        if (s_pregen_tex_count < 128) {
            uint32_t needed = 256 - s_pregen_tex_count;
            glGenTextures((GLsizei)needed, &s_pregen_tex[s_pregen_tex_count]);
            s_pregen_tex_count = 256;
        }
    }

    if (s_display != EGL_NO_DISPLAY) {
        eglMakeCurrent(s_display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
        if (s_context != EGL_NO_CONTEXT) {
            eglDestroyContext(s_display, s_context);
            s_context = EGL_NO_CONTEXT;
        }
        if (s_surface != EGL_NO_SURFACE) {
            eglDestroySurface(s_display, s_surface);
            s_surface = EGL_NO_SURFACE;
        }
        eglTerminate(s_display);
        s_display = EGL_NO_DISPLAY;
    }

    mutexUnlock(&s_gl_mutex);
}

static void ensure_payload_buf(uint8_t** buf, uint32_t* cap, uint32_t needed) {
    if (needed == 0) return;
    if (needed > *cap) {
        uint32_t new_cap = *cap ? *cap * 2 : 65536;
        while (new_cap < needed) new_cap *= 2;
        *buf = (uint8_t*)realloc(*buf, new_cap);
        *cap = new_cap;
    }
}

static void ensure_cmds_buf(GlDrawCmd** buf, uint32_t* cap, uint32_t needed) {
    if (needed == 0) return;
    if (needed > *cap) {
        uint32_t new_cap = *cap ? *cap * 2 : 4096;
        while (new_cap < needed) new_cap *= 2;
        *buf = (GlDrawCmd*)realloc(*buf, new_cap * sizeof(GlDrawCmd));
        *cap = new_cap;
    }
}

// --- Public Interface (Called from Core 1) ---

int switch_gl_init(uint32_t efb_w, uint32_t efb_h, uint32_t cache_verts, uint32_t cache_indices) {
    mutexInit(&s_gl_mutex);
    condvarInit(&s_gl_cond_work);
    condvarInit(&s_gl_cond_queue_free);
    condvarInit(&s_gl_cond_payload_free);
    condvarInit(&s_gl_cond_frame_done);
    condvarInit(&s_gl_cond_sync_done);

    s_queue_head = 0;
    s_queue_tail = 0;
    s_pass_head = 0;
    s_pass_tail = 0;
    s_frames_in_flight = 0;
    s_gl_exit = false;

    s_init_efb_w = efb_w;
    s_init_efb_h = efb_h;
    s_cache_verts = cache_verts;
    s_cache_indices = cache_indices;

    // Allocate initial payload buffers
    for (uint32_t i = 0; i < MAX_PAYLOADS; i++) {
        PassPayload* p = &s_pass_payloads[i];
        p->vert_cap = 512 * 1024;
        p->vert_buf = (uint8_t*)malloc(p->vert_cap);
        p->raw_cap = 1024 * 1024;
        p->raw_buf = (uint8_t*)malloc(p->raw_cap);
        p->idx_cap = 512 * 1024;
        p->idx_buf = (uint8_t*)malloc(p->idx_cap);
        p->params_cap = 64 * 1024;
        p->params_buf = (uint8_t*)malloc(p->params_cap);
        p->vparams_cap = 128 * 1024;
        p->vparams_buf = (uint8_t*)malloc(p->vparams_cap);
        p->cmds_cap = 8192;
        p->cmds = (GlDrawCmd*)malloc(p->cmds_cap * sizeof(GlDrawCmd));
    }

    mutexLock(&s_gl_mutex);
    // Pin Render Thread to Core 2 with priority 0x2B
    Result rc = threadCreate(&s_gl_thread, gl_thread_entry, NULL, NULL, 4u * 1024u * 1024u, 0x2B, 2);
    if (R_FAILED(rc)) {
        rc = threadCreate(&s_gl_thread, gl_thread_entry, NULL, NULL, 4u * 1024u * 1024u, 0x2B, -2);
    }
    if (R_FAILED(rc)) {
        mutexUnlock(&s_gl_mutex);
        switch_log("[gl] Failed to create Core 2 render thread");
        return -10;
    }

    threadStart(&s_gl_thread);
    s_gl_thread_initialized = true;

    // Wait for GL thread to complete EGL & Cache buffer initialization
    condvarWait(&s_gl_cond_sync_done, &s_gl_mutex);
    int res = s_gl_init_result;
    mutexUnlock(&s_gl_mutex);

    return res;
}

void switch_gl_shutdown(void) {
    if (!s_gl_thread_initialized) return;

    mutexLock(&s_gl_mutex);
    s_gl_exit = true;
    condvarWakeAll(&s_gl_cond_work);
    condvarWakeAll(&s_gl_cond_queue_free);
    condvarWakeAll(&s_gl_cond_payload_free);
    condvarWakeAll(&s_gl_cond_frame_done);
    condvarWakeAll(&s_gl_cond_sync_done);
    mutexUnlock(&s_gl_mutex);

    threadWaitForExit(&s_gl_thread);
    threadClose(&s_gl_thread);
    s_gl_thread_initialized = false;

    for (uint32_t i = 0; i < MAX_PAYLOADS; i++) {
        PassPayload* p = &s_pass_payloads[i];
        if (p->vert_buf) free(p->vert_buf);
        if (p->raw_buf) free(p->raw_buf);
        if (p->idx_buf) free(p->idx_buf);
        if (p->params_buf) free(p->params_buf);
        if (p->vparams_buf) free(p->vparams_buf);
        if (p->cmds) free(p->cmds);
    }
}

void switch_gl_resize_efb(uint32_t efb_w, uint32_t efb_h) {
    mutexLock(&s_gl_mutex);
    while (s_queue_head - s_queue_tail >= GL_QUEUE_CAPACITY && !s_gl_exit) {
        condvarWait(&s_gl_cond_queue_free, &s_gl_mutex);
    }
    GlCommand* cmd = &s_queue[s_queue_head % GL_QUEUE_CAPACITY];
    s_queue_head++;
    cmd->type = GL_CMD_RESIZE_EFB;
    cmd->resize_efb.efb_w = efb_w;
    cmd->resize_efb.efb_h = efb_h;
    condvarWakeAll(&s_gl_cond_work);
    condvarWait(&s_gl_cond_sync_done, &s_gl_mutex);
    mutexUnlock(&s_gl_mutex);
}

void switch_gl_get_window_size(uint32_t* out_w, uint32_t* out_h) {
    uint32_t is_docked = switch_is_docked_mode();
    if (out_w) *out_w = is_docked ? 1920 : 1280;
    if (out_h) *out_h = is_docked ? 1080 : 720;
}

uint32_t switch_gl_compile_fs(const char* src_ptr, uint32_t src_len) {
    uint32_t res = 0;
    mutexLock(&s_gl_mutex);
    while (s_queue_head - s_queue_tail >= GL_QUEUE_CAPACITY && !s_gl_exit) {
        condvarWait(&s_gl_cond_queue_free, &s_gl_mutex);
    }
    GlCommand* cmd = &s_queue[s_queue_head % GL_QUEUE_CAPACITY];
    s_queue_head++;
    cmd->type = GL_CMD_COMPILE_FS;
    cmd->compile_fs.src_ptr = src_ptr;
    cmd->compile_fs.src_len = src_len;
    cmd->out_u32 = &res;
    condvarWakeAll(&s_gl_cond_work);
    condvarWait(&s_gl_cond_sync_done, &s_gl_mutex);
    mutexUnlock(&s_gl_mutex);
    return res;
}

uint32_t switch_gl_link_program(uint32_t fs_id, uint32_t is_raw) {
    uint32_t res = 0;
    mutexLock(&s_gl_mutex);
    while (s_queue_head - s_queue_tail >= GL_QUEUE_CAPACITY && !s_gl_exit) {
        condvarWait(&s_gl_cond_queue_free, &s_gl_mutex);
    }
    GlCommand* cmd = &s_queue[s_queue_head % GL_QUEUE_CAPACITY];
    s_queue_head++;
    cmd->type = GL_CMD_LINK_PROGRAM;
    cmd->link_prog.fs_id = fs_id;
    cmd->link_prog.is_raw = is_raw;
    cmd->out_u32 = &res;
    condvarWakeAll(&s_gl_cond_work);
    condvarWait(&s_gl_cond_sync_done, &s_gl_mutex);
    mutexUnlock(&s_gl_mutex);
    return res;
}

uint32_t switch_gl_upload_texture(uint32_t existing_tex, uint32_t w, uint32_t h, const uint8_t* rgba) {
    uint32_t res = 0;
    mutexLock(&s_gl_mutex);
    while (s_queue_head - s_queue_tail >= GL_QUEUE_CAPACITY && !s_gl_exit) {
        condvarWait(&s_gl_cond_queue_free, &s_gl_mutex);
    }
    GlCommand* cmd = &s_queue[s_queue_head % GL_QUEUE_CAPACITY];
    s_queue_head++;
    cmd->type = GL_CMD_UPLOAD_TEXTURE;
    cmd->upload_tex.existing_tex = existing_tex;
    cmd->upload_tex.w = w;
    cmd->upload_tex.h = h;
    cmd->upload_tex.rgba = rgba;
    cmd->out_u32 = &res;
    condvarWakeAll(&s_gl_cond_work);
    condvarWait(&s_gl_cond_sync_done, &s_gl_mutex);
    mutexUnlock(&s_gl_mutex);
    return res;
}

uint32_t switch_gl_create_sampler(uint8_t wrap_s, uint8_t wrap_t, uint8_t linear) {
    uint32_t res = 0;
    mutexLock(&s_gl_mutex);
    while (s_queue_head - s_queue_tail >= GL_QUEUE_CAPACITY && !s_gl_exit) {
        condvarWait(&s_gl_cond_queue_free, &s_gl_mutex);
    }
    GlCommand* cmd = &s_queue[s_queue_head % GL_QUEUE_CAPACITY];
    s_queue_head++;
    cmd->type = GL_CMD_CREATE_SAMPLER;
    cmd->create_sampler.wrap_s = wrap_s;
    cmd->create_sampler.wrap_t = wrap_t;
    cmd->create_sampler.linear = linear;
    cmd->out_u32 = &res;
    condvarWakeAll(&s_gl_cond_work);
    condvarWait(&s_gl_cond_sync_done, &s_gl_mutex);
    mutexUnlock(&s_gl_mutex);
    return res;
}

void switch_gl_cache_upload(
    uint32_t vert_offset_elems,
    const void* verts,
    uint32_t vert_bytes,
    uint32_t idx_offset_elems,
    const void* indices,
    uint32_t idx_bytes
) {
    mutexLock(&s_gl_mutex);
    while (s_queue_head - s_queue_tail >= GL_QUEUE_CAPACITY && !s_gl_exit) {
        condvarWait(&s_gl_cond_queue_free, &s_gl_mutex);
    }
    GlCommand* cmd = &s_queue[s_queue_head % GL_QUEUE_CAPACITY];
    s_queue_head++;
    cmd->type = GL_CMD_CACHE_UPLOAD;
    cmd->cache_upload.vert_offset_elems = vert_offset_elems;
    cmd->cache_upload.verts = verts;
    cmd->cache_upload.vert_bytes = vert_bytes;
    cmd->cache_upload.idx_offset_elems = idx_offset_elems;
    cmd->cache_upload.indices = indices;
    cmd->cache_upload.idx_bytes = idx_bytes;
    condvarWakeAll(&s_gl_cond_work);
    condvarWait(&s_gl_cond_sync_done, &s_gl_mutex);
    mutexUnlock(&s_gl_mutex);
}

void switch_gl_execute_pass(const GlPassDesc* pass) {
    if (pass->cmd_count == 0 && !pass->has_clear) {
        return;
    }
    mutexLock(&s_gl_mutex);
    // Backpressure: if payload ring is full, wait for Core 2 to catch up
    while (s_pass_head - s_pass_tail >= MAX_PAYLOADS && !s_gl_exit) {
        condvarWait(&s_gl_cond_payload_free, &s_gl_mutex);
    }
    while (s_queue_head - s_queue_tail >= GL_QUEUE_CAPACITY && !s_gl_exit) {
        condvarWait(&s_gl_cond_queue_free, &s_gl_mutex);
    }
    if (s_gl_exit) {
        mutexUnlock(&s_gl_mutex);
        return;
    }

    uint32_t payload_idx = s_pass_head % MAX_PAYLOADS;
    PassPayload* payload = &s_pass_payloads[payload_idx];
    s_pass_head++;

    ensure_payload_buf(&payload->vert_buf, &payload->vert_cap, pass->vertices_bytes);
    if (pass->vertices_bytes > 0 && pass->vertices != NULL) {
        memcpy(payload->vert_buf, pass->vertices, pass->vertices_bytes);
    }
    payload->vert_bytes = pass->vertices_bytes;

    ensure_payload_buf(&payload->raw_buf, &payload->raw_cap, pass->raw_bytes);
    if (pass->raw_bytes > 0 && pass->raw_vertices != NULL) {
        memcpy(payload->raw_buf, pass->raw_vertices, pass->raw_bytes);
    }
    payload->raw_bytes = pass->raw_bytes;

    ensure_payload_buf(&payload->idx_buf, &payload->idx_cap, pass->indices_bytes);
    if (pass->indices_bytes > 0 && pass->indices != NULL) {
        memcpy(payload->idx_buf, pass->indices, pass->indices_bytes);
    }
    payload->idx_bytes = pass->indices_bytes;

    ensure_payload_buf(&payload->params_buf, &payload->params_cap, pass->params_bytes);
    if (pass->params_bytes > 0 && pass->params != NULL) {
        memcpy(payload->params_buf, pass->params, pass->params_bytes);
    }
    payload->params_bytes = pass->params_bytes;

    ensure_payload_buf(&payload->vparams_buf, &payload->vparams_cap, pass->vparams_bytes);
    if (pass->vparams_bytes > 0 && pass->vparams != NULL) {
        memcpy(payload->vparams_buf, pass->vparams, pass->vparams_bytes);
    }
    payload->vparams_bytes = pass->vparams_bytes;

    ensure_cmds_buf(&payload->cmds, &payload->cmds_cap, pass->cmd_count);
    if (pass->cmd_count > 0 && pass->cmds != NULL) {
        memcpy(payload->cmds, pass->cmds, pass->cmd_count * sizeof(GlDrawCmd));
    }
    payload->cmd_count = pass->cmd_count;
    payload->has_clear = pass->has_clear;
    if (pass->has_clear) {
        memcpy(payload->clear_color, pass->clear_color, sizeof(payload->clear_color));
    }

    GlCommand* cmd = &s_queue[s_queue_head % GL_QUEUE_CAPACITY];
    s_queue_head++;
    cmd->type = GL_CMD_EXECUTE_PASS;
    cmd->pass.payload_idx = payload_idx;

    condvarWakeAll(&s_gl_cond_work);
    mutexUnlock(&s_gl_mutex);
}

uint32_t switch_gl_copy_efb(uint32_t existing_tex, uint32_t alloc_new_size, uint32_t x, uint32_t y, uint32_t w, uint32_t h) {
    uint32_t tex = existing_tex;
    mutexLock(&s_gl_mutex);
    if (tex == 0) {
        if (s_pregen_tex_count > 0) {
            tex = s_pregen_tex[--s_pregen_tex_count];
            alloc_new_size = 1;
        }
    }
    while (s_queue_head - s_queue_tail >= GL_QUEUE_CAPACITY && !s_gl_exit) {
        condvarWait(&s_gl_cond_queue_free, &s_gl_mutex);
    }
    GlCommand* cmd = &s_queue[s_queue_head % GL_QUEUE_CAPACITY];
    s_queue_head++;
    cmd->type = GL_CMD_COPY_EFB;
    cmd->copy_efb.tex = tex;
    cmd->copy_efb.alloc_new_size = alloc_new_size;
    cmd->copy_efb.x = x;
    cmd->copy_efb.y = y;
    cmd->copy_efb.w = w;
    cmd->copy_efb.h = h;

    condvarWakeAll(&s_gl_cond_work);
    mutexUnlock(&s_gl_mutex);
    return tex;
}

uint32_t switch_gl_efb_color_tex(void) {
    return s_efb_color;
}

void switch_gl_present(
    uint32_t src_tex,
    const float uv_rect[4],
    float picture_aspect,
    const void* overlay_verts,
    uint32_t overlay_vert_count
) {
    mutexLock(&s_gl_mutex);
    // Backpressure: max 2 frames in flight
    while (s_frames_in_flight >= 2 && !s_gl_exit) {
        condvarWait(&s_gl_cond_frame_done, &s_gl_mutex);
    }
    while (s_queue_head - s_queue_tail >= GL_QUEUE_CAPACITY && !s_gl_exit) {
        condvarWait(&s_gl_cond_queue_free, &s_gl_mutex);
    }
    if (s_gl_exit) {
        mutexUnlock(&s_gl_mutex);
        return;
    }

    uint32_t slot = s_overlay_slot % 2;
    s_overlay_slot++;
    uint32_t count = overlay_vert_count > 4096 ? 4096 : overlay_vert_count;
    if (overlay_verts != NULL && count > 0) {
        memcpy(s_overlay_buf[slot], overlay_verts, count * OVERLAY_STRIDE);
    }

    GlCommand* cmd = &s_queue[s_queue_head % GL_QUEUE_CAPACITY];
    s_queue_head++;
    cmd->type = GL_CMD_PRESENT;
    cmd->present.src_tex = src_tex;
    if (uv_rect) {
        memcpy(cmd->present.uv_rect, uv_rect, sizeof(cmd->present.uv_rect));
    }
    cmd->present.picture_aspect = picture_aspect;
    cmd->present.overlay_verts = (overlay_verts != NULL && count > 0) ? s_overlay_buf[slot] : NULL;
    cmd->present.overlay_vert_count = count;

    s_frames_in_flight++;
    condvarWakeAll(&s_gl_cond_work);
    mutexUnlock(&s_gl_mutex);
}

void switch_gl_get_perf_stats(
    uint64_t* out_ring_upload_ns,
    uint64_t* out_state_apply_ns,
    uint64_t* out_draw_calls_ns,
    uint64_t* out_present_blit_ns,
    uint64_t* out_swap_buffers_ns,
    uint32_t* out_draw_count,
    uint32_t* out_batch_count
) {
    mutexLock(&s_gl_mutex);
    if (out_ring_upload_ns)  *out_ring_upload_ns  = s_gl_perf_ring_upload_ns;
    if (out_state_apply_ns)  *out_state_apply_ns  = s_gl_perf_state_apply_ns;
    if (out_draw_calls_ns)   *out_draw_calls_ns   = s_gl_perf_draw_calls_ns;
    if (out_present_blit_ns) *out_present_blit_ns = s_gl_perf_present_blit_ns;
    if (out_swap_buffers_ns) *out_swap_buffers_ns = s_gl_perf_swap_buffers_ns;
    if (out_draw_count)      *out_draw_count      = s_gl_perf_draw_count;
    if (out_batch_count)     *out_batch_count     = s_gl_perf_batch_count;

    s_gl_perf_ring_upload_ns  = 0;
    s_gl_perf_state_apply_ns  = 0;
    s_gl_perf_draw_calls_ns   = 0;
    s_gl_perf_present_blit_ns = 0;
    s_gl_perf_swap_buffers_ns = 0;
    s_gl_perf_draw_count      = 0;
    s_gl_perf_batch_count     = 0;
    mutexUnlock(&s_gl_mutex);
}
