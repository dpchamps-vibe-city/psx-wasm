//! The final link (docs/design.md §8). wasm-ld cannot express what this
//! needs, which is mostly placement:
//!
//! - data symbols at their original addresses from the [`AddressMap`];
//!   clang-only anonymous data on identical original bytes or in
//!   `phys::AUX_DATA` ([`layout`]);
//! - function pointers are MIPS addresses; every cross-tier edge gets an
//!   o32 adapter ([`marshal`]);
//! - guest passes over every function body: address masking, the guest
//!   stack pointer, intrinsic lowering, `call_indirect` rewriting
//!   ([`rewrite`]).
//!
//! [`link`] assembles the module. Overlay dispatch and the game-mode link
//! arrive with M2.
//!
//! [`AddressMap`]: psxw_inputs::AddressMap

pub mod layout;
pub mod link;
pub mod marshal;
pub mod object;
pub mod rewrite;

pub use link::{Division, ExportSpec, Gcc, Input, Linked, Mode, callout, link};
