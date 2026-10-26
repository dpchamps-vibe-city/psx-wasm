/*
 * SPU ops: a baseline reset, SPU RAM writes by DMA, and timed runs.
 *
 * SPU_RESET leaves:
 *   - SPUCNT = 8000h (on, muted, no reverb, no IRQ, no CD/external audio),
 *     transfer control = 0004h;
 *   - a 16-byte silent loop block at SPU RAM 1000h (flags 07h), which every
 *     voice plays once so ENDX ends as 00FFFFFFh whatever ran before;
 *   - every voice keyed off with envelope volume 0, pitch 1000h, start and
 *     repeat addresses at the silent block, ADSR words 0, volumes 0;
 *   - main, reverb, CD and external volumes 0; PMON, NON, EON 0; reverb
 *     registers 0; IRQ address 0; KON and KOFF written last as 0.
 * Vectors must not write SPU RAM 1000h..100Fh.
 *
 * Time is measured with root counter 2 on the system clock.
 */

#include "runner.h"

#define R_KON 0x188u
#define R_KOFF 0x18Cu
#define R_TRANSFER_ADDR 0x1A6u
#define R_DATA 0x1A8u
#define R_SPUCNT 0x1AAu
#define R_TRANSFER_CTRL 0x1ACu
#define R_SPUSTAT 0x1AEu
#define SILENT_BLOCK 0x1000u

static uint32_t t_acc;
static uint16_t t_last;

static void timer_start(void) {
    TIMER_MODE(2) = 0; /* system clock, free running; resets the count */
    t_acc = 0;
    t_last = 0;
}

/* Cycles since timer_start(); call at least every 65535 cycles. */
static uint32_t timer_now(void) {
    uint16_t v = (uint16_t)TIMER_VALUE(2);
    t_acc += (uint16_t)(v - t_last);
    t_last = v;
    return t_acc;
}

static void wait_samples(uint32_t samples) {
    timer_start();
    uint32_t end = samples * PSXW_SPU_CYCLES_PER_SAMPLE;
    while (timer_now() < end) {
    }
}

static uint32_t wait_spustat(uint16_t mask, uint16_t value) {
    for (uint32_t i = 0; i < SPIN_LIMIT; i++)
        if ((SPU_REG(R_SPUSTAT) & mask) == value) return PSXW_ERR_NONE;
    return PSXW_ERR_TIMEOUT;
}

/* SPUCNT bits 4-5: 0 stop, 1 manual write, 2 DMA write, 3 DMA read. */
static uint32_t set_transfer(uint16_t mode) {
    SPU_REG(R_SPUCNT) = (uint16_t)((SPU_REG(R_SPUCNT) & ~0x30u) | (mode << 4));
    return wait_spustat(0x30, (uint16_t)(mode << 4));
}

/* DMA4 between RAM and SPU RAM. `len` is a multiple of 16. */
static uint32_t spu_dma(uint32_t spu_addr, void *buf, uint32_t len, int to_spu) {
    uint32_t e;
    if ((e = set_transfer(0))) return e;
    SPU_REG(R_TRANSFER_CTRL) = 0x0004;
    SPU_REG(R_TRANSFER_ADDR) = (uint16_t)(spu_addr >> 3);
    if ((e = set_transfer(to_spu ? 2 : 3))) return e;
    DMA_DPCR |= 0x00080000u; /* channel 4 enable */
    DMA_MADR(4) = PHYS(buf);
    DMA_BCR(4) = 4u | (len / 16u) << 16;
    DMA_CHCR(4) = to_spu ? 0x01000201u : 0x01000200u;
    for (uint32_t i = 0;; i++) {
        if (!(DMA_CHCR(4) & (1u << 24))) break;
        if (i == SPIN_LIMIT) return PSXW_ERR_TIMEOUT;
    }
    if ((e = wait_spustat(0x0400, 0))) return e; /* transfer busy */
    return set_transfer(0);
}

static uint32_t silent_block[4] __attribute__((aligned(16))) = {0x00000700u, 0, 0, 0};

uint32_t op_spu_reset(const uint8_t *p, uint32_t len, uint8_t *res, uint32_t room, uint32_t *rlen) {
    (void)p;
    (void)res;
    (void)room;
    (void)rlen;
    if (len != 0) return PSXW_ERR_BAD_LEN;
    uint32_t e;
    SPU_REG(R_SPUCNT) = 0x8000;
    if ((e = wait_spustat(0x3F, 0))) return e;
    SPU_REG(R_TRANSFER_CTRL) = 0x0004;
    SPU_REG(R_KOFF) = 0xFFFF;
    SPU_REG(R_KOFF + 2) = 0x00FF;
    wait_samples(2);
    if ((e = spu_dma(SILENT_BLOCK, silent_block, sizeof silent_block, 1))) return e;
    for (uint32_t v = 0; v < 24; v++) {
        uint32_t b = 16u * v;
        SPU_REG(b + 0x0) = 0;                         /* volume L */
        SPU_REG(b + 0x2) = 0;                         /* volume R */
        SPU_REG(b + 0x4) = 0x1000;                    /* pitch */
        SPU_REG(b + 0x6) = (uint16_t)(SILENT_BLOCK >> 3); /* start */
        SPU_REG(b + 0x8) = 0;                         /* ADSR 1 */
        SPU_REG(b + 0xA) = 0;                         /* ADSR 2 */
        SPU_REG(b + 0xE) = (uint16_t)(SILENT_BLOCK >> 3); /* repeat */
    }
    for (uint32_t off = 0x180; off < 0x1A0; off += 2) SPU_REG(off) = 0; /* volumes, KON/KOFF, PMON, NON, EON */
    SPU_REG(0x1A2) = 0;                                                   /* reverb work area */
    SPU_REG(0x1A4) = 0;                                                   /* IRQ address */
    for (uint32_t off = 0x1B0; off < 0x1B8; off += 2) SPU_REG(off) = 0; /* CD, external volume */
    for (uint32_t off = 0x1C0; off < 0x200; off += 2) SPU_REG(off) = 0; /* reverb */
    /* Every voice plays the silent block to its end flag: ENDX = all. */
    SPU_REG(R_KON) = 0xFFFF;
    SPU_REG(R_KON + 2) = 0x00FF;
    wait_samples(64);
    SPU_REG(R_KOFF) = 0xFFFF;
    SPU_REG(R_KOFF + 2) = 0x00FF;
    wait_samples(64);
    for (uint32_t v = 0; v < 24; v++) SPU_REG(16u * v + 0xC) = 0; /* envelope volume */
    SPU_REG(R_KON) = 0;
    SPU_REG(R_KON + 2) = 0;
    SPU_REG(R_KOFF) = 0;
    SPU_REG(R_KOFF + 2) = 0;
    return PSXW_ERR_NONE;
}

uint32_t op_spu_ram_write(const uint8_t *p, uint32_t len, uint8_t *res, uint32_t room, uint32_t *rlen) {
    (void)res;
    (void)room;
    (void)rlen;
    struct reader r = {p, len, 0};
    uint32_t addr, n;
    if (rd_u32(&r, &addr) || rd_u32(&r, &n)) return PSXW_ERR_BAD_LEN;
    const uint8_t *data = rd_bytes(&r, n);
    if (!data || r.pos != len) return PSXW_ERR_BAD_LEN;
    if (addr % PSXW_SPU_ADDR_ALIGN || n % PSXW_SPU_LEN_ALIGN || n == 0 || addr > PSXW_SPU_RAM_SIZE ||
        n > PSXW_SPU_RAM_SIZE - addr)
        return PSXW_ERR_RANGE;
    return spu_dma(addr, (void *)data, n, 1);
}

uint32_t op_spu_run(const uint8_t *p, uint32_t len, uint8_t *res, uint32_t room, uint32_t *rlen) {
    struct reader r = {p, len, 0};
    uint32_t samples, n_writes, n_caps;
    if (rd_u32(&r, &samples) || rd_u32(&r, &n_writes) || n_writes > len / 12) return PSXW_ERR_BAD_LEN;
    if (samples > 44100u * 60u) return PSXW_ERR_RANGE;
    const uint32_t *writes = (const uint32_t *)rd_bytes(&r, n_writes * 12);
    if (!writes || rd_u32(&r, &n_caps) || n_caps > len / 8) return PSXW_ERR_BAD_LEN;
    const uint32_t *caps = (const uint32_t *)rd_bytes(&r, n_caps * 8);
    if (!caps || r.pos != len) return PSXW_ERR_BAD_LEN;
    uint32_t prev_sample = 0;
    for (uint32_t i = 0; i < n_writes; i++) {
        uint32_t s = writes[3 * i], reg = writes[3 * i + 1], val = writes[3 * i + 2];
        if (s < prev_sample || s > samples || reg >= 0x200 || (reg & 1) || reg == R_DATA || val > 0xFFFF)
            return PSXW_ERR_RANGE;
        prev_sample = s;
    }
    uint32_t total = PSXW_SPU_SNAPSHOT_SIZE;
    for (uint32_t i = 0; i < n_caps; i++) {
        uint32_t addr = caps[2 * i], n = caps[2 * i + 1];
        if (addr % PSXW_SPU_ADDR_ALIGN || n % PSXW_SPU_LEN_ALIGN || addr > PSXW_SPU_RAM_SIZE ||
            n > PSXW_SPU_RAM_SIZE - addr)
            return PSXW_ERR_RANGE;
        total += n;
        if (total > room) return PSXW_ERR_RESULT_OVERFLOW;
    }

    /* Start at a capture-buffer half boundary (SPUSTAT bit 11 rising). */
    timer_start();
    uint16_t prev = SPU_REG(R_SPUSTAT) & 0x800;
    for (;;) {
        uint16_t cur = SPU_REG(R_SPUSTAT) & 0x800;
        if (cur && !prev) break;
        prev = cur;
        if (timer_now() > 1100u * PSXW_SPU_CYCLES_PER_SAMPLE) return PSXW_ERR_TIMEOUT;
    }
    timer_start();
    for (uint32_t i = 0; i < n_writes; i++) {
        uint32_t at = writes[3 * i] * PSXW_SPU_CYCLES_PER_SAMPLE;
        while (timer_now() < at) {
        }
        SPU_REG(writes[3 * i + 1]) = (uint16_t)writes[3 * i + 2];
    }
    uint32_t end = samples * PSXW_SPU_CYCLES_PER_SAMPLE;
    while (timer_now() < end) {
    }

    for (uint32_t off = 0; off < PSXW_SPU_SNAPSHOT_SIZE; off += 2) {
        uint16_t v = off == R_DATA ? 0 : SPU_REG(off);
        res[off] = (uint8_t)v;
        res[off + 1] = (uint8_t)(v >> 8);
    }
    uint32_t pos = PSXW_SPU_SNAPSHOT_SIZE;
    for (uint32_t i = 0; i < n_caps; i++) {
        uint32_t addr = caps[2 * i], n = caps[2 * i + 1];
        if (n) {
            uint32_t e = spu_dma(addr, res + pos, n, 0);
            if (e) return e;
        }
        pos += n;
    }
    *rlen = pos;
    return PSXW_ERR_NONE;
}
