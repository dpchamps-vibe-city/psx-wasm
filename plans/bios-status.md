# Native BIOS: remaining work

**Status:** the native recovery is merged (`ceefc20`); the full BIOS is not
done. The per-call source of truth is `support()` in
`crates/psx-bios/src/bios.rs` (working, partial, unsupported, fatal, noop),
which every build reports. The recovery's history is in
[`completed/salvage-bios-work.md`](completed/salvage-bios-work.md).

## Scope

- Port the BIOS *software* to Rust over the existing native backends (disc,
  card, GPU, input, scheduling). No serial exchanges, controller command
  sequences or instruction-count timing inside the BIOS.
- Cover the retail PS1 BIOS's callable services, internal table entries
  included. Not the boot-menu GUI, the devkit PC file server or PS2-only
  services.
- Static compilation only: `Exec`/`LoadExec` for code in the build, never
  arbitrary new machine code.
- Don't invent work: `atof`/`strtod` are fatal on retail (no COP1),
  `FlushCache` has no cache to flush, and documented zero-return entries
  stay zero-return.
- Significant new architecture (control flow, threads, exceptions) needs
  the user's decision before code. Keep Asyncify threading.

## Unsupported: the call errors

| Entry | Function | Blocker / next step |
|---|---|---|
| A(13h), A(14h) | `setjmp` (partial), `longjmp` | **Deferred by the user.** A continuation entered as an ordinary call doesn't unwind the frames between `longjmp` and `setjmp`, so it returns to the wrong caller. When resumed: target ownership across recursion and threads, register capture, frame cleanup. |
| C(19h), A(B2h) | `_ioabort`, `ioabort_raw` | Depend on `longjmp`. |
| A(43h), A(51h) | `Exec`, `LoadExec` | Always error. Support build-known executables: arguments, stack, GP and return. `Regs` exposes only SP/GP. |
| A(A0h) | warm boot | Always errors. Restart over the existing boot facilities. |
| A(52h) | `GetSysSp` (PsyQ's binding) | psx-spx conflicts on the body; don't guess a stack address. |
| B(51h), B(53h) | `Krom2RawAdd`, `Krom2Offset` | Needs a legally usable glyph resource (the user's call). B53's mapping needs evidence: OpenBIOS's lookup aliases valid 899Eh/899Fh codes and was rejected. |
| C(09h) | `SysInitKernelVariables` | Verify the callable behaviour (reference notes post-boot corruption). |
| C(15h), C(17h), C(18h) | `_cdevinput`, `_circgetc`, `_circputc` | Built-in console input and circular buffers. |

## Partial: works, with recorded gaps

Public wrappers (`open`, `read`, `gets`, …) inherit these.

- **Configuration and identity.** A(9Ch) `SetConf`: a live non-main-thread
  reset is blocked. A(B4h) `GetSystemInfo`: BIOS date/version undecided; no
  fabricated values.
- **Threads.** Switching from callbacks or interrupts is rejected; TCB
  context is only partly editable from outside.
- **Exceptions and hooks.** C(06h) hook chaining is rejected. `break` only
  logs; it doesn't deliver the unresolved-exception event or entry/return
  path.
- **Memory card.** A(A7h–AAh, AEh) `bufs_cb_*`: A8/A9/AA end pending
  Info/Load notifications; AE's body and unspecified returns are
  unresolved. A(AFh) write-test notifications are unverified (OpenBIOS has
  no body, which is evidence neither way).
- **Console.** Built-in blocking TTY reads error; host input and low-level
  TTY control are missing. C(16h) `_cdevscan` scans guest devices only.
- **CD.** INT5 and chain-control services; path-table and disc-identity
  checks; status checks in file operations other than read; native
  shell-open/acknowledgement. An early read ERR can still meet a busy
  `SeekL` after a returning `SystemError` patch.

## Validation gaps

- **BIOS oracle:** no non-empty comparison yet; images, vectors and goldens
  are missing.
- **Saves:** game-level save/reload persistence needs the human's recordings
  and state assertions; `card-load` has no state assertions.
- **Full CI:** last run at the ABI-17 foundation (`a74f832`), both games
  exit 2: missing oracle prerequisites, BTN mods census inputs, two BTN
  difftest mismatches. Not rerun on current source.
- BTN mods census inputs and GPU vectors/goldens are missing for the
  browser gates.
