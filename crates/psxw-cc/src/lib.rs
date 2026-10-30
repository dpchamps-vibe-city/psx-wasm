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

/// The instruction selector every C → wasm compile uses (D53): FastISel.
/// The default, SelectionDAG, drops later variable assignments from the
/// DWARF (a lifted register shows a constant zero). Still `-O2`: only
/// instruction selection changes. FastISel's code changes with `-g`, so a
/// game's objects are compiled once, with `-g`, for release and debug.
pub const CODEGEN_FLAGS: &[&str] = &["-mllvm", "-fast-isel=true"];

/// Delete the objects under `dir` unless they were compiled with
/// [`CODEGEN_FLAGS`] plus `flags`, then record those. Objects are otherwise
/// fresh by their source alone, so changed flags must rebuild them.
pub fn objects_compiled_with(dir: &std::path::Path, flags: &[&str]) -> anyhow::Result<()> {
    fn remove_objects(dir: &std::path::Path) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                remove_objects(&path)?;
            } else if path.extension().is_some_and(|e| e == "o") {
                std::fs::remove_file(&path)?;
            }
        }
        Ok(())
    }
    let stamp = dir.join("flags");
    let want = format!("{}\n", CODEGEN_FLAGS.iter().chain(flags).copied().collect::<Vec<_>>().join(" "));
    if std::fs::read_to_string(&stamp).ok().as_deref() != Some(want.as_str()) {
        std::fs::create_dir_all(dir)?;
        remove_objects(dir)?;
        std::fs::write(&stamp, want)?;
    }
    Ok(())
}

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
