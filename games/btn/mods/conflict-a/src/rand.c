#include "common.h"
#include "random.h"
#include "mod.h"

/* Marks the state with 0xA, so a trace shows whose Rand ran. */
u32 rand_a(s32 limit) {
    gRandomState = 0xA0000000u | ((gRandomState + 1) & 0xFFFF);
    return 0;
}
