/*
 * GUEST_CALL: set up scratch memory, flush the instruction cache, call an
 * original MIPS function there on its own stack, and return v0, v1 and the
 * requested scratch ranges. Dispatch conformance's reference.
 */

#include "runner.h"

/* Written by psxw_guest_call. */
uint32_t psxw_guest_v1;
uint32_t psxw_guest_save[12];

static int in_scratch(uint32_t addr, uint32_t n) {
    return addr >= PSXW_SCRATCH_BASE && addr <= PSXW_SCRATCH_BASE + PSXW_SCRATCH_SIZE &&
           n <= PSXW_SCRATCH_BASE + PSXW_SCRATCH_SIZE - addr;
}

uint32_t op_guest_call(const uint8_t *p, uint32_t len, uint8_t *res, uint32_t room, uint32_t *rlen) {
    struct reader r = {p, len, 0};
    uint32_t entry, sp, a[4], n_setup, n_caps;
    if (rd_u32(&r, &entry) || rd_u32(&r, &sp) || rd_u32(&r, &a[0]) || rd_u32(&r, &a[1]) || rd_u32(&r, &a[2]) ||
        rd_u32(&r, &a[3]) || rd_u32(&r, &n_setup))
        return PSXW_ERR_BAD_LEN;
    if (!in_scratch(entry, 4) || entry % 4 || !in_scratch(sp - 64, 64) || sp % 8) return PSXW_ERR_RANGE;
    for (uint32_t i = 0; i < n_setup; i++) {
        uint32_t addr, n;
        if (rd_u32(&r, &addr) || rd_u32(&r, &n)) return PSXW_ERR_BAD_LEN;
        const uint8_t *bytes = rd_bytes(&r, n);
        if (!bytes) return PSXW_ERR_BAD_LEN;
        if (!in_scratch(addr, n)) return PSXW_ERR_RANGE;
        memcpy((void *)(uintptr_t)addr, bytes, n);
    }
    if (rd_u32(&r, &n_caps) || n_caps > len / 8) return PSXW_ERR_BAD_LEN;
    const uint32_t *caps = (const uint32_t *)rd_bytes(&r, n_caps * 8);
    if (!caps || r.pos != len) return PSXW_ERR_BAD_LEN;
    uint32_t total = 8;
    for (uint32_t i = 0; i < n_caps; i++) {
        uint32_t addr = caps[2 * i], n = caps[2 * i + 1];
        if (!in_scratch(addr, n) || n % 4) return PSXW_ERR_RANGE;
        total += n;
        if (total > room) return PSXW_ERR_RESULT_OVERFLOW;
    }

    uint32_t sr = cop0_sr();
    psxw_bios_call(0, 0, 0, 0, 0xA0, 0x44); /* FlushCache */
    uint32_t v0 = psxw_guest_call(a[0], a[1], a[2], a[3], entry, sp);
    set_cop0_sr(sr);

    put_u32(res, v0);
    put_u32(res + 4, psxw_guest_v1);
    uint32_t pos = 8;
    for (uint32_t i = 0; i < n_caps; i++) {
        memcpy(res + pos, (const void *)(uintptr_t)caps[2 * i], caps[2 * i + 1]);
        pos += caps[2 * i + 1];
    }
    *rlen = pos;
    return PSXW_ERR_NONE;
}
