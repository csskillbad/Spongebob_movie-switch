#include <switch.h>
#include <SDL2/SDL.h>
#include <stdio.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>

#include <setjmp.h>
#include "gl_backend.h"

// Symbols exported by libtssm_switch.a (Rust)
extern int      tssm_switch_init(void);
extern void     tssm_switch_guest_main(void);
extern void     tssm_switch_request_exit(void);
extern void     tssm_switch_set_docked(int docked);
extern void     tssm_switch_set_pad(uint32_t buttons);
extern void     tssm_switch_set_sticks(int32_t lx, int32_t ly, int32_t rx, int32_t ry);
extern size_t   tssm_switch_drain_audio(int16_t* out, size_t max_frames);
extern void     tssm_switch_set_boot_scene(const char* scene);
extern void     tssm_switch_warp(const char* scene);
extern int      tssm_switch_get_fps_enabled(void);
extern void     tssm_switch_set_fps_enabled(int val);
extern void     tssm_switch_set_scale_override(uint32_t scale);
extern uint32_t tssm_switch_get_current_scene(void);
extern uint32_t tssm_switch_get_frame_count(void);
extern uint32_t tssm_switch_get_game_mode(void);
extern void     tssm_switch_free_ctx(void* ctx);

static Mutex s_log_mutex;
static FILE* s_log_fp = NULL;

// ---------------------------------------------------------------------------------------------
// Crash reporter: on a CPU exception (data abort, misaligned access, bad instruction...) libnx
// calls __libnx_exception_handler on its own stack. We write the faulting PC/LR/FAR as offsets
// into this module (match them against tssm.elf with `nm`/`addr2line`) to sdmc:/tssm_crash.txt.
// ---------------------------------------------------------------------------------------------
u8 __nx_exception_stack[0x8000] __attribute__((aligned(16)));
u64 __nx_exception_stack_size = sizeof(__nx_exception_stack);
uint32_t switch_get_thread_index(void);

static const char* exception_name(u32 desc) {
    switch (desc) {
        case ThreadExceptionDesc_InstructionAbort: return "InstructionAbort";
        case ThreadExceptionDesc_MisalignedPC:     return "MisalignedPC";
        case ThreadExceptionDesc_MisalignedSP:     return "MisalignedSP";
        case ThreadExceptionDesc_SError:           return "SError";
        case ThreadExceptionDesc_BadSVC:           return "BadSVC";
        case ThreadExceptionDesc_Trap:             return "Trap";
        case ThreadExceptionDesc_Other:            return "Other (data abort / alignment fault)";
        default:                                   return "?";
    }
}

void __libnx_exception_handler(ThreadExceptionDump* ctx) {
    // Module base = start of the code region holding this function (the NSO's .text, i.e. ELF
    // address 0). `__start__` resolved to 0 here, which made every offset 0.
    MemoryInfo mi = {0};
    u32 page_info = 0;
    svcQueryMemory(&mi, &page_info, (u64)(uintptr_t)&__libnx_exception_handler);
    const u64 base = mi.addr;
    const u64 limit = base + 0x08000000ull; // module image (code is ~40 MB)
    if (s_log_fp) fflush(s_log_fp);
    FILE* f = fopen("sdmc:/tssm_crash.txt", "w");
    if (!f) f = fopen("sdmc:/switch/tssm/tssm_crash.txt", "w");
    if (!f) return;
    u32 ec = (ctx->esr >> 26) & 0x3F;
    fprintf(f, "TSSM crash: %s (desc=0x%X) ESR=0x%08X (EC=0x%02X) thread_index=%u\n",
            exception_name(ctx->error_desc), ctx->error_desc, ctx->esr, ec, (unsigned)switch_get_thread_index());
    fprintf(f, "module base = 0x%016llX   (offsets below are relative to it = addresses in tssm.elf)\n",
            (unsigned long long)base);
    #define OFF(v) ((v) >= base && (v) < limit ? (unsigned long long)((v) - base) : 0ull)
    fprintf(f, "PC  = 0x%016llX  off 0x%llX\n", (unsigned long long)ctx->pc.x, OFF(ctx->pc.x));
    fprintf(f, "LR  = 0x%016llX  off 0x%llX\n", (unsigned long long)ctx->lr.x, OFF(ctx->lr.x));
    fprintf(f, "FAR = 0x%016llX  (faulting data address)\n", (unsigned long long)ctx->far.x);
    fprintf(f, "SP  = 0x%016llX  FP = 0x%016llX\n", (unsigned long long)ctx->sp.x, (unsigned long long)ctx->fp.x);
    for (int i = 0; i < 29; i++) {
        fprintf(f, "x%-2d = 0x%016llX%s", i, (unsigned long long)ctx->cpu_gprs[i].x, (i % 2) ? "\n" : "   ");
    }
    fprintf(f, "\nlikely return addresses on the stack (offsets):\n");
    const u64* stack = (const u64*)(uintptr_t)ctx->sp.x;
    int found = 0;
    for (int i = 0; i < 512 && found < 32; i++) {
        u64 v = stack[i];
        if (v >= base + 0x1000 && v < limit && (v & 3) == 0) {
            fprintf(f, "  [sp+0x%03X] 0x%llX\n", i * 8, (unsigned long long)(v - base));
            found++;
        }
    }
    #undef OFF
    fclose(f);
}

void switch_log(const char* msg) {
    if (msg) {
        svcOutputDebugString(msg, strlen(msg));
        svcOutputDebugString("\n", 1);
        printf("%s\n", msg);
        fflush(stdout);

        mutexLock(&s_log_mutex);
        if (!s_log_fp) {
            s_log_fp = fopen("sdmc:/tssm.log", "a");
            if (!s_log_fp) {
                s_log_fp = fopen("sdmc:/switch/tssm/tssm.log", "a");
            }
        }
        if (s_log_fp) {
            fputs(msg, s_log_fp);
            fputc('\n', s_log_fp);
            fflush(s_log_fp);
        }
        mutexUnlock(&s_log_mutex);
    }
}

void switch_yield(void) {
    svcSleepThread(1000ULL);
}

void switch_sleep_ms(uint32_t ms) {
    svcSleepThread((uint64_t)ms * 1000000ULL);
}

void switch_sleep_ns(uint64_t ns) {
    svcSleepThread(ns);
}

uint64_t switch_now_ns(void) {
    return armTicksToNs(armGetSystemTick());
}

static volatile uint32_t s_is_docked = 1u;

uint32_t switch_is_docked_mode(void) {
    return s_is_docked;
}

void switch_stop_gx_worker(void);

void switch_exit_guest_thread(void) {
    switch_stop_gx_worker();
    switch_gl_shutdown();
    threadExit();
}

static Mutex             s_sched_mutex;
static CondVar           s_sched_cond;
static __thread uint32_t s_thread_index = 0;
static __thread uint32_t s_guest_thread = 0x817E0000;
static __thread int32_t  s_thread_prio  = 16;
static __thread jmp_buf  s_worker_jmp;
static __thread bool     s_is_worker = false;

void switch_sched_lock(void) {
    mutexLock(&s_sched_mutex);
}

void switch_sched_unlock(void) {
    mutexUnlock(&s_sched_mutex);
}

void switch_sched_wait(void) {
    condvarWait(&s_sched_cond, &s_sched_mutex);
}

void switch_sched_wake_all(void) {
    condvarWakeAll(&s_sched_cond);
}

uint32_t switch_get_thread_index(void) {
    return s_thread_index;
}

void switch_set_thread_index(uint32_t idx) {
    s_thread_index = idx;
}

uint32_t switch_get_guest_thread(void) {
    return s_guest_thread;
}

void switch_set_guest_thread(uint32_t gt) {
    s_guest_thread = gt;
}

int32_t switch_get_thread_prio(void) {
    return s_thread_prio;
}

void switch_set_thread_prio(int32_t prio) {
    s_thread_prio = prio;
}

void switch_exit_worker_thread(void) {
    if (s_is_worker) {
        longjmp(s_worker_jmp, 1);
    }
    threadExit();
}

// Thread-safe single-file handle cache to eliminate repeated RomFS/SD fopen/fclose overhead during HIP/HOP streaming
static Mutex s_file_mutex;
static FILE* s_cached_fp = NULL;
static char  s_cached_rel[256] = {0};

static void close_cached_file(void) {
    mutexLock(&s_file_mutex);
    if (s_cached_fp) {
        fclose(s_cached_fp);
        s_cached_fp = NULL;
        s_cached_rel[0] = '\0';
    }
    mutexUnlock(&s_file_mutex);
}

int switch_read_file(const char* path, uint32_t offset, uint32_t len, uint8_t* out) {
    if (!path || !out) return -1;

    mutexLock(&s_file_mutex);

    if (!s_cached_fp || strcmp(s_cached_rel, path) != 0) {
        if (s_cached_fp) {
            fclose(s_cached_fp);
            s_cached_fp = NULL;
            s_cached_rel[0] = '\0';
        }

        char full_path[512];
        snprintf(full_path, sizeof(full_path), "romfs:/files/%s", path);
        FILE* f = fopen(full_path, "rb");
        if (!f) {
            snprintf(full_path, sizeof(full_path), "disc/files/%s", path);
            f = fopen(full_path, "rb");
        }
        if (!f) {
            snprintf(full_path, sizeof(full_path), "sdmc:/switch/tssm/disc/files/%s", path);
            f = fopen(full_path, "rb");
        }
        if (!f) {
            mutexUnlock(&s_file_mutex);
            char err[512];
            snprintf(err, sizeof(err), "[switch_read_file] File not found: %s", path);
            switch_log(err);
            return -1;
        }

        s_cached_fp = f;
        strncpy(s_cached_rel, path, sizeof(s_cached_rel) - 1);
        s_cached_rel[sizeof(s_cached_rel) - 1] = '\0';
    }

    if (fseek(s_cached_fp, (long)offset, SEEK_SET) != 0) {
        mutexUnlock(&s_file_mutex);
        return -1;
    }

    size_t read_bytes = fread(out, 1, len, s_cached_fp);
    mutexUnlock(&s_file_mutex);
    return (int)read_bytes;
}

static Thread g_guest_thread;

static void guest_thread_fn(void* arg) {
    (void)arg;
    s_thread_index = 0;
    s_is_worker = false;
    switch_log("[switch] Dedicated Core 1 Game Engine thread started!");
    tssm_switch_guest_main();
    switch_log("[switch] Guest thread finished.");
}

typedef struct SwitchThread {
    Thread               thread;
    void                 (*entry)(void*);
    void*                arg;
    void*                ctx_to_free;
    uint32_t             index;
    struct SwitchThread* next;
} SwitchThread;

static SwitchThread* s_finished_threads = NULL;

static void reap_finished_threads(void) {
    switch_sched_lock();
    SwitchThread* list = s_finished_threads;
    s_finished_threads = NULL;
    switch_sched_unlock();

    while (list) {
        SwitchThread* next = list->next;
        threadWaitForExit(&list->thread);
        threadClose(&list->thread);
        free(list);
        list = next;
    }
}

static void thread_wrapper(void* arg) {
    SwitchThread* st = (SwitchThread*)arg;
    s_is_worker = true;
    s_thread_index = st->index;
    if (setjmp(s_worker_jmp) == 0) {
        st->entry(st->arg);
    }
    if (st->ctx_to_free) {
        tssm_switch_free_ctx(st->ctx_to_free);
        st->ctx_to_free = NULL;
    }
    switch_sched_lock();
    st->next = s_finished_threads;
    s_finished_threads = st;
    switch_sched_unlock();
}

int switch_create_worker_thread(
    void (*entry)(void*),
    void* arg,
    void* ctx_to_free,
    uint32_t index,
    uint32_t stack_size
) {
    reap_finished_threads();

    SwitchThread* st = (SwitchThread*)malloc(sizeof(SwitchThread));
    if (!st) return -1;
    memset(st, 0, sizeof(*st));
    st->entry = entry;
    st->arg = arg;
    st->ctx_to_free = ctx_to_free;
    st->index = index;
    if (stack_size < 1u * 1024u * 1024u) {
        stack_size = 1u * 1024u * 1024u;
    }
    Result rc = threadCreate(&st->thread, thread_wrapper, st, NULL, stack_size, 0x2C, 2);
    if (R_FAILED(rc)) {
        rc = threadCreate(&st->thread, thread_wrapper, st, NULL, stack_size, 0x2C, -2);
    }
    if (R_FAILED(rc)) {
        free(st);
        return -1;
    }
    rc = threadStart(&st->thread);
    if (R_FAILED(rc)) {
        threadClose(&st->thread);
        free(st);
        return -1;
    }
    return 0;
}

// Function-level profiler: a low-priority thread on Core 2 (it only runs while the GL thread is
// idle) that calls back into Rust every `period_ns` to sample which guest function is executing.
static Thread   s_prof_thread;
static void   (*s_prof_tick)(void) = NULL;
static uint64_t s_prof_period_ns = 200000;

static void prof_thread_fn(void* arg) {
    (void)arg;
    for (;;) {
        svcSleepThread((s64)s_prof_period_ns);
        if (s_prof_tick) s_prof_tick();
    }
}

int switch_start_prof_sampler(void (*tick)(void), uint64_t period_ns) {
    s_prof_tick = tick;
    s_prof_period_ns = period_ns;
    Result rc = threadCreate(&s_prof_thread, prof_thread_fn, NULL, NULL, 64u * 1024u, 0x3B, 2);
    if (R_FAILED(rc)) return -1;
    threadStart(&s_prof_thread);
    switch_log("[switch] Profiler sampler thread started on Core 2");
    return 0;
}

// Dedicated Core 0 GX Graphics Frontend Worker Thread (FIFO Parser, Display Lists, Textures, Passes)
static Thread  s_gx_worker;
static Mutex   s_gx_mutex;
static Mutex   s_gx_stop_mutex;
static CondVar s_gx_cond_work;
static CondVar s_gx_cond_done;
static bool    s_gx_worker_started = false;
static void  (*s_gx_entry)(void*) = NULL;
static void*   s_gx_arg = NULL;

static void gx_worker_trampoline(void* arg) {
    (void)arg;
    if (s_gx_entry) {
        s_gx_entry(s_gx_arg);
    }
}

int switch_start_gx_worker(void (*entry)(void*), void* arg) {
    mutexInit(&s_gx_mutex);
    mutexInit(&s_gx_stop_mutex);
    condvarInit(&s_gx_cond_work);
    condvarInit(&s_gx_cond_done);
    s_gx_entry = entry;
    s_gx_arg = arg;
    Result rc = threadCreate(&s_gx_worker, gx_worker_trampoline, NULL, NULL, 4u * 1024u * 1024u, 0x2C, 0);
    if (R_FAILED(rc)) {
        rc = threadCreate(&s_gx_worker, gx_worker_trampoline, NULL, NULL, 4u * 1024u * 1024u, 0x2C, -2);
    }
    if (R_SUCCEEDED(rc)) {
        s_gx_worker_started = true;
        threadStart(&s_gx_worker);
        switch_log("[switch] Dedicated Core 0 GX Frontend worker thread started!");
        return 0;
    }
    switch_log("[switch] Fatal: failed to spawn Core 0 GX Frontend worker thread.");
    return -1;
}

void switch_stop_gx_worker(void) {
    mutexLock(&s_gx_stop_mutex);
    if (s_gx_worker_started) {
        s_gx_worker_started = false;
        mutexLock(&s_gx_mutex);
        condvarWakeAll(&s_gx_cond_work);
        condvarWakeAll(&s_gx_cond_done);
        mutexUnlock(&s_gx_mutex);
        threadWaitForExit(&s_gx_worker);
        threadClose(&s_gx_worker);
    }
    mutexUnlock(&s_gx_stop_mutex);
}

void switch_gx_lock(void)      { mutexLock(&s_gx_mutex); }
void switch_gx_unlock(void)    { mutexUnlock(&s_gx_mutex); }
void switch_gx_wait_work(void) { condvarWait(&s_gx_cond_work, &s_gx_mutex); }
void switch_gx_wake_work(void) { condvarWakeAll(&s_gx_cond_work); }
void switch_gx_wait_done(void) { condvarWait(&s_gx_cond_done, &s_gx_mutex); }
void switch_gx_wake_done(void) { condvarWakeAll(&s_gx_cond_done); }

void switch_init_vertex_worker(void) {}

void switch_dispatch_worker(void (*fn)(void*), void* arg) {
    if (fn) fn(arg);
}

void switch_wait_worker(void) {}

// Allocate GameCube 128 MB guest address space (24 MB RAM + branchless WGPIPE FIFO & MMIO window)
int switch_file_exists(const char* path) {
    FILE* f = fopen(path, "rb");
    if (!f) return 0;
    fclose(f);
    return 1;
}

void* switch_alloc_guest_memory(void) {
    size_t size = (128u * 1024u * 1024u) + 65536u;
    void* ptr = malloc(size);
    if (!ptr) {
        switch_log("[switch] Failed to allocate 128MB guest address space!");
        return NULL;
    }
    memset(ptr, 0, size);
    return ptr;
}

static void audio_callback(void* userdata, Uint8* stream, int len) {
    (void)userdata;
    int16_t* out = (int16_t*)stream;
    size_t stereo_frames = (size_t)len / (2u * sizeof(int16_t));
    size_t got = tssm_switch_drain_audio(out, stereo_frames);
    if (got < stereo_frames) {
        memset(out + got * 2u, 0, (stereo_frames - got) * 2u * sizeof(int16_t));
    }
}

static const char* s_level_names[] = {
    "BB02", // Bikini Bottom
    "BB01", // No Cheese! (Patty Wagon Driving)
    "JK01", // Jellyfish Fields
    "AM01", // Thug Tug
    "TR01", // Trench
    "DE01", // Now That We're Men
    "SC02", // Shell City
    "PT01", // Planktopolis
};
#define NUM_LEVELS (sizeof(s_level_names) / sizeof(s_level_names[0]))
static int s_current_level_idx = 0;

int main(int argc, char* argv[]) {
    (void)argc;
    (void)argv;
    mutexInit(&s_file_mutex);
    mutexInit(&s_sched_mutex);
    condvarInit(&s_sched_cond);
    consoleDebugInit(debugDevice_SVC);
    setvbuf(stdout, NULL, _IONBF, 0);
    fsdevMountSdmc();
    switch_log("[switch] Starting The SpongeBob SquarePants Movie (Hardware GPU 60 FPS Port)...");

    // 1. Mount RomFS
    bool romfs_mounted = false;
    Result rc = romfsInit();
    if (R_SUCCEEDED(rc)) {
        romfs_mounted = true;
        switch_log("[switch] RomFS mounted successfully.");
    } else {
        char rmsg[128];
        snprintf(rmsg, sizeof(rmsg), "[switch] RomFS not available (0x%x), using SD fallback.", rc);
        switch_log(rmsg);
    }

    // 2. Configure Switch controller input
    padConfigureInput(1, HidNpadStyleSet_NpadStandard);
    PadState pad;
    padInitializeDefault(&pad);

    s_is_docked = (appletGetOperationMode() == AppletOperationMode_Console) ? 1u : 0u;
    bool is_docked = (switch_is_docked_mode() != 0);
    tssm_switch_set_docked(is_docked ? 1 : 0);

    // 3. Initialize 32 kHz stereo SDL2 audio output (audio subsystem only; EGL owns NWindow)
    SDL_AudioDeviceID audio_dev = 0;
    if (SDL_Init(SDL_INIT_AUDIO) == 0) {
        SDL_AudioSpec wanted, obtained;
        SDL_zero(wanted);
        wanted.freq = 32000;
        wanted.format = AUDIO_S16SYS;
        wanted.channels = 2;
        wanted.samples = 512;
        wanted.callback = audio_callback;

        audio_dev = SDL_OpenAudioDevice(NULL, 0, &wanted, &obtained, 0);
        if (audio_dev > 0) {
            SDL_PauseAudioDevice(audio_dev, 0);
            switch_log("[switch] 32kHz stereo hardware audio mixer online.");
        }
    } else {
        char emsg[128];
        snprintf(emsg, sizeof(emsg), "[switch] SDL_Init(SDL_INIT_AUDIO) failed: %s", SDL_GetError());
        switch_log(emsg);
    }

    // 4. Initialize recompiled game engine tables & audio state
    int num_funcs = tssm_switch_init();
    char init_msg[128];
    snprintf(init_msg, sizeof(init_msg), "[switch] TSSM initialized (%d recompiled symbols).", num_funcs);
    switch_log(init_msg);

    // Optional boot scene override from SD card
    FILE* sf = fopen("sdmc:/scene.txt", "r");
    if (!sf) sf = fopen("sdmc:/switch/tssm/scene.txt", "r");
    if (sf) {
        char sname[8] = {0};
        if (fscanf(sf, "%4s", sname) == 1 && strlen(sname) == 4) {
            tssm_switch_set_boot_scene(sname);
            for (size_t i = 0; i < NUM_LEVELS; i++) {
                if (strncmp(s_level_names[i], sname, 4) == 0) {
                    s_current_level_idx = (int)i;
                    break;
                }
            }
        }
        fclose(sf);
    }

    // Optional FPS HUD flag
    bool show_fps = false;

    FILE* cf = fopen("sdmc:/switch/tssm/fps_counter.txt", "r");
    if (!cf) cf = fopen("romfs:/config/fps_counter.txt", "r");
    if (cf) {
        int v = 0;
        if (fscanf(cf, "%d", &v) == 1) {
            show_fps = (v != 0);
        }
        fclose(cf);
    }

    FILE* scf = fopen("sdmc:/switch/tssm/scale.txt", "r");
    if (!scf) scf = fopen("romfs:/config/scale.txt", "r");
    if (scf) {
        int sc = 0;
        if (fscanf(scf, "%d", &sc) == 1 && sc >= 1 && sc <= 2) {
            tssm_switch_set_scale_override((uint32_t)sc);
            char sc_msg[64];
            snprintf(sc_msg, sizeof(sc_msg), "[switch] Scale override: %dx", sc);
            switch_log(sc_msg);
        }
        fclose(scf);
    }

    tssm_switch_set_fps_enabled(show_fps ? 1 : 0);

    // 5. Launch GameCube guest engine on dedicated Core 1 (spawns Core 0 GX worker & Core 2 GL renderer)
    size_t stack_size = 8u * 1024u * 1024u;
    Result trc = threadCreate(&g_guest_thread, guest_thread_fn, NULL, NULL, stack_size, 0x2C, 1);
    if (R_FAILED(trc)) {
        trc = threadCreate(&g_guest_thread, guest_thread_fn, NULL, NULL, stack_size, 0x2C, -2);
    }
    if (R_SUCCEEDED(trc)) {
        threadStart(&g_guest_thread);
    } else {
        switch_log("[switch] Fatal: could not spawn Core 1 guest thread!");
        return 1;
    }

    // 6. Lightweight Core 0 OS & input polling loop (125 Hz, < 0.05% CPU usage)
    u64 prev_kHeld = 0;
    uint32_t dock_poll = 0;

    while (appletMainLoop()) {
        if ((dock_poll++ & 31u) == 0) {
            s_is_docked = (appletGetOperationMode() == AppletOperationMode_Console) ? 1u : 0u;
        }
        bool current_docked = (switch_is_docked_mode() != 0);
        if (current_docked != is_docked) {
            is_docked = current_docked;
            tssm_switch_set_docked(is_docked ? 1 : 0);
            switch_log(is_docked ? "[switch] Mode -> Docked (1080p)" : "[switch] Mode -> Handheld (720p)");
        }

        padUpdate(&pad);
        u64 kHeld = padGetButtons(&pad);
        u64 kDown = kHeld & ~prev_kHeld;

        // Clean exit on Plus + Minus
        if ((kHeld & HidNpadButton_Plus) && (kHeld & HidNpadButton_Minus)) {
            switch_log("[switch] Plus+Minus pressed: requesting clean shutdown.");
            break;
        }

        // Toggle GPU FPS HUD pill with Minus (-)
        if ((kDown & HidNpadButton_Minus) && !(kHeld & HidNpadButton_Plus)) {
            show_fps = !show_fps;
            tssm_switch_set_fps_enabled(show_fps ? 1 : 0);
        }

        // Level warp shortcut: hold ZL + press D-Pad Left / Right
        bool warp_combo = false;
        if (kHeld & HidNpadButton_ZL) {
            if (kDown & HidNpadButton_Right) {
                s_current_level_idx = (s_current_level_idx + 1) % (int)NUM_LEVELS;
                tssm_switch_warp(s_level_names[s_current_level_idx]);
                warp_combo = true;
            } else if (kDown & HidNpadButton_Left) {
                s_current_level_idx = (s_current_level_idx + (int)NUM_LEVELS - 1) % (int)NUM_LEVELS;
                tssm_switch_warp(s_level_names[s_current_level_idx]);
                warp_combo = true;
            }
        }
        prev_kHeld = kHeld;

        uint32_t gc_buttons = 0;
        if (kHeld & HidNpadButton_A)    gc_buttons |= 0x0100; // GC A
        if (kHeld & HidNpadButton_B)    gc_buttons |= 0x0200; // GC B
        if (kHeld & HidNpadButton_X)    gc_buttons |= 0x0400; // GC X
        if (kHeld & HidNpadButton_Y)    gc_buttons |= 0x0800; // GC Y
        if (kHeld & HidNpadButton_Plus) gc_buttons |= 0x1000; // GC Start
        if (kHeld & HidNpadButton_L)    gc_buttons |= 0x0040; // GC L
        if (kHeld & HidNpadButton_R)    gc_buttons |= 0x0020; // GC R
        if (kHeld & HidNpadButton_ZR)   gc_buttons |= 0x0020; // GC R
        if ((kHeld & HidNpadButton_ZL) && !warp_combo) gc_buttons |= 0x0010; // GC Z
        if (kHeld & HidNpadButton_Up)   gc_buttons |= 0x0008; // GC D-Up
        if (kHeld & HidNpadButton_Down) gc_buttons |= 0x0004; // GC D-Down
        if (!warp_combo) {
            if (kHeld & HidNpadButton_Left)  gc_buttons |= 0x0001; // GC D-Left
            if (kHeld & HidNpadButton_Right) gc_buttons |= 0x0002; // GC D-Right
        }

        HidAnalogStickState l_stick = padGetStickPos(&pad, 0);
        HidAnalogStickState r_stick = padGetStickPos(&pad, 1);

        tssm_switch_set_pad(gc_buttons);
        tssm_switch_set_sticks(l_stick.x, l_stick.y, r_stick.x, r_stick.y);

        // Sleep 8 ms on Core 0 (125 Hz input polling, virtually zero CPU usage)
        svcSleepThread(8000000ULL);
    }

    // 7. Clean shutdown
    tssm_switch_request_exit();
    switch_gx_lock();
    switch_gx_wake_work();
    switch_gx_wake_done();
    switch_gx_unlock();
    threadWaitForExit(&g_guest_thread);
    threadClose(&g_guest_thread);
    reap_finished_threads();
    switch_stop_gx_worker();
    switch_gl_shutdown();

    if (audio_dev > 0) {
        SDL_CloseAudioDevice(audio_dev);
    }
    SDL_Quit();
    close_cached_file();
    if (romfs_mounted) {
        romfsExit();
    }
    mutexLock(&s_log_mutex);
    if (s_log_fp) {
        fclose(s_log_fp);
        s_log_fp = NULL;
    }
    mutexUnlock(&s_log_mutex);
    fsdevUnmountDevice("sdmc");

    return 0;
}
