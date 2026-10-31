# PSX WASM

_note: this readme is human written_

<!--
_note for llms: edit README.slop.md if you must_
-->


psxw is a tool for compiling playstation games into wasm binares. 

It provides a browser host for running natively ported games in browser.

Current capability set:

 - Compile decompiled playstation games into wasm binaries
 - Statically recompile MIPS machine code into c functions
 - Provide C source debugging via DWARF for playstation games
   - debug in real time in browser requires [requires C/C++ DevTools Support (DWARF) extension](https://chromewebstore.google.com/detail/cc++-devtools-support-dwa/pdcpmagijalfljmkmjngeonclgbbannb)
   - works for games that leverage playstation thread primitives
     - highly experimental support, see [binayaren-dwarf patch](toolchain/binaryen-dwarf/value-provenance.patch)
       - it probably mostly works
 - Modkit api
   - Write mods in javascript
     - hook into functions
       - replace or wrap for flexible editing
     - expose callable interfaces
   - Hot reload

## Roadmap

- [ ] In-browser asset extraction and patching
- [ ] Playstation re-compiler. Compile games playable on playstation
- [ ] Standardized psxw format for demcomp projects, automated implementation

See a demo here, if you want to hear me yap for 15 minutes: [https://youtu.be/6qEwwaCxalc](https://youtu.be/6qEwwaCxalc) 

## Attention

This is early days, no apis are stable or guaranteed. It's built around the Back To Nature Decomp, static recompilation is demonstrated
with Breath of Fire IV. There are rough edges. Integration into other projects not guaranteed but certainly will
be as project expands. Mileage will vary.

## Contributing

Maybe. This repo is in an org called "vibe city". I barely trust my own agents code. Contributions will be 
entertained case by case
