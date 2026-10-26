/*
 * psxw runner: the mailbox loop and batch dispatch.
 *
 * Start-up (start.s) turns interrupts off, enables the GTE, moves the stack
 * above the mailbox and clears BSS. From then on nothing but this loop
 * runs: no BIOS event handlers, no VSync callbacks. The host writes a
 * payload into the mailbox between frames and sets PENDING; the loop runs
 * the batch, writes the results over the data area and sets DONE.
 */

#include "runner.h"

#define MBX(off) (((volatile uint32_t *)PSXW_MBX_ADDR)[(off) / 4u])
#define MBX_DATA ((uint8_t *)(PSXW_MBX_ADDR + PSXW_MBX_OFF_DATA))

/* The payload is copied here first, so results can overwrite the data
 * area while later ops still read their payloads. */
static uint8_t stage[PSXW_MBX_CAPACITY] __attribute__((aligned(16)));

void *memcpy(void *dst, const void *src, unsigned long n) {
    uint8_t *d = dst;
    const uint8_t *s = src;
    if ((((uintptr_t)d | (uintptr_t)s | n) & 3u) == 0) {
        uint32_t *dw = dst;
        const uint32_t *sw = src;
        for (unsigned long i = 0; i < n / 4; i++) dw[i] = sw[i];
        return dst;
    }
    for (unsigned long i = 0; i < n; i++) d[i] = s[i];
    return dst;
}

void *memset(void *dst, int c, unsigned long n) {
    uint8_t *d = dst;
    for (unsigned long i = 0; i < n; i++) d[i] = (uint8_t)c;
    return dst;
}

static uint32_t op_echo(const uint8_t *p, uint32_t len, uint8_t *res, uint32_t room, uint32_t *rlen) {
    if (len > room) return PSXW_ERR_RESULT_OVERFLOW;
    memcpy(res, p, len);
    *rlen = len;
    return PSXW_ERR_NONE;
}

static uint32_t op_info(const uint8_t *p, uint32_t len, uint8_t *res, uint32_t room, uint32_t *rlen) {
    (void)p;
    if (len != 0) return PSXW_ERR_BAD_LEN;
    if (room < PSXW_INFO_RESULT_SIZE) return PSXW_ERR_RESULT_OVERFLOW;
    put_u32(res + 0, PSXW_RUNNER_VERSION);
    put_u32(res + 4, PSXW_WIRE_VERSION);
    put_u32(res + 8, PSXW_MBX_ADDR);
    put_u32(res + 12, PSXW_MBX_CAPACITY);
    put_u32(res + 16, PSXW_SCRATCH_BASE);
    put_u32(res + 20, PSXW_SCRATCH_SIZE);
    put_u32(res + 24, (uint32_t)(uintptr_t)_end);
    put_u32(res + 28, cop0_sr());
    *rlen = PSXW_INFO_RESULT_SIZE;
    return PSXW_ERR_NONE;
}

static op_fn *lookup(uint32_t op) {
    switch (op) {
    case PSXW_OP_ECHO: return op_echo;
    case PSXW_OP_INFO: return op_info;
    case PSXW_OP_GPU_RESET: return op_gpu_reset;
    case PSXW_OP_VRAM_WRITE: return op_vram_write;
    case PSXW_OP_VRAM_READ: return op_vram_read;
    case PSXW_OP_GPU_EXEC: return op_gpu_exec;
    case PSXW_OP_GTE_EXEC: return op_gte_exec;
    case PSXW_OP_SPU_RESET: return op_spu_reset;
    case PSXW_OP_SPU_RAM_WRITE: return op_spu_ram_write;
    case PSXW_OP_SPU_RUN: return op_spu_run;
    case PSXW_OP_CD_XA: return op_cd_xa;
    case PSXW_OP_BIOS_CALL: return op_bios_call;
    default: return 0;
    }
}

/* Batch: u32 n_ops, then (u32 op, u32 len, bytes) records. Results go to
 * `out` as (u32 status, u32 len, bytes) records. Returns a mailbox error
 * for framing problems; op failures are reported per op. */
static uint32_t run_batch(const uint8_t *in, uint32_t len, uint8_t *out, uint32_t *out_len) {
    struct reader r = {in, len, 0};
    uint32_t n;
    if (rd_u32(&r, &n)) return PSXW_ERR_BAD_LEN;
    uint32_t opos = 0, failed = 0;
    for (uint32_t i = 0; i < n; i++) {
        uint32_t op, olen;
        if (rd_u32(&r, &op) || rd_u32(&r, &olen)) return PSXW_ERR_BAD_LEN;
        const uint8_t *p = rd_bytes(&r, olen);
        if (!p) return PSXW_ERR_BAD_LEN;
        if (PSXW_MBX_CAPACITY - opos < PSXW_BATCH_RES_HDR_SIZE) return PSXW_ERR_RESULT_OVERFLOW;
        uint8_t *rec = out + opos;
        uint32_t room = PSXW_MBX_CAPACITY - opos - PSXW_BATCH_RES_HDR_SIZE;
        uint32_t rl = 0, st;
        if (failed) {
            st = PSXW_ERR_SKIPPED;
        } else {
            op_fn *fn = lookup(op);
            st = fn ? fn(p, olen, rec + PSXW_BATCH_RES_HDR_SIZE, room, &rl) : PSXW_ERR_BAD_OP;
            quiesce(); /* BIOS calls may have turned interrupts back on */
        }
        if (st != PSXW_ERR_NONE) {
            failed = 1;
            rl = 0;
        }
        uint32_t padded = (rl + 3u) & ~3u;
        for (uint32_t k = rl; k < padded; k++) rec[PSXW_BATCH_RES_HDR_SIZE + k] = 0;
        put_u32(rec, st);
        put_u32(rec + 4, rl);
        opos += PSXW_BATCH_RES_HDR_SIZE + padded;
    }
    *out_len = opos;
    return PSXW_ERR_NONE;
}

void runner_main(void) {
    quiesce();
    DMA_DICR = 0;
    for (uint32_t off = 0; off < PSXW_MBX_HEADER_SIZE; off += 4) MBX(off) = 0;
    MBX(PSXW_MBX_OFF_VERSION) = PSXW_WIRE_VERSION;
    MBX(PSXW_MBX_OFF_RUNNER_VERSION) = PSXW_RUNNER_VERSION;
    MBX(PSXW_MBX_OFF_CAPACITY) = PSXW_MBX_CAPACITY;
    MBX(PSXW_MBX_OFF_MAGIC) = PSXW_MBX_MAGIC;
    MBX(PSXW_MBX_OFF_STATE) = PSXW_MBX_STATE_READY;

    uint32_t spin = 0;
    for (;;) {
        if (MBX(PSXW_MBX_OFF_STATE) != PSXW_MBX_STATE_PENDING) {
            if ((++spin & 0xFFFu) == 0) MBX(PSXW_MBX_OFF_HEARTBEAT) = MBX(PSXW_MBX_OFF_HEARTBEAT) + 1;
            continue;
        }
        uint32_t seq = MBX(PSXW_MBX_OFF_SEQ);
        uint32_t kind = MBX(PSXW_MBX_OFF_KIND);
        uint32_t len = MBX(PSXW_MBX_OFF_PAYLOAD_LEN);
        uint32_t err, rlen = 0;
        if (kind != PSXW_MBX_KIND_BATCH) {
            err = PSXW_ERR_BAD_KIND;
        } else if (len > PSXW_MBX_CAPACITY) {
            err = PSXW_ERR_BAD_LEN;
        } else {
            memcpy(stage, MBX_DATA, (len + 3u) & ~3u);
            err = run_batch(stage, len, MBX_DATA, &rlen);
        }
        MBX(PSXW_MBX_OFF_RESULT_LEN) = err ? 0 : rlen;
        MBX(PSXW_MBX_OFF_ERROR) = err;
        MBX(PSXW_MBX_OFF_ACK_SEQ) = seq;
        MBX(PSXW_MBX_OFF_STATE) = err ? PSXW_MBX_STATE_ERROR : PSXW_MBX_STATE_DONE;
    }
}
