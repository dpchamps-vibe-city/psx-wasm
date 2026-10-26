# M9 — Second game

**Status:** not started
**Depends on:** M7, M8. **Unlocks:** the generality claim.
**Design refs:** Goal (the generality test); decisions D3, D12, D21.

## Goal

Run a second decompiled game through the whole pipeline with only a game
manifest, replays, and adapters for any SDK objects it introduces. Every
oracle from M0 to M7 must pass. Any code change has to be a generic
improvement, never something specific to the game.

## Choosing the game (human decision, Q4)

Criteria to present to the human:
- **A matching decompilation** with a byte-identical build gate (any
  coverage; translation fills the gaps).
- **splat-based layout:** symbol_addrs files, auto-names, `target_path`
  and `sha1`.
- **A PsyQ version different from btn's 4.7,** to test object-keyed
  adapters. Ideally overlays too.
- **The human can supply a disc image,** and a PsyQ SDK copy if new
  objects need relocatable members.
- **Decomp licensing that allows this use.**

Write a short candidate evaluation (by the criteria, from public
information) and ask the human to pick. Don't pick unilaterally.

## Tasks

1. [ ] Candidate evaluation; the human picks; pin the repo in
   `inputs.lock`; add `games/<id>/game.toml`.
2. [ ] `psxw census <id>`: fix any failure *generically* (normalizer rule,
   adapter for a splat convention). Every fix has a test that would have
   caught it on btn-like code too.
3. [ ] `psxw difftest <id>`: zero unexplained mismatches.
4. [ ] `psxw run <id> --check boot`: boot gates (M2 criteria).
5. [ ] Identity report (M8); adapters for new objects, validated by
   per-object conformance.
6. [ ] Replays (scripted `title-to-gameplay`; a human-recorded long
   session if the human agrees); vectors; goldens; `oracle check` for all
   devices; `ramdiff`.
7. [ ] Browser determinism (M5 criteria).
8. [ ] Write up what the second game required (a generality report) in
   Findings and in `docs/design.md`.

## Exit criteria (all automated)

1. `psxw ci <id>` passes every gate from M0–M7 equivalents: census,
   difftest, boot, device checks, adapter conformance, `ramdiff`, browser
   determinism.
2. `git diff` between the start and end of this milestone touches no Rust
   code that mentions the game. Changes are manifests, replays, generic
   rules or adapters only. A reviewer agent checks this.
3. `psxw ci btn` still passes.

## Progress log

## Findings
