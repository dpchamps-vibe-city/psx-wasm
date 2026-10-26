/* psx-wasm: header for translated (lift-tier) C. Generated functions use
 * the o32 form `uint64_t f(a0, a1, a2, a3)`, returning v0 | v1 << 32.
 *
 * Guest addresses are cast to pointers as they are; the linker's masking
 * pass maps every load and store into the guest window. Accesses are
 * byte-aligned so they never trap. Everything declared extern here is
 * either lowered by the linker (sp, gp, indirect calls) or provided by the
 * runtime (BIOS, syscalls, coprocessors). */

typedef signed char int8_t;
typedef unsigned char uint8_t;
typedef short int16_t;
typedef unsigned short uint16_t;
typedef int int32_t;
typedef unsigned int uint32_t;
typedef long long int64_t;
typedef unsigned long long uint64_t;

typedef struct { uint8_t v; } __attribute__((packed, aligned(1))) psxw_u8;
typedef struct { uint16_t v; } __attribute__((packed, aligned(1))) psxw_u16;
typedef struct { uint32_t v; } __attribute__((packed, aligned(1))) psxw_u32;

#define PSXW_LB(a) (((psxw_u8 *)(uint32_t)(a))->v)
#define PSXW_LH(a) (((psxw_u16 *)(uint32_t)(a))->v)
#define PSXW_LW(a) (((psxw_u32 *)(uint32_t)(a))->v)
#define PSXW_SB(a, x) (((psxw_u8 *)(uint32_t)(a))->v = (uint8_t)(x))
#define PSXW_SH(a, x) (((psxw_u16 *)(uint32_t)(a))->v = (uint16_t)(x))
#define PSXW_SW(a, x) (((psxw_u32 *)(uint32_t)(a))->v = (uint32_t)(x))

/* Lowered by the linker to the shared guest-register globals. */
uint32_t psxw_sp_get(void);
void psxw_sp_set(uint32_t sp);
uint32_t psxw_gp_get(void);
void psxw_gp_set(uint32_t gp);
/* Lowered to `call_indirect` through the dispatch table. */
uint64_t psxw_call_indirect(uint32_t addr, uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3);

/* Runtime services. */
uint64_t psxw_bios(uint32_t table, uint32_t fn, uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3);
/* Code in BIOS kernel RAM; only reachable from functions adapters replace. */
uint64_t psxw_kernel_call(uint32_t addr, uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3);
uint32_t psxw_syscall(uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3);
void __psxw_break(int code);
__attribute__((noreturn)) void psxw_trap_overflow(uint32_t pc);
__attribute__((noreturn)) void psxw_bad_jump(uint32_t pc, uint32_t target);
uint32_t psxw_cop0_read(uint32_t r);
void psxw_cop0_write(uint32_t r, uint32_t v);
void psxw_rfe(void);
uint32_t psx_gte_mfc2(uint32_t r);
uint32_t psx_gte_cfc2(uint32_t r);
void psx_gte_mtc2(uint32_t r, uint32_t v);
void psx_gte_ctc2(uint32_t r, uint32_t v);
void psx_gte_exec(uint32_t cmd);

static inline uint64_t psxw_mult(uint32_t a, uint32_t b) {
    return (uint64_t)((int64_t)(int32_t)a * (int64_t)(int32_t)b);
}

/* lo | hi << 32, with the R3000's results for the cases C leaves undefined
 * (psx-spx "CPU Arithmetic Instructions"). */
static inline uint64_t psxw_div(uint32_t n, uint32_t d) {
    uint32_t q, r;
    if (d == 0) {
        q = (int32_t)n >= 0 ? 0xFFFFFFFFu : 1u;
        r = n;
    } else if (n == 0x80000000u && d == 0xFFFFFFFFu) {
        q = 0x80000000u;
        r = 0;
    } else {
        q = (uint32_t)((int32_t)n / (int32_t)d);
        r = (uint32_t)((int32_t)n % (int32_t)d);
    }
    return ((uint64_t)r << 32) | q;
}

static inline uint64_t psxw_divu(uint32_t n, uint32_t d) {
    if (d == 0)
        return ((uint64_t)n << 32) | 0xFFFFFFFFu;
    return ((uint64_t)(n % d) << 32) | (n / d);
}

static inline uint32_t psxw_add_trap(uint32_t a, uint32_t b, uint32_t pc) {
    uint32_t s = a + b;
    if ((~(a ^ b) & (a ^ s)) & 0x80000000u)
        psxw_trap_overflow(pc);
    return s;
}

static inline uint32_t psxw_sub_trap(uint32_t a, uint32_t b, uint32_t pc) {
    uint32_t s = a - b;
    if (((a ^ b) & (a ^ s)) & 0x80000000u)
        psxw_trap_overflow(pc);
    return s;
}

/* Unaligned word accesses, little-endian (psx-spx "CPU Load/Store"). */
static inline uint32_t psxw_lwl(uint32_t addr, uint32_t rt) {
    uint32_t sh = (addr & 3) * 8;
    uint32_t w = PSXW_LW(addr & ~3u);
    return (rt & (0x00FFFFFFu >> sh)) | (w << (24 - sh));
}

static inline uint32_t psxw_lwr(uint32_t addr, uint32_t rt) {
    uint32_t sh = (addr & 3) * 8;
    uint32_t w = PSXW_LW(addr & ~3u);
    return (rt & (uint32_t)(0xFFFFFF00ull << (24 - sh))) | (w >> sh);
}

static inline void psxw_swl(uint32_t addr, uint32_t rt) {
    uint32_t sh = (addr & 3) * 8;
    uint32_t a = addr & ~3u;
    uint32_t w = PSXW_LW(a);
    PSXW_SW(a, (w & (uint32_t)(0xFFFFFF00ull << sh)) | (rt >> (24 - sh)));
}

static inline void psxw_swr(uint32_t addr, uint32_t rt) {
    uint32_t sh = (addr & 3) * 8;
    uint32_t a = addr & ~3u;
    uint32_t w = PSXW_LW(a);
    PSXW_SW(a, (w & (0x00FFFFFFu >> (24 - sh))) | (rt << sh));
}
