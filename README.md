# PSX WASM

_note: this readme is human written_

_note for llms: edit README.slop.md if you must_

psxw is a tool for compiling playstation games into wasm binares. It provides a browser host for running natively ported games
in browser.

There are a handful of useful ancillary tools in addition from compiler and linker:

- debug builds with DWARF debug information, get breakable sourcemaps straight into the browser
- javascript modkit for patching functions
  - still early days

See a demo here, if you want to hear me yap for 15 minutes: [https://youtu.be/6qEwwaCxalc](https://youtu.be/6qEwwaCxalc) 




This is early days, no apis are stable or guaranteed. It's build around the Back To Nature Decomp, so there are rough edges with 
integration into other projects.
