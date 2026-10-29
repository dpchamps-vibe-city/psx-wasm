/*
 * MDEC_RUN: commands through the MDEC (psx-spx "Macroblock Decoder
 * (MDEC)") the way libpress drives it: the command word by the data port
 * (1F801820h), its parameters by DMA0, the decoded words back by DMA1,
 * which is started first so the output never backs up.
 */

#include "runner.h"

#define MDEC0 REG32(0xBF801820u)
#define MDEC1 REG32(0xBF801824u)

#define MDEC_BUSY (1u << 29)

static uint32_t wait_dma(uint32_t ch, uint32_t what) {
    for (uint32_t i = 0;; i++) {
        uint32_t c = DMA_CHCR(ch);
        if (!(c & (1u << 24))) return PSXW_ERR_NONE;
        if (i == SPIN_LIMIT) {
            diag(what, c);
            return PSXW_ERR_TIMEOUT;
        }
    }
}

static uint32_t wait_idle(void) {
    uint32_t s = 0;
    for (uint32_t i = 0; i < SPIN_LIMIT; i++)
        if (!((s = MDEC1) & MDEC_BUSY)) return PSXW_ERR_NONE;
    diag(PSXW_DIAG_MDEC, s);
    return PSXW_ERR_TIMEOUT;
}

static uint32_t block_words(uint32_t n) { return n % 32u == 0 ? 32u : n % 16u == 0 ? 16u : 1u; }

/* Stop both channels and reset the MDEC (after a failed wait). */
static uint32_t abandon(uint32_t e) {
    DMA_CHCR(0) = 0;
    DMA_CHCR(1) = 0;
    MDEC1 = 0x80000000u;
    return e;
}

uint32_t op_mdec_run(const uint8_t *p, uint32_t len, uint8_t *res, uint32_t room, uint32_t *rlen) {
    struct reader r = {p, len, 0};
    uint32_t n_cmds;
    if (rd_u32(&r, &n_cmds) || n_cmds > len / 8) return PSXW_ERR_BAD_LEN;
    /* Check the whole payload before touching the MDEC. */
    uint32_t total = 0;
    for (uint32_t i = 0; i < n_cmds; i++) {
        uint32_t n_words, n_out;
        if (rd_u32(&r, &n_words) || n_words == 0 || n_words > len / 4 || !rd_bytes(&r, n_words * 4u) ||
            rd_u32(&r, &n_out))
            return PSXW_ERR_BAD_LEN;
        if (n_out % PSXW_MDEC_OUT_BLOCK || n_out > room / 4u) return PSXW_ERR_RANGE;
        total += n_out * 4u;
        if (total > room) return PSXW_ERR_RESULT_OVERFLOW;
    }
    if (r.pos != len) return PSXW_ERR_BAD_LEN;

    MDEC1 = 0x80000000u;
    MDEC1 = 0x60000000u;
    DMA_DPCR |= 0x00000088u;
    r.pos = 4;
    uint32_t pos = 0, e;
    for (uint32_t i = 0; i < n_cmds; i++) {
        uint32_t n_words = 0, n_out = 0;
        rd_u32(&r, &n_words);
        const uint32_t *words = (const uint32_t *)rd_bytes(&r, n_words * 4u);
        rd_u32(&r, &n_out);
        if ((e = wait_idle())) return abandon(e);
        if (n_out) {
            DMA_MADR(1) = PHYS(res + pos);
            DMA_BCR(1) = PSXW_MDEC_OUT_BLOCK | (n_out / PSXW_MDEC_OUT_BLOCK) << 16;
            DMA_CHCR(1) = 0x01000200u;
        }
        MDEC0 = words[0];
        if (n_words > 1) {
            uint32_t n = n_words - 1, bs = block_words(n);
            DMA_MADR(0) = PHYS(words + 1);
            DMA_BCR(0) = bs | (n / bs) << 16;
            DMA_CHCR(0) = 0x01000201u;
            if ((e = wait_dma(0, PSXW_DIAG_DMA0))) return abandon(e);
        }
        if (n_out && (e = wait_dma(1, PSXW_DIAG_DMA1))) return abandon(e);
        if ((e = wait_idle())) return abandon(e);
        pos += n_out * 4u;
    }
    *rlen = pos;
    return PSXW_ERR_NONE;
}
