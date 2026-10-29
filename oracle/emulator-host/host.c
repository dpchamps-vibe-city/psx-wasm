/*
 * psxw-emu-host: a headless libretro frontend for the psx-wasm device
 * oracle (plans/completed/M03-oracle-harness.md, "Emulator host").
 *
 * It dlopen()s a libretro core (Beetle PSX), answers the environment
 * callbacks with a fixed, accuracy-first set of core options, loads the
 * runner as content, and exchanges batches with the runner through a RAM
 * mailbox it reads and writes via retro_get_memory_data(SYSTEM_RAM),
 * polling once per retro_run(). It knows nothing about vector kinds: the
 * driver (crates/psxw-oracle) prepares the exchanges.
 *
 * Modes:
 *   info                         print core, BIOS and option facts as JSON
 *   exe (--disc R.cue | --exe R.exe) --vectors IN --out OUT [--provenance P]
 *                                boot the runner, run every exchange in IN.
 *                                Under OpenBIOS only --disc works: Beetle's
 *                                PS-X EXE loader hooks a Sony-BIOS address.
 *   play --disc GAME.cue --replay R --frames N --out DIR
 *        [--trace-u32 ADDR] [--dump-at F,..] [--dump-counter V,..]
 *        [--frames-every N] [--dump-frames] [--dump-vram]
 *                                run a game from a psx-wasm replay file
 *                                (crates/psxw-abi/src/replay.rs): the pad
 *                                state of frame k is what the core reads
 *                                during retro_run k. After each frame the
 *                                host records the word at ADDR (the game's
 *                                frame counter) and dumps the 2 MiB of RAM
 *                                at the listed frames, and at the first
 *                                frame the counter equals each listed value.
 *                                Writes DIR/trace.bin, DIR/ram_*.bin and
 *                                DIR/play.json. Frame numbers, file names
 *                                and button words match psxw-host-native.
 *                                With --frames-every N, every Nth presented
 *                                frame goes to DIR/frames/frame_NNNNNN.ppm.
 *                                With --dump-frames, each RAM dump also
 *                                writes the picture the core presented in
 *                                that frame beside it (ram_X.bin ->
 *                                frame_X.ppm); with --dump-vram, VRAM
 *                                (vram_X.bin: 1024x512 16-bit words, LE),
 *                                read from the core's savestate
 *                                (retro_serialize), the only libretro call
 *                                that exposes it.
 */

#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

#include "libretro.h"
#include "psxw_oracle_wire.h"
#include "sha1.h"

#define HOST_VERSION 1

#define EXIT_USAGE 1
#define EXIT_BOOT 3
#define EXIT_EXCHANGE 4
#define EXIT_PROTOCOL 5

/* ---- core options ------------------------------------------------------ */

/*
 * The oracle's fixed core options. `required` options must be declared by
 * the core with this value among its allowed values, or the host refuses
 * to run: an option rename on a Beetle bump must not silently fall back to
 * a default. Optional ones only exist in some builds (hardware renderer,
 * dynarec) and apply when declared. Everything else takes the core's
 * declared default; the effective value of every option goes into the
 * provenance.
 */
struct fixed_option {
    const char *key;
    const char *value;
    bool required;
};

static const struct fixed_option FIXED_OPTIONS[] = {
    /* No overclocking, no enhancements. */
    {"beetle_psx_cpu_freq_scale", "100%", true},
    {"beetle_psx_gpu_overclock", "1x(native)", true},
    {"beetle_psx_gte_overclock", "disabled", true},
    {"beetle_psx_internal_resolution", "1x(native)", true},
    {"beetle_psx_dither_mode", "1x(native)", true},
    {"beetle_psx_pgxp_mode", "disabled", true},
    {"beetle_psx_widescreen_hack", "disabled", true},
    /* The Line-to-Quad hack defaults on and rewrites thin triangles. */
    {"beetle_psx_line_render", "disabled", true},
    /* Skipping released voices leaves SPU register state inexact. */
    {"beetle_psx_spu_silent_voice", "disabled", true},
    /* Per-game settings must not override the above. */
    {"beetle_psx_compatibility_settings", "disabled", true},
    /* CD: native speed, synchronous access (maximum timing fidelity). */
    {"beetle_psx_cd_access_method", "sync", true},
    {"beetle_psx_cd_fastload", "2x(native)", true},
    /* BIOS: OpenBIOS from the system directory; the Sony-BIOS skip patch
     * would corrupt it. */
    {"beetle_psx_override_bios", "openbios", true},
    {"beetle_psx_skip_bios", "disabled", true},
    {"beetle_psx_region", "ntsc-u", true},
    /* Present only in some builds. */
    {"beetle_psx_renderer", "software", false},
    {"beetle_psx_depth", "16bpp(native)", false},
    {"beetle_psx_cpu_dynarec", "disabled", false},
    {"beetle_psx_pgxp_vertex", "disabled", false},
    {"beetle_psx_pgxp_texture", "disabled", false},
    {"beetle_psx_pgxp_nclip", "disabled", false},
    {"beetle_psx_pgxp_2d_tol", "disabled", false},
    {"beetle_psx_track_textures", "disabled", false},
    {"beetle_psx_dump_textures", "disabled", false},
    {"beetle_psx_replace_textures", "disabled", false},
    {"beetle_psx_filter", "nearest", false},
    {"beetle_psx_msaa", "1x", false},
    {"beetle_psx_super_sampling", "disabled", false},
    {"beetle_psx_mdec_yuv", "disabled", false},
    {"beetle_psx_analog_toggle", "disabled", false},
    {"beetle_psx_enable_multitap_port1", "disabled", false},
    {"beetle_psx_enable_multitap_port2", "disabled", false},
    {"beetle_psx_enable_memcard1", "disabled", false},
    {"beetle_psx_pal_video_timing_override", "disabled", false},
    {"beetle_psx_display_osd", "disabled", false},
    {"beetle_psx_display_internal_fps", "disabled", false},
};
#define N_FIXED (sizeof FIXED_OPTIONS / sizeof FIXED_OPTIONS[0])

#define MAX_DECLS 512
#define MAX_VALUES 128
struct decl {
    char *key;
    char *def;
    char *values[MAX_VALUES];
    int nvalues;
    bool queried;
};
static struct decl decls[MAX_DECLS];
static int ndecls;

#define MAX_CLI_OPTIONS 64
static char *cli_keys[MAX_CLI_OPTIONS], *cli_vals[MAX_CLI_OPTIONS];
static int ncli;

/* ---- host state -------------------------------------------------------- */

static const char *core_path = "/opt/psxw/lib/mednafen_psx_libretro.so";
static const char *system_dir = "/opt/psxw/system";
static const char *work_dir = ".";
static char save_dir[4096];
static FILE *log_file;
static bool verbose;
static uint64_t frames_presented;
static unsigned unknown_env[64];
static int nunknown_env;

static void die(int code, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    fprintf(stderr, "psxw-emu-host: ");
    vfprintf(stderr, fmt, ap);
    fprintf(stderr, "\n");
    va_end(ap);
    if (log_file) {
        va_start(ap, fmt);
        fprintf(log_file, "[host] fatal: ");
        vfprintf(log_file, fmt, ap);
        fprintf(log_file, "\n");
        va_end(ap);
        fflush(log_file);
    }
    exit(code);
}

static char *xstrdup(const char *s) {
    char *d = strdup(s ? s : "");
    if (!d) die(EXIT_USAGE, "out of memory");
    return d;
}

static struct decl *find_decl(const char *key) {
    for (int i = 0; i < ndecls; i++)
        if (!strcmp(decls[i].key, key)) return &decls[i];
    return NULL;
}

static void declare(const char *key, const char *def, const char *const *values, int nvalues) {
    if (!key) return;
    struct decl *d = find_decl(key);
    if (!d) {
        if (ndecls == MAX_DECLS) die(EXIT_PROTOCOL, "too many core options");
        d = &decls[ndecls++];
        d->key = xstrdup(key);
    } else {
        free(d->def);
        for (int i = 0; i < d->nvalues; i++) free(d->values[i]);
    }
    d->nvalues = 0;
    for (int i = 0; i < nvalues && i < MAX_VALUES; i++) d->values[d->nvalues++] = xstrdup(values[i]);
    d->def = xstrdup(def ? def : (d->nvalues ? d->values[0] : ""));
}

static void declare_v1(const struct retro_core_option_definition *defs) {
    for (; defs && defs->key; defs++) {
        const char *vals[RETRO_NUM_CORE_OPTION_VALUES_MAX];
        int n = 0;
        for (; n < RETRO_NUM_CORE_OPTION_VALUES_MAX && defs->values[n].value; n++) vals[n] = defs->values[n].value;
        declare(defs->key, defs->default_value, vals, n);
    }
}

static void declare_v2(const struct retro_core_options_v2 *opts) {
    if (!opts) return;
    for (const struct retro_core_option_v2_definition *d = opts->definitions; d && d->key; d++) {
        const char *vals[RETRO_NUM_CORE_OPTION_VALUES_MAX];
        int n = 0;
        for (; n < RETRO_NUM_CORE_OPTION_VALUES_MAX && d->values[n].value; n++) vals[n] = d->values[n].value;
        declare(d->key, d->default_value, vals, n);
    }
}

/* SET_VARIABLES: "Description; first|second|third", first is the default. */
static void declare_variables(const struct retro_variable *vars) {
    for (; vars && vars->key; vars++) {
        const char *semi = vars->value ? strchr(vars->value, ';') : NULL;
        if (!semi) continue;
        char *list = xstrdup(semi + 1);
        const char *vals[MAX_VALUES];
        int n = 0;
        for (char *tok = strtok(list, "|"); tok && n < MAX_VALUES; tok = strtok(NULL, "|")) {
            while (*tok == ' ') tok++;
            vals[n++] = tok;
        }
        declare(vars->key, n ? vals[0] : "", vals, n);
        free(list);
    }
}

/* The value the core gets for `key`: CLI override, fixed option, default. */
static const char *option_value(const char *key) {
    for (int i = ncli - 1; i >= 0; i--)
        if (!strcmp(cli_keys[i], key)) return cli_vals[i];
    for (size_t i = 0; i < N_FIXED; i++)
        if (!strcmp(FIXED_OPTIONS[i].key, key)) return FIXED_OPTIONS[i].value;
    struct decl *d = find_decl(key);
    return d ? d->def : NULL;
}

static bool value_allowed(const struct decl *d, const char *v) {
    if (d->nvalues == 0) return true;
    for (int i = 0; i < d->nvalues; i++)
        if (!strcmp(d->values[i], v)) return true;
    return false;
}

/* Every required option must be declared and accept its fixed value. */
static void check_options(void) {
    for (size_t i = 0; i < N_FIXED; i++) {
        const struct fixed_option *f = &FIXED_OPTIONS[i];
        struct decl *d = find_decl(f->key);
        if (!d) {
            if (f->required) die(EXIT_PROTOCOL, "core does not declare required option %s", f->key);
            continue;
        }
        if (!value_allowed(d, f->value))
            die(EXIT_PROTOCOL, "core option %s does not accept the fixed value \"%s\"", f->key, f->value);
    }
    for (int i = 0; i < ncli; i++) {
        struct decl *d = find_decl(cli_keys[i]);
        if (!d) die(EXIT_USAGE, "--option %s: the core declares no such option", cli_keys[i]);
        if (!value_allowed(d, cli_vals[i])) die(EXIT_USAGE, "--option %s: value \"%s\" not allowed", cli_keys[i], cli_vals[i]);
    }
}

/* ---- libretro callbacks ------------------------------------------------ */

static void core_log(enum retro_log_level level, const char *fmt, ...) {
    static const char *names[] = {"debug", "info", "warn", "error"};
    const char *name = level <= RETRO_LOG_ERROR ? names[level] : "?";
    va_list ap;
    if (log_file) {
        va_start(ap, fmt);
        fprintf(log_file, "[core:%s] ", name);
        vfprintf(log_file, fmt, ap);
        va_end(ap);
    }
    if (verbose || level >= RETRO_LOG_WARN) {
        va_start(ap, fmt);
        fprintf(stderr, "[core:%s] ", name);
        vfprintf(stderr, fmt, ap);
        va_end(ap);
    }
}

/* The core's video format (SET_PIXEL_FORMAT), for --frames-every. */
static enum retro_pixel_format pixel_format = RETRO_PIXEL_FORMAT_0RGB1555;

static bool environment(unsigned cmd, void *data) {
    switch (cmd) {
    case RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY:
        *(const char **)data = system_dir;
        return true;
    case RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY:
        *(const char **)data = save_dir;
        return true;
    case RETRO_ENVIRONMENT_GET_CORE_ASSETS_DIRECTORY:
        *(const char **)data = work_dir;
        return true;
    case RETRO_ENVIRONMENT_GET_LOG_INTERFACE:
        ((struct retro_log_callback *)data)->log = core_log;
        return true;
    case RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION:
        *(unsigned *)data = 2;
        return true;
    case RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2:
        declare_v2((const struct retro_core_options_v2 *)data);
        return true;
    case RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2_INTL:
        declare_v2(((const struct retro_core_options_v2_intl *)data)->us);
        return true;
    case RETRO_ENVIRONMENT_SET_CORE_OPTIONS:
        declare_v1((const struct retro_core_option_definition *)data);
        return true;
    case RETRO_ENVIRONMENT_SET_CORE_OPTIONS_INTL:
        declare_v1(((const struct retro_core_options_intl *)data)->us);
        return true;
    case RETRO_ENVIRONMENT_SET_VARIABLES:
        declare_variables((const struct retro_variable *)data);
        return true;
    case RETRO_ENVIRONMENT_GET_VARIABLE: {
        struct retro_variable *v = (struct retro_variable *)data;
        if (!v || !v->key) return false;
        struct decl *d = find_decl(v->key);
        if (d) d->queried = true;
        v->value = option_value(v->key);
        return v->value != NULL;
    }
    case RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE:
        *(bool *)data = false;
        return true;
    case RETRO_ENVIRONMENT_SET_PIXEL_FORMAT:
        pixel_format = *(const enum retro_pixel_format *)data;
        return true;
    case RETRO_ENVIRONMENT_GET_LANGUAGE:
        *(unsigned *)data = RETRO_LANGUAGE_ENGLISH;
        return true;
    case RETRO_ENVIRONMENT_GET_CAN_DUPE:
        *(bool *)data = true;
        return true;
    case RETRO_ENVIRONMENT_GET_AUDIO_VIDEO_ENABLE:
        *(int *)data = 3; /* video and audio on; no fast savestates */
        return true;
    case RETRO_ENVIRONMENT_GET_FASTFORWARDING:
        *(bool *)data = false;
        return true;
    case RETRO_ENVIRONMENT_GET_DISK_CONTROL_INTERFACE_VERSION:
        *(unsigned *)data = 1;
        return true;
    case RETRO_ENVIRONMENT_GET_MESSAGE_INTERFACE_VERSION:
        *(unsigned *)data = 1;
        return true;
    case RETRO_ENVIRONMENT_SET_MESSAGE:
        core_log(RETRO_LOG_INFO, "message: %s\n", ((const struct retro_message *)data)->msg);
        return true;
    case RETRO_ENVIRONMENT_SET_MESSAGE_EXT:
        core_log(RETRO_LOG_INFO, "message: %s\n", ((const struct retro_message_ext *)data)->msg);
        return true;
    case RETRO_ENVIRONMENT_SET_CORE_OPTIONS_DISPLAY:
    case RETRO_ENVIRONMENT_SET_CORE_OPTIONS_UPDATE_DISPLAY_CALLBACK:
    case RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS:
    case RETRO_ENVIRONMENT_SET_CONTROLLER_INFO:
    case RETRO_ENVIRONMENT_SET_SUBSYSTEM_INFO:
    case RETRO_ENVIRONMENT_SET_MEMORY_MAPS:
    case RETRO_ENVIRONMENT_SET_GEOMETRY:
    case RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO:
    case RETRO_ENVIRONMENT_SET_PERFORMANCE_LEVEL:
    case RETRO_ENVIRONMENT_SET_SERIALIZATION_QUIRKS:
    case RETRO_ENVIRONMENT_SET_SUPPORT_ACHIEVEMENTS:
    case RETRO_ENVIRONMENT_SET_CONTENT_INFO_OVERRIDE:
    case RETRO_ENVIRONMENT_SET_DISK_CONTROL_INTERFACE:
    case RETRO_ENVIRONMENT_SET_DISK_CONTROL_EXT_INTERFACE:
        return true;
    /* Refused: no hardware rendering, VFS, rumble, LEDs, HDR, perf. */
    case RETRO_ENVIRONMENT_SET_HW_RENDER:
    case RETRO_ENVIRONMENT_GET_PREFERRED_HW_RENDER:
    case RETRO_ENVIRONMENT_GET_VFS_INTERFACE:
    case RETRO_ENVIRONMENT_GET_RUMBLE_INTERFACE:
    case RETRO_ENVIRONMENT_GET_LED_INTERFACE:
    case RETRO_ENVIRONMENT_GET_PERF_INTERFACE:
    case RETRO_ENVIRONMENT_GET_INPUT_BITMASKS:
    case RETRO_ENVIRONMENT_GET_CURRENT_SOFTWARE_FRAMEBUFFER:
        return false;
    default: {
        bool seen = false;
        for (int i = 0; i < nunknown_env; i++) seen |= unknown_env[i] == cmd;
        if (!seen && nunknown_env < 64) {
            unknown_env[nunknown_env++] = cmd;
            if (log_file) fprintf(log_file, "[host] environment command %u (0x%x) refused\n", cmd, cmd);
        }
        return false;
    }
    }
}

/* play --frames-every: the frame retro_run is producing, and where its
 * pictures go. */
static uint64_t frames_run;
static uint64_t frames_every;
static const char *frames_dir;

static size_t bytes_per_pixel(enum retro_pixel_format f) {
    return f == RETRO_PIXEL_FORMAT_XRGB8888 ? 4 : 2;
}

/* A picture in the core's pixel format as a binary PPM. */
static void write_ppm_file(const char *path, const void *data, unsigned width, unsigned height, size_t pitch,
                           enum retro_pixel_format fmt) {
    FILE *f = fopen(path, "wb");
    if (!f) die(EXIT_USAGE, "create %s: %s", path, strerror(errno));
    fprintf(f, "P6\n%u %u\n255\n", width, height);
    for (unsigned y = 0; y < height; y++) {
        const uint8_t *row = (const uint8_t *)data + y * pitch;
        for (unsigned x = 0; x < width; x++) {
            uint8_t rgb[3];
            if (fmt == RETRO_PIXEL_FORMAT_XRGB8888) {
                uint32_t p = ((const uint32_t *)row)[x];
                rgb[0] = p >> 16, rgb[1] = p >> 8, rgb[2] = p;
            } else {
                uint16_t p = ((const uint16_t *)row)[x];
                if (fmt == RETRO_PIXEL_FORMAT_RGB565)
                    rgb[0] = (p >> 11) << 3, rgb[1] = ((p >> 5) & 63) << 2, rgb[2] = (p & 31) << 3;
                else
                    rgb[0] = ((p >> 10) & 31) << 3, rgb[1] = ((p >> 5) & 31) << 3, rgb[2] = (p & 31) << 3;
            }
            fwrite(rgb, 1, 3, f);
        }
    }
    if (fclose(f) != 0) die(EXIT_USAGE, "write %s", path);
}

/* One presented frame as DIR/frames/frame_NNNNNN.ppm. */
static void write_ppm(const void *data, unsigned width, unsigned height, size_t pitch) {
    char path[4096];
    snprintf(path, sizeof path, "%s/frame_%06llu.ppm", frames_dir, (unsigned long long)frames_run);
    write_ppm_file(path, data, width, height, pitch, pixel_format);
}

/* play --dump-frames: a copy of the last picture the core presented (a
 * duped frame, data NULL, shows the previous one), so a dump decided after
 * retro_run returns can write it. */
static bool keep_frames;
static uint8_t *kept;
static size_t kept_cap;
static unsigned kept_w, kept_h;
static enum retro_pixel_format kept_fmt;

static void keep_frame(const void *data, unsigned width, unsigned height, size_t pitch) {
    size_t row = width * bytes_per_pixel(pixel_format);
    if (row * height > kept_cap) {
        free(kept);
        kept_cap = row * height;
        kept = malloc(kept_cap);
        if (!kept) die(EXIT_USAGE, "out of memory keeping a frame");
    }
    for (unsigned y = 0; y < height; y++) memcpy(kept + y * row, (const uint8_t *)data + y * pitch, row);
    kept_w = width, kept_h = height, kept_fmt = pixel_format;
}

static void video_refresh(const void *data, unsigned width, unsigned height, size_t pitch) {
    frames_presented++;
    if (frames_every && data && frames_run % frames_every == 0) write_ppm(data, width, height, pitch);
    if (keep_frames && data) keep_frame(data, width, height, pitch);
}

static void audio_sample(int16_t left, int16_t right) {
    (void)left;
    (void)right;
}

static size_t audio_sample_batch(const int16_t *data, size_t frames) {
    (void)data;
    return frames;
}

static void input_poll(void) {}

/* play mode: the replay, and the frame retro_run is producing. Version 2
 * replays may name a clock: the guest address of a u32 frame counter whose
 * value picks the entry, instead of the frame number (psxw-abi replay). */
static const uint8_t *replay;
static size_t replay_len;
static uint32_t replay_frames;
static size_t replay_data;
static uint32_t replay_clock;
/* Version 2: bit n set means port n+1 has no pad (psxw-abi replay). */
static uint32_t replay_unplugged;

#define REPLAY_HEADER_LEN_V1 56
#define REPLAY_HEADER_LEN 64
#define REPLAY_FRAME_LEN 4

/*
 * Beetle builds the pad word from RetroPad ids with this table
 * (input.c, input_map_controller): PlayStation bit i is RetroPad id
 * PAD_MAP[i]. Replays hold the word libpad reports, active low.
 */
static const unsigned PAD_MAP[16] = {
    RETRO_DEVICE_ID_JOYPAD_SELECT, RETRO_DEVICE_ID_JOYPAD_L3, RETRO_DEVICE_ID_JOYPAD_R3, RETRO_DEVICE_ID_JOYPAD_START,
    RETRO_DEVICE_ID_JOYPAD_UP,     RETRO_DEVICE_ID_JOYPAD_RIGHT, RETRO_DEVICE_ID_JOYPAD_DOWN, RETRO_DEVICE_ID_JOYPAD_LEFT,
    RETRO_DEVICE_ID_JOYPAD_L2,     RETRO_DEVICE_ID_JOYPAD_R2, RETRO_DEVICE_ID_JOYPAD_L, RETRO_DEVICE_ID_JOYPAD_R,
    RETRO_DEVICE_ID_JOYPAD_X,      RETRO_DEVICE_ID_JOYPAD_A, RETRO_DEVICE_ID_JOYPAD_B, RETRO_DEVICE_ID_JOYPAD_Y,
};

/* The core's main RAM (2 MiB), once a game is loaded. */
static uint8_t *ram;
static size_t ram_size;

static uint16_t replay_buttons(unsigned port) {
    if (!replay || port > 1) return 0xFFFF;
    uint64_t at = frames_run;
    if (replay_clock) {
        uint32_t o = replay_clock & 0x1FFFFCu;
        if (!ram || o + 4 > ram_size) return 0xFFFF;
        at = (uint32_t)ram[o] | (uint32_t)ram[o + 1] << 8 | (uint32_t)ram[o + 2] << 16 | (uint32_t)ram[o + 3] << 24;
    }
    if (at >= replay_frames) return 0xFFFF;
    const uint8_t *p = replay + replay_data + at * REPLAY_FRAME_LEN + 2 * port;
    return (uint16_t)(p[0] | p[1] << 8);
}

static int16_t input_state(unsigned port, unsigned device, unsigned index, unsigned id) {
    if (device != RETRO_DEVICE_JOYPAD || index != 0) return 0;
    uint16_t word = replay_buttons(port);
    for (int i = 0; i < 16; i++)
        if (PAD_MAP[i] == id) return !(word >> i & 1);
    return 0;
}

/* ---- the core ---------------------------------------------------------- */

static struct {
    void *handle;
    void (*set_environment)(retro_environment_t);
    void (*set_video_refresh)(retro_video_refresh_t);
    void (*set_audio_sample)(retro_audio_sample_t);
    void (*set_audio_sample_batch)(retro_audio_sample_batch_t);
    void (*set_input_poll)(retro_input_poll_t);
    void (*set_input_state)(retro_input_state_t);
    void (*init)(void);
    void (*deinit)(void);
    unsigned (*api_version)(void);
    void (*get_system_info)(struct retro_system_info *);
    void (*get_system_av_info)(struct retro_system_av_info *);
    void (*set_controller_port_device)(unsigned, unsigned);
    void (*run)(void);
    bool (*load_game)(const struct retro_game_info *);
    void (*unload_game)(void);
    void *(*get_memory_data)(unsigned);
    size_t (*get_memory_size)(unsigned);
    size_t (*serialize_size)(void);
    bool (*serialize)(void *, size_t);
} core;

static void *sym(const char *name) {
    void *p = dlsym(core.handle, name);
    if (!p) die(EXIT_USAGE, "%s: missing symbol %s", core_path, name);
    return p;
}

static void core_open(void) {
    core.handle = dlopen(core_path, RTLD_NOW | RTLD_LOCAL);
    if (!core.handle) die(EXIT_USAGE, "dlopen %s: %s", core_path, dlerror());
    *(void **)&core.set_environment = sym("retro_set_environment");
    *(void **)&core.set_video_refresh = sym("retro_set_video_refresh");
    *(void **)&core.set_audio_sample = sym("retro_set_audio_sample");
    *(void **)&core.set_audio_sample_batch = sym("retro_set_audio_sample_batch");
    *(void **)&core.set_input_poll = sym("retro_set_input_poll");
    *(void **)&core.set_input_state = sym("retro_set_input_state");
    *(void **)&core.init = sym("retro_init");
    *(void **)&core.deinit = sym("retro_deinit");
    *(void **)&core.api_version = sym("retro_api_version");
    *(void **)&core.get_system_info = sym("retro_get_system_info");
    *(void **)&core.get_system_av_info = sym("retro_get_system_av_info");
    *(void **)&core.set_controller_port_device = sym("retro_set_controller_port_device");
    *(void **)&core.run = sym("retro_run");
    *(void **)&core.load_game = sym("retro_load_game");
    *(void **)&core.unload_game = sym("retro_unload_game");
    *(void **)&core.get_memory_data = sym("retro_get_memory_data");
    *(void **)&core.get_memory_size = sym("retro_get_memory_size");
    *(void **)&core.serialize_size = sym("retro_serialize_size");
    *(void **)&core.serialize = sym("retro_serialize");
    if (core.api_version() != RETRO_API_VERSION)
        die(EXIT_USAGE, "core API version %u, host built for %u", core.api_version(), RETRO_API_VERSION);
    core.set_environment(environment);
    core.set_video_refresh(video_refresh);
    core.set_audio_sample(audio_sample);
    core.set_audio_sample_batch(audio_sample_batch);
    core.set_input_poll(input_poll);
    core.set_input_state(input_state);
    core.init();
    check_options();
}

/* ---- files and JSON ---------------------------------------------------- */

static uint8_t *read_file(const char *path, size_t *len) {
    FILE *f = fopen(path, "rb");
    if (!f) die(EXIT_USAGE, "open %s: %s", path, strerror(errno));
    if (fseek(f, 0, SEEK_END) != 0) die(EXIT_USAGE, "seek %s", path);
    long n = ftell(f);
    if (n < 0) die(EXIT_USAGE, "tell %s", path);
    rewind(f);
    uint8_t *buf = malloc(n ? (size_t)n : 1);
    if (!buf) die(EXIT_USAGE, "out of memory reading %s", path);
    if (fread(buf, 1, (size_t)n, f) != (size_t)n) die(EXIT_USAGE, "read %s", path);
    fclose(f);
    *len = (size_t)n;
    return buf;
}

static void json_str(FILE *f, const char *s) {
    fputc('"', f);
    for (; s && *s; s++) {
        unsigned char c = (unsigned char)*s;
        if (c == '"' || c == '\\')
            fprintf(f, "\\%c", c);
        else if (c < 0x20)
            fprintf(f, "\\u%04x", c);
        else
            fputc(c, f);
    }
    fputc('"', f);
}

static int cmp_decl(const void *a, const void *b) {
    return strcmp(((const struct decl *)a)->key, ((const struct decl *)b)->key);
}

struct bios_info {
    char path[4096];
    char sha1[41];
    size_t size;
    bool present;
};

/* The BIOS Beetle loads with override_bios=openbios: openbios.bin in the
 * system directory. */
static struct bios_info bios_info(void) {
    struct bios_info b = {0};
    snprintf(b.path, sizeof b.path, "%s/openbios.bin", system_dir);
    struct stat st;
    if (stat(b.path, &st) != 0) return b;
    uint8_t *data = read_file(b.path, &b.size);
    sha1_hex(data, b.size, b.sha1);
    free(data);
    b.present = true;
    return b;
}

/* Core, BIOS and options; `extra` writes more members (may be NULL). */
static void write_facts(FILE *f, void (*extra)(FILE *)) {
    struct retro_system_info si = {0};
    core.get_system_info(&si);
    struct bios_info b = bios_info();
    qsort(decls, (size_t)ndecls, sizeof decls[0], cmp_decl);
    fprintf(f, "{\n  \"host_version\": %d,\n  \"wire_version\": %u,\n", HOST_VERSION, PSXW_WIRE_VERSION);
    fprintf(f, "  \"core\": {\"library_name\": ");
    json_str(f, si.library_name);
    fprintf(f, ", \"library_version\": ");
    json_str(f, si.library_version);
    fprintf(f, ", \"path\": ");
    json_str(f, core_path);
    fprintf(f, "},\n  \"bios\": {\"path\": ");
    json_str(f, b.path);
    fprintf(f, ", \"present\": %s, \"size\": %zu, \"sha1\": ", b.present ? "true" : "false", b.size);
    json_str(f, b.present ? b.sha1 : "");
    fprintf(f, "},\n  \"core_options\": {");
    for (int i = 0; i < ndecls; i++) {
        fprintf(f, "%s\n    ", i ? "," : "");
        json_str(f, decls[i].key);
        fprintf(f, ": ");
        json_str(f, option_value(decls[i].key));
    }
    fprintf(f, "\n  },\n  \"fixed_options\": {");
    int n = 0;
    for (size_t i = 0; i < N_FIXED; i++) {
        if (!find_decl(FIXED_OPTIONS[i].key)) continue;
        fprintf(f, "%s\n    ", n++ ? "," : "");
        json_str(f, FIXED_OPTIONS[i].key);
        fprintf(f, ": ");
        json_str(f, FIXED_OPTIONS[i].value);
    }
    fprintf(f, "\n  },\n  \"cli_options\": {");
    for (int i = 0; i < ncli; i++) {
        fprintf(f, "%s\n    ", i ? "," : "");
        json_str(f, cli_keys[i]);
        fprintf(f, ": ");
        json_str(f, cli_vals[i]);
    }
    fprintf(f, "\n  }");
    if (extra) extra(f);
    fprintf(f, "\n}\n");
}

/* ---- the mailbox ------------------------------------------------------- */

static uint32_t mbx;

static uint32_t rd32(uint32_t off) {
    const uint8_t *p = ram + mbx + off;
    return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}

static void wr32(uint32_t off, uint32_t v) {
    uint8_t *p = ram + mbx + off;
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
    p[2] = (uint8_t)(v >> 16);
    p[3] = (uint8_t)(v >> 24);
}

static uint32_t le32(const uint8_t *p) {
    return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}

static void put32(FILE *f, uint32_t v) {
    uint8_t b[4] = {(uint8_t)v, (uint8_t)(v >> 8), (uint8_t)(v >> 16), (uint8_t)(v >> 24)};
    if (fwrite(b, 1, 4, f) != 4) die(EXIT_USAGE, "write failed");
}

static void run_frame(void) {
    core.run();
    frames_run++;
}

/* Statistics for the provenance's "run" member. */
static uint64_t stat_boot_frames, stat_exchanges, stat_exchange_frames, stat_max_exchange_frames, stat_diagnostics;
static uint32_t stat_runner_version, stat_capacity;
static const char *stat_content = "";

static void write_run_stats(FILE *f) {
    fprintf(f, ",\n  \"run\": {\"mode\": \"exe\", \"content\": \"%s\", \"mailbox\": %u, \"runner_version\": %u, \"capacity\": %u, ",
            stat_content, mbx | 0x80000000u, stat_runner_version, stat_capacity);
    fprintf(f, "\"boot_frames\": %llu, \"exchanges\": %llu, \"exchange_frames\": %llu, \"max_exchange_frames\": %llu, ",
            (unsigned long long)stat_boot_frames, (unsigned long long)stat_exchanges,
            (unsigned long long)stat_exchange_frames, (unsigned long long)stat_max_exchange_frames);
    fprintf(f, "\"frames\": %llu, \"frames_presented\": %llu, \"timed_out_waits\": %llu}", (unsigned long long)frames_run,
            (unsigned long long)frames_presented, (unsigned long long)stat_diagnostics);
}

static void dump_ram(const char *path) {
    if (!path || !ram) return;
    FILE *f = fopen(path, "wb");
    if (!f) return;
    fwrite(ram, 1, ram_size, f);
    fclose(f);
    fprintf(stderr, "psxw-emu-host: RAM dumped to %s\n", path);
}

struct exe_args {
    const char *exe, *disc, *vectors, *out, *provenance, *dump_ram;
    uint32_t mailbox;
    uint64_t boot_frames, timeout_frames;
};

static int mode_exe(const struct exe_args *a) {
    if ((!a->exe == !a->disc) || !a->vectors || !a->out)
        die(EXIT_USAGE, "exe needs one of --disc and --exe, plus --vectors and --out");
    const char *content = a->disc ? a->disc : a->exe;
    stat_content = a->disc ? "disc" : "exe";
    size_t in_len;
    uint8_t *in = read_file(a->vectors, &in_len);
    if (in_len < 16 || memcmp(in, PSXW_XCHG_IN_MAGIC, 8) != 0) die(EXIT_USAGE, "%s: not an exchange file", a->vectors);
    if (le32(in + 8) != PSXW_XCHG_VERSION) die(EXIT_USAGE, "%s: exchange version %u", a->vectors, le32(in + 8));
    uint32_t count = le32(in + 12);

    struct retro_game_info game = {.path = content, .data = NULL, .size = 0, .meta = NULL};
    if (!core.load_game(&game)) die(EXIT_USAGE, "the core refused %s", content);
    core.set_controller_port_device(0, RETRO_DEVICE_JOYPAD);
    core.set_controller_port_device(1, RETRO_DEVICE_JOYPAD);
    ram = core.get_memory_data(RETRO_MEMORY_SYSTEM_RAM);
    ram_size = core.get_memory_size(RETRO_MEMORY_SYSTEM_RAM);
    if (!ram || ram_size < 0x200000) die(EXIT_PROTOCOL, "core exposes %zu bytes of system RAM", ram_size);
    mbx = a->mailbox & 0x1FFFFF;
    if (mbx + PSXW_MBX_SIZE > ram_size) die(EXIT_USAGE, "mailbox outside RAM");

    /* Boot: OpenBIOS starts, the runner is loaded, it marks the mailbox. */
    for (;;) {
        if (rd32(PSXW_MBX_OFF_MAGIC) == PSXW_MBX_MAGIC && rd32(PSXW_MBX_OFF_STATE) == PSXW_MBX_STATE_READY) break;
        if (frames_run >= a->boot_frames) {
            dump_ram(a->dump_ram);
            die(EXIT_BOOT, "runner not ready after %llu frames (mailbox magic %08x state %u)",
                (unsigned long long)frames_run, rd32(PSXW_MBX_OFF_MAGIC), rd32(PSXW_MBX_OFF_STATE));
        }
        run_frame();
    }
    stat_boot_frames = frames_run;
    stat_runner_version = rd32(PSXW_MBX_OFF_RUNNER_VERSION);
    stat_capacity = rd32(PSXW_MBX_OFF_CAPACITY);
    if (rd32(PSXW_MBX_OFF_VERSION) != PSXW_WIRE_VERSION)
        die(EXIT_PROTOCOL, "runner speaks wire version %u, host %u", rd32(PSXW_MBX_OFF_VERSION), PSXW_WIRE_VERSION);
    if (stat_capacity > PSXW_MBX_CAPACITY) die(EXIT_PROTOCOL, "runner capacity %u too large", stat_capacity);
    if (log_file) fprintf(log_file, "[host] runner %u ready after %llu frames\n", stat_runner_version,
                          (unsigned long long)frames_run);

    char tmp[4096];
    snprintf(tmp, sizeof tmp, "%s.partial", a->out);
    FILE *out = fopen(tmp, "wb");
    if (!out) die(EXIT_USAGE, "create %s: %s", tmp, strerror(errno));
    fwrite(PSXW_XCHG_OUT_MAGIC, 1, 8, out);
    put32(out, PSXW_XCHG_VERSION);
    put32(out, count);

    size_t pos = 16;
    uint32_t seq = rd32(PSXW_MBX_OFF_ACK_SEQ);
    for (uint32_t i = 0; i < count; i++) {
        if (pos + 8 > in_len) die(EXIT_USAGE, "%s: truncated at exchange %u", a->vectors, i);
        uint32_t kind = le32(in + pos), len = le32(in + pos + 4);
        pos += 8;
        if (len > stat_capacity || pos + len > in_len) die(EXIT_USAGE, "exchange %u: payload %u bytes", i, len);
        memcpy(ram + mbx + PSXW_MBX_OFF_DATA, in + pos, len);
        pos += (len + 3) & ~3u;
        wr32(PSXW_MBX_OFF_KIND, kind);
        wr32(PSXW_MBX_OFF_PAYLOAD_LEN, len);
        wr32(PSXW_MBX_OFF_RESULT_LEN, 0);
        wr32(PSXW_MBX_OFF_ERROR, 0);
        wr32(PSXW_MBX_OFF_DIAG_WHAT, 0);
        wr32(PSXW_MBX_OFF_DIAG_VALUE, 0);
        wr32(PSXW_MBX_OFF_SEQ, ++seq);
        wr32(PSXW_MBX_OFF_STATE, PSXW_MBX_STATE_PENDING);
        uint64_t start = frames_run;
        uint32_t state;
        for (;;) {
            run_frame();
            state = rd32(PSXW_MBX_OFF_STATE);
            if ((state == PSXW_MBX_STATE_DONE || state == PSXW_MBX_STATE_ERROR) && rd32(PSXW_MBX_OFF_ACK_SEQ) == seq) break;
            if (frames_run - start >= a->timeout_frames) {
                fclose(out);
                dump_ram(a->dump_ram);
                die(EXIT_EXCHANGE, "exchange %u: no answer after %llu frames (state %u)", i,
                    (unsigned long long)(frames_run - start), state);
            }
        }
        if (rd32(PSXW_MBX_OFF_DIAG_WHAT)) {
            stat_diagnostics++;
            if (log_file)
                fprintf(log_file, "[host] exchange %u: runner wait %u timed out, last read %08x\n", i,
                        rd32(PSXW_MBX_OFF_DIAG_WHAT), rd32(PSXW_MBX_OFF_DIAG_VALUE));
            if (verbose)
                fprintf(stderr, "psxw-emu-host: exchange %u: runner wait %u timed out, last read %08x\n", i,
                        rd32(PSXW_MBX_OFF_DIAG_WHAT), rd32(PSXW_MBX_OFF_DIAG_VALUE));
        }
        uint64_t took = frames_run - start;
        stat_exchange_frames += took;
        if (took > stat_max_exchange_frames) stat_max_exchange_frames = took;
        stat_exchanges++;
        uint32_t rlen = state == PSXW_MBX_STATE_DONE ? rd32(PSXW_MBX_OFF_RESULT_LEN) : 0;
        if (rlen > stat_capacity) die(EXIT_PROTOCOL, "exchange %u: result length %u", i, rlen);
        put32(out, state);
        put32(out, rd32(PSXW_MBX_OFF_ERROR));
        put32(out, rlen);
        if (fwrite(ram + mbx + PSXW_MBX_OFF_DATA, 1, rlen, out) != rlen) die(EXIT_USAGE, "write failed");
        static const uint8_t zero[3];
        fwrite(zero, 1, ((rlen + 3) & ~3u) - rlen, out);
    }
    if (fclose(out) != 0 || rename(tmp, a->out) != 0) die(EXIT_USAGE, "finishing %s: %s", a->out, strerror(errno));
    free(in);

    if (a->provenance) {
        FILE *f = fopen(a->provenance, "w");
        if (!f) die(EXIT_USAGE, "create %s: %s", a->provenance, strerror(errno));
        write_facts(f, write_run_stats);
        fclose(f);
    }
    core.unload_game();
    return 0;
}

/* ---- play mode --------------------------------------------------------- */

#define MAX_DUMPS 256

struct play_args {
    const char *disc, *replay, *out_dir, *provenance;
    uint64_t frames;
    bool trace;
    uint32_t trace_addr;
    uint64_t dump_at[MAX_DUMPS];
    int ndump_at;
    uint32_t dump_counter[MAX_DUMPS];
    int ndump_counter;
    uint64_t frames_every;
    bool dump_frames, dump_vram;
};

struct dump_rec {
    char file[64];
    uint64_t frame;
    uint32_t counter;
    bool by_counter;
    char sha1[41];
    /* --dump-frames, --dump-vram: the files beside it ("" for none). */
    char picture[64];
    char vram[64];
};
static struct dump_rec dumps[2 * MAX_DUMPS];
static int ndumps;
static char replay_sha1[41];
static uint32_t play_pad_type;

static void parse_list64(const char *s, uint64_t *out, int *n) {
    char *copy = xstrdup(s);
    for (char *tok = strtok(copy, ","); tok; tok = strtok(NULL, ",")) {
        if (*n == MAX_DUMPS) die(EXIT_USAGE, "too many values in %s", s);
        char *end;
        errno = 0;
        unsigned long long v = strtoull(tok, &end, 0);
        if (errno || *end) die(EXIT_USAGE, "bad number %s", tok);
        out[(*n)++] = v;
    }
    free(copy);
}

static void write_dump(const char *dir, const char *name, uint64_t frame, uint32_t counter, bool by_counter) {
    if (ndumps == 2 * MAX_DUMPS) die(EXIT_USAGE, "too many dumps");
    struct dump_rec *d = &dumps[ndumps++];
    snprintf(d->file, sizeof d->file, "%s", name);
    d->frame = frame;
    d->counter = counter;
    d->by_counter = by_counter;
    sha1_hex(ram, ram_size, d->sha1);
    char path[4096];
    snprintf(path, sizeof path, "%s/%s", dir, name);
    FILE *f = fopen(path, "wb");
    if (!f) die(EXIT_USAGE, "create %s: %s", path, strerror(errno));
    if (fwrite(ram, 1, ram_size, f) != ram_size || fclose(f) != 0) die(EXIT_USAGE, "write %s", path);
}

/*
 * VRAM from the core's savestate: Mednafen's format is a 32-byte header
 * ("MDFNSVST", the total size at offset 20), then sections (a 32-byte
 * name, a u32 LE size) of entries (a u8 name length, the name, a u32 LE
 * size, the bytes). Beetle's "GPU" section keeps VRAM as "&GPURAM[0][0]",
 * 1024x512 16-bit words. NULL, with the reason in vram_unavailable, when
 * the state has no such entry.
 */
#define VRAM_SECTION "GPU"
#define VRAM_ENTRY "&GPURAM[0][0]"
#define VRAM_BYTES (1024 * 512 * 2)
static const char *vram_unavailable;
static uint8_t *state_buf;
static size_t state_cap;

static const uint8_t *state_vram(void) {
    size_t n = core.serialize_size();
    if (n < 32) {
        vram_unavailable = "retro_serialize_size gave no room for a state";
        return NULL;
    }
    if (n > state_cap) {
        free(state_buf);
        state_buf = malloc(n);
        if (!state_buf) die(EXIT_USAGE, "out of memory for a %zu-byte state", n);
        state_cap = n;
    }
    if (!core.serialize(state_buf, n)) {
        vram_unavailable = "retro_serialize failed";
        return NULL;
    }
    if (memcmp(state_buf, "MDFNSVST", 8) != 0) {
        vram_unavailable = "the core's savestate is not in Mednafen's format";
        return NULL;
    }
    size_t total = le32(state_buf + 20);
    if (total > n) total = n;
    for (size_t pos = 32; pos + 36 <= total;) {
        size_t start = pos + 36, end = start + le32(state_buf + pos + 32);
        if (end > total) break;
        if (!strncmp((const char *)state_buf + pos, VRAM_SECTION, 32)) {
            for (size_t p = start; p + 1 <= end;) {
                size_t nl = state_buf[p];
                if (p + 1 + nl + 4 > end) break;
                size_t data = p + 1 + nl + 4, size = le32(state_buf + p + 1 + nl);
                if (data + size > end) break;
                if (nl == strlen(VRAM_ENTRY) && !memcmp(state_buf + p + 1, VRAM_ENTRY, nl) && size == VRAM_BYTES)
                    return state_buf + data;
                p = data + size;
            }
        }
        pos = end;
    }
    vram_unavailable = "the core's savestate has no " VRAM_SECTION " section entry " VRAM_ENTRY " of 1 MiB";
    return NULL;
}

/* --dump-frames and --dump-vram beside the RAM dump just written. */
static void write_dump_extras(const struct play_args *a, const char *dir, const char *suffix) {
    struct dump_rec *d = &dumps[ndumps - 1];
    char path[4096];
    if (a->dump_frames && kept) {
        snprintf(d->picture, sizeof d->picture, "frame_%s.ppm", suffix);
        snprintf(path, sizeof path, "%s/%s", dir, d->picture);
        write_ppm_file(path, kept, kept_w, kept_h, kept_w * bytes_per_pixel(kept_fmt), kept_fmt);
    }
    const uint8_t *vram = a->dump_vram ? state_vram() : NULL;
    if (vram) {
        snprintf(d->vram, sizeof d->vram, "vram_%s.bin", suffix);
        snprintf(path, sizeof path, "%s/%s", dir, d->vram);
        FILE *f = fopen(path, "wb");
        if (!f) die(EXIT_USAGE, "create %s: %s", path, strerror(errno));
        if (fwrite(vram, 1, VRAM_BYTES, f) != VRAM_BYTES || fclose(f) != 0) die(EXIT_USAGE, "write %s", path);
    }
}

static void write_play_stats(FILE *f) {
    fprintf(f, ",\n  \"run\": {\"mode\": \"play\", \"replay_sha1\": \"%s\", \"pad_type\": %u, \"replay_frames\": %u, ",
            replay_sha1, play_pad_type, replay_frames);
    fprintf(f, "\"frames\": %llu, \"frames_presented\": %llu, \"dumps\": [", (unsigned long long)frames_run,
            (unsigned long long)frames_presented);
    for (int i = 0; i < ndumps; i++) {
        fprintf(f, "%s\n    {\"file\": \"%s\", \"frame\": %llu, ", i ? "," : "", dumps[i].file,
                (unsigned long long)dumps[i].frame);
        if (dumps[i].by_counter) fprintf(f, "\"counter\": %u, ", dumps[i].counter);
        if (dumps[i].picture[0]) fprintf(f, "\"picture\": \"%s\", ", dumps[i].picture);
        if (dumps[i].vram[0]) fprintf(f, "\"vram\": \"%s\", ", dumps[i].vram);
        fprintf(f, "\"sha1\": \"%s\"}", dumps[i].sha1);
    }
    fprintf(f, "%s]", ndumps ? "\n  " : "");
    if (vram_unavailable) {
        fprintf(f, ", \"vram_unavailable\": ");
        json_str(f, vram_unavailable);
    }
    fprintf(f, "}");
}

static int mode_play(const struct play_args *a) {
    if (!a->disc || !a->replay || !a->out_dir || !a->frames)
        die(EXIT_USAGE, "play needs --disc, --replay, --frames and --out");
    if (a->ndump_counter && !a->trace) die(EXIT_USAGE, "--dump-counter needs --trace-u32");
    char frames_path[4096];
    if (a->frames_every) {
        snprintf(frames_path, sizeof frames_path, "%s/frames", a->out_dir);
        mkdir(frames_path, 0777);
        frames_dir = frames_path;
        frames_every = a->frames_every;
    }
    keep_frames = a->dump_frames;
    uint8_t *r = read_file(a->replay, &replay_len);
    if (replay_len < REPLAY_HEADER_LEN_V1 || memcmp(r, "PSXWRPL", 8) != 0) die(EXIT_USAGE, "%s: not a replay", a->replay);
    switch (le32(r + 8)) {
    case 1: replay_data = REPLAY_HEADER_LEN_V1; replay_clock = 0; break;
    case 2:
        if (replay_len < REPLAY_HEADER_LEN) die(EXIT_USAGE, "%s: truncated", a->replay);
        replay_data = REPLAY_HEADER_LEN;
        replay_clock = le32(r + 56);
        replay_unplugged = le32(r + 60);
        break;
    default: die(EXIT_USAGE, "%s: replay version %u", a->replay, le32(r + 8));
    }
    play_pad_type = le32(r + 12);
    replay_frames = le32(r + 16);
    if (replay_len < replay_data + (size_t)replay_frames * REPLAY_FRAME_LEN) die(EXIT_USAGE, "%s: truncated", a->replay);
    if (play_pad_type != 0x41) die(EXIT_USAGE, "%s: pad type 0x%x (only the digital pad, 0x41, is mapped)", a->replay, play_pad_type);
    sha1_hex(r, replay_len, replay_sha1);
    replay = r;

    struct retro_game_info game = {.path = a->disc, .data = NULL, .size = 0, .meta = NULL};
    if (!core.load_game(&game)) die(EXIT_USAGE, "the core refused %s", a->disc);
    core.set_controller_port_device(0, (replay_unplugged & 1) ? RETRO_DEVICE_NONE : RETRO_DEVICE_JOYPAD);
    core.set_controller_port_device(1, (replay_unplugged & 2) ? RETRO_DEVICE_NONE : RETRO_DEVICE_JOYPAD);
    ram = core.get_memory_data(RETRO_MEMORY_SYSTEM_RAM);
    ram_size = core.get_memory_size(RETRO_MEMORY_SYSTEM_RAM);
    if (!ram || ram_size != 0x200000) die(EXIT_PROTOCOL, "core exposes %zu bytes of system RAM", ram_size);
    mbx = 0;

    char path[4096];
    snprintf(path, sizeof path, "%s/trace.bin", a->out_dir);
    FILE *trace = a->trace ? fopen(path, "wb") : NULL;
    if (a->trace && !trace) die(EXIT_USAGE, "create %s: %s", path, strerror(errno));
    uint32_t taddr = a->trace_addr & 0x1FFFFC;
    bool counter_done[MAX_DUMPS] = {0};
    while (frames_run < a->frames) {
        run_frame();
        uint32_t word = a->trace ? rd32(taddr) : 0;
        if (trace) put32(trace, word);
        for (int i = 0; i < a->ndump_at; i++) {
            if (a->dump_at[i] != frames_run) continue;
            char suffix[32], name[64];
            snprintf(suffix, sizeof suffix, "%06llu", (unsigned long long)frames_run);
            snprintf(name, sizeof name, "ram_%s.bin", suffix);
            write_dump(a->out_dir, name, frames_run, word, false);
            write_dump_extras(a, a->out_dir, suffix);
        }
        for (int i = 0; i < a->ndump_counter; i++) {
            if (counter_done[i] || a->dump_counter[i] != word) continue;
            counter_done[i] = true;
            char suffix[32], name[64];
            snprintf(suffix, sizeof suffix, "c%010u", word);
            snprintf(name, sizeof name, "ram_%s.bin", suffix);
            write_dump(a->out_dir, name, frames_run, word, true);
            write_dump_extras(a, a->out_dir, suffix);
        }
    }
    if (trace && fclose(trace) != 0) die(EXIT_USAGE, "write %s", path);

    snprintf(path, sizeof path, "%s/play.json", a->out_dir);
    FILE *f = fopen(path, "w");
    if (!f) die(EXIT_USAGE, "create %s: %s", path, strerror(errno));
    write_facts(f, write_play_stats);
    fclose(f);
    if (a->provenance) {
        f = fopen(a->provenance, "w");
        if (!f) die(EXIT_USAGE, "create %s: %s", a->provenance, strerror(errno));
        write_facts(f, write_play_stats);
        fclose(f);
    }
    core.unload_game();
    replay = NULL;
    free(r);
    return 0;
}

/* ---- main -------------------------------------------------------------- */

static void usage(void) {
    fprintf(stderr,
            "usage: psxw-emu-host info [common]\n"
            "       psxw-emu-host exe (--disc R.cue | --exe R.exe) --vectors IN --out OUT [--provenance P.json]\n"
            "                     [--mailbox ADDR] [--boot-frames N] [--timeout-frames N] [--dump-ram F] [common]\n"
            "       psxw-emu-host play --disc GAME.cue --replay R --frames N --out DIR [--trace-u32 ADDR]\n"
            "                     [--dump-at F,..] [--dump-counter V,..] [--frames-every N] [--dump-frames]\n"
            "                     [--dump-vram] [--provenance P.json] [common]\n"
            "common: --core LIB --system DIR --work DIR --log FILE --option KEY=VALUE --verbose\n");
    exit(EXIT_USAGE);
}

static uint64_t parse_num(const char *s) {
    char *end;
    errno = 0;
    unsigned long long v = strtoull(s, &end, 0);
    if (errno || *end) die(EXIT_USAGE, "bad number %s", s);
    return v;
}

int main(int argc, char **argv) {
    if (argc < 2) usage();
    const char *mode = argv[1];
    const char *log_path = NULL;
    struct exe_args ea = {.mailbox = PSXW_MBX_ADDR, .boot_frames = 3600, .timeout_frames = 600};
    static struct play_args pa;
    for (int i = 2; i < argc; i++) {
        const char *k = argv[i];
        const char *v = i + 1 < argc ? argv[i + 1] : NULL;
#define TAKE()                                  \
    do {                                        \
        if (!v) die(EXIT_USAGE, "%s needs a value", k); \
        i++;                                    \
    } while (0)
        if (!strcmp(k, "--verbose")) {
            verbose = true;
        } else if (!strcmp(k, "--core")) {
            TAKE();
            core_path = v;
        } else if (!strcmp(k, "--system")) {
            TAKE();
            system_dir = v;
        } else if (!strcmp(k, "--work")) {
            TAKE();
            work_dir = v;
        } else if (!strcmp(k, "--log")) {
            TAKE();
            log_path = v;
        } else if (!strcmp(k, "--option")) {
            TAKE();
            const char *eq = strchr(v, '=');
            if (!eq || ncli == MAX_CLI_OPTIONS) die(EXIT_USAGE, "--option KEY=VALUE");
            cli_keys[ncli] = strndup(v, (size_t)(eq - v));
            cli_vals[ncli++] = xstrdup(eq + 1);
        } else if (!strcmp(k, "--exe")) {
            TAKE();
            ea.exe = v;
        } else if (!strcmp(k, "--disc")) {
            TAKE();
            ea.disc = v;
            pa.disc = v;
        } else if (!strcmp(k, "--replay")) {
            TAKE();
            pa.replay = v;
        } else if (!strcmp(k, "--frames")) {
            TAKE();
            pa.frames = parse_num(v);
        } else if (!strcmp(k, "--trace-u32")) {
            TAKE();
            pa.trace = true;
            pa.trace_addr = (uint32_t)parse_num(v);
        } else if (!strcmp(k, "--dump-at")) {
            TAKE();
            parse_list64(v, pa.dump_at, &pa.ndump_at);
        } else if (!strcmp(k, "--dump-counter")) {
            TAKE();
            uint64_t vals[MAX_DUMPS];
            int n = 0;
            parse_list64(v, vals, &n);
            for (int j = 0; j < n; j++) {
                if (pa.ndump_counter == MAX_DUMPS) die(EXIT_USAGE, "too many --dump-counter values");
                pa.dump_counter[pa.ndump_counter++] = (uint32_t)vals[j];
            }
        } else if (!strcmp(k, "--frames-every")) {
            TAKE();
            pa.frames_every = parse_num(v);
        } else if (!strcmp(k, "--dump-frames")) {
            pa.dump_frames = true;
        } else if (!strcmp(k, "--dump-vram")) {
            pa.dump_vram = true;
        } else if (!strcmp(k, "--vectors")) {
            TAKE();
            ea.vectors = v;
        } else if (!strcmp(k, "--out")) {
            TAKE();
            ea.out = v;
            pa.out_dir = v;
        } else if (!strcmp(k, "--provenance")) {
            TAKE();
            ea.provenance = v;
            pa.provenance = v;
        } else if (!strcmp(k, "--dump-ram")) {
            TAKE();
            ea.dump_ram = v;
        } else if (!strcmp(k, "--mailbox")) {
            TAKE();
            ea.mailbox = (uint32_t)parse_num(v);
        } else if (!strcmp(k, "--boot-frames")) {
            TAKE();
            ea.boot_frames = parse_num(v);
        } else if (!strcmp(k, "--timeout-frames")) {
            TAKE();
            ea.timeout_frames = parse_num(v);
        } else {
            usage();
        }
#undef TAKE
    }
    if (log_path) {
        log_file = fopen(log_path, "w");
        if (!log_file) die(EXIT_USAGE, "create %s: %s", log_path, strerror(errno));
    }
    snprintf(save_dir, sizeof save_dir, "%s/save", work_dir);
    mkdir(save_dir, 0777);

    if (strcmp(mode, "info") && strcmp(mode, "exe") && strcmp(mode, "play")) usage();

    core_open();
    int rc = 0;
    if (!strcmp(mode, "info"))
        write_facts(stdout, NULL);
    else if (!strcmp(mode, "play"))
        rc = mode_play(&pa);
    else
        rc = mode_exe(&ea);
    core.deinit();
    if (log_file) fclose(log_file);
    return rc;
}
