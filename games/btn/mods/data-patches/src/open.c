#include "common.h"
#include "mod.h"

/* The executable's message window phase 3 handler; the game calls it
 * through gMessageWindowHandlers with the window work as its argument. */
s32 OpenMessageWindow(void);

static int opened;

/* Stored over gMessageWindowHandlers[3]: a mod function the game calls
 * through its own table, at a synthetic address. */
s32 logged_open(void *work) {
    if (opened++ == 0) {
        psxmod_log("message window opened");
    }
    return OpenMessageWindow();
}
