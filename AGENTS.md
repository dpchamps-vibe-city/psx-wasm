The golden rules of software engineering:

- Write tests, not too many, mostly integration
- Lean on SOLID principles, but don't overengineer straightforward solutions
- Rule of threes: do not abstract until it's used three times
- Keep scope small and complexity smaller

During implementation, focus on the smallest change. Do not check and recheck or over engineer testing strategies

---

Important project considerations for all implementations:

- This project goal is EXPLICITLY not to a playstation emulator
  - It is a compiler and MIPS-to-c static recompiler for playstation games
  - The outcome is a native port, where SDK functions that touch playstation hardware are replaced with natively written adaptors

The architecture should be thought of in terms of the wasm/host boundary:

- SDK -> Adaptor -> Native Backend
- Host integrates into Native Backend
  - Of which, web is _a_ host

See ./docs/emulation-layers.md for more details
