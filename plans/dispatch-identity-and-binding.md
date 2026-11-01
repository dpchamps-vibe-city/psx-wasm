# Dispatch: code identity, runtime binding and translation validity

**Status:** problem statement and design discussion, not an approved redesign.
No implementation work or change to the supported execution model is authorized
by this document.

**Scope:** a general compiler/static-recompiler problem, not a porting recipe
for an individual game. Input-specific facts belong in declarative input
metadata; compiler rules must not be fitted to particular games.

Related:
- [Architecture](../docs/emulation-layers.md): SDK → adaptor → native backend;
  the host integrates with the backend. This is not a PlayStation CPU emulator.
- [Design](../docs/design.md): D5 preserves guest addresses, D6 preserves guest
  function-pointer values, D7 selects overlays using resident code words.
  The D69 residency redesign remains deferred.
- [Dispatch conformance](dispatch-conformance.md): executable evidence and
  outstanding findings. Red tests do not authorize architectural changes.

## The problem in one sentence

We compile different functions into separate Wasm functions, but the original
program can refer to them using the **same guest address at different times**.
The compiler/runtime must preserve that changing meaning without guessing an
incompatible compiled implementation.

Four concepts must be kept separate:

| Concept | Question it answers |
|---|---|
| Function identity | Which function in which module? |
| Guest address | Where was code located in the original address space? |
| Binding | Which function should this reference invoke now? |
| Translation validity | Do the assumptions embedded in this compiled implementation hold for this execution? |

An address identifies a location. It need not permanently identify a function.
Likewise, knowing an image's identity does not automatically justify every
assumption made while translating it.

## Why loading works differently here

On the PlayStation, once executable writes are made visible, the processor
executes the instructions currently stored at the called address:

```text
Load module B at P → call P → execute B's instructions at P
```

Here, the MIPS bytes in the game-memory buffer are not what the host executes.
Both implementations already exist separately as compiled Wasm functions:

```text
BUILD TIME

Module A: function at P ───────────→ compiled function A
Module B: function at P ───────────→ compiled function B

RUN TIME

Load B at P ──→ change bytes in the game-memory buffer

Call P ───────→ must select compiled function A or B
```

Writing B's original bytes into the buffer does not replace compiled function A
with compiled function B. We have separated executable code from its original
storage location, while retaining calls that name that location.

This is not specific to lifted MIPS. Decompiled C participates in the same
address-based interface today. Reaching full C coverage would not automatically
remove this requirement. A source-native representation with distinct function
identities would require transforming the relevant references and bindings too.

## What exists today

### Build-time identity already exists

`GameSpec.tiers` in `crates/psxw-link/src/game.rs` maps
`(container, address)` to an implementation: C, lifted code or an adaptor.
The linker therefore already distinguishes:

```text
("A", P) → compiled function A
("B", P) → compiled function B
```

`Resolved` in `crates/psxw-inputs/src/map.rs` can also carry the container whose
symbol file named a reference.

However, the game-mode linker's `func_addr()` returns the original guest
address. For function-address expressions, the representation becomes:

```text
Reference associated with A's function ──→ P
Reference associated with B's function ──→ P
```

This is intentional under D6, not merely an accidental loss of a name. C-made
pointers, pointers in original binary data and targets constructed by MIPS code
can then use the same numeric interface. Changing it requires handling all of
those producers and consumers.

Some known calls already link directly. Not every call goes through overlay
selection. The problem concerns references whose binding depends on residency,
and translations whose assumptions depend on more than the sampled code.

### Shared addresses use sampled code-word checks

`overlay_plan()` and `overlay_checks()` in `crates/psxw-build/src/game.rs`:

1. Group implemented functions by guest entry address.
2. For shared addresses, start each candidate's checks with its entry words.
3. If those checks do not distinguish another candidate, find an additional
   differing word in their intersecting text. The search prefers text at or
   after the entry before looking earlier in the container.
4. Emit the selected addresses and expected words into the link specification.

Only original text is used, not mutable data. Non-overlapping text cannot
reliably distinguish a newly loaded shorter overlay from stale bytes left by
an earlier longer one.

Illustrative code words, not game-derived input:

```text
Address       Module A       Module B
P             common         common
P + 4         common         common
P + 8         common         common
P + 12        0x24020011     0x24020022
```

The compiler can emit checks at P, P+4 and P+12. The outward search is **build-time
work**. At runtime, `Synth::OverlayStub` in `crates/psxw-link/src/game.rs` performs
ordinary Wasm memory loads and comparisons:

```text
Load B's bytes ──→ game-memory buffer at P
                              ▲
                              │ read selected words
                              │
Call P ──→ overlay selector ───┘
                  │
                  │ first candidate whose checks all match
                  ▼
           compiled function B
```

No matching candidate calls the untranslated diagnostic. Multiple matches are
currently settled by candidate order. There is no whole-function hash in this
mechanism. Additional probes cost generated code and memory reads/comparisons
when the stub runs.

## Why more distinguishing words are not a general solution

### 1. Known, distinguishable images

If the current state is one of the known images and their relevant text differs,
a small set of words can distinguish them. This fixes shared-prologue collisions
without validating every instruction on every call.

But the conclusion is only **“these samples match this candidate.”** It is not a
proof that every byte or translation dependency has the expected value.

### 2. Identical code can depend on runtime data

Consider identical original instructions implementing:

```text
target = function_table[index]
call target
```

```text
                   identical code
                         │
               read runtime table
                         │
              ┌──────────┴──────────┐
              ▼                     ▼
         target is X           target is Y
              │                     │
          execute X             execute Y
```

The correct translation can preserve this runtime choice. Ordinary mutable data
should affect the computation, not be mistaken for immutable overlay identity.

If compilation instead specialises the implementation using the original table
or a discovered target set, two translations may embody different assumptions
despite identical instruction bytes. Looking for another differing instruction
cannot necessarily distinguish those assumptions. The conformance suite's
`D02/jump-table-disjoint` finding is an example involving discovered jump targets.

Related dependencies include expanded callees and calls into independently
replaceable regions. Function identity alone does not prove a translation valid.

This is not a claim that static analysis is useless. Analysis may establish a
fixed target, prove equivalent implementations, or emit the required dynamic
choice. It must not invent a fixed binding where the program's binding changes.

### 3. Some candidates do not need distinguishing

If candidates have equivalent executable behaviour and equivalent translation
assumptions, sharing an implementation can be valid. Lack of a differing code
word is not, by itself, an error.

Conversely, identical sampled words—or even identical entry instructions—do not
establish that equivalence. Candidate order is not a correctness argument.

### 4. Mixed or modified states may match neither original image

A partial load or later store can leave all selected probes unchanged while
changing another relevant instruction. Sampling can then select a compiled
function that does not describe the current executable bytes.

Checks elsewhere in the container can also depend on text that is unrelated to
a particular completed function but has not yet arrived. Deciding what must be
supported in such intermediate states requires an explicit execution contract.

Hashing more bytes can detect additional changes, but it does not supply an
implementation for arbitrary new code or remove the need to preserve genuinely
dynamic behaviour. Per-call whole-code validation is not an approved response.

## Activation: move identification from calls to a binding event

An activation design would explicitly establish which known module is active at
a destination, then update the relevant bindings:

```text
                         LOAD MODULE B
                               │
                ┌──────────────┴──────────────┐
                ▼                             ▼
       update game-memory             establish activation
            contents                     of B at P
                                              │
                                              ▼
                                    current binding[P] = B
                                              ▲
                                              │ lookup
                                            call P
                                              │
                                              ▼
                                      compiled function B
```

```text
Today:       call → inspect bytes → infer identity → execute

Activation:  load/activation → establish bindings
             call → use binding → execute
```

This retains guest-address dispatch for unresolved-by-address calls. It removes
repeated byte-based identification; it does not eliminate every indirect call.
Stable bindings can still use direct calls where justified.

### Why the existing catalogue is only half of this

```text
Already known:                       Additional runtime fact:

Which functions does B contain?      Which operation made B active,
                                    at which destination, and when?
          │                                      │
          ▼                                      ▼
   function catalogue                      activation event
```

A symbol map answers the first question, not the second. Reads, copies and
unpacking can be stages in loading rather than completed activation. A generic
`activate(module, destination)` API alone does not establish where it is correct
to emit that event. A disc read alone is not a universal activation signal.

The documented D7 choice avoided requiring that information by inferring
residency from memory. Its rationale, “it's how hardware decides,” must not be
read as an equivalence: the hardware executes the resident instructions, whereas
our stub classifies sampled bytes and executes different, precompiled code.

“No per-game loader hooks” does not logically require “no activation mechanism.”
But a general activation contract still needs reliable information from supported
inputs, compiler analysis or suitable software boundaries. That contract has not
been established here. Arbitrary unobserved writes cannot update bindings by magic.

## Explicit references: preserve identity and distinguish late binding

A more explicit compiler representation would distinguish at least these
conceptual cases. This is a semantic model, not a proposal for a particular new
IR or handle encoding:

```text
                          code reference
                                │
              ┌─────────────────┼──────────────────┐
              ▼                 ▼                  ▼
       particular function   changing slot    unresolved address
              │                 │                  │
              ▼                 ▼                  ▼
       direct call or       explicit binding   more information,
       function handle          lookup        diagnosis or a
                                              compatibility path
```

A function handle could be a Wasm table handle rather than a host machine-code
pointer. A changing slot still needs a reliable binding event. Thus activation
and explicit identities are complementary, not mutually exclusive alternatives.

### Provenance is not necessarily a permanent binding

This behaviour must not be accidentally changed:

```text
activate A
p = P
call p         → A's function

activate B
call p         → B's function, although p did not change
```

Replacing `p = P` with a permanent reference to A is wrong for that program.
Knowing which module was active when a pointer was produced is insufficient.
The compiler must preserve the reference's binding semantics, for example as an
explicit slot reference or by a justified transformation of the reference flow.

It is not generally possible to eliminate a runtime choice that is part of the
program's behaviour. The objective is to represent that choice correctly, not
to rename an address or assume every function reference is permanently bound.

## Requirements for a general tool

| Area | Existing foundation | Information or capability still needed for explicit binding |
|---|---|---|
| Function inventory | Container/address identities and separate compiled functions | Carry identities through reference lowering where semantically valid |
| Symbol references | `Resolved` can retain container provenance | Distinguish provenance from a guarantee of fixed binding |
| C references | Wasm function relocations; direct-call and o32 lowering | Consistent fixed-function/late-bound reference representation |
| Original data | Guest layouts and original numeric pointer values | Relocations, data descriptions or analysis sufficient for safe conversion |
| Lifted targets | Direct/indirect callee classification and integer targets | Preserve or recover useful code-reference information; expose unresolved cases |
| Activation | Original loading effects visible as bytes | A supported, reliable way to establish module identity, destination and callable lifetime |
| Mixed boundaries | Existing C/lift/adaptor wrappers | Agreement about reference representations and conversions at their boundaries |

Relevant source seams include:
- `crates/psxw-inputs/src/map.rs`: `Resolved`, `AddressMap`.
- `crates/psxw-link/src/game.rs`: `GameSpec`, `func_addr`, `entry_from`,
  `dynamic_entry`, `Synth::OverlayStub`, `Synth::ICall`.
- `crates/psxw-link/src/rewrite.rs`: symbolic function relocations and indirect
  call lowering.
- `crates/psxw-link/src/layout.rs`: function-pointer relocations in placed data.
- `crates/psxw-lift/src/emit.rs`: direct calls and integer-target indirect calls.
- `crates/psxw-build/src/game.rs`: candidate planning and probe selection.

A labelled relocation saying “this field references this function” carries
meaning that a raw word such as `0x80110000` does not. That raw value could be a
function reference, a data address, an ordinary integer, or a changing slot.
Scanning for matching numbers is not a sound conversion strategy. Numeric
comparisons, casts, copies and address construction also matter where observable.

A general solution must state which facts it receives or can prove, and what
happens when those facts are absent. Declarative input facts are not permission
for game-specific compiler branches or a sequence of handwritten game ports.

Code identity, calling convention and data addressing are separate axes. Improving
identity/binding does not inherently require removing the guest-memory window,
rewriting every data pointer, replacing every o32 wrapper, or changing the
SDK → adaptor → native-backend boundary. A staged compatibility path is possible,
but must not be described as complete elimination of address-based dispatch.

## Questions a design proposal must answer

1. Which references are provably fixed, which are deliberately late-bound, and
   which remain unresolved across each supported input form?
2. Which translation assumptions must remain dynamic rather than being embedded
   from one original image? When can implementations safely be shared?
3. What reliable input/runtime facts establish activation through supported
   loading, copying and unpacking paths, without game-specific compiler logic?
4. How do original-data references, C relocations and lifted targets interoperate
   without changing observable pointer behaviour?
5. What are the reviewed rules for partial loads, independently replaceable
   regions and the lifetime of active or suspended code? Existing positive cases
   must not be silently reclassified as unsupported to obtain passing tests.
6. What does the tool diagnose or retain as compatibility behaviour when required
   information is unavailable?

The appropriate first design activity is an inventory of reference information
and binding semantics across supported input forms, demonstrated with small
synthetic examples—not implementing one game's loader and generalising afterward.
No new implementation phases or large test campaign are prescribed here.

**Bottom line:** distinguishing known images, preserving runtime binding, and
validating translation assumptions are related but different problems. Adding
more code-word comparisons cannot be assumed to solve all three.
