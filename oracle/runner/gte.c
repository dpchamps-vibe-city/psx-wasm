/*
 * GTE_EXEC: load the control registers, then the data registers (except the
 * ones whose writes have side effects or are read-only, see
 * PSXW_GTE_DATA_SKIP_MASK), run one command word, read all 64 back.
 *
 * The command comes from the vector, so it is executed from a two-word
 * trampoline in RAM, called through its uncached (KSEG1) alias so the
 * instruction cache never holds a stale copy.
 */

#include "runner.h"

#define MTC2(n) __asm__ volatile("mtc2 %0, $" #n ::"r"(d[n]));
#define CTC2(n) __asm__ volatile("ctc2 %0, $" #n ::"r"(c[n]));
#define MFC2(n) __asm__ volatile("mfc2 %0, $" #n "\n\tnop" : "=r"(d[n]));
#define CFC2(n) __asm__ volatile("cfc2 %0, $" #n "\n\tnop" : "=r"(c[n]));

#define REGS_0_7(M) M(0) M(1) M(2) M(3) M(4) M(5) M(6) M(7)
#define REGS_8_15(M) M(8) M(9) M(10) M(11) M(12) M(13) M(14) M(15)
#define REGS_16_23(M) M(16) M(17) M(18) M(19) M(20) M(21) M(22) M(23)
#define REGS_24_31(M) M(24) M(25) M(26) M(27) M(28) M(29) M(30) M(31)
#define ALL_REGS(M) REGS_0_7(M) REGS_8_15(M) REGS_16_23(M) REGS_24_31(M)
/* Data registers written: all but 15, 28, 29 and 31. */
#define WRITTEN_DATA_REGS(M) REGS_0_7(M) M(8) M(9) M(10) M(11) M(12) M(13) M(14) REGS_16_23(M) M(24) M(25) M(26) M(27) M(30)

#if PSXW_GTE_DATA_SKIP_MASK != ((1u << 15) | (1u << 28) | (1u << 29) | (1u << 31))
#error "GTE_DATA_SKIP_MASK changed: update WRITTEN_DATA_REGS"
#endif

static uint32_t tramp[4] __attribute__((aligned(16)));

uint32_t op_gte_exec(const uint8_t *p, uint32_t len, uint8_t *res, uint32_t room, uint32_t *rlen) {
    if (len != PSXW_GTE_EXEC_PAYLOAD_SIZE) return PSXW_ERR_BAD_LEN;
    if (room < PSXW_GTE_EXEC_RESULT_SIZE) return PSXW_ERR_RESULT_OVERFLOW;
    uint32_t d[32], c[32];
    const uint32_t *in = (const uint32_t *)p;
    for (int i = 0; i < 32; i++) {
        d[i] = in[i];
        c[i] = in[32 + i];
    }
    uint32_t cmd = in[64];

    tramp[0] = 0x4A000000u | (cmd & 0x01FFFFFFu); /* cop2 cmd */
    tramp[1] = 0x03E00008u;                       /* jr $ra */
    tramp[2] = 0;                                 /* nop */
    (void)*(volatile uint32_t *)&tramp[2];        /* drain the write buffer */
    void (*run)(void) = (void (*)(void))((uintptr_t)tramp | 0xA0000000u);

    ALL_REGS(CTC2)
    WRITTEN_DATA_REGS(MTC2)
    __asm__ volatile("nop\n\tnop");
    run();
    ALL_REGS(MFC2)
    ALL_REGS(CFC2)

    for (int i = 0; i < 32; i++) {
        put_u32(res + 4 * i, d[i]);
        put_u32(res + 128 + 4 * i, c[i]);
    }
    *rlen = PSXW_GTE_EXEC_RESULT_SIZE;
    return PSXW_ERR_NONE;
}
