/* psx-wasm host prelude: force-included ahead of every guest translation
 * unit compiled for wasm32. Keeps the decomp's sources unmodified and makes
 * no assumption about its headers: INCLUDE_ASM and friends are recognized
 * by what they expand to (see asm.rs), whatever header defines them. */

/* MIPS `break n` becomes a runtime call (normalize rule asm-break). */
void __psxw_break(int code);

/* PsyQ GTE macros become these (normalize rule asm-gte): the runtime's GTE
 * intrinsics, which the linker binds, and byte-aligned guest accesses. */
unsigned int psx_gte_mfc2(unsigned int r);
unsigned int psx_gte_cfc2(unsigned int r);
void psx_gte_mtc2(unsigned int r, unsigned int v);
void psx_gte_ctc2(unsigned int r, unsigned int v);
void psx_gte_exec(unsigned int cmd);
typedef struct { unsigned int v; } __attribute__((packed, aligned(1))) __psxw_u32;
typedef struct { unsigned short v; } __attribute__((packed, aligned(1))) __psxw_u16;
static __inline__ unsigned int __psxw_lw(unsigned int a) { return ((__psxw_u32 *)a)->v; }
static __inline__ unsigned int __psxw_lhu(unsigned int a) { return ((__psxw_u16 *)a)->v; }
static __inline__ unsigned int __psxw_lbu(unsigned int a) { return *(unsigned char *)a; }
static __inline__ void __psxw_sw(unsigned int a, unsigned int v) { ((__psxw_u32 *)a)->v = v; }
static __inline__ void __psxw_sh(unsigned int a, unsigned int v) { ((__psxw_u16 *)a)->v = (unsigned short)v; }
static __inline__ void __psxw_sb(unsigned int a, unsigned int v) { *(unsigned char *)a = (unsigned char)v; }
