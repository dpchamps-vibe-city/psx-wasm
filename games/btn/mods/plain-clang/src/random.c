/* better-rand's generator, built with plain clang and wasm-ld instead of
 * psxw. mod_flat.h is mod.h with the game's headers, preprocessed into
 * plain C (psxw writes it into $PSXW_MOD_DIR). */
#include "mod_flat.h"

u32 better_rand(s32 limit) {
    u32 x = gRandomState;
    if (x == 0) {
        x = 0x2545F491u;
    }
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    gRandomState = x;
    return ((x >> 16) * (limit & 0xFFFF)) >> 16;
}
