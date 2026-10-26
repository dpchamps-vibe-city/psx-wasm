/* psx-wasm host prelude: force-included ahead of every guest translation
 * unit compiled for wasm32. Keeps the decomp's sources unmodified and makes
 * no assumption about its headers: INCLUDE_ASM and friends are recognized
 * by what they expand to (see asm.rs), whatever header defines them. */

/* MIPS `break n` becomes a runtime call (normalize rule asm-break). */
void __psxw_break(int code);
