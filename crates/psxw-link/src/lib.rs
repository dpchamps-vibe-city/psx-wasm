//! Final link of `guest.wasm`. wasm-ld cannot express what this needs, which
//! is mostly placement (`docs/design.md` §8):
//!
//! - data symbols at their original addresses from the [`AddressMap`];
//!   clang-only anonymous data in `phys::AUX_DATA`;
//! - every C-defined initializer checked byte-for-byte against the original
//!   image (struct-layout verification);
//! - function addresses resolve to original MIPS addresses; one dispatch
//!   table indexed by `psxw_abi::table_index`;
//! - o32-indirect-ABI adapters for every table entry and every
//!   `call_indirect` site;
//! - overlay dispatch stubs that pick the resident overlay by comparing the
//!   code words in guest RAM against each candidate;
//! - `replace`-tier symbols bound to `psx.*` imports; lifted bodies spliced in.
//!
//! Then the guest passes run over guest functions only: address masking
//! (loads, stores, `memory.copy`/`memory.fill`), `__stack_pointer` →
//! `__guest_sp`, safepoint budget at every loop header. `wasm-opt -O3` last.
//!
//! Only [`object`] (the relocatable-object reader) exists so far.
//!
//! [`AddressMap`]: psxw_inputs::AddressMap

pub mod object;

pub mod passes {
    //! Guest-only wasm rewrites, applied after linking.
}
