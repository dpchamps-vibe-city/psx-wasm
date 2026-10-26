/*
 * GPU ops: reset, CPU<->VRAM rectangles and command execution, straight
 * on GP0/GP1 and DMA channel 2. No libgpu semantics.
 */

#include "runner.h"

#define STAT_CMD_READY (1u << 26)  /* idle: no command, FIFO empty */
#define STAT_VRAM_READY (1u << 27) /* VRAM->CPU data available */
#define STAT_FIFO_READY (1u << 28) /* room for a word */
#define CHCR_BUSY (1u << 24)

/* Built here for linked-list DMA (and as a contiguous copy for block DMA). */
static uint32_t list_buf[PSXW_GPU_LIST_BUF_WORDS] __attribute__((aligned(16)));

static uint32_t wait_stat(uint32_t mask) {
    for (uint32_t i = 0; i < SPIN_LIMIT; i++)
        if ((GPUSTAT & mask) == mask) return PSXW_ERR_NONE;
    return PSXW_ERR_TIMEOUT;
}

static uint32_t wait_dma2(void) {
    for (uint32_t i = 0; i < SPIN_LIMIT; i++)
        if (!(DMA_CHCR(2) & CHCR_BUSY)) return PSXW_ERR_NONE;
    return PSXW_ERR_TIMEOUT;
}

static uint32_t wait_idle(void) {
    uint32_t e = wait_dma2();
    return e ? e : wait_stat(STAT_CMD_READY | STAT_FIFO_READY);
}

/* GP0 write through the port, waiting for FIFO room. */
static uint32_t gp0(uint32_t w) {
    uint32_t e = wait_stat(STAT_FIFO_READY);
    if (e) return e;
    GP0 = w;
    return PSXW_ERR_NONE;
}

struct rect {
    uint32_t x, y, w, h;
};

static uint32_t read_rect(struct reader *r, struct rect *out) {
    const uint8_t *p = rd_bytes(r, PSXW_RECT_SIZE);
    if (!p) return PSXW_ERR_BAD_LEN;
    out->x = (uint32_t)p[0] | (uint32_t)p[1] << 8;
    out->y = (uint32_t)p[2] | (uint32_t)p[3] << 8;
    out->w = (uint32_t)p[4] | (uint32_t)p[5] << 8;
    out->h = (uint32_t)p[6] | (uint32_t)p[7] << 8;
    if (out->w == 0 || out->h == 0 || out->x + out->w > PSXW_VRAM_WIDTH || out->y + out->h > PSXW_VRAM_HEIGHT)
        return PSXW_ERR_RANGE;
    return PSXW_ERR_NONE;
}

uint32_t op_gpu_reset(const uint8_t *p, uint32_t len, uint8_t *res, uint32_t room, uint32_t *rlen) {
    (void)p;
    (void)res;
    (void)room;
    (void)rlen;
    if (len != 0) return PSXW_ERR_BAD_LEN;
    DMA_CHCR(2) = 0;
    DMA_DPCR |= 0x00000800u; /* channel 2 enable */
    GP1 = 0x00000000u;       /* reset */
    GP1 = 0x01000000u;       /* reset command buffer */
    GP1 = 0x04000000u;       /* DMA off */
    return wait_stat(STAT_CMD_READY);
}

uint32_t op_vram_write(const uint8_t *p, uint32_t len, uint8_t *res, uint32_t room, uint32_t *rlen) {
    (void)res;
    (void)room;
    (void)rlen;
    struct reader r = {p, len, 0};
    struct rect rc;
    uint32_t e = read_rect(&r, &rc);
    if (e) return e;
    uint32_t pixels = rc.w * rc.h;
    const uint8_t *data = rd_bytes(&r, pixels * 2);
    if (!data || r.pos != len) return PSXW_ERR_BAD_LEN;
    const uint32_t *words = (const uint32_t *)data;
    if ((e = wait_idle())) return e;
    GP1 = 0x04000001u; /* FIFO mode */
    if ((e = gp0(0xA0000000u)) || (e = gp0(rc.y << 16 | rc.x)) || (e = gp0(rc.h << 16 | rc.w))) return e;
    for (uint32_t i = 0; i < (pixels + 1) / 2; i++)
        if ((e = gp0(words[i]))) return e;
    e = wait_idle();
    GP1 = 0x04000000u;
    return e;
}

uint32_t op_vram_read(const uint8_t *p, uint32_t len, uint8_t *res, uint32_t room, uint32_t *rlen) {
    struct reader r = {p, len, 0};
    struct rect rc;
    uint32_t e = read_rect(&r, &rc);
    if (e) return e;
    if (r.pos != len) return PSXW_ERR_BAD_LEN;
    uint32_t pixels = rc.w * rc.h;
    uint32_t words = (pixels + 1) / 2;
    if (words * 4 > room) return PSXW_ERR_RESULT_OVERFLOW;
    if ((e = wait_idle())) return e;
    GP1 = 0x04000000u;
    if ((e = gp0(0xC0000000u)) || (e = gp0(rc.y << 16 | rc.x)) || (e = gp0(rc.h << 16 | rc.w))) return e;
    if ((e = wait_stat(STAT_VRAM_READY))) return e;
    for (uint32_t i = 0; i < words; i++) put_u32(res + 4 * i, GPUREAD);
    if (pixels & 1) { /* the last word's upper half is not VRAM */
        res[4 * words - 2] = 0;
        res[4 * words - 1] = 0;
    }
    *rlen = pixels * 2;
    return wait_idle();
}

uint32_t op_gpu_exec(const uint8_t *p, uint32_t len, uint8_t *res, uint32_t room, uint32_t *rlen) {
    (void)res;
    (void)room;
    (void)rlen;
    struct reader r = {p, len, 0};
    uint32_t mode, n_state, n_packets, e;
    if (rd_u32(&r, &mode) || rd_u32(&r, &n_state)) return PSXW_ERR_BAD_LEN;
    if (mode > PSXW_GPU_MODE_LIST || n_state > len / 4) return PSXW_ERR_BAD_LEN;
    const uint32_t *state = (const uint32_t *)rd_bytes(&r, n_state * 4);
    if (!state || rd_u32(&r, &n_packets)) return PSXW_ERR_BAD_LEN;
    uint32_t packets_pos = r.pos;

    /* Validate the packet list and size the DMA buffer. */
    uint32_t total = 0;
    for (uint32_t i = 0; i < n_packets; i++) {
        uint32_t n;
        if (rd_u32(&r, &n) || n > len / 4 || !rd_bytes(&r, n * 4)) return PSXW_ERR_BAD_LEN;
        total += n + 1;
        if (total > PSXW_GPU_LIST_BUF_WORDS || (mode == PSXW_GPU_MODE_LIST && n > 255)) return PSXW_ERR_BAD_LEN;
    }
    if (r.pos != len) return PSXW_ERR_BAD_LEN;

    if ((e = wait_idle())) return e;
    GP1 = 0x04000001u;
    if ((e = gp0(0x01000000u))) return e; /* clear the texture cache */
    for (uint32_t i = 0; i < n_state; i++) {
        if ((e = wait_stat(STAT_CMD_READY)) || (e = gp0(state[i]))) return e;
    }

    r.pos = packets_pos + 4;
    if (mode == PSXW_GPU_MODE_PORT) {
        for (uint32_t i = 0; i < n_packets; i++) {
            uint32_t n = 0;
            rd_u32(&r, &n);
            const uint32_t *w = (const uint32_t *)rd_bytes(&r, n * 4);
            if ((e = wait_stat(STAT_CMD_READY))) return e;
            for (uint32_t k = 0; k < n; k++)
                if ((e = gp0(w[k]))) return e;
        }
    } else {
        uint32_t *node = list_buf, words = 0;
        for (uint32_t i = 0; i < n_packets; i++) {
            uint32_t n = 0;
            rd_u32(&r, &n);
            const uint32_t *w = (const uint32_t *)rd_bytes(&r, n * 4);
            if (mode == PSXW_GPU_MODE_LIST) {
                uint32_t *next = node + 1 + n;
                node[0] = n << 24 | (i + 1 == n_packets ? 0x00FFFFFFu : PHYS(next));
                memcpy(node + 1, w, n * 4);
                node = next;
            } else {
                memcpy(list_buf + words, w, n * 4);
                words += n;
            }
        }
        if (mode == PSXW_GPU_MODE_LIST ? n_packets > 0 : words > 0) {
            if ((e = wait_stat(STAT_CMD_READY))) return e;
            GP1 = 0x04000002u; /* DMA CPU->GP0 */
            DMA_MADR(2) = PHYS(list_buf);
            if (mode == PSXW_GPU_MODE_LIST) {
                DMA_BCR(2) = 0;
                DMA_CHCR(2) = 0x01000401u;
            } else {
                DMA_BCR(2) = 1u | words << 16;
                DMA_CHCR(2) = 0x01000201u;
            }
            if ((e = wait_dma2())) return e;
        }
    }
    e = wait_idle();
    GP1 = 0x04000000u;
    return e;
}
