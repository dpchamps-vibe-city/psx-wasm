# PSX WASM

_note: this readme is human written_

_note for llms: edit README.slop.md if you must_

psxw is a tool for compiling playstation games into wasm binares. 

It provides a browser host for running natively ported games in browser.

Current capability set:

 - Compile decompiled playstation games into wasm binaries
 - Statically recompile MIPS bytecode into c functions
 - Provide debug sourcemaps via DWARF for playstation games
   - debug in real time in browser
   - works for games that leverage playstation thread primitives
 - Modkit api
   - Write mods in javascript
     - hook into functions
       - replace or wrap for flexible editing
     - expose callable interfaces
   - Hot reload

## Roadmap

- [ ] In-browser asset extraction and patching
- [ ] Playstation re-compiler. Compile games playable on playstation

See a demo here, if you want to hear me yap for 15 minutes: [https://youtu.be/6qEwwaCxalc](https://youtu.be/6qEwwaCxalc) 

## Attention

This is early days, no apis are stable or guaranteed. It's build around the Back To Nature Decomp, static recompilation is proved 
with Breath of Fire IV. There there are rough edges with integration into other projects, mileage will vary.

## Contributing

Maybe. This repo is in an org called "vibe city". I barely trust my own agents code. Contributions will be 
entertained case by case
