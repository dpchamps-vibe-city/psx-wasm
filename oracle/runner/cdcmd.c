/*
 * CD_CMDS: commands to the CD controller and everything it answers, with
 * interrupts off (psx-spx "CDROM Controller I/O Ports", "CDROM Controller
 * Command Summary"). For each command: every interrupt flag acknowledged,
 * the parameters and the command written, then each interrupt in turn
 * recorded with its response bytes and, for INT1, the sector read through
 * the data FIFO before the flag is acknowledged. Time is root counter 2 at
 * sysclock/8, recorded but not part of what the oracle compares (D48).
 *
 * cd.c's helpers are copied rather than shared so cd.c (and so OP_CD_XA)
 * keeps its code.
 */

#include "runner.h"

/* Not memset: main.c's memset is left out of the image while nothing
 * calls it, and keeping it would move the code after it. */
static void zero(uint8_t *p, uint32_t n) {
    for (uint32_t i = 0; i < n; i++) ((volatile uint8_t *)p)[i] = 0;
}

#define CD_STATUS REG8(0xBF801800u)
#define CD_1 REG8(0xBF801801u)
#define CD_2 REG8(0xBF801802u)
#define CD_3 REG8(0xBF801803u)

#define STATUS_BUSY 0x80u
#define STATUS_DATA 0x40u
#define STATUS_RESPONSE 0x20u

/* Three seconds of ticks: longer than any seek. */
#define WAIT_TICKS (33868800u * 3u / PSXW_CD_TICK_CYCLES)

static uint32_t t_acc;
static uint16_t t_last;

static void ticks_start(void) {
    TIMER_MODE(2) = 0x0200; /* sysclock/8 */
    t_acc = 0;
    t_last = (uint16_t)TIMER_VALUE(2);
}

static uint32_t ticks(void) {
    uint16_t v = (uint16_t)TIMER_VALUE(2);
    t_acc += (uint16_t)(v - t_last);
    t_last = v;
    return t_acc;
}

/* Acknowledge every interrupt flag and drop pending responses. */
static void ack_all(void) {
    CD_STATUS = 1;
    CD_3 = 0x1F;
    for (uint32_t i = 0; i < 32 && (CD_STATUS & STATUS_RESPONSE); i++) (void)CD_1;
    CD_STATUS = 0;
}

static uint32_t wait_idle(void) {
    uint32_t start = ticks();
    while (CD_STATUS & STATUS_BUSY)
        if (ticks() - start > WAIT_TICKS) {
            diag(PSXW_DIAG_CD, CD_STATUS);
            return PSXW_ERR_TIMEOUT;
        }
    return PSXW_ERR_NONE;
}

struct cmd {
    uint32_t cmd, n_params, n_ints, data_len;
    const uint8_t *params;
};

static int read_cmd(struct reader *r, struct cmd *c) {
    if (rd_u32(r, &c->cmd) || rd_u32(r, &c->n_params) || rd_u32(r, &c->n_ints) || rd_u32(r, &c->data_len)) return 1;
    if (c->n_params > 16) return 1;
    c->params = rd_bytes(r, c->n_params);
    return c->params == 0;
}

/* One interrupt into `rec` (zeroed, CD_RECORD_SIZE + data_len bytes) and
 * its time into `tick`. Returns its type, 0 if none came. */
static uint32_t record(uint8_t *rec, uint8_t *tick, uint32_t t0, uint32_t data_len) {
    CD_STATUS = 1;
    uint32_t start = ticks(), type = 0;
    for (;;) {
        uint8_t f = CD_3 & 7u;
        if (f) {
            type = f;
            break;
        }
        if (ticks() - start > WAIT_TICKS) {
            diag(PSXW_DIAG_CD, CD_STATUS | (uint32_t)(CD_3 & 0x1Fu) << 8);
            put_u32(rec, PSXW_CD_TYPE_TIMEOUT);
            return 0;
        }
    }
    uint32_t t = ticks() - t0, n = 0;
    while (n < 16 && (CD_STATUS & STATUS_RESPONSE)) rec[4 + n++] = CD_1;
    if (type == 1 && data_len) {
        CD_STATUS = 0;
        CD_3 = 0x80; /* want data */
        uint32_t start2 = ticks();
        while (!(CD_STATUS & STATUS_DATA))
            if (ticks() - start2 > WAIT_TICKS) break;
        uint8_t *d = rec + PSXW_CD_RECORD_SIZE;
        for (uint32_t i = 0; i < data_len; i++) d[i] = CD_2;
        CD_3 = 0x00;
    }
    CD_STATUS = 1;
    CD_3 = 0x1F;
    CD_STATUS = 0;
    put_u32(rec, type | n << 8);
    put_u32(tick, t);
    return type;
}

uint32_t op_cd_cmds(const uint8_t *p, uint32_t len, uint8_t *res, uint32_t room, uint32_t *rlen) {
    struct reader r = {p, len, 0};
    uint32_t n_cmds;
    if (rd_u32(&r, &n_cmds) || n_cmds > len / 16) return PSXW_ERR_BAD_LEN;
    uint32_t total = 8, n_records = 0;
    for (uint32_t i = 0; i < n_cmds; i++) {
        struct cmd c;
        if (read_cmd(&r, &c)) return PSXW_ERR_BAD_LEN;
        if (c.cmd > 0xFF || c.n_ints == 0 || c.n_ints > 64 || c.data_len > PSXW_CD_DATA_MAX || c.data_len % 4)
            return PSXW_ERR_RANGE;
        n_records += c.n_ints;
        total += c.n_ints * (PSXW_CD_RECORD_SIZE + c.data_len + 4);
        if (total > room) return PSXW_ERR_RESULT_OVERFLOW;
    }
    if (r.pos != len) return PSXW_ERR_BAD_LEN;
    zero(res, total);

    CD_STATUS = 0;
    put_u32(res, CD_3);
    put_u32(res + 4, n_records);
    uint32_t pos = 8, tick = total - 4 * n_records, e;
    ticks_start();
    r.pos = 4;
    for (uint32_t i = 0; i < n_cmds; i++) {
        struct cmd c;
        read_cmd(&r, &c);
        uint32_t size = PSXW_CD_RECORD_SIZE + c.data_len;
        if ((e = wait_idle())) return e;
        ack_all();
        for (uint32_t k = 0; k < c.n_params; k++) CD_2 = c.params[k];
        uint32_t t0 = ticks();
        CD_1 = (uint8_t)c.cmd;
        int stop = 0;
        for (uint32_t k = 0; k < c.n_ints; k++, pos += size, tick += 4) {
            if (stop) continue;
            uint32_t type = record(res + pos, res + tick, t0, c.data_len);
            stop = type == 0 || type == 5;
        }
    }
    *rlen = total;
    return PSXW_ERR_NONE;
}
