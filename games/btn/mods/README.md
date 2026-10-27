# btn's test mods

psx-wasm's own mods for btn, which the CI gate `mods` runs (plans/M11).
They contain only our code. The acceptance mod, better-rand, lives in the
separate `btn-mods` repository that `inputs.lock` pins.

- `conflict-a`, `conflict-b`: both replace `Rand` outright. Applied
  together, `psxw mod check` warns naming both, and the one applied last
  runs: each one's test passes only when it is the one applied last.
- `frame-log`: an `after` hook on the manifest's `frame` point, keeping a
  counter in mod data and logging through `psxmod_log`.
- `wrap-rand`: `before`, `after` and a wrapping `patch` on `Rand`; it
  checks every result against the original's range.
