//! C tier: drive the pinned clang (wasm32, freestanding) over guest
//! translation units, producing relocatable wasm objects.
//!
//! Sources are never edited. Each TU is preprocessed with the host
//! [`PRELUDE`], then normalized as text:
//! - [`asm`]: register pinning stripped, top-level asm dropped, `break`
//!   becomes a runtime call, barriers kept, PsyQ GTE macros become GTE
//!   intrinsics ([`gte`]); other real MIPS asm marks its function for the
//!   lift tier;
//! - [`compile`]: an error-driven loop applies named fixups for GCC 2.95-isms
//!   clang rejects at file scope, and lifts any function clang can't compile.
//!
//! Every rule application and lift is counted, so the census can say which
//! decomp-side cleanups would make the C portable. [`probe`] compares struct
//! layout between the reference compiler and clang.

pub mod asm;
pub mod ast;
pub mod clang;
pub mod compile;
pub mod diag;
pub mod edit;
pub mod gte;
pub mod linemap;
pub mod probe;
pub mod protos;

pub use clang::Clang;
pub use compile::{Compiled, Lifted, compile};

/// Force-included ahead of every guest TU.
pub const PRELUDE: &str = include_str!("prelude.h");

/// Data symbols the prelude and normalizer emit to mark a function as lifted;
/// the rest of the name is the function, the initializer is the reason.
pub const LIFT_MARKER_PREFIX: &str = "__psxw_lift_";

/// Functions GCC 2.x expands inline as builtins, so original binaries carry
/// no copy, while freestanding clang emits calls. The host support library
/// supplies them.
pub const HOST_SUPPORT: &[&str] = &["abs", "labs", "fabs"];

/// UB containment flags (docs/design.md §Validation).
pub const UB_CONTAINMENT_FLAGS: &[&str] = &[
    "-std=gnu89",
    "-fwrapv",
    "-fno-strict-aliasing",
    "-fno-delete-null-pointer-checks",
    "-funsigned-char",
    "-ffreestanding",
    "-ftrivial-auto-var-init=zero",
];
