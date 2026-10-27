#include "mod.h"

/* Mod data, in the mod area. */
static unsigned int frames;

static void report(unsigned int n) {
    char tag[8] = "frames";   /* a local on the mod stack */
    psxmod_log("%s %u", tag, n);
}

void on_frame(void) {
    frames++;
    if (frames % 100 == 0) {
        report(frames);
    }
}
