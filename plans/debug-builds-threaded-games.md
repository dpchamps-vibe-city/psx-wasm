# Debug builds of games that switch threads: problem statement

**Status:** problem statement; [findings and open feasibility questions](debug-builds-threaded-games-investigation.md)
recorded, no implementation plan selected.
**Design refs:** [docs/design.md](../docs/design.md) D53 (debug builds),
D57 (guest threads).

## The problem

A debug build can't be made for a game that switches threads. bof4 is
such a game, so there is no debug build of bof4:

```
$ psxw build-web bof4 --debug
psxw: debug builds of games that switch threads aren't supported (D57): Asyncify drops the DWARF
```

The check is in `crates/psxw-build/src/game.rs` (`prepare`). It refuses
`--debug` whenever the game's translated code makes a ChangeThread call
(B0h:10h).

## Why it happens

- **Debug builds (D53)** compile the C with `-g`. They merge the guest
  with the runtime in Rust, not with binaryen, because binaryen can't carry
  DWARF through a merge, and they keep the DWARF. The browser's DevTools
  then shows the decomp's C and the generated C for each function, with
  variables, at the game's own addresses.
- **Thread switching (D57)** is done by binaryen's Asyncify pass (pinned,
  version 133), run over the linked guest. It instruments the functions
  that can be on the stack at a switch. For bof4 that's about 13% of
  functions.
- For this target, the pinned clang's plain `-g` emits **DWARF 4**.
  Asyncify retains and relocates those debug sections, but can leave
  false variable locations after moving operand-stack values into
  temporary locals—even when the DWARF verifier passes. Explicit DWARF 5
  has a separate Binaryen compatibility failure; it is not the current
  default or the explanation for this correctness defect.
  See the [investigation](debug-builds-threaded-games-investigation.md)
  for the reproducer and upstream research. The diagnostic above
  overstates the problem: preserving valid DWARF is unsupported here,
  not inherently impossible.

## Who it affects

- **bof4:** no debug build, and no debug web bundle. The C isn't visible
  in DevTools, and neither are breakpoints by source line or variable
  inspection.
- **Any game that switches threads:** the same.
- **Not btn:** it switches no threads, and its debug builds work.
- **Script mods built against a debug base (D56)** aren't available for
  threaded games either. A moddable debug base is a debug build.

## What a fix has to respect

- **Threads must still switch exactly as they do now.** D57's scope rule
  decides which functions are instrumented, and the replays' timing and
  results depend on it (bof4's `title-to-field`).
- **The debug build's existing properties must hold:**
  - the game's own addresses;
  - the C's DWARF mapped to the merged module;
  - the sources served beside `game.wasm`;
  - the same code as the release build, apart from `-g`.
- **Inputs stay pinned.** The binaryen version is pinned in
  `inputs.lock`.
