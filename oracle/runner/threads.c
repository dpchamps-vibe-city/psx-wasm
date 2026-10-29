/*
 * BIOS_THREADS: OpenThread, ChangeThread and CloseThread (B0h:0Eh-10h) on
 * the target's own BIOS (psx-spx "BIOS Thread Functions"), seen through
 * the kernel's table of tables: 108h/10Ch the PCB and its size, 110h/114h
 * the TCBs and their size. A TCB is 0C0h bytes: the status (1000h free,
 * 4000h used), then the registers r0-r31 from +08h, epc +88h, hi, lo, sr
 * +94h. Each BIOS lays out its kernel memory where it likes, so results
 * name TCBs by index and sp/gp by their difference from what the caller
 * had, never by address.
 *
 * Every thread runs psxw_thread_body (entered through the copy of
 * psxw_thread_entry at the op's pc): a round notes what it sees and
 * ChangeThreads back. Rounds share the `th` record; each thread keeps
 * what its last ChangeThread returned, and the sp and gp it made it with,
 * on its own stack (`th.call` points at the running thread's).
 *
 * TS_CHANGE result words:
 *   0  1 if done (0: the slot names no open thread, nothing ran)
 *   1  ChangeThread's return in the switching thread
 *   2  rounds so far, every thread together
 *   3  sp a thread last started with (as the BIOS entered it)
 *   4  gp it started with
 *   5  the PCB's current TCB as the running thread saw it
 *   6  what the thread's own ChangeThread returned before this round (0
 *      on a fresh start)
 *   7  its sp after that return minus its sp at the call (0 on a fresh
 *      start)
 *   8  the thread's TCB's saved sp minus its sp at its ChangeThread
 *   9  that TCB's saved gp minus its gp at the call
 *   10 the switching thread's TCB's saved sp minus its sp at the call
 *   11 that TCB's saved gp minus its gp at the call
 *   12 the PCB's current TCB after the switch back
 */

#include "runner.h"

/* Not memset: main.c's memset is left out of the image while nothing
 * calls it, and keeping it would move the code after it. */
static void zero(uint8_t *p, uint32_t n) {
    for (uint32_t i = 0; i < n; i++) ((volatile uint8_t *)p)[i] = 0;
}

struct psxw_call {
    uint32_t sp_before, sp_after, gp;
};

uint32_t psxw_change_thread(uint32_t handle, struct psxw_call *c);
extern const uint32_t psxw_thread_entry[];
void psxw_thread_body(uint32_t sp, uint32_t gp) __attribute__((noreturn));

#define KWORD(a) (*(volatile uint32_t *)(0x80000000u | (a)))
#define TCB_WORDS 0x30u
#define TCB_BYTES 0xC0u
#define TCB_FIELD_SR 0x25u /* +94h */
#define REG(r) (2u + (r))

static struct {
    uint32_t back, runs, entry_sp, entry_gp, pcb_index, resume_v0, resume_sp_delta;
    struct psxw_call *call;
} volatile th;

static int in_ram(uint32_t a) {
    a &= 0x1FFFFFFFu;
    return a < 0x00200000u && !(a & 3u);
}

static uint32_t tcb_count(void) { return KWORD(0x114) / TCB_BYTES; }

static int tables_ok(void) { return in_ram(KWORD(0x108)) && in_ram(KWORD(0x110)) && tcb_count() <= 32; }

static volatile uint32_t *tcb(uint32_t i) {
    return (volatile uint32_t *)(0x80000000u | ((KWORD(0x110) & 0x1FFFFFFFu) + i * TCB_BYTES));
}

static uint32_t current_index(void) {
    if (!tables_ok()) return 0xFFFFFFFFu;
    uint32_t cur = KWORD(KWORD(0x108) & 0x1FFFFFFFu) & 0x1FFFFFFFu;
    uint32_t base = KWORD(0x110) & 0x1FFFFFFFu;
    if (cur < base || (cur - base) % TCB_BYTES || (cur - base) / TCB_BYTES >= tcb_count()) return 0xFFFFFFFFu;
    return (cur - base) / TCB_BYTES;
}

/* The TCB a handle names, if it is in the table. */
static int handle_index(uint32_t h, uint32_t *i) {
    *i = h & 0xFFFFu;
    return (h & 0xFFFF0000u) == 0xFF000000u && *i < tcb_count();
}

void psxw_thread_body(uint32_t sp, uint32_t gp) {
    struct psxw_call call = {0, 0, 0};
    uint32_t v0 = 0, delta = 0;
    th.entry_sp = sp;
    th.entry_gp = gp;
    for (;;) {
        th.runs++;
        th.resume_v0 = v0;
        th.resume_sp_delta = delta;
        th.pcb_index = current_index();
        th.call = &call;
        v0 = psxw_change_thread(th.back, &call);
        delta = call.sp_after - call.sp_before;
    }
}

static uint32_t step_words(uint32_t op) {
    switch (op) {
    case PSXW_TS_TABLES: return PSXW_TS_TABLES_WORDS;
    case PSXW_TS_FILL: return PSXW_TS_FILL_WORDS;
    case PSXW_TS_OPEN: return PSXW_TS_OPEN_WORDS;
    case PSXW_TS_CHANGE: return PSXW_TS_CHANGE_WORDS;
    case PSXW_TS_CLOSE: return PSXW_TS_CLOSE_WORDS;
    default: return 0xFFFFFFFFu;
    }
}

static void change(uint32_t h, uint32_t *w) {
    uint32_t i, main = current_index();
    if (!handle_index(h, &i) || tcb(i)[0] != 0x4000u || main == 0xFFFFFFFFu || i == main) return;
    struct psxw_call mc;
    th.back = 0xFF000000u | main;
    uint32_t v0 = psxw_change_thread(h, &mc);
    w[0] = 1;
    w[1] = v0;
    w[2] = th.runs;
    w[3] = th.entry_sp;
    w[4] = th.entry_gp;
    w[5] = th.pcb_index;
    w[6] = th.resume_v0;
    w[7] = th.resume_sp_delta;
    if (th.call) {
        w[8] = tcb(i)[REG(29)] - th.call->sp_before;
        w[9] = tcb(i)[REG(28)] - th.call->gp;
    }
    w[10] = tcb(main)[REG(29)] - mc.sp_before;
    w[11] = tcb(main)[REG(28)] - mc.gp;
    w[12] = current_index();
}

uint32_t op_bios_threads(const uint8_t *p, uint32_t len, uint8_t *res, uint32_t room, uint32_t *rlen) {
    struct reader r = {p, len, 0};
    uint32_t pc;
    if (rd_u32(&r, &pc) || (len - 4) % PSXW_THREAD_STEP_SIZE) return PSXW_ERR_BAD_LEN;
    if (pc % 4 || pc < PSXW_SCRATCH_BASE || pc > PSXW_SCRATCH_BASE + PSXW_SCRATCH_SIZE - 4 * PSXW_THREAD_STUB_WORDS)
        return PSXW_ERR_RANGE;
    uint32_t n = (len - 4) / PSXW_THREAD_STEP_SIZE, total = 0;
    const uint32_t *steps = (const uint32_t *)(p + 4);
    for (uint32_t s = 0; s < n; s++) {
        uint32_t w = step_words(steps[4 * s]);
        if (w == 0xFFFFFFFFu || steps[4 * s + 1] >= 32) return PSXW_ERR_RANGE;
        total += 4 * w;
        if (total > room) return PSXW_ERR_RESULT_OVERFLOW;
    }
    zero(res, total);
    if (!tables_ok()) {
        /* Nothing to see through: every result stays zero. */
        *rlen = total;
        return PSXW_ERR_NONE;
    }

    /* Nothing carries over from an earlier vector. */
    th.back = th.runs = th.entry_sp = th.entry_gp = th.pcb_index = th.resume_v0 = th.resume_sp_delta = 0;
    th.call = 0;
    for (uint32_t k = 0; k < PSXW_THREAD_STUB_WORDS; k++) ((volatile uint32_t *)pc)[k] = psxw_thread_entry[k];
    psxw_bios_call(0, 0, 0, 0, 0xA0, 0x44); /* FlushCache */

    uint32_t slots[PSXW_THREAD_SLOTS];
    for (uint32_t k = 0; k < PSXW_THREAD_SLOTS; k++) slots[k] = 0xFFFFFFFFu;
    uint32_t *w = (uint32_t *)res;
    for (uint32_t s = 0; s < n; s++) {
        uint32_t op = steps[4 * s], a = steps[4 * s + 1], b = steps[4 * s + 2], c = steps[4 * s + 3], i;
        switch (op) {
        case PSXW_TS_TABLES:
            w[0] = KWORD(0x10C);
            w[1] = KWORD(0x114);
            w[2] = current_index();
            for (i = 0; i < PSXW_THREAD_MAX_TCBS && i < tcb_count(); i++) w[3 + i] = tcb(i)[0];
            break;
        case PSXW_TS_FILL:
            if (a < tcb_count() && tcb(a)[0] != 0x4000u) {
                for (i = 1; i < TCB_WORDS; i++) tcb(a)[i] = b | i << 8;
                tcb(a)[TCB_FIELD_SR] = c;
            }
            break;
        case PSXW_TS_OPEN:
            if (a >= PSXW_THREAD_SLOTS) break;
            slots[a] = w[0] = psxw_bios_call(pc, b, c, 0, 0xB0, 0x0E);
            if (handle_index(slots[a], &i))
                for (uint32_t k = 0; k < TCB_WORDS; k++) w[1 + k] = tcb(i)[k];
            break;
        case PSXW_TS_CHANGE:
            if (a < PSXW_THREAD_SLOTS) change(slots[a], w);
            break;
        case PSXW_TS_CLOSE:
            if (a < PSXW_THREAD_SLOTS && handle_index(slots[a], &i)) {
                w[0] = psxw_bios_call(slots[a], 0, 0, 0, 0xB0, 0x0F);
                w[1] = tcb(i)[0];
            }
            break;
        }
        w += step_words(op);
    }
    *rlen = total;
    return PSXW_ERR_NONE;
}
