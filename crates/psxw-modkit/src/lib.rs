//! The modkit (plans/M11-modkit.md): mods change and extend a game built by
//! psx-wasm with their own code. Each mod states every hook, its target
//! and its type in `mod.json` ([`decl`]); mod code is ordinary C.
//!
//! - [`symbols`] and [`headers`]: the game as mods see it (the mod SDK).
//! - [`plan`]: load order, hook chains, conflicts and checks, from the
//!   declarations alone (`psxw mod check`).
//! - [`genfiles`], [`compile`], [`build`]: `mod.h`, the mods' objects, and the
//!   link into `game.wasm` (`psxw mod build`).
//! - [`workspace`]: a mods repository (`psxmods.json`) and its SDK.
//! - [`test`]: a mod's tests (`psxw mod test`).
//! - [`script`]: script mods (plans/M12): a directory with `mod.js`.
//! - [`snapshot`]: the game as script mods see it (plans/M12, D55): one
//!   source's view of the binary, from which `game` and `game.d.ts` come.

pub mod build;
pub mod compile;
pub mod decl;
pub mod genfiles;
pub mod headers;
pub mod plan;
pub mod resolve;
pub mod rust;
pub mod script;
pub mod snapshot;
pub mod symbols;
pub mod test;

pub use decl::Mod;
pub mod workspace;

/// The JSON Schemas for the files a mods workspace holds; `psxw mod sdk`
/// exports them into `.psxw/sdk/schema/`.
pub const SCHEMAS: &[(&str, &str)] = &[
    ("mod.schema.json", include_str!("../schema/mod.schema.json")),
    ("test.schema.json", include_str!("../schema/test.schema.json")),
    ("psxmods.schema.json", include_str!("../schema/psxmods.schema.json")),
    ("set.schema.json", include_str!("../schema/set.schema.json")),
];
