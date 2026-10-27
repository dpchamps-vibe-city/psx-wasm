//! A mod as its own wasm module (plans/M11 phase 2), which a moddable base
//! loads at run time. The same objects and declarations as a static link
//! (`mods`), the same guest pass and the same hook layers; what the static
//! link fixes, the loader binds:
//!
//! - **Its area.** The mod's data is one image the loader places at
//!   `psxmod.data_base` (an address in the mod area); its own data
//!   addresses are relative to that global, so any slice of the mod area
//!   will do. Its stack sits above the image (`psxmod.stack_top`).
//! - **Its functions' addresses.** Synthetic, from `psxmod.code_base`; its
//!   element segment puts each function's o32 entry into the game's
//!   dispatch table at `psxmod.code_index`.
//! - **The game's functions** it calls: a C function through its entry, in
//!   its own type, as the static link calls it directly (the module's
//!   `psxmod_calls` table, which the loader fills from the base's
//!   `psxmod_entries` by address); anything else through the dispatch
//!   table, o32.
//! - **Each hooked function's next link**: `psxmod.chain` at `original.<k>`.
//!
//! Exports: `hook.<k>` (the mod's layer on hooked function `k`, in the
//! slot's type), `psxmod_init` (place the image, apply the executable's
//! data patches and any for overlays resident now), `psxmod_resume` (the
//! same keeping the image: a hot swap with the same data layout) and
//! `psxmod_loaded` (overlay data patches as the disc writes them).

use crate::game::{c_layer, o32, o32_layer, service, store_byte};
use crate::link::Input;
use crate::marshal;
use crate::mods::{DataPatch, Impl, Layer, PatchValue};
use crate::object::{FuncType, NameClass, RelocKind, SymbolKind, classify_name};
use crate::rewrite::{self, Lowered, Resolver};
use anyhow::{Context as _, Result, bail, ensure};
use psxw_abi::{WINDOW_BASE, WINDOW_MASK, table_index};
use psxw_inputs::AddressMap;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, CustomSection, DataSection, ElementSection, Elements, EntityType, ExportKind,
    ExportSection, Function, FunctionSection, GlobalSection, GlobalType, ImportSection, Instruction as I, MemArg,
    MemoryType, Module, NameMap, NameSection, RefType, TableType, TypeSection, ValType,
};
use wasmparser::ValType as V;

/// Import names.
pub mod names {
    pub const PSX: &str = "psx";
    pub const MEMORY: &str = "memory";
    pub const DISPATCH: &str = "dispatch";
    /// The game's guest `$sp` (mutable i32).
    pub const SP: &str = "sp";
    pub const LOG: &str = "mod_log";
    pub const UNTRANSLATED: &str = "untranslated";
    pub const PSXMOD: &str = "psxmod";
    pub const CHAIN: &str = "chain";
    pub const ENTRIES: &str = "entries";
    pub const DATA_BASE: &str = "data_base";
    pub const CODE_INDEX: &str = "code_index";
    pub const CODE_BASE: &str = "code_base";
    pub const STACK_TOP: &str = "stack_top";
    /// Module of the i32 globals naming each hooked function's next link
    /// (`original.<k>`), and of each called game function's entry index
    /// (`game.<k>`).
    pub const ORIGINAL: &str = "original";
    pub const GAME: &str = "game";
    pub const INIT: &str = "psxmod_init";
    pub const RESUME: &str = "psxmod_resume";
    pub const LOADED: &str = "psxmod_loaded";
    /// The custom section with the module's declarations (JSON).
    pub const DECL_SECTION: &str = "psxmod.decl";
}

/// A function the mod hooks.
#[derive(Debug, Clone)]
pub struct ModTarget {
    pub container: String,
    pub addr: u32,
    /// Its C type when it is decompiled C (a C-typed chain), else o32.
    pub sig: Option<FuncType>,
    pub returns: bool,
    /// The mod's layer on it (`module` unused).
    pub layer: Layer,
}

#[derive(Debug, Clone)]
pub struct ModuleSpec {
    pub id: String,
    pub targets: Vec<ModTarget>,
    /// Indices of inputs refer to the module's inputs.
    pub data: Vec<DataPatch>,
    /// Game functions with an entry (`psxmod.entries`), and their C type
    /// when they are decompiled C.
    pub entries: BTreeMap<(String, u32), Option<FuncType>>,
    pub stack_size: u32,
}

/// A game function the mod calls through its entry, in its own type: slot
/// `k` of the module's `psxmod_calls` table, which the loader fills from
/// the base's `psxmod_entries`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameCall {
    pub container: String,
    pub addr: u32,
    pub sig: FuncType,
}

#[derive(Debug, Clone)]
pub struct Linked {
    pub wasm: Vec<u8>,
    /// Bytes of the data image.
    pub data_size: u32,
    /// The mod's functions (synthetic addresses it needs).
    pub functions: u32,
    /// Hash of the data image's layout (names, offsets, sizes): a hot swap
    /// keeps the mod's state only when it is unchanged.
    pub layout_hash: String,
    /// The module's `psxmod_calls`, in order.
    pub calls: Vec<GameCall>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum S {
    NextC { k: u32, sig: FuncType },
    NextO32 { k: u32 },
    GameC { k: u32, sig: FuncType },
    GameO32 { addr: u32 },
    ToO32 { input: usize, name: String, sig: FuncType, target: u32 },
    ICall(FuncType),
    Indirect,
    Support(String, FuncType),
    CLayer { befores: Vec<(u32, bool)>, core: u32, afters: Vec<(u32, bool)>, sig: FuncType, input: usize, name: String },
    O32Layer { befores: Vec<u32>, core: u32, afters: Vec<u32>, returns: bool },
    ModWrap { input: usize, func: u32, hook: Option<i32> },
    ModAfter { input: usize, func: u32, result: bool },
    Transparent { input: usize, name: String, sig: FuncType, next: u32, id: i32 },
    Init { fresh: bool },
    Loaded,
}

#[derive(Debug, Clone)]
enum Bound {
    C(u32, FuncType),
    O32 { next: u32, id: i32 },
}

/// A word of the image the init writes when it places it.
#[derive(Debug, Clone, Copy)]
enum Fixup {
    /// `data_base + off`.
    Data(u32),
    /// `code_base + 4 × function`.
    Func(u32),
}

struct M<'a> {
    map: &'a AddressMap,
    inputs: &'a [Input],
    spec: &'a ModuleSpec,
    main: String,
    types: Vec<FuncType>,
    type_ix: HashMap<FuncType, u32>,
    imports: Vec<(&'static str, String, FuncType)>,
    import_ix: HashMap<String, u32>,
    guest: HashMap<(usize, u32), u32>,
    guest_order: Vec<(usize, u32)>,
    synth: Vec<S>,
    synth_ix: HashMap<S, u32>,
    /// Own data: (input, symbol) → image offset.
    data: HashMap<(usize, u32), u32>,
    image: Vec<u8>,
    fixups: Vec<(u32, Fixup)>,
    layout: Vec<(String, u32, u32)>,
    bindings: HashMap<String, Bound>,
    calls: Vec<GameCall>,
    // Globals: imported, then defined.
    g_sp: u32,
    g_data_base: u32,
    g_code_index: u32,
    g_code_base: u32,
    g_stack_top: u32,
    g_original: u32,
    g_mod_sp: u32,
    g_hook_sp: u32,
    g_hook_id: u32,
    g_pending: u32,
    o32_ty: u32,
}

impl M<'_> {
    fn intern(&mut self, t: &FuncType) -> u32 {
        if let Some(&i) = self.type_ix.get(t) {
            return i;
        }
        let i = self.types.len() as u32;
        self.types.push(t.clone());
        self.type_ix.insert(t.clone(), i);
        i
    }

    fn nimports(&self) -> u32 {
        self.imports.len() as u32
    }

    fn synth(&mut self, s: S) -> u32 {
        if let Some(&i) = self.synth_ix.get(&s) {
            return i;
        }
        let i = self.nimports() + self.guest_order.len() as u32 + self.synth.len() as u32;
        self.synth.push(s.clone());
        self.synth_ix.insert(s, i);
        i
    }

    fn func_sig(&self, input: usize, func: u32) -> FuncType {
        let o = &self.inputs[input].obj;
        o.types[o.funcs[func as usize].ty as usize].clone()
    }

    /// The function a symbol of `input` names inside the mod.
    fn own_function(&self, input: usize, symbol: u32) -> Option<(usize, u32)> {
        let inp = &self.inputs[input];
        let s = &inp.obj.symbols[symbol as usize];
        if s.kind != SymbolKind::Function {
            return None;
        }
        if s.defined {
            return Some((input, s.index? - inp.obj.num_imported_funcs()));
        }
        for (j, o) in self.inputs.iter().enumerate() {
            let o = &o.obj;
            if let Some(t) = o.symbols.iter().find(|t| t.kind == SymbolKind::Function && t.defined && !t.local && t.name == s.name) {
                return Some((j, t.index? - o.num_imported_funcs()));
            }
        }
        None
    }

    fn function_number(&self, k: (usize, u32)) -> Option<u32> {
        self.guest_order.iter().position(|x| *x == k).map(|p| p as u32)
    }

    fn impl_func(&self, imp: &Impl) -> Result<(usize, u32)> {
        let o = &self.inputs[imp.input].obj;
        let s = o
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Function && s.defined && s.name == imp.function)
            .with_context(|| format!("{} defines no function {}", self.inputs[imp.input].label, imp.function))?;
        Ok((imp.input, s.index.context("function symbol without index")? - o.num_imported_funcs()))
    }

    fn own_data(&self, input: usize, symbol: u32) -> Option<u32> {
        if let Some(&o) = self.data.get(&(input, symbol)) {
            return Some(o);
        }
        let s = &self.inputs[input].obj.symbols[symbol as usize];
        if s.kind != SymbolKind::Data || s.defined {
            return None;
        }
        for (j, inp) in self.inputs.iter().enumerate() {
            for (sj, t) in inp.obj.symbols.iter().enumerate() {
                if t.kind == SymbolKind::Data && t.defined && !t.local && t.name == s.name {
                    if let Some(&o) = self.data.get(&(j, sj as u32)) {
                        return Some(o);
                    }
                }
            }
        }
        None
    }

    /// A game function's entry index global (`game.<k>`).
    fn game_call(&mut self, container: &str, addr: u32, sig: &FuncType) -> u32 {
        let c = GameCall { container: container.to_string(), addr, sig: sig.clone() };
        let k = match self.calls.iter().position(|x| *x == c) {
            Some(k) => k,
            None => {
                self.calls.push(c);
                self.calls.len() - 1
            }
        };
        k as u32
    }
}

struct R<'c, 'a> {
    m: &'c mut M<'a>,
    input: usize,
}

impl Resolver for R<'_, '_> {
    fn call(&mut self, symbol: u32, sig: &FuncType, _last_const: Option<i32>) -> Result<Lowered> {
        let inputs = self.m.inputs;
        let inp = &inputs[self.input];
        let s = &inp.obj.symbols[symbol as usize];
        let name = s.name.clone();
        if name == "psxw_call_indirect" {
            return Ok(Lowered::Call(self.m.synth(S::Indirect)));
        }
        if name == "psxmod_log" && !s.defined {
            return Ok(Lowered::Call(self.m.import_ix[names::LOG]));
        }
        if let Some(imp) = service(&name) {
            return Ok(Lowered::Call(*self.m.import_ix.get(imp).with_context(|| format!("{name}: not imported"))?));
        }
        if ["abs", "labs", "fabs"].contains(&name.as_str()) && !s.defined {
            return Ok(Lowered::Call(self.m.synth(S::Support(name, sig.clone()))));
        }
        if let Some(b) = self.m.bindings.get(&name).cloned() {
            return Ok(Lowered::Call(match b {
                Bound::C(f, t) => {
                    ensure!(t == *sig, "{}: {name} is called as {sig:?}, but the function it wraps is {t:?}", inp.label);
                    f
                }
                Bound::O32 { next, id } => self.m.synth(S::Transparent { input: self.input, name, sig: sig.clone(), next, id }),
            }));
        }
        if let Some((j, f)) = self.m.own_function(self.input, symbol) {
            let t = self.m.func_sig(j, f);
            ensure!(t == *sig, "{}: {name} is called as {sig:?} but defined as {t:?}", inp.label);
            return Ok(Lowered::Call(self.m.guest[&(j, f)]));
        }
        // A game function.
        let callee = match classify_name(&name) {
            NameClass::Guest(n) | NameClass::SignatureThunk { target: n } => n.to_string(),
            NameClass::MainWrapper => bail!("call to clang's main wrapper {name}"),
        };
        let main = self.m.main.clone();
        let r = self.m.map.resolve(&main, &callee).with_context(|| format!("{}: function {callee} has no address", inp.label))?;
        let has = |c: &str| self.m.map.container(c).is_some_and(|k| k.text.iter().any(|t| t.contains(&r.addr)));
        if has(&main) {
            if let Some(Some(t)) = self.m.spec.entries.get(&(main.clone(), r.addr)) {
                if t == sig {
                    let k = self.m.game_call(&main, r.addr, sig);
                    return Ok(Lowered::Call(self.m.synth(S::GameC { k, sig: sig.clone() })));
                }
            }
        }
        let target = self.m.synth(S::GameO32 { addr: r.addr });
        Ok(Lowered::Call(self.m.synth(S::ToO32 { input: self.input, name: callee, sig: sig.clone(), target })))
    }

    fn call_indirect(&mut self, sig: &FuncType) -> Result<u32> {
        Ok(self.m.synth(S::ICall(sig.clone())))
    }

    fn global(&mut self, symbol: u32) -> Result<u32> {
        let s = &self.m.inputs[self.input].obj.symbols[symbol as usize];
        match s.name.as_str() {
            "__stack_pointer" => Ok(self.m.g_mod_sp),
            other => bail!("mod code uses global {other}"),
        }
    }

    fn data_addr(&mut self, symbol: u32) -> Result<u32> {
        let inp = &self.m.inputs[self.input];
        let s = &inp.obj.symbols[symbol as usize];
        let main = self.m.main.clone();
        self.m.map.resolve(&main, &s.name).map(|r| r.addr).with_context(|| format!("{}: data symbol {} has no address", inp.label, s.name))
    }

    fn data_rel(&mut self, symbol: u32) -> Result<Option<(u32, u32)>> {
        Ok(self.m.own_data(self.input, symbol).map(|o| (self.m.g_data_base, o)))
    }

    fn func_addr(&mut self, symbol: u32) -> Result<u32> {
        let s = &self.m.inputs[self.input].obj.symbols[symbol as usize];
        let name = match classify_name(&s.name) {
            NameClass::Guest(n) | NameClass::SignatureThunk { target: n } => n.to_string(),
            NameClass::MainWrapper => bail!("address of clang's main wrapper"),
        };
        let main = self.m.main.clone();
        self.m.map.resolve(&main, &name).map(|r| r.addr).with_context(|| format!("function pointer to {name}, which has no address"))
    }

    fn func_rel(&mut self, symbol: u32) -> Result<Option<(u32, u32)>> {
        let Some(k) = self.m.own_function(self.input, symbol) else { return Ok(None) };
        let n = self.m.function_number(k).context("own function not linked")?;
        Ok(Some((self.m.g_code_base, 4 * n)))
    }

    fn type_index(&mut self, ty: u32) -> u32 {
        let t = self.m.inputs[self.input].obj.types[ty as usize].clone();
        self.m.intern(&t)
    }

    fn dirty_map(&self) -> Option<u32> {
        None
    }

    fn mark_range(&mut self) -> u32 {
        unreachable!("no dirty map in a mod module")
    }

    fn division(&mut self, _op: &'static str) -> Option<u32> {
        None
    }

    fn prologue(&mut self, _func_symbol: Option<&str>) -> Result<Vec<I<'static>>> {
        Ok(Vec::new())
    }
}

fn dispatch_call(f: &mut Function, o32_ty: u32) {
    // stack: a0..a3, addr
    f.instruction(&I::I32Const(0x003F_FFFC));
    f.instruction(&I::I32And);
    f.instruction(&I::I32Const(2));
    f.instruction(&I::I32ShrU);
    f.instruction(&I::CallIndirect { type_index: o32_ty, table_index: 0 });
}

fn body(m: &mut M, s: &S) -> Result<(FuncType, Function, String)> {
    let sp = m.g_sp;
    Ok(match s.clone() {
        S::NextC { k, sig } => {
            let tix = m.intern(&sig);
            let mut f = Function::new([]);
            for l in 0..sig.params.len() as u32 {
                f.instruction(&I::LocalGet(l));
            }
            f.instruction(&I::GlobalGet(m.g_original + k));
            f.instruction(&I::CallIndirect { type_index: tix, table_index: 1 });
            f.instruction(&I::End);
            (sig, f, format!("next:{k}"))
        }
        S::NextO32 { k } => {
            let mut f = Function::new([]);
            for l in 0..4 {
                f.instruction(&I::LocalGet(l));
            }
            f.instruction(&I::GlobalGet(m.g_original + k));
            f.instruction(&I::CallIndirect { type_index: m.o32_ty, table_index: 1 });
            f.instruction(&I::End);
            (o32(), f, format!("next:{k}"))
        }
        S::GameC { k, sig } => {
            let tix = m.intern(&sig);
            let mut f = Function::new([]);
            for l in 0..sig.params.len() as u32 {
                f.instruction(&I::LocalGet(l));
            }
            f.instruction(&I::I32Const(k as i32));
            f.instruction(&I::CallIndirect { type_index: tix, table_index: 2 });
            f.instruction(&I::End);
            (sig, f, format!("game:{k}"))
        }
        S::GameO32 { addr } => {
            let mut f = Function::new([]);
            for l in 0..4 {
                f.instruction(&I::LocalGet(l));
            }
            f.instruction(&I::I32Const(table_index(addr) as i32));
            f.instruction(&I::CallIndirect { type_index: m.o32_ty, table_index: 0 });
            f.instruction(&I::End);
            (o32(), f, format!("game:{addr:08X}"))
        }
        S::ToO32 { input, name, sig, target } => {
            let proto = m.inputs[input].protos.get(&name);
            let (kinds, variadic) = marshal::param_kinds(&sig, proto);
            let (words, _) = marshal::words(&kinds, variadic);
            let f = marshal::c_to_o32(&sig, &words, sp, &|_| {}, &|_, _| {}, &|f| {
                f.instruction(&I::Call(target));
            });
            (sig, f, format!("o32_call:{name}"))
        }
        S::ICall(sig) => {
            let (kinds, _) = marshal::param_kinds(&sig, None);
            let (words, _) = marshal::words(&kinds, false);
            let mut helper = sig.clone();
            helper.params.push(V::I32);
            let target = sig.params.len() as u32;
            let o32_ty = m.o32_ty;
            let f = marshal::c_to_o32(&helper, &words, sp, &|_| {}, &|_, _| {}, &|f| {
                f.instruction(&I::LocalGet(target));
                dispatch_call(f, o32_ty);
            });
            (helper, f, "icall".into())
        }
        S::Indirect => {
            let mut f = Function::new([]);
            for l in 1..5 {
                f.instruction(&I::LocalGet(l));
            }
            f.instruction(&I::LocalGet(0));
            dispatch_call(&mut f, m.o32_ty);
            f.instruction(&I::End);
            (FuncType { params: vec![V::I32; 5], results: vec![V::I64] }, f, "call_indirect".into())
        }
        S::Support(name, sig) => (sig.clone(), crate::bodies::support(&name), name),
        S::CLayer { befores, core, afters, sig, input, name } => {
            let proto = m.inputs[input].protos.get(&name);
            (sig.clone(), c_layer(m.g_mod_sp, proto, &befores, core, &afters, &sig), format!("hook:{name}"))
        }
        S::O32Layer { befores, core, afters, returns } => (o32(), o32_layer(&befores, core, &afters, returns), "hook:o32".into()),
        S::ModWrap { input, func, hook } => {
            let sig = m.func_sig(input, func);
            let name = guest_name(m, input, func);
            let proto = m.inputs[input].protos.get(&name);
            let g = marshal::WrapGlobals { sp, scratch: m.g_mod_sp, hook: hook.map(|id| (m.g_hook_sp, m.g_hook_id, id)) };
            let callee = m.guest[&(input, func)];
            (o32(), marshal::o32_to_c_with(&sig, proto, callee, marshal::Shape::O32, g), format!("o32:mod:{name}"))
        }
        S::ModAfter { input, func, result } => {
            let sig = m.func_sig(input, func);
            let name = guest_name(m, input, func);
            let proto = m.inputs[input].protos.get(&name);
            let g = marshal::WrapGlobals { sp, scratch: m.g_mod_sp, hook: None };
            let callee = m.guest[&(input, func)];
            let mut params = if result { vec![V::I64] } else { vec![] };
            params.extend([V::I32; 4]);
            (FuncType { params, results: vec![] }, marshal::o32_to_c_with(&sig, proto, callee, marshal::Shape::After { result }, g), format!("o32:after:{name}"))
        }
        S::Transparent { input, name, sig, next, id } => {
            let proto = m.inputs[input].protos.get(&name);
            let (kinds, variadic) = marshal::param_kinds(&sig, proto);
            let (words, _) = marshal::words(&kinds, variadic);
            (sig.clone(), marshal::c_to_o32_transparent(&sig, &words, sp, (m.g_hook_sp, m.g_hook_id, id), next), format!("original:{name}"))
        }
        S::Init { fresh } => (FuncType { params: vec![], results: vec![] }, init(m, fresh)?, if fresh { names::INIT.into() } else { names::RESUME.into() }),
        S::Loaded => (FuncType { params: vec![V::I32; 2], results: vec![] }, loaded(m)?, names::LOADED.into()),
    })
}

fn guest_name(m: &M, input: usize, func: u32) -> String {
    let o = &m.inputs[input].obj;
    let name = o.func_symbol(func + o.num_imported_funcs()).map_or_else(String::new, |s| s.name.clone());
    match classify_name(&name) {
        NameClass::Guest(n) => n.to_string(),
        _ => name,
    }
}

/// Store the i32 on top of the stack at `window(global + off)`.
fn store_rel(f: &mut Function, global: u32, off: u32, value: &dyn Fn(&mut Function)) {
    f.instruction(&I::GlobalGet(global));
    f.instruction(&I::I32Const(off as i32));
    f.instruction(&I::I32Add);
    f.instruction(&I::I32Const(WINDOW_MASK as i32));
    f.instruction(&I::I32And);
    value(f);
    f.instruction(&I::I32Store(MemArg { offset: WINDOW_BASE as u64, align: 0, memory_index: 0 }));
}

/// A data patch's write; a mod function's address is computed.
fn patch(m: &M, f: &mut Function, d: &DataPatch) -> Result<()> {
    match &d.value {
        PatchValue::Bytes(b) => {
            for (k, x) in b.iter().enumerate() {
                store_byte(f, d.addr + k as u32, *x);
            }
        }
        PatchValue::FnPtr(imp) => {
            let n = m.function_number(m.impl_func(imp)?).context("fnptr function not linked")?;
            let code_base = m.g_code_base;
            f.instruction(&I::I32Const((d.addr & WINDOW_MASK) as i32));
            f.instruction(&I::GlobalGet(code_base));
            f.instruction(&I::I32Const(4 * n as i32));
            f.instruction(&I::I32Add);
            f.instruction(&I::I32Store(MemArg { offset: WINDOW_BASE as u64, align: 0, memory_index: 0 }));
        }
    }
    Ok(())
}

fn resident(f: &mut Function, d: &DataPatch) {
    let id = d.resident.expect("overlay patch");
    let load = |f: &mut Function, a: u32| {
        f.instruction(&I::I32Const((a & WINDOW_MASK) as i32));
        f.instruction(&I::I32Load(MemArg { offset: WINDOW_BASE as u64, align: 2, memory_index: 0 }));
    };
    load(f, id.at);
    f.instruction(&I::I32Const(id.words[0] as i32));
    f.instruction(&I::I32Eq);
    load(f, id.at + 4);
    f.instruction(&I::I32Const(id.words[1] as i32));
    f.instruction(&I::I32Eq);
    f.instruction(&I::I32And);
}

/// `psxmod_init` / `psxmod_resume`: place the image (fresh) with its
/// pointers, apply the executable's data patches, and those of overlays
/// resident now.
fn init(m: &mut M, fresh: bool) -> Result<Function> {
    let mut f = Function::new([]);
    if fresh && !m.image.is_empty() {
        f.instruction(&I::GlobalGet(m.g_data_base));
        f.instruction(&I::I32Const(WINDOW_MASK as i32));
        f.instruction(&I::I32And);
        f.instruction(&I::I32Const(WINDOW_BASE as i32));
        f.instruction(&I::I32Add);
        f.instruction(&I::I32Const(0));
        f.instruction(&I::I32Const(m.image.len() as i32));
        f.instruction(&I::MemoryInit { mem: 0, data_index: 0 });
        for (at, fx) in m.fixups.clone() {
            let (g, off) = match fx {
                Fixup::Data(o) => (m.g_data_base, o),
                Fixup::Func(n) => (m.g_code_base, 4 * n),
            };
            store_rel(&mut f, m.g_data_base, at, &|f| {
                f.instruction(&I::GlobalGet(g));
                f.instruction(&I::I32Const(off as i32));
                f.instruction(&I::I32Add);
            });
        }
    }
    for d in m.spec.data.clone().iter() {
        match d.resident {
            None => patch(m, &mut f, d)?,
            Some(_) => {
                resident(&mut f, d);
                f.instruction(&I::If(BlockType::Empty));
                patch(m, &mut f, d)?;
                f.instruction(&I::End);
            }
        }
    }
    f.instruction(&I::End);
    Ok(f)
}

/// `psxmod_loaded(addr, len)`: as the static link's, with this module's
/// pending flags.
fn loaded(m: &mut M) -> Result<Function> {
    let mut f = Function::new([(2, ValType::I32)]);
    let (phys, end) = (2, 3);
    let patches: Vec<(DataPatch, u32)> = m.spec.data.iter().filter(|d| d.resident.is_some()).cloned().enumerate().map(|(k, d)| (d, m.g_pending + k as u32)).collect();
    if !patches.is_empty() {
        f.instruction(&I::LocalGet(0));
        f.instruction(&I::I32Const(0x1F_FFFF));
        f.instruction(&I::I32And);
        f.instruction(&I::LocalTee(phys));
        f.instruction(&I::LocalGet(1));
        f.instruction(&I::I32Add);
        f.instruction(&I::LocalSet(end));
    }
    for (d, g) in &patches {
        let a = (d.addr & 0x1F_FFFF) as i32;
        f.instruction(&I::I32Const(a));
        f.instruction(&I::LocalGet(end));
        f.instruction(&I::I32LtU);
        f.instruction(&I::LocalGet(phys));
        f.instruction(&I::I32Const(a + d.len() as i32));
        f.instruction(&I::I32LtU);
        f.instruction(&I::I32And);
        f.instruction(&I::If(BlockType::Empty));
        f.instruction(&I::I32Const(1));
        f.instruction(&I::GlobalSet(*g));
        f.instruction(&I::End);
    }
    for (d, g) in &patches {
        f.instruction(&I::GlobalGet(*g));
        resident(&mut f, d);
        f.instruction(&I::I32And);
        f.instruction(&I::If(BlockType::Empty));
        patch(m, &mut f, d)?;
        f.instruction(&I::I32Const(0));
        f.instruction(&I::GlobalSet(*g));
        f.instruction(&I::End);
    }
    f.instruction(&I::End);
    Ok(f)
}

/// The mod's layer on target `k` over its next link: its top.
fn realize(m: &mut M, k: u32, t: &ModTarget, next_id: &mut i32) -> Result<u32> {
    let l = &t.layer;
    let what = format!("{} {:#010x}", t.container, t.addr);
    let id = m.spec.id.clone();
    let bind = |m: &mut M, name: &str, b: Bound| -> Result<()> {
        ensure!(m.bindings.insert(name.to_string(), b).is_none(), "mod {id} names two next links {name}");
        Ok(())
    };
    match &t.sig {
        Some(sig) => {
            let next = m.synth(S::NextC { k, sig: sig.clone() });
            let core = match &l.patch {
                Some(p) => {
                    let (i, f) = m.impl_func(&p.imp)?;
                    let ty = m.func_sig(i, f);
                    ensure!(ty == *sig, "{id}: patch {} is {ty:?}, but {what} is {sig:?}", p.imp.function);
                    if let Some(o) = &p.original {
                        bind(m, o, Bound::C(next, sig.clone()))?;
                    }
                    m.guest[&(i, f)]
                }
                None => next,
            };
            if l.befores.is_empty() && l.afters.is_empty() {
                return Ok(core);
            }
            let unit = FuncType { params: vec![], results: vec![] };
            let mut befores = Vec::new();
            for b in &l.befores {
                let (i, f) = m.impl_func(&b.imp)?;
                let ty = m.func_sig(i, f);
                let want = if b.bare { unit.clone() } else { FuncType { params: sig.params.clone(), results: vec![] } };
                ensure!(ty == want, "{id}: before {} is {ty:?}, but {what} takes {want:?}", b.imp.function);
                befores.push((m.guest[&(i, f)], b.bare));
            }
            let mut afters = Vec::new();
            for a in &l.afters {
                let (i, f) = m.impl_func(&a.imp)?;
                let ty = m.func_sig(i, f);
                let mut params = sig.results.clone();
                params.extend(sig.params.iter().copied());
                let want = if a.bare { unit.clone() } else { FuncType { params, results: vec![] } };
                ensure!(ty == want, "{id}: after {} is {ty:?}, but {what} wants {want:?} (the result first)", a.imp.function);
                afters.push((m.guest[&(i, f)], a.bare));
            }
            // Prototype facts for structs by value: any of the mod's full
            // implementations has the function's parameters.
            let pi = l.patch.iter().map(|p| &p.imp).chain(l.befores.iter().filter(|b| !b.bare).map(|b| &b.imp)).next();
            let (input, name) = match pi {
                Some(imp) => (imp.input, imp.function.clone()),
                None => (0, String::new()),
            };
            Ok(m.synth(S::CLayer { befores, core, afters, sig: sig.clone(), input, name }))
        }
        None => {
            let next = m.synth(S::NextO32 { k });
            let core = match &l.patch {
                Some(p) => {
                    let (i, f) = m.impl_func(&p.imp)?;
                    let hook = match &p.original {
                        Some(o) => {
                            let hid = *next_id;
                            *next_id += 1;
                            bind(m, o, Bound::O32 { next, id: hid })?;
                            Some(hid)
                        }
                        None => None,
                    };
                    m.synth(S::ModWrap { input: i, func: f, hook })
                }
                None => next,
            };
            if l.befores.is_empty() && l.afters.is_empty() {
                return Ok(core);
            }
            let mut befores = Vec::new();
            for b in &l.befores {
                let (i, f) = m.impl_func(&b.imp)?;
                befores.push(m.synth(S::ModWrap { input: i, func: f, hook: None }));
            }
            let mut afters = Vec::new();
            for a in &l.afters {
                let (i, f) = m.impl_func(&a.imp)?;
                afters.push(m.synth(S::ModAfter { input: i, func: f, result: t.returns && !a.bare }));
            }
            Ok(m.synth(S::O32Layer { befores, core, afters, returns: t.returns }))
        }
    }
}

/// Link the mod's objects into its module.
pub fn link(map: &AddressMap, spec: &ModuleSpec, inputs: &[Input]) -> Result<Linked> {
    let mut m = M {
        map,
        inputs,
        spec,
        main: map.main.clone(),
        types: Vec::new(),
        type_ix: HashMap::new(),
        imports: Vec::new(),
        import_ix: HashMap::new(),
        guest: HashMap::new(),
        guest_order: Vec::new(),
        synth: Vec::new(),
        synth_ix: HashMap::new(),
        data: HashMap::new(),
        image: Vec::new(),
        fixups: Vec::new(),
        layout: Vec::new(),
        bindings: HashMap::new(),
        calls: Vec::new(),
        g_sp: 0,
        g_data_base: 1,
        g_code_index: 2,
        g_code_base: 3,
        g_stack_top: 4,
        g_original: 5,
        g_mod_sp: 0,
        g_hook_sp: 0,
        g_hook_id: 0,
        g_pending: 0,
        o32_ty: 0,
    };
    m.o32_ty = m.intern(&o32());
    // Function imports: the log, untranslated, and any runtime service
    // the code calls.
    let mut wanted: BTreeSet<&'static str> = BTreeSet::new();
    for inp in inputs {
        for s in inp.obj.symbols.iter().filter(|s| s.kind == SymbolKind::Function && !s.defined) {
            if let Some(imp) = service(&s.name) {
                wanted.insert(imp);
            }
        }
    }
    let psx_types: HashMap<&str, FuncType> =
        crate::game::psx_imports().into_iter().map(|(n, p, r)| (n, FuncType { params: p, results: r })).collect();
    let add = |m: &mut M, name: &'static str| -> Result<()> {
        let t = psx_types.get(name).with_context(|| format!("no psx import {name}"))?.clone();
        m.intern(&t);
        m.import_ix.insert(name.to_string(), m.imports.len() as u32);
        m.imports.push((names::PSX, name.to_string(), t));
        Ok(())
    };
    add(&mut m, psxw_abi::psx::MOD_LOG)?;
    add(&mut m, psxw_abi::psx::UNTRANSLATED)?;
    for w in wanted {
        add(&mut m, w)?;
    }
    // Every defined function, in input order: its number gives its
    // synthetic address.
    for (i, inp) in inputs.iter().enumerate() {
        for f in 0..inp.obj.funcs.len() as u32 {
            let index = f + inp.obj.num_imported_funcs();
            if inp.obj.func_symbol(index).is_some_and(|s| !matches!(classify_name(&s.name), NameClass::Guest(_))) {
                continue;
            }
            let out = m.nimports() + m.guest_order.len() as u32;
            m.guest.insert((i, f), out);
            m.guest_order.push((i, f));
        }
    }
    // The data image: every defined data symbol, aligned.
    for (i, inp) in inputs.iter().enumerate() {
        for (si, s) in inp.obj.symbols.iter().enumerate() {
            let (true, SymbolKind::Data, Some(def)) = (s.defined, s.kind, s.data) else { continue };
            let align = 1u32 << inp.obj.segment_of(def).align_log2;
            let at = (m.image.len() as u32).div_ceil(align.max(4)) * align.max(4);
            m.image.resize(at as usize, 0);
            m.data.insert((i, si as u32), at);
            m.layout.push((s.name.clone(), at, def.size));
            let seg = inp.obj.segment_of(def);
            if seg.is_bss() {
                m.image.resize((at + def.size) as usize, 0);
            } else {
                m.image.extend_from_slice(inp.obj.data_bytes(def));
            }
        }
    }
    // The image's pointers: game addresses now, its own at placement.
    let main = m.main.clone();
    for (i, inp) in inputs.iter().enumerate() {
        for (si, s) in inp.obj.symbols.iter().enumerate() {
            let (true, SymbolKind::Data, Some(def)) = (s.defined, s.kind, s.data) else { continue };
            let base = m.data[&(i, si as u32)];
            for (off, r) in inp.obj.relocs_in(def) {
                let at = base + off;
                let target = &inp.obj.symbols[r.symbol as usize];
                let value = match r.kind {
                    RelocKind::MemoryAddrI32 => match m.own_data(i, r.symbol) {
                        Some(o) => {
                            m.fixups.push((at, Fixup::Data(o.wrapping_add(r.addend as u32))));
                            continue;
                        }
                        None => map.resolve(&main, &target.name).with_context(|| format!("{}: data {} has no address", inp.label, target.name))?.addr.wrapping_add(r.addend as u32),
                    },
                    RelocKind::TableIndexI32 => match m.own_function(i, r.symbol) {
                        Some(k) => {
                            m.fixups.push((at, Fixup::Func(m.function_number(k).context("function")?)));
                            continue;
                        }
                        None => map.resolve(&main, &target.name).with_context(|| format!("{}: function {} has no address", inp.label, target.name))?.addr,
                    },
                    RelocKind::Other => bail!("unsupported data relocation in {}", s.name),
                };
                m.image[at as usize..at as usize + 4].copy_from_slice(&value.to_le_bytes());
            }
        }
    }
    // Globals: imported (sp, data_base, code_index, code_base, stack_top,
    // original.<k>…), then defined (mod sp, hook sp and id, a pending flag
    // per overlay data patch).
    let n_targets = spec.targets.len() as u32;
    let defined0 = 5 + n_targets;
    m.g_mod_sp = defined0;
    m.g_hook_sp = defined0 + 1;
    m.g_hook_id = defined0 + 2;
    m.g_pending = defined0 + 3;
    let n_pending = spec.data.iter().filter(|d| d.resident.is_some()).count() as u32;
    // The hooks first: their bindings must exist before the rewrite.
    let mut next_id = 1i32;
    let mut tops = Vec::new();
    for (k, t) in spec.targets.iter().enumerate() {
        tops.push(realize(&mut m, k as u32, t, &mut next_id)?);
    }
    let mut bodies = Vec::new();
    let mut fnames: Vec<(u32, String)> = Vec::new();
    for &(i, f) in &m.guest_order.clone() {
        let obj = &inputs[i].obj;
        let ty = obj.types[obj.funcs[f as usize].ty as usize].clone();
        let tix = m.intern(&ty);
        let name = obj.func_symbol(f + obj.num_imported_funcs()).map_or_else(|| format!("{}#{f}", inputs[i].label), |s| s.name.clone());
        let b = rewrite::rewrite(obj, f as usize, &mut R { m: &mut m, input: i }).with_context(|| format!("{}: {name}", inputs[i].label))?;
        fnames.push((m.guest[&(i, f)], format!("mod:{name}")));
        bodies.push((tix, b));
    }
    // Every function's o32 entry, for the dispatch table.
    let wraps: Vec<u32> = m.guest_order.clone().into_iter().map(|(i, f)| m.synth(S::ModWrap { input: i, func: f, hook: None })).collect();
    let init_f = m.synth(S::Init { fresh: true });
    let resume_f = m.synth(S::Init { fresh: false });
    let loaded_f = m.synth(S::Loaded);
    let mut si = 0;
    while si < m.synth.len() {
        let s = m.synth[si].clone();
        let (ty, b, name) = body(&mut m, &s)?;
        let tix = m.intern(&ty);
        fnames.push((m.nimports() + m.guest_order.len() as u32 + si as u32, name));
        bodies.push((tix, b));
        si += 1;
    }

    let mut module = Module::new();
    let mut types = TypeSection::new();
    for t in &m.types {
        types.ty().function(t.params.iter().map(|&v| marshal::val(v)), t.results.iter().map(|&v| marshal::val(v)));
    }
    module.section(&types);
    let mut imports = ImportSection::new();
    for (module_name, name, t) in m.imports.clone() {
        imports.import(module_name, &name, EntityType::Function(m.type_ix[&t]));
    }
    let table = |min: u64| EntityType::Table(TableType { element_type: RefType::FUNCREF, minimum: min, maximum: None, table64: false, shared: false });
    imports.import(names::PSX, names::DISPATCH, table(psxw_abi::TABLE_LEN as u64));
    imports.import(names::PSXMOD, names::CHAIN, table(0));
    imports.import(names::PSX, names::MEMORY, EntityType::Memory(MemoryType { minimum: 0, maximum: None, memory64: false, shared: false, page_size_log2: None }));
    let gi = |mutable: bool| EntityType::Global(GlobalType { val_type: ValType::I32, mutable, shared: false });
    imports.import(names::PSX, names::SP, gi(true));
    for n in [names::DATA_BASE, names::CODE_INDEX, names::CODE_BASE, names::STACK_TOP] {
        imports.import(names::PSXMOD, n, gi(false));
    }
    for k in 0..n_targets {
        imports.import(names::ORIGINAL, &k.to_string(), gi(false));
    }
    module.section(&imports);
    let mut functions = FunctionSection::new();
    for (t, _) in &bodies {
        functions.function(*t);
    }
    module.section(&functions);
    let mut tables = wasm_encoder::TableSection::new();
    let n_calls = m.calls.len() as u64;
    tables.table(TableType { element_type: RefType::FUNCREF, minimum: n_calls, maximum: Some(n_calls), table64: false, shared: false });
    module.section(&tables);
    let mut globals = GlobalSection::new();
    let mutable = GlobalType { val_type: ValType::I32, mutable: true, shared: false };
    globals.global(mutable, &ConstExpr::global_get(m.g_stack_top));
    for _ in 0..2 + n_pending {
        globals.global(mutable, &ConstExpr::i32_const(0));
    }
    module.section(&globals);
    let mut ex = ExportSection::new();
    for (k, &t) in tops.iter().enumerate() {
        ex.export(&format!("hook.{k}"), ExportKind::Func, t);
    }
    ex.export(names::INIT, ExportKind::Func, init_f);
    ex.export(names::RESUME, ExportKind::Func, resume_f);
    ex.export(names::LOADED, ExportKind::Func, loaded_f);
    ex.export("psxmod_calls", ExportKind::Table, 2);
    module.section(&ex);
    let mut elems = ElementSection::new();
    if !wraps.is_empty() {
        elems.active(Some(0), &ConstExpr::global_get(m.g_code_index), Elements::Functions(wraps.into()));
    }
    module.section(&elems);
    // memory.init needs the data count.
    module.section(&wasm_encoder::DataCountSection { count: 1 });
    let mut code = CodeSection::new();
    for (_, b) in &bodies {
        code.function(b);
    }
    module.section(&code);
    let mut data = DataSection::new();
    data.passive(m.image.clone());
    module.section(&data);
    let mut nm = NameMap::new();
    fnames.sort_by_key(|(i, _)| *i);
    for (i, n) in &fnames {
        nm.append(*i, n);
    }
    let mut ns = NameSection::new();
    ns.functions(&nm);
    module.section(&ns);
    module.section(&CustomSection { name: "psxw.abi".into(), data: psxw_abi::ABI_VERSION.to_le_bytes().to_vec().into() });
    let layout_hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        m.layout.hash(&mut h);
        format!("{:016x}", h.finish())
    };
    Ok(Linked {
        wasm: module.finish(),
        data_size: m.image.len() as u32,
        functions: m.guest_order.len() as u32,
        layout_hash,
        calls: m.calls.clone(),
    })
}

