# Bug follow-ups

Bugs found playing btn in the browser (2026-10-26), what each one is, and
the fix we want. Open toolchain tasks that aren't bugs stay in
[`follow-up-tasks.md`](follow-up-tasks.md).

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
  NULL, request 116 was never queued.
- The opening flashback froze after the kid walks. The director program
  waits at step 0x0E on opcode 82, whose handler
  `ovl_11_func_800EADB8` is `void` in C but is called through
  `gOvl11ObjectOpcodeHandlers`, typed `s32 (*)(s16, s16, s32, s32)`, and
  returns `ovl_11_func_800E5704`'s "arrived" result. It never read 1.

**Current state: a stopgap to remove.** `36764d8` routes these functions
to the translated tier (`build.json` `void_results`, 62 in btn). It works,
but it hides a decomp defect by swapping code. Replace it with:

1. **A normalizer rule in psxw-cc** (like the existing GCC-idiom rules).
   For a C-defined `void` function whose original leaves its last
   callee's v0 at every exit (lifter fact `returns_callee_v0`) and whose
   result something reads (a C caller declares a result, translated code
   reads v0 after the call, or its address is taken), rewrite the
   definition before clang sees it: the return type becomes the callee's,
   and each exit's trailing call becomes `return <call>;`. The census
   counts it like any rule. If a flagged function can't be rewritten
   (exits that don't end in the call), the build fails with the function,
   the conflicting declarations and the evidence. No silent fallback to
   the translated tier. Then drop the tiering half of `36764d8`, keeping
   the detection.
2. **Fix btn-decompilation.** Change each flagged definition to return its
   callee's value, and make the conflicting prototypes agree (the code
   still matches byte for byte). Bump the pin. After that the rule applies
   nowhere in btn and stays for other decomps.

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

**Fix (linker).** "C calls C directly when the signatures agree" now
also compares clang's C-level kinds: a struct by value (or hidden
struct return) on one side only, or variadic on one side only, sends the
call through the o32 boundary, which converts exactly as the PS1 does.
Unprototyped declarations and narrow-integer differences still call
directly. Each such edge is listed in `build.json` `abi_mismatches`;
btn has one.

**Decomp follow-up.** The two declarations should agree (the comment in
`collision_query_budget.c` keeps them apart on purpose). Either spelling
matches; calling through an incompatible declaration is undefined in C.

## 3. Smaller findings from the same session

- **Lifted SDK asm that reads temporaries on entry** (`_spu_pitch2note`
  t1/t2, `func_80037470`/`94` t0–t2, `func_8003A2A4` t3): their callers'
  values don't reach them. Audit each.
- **Emulator title-menu timing:** the menu appears about 10 counter steps
  later on the emulator than natively, so a press recorded right after it
  appears natively is lost there; loads also tick the counter differently
  (D48). Replays recorded in the page don't steer the emulator through the
  title menu.
