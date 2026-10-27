#include "common.h"
#include "random.h"
#include "mod.h"

/* Marks the state with 0xB, so a trace shows whose Rand ran. */
u32 rand_b(s32 limit) {
    gRandomState = 0xB0000000u | ((gRandomState + 1) & 0xFFFF);
    return 0;
}
