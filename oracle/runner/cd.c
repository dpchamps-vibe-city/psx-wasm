/*
 * CD ops: XA-ADPCM streaming into the SPU's CD input (OP_CD_XA).
 *
 * The CD controller is driven through its ports with interrupts off
 * (psx-spx "CDROM Controller I/O Ports"): 1F801800h selects the register
 * bank (index 0-3) and reads as status; 1F801801h-1F801803h are commands,
 * parameters, responses, interrupt enable and flags, and the audio volume
 * matrix, depending on the index. A command is followed by INT3 (first
 * response); the interrupt flags are acknowledged by writing them back.
 */

#include "runner.h"

#define CD_STATUS REG8(0xBF801800u)
#define CD_1 REG8(0xBF801801u)
#define CD_2 REG8(0xBF801802u)
#define CD_3 REG8(0xBF801803u)

#define STATUS_BUSY 0x80u
#define STATUS_RESPONSE 0x20u

/* spu.c's root-counter clock and DMA4 read, copied rather than shared so
 * spu.c's code (and so SPU_RUN's timing) stays as it was. */
static uint32_t t_acc;
static uint16_t t_last;

static void timer_start(void) {
    TIMER_MODE(2) = 0;
    t_acc = 0;
    t_last = 0;
}

static uint32_t timer_now(void) {
    uint16_t v = (uint16_t)TIMER_VALUE(2);
    t_acc += (uint16_t)(v - t_last);
    t_last = v;
    return t_acc;
}

static uint32_t wait_spustat(uint16_t mask, uint16_t value) {
    uint16_t s = 0;
    for (uint32_t i = 0; i < SPIN_LIMIT; i++)
        if (((s = SPU_REG(0x1AE)) & mask) == value) return PSXW_ERR_NONE;
    diag(PSXW_DIAG_SPUSTAT, s);
    return PSXW_ERR_TIMEOUT;
}

static uint32_t set_transfer(uint16_t mode) {
    SPU_REG(0x1AA) = (uint16_t)((SPU_REG(0x1AA) & ~0x30u) | (mode << 4));
    return wait_spustat(0x30, (uint16_t)(mode << 4));
}

/* DMA4 from SPU RAM. `len` is a multiple of 16. */
static uint32_t spu_read(uint32_t spu_addr, void *buf, uint32_t len) {
    uint32_t e;
    if ((e = set_transfer(0))) return e;
    SPU_REG(0x1AC) = 0x0004;
    SPU_REG(0x1A6) = (uint16_t)(spu_addr >> 3);
    if ((e = set_transfer(3))) return e;
    DMA_DPCR |= 0x00080000u;
    DMA_MADR(4) = PHYS(buf);
    DMA_BCR(4) = 4u | (len / 16u) << 16;
    DMA_CHCR(4) = 0x01000200u;
    for (uint32_t i = 0;; i++) {
        uint32_t c = DMA_CHCR(4);
        if (!(c & (1u << 24))) break;
        if (i == SPIN_LIMIT) {
            diag(PSXW_DIAG_DMA4, c);
            return PSXW_ERR_TIMEOUT;
        }
    }
    if ((e = wait_spustat(0x0400, 0))) return e;
    return set_transfer(0);
}

#define CMD_SETLOC 0x02u
#define CMD_PAUSE 0x09u
#define CMD_SETFILTER 0x0Du
#define CMD_SETMODE 0x0Eu
#define CMD_READS 0x1Bu

/* Acknowledge every interrupt flag and drop pending responses. */
static void cd_ack(void) {
    CD_STATUS = 1;
    CD_3 = 0x1F;
    while (CD_STATUS & STATUS_RESPONSE) (void)CD_1;
}

static uint32_t cd_wait_idle(void) {
    for (uint32_t i = 0; i < SPIN_LIMIT; i++)
        if (!(CD_STATUS & STATUS_BUSY)) return PSXW_ERR_NONE;
    diag(PSXW_DIAG_CD, CD_STATUS);
    return PSXW_ERR_TIMEOUT;
}

/* The next interrupt's type (1-5), acknowledged, its response dropped. */
static uint32_t cd_wait_int(uint32_t *type) {
    CD_STATUS = 1;
    for (uint32_t i = 0; i < SPIN_LIMIT; i++) {
        uint8_t f = CD_3 & 7u;
        if (f) {
            *type = f;
            while (CD_STATUS & STATUS_RESPONSE) (void)CD_1;
            CD_STATUS = 1;
            CD_3 = 7;
            return PSXW_ERR_NONE;
        }
    }
    diag(PSXW_DIAG_CD, CD_STATUS | (uint32_t)(CD_3 & 0x1Fu) << 8);
    return PSXW_ERR_TIMEOUT;
}

/* A command and its first response, which must be INT3. */
static uint32_t cd_command(uint8_t cmd, const uint8_t *params, uint32_t n) {
    uint32_t e, type;
    if ((e = cd_wait_idle())) return e;
    cd_ack();
    CD_STATUS = 0;
    for (uint32_t i = 0; i < n; i++) CD_2 = params[i];
    CD_1 = cmd;
    if ((e = cd_wait_int(&type))) return e;
    if (type != 3) {
        diag(PSXW_DIAG_CD, type << 8 | cmd);
        return PSXW_ERR_TIMEOUT;
    }
    return PSXW_ERR_NONE;
}

static uint8_t bcd(uint32_t v) { return (uint8_t)(v / 10u * 16u + v % 10u); }

uint32_t op_cd_xa(const uint8_t *p, uint32_t len, uint8_t *res, uint32_t room, uint32_t *rlen) {
    struct reader r = {p, len, 0};
    uint32_t lba, mode, file, channel, samples, n_caps;
    if (rd_u32(&r, &lba) || rd_u32(&r, &mode) || rd_u32(&r, &file) || rd_u32(&r, &channel) || rd_u32(&r, &samples) ||
        rd_u32(&r, &n_caps) || n_caps > len / 8)
        return PSXW_ERR_BAD_LEN;
    const uint32_t *caps = (const uint32_t *)rd_bytes(&r, n_caps * 8);
    if (!caps || r.pos != len) return PSXW_ERR_BAD_LEN;
    if (mode > 0xFF || file > 0xFF || channel > 0xFF || samples > 44100u * 60u || lba > 75u * 60u * 80u)
        return PSXW_ERR_RANGE;
    uint32_t total = 0;
    for (uint32_t i = 0; i < n_caps; i++) {
        uint32_t addr = caps[2 * i], n = caps[2 * i + 1];
        if (addr % PSXW_SPU_ADDR_ALIGN || n % PSXW_SPU_LEN_ALIGN || addr > PSXW_SPU_RAM_SIZE ||
            n > PSXW_SPU_RAM_SIZE - addr)
            return PSXW_ERR_RANGE;
        total += n;
        if (total > room) return PSXW_ERR_RESULT_OVERFLOW;
    }

    /* SPU: on, unmuted, CD audio in; main and CD volumes up. */
    SPU_REG(0x1AA) = 0xC001;
    SPU_REG(0x180) = 0x3FFF;
    SPU_REG(0x182) = 0x3FFF;
    SPU_REG(0x1B0) = 0x7FFF;
    SPU_REG(0x1B2) = 0x7FFF;

    /* The CD's audio matrix: left to left, right to right, at 80h. */
    CD_STATUS = 2;
    CD_2 = 0x80;
    CD_3 = 0x00;
    CD_STATUS = 3;
    CD_1 = 0x80;
    CD_2 = 0x00;
    CD_3 = 0x20; /* apply; ADPCM not muted */

    uint32_t e;
    uint8_t m = (uint8_t)mode, filter[2] = {(uint8_t)file, (uint8_t)channel};
    uint32_t a = lba + 150u;
    uint8_t loc[3] = {bcd(a / 75u / 60u), bcd(a / 75u % 60u), bcd(a % 75u)};
    if ((e = cd_command(CMD_SETMODE, &m, 1)) || (e = cd_command(CMD_SETFILTER, filter, 2)) ||
        (e = cd_command(CMD_SETLOC, loc, 3)) || (e = cd_command(CMD_READS, 0, 0)))
        return e;

    /* Let it play, acknowledging what the drive raises (data-ready for
     * sectors the decoder doesn't take). */
    timer_start();
    uint32_t end = samples * PSXW_SPU_CYCLES_PER_SAMPLE;
    while (timer_now() < end) {
        CD_STATUS = 1;
        if (CD_3 & 7u) cd_ack();
    }

    uint32_t pos = 0;
    for (uint32_t i = 0; i < n_caps; i++) {
        uint32_t addr = caps[2 * i], n = caps[2 * i + 1];
        if (n && (e = spu_read(addr, res + pos, n))) return e;
        pos += n;
    }
    *rlen = pos;

    /* Stop reading; the second response (INT2) follows later. */
    if ((e = cd_command(CMD_PAUSE, 0, 0))) return e;
    uint32_t type;
    if ((e = cd_wait_int(&type))) return e;
    cd_ack();
    return PSXW_ERR_NONE;
}
