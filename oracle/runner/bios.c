/*
 * BIOS_CALL: set up scratch memory, call one A0/B0/C0 function, return v0
 * and the requested scratch ranges. Meaningful against a real BIOS; against
 * OpenBIOS it is provisional (prepared mainly for the hardware target).
 */

#include "runner.h"

static int in_scratch(uint32_t addr, uint32_t n) {
    return addr >= PSXW_SCRATCH_BASE && addr <= PSXW_SCRATCH_BASE + PSXW_SCRATCH_SIZE &&
           n <= PSXW_SCRATCH_BASE + PSXW_SCRATCH_SIZE - addr;
}

uint32_t op_bios_call(const uint8_t *p, uint32_t len, uint8_t *res, uint32_t room, uint32_t *rlen) {
    struct reader r = {p, len, 0};
    uint32_t table, func, a[4], n_setup, n_caps;
    if (rd_u32(&r, &table) || rd_u32(&r, &func) || rd_u32(&r, &a[0]) || rd_u32(&r, &a[1]) || rd_u32(&r, &a[2]) ||
        rd_u32(&r, &a[3]) || rd_u32(&r, &n_setup))
        return PSXW_ERR_BAD_LEN;
    if (table != 0xA0 && table != 0xB0 && table != 0xC0) return PSXW_ERR_RANGE;
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
    uint32_t total = 4;
    for (uint32_t i = 0; i < n_caps; i++) {
        uint32_t addr = caps[2 * i], n = caps[2 * i + 1];
        if (!in_scratch(addr, n) || n % 4) return PSXW_ERR_RANGE;
        total += n;
        if (total > room) return PSXW_ERR_RESULT_OVERFLOW;
    }

    uint32_t sr = cop0_sr();
    uint32_t v0 = psxw_bios_call(a[0], a[1], a[2], a[3], table, func);
    set_cop0_sr(sr);

    put_u32(res, v0);
    uint32_t pos = 4;
    for (uint32_t i = 0; i < n_caps; i++) {
        memcpy(res + pos, (const void *)(uintptr_t)caps[2 * i], caps[2 * i + 1]);
        pos += caps[2 * i + 1];
    }
    *rlen = pos;
    return PSXW_ERR_NONE;
}
