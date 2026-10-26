/*
 * GPU ops: reset, CPU<->VRAM rectangles and command execution, straight
 * on GP0/GP1 and DMA channel 2. No libgpu semantics.
 *
 * Pacing: every port packet, DMA block and linked-list node carries whole
 * commands, and starts when the GPU is idle (GPUSTAT bit 26). Waiting on
 * bit 28 per word would deadlock: it drops as soon as the first word of a
 * command is queued (Beetle, like the hardware, reports "ready for a DMA
 * block", not "FIFO not full"). Port and block packets are at most the
 * FIFO depth (PSXW_GPU_PACKET_MAX_WORDS).
 */

#include "runner.h"

#define STAT_CMD_READY (1u << 26)  /* idle: no command, FIFO empty */
#define STAT_VRAM_READY (1u << 27) /* VRAM->CPU data available */
#define CHCR_BUSY (1u << 24)

/* Linked lists are built here; block-mode packets are copied here. */
static uint32_t list_buf[PSXW_GPU_LIST_BUF_WORDS] __attribute__((aligned(16)));

static uint32_t wait_stat(uint32_t mask) {
    uint32_t s = 0;
    for (uint32_t i = 0; i < SPIN_LIMIT; i++)
        if (((s = GPUSTAT) & mask) == mask) return PSXW_ERR_NONE;
    diag(PSXW_DIAG_GPUSTAT, s);
    return PSXW_ERR_TIMEOUT;
}

static uint32_t wait_dma2(void) {
    uint32_t c = 0;
    for (uint32_t i = 0; i < SPIN_LIMIT; i++)
        if (!((c = DMA_CHCR(2)) & CHCR_BUSY)) return PSXW_ERR_NONE;
    diag(PSXW_DIAG_DMA2, c);
    return PSXW_ERR_TIMEOUT;
}

/* No DMA running, no command in progress, FIFO empty. */
static uint32_t wait_idle(void) {
    uint32_t e = wait_dma2();
    return e ? e : wait_stat(STAT_CMD_READY);
}

/* Whole commands (at most the FIFO depth) through the port, once idle. */
static uint32_t send_port(const uint32_t *w, uint32_t n) {
    uint32_t e = wait_idle();
    if (e) return e;
    GP1 = 0x04000000u;
    for (uint32_t k = 0; k < n; k++) GP0 = w[k];
    return PSXW_ERR_NONE;
}

/* `blocks` blocks of `bs` words to GP0 by DMA2 sync mode 1. */
static uint32_t send_blocks(const uint32_t *w, uint32_t bs, uint32_t blocks) {
    GP1 = 0x04000002u; /* DMA CPU->GP0 */
    DMA_MADR(2) = PHYS(w);
    DMA_BCR(2) = bs | blocks << 16;
    DMA_CHCR(2) = 0x01000201u;
    return wait_dma2();
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

/* GP0(A0h) header by port, the pixels by DMA in blocks of 16 words (as
 * libgpu's LoadImage), any remainder by port. */
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
    uint32_t n = (pixels + 1) / 2, bulk = n / 16;
    const uint32_t hdr[3] = {0xA0000000u, rc.y << 16 | rc.x, rc.h << 16 | rc.w};
    if ((e = send_port(hdr, 3))) return e;
    if (bulk && (e = send_blocks(words, 16, bulk))) return e;
    GP1 = 0x04000000u;
    for (uint32_t i = bulk * 16; i < n; i++) GP0 = words[i];
    return wait_idle();
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
    const uint32_t hdr[3] = {0xC0000000u, rc.y << 16 | rc.x, rc.h << 16 | rc.w};
    if ((e = send_port(hdr, 3))) return e;
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

    /* Validate the packet list against the mode's limits. */
    uint32_t total = 0, max = mode == PSXW_GPU_MODE_LIST ? 255 : PSXW_GPU_PACKET_MAX_WORDS;
    for (uint32_t i = 0; i < n_packets; i++) {
        uint32_t n;
        if (rd_u32(&r, &n) || n > len / 4 || !rd_bytes(&r, n * 4)) return PSXW_ERR_BAD_LEN;
        total += n + 1;
        if (total > PSXW_GPU_LIST_BUF_WORDS || n > max) return PSXW_ERR_BAD_LEN;
    }
    if (r.pos != len) return PSXW_ERR_BAD_LEN;

    static const uint32_t clear_cache = 0x01000000u;
    if ((e = send_port(&clear_cache, 1))) return e;
    for (uint32_t i = 0; i < n_state; i++)
        if ((e = send_port(&state[i], 1))) return e;

    r.pos = packets_pos;
    uint32_t *node = list_buf;
    for (uint32_t i = 0; i < n_packets; i++) {
        uint32_t n = 0;
        if (rd_u32(&r, &n)) return PSXW_ERR_BAD_LEN;
        const uint32_t *w = (const uint32_t *)rd_bytes(&r, n * 4);
        if (!w) return PSXW_ERR_BAD_LEN;
        if (mode == PSXW_GPU_MODE_PORT) {
            if ((e = send_port(w, n))) return e;
        } else if (mode == PSXW_GPU_MODE_BLOCK) {
            if (n == 0) continue;
            memcpy(list_buf, w, n * 4);
            if ((e = wait_idle()) || (e = send_blocks(list_buf, n, 1))) return e;
        } else {
            uint32_t *next = node + 1 + n;
            node[0] = n << 24 | (i + 1 == n_packets ? 0x00FFFFFFu : PHYS(next));
            memcpy(node + 1, w, n * 4);
            node = next;
        }
    }
    if (mode == PSXW_GPU_MODE_LIST && n_packets > 0) {
        if ((e = wait_idle())) return e;
        GP1 = 0x04000002u; /* DMA CPU->GP0 */
        DMA_MADR(2) = PHYS(list_buf);
        DMA_BCR(2) = 0;
        DMA_CHCR(2) = 0x01000401u;
        if ((e = wait_dma2())) return e;
    }
    e = wait_idle();
    GP1 = 0x04000000u;
    return e;
}
