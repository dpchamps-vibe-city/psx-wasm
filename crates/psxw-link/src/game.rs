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
use crate::object::{FuncType, NameClass, WasmObject, classify_name};
use crate::rewrite::{self, Lowered, Resolver};
use anyhow::{Context as _, Result, bail};
use psxw_abi::{TABLE_LEN, WINDOW_BASE, WINDOW_MASK, guest, psx, table_index};
use psxw_inputs::AddressMap;
use std::collections::{BTreeMap, HashMap};
use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, CustomSection, DataSection, ElementSection, Elements, EntityType, ExportKind,
    ExportSection, Function, FunctionSection, GlobalSection, GlobalType, ImportSection, Instruction as I, MemArg,
    MemorySection, MemoryType, Module, NameMap, NameSection, RefType, TableSection, TableType, TypeSection, ValType,
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
    pub main: String,
    /// Initial guest memory pages (the runtime grows it).
    pub min_pages: u64,
    /// Functions whose entry calls `psx.probe(addr)`.
    pub probes: Vec<(String, u32)>,
}

/// The `psx.*` imports, in import order, with their signatures.
fn psx_imports() -> Vec<(&'static str, Vec<V>, Vec<V>)> {
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
        (psx::IO_ACCESS, vec![i], vec![]),
        ("adapter", vec![i; 5], vec![V::I64]),
        (psx::PROBE, vec![i], vec![]),
    ]
}

/// C names of the runtime services, and the `psx` import each lowers to.
fn service(name: &str) -> Option<&'static str> {
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
    Support(String, FuncType),
    Division(&'static str, crate::link::Division),
    InlineCopy,
    SpGet,
    SpSet,
    GpGet,
    GpSet,
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
    o32_ty: u32,
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

    /// The o32 entry of the function at (container, addr).
    fn entry(&mut self, container: &str, addr: u32) -> Option<u32> {
        if self.spec.overlays.contains_key(&addr) && container != self.spec.main {
            return Some(self.synth(Synth::OverlayStub(addr)));
        }
        let tier = *self.spec.tiers.get(&(container.to_string(), addr))?;
        Some(match tier {
            Tier::Lift { input, func } => self.guest[&(input, func)],
            Tier::C { input, func } => self.synth(Synth::O32Wrap { input, func }),
            Tier::Adapter(id) => self.synth(Synth::AdapterThunk(id)),
        })
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
            "psxw_call_indirect" => return Ok(Lowered::Call(self.ctx.synth(Synth::Indirect))),
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
        if let Some((c, addr)) = parse_generated(&name, &self.ctx.spec.main) {
            let e = match c {
                Some(c) => self.ctx.entry(&c, addr),
                None => self.ctx.dynamic_entry(addr),
            };
            return e.map(Lowered::Call).with_context(|| format!("{name}: no function at that address"));
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
        let direct = |ctx: &Ctx, c: &str| -> Option<u32> {
            // C→C with matching signatures: a plain call.
            match ctx.spec.tiers.get(&(c.to_string(), addr))? {
                Tier::C { input, func } => {
                    let o = &ctx.inputs[*input].obj;
                    (o.types[o.funcs[*func as usize].ty as usize] == *sig && !ctx.spec.overlays.contains_key(&addr))
                        .then(|| ctx.guest[&(*input, *func)])
                }
                _ => None,
            }
        };
        let target = match &home {
            Some(c) => {
                if let Some(i) = direct(self.ctx, c) {
                    return Ok(Lowered::Call(i));
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
            "__stack_pointer" => Ok(self.ctx.sp),
            other => bail!("guest code uses global {other}"),
        }
    }

    fn data_addr(&mut self, symbol: u32) -> Result<u32> {
        self.ctx.layout.data_addr(&self.ctx.view, self.ctx.map, self.input, symbol)
    }

    fn func_addr(&mut self, symbol: u32) -> Result<u32> {
        let inputs = self.ctx.inputs;
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
        self.ctx.spec.debug_io.then(|| self.ctx.import(psx::IO_ACCESS))
    }
}

fn o32() -> FuncType {
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
    Ok(match s.clone() {
        Synth::ToO32 { input, name, sig, target } => {
            let proto = ctx.inputs[input].protos.get(&name);
            let (kinds, variadic) = marshal::param_kinds(&sig, proto);
            let (words, _) = marshal::words(&kinds, variadic);
            let f = marshal::c_to_o32(&sig, &words, sp, &|_| {}, &|_, _| {}, &|f| {
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
            (o32(), marshal::o32_to_c(&sig, proto, callee, sp), format!("o32:{guest_name}"))
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
            let cands = ctx.spec.overlays[&addr].clone();
            let mut entries = Vec::new();
            for (c, words) in &cands {
                let tier = *ctx.spec.tiers.get(&(c.clone(), addr)).context("overlay candidate without a tier")?;
                let e = match tier {
                    Tier::Lift { input, func } => ctx.guest[&(input, func)],
                    Tier::C { input, func } => ctx.synth(Synth::O32Wrap { input, func }),
                    Tier::Adapter(id) => ctx.synth(Synth::AdapterThunk(id)),
                };
                entries.push((*words, e));
            }
            let mut f = Function::new([]);
            let word = |f: &mut Function, off: u32| {
                f.instruction(&I::I32Const(((addr + off) & WINDOW_MASK) as i32));
                f.instruction(&I::I32Load(MemArg { offset: WINDOW_BASE as u64, align: 2, memory_index: 0 }));
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
            let f = marshal::c_to_o32(&helper, &words, sp, &|_| {}, &|_, _| {}, &|f| {
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
        Synth::Support(name, sig) => (sig.clone(), crate::bodies::support(&name), name),
        Synth::Division(op, conv) => {
            let brk = ctx.import(psx::BREAK);
            let sig = FuncType { params: vec![V::I32; 2], results: vec![V::I32] };
            (sig, crate::bodies::division(op, conv, brk), format!("r3000_{op}"))
        }
        Synth::InlineCopy => {
            let sig = FuncType { params: vec![V::I32; 3], results: vec![V::I32] };
            (sig, crate::bodies::inline_copy(None), "builtin_memcpy".into())
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
    })
}

/// Link the guest module.
pub fn link(map: &AddressMap, rodata: Option<&Rodata>, spec: &GameSpec, inputs: &[Input]) -> Result<Linked> {
    let view: Vec<(&str, &WasmObject)> = inputs.iter().map(|i| (i.container.as_str(), &i.obj)).collect();
    let layout = Layout::place(&view, map, rodata)?;
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
        o32_ty: 0,
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

    let mut bodies = Vec::new();
    let mut names: Vec<(u32, String)> = Vec::new();
    for &(i, f) in &ctx.guest_order.clone() {
        let obj = &inputs[i].obj;
        let ty = obj.types[obj.funcs[f as usize].ty as usize].clone();
        let tix = ctx.intern(&ty);
        let index = f + obj.num_imported_funcs();
        let name = obj.func_symbol(index).map_or_else(|| format!("{}#{f}", inputs[i].label), |s| s.name.clone());
        let b = rewrite::rewrite(obj, f as usize, &mut R { ctx: &mut ctx, input: i })
            .with_context(|| format!("{}: {name}", inputs[i].label))?;
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
    let invoke = ctx.synth(Synth::Indirect);
    let reg_fns = [
        ("guest_sp_get", ctx.synth(Synth::SpGet)),
        ("guest_sp_set", ctx.synth(Synth::SpSet)),
        ("guest_gp_get", ctx.synth(Synth::GpGet)),
        ("guest_gp_set", ctx.synth(Synth::GpSet)),
    ];

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
    module.section(&tables);
    let mut memory = MemorySection::new();
    memory.memory(MemoryType { minimum: spec.min_pages, maximum: None, memory64: false, shared: false, page_size_log2: None });
    module.section(&memory);
    let mut globals = GlobalSection::new();
    for init in [0, 0, spec.budget as i32] {
        globals.global(GlobalType { val_type: ValType::I32, mutable: true, shared: false }, &ConstExpr::i32_const(init));
    }
    module.section(&globals);
    let mut ex = ExportSection::new();
    ex.export(guest::MEMORY, ExportKind::Memory, 0);
    ex.export(guest::INVOKE, ExportKind::Func, invoke);
    for (n, f) in reg_fns {
        ex.export(n, ExportKind::Func, f);
    }
    ex.export(guest::SP, ExportKind::Global, ctx.sp);
    ex.export(guest::GP, ExportKind::Global, ctx.gp);
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
            let s = &ctx.inputs[input].obj.symbols[sym as usize];
            let n = match classify_name(&s.name) {
                NameClass::Guest(n) | NameClass::SignatureThunk { target: n } => n,
                NameClass::MainWrapper => bail!("main wrapper"),
            };
            ctx.map.resolve(&ctx.inputs[input].container, n).map(|r| r.addr).context("function without address")
        };
        let bytes = ctx.layout.bytes_of(&ctx.view, map, i, si, &fa)?;
        data.active(0, &ConstExpr::i32_const((WINDOW_BASE + (addr & WINDOW_MASK)) as i32), bytes);
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
    Ok(Linked { wasm: module.finish(), aux, matched })
}
