/*
 * psxw runner: shared declarations. The runner is a bare-metal PS1 program
 * (built with PSn00bSDK's toolchain, but using none of its libraries) that
 * executes oracle operations directly on the chips. The wire format is in
 * psxw_oracle_wire.h, generated from crates/psxw-oracle/src/wire.rs.
 */

#ifndef PSXW_RUNNER_H
#define PSXW_RUNNER_H

#include <stdint.h>

#include "psxw_oracle_wire.h"

/* ---- hardware registers (KSEG1) ---------------------------------------- */

#define REG32(a) (*(volatile uint32_t *)(a))
#define REG16(a) (*(volatile uint16_t *)(a))

#define GP0 REG32(0xBF801810u)
#define GP1 REG32(0xBF801814u)
#define GPUREAD REG32(0xBF801810u)
#define GPUSTAT REG32(0xBF801814u)

#define DMA_MADR(n) REG32(0xBF801080u + 16u * (n))
#define DMA_BCR(n) REG32(0xBF801084u + 16u * (n))
#define DMA_CHCR(n) REG32(0xBF801088u + 16u * (n))
#define DMA_DPCR REG32(0xBF8010F0u)
#define DMA_DICR REG32(0xBF8010F4u)

#define IRQ_STAT REG32(0xBF801070u)
#define IRQ_MASK REG32(0xBF801074u)

#define TIMER_VALUE(n) REG32(0xBF801100u + 16u * (n))
#define TIMER_MODE(n) REG32(0xBF801104u + 16u * (n))

#define SPU_REG(off) REG16(0xBF801C00u + (off))

/* Physical address for DMA. */
#define PHYS(p) ((uint32_t)(uintptr_t)(p) & 0x00FFFFFFu)

/* Poll bound for device waits: far longer than any legitimate wait. */
#define SPIN_LIMIT 0x01000000u

/* Record a timed-out wait in the mailbox header (PSXW_DIAG_* code and the
 * register value last read); the host logs it. */
static inline void diag(uint32_t what, uint32_t value) {
    ((volatile uint32_t *)PSXW_MBX_ADDR)[PSXW_MBX_OFF_DIAG_WHAT / 4u] = what;
    ((volatile uint32_t *)PSXW_MBX_ADDR)[PSXW_MBX_OFF_DIAG_VALUE / 4u] = value;
}

static inline uint32_t cop0_sr(void) {
    uint32_t v;
    __asm__ volatile("mfc0 %0, $12\n\tnop" : "=r"(v));
    return v;
}

static inline void set_cop0_sr(uint32_t v) { __asm__ volatile("mtc0 %0, $12\n\tnop\n\tnop" ::"r"(v)); }

/* Interrupts off, GTE on: the state every op runs in. */
static inline void quiesce(void) {
    set_cop0_sr((cop0_sr() & ~0x0401u) | 0x40000000u);
    IRQ_MASK = 0;
    IRQ_STAT = 0;
}

/* ---- payload reading and result writing -------------------------------- */

struct reader {
    const uint8_t *p;
    uint32_t len, pos;
};

/* 0 on success; the payload is little-endian and 4-byte aligned. */
static inline int rd_u32(struct reader *r, uint32_t *v) {
    if (r->pos > r->len || r->len - r->pos < 4) return 1;
    const uint8_t *q = r->p + r->pos;
    *v = (uint32_t)q[0] | (uint32_t)q[1] << 8 | (uint32_t)q[2] << 16 | (uint32_t)q[3] << 24;
    r->pos += 4;
    return 0;
}

/* Take `n` bytes (padded to 4); NULL if the payload is too short. */
static inline const uint8_t *rd_bytes(struct reader *r, uint32_t n) {
    uint32_t padded = (n + 3u) & ~3u;
    if (padded < n || r->pos > r->len || r->len - r->pos < padded) return 0;
    const uint8_t *q = r->p + r->pos;
    r->pos += padded;
    return q;
}

static inline void put_u32(uint8_t *p, uint32_t v) {
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
    p[2] = (uint8_t)(v >> 16);
    p[3] = (uint8_t)(v >> 24);
}

void *memcpy(void *dst, const void *src, unsigned long n);
void *memset(void *dst, int c, unsigned long n);

/* One op: payload in, result out (`room` bytes available). Returns an
 * ERR_* code and sets *result_len. */
typedef uint32_t op_fn(const uint8_t *payload, uint32_t len, uint8_t *result, uint32_t room, uint32_t *result_len);

op_fn op_gpu_reset, op_vram_write, op_vram_read, op_gpu_exec;
op_fn op_gte_exec;
op_fn op_spu_reset, op_spu_ram_write, op_spu_run;
op_fn op_bios_call;

/* A0/B0/C0 call: jumps to `table` with t1 = func. */
uint32_t psxw_bios_call(uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3, uint32_t table, uint32_t func);

extern uint8_t _end[];

#endif
