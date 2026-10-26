# M1 — Translator and linker core

**Status:** done (2026-10-26)
**Depends on:** M0. **Unlocks:** M2 (and M8's SDK translation).
**Design refs:** design §1 (address space), §2 (function pointers), §4
(implementations), §8 (linker); decisions D5, D6, D8–D11, D17.

## Goal

Make the CPU side complete and verified before any device work:
- **`psxw-lift`** translates any function of a container's original image
  into generated C.
- **`psxw-link`** (core) places code and data in the PSX address space and
  rewrites guest code for the window.
- **A function-level differential harness** proves translated code equals
  decompiled C, function by function.

The oracle is the original code: the decomp is byte-matching, so its C and
the original MIPS define the same behaviour.

## Context

- M0 produces per-TU wasm objects for all 1,802 btn TUs at
  `out/census/btn/<sha7>/obj/<container>/<stem>.o`. It also produces lift
  markers (`__psxw_lift_<name>`, whose data is the reason string) and the
  address map.
  - The address map comes from `psxw_inputs::AddressMap`: containers, TUs,
    SDK placements and symbols, with auto-name resolution and `sym+off`
    labels.
- Original images come from `psxw_inputs::originals::Originals`, keyed by
  container with guest-address reads; the bytes are verified by splat
  `sha1`.
- Memory layout constants are in `psxw-abi`: `WINDOW_BASE`, `WINDOW_MASK`,
  `phys::*`, `table_index`, `TABLE_LEN` and `indirect_abi`.
- The object reader exists: `psxw-link::object` (`WasmObject`,
  `classify_name`). It must be extended to code relocations and function
  bodies.
- **Census findings to handle here:**
  - 32 variadic signature thunks (`FntPrint` / `KanjiFntPrint`);
  - 43 multiply-defined tentative data addresses;
  - 61 anonymous data symbols;
  - host support functions `abs`, `labs`, `fabs`.

## Scope

**In**
- MIPS-I (R3000) decoder and instruction semantics, including COP0, COP2,
  `lwl`/`lwr`/`swl`/`swr`, `mult`/`div`, branches, jumps, `syscall` and
  `break`.
- Function discovery from the address map. CFG with delay slots,
  load-delay modelling, and jump-table recovery.
- C code generation: the o32 form, intrinsics, the BIOS-call pattern, and
  hardware-access detection.
- Prototype facts from `psxw-cc`: parameter kinds, structs by value and
  their sizes, variadics.
- **Linker core:**
  - symbol resolution;
  - data placement (named data at original addresses, merging of tentative
    definitions, the AUX allocator for anonymous data);
  - code relocation patching;
  - function pointers as MIPS addresses;
  - the guest dispatch table (table 1);
  - adapters between C and o32 code;
  - guest passes (masking, guest stack pointer, intrinsic lowering,
    indirect-call rewriting);
  - wasm validation; `wasm-opt`.
- **`psxw-difftest`:** builds harness modules and runs them in wasmtime.
- Pinning binaryen (`wasm-opt`) in `inputs.lock`.
- `psxw ci` (initial: census plus difftest).

**Out**
- the runtime and devices;
- overlay dispatch stubs, the full-game link and boot (M2);
- the SDK cache from relocatable members (M8);
- full-state translation mode, unless the data shows it's needed;
- GTE semantics (COP2 is recorded as call-outs in the harness).

## Deliverables

- `crates/psxw-lift`:
  - `decode`: instruction decoding;
  - `discover`: function extents, jump tables;
  - `cfg`;
  - `emit`: generated C;
  - `psxw_rt.h`: the header the generated C includes, with intrinsics and
    memory macros.
- `crates/psxw-cc`:
  - a `protos` module that writes `<stem>.protos.json` for each TU;
  - compiling generated C through the same pipeline.
- `crates/psxw-link`:
  - `object`, extended with code sections, relocations and function types;
  - `layout`: placement, AUX allocator, tentative merge;
  - `emit`: wasm-encoder;
  - `passes`: mask, guest SP, intrinsics, call_indirect;
  - `adapters`;
  - `table`.
- `crates/psxw-difftest`: the harness builder, a wasmtime runner,
  comparison and the report.
- CLI:
  - `psxw lift <game> [--function F | --container C | --all]`
  - `psxw link-test <game> --function F` (debug)
  - `psxw difftest <game> [--only S] [--seeds N]`
  - `psxw ci <game>`

## Design details

### Function discovery (`psxw-lift::discover`)

- **Function starts** come from the union of symbol-file entries with
  `type:func` in the container's vram range, the lift markers and
  C-defined names (resolved through the address map), and SDK placement
  symbols.
- **A function's end** is the next function start in the container, or
  start + `size` when the symbol file gives one. Interior names (inside a
  sized extent) are entry points within that function, not separate
  functions.
- **Code words** are read from `Originals` at guest addresses. A function
  whose words aren't available is an error.

### Jump tables

- **Recognize the GCC/ASPSX pattern:** `sltiu t, idx, N` / `beqz t,
  default`, then `sll`, the table address loaded with `lui/addiu`, then
  `addu`, `lw`, `jr`.
- **Table entries** are read from the original image (N words at the table
  address). All targets must lie inside the function.
- **Any other `jr reg`** whose value isn't `ra` and isn't a recognized
  table is a tail call through a function pointer, emitted as an indirect
  call through the dispatch table followed by a return. Report every such
  site.
- **Fail loudly.** An unbounded or unrecognized table is a translation
  error listing the function and pc. Never guess.

### Semantics and code generation (`emit`)

- **One C function per guest function:**
  `uint64_t psxw_t_<ADDR>(uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3)`.
  The address is in hex, uppercase, 8 digits. Overlay functions add a
  container prefix (`psxw_t_ovl_11_<ADDR>`), because addresses collide
  across overlays.
- **Registers are locals** `r1..r31`, plus `hi` and `lo`. `r0` reads as 0
  and writes are discarded. `a0`–`a3` are initialized from the parameters.
  `sp` is read with `psxw_sp_get()` at entry and written back with
  `psxw_sp_set()` before calls and at returns.
- **Control flow** is labels and `goto`, one label per basic-block leader,
  with `#line` directives pointing at `<container>:0x<pc>`.
- **Branch delay slots.** The branch condition and target are evaluated
  *before* the delay-slot instruction executes; then the slot runs; then
  the branch is taken. If the slot writes a register the condition reads,
  the condition is captured first. For likely-style patterns (none in
  MIPS-I), use an explicit nop.
- **Load delay.** If the instruction after a load reads the loaded
  register, it sees the old value. Emit a temporary holding the old value
  for that one instruction. A load followed by a write to the same
  register in the slot follows R3000 rules (the later write wins); model
  it explicitly and add a unit test.
- **Arithmetic.**
  - All values are `uint32_t`. Signed comparisons cast to `int32_t`.
    Shifts mask with `& 31`.
  - `mult`/`multu` use 64-bit products into `hi`/`lo`.
  - `div`/`divu` follow the hardware results for division by zero and for
    `0x80000000 / -1` (psx-spx), and must never trap in wasm.
  - `add`/`addi`/`sub` overflow calls `psxw_trap_overflow(pc)`.
    `addu`/`addiu`/`subu` wrap.
- **Memory.** Macros `PSXW_LB/LBU/LH/LHU/LW/SB/SH/SW(addr)` cast the guest
  address to a pointer. The linker's masking pass makes them window
  accesses. `lwl`/`lwr`/`swl`/`swr` are byte-merged exactly per the MIPS
  rules (little-endian). Accesses use `align=1` loads, so alignment never
  traps.
- **COP2** goes to intrinsics `psx_gte_mtc2(r, v)`, `psx_gte_mfc2(r)`,
  `psx_gte_ctc2`, `psx_gte_cfc2`, `psx_gte_exec(cmd)`. `lwc2`/`swc2` become
  memory plus mtc2/mfc2.
- **COP0.** `mfc0`/`mtc0`/`rfe` call `psxw_cop0_read(r)`,
  `psxw_cop0_write(r, v)`, `psxw_rfe()`.
- **`syscall`** calls `psxw_syscall(a0, pc)`; **`break`** calls
  `__psxw_break(code)`.
- **BIOS calls.** The pattern `li t2, 0xA0|0xB0|0xC0` … `jr t2`, with
  `t1 = n`, possibly in the delay slot, becomes
  `v0 = psxw_bios(table, n, a0, a1, a2, a3)` followed by a return. Also
  recognize `jal 0xA0` and the like.
- **Calls.**
  - `jal T` where T is translated: a direct call `psxw_t_<T>(a0..a3)`.
    Unpack `v0`/`v1` from the result.
  - `jal T` where T is C (decompiled), or has an adapter: call
    `psxw_o32_<name>(a0..a3)`, which the linker resolves to an adapter.
  - `jalr` / `jr reg`: `psxw_call_indirect(addr, a0..a3)`.
  - `jr ra`: return `v0 | (uint64_t)v1 << 32`.
  - `j T` to another function's entry: a tail call
    `return psxw_t_<T>(a0..a3)`.
- **Hardware access.** A constant-folded address in
  `0x1F801000–0x1F802FFF` or `0xBF801000–0xBF802FFF` is a *translation
  error*, unless the function is on the adapter allowlist. Its report entry
  is the evidence the per-member classification uses later. Dynamic
  addresses are caught at run time by a debug-mode masking check (M2).
- **Header.** `psxw_rt.h` declares every intrinsic. The linker lowers the
  pseudo-intrinsics: `psxw_sp_get/set` become `global.get/set $__guest_sp`,
  and `psxw_call_indirect` becomes a uniform `call_indirect` on table 1.
  Others are runtime imports, stubbed as call-outs in the harness.

### Prototype facts (`psxw-cc::protos`)

From the preprocessed, normalized text (tree-sitter), for every function
definition and declaration in a TU, record:
- the name;
- each parameter's type text and kind (int, pointer, float, double,
  64-bit, struct by value with its size from the layout-probe machinery);
- the return kind (and struct-return size);
- whether it's variadic;
- whether it's K&R or unprototyped.

Written to `<stem>.protos.json` next to the object. The linker uses it for
adapters.

### Linker core (`psxw-link`)

- **Inputs:** relocatable wasm objects (decompiled C and generated C), the
  address map, prototype facts, and a mode (`harness` or `game`).
- **Symbols.**
  - Names resolve through `AddressMap::resolve`.
  - Function names follow `classify_name`: `__original_main` → `main`;
    clang `main` wrappers are dropped; `.L<f>_bitcast` thunks become
    adapter requests.
  - A function or data name with no address is a link error (M0 found
    none).
- **Data placement.**
  - Named data goes at its resolved guest address. In the module that
    means an active data segment at `WINDOW_BASE + (addr & WINDOW_MASK)`,
    only for C-defined *initialized* data; bss isn't emitted.
  - Tentative definitions from several TUs at one address merge into one;
    sizes must agree, else error.
  - Anonymous data (`.L*` and `f.var` statics) goes to the AUX allocator,
    which starts at guest `0x8020_0000`, i.e. window offset `0x200000`,
    8-byte aligned.
  - Generated C defines no data.
- **Relocations in code and data:**
  - `MEMORY_ADDR_*` → the guest address (`0x80xxxxxx` form) + addend;
  - `TABLE_INDEX_*` → the function's MIPS address;
  - `FUNCTION_INDEX_LEB` → the final function index;
  - `TYPE_INDEX_LEB`;
  - `GLOBAL_INDEX_LEB` (`__stack_pointer` → `__guest_sp`);
  - `TABLE_NUMBER_LEB` → table 1 for guest `call_indirect`.
- **Re-encoding.** Function bodies are decoded with `wasmparser` operators
  and re-encoded with `wasm-encoder`, applying the passes during
  re-encoding:
  1. **Masking.** Every guest load, store, `memory.copy` and `memory.fill`
     address becomes `((addr + offset) & WINDOW_MASK)`, with the memarg
     offset set to `WINDOW_BASE`. Fold the original offset into the add
     only when it's non-zero.
  2. **Guest SP.** `__stack_pointer` becomes `__guest_sp`, a mutable i32
     global initialized by the runtime (harness: a fixed stack top).
  3. **Intrinsics** are lowered as described above.
  4. **C `call_indirect` sites** with arbitrary signatures are rewritten to
     call a generated helper `psxw_icall_<sig>`. It marshals the arguments
     into o32 form (spilling beyond four words to the guest stack, with
     the 16-byte home area reserved), calls the dispatch table, and
     converts the result back.
- **Adapters.**
  - **C → o32 callee:** for each generated or adapter function referenced
    from C under some wasm signature, generate a wrapper with that
    signature that marshals into o32 (64-bit values in even-aligned pairs;
    floats as their bit patterns, since the game is soft-float) and calls
    `psxw_t_*`.
  - **o32 caller → C callee:** a `psxw_o32_<name>` wrapper unpacks
    `a0..a3` and stack words per the C function's wasm signature.
  - **Structs by value** (from prototype facts) copy their bytes between
    o32 slots and a temporary.
  - **Variadic calls,** whose arg count is unknown, copy a fixed number of
    words (`a1..a3` plus 5 stack words) into or out of clang's va-buffer.
    Extra words are harmless.
  - **Every table entry has the uniform type** `(i32,i32,i32,i32)->i64`.
    C functions enter the table through their o32 wrapper.
- **Dispatch table.** Table 1 has `TABLE_LEN` entries. Every function with
  an address gets `table[table_index(addr)]`, either its generated function
  or the o32 wrapper of its C function. Empty entries hold a trap stub that
  calls `psxw_untranslated(addr)`.
- **Output.** Validate with the `wasmparser` validator, then run `wasm-opt`
  (pinned binaryen) at `-O2` in harness mode and `-O3` in game mode.

### Differential harness (`psxw-difftest`)

- **What it tests:** for each function F with both decompiled C and
  original code, in the same container, F_c (C, renamed) against F_t
  (translated).
- **The module.** One harness module per container shard, containing many
  (F_c, F_t) pairs plus shared call-out stubs. Every other callee, BIOS
  call, `syscall`, `break`, COP0 and COP2 intrinsic resolves to a
  *call-out recorder*:
  - it logs the kind, target address or id, `a0..a3` and the first 8
    guest-stack words;
  - it returns `v0`/`v1` from a per-run seeded sequence, the same for both
    sides.
- **Initial state per run:**
  - guest RAM is the container's original image at its load address, plus
    the EXE image when F is in an overlay, with BSS zeroed;
  - `__guest_sp` is a fixed stack top (`0x801F_F000`);
  - `a0..a3` come from a seeded generator mixing small integers, data
    symbol addresses from the address map (weighted towards symbols F
    references, found from relocations or constant addresses), and
    pointers into a scratch arena in the AUX region filled with seeded
    bytes.
- **Execution.** Run F_c on memory copy A and F_t on copy B under wasmtime
  with fuel limits. Both running out of fuel counts as "both diverge",
  recorded, not failed.
- **Comparison:**
  - the return value, masked to the declared return kind from prototype
    facts;
  - RAM byte for byte, excluding the stack below the entry `sp` (the
    callee's own frame) and the call-out scratch;
  - the call-out log sequence, including arguments.
- **Report.** For each function: pass, mismatch (first difference,
  symbolized address), trap, or fuel. Mismatches are triaged as:
  - **translator bug:** fix it;
  - **clang UB in the decompiled C:** record the evidence, then either
    rewrite with a normalizer rule if the idiom is general, or pin the
    function to `lift` in `game.toml` with a reason;
  - **legitimate nondeterminism** (an uninitialized local in the original):
    record the evidence and exclude it from comparison for that function
    only, with the reason in the report.
- **Seeds.** 64 by default. The report lists the seed set.

## Tasks

1. [x] Pin binaryen in `inputs.lock` as `[toolchain.binaryen]` (release
   tarball, sha256) and extend `psxw toolchain fetch`.
2. [x] `psxw-lift::decode`: the full MIPS-I and COP0/COP2 decoder, with a
   table test of encodings for every opcode.
3. [x] Spec-derived unit tests for semantics edge cases (debug aids, not
   the oracle):
   - div/divu by zero, `INT_MIN / -1`;
   - `lwl`/`lwr`/`swl`/`swr` at all alignments;
   - load delay, delay slots;
   - add overflow.
4. [x] `discover`: function extents, interior entries, jump-table
   recognition with failure reporting.
5. [x] `emit` plus `psxw_rt.h`, generating C for one function. Compile it
   through `psxw-cc` (the normalizer isn't needed, but use the same flags).
6. [x] `psxw lift btn --all`: translate every function in every container
   (game and SDK ranges). Exit criterion 1.
7. [x] `psxw-cc::protos`: prototype facts per TU, unit-tested on census
   TUs.
8. [x] `psxw-link::object`: code section, function types, code
   relocations.
9. [x] `psxw-link` layout: data placement, tentative merge, the AUX
   allocator.
10. [x] `psxw-link` emit plus passes: mask, guest SP, intrinsics, indirect
    calls. Test each pass on handcrafted objects.
11. [x] Adapters (C↔o32, structs by value, variadic) and the dispatch
    table.
12. [x] `psxw link-test btn --function F`: link one pair plus call-outs;
    validate; run once.
13. [x] `psxw-difftest`: generator, runner, comparison, report;
    `psxw difftest`.
14. [x] Run the full difftest; triage every mismatch to zero unexplained.
15. [x] Full-state mode decision. If any function in the boot path needs
    non-ABI register passing (detected by entry-register-read analysis),
    implement it; otherwise record that it isn't needed yet.
16. [x] `psxw ci btn`: census plus difftest. Update docs and the README
    status.

## Exit criteria (all automated)

1. `psxw lift btn --all` translates every function in every container
   with zero errors. The report lists counts per container, jump tables
   found, indirect tail calls, and any hardware-access sites (which must
   all be in SDK functions destined for adapters).
2. `psxw difftest btn --seeds 64` passes all 2,584 functions with both
   forms, or every non-pass carries a recorded root cause and disposition
   (fixed, normalizer rule, tier pin with reason, or documented
   exclusion). Zero unexplained.
3. Every harness module passes the wasm validator. `wasm-opt` succeeds.
4. `cargo test --workspace` passes, including decoder and semantics tests.
5. `psxw ci btn` runs criteria 1–4 from a clean checkout of the pinned
   inputs.

## Risks and decision points

- **Clang UB in decompiled C** (uninitialized locals steering register
  allocation, out-of-bounds struct indexing). Expect a tail of mismatches.
  Prefer general normalizer rules over pins.
- **Jump-table variants in hand-written SDK asm.** Fail loudly and add
  patterns with evidence.
- **Variadic adapters are approximate by design.** They're valid because
  the o32 callee reads only what its format string asks for.
- **Harness throughput.** 2,584 × 64 runs. Instantiate once per shard and
  reset memory by copying the image.
- **Full-state mode** may be needed for libgte or libapi internals. Decide
  by data in task 15.

## Progress log

- **2026-10-26 (`de56b5a`): translator.**
  - binaryen `version_133` is pinned.
  - `psxw-lift` covers decode, discovery to a fixed point, delay-slot-aware
    control flow, jump tables, constants/liveness/load-delay analysis,
    o32-form C emission and `psxw_rt.h`.
  - `psxw lift btn --all`: 3,359 functions, all compiling.
- **2026-10-26 (`ef75c1e`): linker and differential harness.**
  - `psxw-cc::protos` reads prototype facts from clang's IR.
  - `psxw-link`: full object reader, layout (named, content-matched,
    AUX), rewrite passes, o32 marshaling, harness-mode link, GCC
    conventions.
  - `psxw-difftest`: shards, wasmtime runner with dirty-page snapshots,
    seeded ABI-shaped inputs, comparison, and `--verbose` explanations.
  - Triage of all mismatches into rules and fixes (see Findings).
- **2026-10-26: gate and documentation.**
  - R3000 semantics tests (`tests/semantics.rs`) and linker pass tests
    (`tests/link_passes.rs`).
  - `psxw link-test`, and `psxw ci btn` (inputs, census, lift, difftest,
    unit tests, oracle): PASS in 38 s.
  - Design, decisions (D33–D37) and README status updated.

## Findings

- **Exit numbers.**
  - Lift: 3,359 functions, 164,741 instructions, 178 jump tables, 3
    indirect tail calls (all SDK), 208 indirect calls, 63 BIOS calls; 21
    fall-throughs into the next function.
  - I/O sites: in `libpad/pdresres`, `libmcx/mcxmain` and `libmcx/mcxdsr`.
  - BIOS-kernel sites: in `libapi/first`, `libcard/patch` and
    `libmcx/mcxdsr`.
  - Difftest at 64 seeds: 2,567 pass, 3 excluded with evidence.
- **Full-state mode is not needed (task 15).** Inline expansion covers
  register passing (D33): 13 functions inline a callee.
- **Mismatch classes and their dispositions**, all general unless noted:
  - **Global register variables** (`CAPTURE_PREV_RET`, a v0 channel
    across calls) and **register-capture locals:** lifted (D35).
  - **`-Wreturn-type` functions** (the original returns the leftover
    v0): lifted.
  - **Soft-float:** one C function did double arithmetic; the original
    calls the game's `__adddf3`/`__floatsidf`/…, so it was lifted.
  - **Clang behaviour that changes the call sequence:** inlining,
    deletion of calls to side-effect-free functions, IPSCCP, and
    tail-recursion elimination. Handled by the harness's
    function-at-a-time compile (D34). These clang transformations remain
    legal in the game build.
  - **GCC 2.95 builtins:** `memcpy` with a constant size was expanded
    inline, so the linker reproduces it (D36).
  - **Division by zero:** GCC/ASPSX checks it with `break 7` and
    continues with the R3000's results; clang traps. The linker
    reproduces the checks (D36). Clang may move or merge a trapping
    division, so `break` events compare as a set.
  - **Bug:** splat auto-name resolution matched clang's
    `.Lswitch.table.<func>_<ADDR>`. Auto-names are now C identifiers only.
  - **Bug:** jump-table recovery lost the index when a conditional path
    set it to a constant; shifts no longer constant-fold in the window.
  - **Harness bugs fixed:**
    - the guard page past the window wasn't tracked;
    - inline copies didn't mark their pages;
    - adapter stack writes weren't restored between sides.
  - **Excluded (3):** local-array UB reachable only with inputs outside
    the function's contract (`Rand(n)` answered out of range, impossible
    asset data). See `games/btn/game.toml [difftest.exclude]`.
- **For M2 (copied to its Context):**
  - **gp:** `__start` sets gp to `0x8005E274` (from code, not splat).
  - **Narrow arguments:** the decomp's prototypes sometimes differ in
    width or signedness from what GCC compiled against (a `u16` param the
    original passes sign-extended). At C→translated edges the callee may
    see differently-extended arguments. Expect `ramdiff` to catch any that
    matter.
  - **Call naming:** translated code calls `psxw_f_<ADDR>` (and
    `psxw_d_<ADDR>` from the EXE into overlay slots); the game link binds
    each to a tier.
  - **Missing link features:** the dispatch table, imports, and Rust
    objects (harness mode has none). Overlay function pointers are MIPS
    addresses that collide across overlays, so the dispatch table needs
    the overlay stub.
