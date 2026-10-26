//! Lift tier: ahead-of-time translation of one MIPS R3000 function from a
//! relocatable object (matching-build `.o` or SDK archive member) into a
//! wasm function body for `psxw-link`. No MIPS is decoded at run time.
//!
//! - Relocations give symbolic call targets, data references and jump
//!   tables; nothing is guessed from raw addresses.
//! - Branch delay slots are resolved statically; load-delay hazards are
//!   modelled where a consumer sits in the slot.
//! - COP2 maps onto `psx-gte`; COP0, `syscall` and `break` become runtime
//!   imports.
//! - Memory accesses use the guest window (`psxw-abi`), `$sp` is the
//!   `__guest_sp` global shared with C code.
//!
//! Two modes: [`Mode::Abi`] assumes o32 at the function boundary (registers
//! become wasm locals); [`Mode::FullState`] keeps every register in the
//! runtime-private page for asm clusters that pass values in non-argument
//! registers.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Abi,
    FullState,
}
