//! The game-mode link (plans/M02): every container's C and generated code
//! into the guest module that `wasm-merge` joins with `runtime.wasm`.
//!
//! - Each guest function's entry is chosen by its tier: an adapter thunk
//!   (`psx.adapter(id, …)`), the C function's o32 wrapper, or its
//!   translation.
//! - Function pointers are MIPS addresses; the dispatch table maps each
//!   address to its entry. Where overlays share an address, the entry is a
//!   stub that compares the code resident in RAM with each candidate's
//!   original words (D7).
//! - C calls C directly when the signatures agree; every other edge goes
//!   through o32 adapters.
//! - Every loop header spends the safepoint budget.

use crate::layout::{Layout, Rodata};
use crate::link::{Input, Linked};
use crate::marshal;
use crate::mods::{Impl, Layer, ModSpec, PatchValue};
use crate::object::{FuncType, NameClass, SymbolKind, WasmObject, classify_name};
use crate::rewrite::{self, Lowered, Resolver};
use anyhow::{Context as _, Result, bail, ensure};
use psxw_abi::{TABLE_LEN, WINDOW_MASK, guest, psx, table_index};
use psxw_inputs::AddressMap;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use wasm_encoder::{
    BlockType, Catch, CodeSection, ConstExpr, CustomSection, DataSection, ElementSection, Elements, EntityType,
    ExportKind, ExportSection, Function, FunctionSection, GlobalSection, GlobalType, ImportSection, Instruction as I,
    MemArg, MemorySection, MemoryType, Module, NameMap, NameSection, RefType, TableSection, TableType, TagKind,
    TagSection, TagType, TypeSection, ValType,
};
use wasmparser::ValType as V;

/// How a guest function is implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Decompiled C: function `func` (defined index) of input `input`.
    C { input: usize, func: u32 },
    /// Translated: `psxw_t_*` function `func` of generated input `input`.
    Lift { input: usize, func: u32 },
    /// Runtime adapter number `id`.
    Adapter(u32),
}

#[derive(Debug, Clone)]
pub struct GameSpec {
    /// Every guest function: (container, address) → tier.
    pub tiers: BTreeMap<(String, u32), Tier>,
    /// Addresses more than one container defines: each candidate's first
    /// two original code words.
    pub overlays: BTreeMap<u32, Vec<(String, [u32; 2])>>,
    /// Loop iterations between safepoints.
    pub budget: u32,
    /// Trap on guest accesses to the I/O window.
    pub debug_io: bool,
    /// Functions of device libraries (D40): their I/O-window accesses go
    /// to the runtime's devices (`psx.io_access`).
    pub device_funcs: BTreeSet<(String, u32)>,
    pub main: String,
    /// Initial guest memory pages (the runtime grows it).
    pub min_pages: u64,
    /// The guest window's wasm address: psxw-abi `WINDOW_BASE`, or
    /// `DEBUG_WINDOW_BASE` in a debug build (`min_pages` must reach past
    /// its end).
    pub window_base: u32,
    /// Carry the inputs' DWARF into the module, through the guest passes
    /// ([`crate::dwarf`]).
    pub debug_info: bool,
    /// Functions whose entry calls `psx.probe(addr)`.
    pub probes: Vec<(String, u32)>,
    /// Mods (plans/M11): their inputs, hook chains and data patches.
    pub mods: Option<ModSpec>,
    /// A moddable base (plans/M11 phase 2): each of these functions'
    /// entries checks its hook slot (its index here), which mods loaded at
    /// run time fill.
    pub slots: Option<Vec<(String, u32)>>,
    /// The game switches threads (D57): the build instruments the module
    /// with Asyncify, which exports the control functions; otherwise the
    /// link exports stubs.
    pub threads: bool,
}

/// A moddable base's tables, by index.
pub mod tables {
    pub const DISPATCH: u32 = 0;
    /// Hook slots, then the next links mods' layers call (funcref; grows).
    pub const CHAIN: u32 = 1;
    /// Each slot's function entry, hooks included, in its own type: what a
    /// mod's call to a game function goes through.
    pub const ENTRIES: u32 = 2;
    /// Each slot's function without hooks: the bottom of its chain.
    pub const RAW: u32 = 3;
    /// Mods' `psxmod_init` and `psxmod_loaded`, called in load order.
    pub const ON_INIT: u32 = 4;
    pub const ON_LOADED: u32 = 5;
    /// Room for this many mods' init and loaded functions.
    pub const MAX_MODS: u32 = 256;
}

/// A moddable base's exports (plans/M11 phase 2).
pub mod exports {
    pub const DISPATCH: &str = "psxmod_dispatch";
    pub const CHAIN: &str = "psxmod_chain";
    pub const ENTRIES: &str = "psxmod_entries";
    pub const RAW: &str = "psxmod_raw";
    pub const ON_INIT: &str = "psxmod_on_init";
    pub const ON_LOADED: &str = "psxmod_on_loaded";
    /// Mutable i32 globals: how many entries of the two tables are used.
    pub const N_INIT: &str = "psxmod_n_init";
    pub const N_LOADED: &str = "psxmod_n_loaded";
    /// The custom section listing the slots, one line each:
    /// `<container> <address> c <params>-><results>` or `... o32`.
    pub const SLOTS_SECTION: &str = "psxmod.slots";
    /// Each slot's tier, a line each in the slots' order: `c`, `lift` or
    /// `adapter` (script mods, plans/M12).
    pub const TIERS_SECTION: &str = "psxmod.tiers";
    /// How to tell each overlay is resident, a line each:
    /// `<container> <address> <word> <word>`, its first two code words.
    pub const RESIDENT_SECTION: &str = "psxmod.resident";
}

/// The `psx.*` imports, in import order, with their signatures.
/// The guest's one exception tag, `() -> ()`: ReturnFromException.
const RFE_TAG: u32 = 0;

pub(crate) fn psx_imports() -> Vec<(&'static str, Vec<V>, Vec<V>)> {
    let i = V::I32;
    vec![
        (psx::SAFEPOINT, vec![], vec![]),
        (psx::UNTRANSLATED, vec![i], vec![V::I64]),
        (psx::BIOS, vec![i; 6], vec![V::I64]),
        (psx::SYSCALL, vec![i; 4], vec![i]),
        (psx::BREAK, vec![i], vec![]),
        (psx::OVERFLOW, vec![i], vec![]),
        (psx::BAD_JUMP, vec![i, i], vec![]),
        (psx::KERNEL_CALL, vec![i; 5], vec![V::I64]),
        (psx::COP0_READ, vec![i], vec![i]),
        (psx::COP0_WRITE, vec![i, i], vec![]),
        (psx::RFE, vec![], vec![]),
        (psx::GTE_MFC2, vec![i], vec![i]),
        (psx::GTE_CFC2, vec![i], vec![i]),
        (psx::GTE_MTC2, vec![i, i], vec![]),
        (psx::GTE_CTC2, vec![i, i], vec![]),
        (psx::GTE_EXEC, vec![i], vec![]),
        (psx::IO_ACCESS, vec![i, i], vec![]),
        (psx::IO_TRAP, vec![i, i], vec![]),
        ("adapter", vec![i; 5], vec![V::I64]),
        (psx::PROBE, vec![i], vec![]),
        (psx::RETURN_FROM_EXCEPTION, vec![], vec![]),
        (psx::MOD_LOG, vec![i, i], vec![]),
    ]
}

/// C names of the runtime services, and the `psx` import each lowers to.
pub(crate) fn service(name: &str) -> Option<&'static str> {
    Some(match name {
        "psxw_bios" => psx::BIOS,
        "psxw_syscall" => psx::SYSCALL,
        "__psxw_break" => psx::BREAK,
        "psxw_trap_overflow" => psx::OVERFLOW,
        "psxw_bad_jump" => psx::BAD_JUMP,
        "psxw_kernel_call" => psx::KERNEL_CALL,
        "psxw_cop0_read" => psx::COP0_READ,
        "psxw_cop0_write" => psx::COP0_WRITE,
        "psxw_rfe" => psx::RFE,
        "psx_gte_mfc2" => psx::GTE_MFC2,
        "psx_gte_cfc2" => psx::GTE_CFC2,
        "psx_gte_mtc2" => psx::GTE_MTC2,
        "psx_gte_ctc2" => psx::GTE_CTC2,
        "psx_gte_exec" => psx::GTE_EXEC,
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Synth {
    /// C call through o32 to the entry `target` (function index).
    ToO32 { input: usize, name: String, sig: FuncType, target: u32 },
    /// o32 wrapper around a C function.
    O32Wrap { input: usize, func: u32 },
    AdapterThunk(u32),
    /// Dispatch on the code resident at an address shared by overlays.
    OverlayStub(u32),
    /// C `call_indirect` with a signature (the MIPS address trails).
    ICall(FuncType),
    /// `psxw_call_indirect(addr, a0..a3)` and `guest_invoke`.
    Indirect,
    /// `guest_invoke_irq`: `Indirect` catching ReturnFromException.
    InvokeIrq,
    /// `psxw_return_from_exception`: tell the runtime, then throw.
    Throw,
    Support(String, FuncType),
    Division(&'static str, crate::link::Division),
    InlineCopy,
    SpGet,
    SpSet,
    GpGet,
    GpSet,
    /// `guest.guest_window`: the window's wasm address.
    Window,
    /// Asyncify's control function `name` for a guest that never switches
    /// threads (D57): the state is always normal, and the rest trap.
    AsyncifyStub(&'static str),
    /// o32 wrapper around the C-typed function index `callee` (signature
    /// `sig`), with the prototype facts of `name` in input `input`.
    O32WrapOf { callee: u32, sig: FuncType, input: usize, name: String },
    /// A C-typed hook layer with the hooked function's signature `sig`
    /// (prototype facts of `name` in `input`): befores, `core` (a patch or
    /// the next link), afters; each with whether it takes no arguments.
    CLayer { befores: Vec<(u32, bool)>, core: u32, afters: Vec<(u32, bool)>, sig: FuncType, input: usize, name: String },
    /// An o32 hook layer; each after takes the result first when `returns`.
    O32Layer { befores: Vec<u32>, core: u32, afters: Vec<u32>, returns: bool },
    /// A mod's C function entered from an o32 chain; `hook`: a wrapping
    /// patch's id.
    ModWrap { input: usize, func: u32, hook: Option<i32> },
    /// A mod's `after` entered from an o32 layer.
    ModAfter { input: usize, func: u32, result: bool },
    /// A mod's call to the next link of an o32 chain (its `original`).
    Transparent { input: usize, name: String, sig: FuncType, next: u32, id: i32 },
    /// `guest.psxmod_init`: data patches to the executable.
    ModInit,
    /// `guest.psxmod_loaded(addr, len)`: data patches to overlays.
    ModLoaded,
    /// A moddable base's C-typed slot entry: the hook in slot `slot`, else
    /// the function `callee`.
    SlotC { slot: u32, callee: u32, sig: FuncType },
    /// The same for an o32 function.
    SlotO32 { slot: u32, raw: u32 },
}

/// A hooked function's entries.
#[derive(Debug, Clone)]
struct Head {
    /// The C-typed entry and its signature (a C function's chain).
    c: Option<(u32, FuncType)>,
    /// The o32 entry.
    o32: u32,
}

/// What a mod's `original` name calls.
#[derive(Debug, Clone)]
enum Bound {
    /// The next link of a C-typed chain.
    C(u32, FuncType),
    /// The next link of an o32 chain, inside the wrapping patch `id`.
    O32 { next: u32, id: i32 },
}

/// A chain's link while it is built.
#[derive(Debug, Clone, Copy)]
enum Link {
    C(u32),
    O32(u32),
}

struct Ctx<'a> {
    map: &'a AddressMap,
    inputs: &'a [Input],
    view: Vec<(&'a str, &'a WasmObject)>,
    layout: Layout,
    spec: &'a GameSpec,
    types: Vec<FuncType>,
    type_ix: HashMap<FuncType, u32>,
    imports: HashMap<&'static str, u32>,
    nimports: u32,
    guest: HashMap<(usize, u32), u32>,
    guest_order: Vec<(usize, u32)>,
    synth: Vec<Synth>,
    synth_ix: HashMap<Synth, u32>,
    sp: u32,
    gp: u32,
    budget: u32,
    /// GCC's static chain between lifted functions (psxw_rt.h).
    chain: u32,
    o32_ty: u32,
    /// (input, function) of every device-library function.
    device: HashSet<(usize, u32)>,
    /// C→C calls routed through o32 because clang's C-level kinds differ.
    abi_mismatches: BTreeSet<String>,
    /// Generated calls to addresses no container defines (reported together).
    unresolved: BTreeSet<String>,
    /// Per input, the mod it belongs to (index into `ModSpec::mods`).
    mod_of: Vec<Option<usize>>,
    /// The mod stack pointer and the o32 hook frame (`$sp` at a wrapping
    /// patch's entry, and its id); globals that exist only with mods.
    mod_sp: u32,
    hook_sp: u32,
    hook_id: u32,
    /// The first of the overlay data patches' pending flags (globals).
    pending: u32,
    /// Synthetic MIPS addresses of mod functions: (input, local function).
    synthetic: BTreeMap<(usize, u32), u32>,
    /// Hooked functions' entries: (container, address, from-caller).
    heads: HashMap<(String, u32, Option<(String, u32)>), Head>,
    /// Mods' `original` names: (mod, name).
    bindings: HashMap<(usize, String), Bound>,
    /// Each game function's (container, address), for `from` scopes.
    caller_of: HashMap<(usize, u32), (String, u32)>,
    /// A moddable base's hook-count globals (init, loaded).
    moddable: Option<(u32, u32)>,
}

impl Ctx<'_> {
    fn intern(&mut self, t: &FuncType) -> u32 {
        if let Some(&i) = self.type_ix.get(t) {
            return i;
        }
        let i = self.types.len() as u32;
        self.types.push(t.clone());
        self.type_ix.insert(t.clone(), i);
        i
    }

    fn synth(&mut self, s: Synth) -> u32 {
        if let Some(&i) = self.synth_ix.get(&s) {
            return i;
        }
        let i = self.nimports + self.guest_order.len() as u32 + self.synth.len() as u32;
        self.synth.push(s.clone());
        self.synth_ix.insert(s, i);
        i
    }

    fn import(&self, name: &str) -> u32 {
        self.imports[name]
    }

    /// The o32 entry of the function at (container, addr): its hook chain
    /// when it has one.
    fn entry(&mut self, container: &str, addr: u32) -> Option<u32> {
        if self.spec.overlays.contains_key(&addr) && container != self.spec.main {
            return Some(self.synth(Synth::OverlayStub(addr)));
        }
        if let Some(h) = self.heads.get(&(container.to_string(), addr, None)) {
            return Some(h.o32);
        }
        self.entry_raw(container, addr)
    }

    /// The function's own o32 entry, without hooks or overlay dispatch.
    fn entry_raw(&mut self, container: &str, addr: u32) -> Option<u32> {
        let tier = *self.spec.tiers.get(&(container.to_string(), addr))?;
        Some(match tier {
            Tier::Lift { input, func } => self.guest[&(input, func)],
            Tier::C { input, func } => self.synth(Synth::O32Wrap { input, func }),
            Tier::Adapter(id) => self.synth(Synth::AdapterThunk(id)),
        })
    }

    /// A hooked function's entries as seen from a caller.
    fn head(&self, container: &str, addr: u32, caller: Option<&(String, u32)>) -> Option<&Head> {
        caller
            .and_then(|k| self.heads.get(&(container.to_string(), addr, Some(k.clone()))))
            .or_else(|| self.heads.get(&(container.to_string(), addr, None)))
    }

    /// [`Ctx::entry`] for a call from `caller`, whose `from`-scoped hooks
    /// apply.
    fn entry_from(&mut self, container: &str, addr: u32, caller: Option<&(String, u32)>) -> Option<u32> {
        if self.spec.overlays.contains_key(&addr) && container != self.spec.main {
            return Some(self.synth(Synth::OverlayStub(addr)));
        }
        match self.head(container, addr, caller) {
            Some(h) => Some(h.o32),
            None => self.entry_raw(container, addr),
        }
    }

    /// A function mod `m` defines that symbol `symbol` of input `input`
    /// names: the input's own definition, else a global one in another of
    /// the mod's objects. (input, local function).
    fn mod_function(&self, m: usize, input: usize, symbol: u32) -> Option<(usize, u32)> {
        let inp = &self.inputs[input];
        let s = &inp.obj.symbols[symbol as usize];
        if s.kind != SymbolKind::Function {
            return None;
        }
        if s.defined {
            let local = s.index? - inp.obj.num_imported_funcs();
            return Some((input, local));
        }
        let spec = self.spec.mods.as_ref()?;
        for &j in &spec.mods[m].inputs {
            let o = &self.inputs[j].obj;
            if let Some(t) = o.symbols.iter().find(|t| t.kind == SymbolKind::Function && t.defined && !t.local && t.name == s.name) {
                return Some((j, t.index? - o.num_imported_funcs()));
            }
        }
        None
    }

    /// The synthetic MIPS address of a mod function a symbol names.
    fn mod_func_addr(&self, m: usize, input: usize, symbol: u32) -> Option<u32> {
        let k = self.mod_function(m, input, symbol)?;
        self.synthetic.get(&k).copied()
    }

    /// A declaration's mod function: (input, local function).
    fn impl_func(&self, imp: &Impl) -> Result<(usize, u32)> {
        let o = &self.inputs[imp.input].obj;
        let s = o
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Function && s.defined && s.name == imp.function)
            .with_context(|| format!("{} defines no function {}", self.inputs[imp.input].label, imp.function))?;
        let index = s.index.context("function symbol without index")?;
        Ok((imp.input, index - o.num_imported_funcs()))
    }

    fn func_sig(&self, input: usize, func: u32) -> FuncType {
        let o = &self.inputs[input].obj;
        o.types[o.funcs[func as usize].ty as usize].clone()
    }

    /// The entry for a call into an overlay slot from outside it.
    fn dynamic_entry(&mut self, addr: u32) -> Option<u32> {
        if self.spec.overlays.contains_key(&addr) {
            return Some(self.synth(Synth::OverlayStub(addr)));
        }
        let c = self.spec.tiers.keys().find(|(_, a)| *a == addr).map(|(c, _)| c.clone())?;
        self.entry(&c, addr)
    }

    /// Container whose code holds `addr`, as seen from `from`.
    fn home(&self, from: &str, addr: u32) -> Option<Option<String>> {
        let has = |c: &str| self.map.container(c).is_some_and(|k| k.text.iter().any(|r| r.contains(&addr)));
        if has(from) {
            return Some(Some(from.to_string()));
        }
        if has(&self.spec.main) {
            return Some(Some(self.spec.main.clone()));
        }
        self.map.containers.iter().any(|c| has(&c.id)).then_some(None)
    }
}

fn parse_generated(name: &str, main: &str) -> Option<(Option<String>, u32)> {
    if let Some(rest) = name.strip_prefix("psxw_d_") {
        return u32::from_str_radix(rest, 16).ok().map(|a| (None, a));
    }
    let rest = name.strip_prefix("psxw_f_")?;
    let (container, hex) = match rest.rsplit_once('_') {
        Some((c, h)) => (c.to_string(), h),
        None => (main.to_string(), rest),
    };
    u32::from_str_radix(hex, 16).ok().map(|a| (Some(container), a))
}

struct R<'c, 'a> {
    ctx: &'c mut Ctx<'a>,
    input: usize,
    /// The function being rewritten (index into the input's functions).
    func: u32,
}

impl Resolver for R<'_, '_> {
    fn call(&mut self, symbol: u32, sig: &FuncType, last_const: Option<i32>) -> Result<Lowered> {
        let inputs = self.ctx.inputs;
        let inp = &inputs[self.input];
        let s = &inp.obj.symbols[symbol as usize];
        let name = s.name.clone();
        let container = inp.container.clone();
        match name.as_str() {
            "psxw_sp_get" => return Ok(Lowered::Instrs(vec![I::GlobalGet(self.ctx.sp)])),
            "psxw_sp_set" => return Ok(Lowered::Instrs(vec![I::GlobalSet(self.ctx.sp)])),
            "psxw_gp_get" => return Ok(Lowered::Instrs(vec![I::GlobalGet(self.ctx.gp)])),
            "psxw_gp_set" => return Ok(Lowered::Instrs(vec![I::GlobalSet(self.ctx.gp)])),
            "psxw_chain_get" => return Ok(Lowered::Instrs(vec![I::GlobalGet(self.ctx.chain)])),
            "psxw_chain_set" => return Ok(Lowered::Instrs(vec![I::GlobalSet(self.ctx.chain)])),
            "psxw_call_indirect" => return Ok(Lowered::Call(self.ctx.synth(Synth::Indirect))),
            "psxw_return_from_exception" => return Ok(Lowered::Call(self.ctx.synth(Synth::Throw))),
            _ => {}
        }
        if let Some(imp) = service(&name) {
            return Ok(Lowered::Call(self.ctx.import(imp)));
        }
        if ["abs", "labs", "fabs"].contains(&name.as_str()) && !s.defined {
            return Ok(Lowered::Call(self.ctx.synth(Synth::Support(name, sig.clone()))));
        }
        if name == "memcpy" && last_const.is_some() && inp.gcc.is_some_and(|g| g.builtin_memcpy) && !s.defined {
            return Ok(Lowered::Call(self.ctx.synth(Synth::InlineCopy)));
        }
        if let Some(m) = self.ctx.mod_of[self.input] {
            if let Some(l) = self.mod_call(m, symbol, sig)? {
                return Ok(l);
            }
        }
        let caller = self.ctx.caller_of.get(&(self.input, self.func)).cloned();
        if let Some((c, addr)) = parse_generated(&name, &self.ctx.spec.main) {
            let e = match c {
                Some(c) => self.ctx.entry_from(&c, addr, caller.as_ref()),
                None => self.ctx.dynamic_entry(addr),
            };
            return match e {
                Some(f) => Ok(Lowered::Call(f)),
                None => {
                    // Listed after the link with every other such call.
                    self.ctx.unresolved.insert(format!("{}: {name}", inp.label));
                    Ok(Lowered::Call(self.ctx.synth(Synth::OverlayStub(addr))))
                }
            };
        }
        // Helpers defined in the same object with no guest address.
        let callee = match classify_name(&name) {
            NameClass::Guest(n) => n.to_string(),
            NameClass::SignatureThunk { target } => target.to_string(),
            NameClass::MainWrapper => bail!("call to clang's main wrapper {name}"),
        };
        let resolved = self.ctx.map.resolve(&container, &callee);
        if resolved.is_none() || inp.generated {
            if let (true, Some(idx)) = (s.defined, s.index) {
                let local = idx - inp.obj.num_imported_funcs();
                if let Some(&i) = self.ctx.guest.get(&(self.input, local)) {
                    return Ok(Lowered::Call(i));
                }
            }
        }
        let addr = resolved.with_context(|| format!("function {callee} has no address"))?.addr;
        let home = self.ctx.home(&container, addr).with_context(|| format!("{callee} at {addr:#x} is not code"))?;
        // C→C with matching signatures: a plain call. Matching means clang's
        // C-level kinds as well as the wasm types: a struct by value and a
        // scalar are both i32 in wasm (a pointer to a copy; the value) but
        // the same o32 word on the PS1, so a decomp can declare a function
        // either way and still match. Such a call goes through o32.
        let mismatch = |ctx: &Ctx, c: &str| -> Option<String> {
            let Some(Tier::C { input, func }) = ctx.spec.tiers.get(&(c.to_string(), addr)) else { return None };
            let o = &ctx.inputs[*input].obj;
            let def = o.func_symbol(func + o.num_imported_funcs())?;
            let NameClass::Guest(n) = classify_name(&def.name) else { return None };
            let declared = ctx.inputs[self.input].protos.get(&callee)?;
            let defined = ctx.inputs[*input].protos.get(n)?;
            abi_differs(declared, defined).then(|| {
                format!(
                    "{} calls {callee} as {:?} -> {:?}; {} defines {:?} -> {:?}",
                    ctx.inputs[self.input].label,
                    declared.params,
                    declared.ret,
                    ctx.inputs[*input].label,
                    defined.params,
                    defined.ret
                )
            })
        };
        let direct = |ctx: &Ctx, c: &str| -> Option<u32> {
            match ctx.spec.tiers.get(&(c.to_string(), addr))? {
                Tier::C { input, func } => {
                    let o = &ctx.inputs[*input].obj;
                    (o.types[o.funcs[*func as usize].ty as usize] == *sig && !ctx.spec.overlays.contains_key(&addr))
                        .then(|| ctx.guest[&(*input, *func)])
                }
                _ => None,
            }
        };
        // A hooked function: its chain's C entry when the signatures agree,
        // else its o32 entry. (An overlay slot's goes through the stub.)
        if let Some(c) = home.as_ref().filter(|c| !self.ctx.spec.overlays.contains_key(&addr) || **c == self.ctx.spec.main) {
            if let Some(h) = self.ctx.head(c, addr, caller.as_ref()).cloned() {
                if let Some((ci, csig)) = &h.c {
                    match mismatch(self.ctx, c) {
                        None if csig == sig => return Ok(Lowered::Call(*ci)),
                        Some(m) => {
                            self.ctx.abi_mismatches.insert(m);
                        }
                        None => {}
                    }
                }
                let target = h.o32;
                return Ok(Lowered::Call(self.ctx.synth(Synth::ToO32 { input: self.input, name: callee, sig: sig.clone(), target })));
            }
        }
        let target = match &home {
            Some(c) => {
                let differs = mismatch(self.ctx, c);
                match (differs, direct(self.ctx, c)) {
                    (None, Some(i)) => return Ok(Lowered::Call(i)),
                    (Some(m), Some(_)) => {
                        self.ctx.abi_mismatches.insert(m);
                    }
                    _ => {}
                }
                self.ctx.entry(c, addr)
            }
            None => self.ctx.dynamic_entry(addr),
        }
        .with_context(|| format!("{callee} ({addr:#010x}) has no implementation"))?;
        Ok(Lowered::Call(self.ctx.synth(Synth::ToO32 { input: self.input, name: callee, sig: sig.clone(), target })))
    }

    fn call_indirect(&mut self, sig: &FuncType) -> Result<u32> {
        Ok(self.ctx.synth(Synth::ICall(sig.clone())))
    }

    fn global(&mut self, symbol: u32) -> Result<u32> {
        let inputs = self.ctx.inputs;
        let s = &inputs[self.input].obj.symbols[symbol as usize];
        match s.name.as_str() {
            "__stack_pointer" if self.ctx.mod_of[self.input].is_some() => Ok(self.ctx.mod_sp),
            "__stack_pointer" => Ok(self.ctx.sp),
            other => bail!("guest code uses global {other}"),
        }
    }

    fn data_addr(&mut self, symbol: u32) -> Result<u32> {
        self.ctx.layout.data_addr(&self.ctx.view, self.ctx.map, self.input, symbol)
    }

    fn func_addr(&mut self, symbol: u32) -> Result<u32> {
        let inputs = self.ctx.inputs;
        if let Some(m) = self.ctx.mod_of[self.input] {
            if let Some(a) = self.ctx.mod_func_addr(m, self.input, symbol) {
                return Ok(a);
            }
        }
        let s = &inputs[self.input].obj.symbols[symbol as usize];
        let name = match classify_name(&s.name) {
            NameClass::Guest(n) | NameClass::SignatureThunk { target: n } => n,
            NameClass::MainWrapper => bail!("address of clang's main wrapper"),
        };
        self.ctx
            .map
            .resolve(&self.ctx.inputs[self.input].container, name)
            .map(|r| r.addr)
            .with_context(|| format!("function pointer to {name}, which has no address"))
    }

    fn type_index(&mut self, ty: u32) -> u32 {
        let t = self.ctx.inputs[self.input].obj.types[ty as usize].clone();
        self.ctx.intern(&t)
    }

    fn window_base(&self) -> u32 {
        self.ctx.spec.window_base
    }

    fn dirty_map(&self) -> Option<u32> {
        None
    }

    fn mark_range(&mut self) -> u32 {
        unreachable!("no dirty map in game mode")
    }

    fn division(&mut self, op: &'static str) -> Option<u32> {
        let d = self.ctx.inputs[self.input].gcc?.division;
        Some(self.ctx.synth(Synth::Division(op, d)))
    }

    fn prologue(&mut self, func_symbol: Option<&str>) -> Result<Vec<I<'static>>> {
        let inputs = self.ctx.inputs;
        let inp = &inputs[self.input];
        let mut out = Vec::new();
        if self.ctx.mod_of[self.input].is_some() {
            return Ok(out);
        }
        // A watched function: by its guest name (C) or its translation's.
        let addr = func_symbol.and_then(|n| match n.strip_prefix("psxw_t_") {
            Some(rest) => u32::from_str_radix(rest.rsplit('_').next()?, 16).ok(),
            None => match classify_name(n) {
                NameClass::Guest(g) if !inp.generated => self.ctx.map.resolve(&inp.container, g).map(|r| r.addr),
                _ => None,
            },
        });
        if let Some(a) = addr {
            if self.ctx.spec.probes.iter().any(|(c, pa)| *c == inp.container && *pa == a) {
                out.push(I::I32Const(a as i32));
                out.push(I::Call(self.ctx.import(psx::PROBE)));
            }
        }
        let is_main = func_symbol.is_some_and(|n| classify_name(n) == NameClass::Guest("main"));
        if !(is_main && inp.gcc.is_some_and(|g| g.main_calls_main)) {
            return Ok(out);
        }
        let container = inp.container.clone();
        let Some(r) = self.ctx.map.resolve(&container, "__main") else { return Ok(out) };
        let target = self.ctx.entry(&container, r.addr).context("__main has no implementation")?;
        let sig = FuncType { params: vec![], results: vec![] };
        let stub = self.ctx.synth(Synth::ToO32 { input: self.input, name: "__main".into(), sig, target });
        out.push(I::Call(stub));
        Ok(out)
    }

    fn loop_entry(&mut self) -> Vec<I<'static>> {
        // A mod's loops don't move the game's virtual time.
        if self.ctx.mod_of[self.input].is_some() {
            return Vec::new();
        }
        let (b, n) = (self.ctx.budget, self.ctx.spec.budget as i32);
        vec![
            I::GlobalGet(b),
            I::I32Const(1),
            I::I32Sub,
            I::GlobalSet(b),
            I::GlobalGet(b),
            I::I32Eqz,
            I::If(BlockType::Empty),
            I::I32Const(n),
            I::GlobalSet(b),
            I::Call(self.ctx.import(psx::SAFEPOINT)),
            I::End,
        ]
    }

    fn io_check(&mut self) -> Option<u32> {
        if self.ctx.device.contains(&(self.input, self.func)) {
            Some(self.ctx.import(psx::IO_ACCESS))
        } else {
            self.ctx.spec.debug_io.then(|| self.ctx.import(psx::IO_TRAP))
        }
    }
}

impl R<'_, '_> {
    /// A call from mod code that the mod itself answers: `psxmod_log`, an
    /// `original` name, or a function the mod defines (in this object, then
    /// in its others). `None`: a game function, resolved as the game's
    /// calls are.
    fn mod_call(&mut self, m: usize, symbol: u32, sig: &FuncType) -> Result<Option<Lowered>> {
        let inputs = self.ctx.inputs;
        let inp = &inputs[self.input];
        let s = &inp.obj.symbols[symbol as usize];
        let name = s.name.clone();
        if name == "psxmod_log" && !s.defined {
            return Ok(Some(Lowered::Call(self.ctx.import(psx::MOD_LOG))));
        }
        if let Some(b) = self.ctx.bindings.get(&(m, name.clone())).cloned() {
            return Ok(Some(match b {
                Bound::C(f, t) => {
                    ensure!(t == *sig, "{}: {name} is called as {sig:?}, but the function it wraps is {t:?}", inp.label);
                    Lowered::Call(f)
                }
                Bound::O32 { next, id } => {
                    Lowered::Call(self.ctx.synth(Synth::Transparent { input: self.input, name, sig: sig.clone(), next, id }))
                }
            }));
        }
        let Some((j, f)) = self.ctx.mod_function(m, self.input, symbol) else { return Ok(None) };
        let o = &inputs[j].obj;
        let t = &o.types[o.funcs[f as usize].ty as usize];
        ensure!(t == sig, "{}: {name} is called as {sig:?} but defined as {t:?} in {}", inp.label, inputs[j].label);
        Ok(Some(Lowered::Call(self.ctx.guest[&(j, f)])))
    }
}

/// Whether a call through `declared` reaches `defined` differently in wasm
/// than on the PS1, where both are the same o32 call: a struct passed by
/// value (or returned through a hidden pointer) on one side only, or
/// variadic on one side only. Narrow integers pass alike, and an
/// unprototyped declaration (`f()`) says nothing about the parameters.
pub fn abi_differs(declared: &psxw_cc::protos::Proto, defined: &psxw_cc::protos::Proto) -> bool {
    use psxw_cc::protos::Kind;
    let unprototyped = declared.params.is_empty() && declared.variadic;
    if unprototyped {
        return false;
    }
    let aggregate = |k: &Kind| matches!(k, Kind::ByVal { .. } | Kind::Sret { .. });
    let n = declared.params.len().max(defined.params.len());
    let params = (0..n).any(|i| {
        let (a, b) = (declared.params.get(i), defined.params.get(i));
        (a.is_some_and(aggregate) || b.is_some_and(aggregate)) && a != b
    });
    params || declared.variadic != defined.variadic
}

pub(crate) fn o32() -> FuncType {
    FuncType { params: vec![V::I32; 4], results: vec![V::I64] }
}

/// `(addr, a0..a3) -> i64` through the dispatch table, trapping into
/// `psx.untranslated` on an empty slot. `addr` is local `a`, arguments
/// locals `args`.
fn dispatch(f: &mut Function, a: u32, args: [u32; 4], untranslated: u32, o32_ty: u32) {
    f.instruction(&I::LocalGet(a));
    f.instruction(&I::I32Const(0x003F_FFFC));
    f.instruction(&I::I32And);
    f.instruction(&I::I32Const(2));
    f.instruction(&I::I32ShrU);
    f.instruction(&I::TableGet(0));
    f.instruction(&I::RefIsNull);
    f.instruction(&I::If(BlockType::Empty));
    f.instruction(&I::LocalGet(a));
    f.instruction(&I::Call(untranslated));
    f.instruction(&I::Return);
    f.instruction(&I::End);
    for l in args {
        f.instruction(&I::LocalGet(l));
    }
    f.instruction(&I::LocalGet(a));
    f.instruction(&I::I32Const(0x003F_FFFC));
    f.instruction(&I::I32And);
    f.instruction(&I::I32Const(2));
    f.instruction(&I::I32ShrU);
    f.instruction(&I::CallIndirect { type_index: o32_ty, table_index: 0 });
}

fn body(ctx: &mut Ctx, s: &Synth) -> Result<(FuncType, Function, String)> {
    let sp = ctx.sp;
    let window = ctx.spec.window_base;
    Ok(match s.clone() {
        Synth::ToO32 { input, name, sig, target } => {
            let proto = ctx.inputs[input].protos.get(&name);
            let (kinds, variadic) = marshal::param_kinds(&sig, proto);
            let (words, _) = marshal::words(&kinds, variadic);
            let f = marshal::c_to_o32(&sig, &words, sp, window, &|_| {}, &|_, _| {}, &|f| {
                f.instruction(&I::Call(target));
            });
            (sig, f, format!("o32_call:{name}"))
        }
        Synth::O32Wrap { input, func } => {
            let obj = &ctx.inputs[input].obj;
            let sig = obj.types[obj.funcs[func as usize].ty as usize].clone();
            let name = obj.func_symbol(func + obj.num_imported_funcs()).map_or_else(String::new, |s| s.name.clone());
            let guest_name = match classify_name(&name) {
                NameClass::Guest(n) => n.to_string(),
                _ => name.clone(),
            };
            let proto = ctx.inputs[input].protos.get(&guest_name);
            let callee = ctx.guest[&(input, func)];
            (o32(), marshal::o32_to_c(&sig, proto, callee, sp, window), format!("o32:{guest_name}"))
        }
        Synth::AdapterThunk(id) => {
            let mut f = Function::new([]);
            f.instruction(&I::I32Const(id as i32));
            for l in 0..4 {
                f.instruction(&I::LocalGet(l));
            }
            f.instruction(&I::Call(ctx.import("adapter")));
            f.instruction(&I::End);
            (o32(), f, format!("adapter#{id}"))
        }
        Synth::OverlayStub(addr) => {
            let cands = ctx.spec.overlays.get(&addr).cloned().unwrap_or_default();
            let mut entries = Vec::new();
            for (c, words) in &cands {
                let e = match ctx.heads.get(&(c.clone(), addr, None)) {
                    Some(h) => h.o32,
                    None => ctx.entry_raw(c, addr).context("overlay candidate without a tier")?,
                };
                entries.push((*words, e));
            }
            let mut f = Function::new([]);
            let word = |f: &mut Function, off: u32| {
                f.instruction(&I::I32Const(((addr + off) & WINDOW_MASK) as i32));
                f.instruction(&I::I32Load(MemArg { offset: window as u64, align: 2, memory_index: 0 }));
            };
            for (w, e) in entries {
                word(&mut f, 0);
                f.instruction(&I::I32Const(w[0] as i32));
                f.instruction(&I::I32Eq);
                word(&mut f, 4);
                f.instruction(&I::I32Const(w[1] as i32));
                f.instruction(&I::I32Eq);
                f.instruction(&I::I32And);
                f.instruction(&I::If(BlockType::Empty));
                for l in 0..4 {
                    f.instruction(&I::LocalGet(l));
                }
                f.instruction(&I::Call(e));
                f.instruction(&I::Return);
                f.instruction(&I::End);
            }
            f.instruction(&I::I32Const(addr as i32));
            f.instruction(&I::Call(ctx.import(psx::UNTRANSLATED)));
            f.instruction(&I::End);
            (o32(), f, format!("overlay:{addr:08X}"))
        }
        Synth::ICall(sig) => {
            let (kinds, _) = marshal::param_kinds(&sig, None);
            let (words, _) = marshal::words(&kinds, false);
            let mut helper = sig.clone();
            helper.params.push(V::I32);
            let target = sig.params.len() as u32;
            let (unt, o32_ty) = (ctx.import(psx::UNTRANSLATED), ctx.o32_ty);
            // The o32 arguments are already pushed when `call` runs; spill
            // them to fresh locals so the dispatch can push them again.
            let f = marshal::c_to_o32(&helper, &words, sp, window, &|_| {}, &|_, _| {}, &|f| {
                // stack: a0 a1 a2 a3 — leave them for the indirect call.
                f.instruction(&I::LocalGet(target));
                f.instruction(&I::I32Const(0x003F_FFFC));
                f.instruction(&I::I32And);
                f.instruction(&I::I32Const(2));
                f.instruction(&I::I32ShrU);
                f.instruction(&I::CallIndirect { type_index: o32_ty, table_index: 0 });
                let _ = unt;
            });
            (helper, f, "icall".into())
        }
        Synth::Indirect => {
            let mut f = Function::new([]);
            dispatch(&mut f, 0, [1, 2, 3, 4], ctx.import(psx::UNTRANSLATED), ctx.o32_ty);
            f.instruction(&I::End);
            (FuncType { params: vec![V::I32; 5], results: vec![V::I64] }, f, "call_indirect".into())
        }
        Synth::InvokeIrq => {
            // Locals 5 and 6: $sp and $gp at the call, restored when a
            // ReturnFromException unwinds (it returns to the interrupted
            // context, as the BIOS restores the saved registers). With
            // mods, 7..9: the mod stack and the hook frame, likewise.
            let mod_globals = if ctx.spec.mods.is_some() { vec![ctx.mod_sp, ctx.hook_sp, ctx.hook_id] } else { vec![] };
            let mut f = Function::new([(2 + mod_globals.len() as u32, ValType::I32)]);
            f.instruction(&I::GlobalGet(ctx.sp));
            f.instruction(&I::LocalSet(5));
            f.instruction(&I::GlobalGet(ctx.gp));
            f.instruction(&I::LocalSet(6));
            for (k, &g) in mod_globals.iter().enumerate() {
                f.instruction(&I::GlobalGet(g));
                f.instruction(&I::LocalSet(7 + k as u32));
            }
            f.instruction(&I::Block(BlockType::Empty));
            f.instruction(&I::TryTable(BlockType::Result(ValType::I64), vec![Catch::One { tag: RFE_TAG, label: 0 }].into()));
            dispatch(&mut f, 0, [1, 2, 3, 4], ctx.import(psx::UNTRANSLATED), ctx.o32_ty);
            f.instruction(&I::End);
            f.instruction(&I::Return);
            f.instruction(&I::End);
            f.instruction(&I::LocalGet(5));
            f.instruction(&I::GlobalSet(ctx.sp));
            f.instruction(&I::LocalGet(6));
            f.instruction(&I::GlobalSet(ctx.gp));
            for (k, &g) in mod_globals.iter().enumerate() {
                f.instruction(&I::LocalGet(7 + k as u32));
                f.instruction(&I::GlobalSet(g));
            }
            f.instruction(&I::I64Const(0));
            f.instruction(&I::End);
            (FuncType { params: vec![V::I32; 5], results: vec![V::I64] }, f, "invoke_irq".into())
        }
        Synth::Throw => {
            let mut f = Function::new([]);
            f.instruction(&I::Call(ctx.import(psx::RETURN_FROM_EXCEPTION)));
            f.instruction(&I::Throw(RFE_TAG));
            f.instruction(&I::End);
            (FuncType { params: vec![], results: vec![] }, f, "return_from_exception".into())
        }
        Synth::Support(name, sig) => (sig.clone(), crate::bodies::support(&name), name),
        Synth::Division(op, conv) => {
            let brk = ctx.import(psx::BREAK);
            let sig = FuncType { params: vec![V::I32; 2], results: vec![V::I32] };
            (sig, crate::bodies::division(op, conv, brk), format!("r3000_{op}"))
        }
        Synth::InlineCopy => {
            let sig = FuncType { params: vec![V::I32; 3], results: vec![V::I32] };
            (sig, crate::bodies::inline_copy(window, None), "builtin_memcpy".into())
        }
        Synth::Window => {
            let mut f = Function::new([]);
            f.instruction(&I::I32Const(window as i32));
            f.instruction(&I::End);
            (FuncType { params: vec![], results: vec![V::I32] }, f, "guest_window".into())
        }
        Synth::AsyncifyStub(name) => {
            let mut f = Function::new([]);
            let name: &'static str = name;
            let ty = if name == guest::ASYNCIFY_GET_STATE {
                f.instruction(&I::I32Const(0));
                FuncType { params: vec![], results: vec![V::I32] }
            } else {
                f.instruction(&I::Unreachable);
                let starts = [guest::ASYNCIFY_START_UNWIND, guest::ASYNCIFY_START_REWIND];
                FuncType { params: if starts.contains(&name) { vec![V::I32] } else { vec![] }, results: vec![] }
            };
            f.instruction(&I::End);
            (ty, f, name.to_string())
        }
        Synth::SpGet | Synth::GpGet => {
            let g = if *s == Synth::SpGet { ctx.sp } else { ctx.gp };
            let mut f = Function::new([]);
            f.instruction(&I::GlobalGet(g));
            f.instruction(&I::End);
            (FuncType { params: vec![], results: vec![V::I32] }, f, "reg_get".into())
        }
        Synth::SpSet | Synth::GpSet => {
            let g = if *s == Synth::SpSet { ctx.sp } else { ctx.gp };
            let mut f = Function::new([]);
            f.instruction(&I::LocalGet(0));
            f.instruction(&I::GlobalSet(g));
            f.instruction(&I::End);
            (FuncType { params: vec![V::I32], results: vec![] }, f, "reg_set".into())
        }
        Synth::O32WrapOf { callee, sig, input, name } => {
            let proto = ctx.inputs[input].protos.get(&name);
            (o32(), marshal::o32_to_c(&sig, proto, callee, sp, window), format!("o32:hook:{name}"))
        }
        Synth::CLayer { befores, core, afters, sig, input, name } => {
            let proto = ctx.inputs[input].protos.get(&name);
            (sig.clone(), c_layer(ctx.mod_sp, window, proto, &befores, core, &afters, &sig), format!("hook:{name}"))
        }
        Synth::O32Layer { befores, core, afters, returns } => (o32(), o32_layer(&befores, core, &afters, returns), "hook:o32".into()),
        Synth::ModWrap { input, func, hook } => {
            let sig = ctx.func_sig(input, func);
            let name = guest_name(ctx, input, func);
            let proto = ctx.inputs[input].protos.get(&name);
            let g = marshal::WrapGlobals { sp, scratch: ctx.mod_sp, hook: hook.map(|id| (ctx.hook_sp, ctx.hook_id, id)), window };
            let callee = ctx.guest[&(input, func)];
            (o32(), marshal::o32_to_c_with(&sig, proto, callee, marshal::Shape::O32, g), format!("o32:mod:{name}"))
        }
        Synth::ModAfter { input, func, result } => {
            let sig = ctx.func_sig(input, func);
            let name = guest_name(ctx, input, func);
            let proto = ctx.inputs[input].protos.get(&name);
            let g = marshal::WrapGlobals { sp, scratch: ctx.mod_sp, hook: None, window };
            let callee = ctx.guest[&(input, func)];
            let mut params = if result { vec![V::I64] } else { vec![] };
            params.extend([V::I32; 4]);
            let f = marshal::o32_to_c_with(&sig, proto, callee, marshal::Shape::After { result }, g);
            (FuncType { params, results: vec![] }, f, format!("o32:after:{name}"))
        }
        Synth::Transparent { input, name, sig, next, id } => {
            let proto = ctx.inputs[input].protos.get(&name);
            let (kinds, variadic) = marshal::param_kinds(&sig, proto);
            let (words, _) = marshal::words(&kinds, variadic);
            let f = marshal::c_to_o32_transparent(&sig, &words, sp, window, (ctx.hook_sp, ctx.hook_id, id), next);
            (sig, f, format!("original:{name}"))
        }
        Synth::SlotC { slot, callee, sig } => {
            let tix = ctx.intern(&sig);
            let mut f = Function::new([]);
            let n = sig.params.len() as u32;
            f.instruction(&I::I32Const(slot as i32));
            f.instruction(&I::TableGet(tables::CHAIN));
            f.instruction(&I::RefIsNull);
            let bt = sig.results.first().map_or(BlockType::Empty, |&t| BlockType::Result(marshal::val(t)));
            f.instruction(&I::If(bt));
            for l in 0..n {
                f.instruction(&I::LocalGet(l));
            }
            f.instruction(&I::Call(callee));
            f.instruction(&I::Else);
            for l in 0..n {
                f.instruction(&I::LocalGet(l));
            }
            f.instruction(&I::I32Const(slot as i32));
            f.instruction(&I::CallIndirect { type_index: tix, table_index: tables::CHAIN });
            f.instruction(&I::End);
            f.instruction(&I::End);
            (sig, f, format!("slot:{slot}"))
        }
        Synth::SlotO32 { slot, raw } => {
            let mut f = Function::new([]);
            f.instruction(&I::I32Const(slot as i32));
            f.instruction(&I::TableGet(tables::CHAIN));
            f.instruction(&I::RefIsNull);
            f.instruction(&I::If(BlockType::Result(ValType::I64)));
            for l in 0..4 {
                f.instruction(&I::LocalGet(l));
            }
            f.instruction(&I::Call(raw));
            f.instruction(&I::Else);
            for l in 0..4 {
                f.instruction(&I::LocalGet(l));
            }
            f.instruction(&I::I32Const(slot as i32));
            f.instruction(&I::CallIndirect { type_index: ctx.o32_ty, table_index: tables::CHAIN });
            f.instruction(&I::End);
            f.instruction(&I::End);
            (o32(), f, format!("slot:{slot}"))
        }
        Synth::ModInit => (FuncType { params: vec![], results: vec![] }, mod_init(ctx)?, "psxmod_init".into()),
        Synth::ModLoaded => (FuncType { params: vec![V::I32; 2], results: vec![] }, mod_loaded(ctx)?, "psxmod_loaded".into()),
    })
}

/// The guest name of a function (input, local function).
fn guest_name(ctx: &Ctx, input: usize, func: u32) -> String {
    let o = &ctx.inputs[input].obj;
    let name = o.func_symbol(func + o.num_imported_funcs()).map_or_else(String::new, |s| s.name.clone());
    match classify_name(&name) {
        NameClass::Guest(n) => n.to_string(),
        _ => name,
    }
}

/// An o32 hook layer: each before (an o32 wrapper, result dropped), then
/// `core`, then each after (taking the result first when `returns`), with
/// `a0..a3` and the guest `$sp` passed through untouched.
pub(crate) fn o32_layer(befores: &[u32], core: u32, afters: &[u32], returns: bool) -> Function {
    let mut f = Function::new([(1, ValType::I64)]);
    for &b in befores {
        for l in 0..4 {
            f.instruction(&I::LocalGet(l));
        }
        f.instruction(&I::Call(b));
        f.instruction(&I::Drop);
    }
    for l in 0..4 {
        f.instruction(&I::LocalGet(l));
    }
    f.instruction(&I::Call(core));
    f.instruction(&I::LocalSet(4));
    for &a in afters {
        if returns {
            f.instruction(&I::LocalGet(4));
        }
        for l in 0..4 {
            f.instruction(&I::LocalGet(l));
        }
        f.instruction(&I::Call(a));
    }
    f.instruction(&I::LocalGet(4));
    f.instruction(&I::End);
    f
}

/// A C-typed hook layer: the befores, then `core` (with copies of structs
/// passed by value, so the afters see the arguments as passed), then the
/// afters, each after taking the result first. `mod_sp`: the mod stack's
/// global; `window`: the guest window's wasm address; `proto`: the hooked
/// function's prototype facts.
pub(crate) fn c_layer(mod_sp: u32, window: u32, proto: Option<&psxw_cc::protos::Proto>, befores: &[(u32, bool)], core: u32, afters: &[(u32, bool)], sig: &FuncType) -> Function {
    let n = sig.params.len() as u32;
    let (kinds, _) = marshal::param_kinds(sig, proto);
    let byval: Vec<(u32, u32)> = if afters.iter().all(|(_, bare)| *bare) {
        Vec::new()
    } else {
        kinds.iter().enumerate().filter_map(|(i, k)| match k {
            psxw_cc::protos::Kind::ByVal { size, .. } => Some((i as u32, *size)),
            _ => None,
        }).collect()
    };
    // Locals: the result, the saved mod sp, one copy address per struct.
    let r = n;
    let save = n + 1;
    let copy = n + 2;
    let result = sig.results.first().map(|&t| marshal::val(t));
    let mut locals = vec![(1, result.unwrap_or(ValType::I32)), (1, ValType::I32)];
    if !byval.is_empty() {
        locals.push((byval.len() as u32, ValType::I32));
    }
    let mut f = Function::new(locals);
    for &(b, bare) in befores {
        if !bare {
            for l in 0..n {
                f.instruction(&I::LocalGet(l));
            }
        }
        f.instruction(&I::Call(b));
    }
    if !byval.is_empty() {
        f.instruction(&I::GlobalGet(mod_sp));
        f.instruction(&I::LocalSet(save));
        for (j, &(p, size)) in byval.iter().enumerate() {
            let at = copy + j as u32;
            f.instruction(&I::GlobalGet(mod_sp));
            f.instruction(&I::I32Const(size.div_ceil(8) as i32 * 8));
            f.instruction(&I::I32Sub);
            f.instruction(&I::LocalTee(at));
            f.instruction(&I::GlobalSet(mod_sp));
            for (l, _) in [(at, 0), (p, 0)] {
                f.instruction(&I::LocalGet(l));
                f.instruction(&I::I32Const(WINDOW_MASK as i32));
                f.instruction(&I::I32And);
                f.instruction(&I::I32Const(window as i32));
                f.instruction(&I::I32Add);
            }
            f.instruction(&I::I32Const(size as i32));
            f.instruction(&I::MemoryCopy { src_mem: 0, dst_mem: 0 });
        }
    }
    for l in 0..n {
        match byval.iter().position(|&(p, _)| p == l) {
            Some(j) => f.instruction(&I::LocalGet(copy + j as u32)),
            None => f.instruction(&I::LocalGet(l)),
        };
    }
    f.instruction(&I::Call(core));
    if result.is_some() {
        f.instruction(&I::LocalSet(r));
    }
    if !byval.is_empty() {
        f.instruction(&I::LocalGet(save));
        f.instruction(&I::GlobalSet(mod_sp));
    }
    for &(a, bare) in afters {
        if !bare {
            if result.is_some() {
                f.instruction(&I::LocalGet(r));
            }
            for l in 0..n {
                f.instruction(&I::LocalGet(l));
            }
        }
        f.instruction(&I::Call(a));
    }
    if result.is_some() {
        f.instruction(&I::LocalGet(r));
    }
    f.instruction(&I::End);
    f
}

/// The bytes a data patch writes.
fn patch_bytes(ctx: &Ctx, v: &PatchValue) -> Result<Vec<u8>> {
    Ok(match v {
        PatchValue::Bytes(b) => b.clone(),
        PatchValue::FnPtr(imp) => {
            let k = ctx.impl_func(imp)?;
            let a = ctx.synthetic.get(&k).with_context(|| format!("{} has no synthetic address", imp.function))?;
            a.to_le_bytes().to_vec()
        }
    })
}

pub(crate) fn store_byte(f: &mut Function, window: u32, addr: u32, b: u8) {
    f.instruction(&I::I32Const((addr & WINDOW_MASK) as i32));
    f.instruction(&I::I32Const(b as i32));
    f.instruction(&I::I32Store8(MemArg { offset: window as u64, align: 0, memory_index: 0 }));
}

/// `psxmod_init`: the executable's data patches, in load order.
fn mod_init(ctx: &mut Ctx) -> Result<Function> {
    if let Some((n, _)) = ctx.moddable {
        let t = ctx.intern(&FuncType { params: vec![], results: vec![] });
        return Ok(call_registered(n, tables::ON_INIT, t, 0));
    }
    let mut f = Function::new([]);
    for d in ctx.spec.mods.iter().flat_map(|m| &m.data).filter(|d| d.resident.is_none()) {
        for (k, b) in patch_bytes(ctx, &d.value)?.into_iter().enumerate() {
            store_byte(&mut f, ctx.spec.window_base, d.addr + k as u32, b);
        }
    }
    f.instruction(&I::End);
    Ok(f)
}

/// A moddable base's `psxmod_init`/`psxmod_loaded`: call each mod's,
/// registered in `table` (type `ty`, `params` i32 parameters passed on),
/// in load order; global `n` counts them.
fn call_registered(n: u32, table: u32, ty: u32, params: u32) -> Function {
    let i = params;
    let mut f = Function::new([(1, ValType::I32)]);
    f.instruction(&I::Block(BlockType::Empty));
    f.instruction(&I::Loop(BlockType::Empty));
    f.instruction(&I::LocalGet(i));
    f.instruction(&I::GlobalGet(n));
    f.instruction(&I::I32GeU);
    f.instruction(&I::BrIf(1));
    for p in 0..params {
        f.instruction(&I::LocalGet(p));
    }
    f.instruction(&I::LocalGet(i));
    f.instruction(&I::CallIndirect { type_index: ty, table_index: table });
    f.instruction(&I::LocalGet(i));
    f.instruction(&I::I32Const(1));
    f.instruction(&I::I32Add);
    f.instruction(&I::LocalSet(i));
    f.instruction(&I::Br(0));
    f.instruction(&I::End);
    f.instruction(&I::End);
    f.instruction(&I::End);
    f
}

/// The overlay data patches, in order: each with its pending flag's global.
fn overlay_patches<'a>(ctx: &Ctx<'a>) -> Vec<(&'a crate::mods::DataPatch, u32)> {
    ctx.spec
        .mods
        .iter()
        .flat_map(|m| &m.data)
        .filter(|d| d.resident.is_some())
        .enumerate()
        .map(|(k, d)| (d, ctx.pending + k as u32))
        .collect()
}

/// `psxmod_loaded(addr, len)`: a disc write over an overlay patch's bytes
/// marks it pending; a pending patch is applied once its overlay's first
/// code words are resident (the write that brings them may come before or
/// after the patch's, in another sector).
fn mod_loaded(ctx: &mut Ctx) -> Result<Function> {
    if let Some((_, n)) = ctx.moddable {
        let t = ctx.intern(&FuncType { params: vec![V::I32; 2], results: vec![] });
        return Ok(call_registered(n, tables::ON_LOADED, t, 2));
    }
    let mut f = Function::new([(2, ValType::I32)]);
    let (phys, end) = (2, 3);
    let patches = overlay_patches(ctx);
    if !patches.is_empty() {
        f.instruction(&I::LocalGet(0));
        f.instruction(&I::I32Const(0x1F_FFFF));
        f.instruction(&I::I32And);
        f.instruction(&I::LocalTee(phys));
        f.instruction(&I::LocalGet(1));
        f.instruction(&I::I32Add);
        f.instruction(&I::LocalSet(end));
    }
    for &(d, g) in &patches {
        let a = (d.addr & 0x1F_FFFF) as i32;
        // a < end && phys < a + len
        f.instruction(&I::I32Const(a));
        f.instruction(&I::LocalGet(end));
        f.instruction(&I::I32LtU);
        f.instruction(&I::LocalGet(phys));
        f.instruction(&I::I32Const(a + d.len() as i32));
        f.instruction(&I::I32LtU);
        f.instruction(&I::I32And);
        f.instruction(&I::If(BlockType::Empty));
        f.instruction(&I::I32Const(1));
        f.instruction(&I::GlobalSet(g));
        f.instruction(&I::End);
    }
    let window = ctx.spec.window_base;
    let load = |f: &mut Function, a: u32| {
        f.instruction(&I::I32Const((a & WINDOW_MASK) as i32));
        f.instruction(&I::I32Load(MemArg { offset: window as u64, align: 2, memory_index: 0 }));
    };
    let mut by: BTreeMap<String, Vec<(&crate::mods::DataPatch, u32)>> = BTreeMap::new();
    for &(d, g) in &patches {
        by.entry(d.container.clone()).or_default().push((d, g));
    }
    for group in by.values() {
        let id = group[0].0.resident.expect("overlay patch");
        load(&mut f, id.at);
        f.instruction(&I::I32Const(id.words[0] as i32));
        f.instruction(&I::I32Eq);
        load(&mut f, id.at + 4);
        f.instruction(&I::I32Const(id.words[1] as i32));
        f.instruction(&I::I32Eq);
        f.instruction(&I::I32And);
        f.instruction(&I::If(BlockType::Empty));
        for &(d, g) in group {
            f.instruction(&I::GlobalGet(g));
            f.instruction(&I::If(BlockType::Empty));
            for (k, b) in patch_bytes(ctx, &d.value)?.into_iter().enumerate() {
                store_byte(&mut f, ctx.spec.window_base, d.addr + k as u32, b);
            }
            f.instruction(&I::I32Const(0));
            f.instruction(&I::GlobalSet(g));
            f.instruction(&I::End);
        }
        f.instruction(&I::End);
    }
    f.instruction(&I::End);
    Ok(f)
}

/// The hook chains (plans/M11): every hooked function's entries, and the
/// next link each mod's `original` names.
fn build_chains(ctx: &mut Ctx) -> Result<()> {
    let Some(spec) = ctx.spec.mods.as_ref() else { return Ok(()) };
    let mut next_id = 1i32;
    for ((c, a), chains) in &spec.targets {
        let tier = *ctx.spec.tiers.get(&(c.clone(), *a)).with_context(|| format!("hooked {c} {a:#010x} is not a function"))?;
        // A C function's chain is C-typed, anything else's o32.
        let cfun = match tier {
            Tier::C { input, func } => Some((ctx.guest[&(input, func)], ctx.func_sig(input, func), input, func)),
            _ => None,
        };
        let bottom = match &cfun {
            Some((i, ..)) => Link::C(*i),
            None => Link::O32(ctx.entry_raw(c, *a).context("hooked function without an entry")?),
        };
        let what = format!("{c} {a:#010x}");
        let mut top = bottom;
        for l in &chains.default {
            top = realize(ctx, l, top, cfun.as_ref(), chains.returns, &mut next_id, &what)?;
        }
        let head = make_head(ctx, top, cfun.as_ref());
        ctx.heads.insert((c.clone(), *a, None), head);
        for (caller, layers) in &chains.from {
            let mut scoped = top;
            for l in layers {
                scoped = realize(ctx, l, scoped, cfun.as_ref(), chains.returns, &mut next_id, &what)?;
            }
            let head = make_head(ctx, scoped, cfun.as_ref());
            ctx.heads.insert((c.clone(), *a, Some(caller.clone())), head);
        }
    }
    Ok(())
}

fn make_head(ctx: &mut Ctx, top: Link, cfun: Option<&(u32, FuncType, usize, u32)>) -> Head {
    match (top, cfun) {
        (Link::C(i), Some((own, sig, input, func))) => {
            let o32 = if i == *own {
                ctx.synth(Synth::O32Wrap { input: *input, func: *func })
            } else {
                let name = guest_name(ctx, *input, *func);
                ctx.synth(Synth::O32WrapOf { callee: i, sig: sig.clone(), input: *input, name })
            };
            Head { c: Some((i, sig.clone())), o32 }
        }
        (Link::O32(i), _) | (Link::C(i), None) => Head { c: None, o32: i },
    }
}

/// One mod's layer over `next`.
fn realize(
    ctx: &mut Ctx,
    l: &Layer,
    next: Link,
    cfun: Option<&(u32, FuncType, usize, u32)>,
    returns: bool,
    next_id: &mut i32,
    what: &str,
) -> Result<Link> {
    let id = &ctx.spec.mods.as_ref().expect("mods").mods[l.module].id;
    let id = id.clone();
    let bind = |ctx: &mut Ctx, name: &str, b: Bound| -> Result<()> {
        ensure!(ctx.bindings.insert((l.module, name.to_string()), b).is_none(), "mod {id} names two next links {name}");
        Ok(())
    };
    match (cfun, next) {
        (Some((_, sig, finput, ffunc)), Link::C(next)) => {
            let core = match &l.patch {
                Some(p) => {
                    let (i, f) = ctx.impl_func(&p.imp)?;
                    let t = ctx.func_sig(i, f);
                    ensure!(t == *sig, "{id}: patch {} is {t:?}, but {what} is {sig:?}", p.imp.function);
                    if let Some(o) = &p.original {
                        bind(ctx, o, Bound::C(next, sig.clone()))?;
                    }
                    ctx.guest[&(i, f)]
                }
                None => next,
            };
            if l.befores.is_empty() && l.afters.is_empty() {
                return Ok(Link::C(core));
            }
            let unit = FuncType { params: vec![], results: vec![] };
            let mut befores = Vec::new();
            for b in &l.befores {
                let (i, f) = ctx.impl_func(&b.imp)?;
                let t = ctx.func_sig(i, f);
                let want = if b.bare { unit.clone() } else { FuncType { params: sig.params.clone(), results: vec![] } };
                ensure!(t == want, "{id}: before {} is {t:?}, but {what} takes {want:?}", b.imp.function);
                befores.push((ctx.guest[&(i, f)], b.bare));
            }
            let mut afters = Vec::new();
            for a in &l.afters {
                let (i, f) = ctx.impl_func(&a.imp)?;
                let t = ctx.func_sig(i, f);
                let mut params = sig.results.clone();
                params.extend(sig.params.iter().copied());
                let want = if a.bare { unit.clone() } else { FuncType { params, results: vec![] } };
                ensure!(t == want, "{id}: after {} is {t:?}, but {what} wants {want:?} (the result first)", a.imp.function);
                afters.push((ctx.guest[&(i, f)], a.bare));
            }
            let name = guest_name(ctx, *finput, *ffunc);
            Ok(Link::C(ctx.synth(Synth::CLayer { befores, core, afters, sig: sig.clone(), input: *finput, name })))
        }
        (None, Link::O32(next)) => {
            let core = match &l.patch {
                Some(p) => {
                    let (i, f) = ctx.impl_func(&p.imp)?;
                    let hook = match &p.original {
                        Some(o) => {
                            let hid = *next_id;
                            *next_id += 1;
                            bind(ctx, o, Bound::O32 { next, id: hid })?;
                            Some(hid)
                        }
                        None => None,
                    };
                    ctx.synth(Synth::ModWrap { input: i, func: f, hook })
                }
                None => next,
            };
            if l.befores.is_empty() && l.afters.is_empty() {
                return Ok(Link::O32(core));
            }
            let mut befores = Vec::new();
            for b in &l.befores {
                let (i, f) = ctx.impl_func(&b.imp)?;
                befores.push(ctx.synth(Synth::ModWrap { input: i, func: f, hook: None }));
            }
            let mut afters = Vec::new();
            for a in &l.afters {
                let (i, f) = ctx.impl_func(&a.imp)?;
                afters.push(ctx.synth(Synth::ModAfter { input: i, func: f, result: returns && !a.bare }));
            }
            Ok(Link::O32(ctx.synth(Synth::O32Layer { befores, core, afters, returns })))
        }
        _ => bail!("{what}: a chain mixes C and o32 links"),
    }
}

/// Link the guest module.
pub fn link(map: &AddressMap, rodata: Option<&Rodata>, spec: &GameSpec, inputs: &[Input]) -> Result<Linked> {
    let view: Vec<(&str, &WasmObject)> = inputs.iter().map(|i| (i.container.as_str(), &i.obj)).collect();
    // An empty mod set links exactly as no mods.
    let mods = spec.mods.as_ref().filter(|m| !m.is_empty());
    let mut mod_of: Vec<Option<usize>> = vec![None; inputs.len()];
    for (m, mi) in mods.iter().flat_map(|s| s.mods.iter().enumerate()) {
        for &i in &mi.inputs {
            *mod_of.get_mut(i).context("mod input out of range")? = Some(m);
        }
    }
    let layout = Layout::place_mods(&view, map, rodata, &mod_of)?;
    let spec_own;
    let spec = if spec.mods.is_some() && mods.is_none() {
        spec_own = GameSpec { mods: None, ..spec.clone() };
        &spec_own
    } else {
        spec
    };
    let mut ctx = Ctx {
        map,
        inputs,
        view,
        layout,
        spec,
        types: Vec::new(),
        type_ix: HashMap::new(),
        imports: HashMap::new(),
        nimports: 0,
        guest: HashMap::new(),
        guest_order: Vec::new(),
        synth: Vec::new(),
        synth_ix: HashMap::new(),
        sp: 0,
        gp: 1,
        budget: 2,
        chain: 3,
        abi_mismatches: BTreeSet::new(),
        unresolved: BTreeSet::new(),
        mod_of,
        mod_sp: 4,
        hook_sp: 5,
        hook_id: 6,
        pending: 7,
        synthetic: BTreeMap::new(),
        heads: HashMap::new(),
        bindings: HashMap::new(),
        caller_of: HashMap::new(),
        moddable: None,
        o32_ty: 0,
        device: spec
            .device_funcs
            .iter()
            .filter_map(|k| match spec.tiers.get(k) {
                Some(Tier::Lift { input, func } | Tier::C { input, func }) => Some((*input, *func)),
                _ => None,
            })
            .collect(),
    };
    ctx.o32_ty = ctx.intern(&o32());
    let mut import_types = Vec::new();
    for (i, (name, params, results)) in psx_imports().into_iter().enumerate() {
        let t = ctx.intern(&FuncType { params, results });
        ctx.imports.insert(name, i as u32);
        import_types.push((name, t));
    }
    ctx.nimports = import_types.len() as u32;

    for (i, inp) in inputs.iter().enumerate() {
        for f in 0..inp.obj.funcs.len() as u32 {
            let index = f + inp.obj.num_imported_funcs();
            let sym = inp.obj.func_symbol(index);
            if sym.is_some_and(|s| !matches!(classify_name(&s.name), NameClass::Guest(_))) {
                continue;
            }
            let out = ctx.nimports + ctx.guest_order.len() as u32;
            ctx.guest.insert((i, f), out);
            ctx.guest_order.push((i, f));
        }
    }

    // Game functions' own addresses (`from` scopes), and mod functions'
    // synthetic ones, in input order.
    let has_mods = spec.mods.is_some();
    if has_mods {
        let mut next = psxw_abi::SYNTHETIC_CODE.start;
        for &(i, f) in &ctx.guest_order {
            let inp = &inputs[i];
            let index = f + inp.obj.num_imported_funcs();
            let Some(sym) = inp.obj.func_symbol(index) else { continue };
            if ctx.mod_of[i].is_some() {
                ensure!(next < psxw_abi::SYNTHETIC_CODE.end, "mods define more functions than synthetic addresses");
                ctx.synthetic.insert((i, f), next);
                next += 4;
                continue;
            }
            let key = match sym.name.strip_prefix("psxw_t_") {
                Some(rest) => rest.rsplit('_').next().and_then(|h| u32::from_str_radix(h, 16).ok()),
                None => match classify_name(&sym.name) {
                    NameClass::Guest(g) if !inp.generated => map.resolve(&inp.container, g).map(|r| r.addr),
                    _ => None,
                },
            };
            if let Some(a) = key {
                ctx.caller_of.insert((i, f), (inp.container.clone(), a));
            }
        }
        build_chains(&mut ctx)?;
    }
    // A moddable base: every slot's entries check it.
    let mut slot_sigs: Vec<Option<FuncType>> = Vec::new();
    let mut entries_raw: Vec<(u32, u32)> = Vec::new();
    if let Some(slots) = &spec.slots {
        ensure!(!has_mods, "a moddable base links no mods itself");
        ctx.moddable = Some((4, 5));
        for (s, (c, a)) in slots.iter().enumerate() {
            let s = s as u32;
            let tier = *spec.tiers.get(&(c.clone(), *a)).with_context(|| format!("slot {c} {a:#010x} is not a function"))?;
            let head = match tier {
                Tier::C { input, func } => {
                    let callee = ctx.guest[&(input, func)];
                    let sig = ctx.func_sig(input, func);
                    let entry = ctx.synth(Synth::SlotC { slot: s, callee, sig: sig.clone() });
                    let name = guest_name(&ctx, input, func);
                    let o32 = ctx.synth(Synth::O32WrapOf { callee: entry, sig: sig.clone(), input, name });
                    slot_sigs.push(Some(sig.clone()));
                    entries_raw.push((entry, callee));
                    Head { c: Some((entry, sig)), o32 }
                }
                _ => {
                    let raw = ctx.entry_raw(c, *a).context("slot without an entry")?;
                    let entry = ctx.synth(Synth::SlotO32 { slot: s, raw });
                    slot_sigs.push(None);
                    entries_raw.push((entry, raw));
                    Head { c: None, o32: entry }
                }
            };
            ctx.heads.insert((c.clone(), *a, None), head);
        }
    }

    let mut bodies = Vec::new();
    let mut names: Vec<(u32, String)> = Vec::new();
    // With debug info: where each input instruction landed, per function.
    let mut moved: HashMap<(usize, u32), Vec<(u32, u32)>> = HashMap::new();
    for &(i, f) in &ctx.guest_order.clone() {
        let obj = &inputs[i].obj;
        let ty = obj.types[obj.funcs[f as usize].ty as usize].clone();
        let tix = ctx.intern(&ty);
        let index = f + obj.num_imported_funcs();
        let name = obj.func_symbol(index).map_or_else(|| format!("{}#{f}", inputs[i].label), |s| s.name.clone());
        let mut ops = Vec::new();
        let track = spec.debug_info && !obj.debug.is_empty();
        let b = rewrite::rewrite_mapped(obj, f as usize, &mut R { ctx: &mut ctx, input: i, func: f }, track.then_some(&mut ops))
            .with_context(|| format!("{}: {name}", inputs[i].label))?;
        if track {
            moved.insert((i, f), ops);
        }
        names.push((ctx.guest[&(i, f)], format!("{}:{name}", inputs[i].container)));
        bodies.push((tix, b));
    }

    // Table entries for every function with an address, and the exports.
    let mut elements: BTreeMap<u32, u32> = BTreeMap::new();
    for (c, addr) in spec.tiers.keys().cloned().collect::<Vec<_>>() {
        let e = if spec.overlays.contains_key(&addr) {
            ctx.synth(Synth::OverlayStub(addr))
        } else {
            ctx.entry(&c, addr).context("entry")?
        };
        elements.insert(table_index(addr), e);
    }
    // Mod functions, at their synthetic addresses.
    for (&(i, f), &addr) in &ctx.synthetic.clone() {
        let e = ctx.synth(Synth::ModWrap { input: i, func: f, hook: None });
        elements.insert(table_index(addr), e);
    }
    let mod_init = ctx.synth(Synth::ModInit);
    let mod_loaded = ctx.synth(Synth::ModLoaded);
    let invoke = ctx.synth(Synth::Indirect);
    let invoke_irq = ctx.synth(Synth::InvokeIrq);
    let unit_ty = ctx.intern(&FuncType { params: vec![], results: vec![] });
    let reg_fns = [
        ("guest_sp_get", ctx.synth(Synth::SpGet)),
        ("guest_sp_set", ctx.synth(Synth::SpSet)),
        ("guest_gp_get", ctx.synth(Synth::GpGet)),
        ("guest_gp_set", ctx.synth(Synth::GpSet)),
        (guest::WINDOW, ctx.synth(Synth::Window)),
    ];
    // Asyncify adds the real ones after the link when the game switches
    // threads (D57).
    let asyncify_stubs: Vec<(&str, u32)> = if spec.threads {
        Vec::new()
    } else {
        [
            guest::ASYNCIFY_START_UNWIND,
            guest::ASYNCIFY_STOP_UNWIND,
            guest::ASYNCIFY_START_REWIND,
            guest::ASYNCIFY_STOP_REWIND,
            guest::ASYNCIFY_GET_STATE,
        ]
        .into_iter()
        .map(|n| (n, ctx.synth(Synth::AsyncifyStub(n))))
        .collect()
    };

    let mut si = 0;
    while si < ctx.synth.len() {
        let s = ctx.synth[si].clone();
        let (ty, b, name) = body(&mut ctx, &s)?;
        let tix = ctx.intern(&ty);
        names.push((ctx.nimports + ctx.guest_order.len() as u32 + si as u32, name));
        bodies.push((tix, b));
        si += 1;
    }

    let mut module = Module::new();
    let mut types = TypeSection::new();
    for t in &ctx.types {
        types.ty().function(t.params.iter().map(|&v| marshal::val(v)), t.results.iter().map(|&v| marshal::val(v)));
    }
    module.section(&types);
    let mut imports = ImportSection::new();
    for (name, t) in &import_types {
        imports.import(psx::MODULE, name, EntityType::Function(*t));
    }
    module.section(&imports);
    let mut functions = FunctionSection::new();
    for (t, _) in &bodies {
        functions.function(*t);
    }
    module.section(&functions);
    let mut tables = TableSection::new();
    tables.table(TableType {
        element_type: RefType::FUNCREF,
        minimum: TABLE_LEN as u64,
        maximum: Some(TABLE_LEN as u64),
        table64: false,
        shared: false,
    });
    if let Some(slots) = &spec.slots {
        let n = slots.len() as u64;
        for (min, max) in [(n, None), (n, Some(n)), (n, Some(n)), (tables::MAX_MODS as u64, Some(tables::MAX_MODS as u64)), (tables::MAX_MODS as u64, Some(tables::MAX_MODS as u64))] {
            tables.table(TableType { element_type: RefType::FUNCREF, minimum: min, maximum: max, table64: false, shared: false });
        }
    }
    module.section(&tables);
    let mut memory = MemorySection::new();
    memory.memory(MemoryType { minimum: spec.min_pages, maximum: None, memory64: false, shared: false, page_size_log2: None });
    module.section(&memory);
    let mut tags = TagSection::new();
    tags.tag(TagType { kind: TagKind::Exception, func_type_idx: unit_ty });
    module.section(&tags);
    let mut globals = GlobalSection::new();
    let mut inits = vec![0, 0, spec.budget as i32, 0];
    if has_mods {
        // The mod stack (a guest address), the hook frame's sp and id, then
        // a pending flag per overlay data patch.
        inits.extend([(0x8000_0000u32 | psxw_abi::phys::MOD_STACK.end) as i32, 0, 0]);
        inits.extend(overlay_patches(&ctx).iter().map(|_| 0));
    }
    if spec.slots.is_some() {
        // How many mods' init and loaded functions are registered.
        inits.extend([0, 0]);
    }
    for init in inits {
        globals.global(GlobalType { val_type: ValType::I32, mutable: true, shared: false }, &ConstExpr::i32_const(init));
    }
    module.section(&globals);
    let mut ex = ExportSection::new();
    ex.export(guest::MEMORY, ExportKind::Memory, 0);
    ex.export(guest::INVOKE, ExportKind::Func, invoke);
    ex.export(guest::INVOKE_IRQ, ExportKind::Func, invoke_irq);
    ex.export(guest::MOD_INIT, ExportKind::Func, mod_init);
    ex.export(guest::MOD_LOADED, ExportKind::Func, mod_loaded);
    for (n, f) in reg_fns.into_iter().chain(asyncify_stubs) {
        ex.export(n, ExportKind::Func, f);
    }
    ex.export(guest::SP, ExportKind::Global, ctx.sp);
    ex.export(guest::GP, ExportKind::Global, ctx.gp);
    if let Some((ni, nl)) = ctx.moddable {
        for (name, t) in [
            (exports::DISPATCH, tables::DISPATCH),
            (exports::CHAIN, tables::CHAIN),
            (exports::ENTRIES, tables::ENTRIES),
            (exports::RAW, tables::RAW),
            (exports::ON_INIT, tables::ON_INIT),
            (exports::ON_LOADED, tables::ON_LOADED),
        ] {
            ex.export(name, ExportKind::Table, t);
        }
        ex.export(exports::N_INIT, ExportKind::Global, ni);
        ex.export(exports::N_LOADED, ExportKind::Global, nl);
    }
    module.section(&ex);
    let mut elems = ElementSection::new();
    // Contiguous runs of table slots share a segment.
    let mut run: Vec<u32> = Vec::new();
    let mut run_start = 0;
    let flush = |elems: &mut ElementSection, start: u32, run: &mut Vec<u32>| {
        if !run.is_empty() {
            elems.active(Some(0), &ConstExpr::i32_const(start as i32), Elements::Functions(run.clone().into()));
            run.clear();
        }
    };
    for (&slot, &f) in &elements {
        if run.is_empty() || slot != run_start + run.len() as u32 {
            flush(&mut elems, run_start, &mut run);
            run_start = slot;
        }
        run.push(f);
    }
    flush(&mut elems, run_start, &mut run);
    if !entries_raw.is_empty() {
        let entries: Vec<u32> = entries_raw.iter().map(|e| e.0).collect();
        let raws: Vec<u32> = entries_raw.iter().map(|e| e.1).collect();
        elems.active(Some(tables::ENTRIES), &ConstExpr::i32_const(0), Elements::Functions(entries.into()));
        elems.active(Some(tables::RAW), &ConstExpr::i32_const(0), Elements::Functions(raws.into()));
    }
    module.section(&elems);
    let mut code = CodeSection::new();
    for (_, b) in &bodies {
        code.function(b);
    }
    module.section(&code);
    let mut data = DataSection::new();
    let mut aux = Vec::new();
    for &(i, si, addr) in &ctx.layout.aux.clone() {
        let fa = |input: usize, sym: u32| -> Result<u32> {
            if let Some(a) = ctx.mod_of[input].and_then(|m| ctx.mod_func_addr(m, input, sym)) {
                return Ok(a);
            }
            let s = &ctx.inputs[input].obj.symbols[sym as usize];
            let n = match classify_name(&s.name) {
                NameClass::Guest(n) | NameClass::SignatureThunk { target: n } => n,
                NameClass::MainWrapper => bail!("main wrapper"),
            };
            ctx.map.resolve(&ctx.inputs[input].container, n).map(|r| r.addr).context("function without address")
        };
        let bytes = ctx.layout.bytes_of(&ctx.view, map, i, si, &fa)?;
        data.active(0, &ConstExpr::i32_const((spec.window_base + (addr & WINDOW_MASK)) as i32), bytes);
        aux.push((inputs[i].label.clone(), inputs[i].obj.symbols[si as usize].name.clone(), addr));
    }
    module.section(&data);
    let mut fnames = NameMap::new();
    names.sort_by_key(|(i, _)| *i);
    for (i, n) in &names {
        fnames.append(*i, n);
    }
    let mut ns = NameSection::new();
    ns.functions(&fnames);
    module.section(&ns);
    module.section(&CustomSection { name: "psxw.abi".into(), data: psxw_abi::ABI_VERSION.to_le_bytes().to_vec().into() });
    let matched = ctx.layout.defined.values().filter(|p| p.how == crate::layout::How::Matched).count();
    let mut wasm = module.finish();
    if spec.debug_info {
        debug_info(&ctx, &moved, &mut wasm)?;
    }
    ensure!(
        ctx.unresolved.is_empty(),
        "{} generated calls go to addresses where no container defines a function:\n  {}",
        ctx.unresolved.len(),
        ctx.unresolved.iter().cloned().collect::<Vec<_>>().join("\n  ")
    );
    Ok(Linked { wasm, aux, matched, abi_mismatches: ctx.abi_mismatches.iter().cloned().collect() })
}

/// The inputs' DWARF, for where the guest passes put their code
/// ([`crate::dwarf`]), appended to `wasm`.
fn debug_info(ctx: &Ctx, moved: &HashMap<(usize, u32), Vec<(u32, u32)>>, wasm: &mut Vec<u8>) -> Result<()> {
    use wasm_encoder::Section;
    let starts = crate::dwarf::body_starts(wasm)?;
    let mut by_input: BTreeMap<usize, Vec<crate::dwarf::FuncMap>> = BTreeMap::new();
    for (&(i, f), ops) in moved {
        let obj = &ctx.inputs[i].obj;
        let out = ctx.guest[&(i, f)] - ctx.nimports;
        by_input.entry(i).or_default().push(crate::dwarf::FuncMap {
            from: (obj.funcs[f as usize].body.start - obj.code_start) as u32,
            to: starts[out as usize],
            ops: ops.clone(),
        });
    }
    let mut dwarf = crate::dwarf::Dwarf::new();
    for (i, funcs) in by_input {
        let code = crate::dwarf::CodeMap::new(funcs);
        let (view, map, layout) = (&ctx.view, ctx.map, &ctx.layout);
        let mut data_addr = |sym: u32| layout.data_addr(view, map, i, sym).ok();
        let (sp, mod_sp, is_mod) = (ctx.sp, ctx.mod_sp, ctx.mod_of[i].is_some());
        let obj = &ctx.inputs[i].obj;
        let mut global = |sym: u32| match obj.symbols.get(sym as usize).map(|s| s.name.as_str()) {
            Some("__stack_pointer") => Some(if is_mod { mod_sp } else { sp }),
            _ => None,
        };
        dwarf.add_object(obj, &code, &mut data_addr, &mut global).with_context(|| format!("{}: debug info", ctx.inputs[i].label))?;
    }
    for (name, data) in dwarf.finish()? {
        CustomSection { name: name.into(), data: data.into() }.append_to(wasm);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::abi_differs;
    use psxw_cc::protos::{Kind, Proto};

    fn proto(params: Vec<Kind>, variadic: bool) -> Proto {
        Proto { name: "f".into(), defined: false, ret: Kind::I32, params, variadic }
    }

    #[test]
    fn abi_differences() {
        let by_val = Kind::ByVal { size: 4, align: 2 };
        // A 4-byte struct by value against the word it occupies in o32.
        assert!(abi_differs(&proto(vec![Kind::Ptr, by_val], false), &proto(vec![Kind::Ptr, Kind::I32], false)));
        // Narrow integers and pointers pass alike; an unprototyped
        // declaration says nothing.
        assert!(!abi_differs(&proto(vec![Kind::I16 { signed: true }], false), &proto(vec![Kind::I32], false)));
        assert!(!abi_differs(&proto(vec![Kind::I32], false), &proto(vec![Kind::Ptr], false)));
        assert!(!abi_differs(&proto(vec![], true), &proto(vec![by_val], false)));
        // Variadic on one side only.
        assert!(abi_differs(&proto(vec![Kind::Ptr], true), &proto(vec![Kind::Ptr, Kind::I32], false)));
    }
}
