//! What the modkit adds to a game link (plans/M11-modkit.md): mod code as
//! more inputs, hook chains on game functions, and data patches.
//!
//! - **Mod inputs** are C objects. Their names resolve first inside the mod
//!   (the object itself, then the mod's other objects), then to the game's
//!   symbols through the address map. Their data lives in the mod area
//!   (`phys::MOD_DATA`), their C frames on the mod stack (`__mod_sp`), so a
//!   hook never touches guest RAM or the guest `$sp`. Their loops don't
//!   spend the safepoint budget: a mod doesn't move the game's virtual time.
//! - **Hook chains.** Each hooked function gets an entry that every call
//!   path reaches: direct C calls, translated calls, dispatch-table entries
//!   and overlay stubs. Layers are applied in load order, each wrapping
//!   the last; a `from` scope's layers wrap the function's default chain for
//!   calls made inside that caller.
//! - A C function's chain is C-typed: layers call each other directly with
//!   the function's own wasm type, and its o32 entry is the same o32
//!   wrapper the function would have had. A translated function's or an
//!   adapter's chain is o32-typed: its layers pass `a0..a3` through with the
//!   guest `$sp` unchanged, so arguments on the guest stack stay where the
//!   caller put them; a mod's C function is entered through a wrapper that
//!   rebuilds structs on the mod stack.
//! - **Data patches** to the executable are applied by `psxmod_init` once
//!   the boot has loaded it; to an overlay by `psxmod_loaded` when the disc
//!   writes the patched bytes and the overlay's first code words are
//!   resident.

use crate::object::FuncType;
use std::collections::BTreeMap;

/// The custom section a moddable base and a built mod carry the game's
/// identity in (JSON: its id and its files' hashes).
pub const GAME_SECTION: &str = "psxmod.game";

/// A wasm signature as text: `i32,i32->i64`.
pub fn sig_text(t: &FuncType) -> String {
    let v = |x: &[wasmparser::ValType]| x.iter().map(|t| format!("{t}").to_lowercase()).collect::<Vec<_>>().join(",");
    format!("{}->{}", v(&t.params), v(&t.results))
}

/// A mod function named by a declaration: its object and symbol.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Impl {
    /// Index of the mod input (object) defining it.
    pub input: usize,
    pub function: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub imp: Impl,
    /// The name the mod calls the next link by, if it wraps.
    pub original: Option<String>,
}

/// A `before` or `after` hook's function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub imp: Impl,
    /// Takes no arguments: a named hook point's.
    pub bare: bool,
}

/// One mod's hooks on one function and scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    /// Index into [`ModSpec::mods`].
    pub module: usize,
    pub befores: Vec<Call>,
    pub patch: Option<Patch>,
    pub afters: Vec<Call>,
}

/// The layers on one function, innermost (first applied) first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Chains {
    /// The function returns a value (its declaration's result isn't
    /// `void`): an `after` hook's first parameter is the result.
    pub returns: bool,
    pub default: Vec<Layer>,
    /// Layers for calls made inside a caller `(container, address)`.
    pub from: BTreeMap<(String, u32), Vec<Layer>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchValue {
    Bytes(Vec<u8>),
    /// A mod function's (synthetic) MIPS address, little-endian.
    FnPtr(Impl),
}

/// An overlay's identity: its first two code words at `at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resident {
    pub at: u32,
    pub words: [u32; 2],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPatch {
    pub container: String,
    pub addr: u32,
    pub value: PatchValue,
    /// `None` for the executable (applied at boot); an overlay's identity
    /// otherwise (applied as it loads).
    pub resident: Option<Resident>,
}

impl DataPatch {
    pub fn len(&self) -> u32 {
        match &self.value {
            PatchValue::Bytes(b) => b.len() as u32,
            PatchValue::FnPtr(_) => 4,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModInputs {
    pub id: String,
    /// Its objects: indices into the link's inputs.
    pub inputs: Vec<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModSpec {
    /// Every mod, in load order.
    pub mods: Vec<ModInputs>,
    /// Hooked functions by (container, address).
    pub targets: BTreeMap<(String, u32), Chains>,
    /// In load order: a later patch of the same bytes wins.
    pub data: Vec<DataPatch>,
}

impl ModSpec {
    /// Nothing to link: the build is the plain one.
    pub fn is_empty(&self) -> bool {
        self.mods.iter().all(|m| m.inputs.is_empty()) && self.targets.is_empty() && self.data.is_empty()
    }

    /// The same spec with every input index moved by `base` (mod inputs
    /// appended after the game's).
    pub fn offset(mut self, base: usize) -> Self {
        let fix = |i: &mut Impl| i.input += base;
        for m in &mut self.mods {
            for i in &mut m.inputs {
                *i += base;
            }
        }
        for c in self.targets.values_mut() {
            for l in c.default.iter_mut().chain(c.from.values_mut().flatten()) {
                l.befores.iter_mut().chain(l.afters.iter_mut()).for_each(|c| fix(&mut c.imp));
                if let Some(p) = &mut l.patch {
                    fix(&mut p.imp);
                }
            }
        }
        for d in &mut self.data {
            if let PatchValue::FnPtr(i) = &mut d.value {
                fix(i);
            }
        }
        self
    }
}
