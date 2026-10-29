# Bug follow-ups

Bugs found playing btn in the browser (2026-10-26), what each one is, and
the fix we want. §2 and §3 are fixed and kept as history. §1 and §4 are
open, tracked in [`open-items.md`](../open-items.md).

## 1. `void` functions that return a value

**What happens.** GCC compiles `void f() { g(); }` and
`int f() { return g(); }` to the same bytes: `g`'s result is left in v0
and `f` returns with it. A matching decomp can therefore declare `void` a
function whose callers use its result. On the PS1 that works. In wasm a
`void` function returns nothing, and the o32 entry the linker gives it
answers v0 = 0.

**Seen in btn.**
- New game skipped the mayor's arrival event, leaving the farm in a
  half-finished event state. `ovl_11_func_800DBC04` is defined
  `void` but `ovl_11_func_800F12D0.c` declares it `void *` and uses the
  request slot it returns (`ovl_11_func_800DBD78`'s result). Read as
  NULL, request 116 was never queued. Fixed in btn by b37ae28: it is now
  `s16 *Ovl11ReserveFirstRecordSlotIfAbsent`, declared that way
  everywhere.
- The opening flashback froze after the kid walks. The director program
  waits at step 0x0E on opcode 82, whose handler
  `ovl_11_func_800EADB8` is `void` in C but is called through
  `gOvl11ObjectOpcodeHandlers`, typed `s32 (*)(s16, s16, s32, s32)`, and
  returns `ovl_11_func_800E5704`'s "arrived" result. It never read 1.
  Still `void` at b37ae28.

**Current state: open.** `36764d8` routes these functions to the
translated tier (`build.json` `void_results`: 62 in btn at 3a15992, 54 at
b37ae28). It works, but it hides a decomp defect by swapping code. The
replacement (a psxw-cc normalizer rule, then btn-decompilation fixes) is
in [`open-items.md`](../open-items.md), Bugs.

## 2. Entities don't collide or respond

**What happens.** On the farm, the player walks through stones, weeds and
sticks, can't pick up the dog and can't talk to NPCs. Scenery collision
works (buildings, the doghouse, doors), and dialogs work, including an
NPC's line when a door they guard is tried. Not caused by the skipped
opening: it persists with the opening playing.

**Cause: a linker bug.** The decomp declares the collision engine two
ways. `collision_query_budget.c` passes the collision tag (a 4-byte
struct) by value to `ovl_11_func_800BEB28`, whose definition takes that
argument as `s32`. On the PS1 both are the same o32 call: the struct's
bytes travel in a1. clang's wasm ABI passes a by-value struct as a
pointer to a copy, which is also an i32, so the wasm types matched and
the linker called directly. The engine then read a stack address as the
tag (`query.object = &arg1; ... query.object->field_2`), matched no entry
in the 0xC5-entry item pool (weeds, stones, sticks) and no actor, while
the map probe, which ignores the tag, kept working.

**Fix (linker, `e3f65f1`).** "C calls C directly when the signatures agree" now
also compares clang's C-level kinds: a struct by value (or hidden
struct return) on one side only, or variadic on one side only, sends the
call through the o32 boundary, which converts exactly as the PS1 does.
Unprototyped declarations and narrow-integer differences still call
directly. Each such edge is listed in `build.json` `abi_mismatches`;
btn had one at 3a15992.

**Decomp follow-up: done in btn by b37ae28.** Both sides now declare
`Ovl11DispatchCollisionProbes` (was `ovl_11_func_800BEB28`) with the tag
by value, and `abi_mismatches` is empty. The linker rule stays for other
decomps.

## 3. Loading and saving hang at "Searching the MEMORY CARD."

**What happens.** CONTINUE on the title screen, then a slot: the game
shows "Searching the MEMORY CARD." forever (the frame counter stops;
vblanks go on). Saving hangs the same way. The emulator, on the same
replay (`games/btn/replays/card-load.toml`), says "No loadable diary in
MEMORY CARD slot 1." within about 120 frames.

**Cause: a runtime BIOS gap.** libmcrd's `MemCardGetDirentry` clears the
card events, calls `firstfile`, and when it finds nothing spins in
`_get_card_event_x` until a low-level card event (F0000011h) says why.
The PS1 BIOS checks the card before every directory function (open,
firstfile, erase, rename; OpenBIOS `buDevInit`: a synchronous read of
frame 0), and the driver delivers that read's event during the call. Our
`firstfile` answered from the image and delivered nothing, so any search
that matched no file, such as an empty card or a save that doesn't exist
yet, waited forever. Saves found by the search returned before the wait,
which is why the libmcrd conformance scenario never hit it.

**Fix (`psx-bios::card`, `ad39b70`).** The `bu` functions make the BIOS's check
and deliver its events: 04h, 100h with no card, and 2000h then 04h for a
new card, which the check accepts. The event split now follows OpenBIOS
and what libmcrd waits on. Raw `_card_read`/`_card_write` and every file
frame deliver only F0000011h; F4000001h comes from `_card_info`,
`_card_load`, `card_write_test` and the end of asynchronous file I/O.

## 4. Smaller findings from the same session

Both open, in [`open-items.md`](../open-items.md):
- **Lifted SDK asm that reads temporaries on entry** (Bugs);
- **Emulator title-menu timing** (Blocked on CD timing).
