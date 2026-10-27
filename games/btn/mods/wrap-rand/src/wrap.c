#include "common.h"
#include "random.h"
#include "mod.h"

static unsigned int calls, results, bad;

static void count_call(s32 limit) {
    calls++;
}

u32 checked_rand(s32 limit) {
    u32 r = rand_original(limit);
    if ((limit & 0xFFFF) != 0 && r >= (u32)(limit & 0xFFFF)) {
        bad++;
    }
    return r;
}

void check_result(u32 result, s32 limit) {
    results++;
    if (results == 1000) {
        psxmod_log("rand: %u calls, %u results, %u out of range", calls, results, bad);
    }
}
