#!/bin/sh
# A mod built outside psxw: plain clang and wasm-ld, linked with
# --emit-relocs (psxw places the module by its relocations) and
# --allow-undefined (the game's functions and data stay undefined).
set -e
mkdir -p build
"$PSXW_CLANG" --target=wasm32-unknown-unknown -O2 -nostdinc -ffreestanding -fno-builtin -I"$PSXW_MOD_DIR" -c src/random.c -o build/random.o
"$PSXW_WASM_LD" --no-entry --emit-relocs --allow-undefined --export=better_rand -o build/plain_clang.wasm build/random.o
