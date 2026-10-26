//! The link: guest objects (decompiled C and generated C) plus the
//! functions the linker synthesizes (o32 adapters, call-outs, intrinsics)
//! into one module.
//!
//! In [`Mode::Harness`] every call a guest function makes — to another
//! guest function, the BIOS, a coprocessor — goes to a single *call-out*
//! import, so the differential harness can log it and feed both sides of a
//! comparison the same results.

use crate::layout::{How, Layout, Rodata};
use crate::marshal;
use crate::object::{FuncType, NameClass, SymbolKind, WasmObject, classify_name};
use crate::rewrite::{self, Lowered, Resolver};
use anyhow::{Context as _, Result, anyhow, bail};
use psxw_abi::{WINDOW_BASE, WINDOW_MASK};
use psxw_cc::protos::Proto;
use psxw_inputs::AddressMap;
use std::collections::{BTreeMap, HashMap};
use wasm_encoder::{
    CodeSection, ConstExpr, DataSection, EntityType, ExportKind, ExportSection, Function, FunctionSection,
    GlobalSection, GlobalType, ImportSection, IndirectNameMap, Instruction as I, MemorySection, MemoryType, Module,
    NameMap, NameSection, TypeSection, ValType,
};

/// Call-out kinds: the first argument of the harness's `h.callout`.
pub mod callout {
    pub const CALL: u32 = 0;
    pub const INDIRECT: u32 = 1;
    pub const BIOS: u32 = 2;
    pub const SYSCALL: u32 = 3;
    pub const BREAK: u32 = 4;
    pub const COP0_READ: u32 = 5;
    pub const COP0_WRITE: u32 = 6;
    pub const RFE: u32 = 7;
    pub const GTE_MFC2: u32 = 8;
    pub const GTE_CFC2: u32 = 9;
    pub const GTE_MTC2: u32 = 10;
    pub const GTE_CTC2: u32 = 11;
    pub const GTE_EXEC: u32 = 12;
    pub const TRAP_OVERFLOW: u32 = 13;
    pub const BAD_JUMP: u32 = 14;
    pub const KERNEL: u32 = 15;

    pub fn name(kind: u32) -> &'static str {
        match kind {
            CALL => "call",
            INDIRECT => "indirect",
            BIOS => "bios",
            SYSCALL => "syscall",
            BREAK => "break",
            COP0_READ => "cop0-read",
            COP0_WRITE => "cop0-write",
            RFE => "rfe",
            GTE_MFC2 => "mfc2",
            GTE_CFC2 => "cfc2",
            GTE_MTC2 => "mtc2",
            GTE_CTC2 => "ctc2",
            GTE_EXEC => "gte",
            TRAP_OVERFLOW => "overflow",
            BAD_JUMP => "bad-jump",
            KERNEL => "kernel",
            _ => "?",
        }
    }
}

/// Functions GCC expanded inline, supplied by the linker (D29).
const HOST_SUPPORT: &[&str] = &["abs", "labs", "fabs"];

#[derive(Debug, Clone)]
pub struct Input {
    pub label: String,
    pub container: String,
    pub obj: WasmObject,
    /// Prototype facts by function name (C inputs only).
    pub protos: BTreeMap<String, Proto>,
    /// Generated (lift-tier) code: its local helpers are called directly.
    pub generated: bool,
    /// What the original compiler did that clang doesn't, for decompiled
    /// C. `None` for generated code, which carries the original's
    /// behaviour itself.
    pub gcc: Option<Gcc>,
}

/// GCC 2.95 behaviour C inputs are linked to reproduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Gcc {
    /// Divisions get these checks instead of trapping.
    pub division: Division,
    /// GCC expanded `memcpy` with a constant size inline (builtins on, as
    /// they are without `-fno-builtin`); such calls become a copy.
    pub builtin_memcpy: bool,
    /// GCC (without init sections) starts `main` with a call to `__main`.
    pub main_calls_main: bool,
}

/// GCC's checks on a variable divisor, as the original's assembler encoded
/// them: the `break` code for a zero divisor, and for INT_MIN / -1 if
/// checked at all (ASPSX checks only zero, with code 7; GNU as checks both,
/// with 7 << 10 and 6 << 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Division {
    pub zero: u32,
    pub overflow: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
pub enum Mode {
    /// Differential harness: every call is a call-out. With `dirty`, stores
    /// mark their page in a byte map at that wasm address.
    Harness { sp: u32, gp: u32, dirty: Option<u32> },
}

#[derive(Debug, Clone)]
pub struct ExportSpec {
    pub name: String,
    pub input: usize,
    pub function: String,
    /// Export through an o32 wrapper (C functions).
    pub o32: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Linked {
    pub wasm: Vec<u8>,
    /// Data placed in AUX: (input label, symbol, address).
    pub aux: Vec<(String, String, u32)>,
    pub matched: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Synth {
    /// C call to a guest function at `addr` with a call-site signature.
    CallStub { input: usize, name: String, addr: u32, sig: FuncType },
    /// Generated code's call to the function at `addr` (o32 form).
    O32Call { addr: u32 },
    /// o32 wrapper around a guest C function.
    O32Wrap { input: usize, func: u32, name: String },
    /// Replacement for C `call_indirect` with a signature.
    ICall { sig: FuncType },
    /// `psxw_call_indirect(addr, a0..a3)`.
    Indirect,
    /// A runtime service with its call-site signature.
    Service { name: String, sig: FuncType },
    /// abs/labs/fabs.
    Support { name: String, sig: FuncType },
    /// `(wasm dst, n)`: mark the window pages of a byte range dirty.
    MarkRange,
    /// R3000 division semantics for C code (`div_s`, `div_u`, `rem_s`, `rem_u`).
    Division(&'static str, Division),
    /// `memcpy(dst, src, n) -> dst` expanded as a copy.
    InlineCopy,
}

struct Ctx<'a> {
    map: &'a AddressMap,
    inputs: &'a [Input],
    view: Vec<(&'a str, &'a WasmObject)>,
    layout: Layout,
    mode: Mode,
    types: Vec<FuncType>,
    type_ix: HashMap<FuncType, u32>,
    nimports: u32,
    /// (input, defined function index) → output index.
    guest: HashMap<(usize, u32), u32>,
    guest_order: Vec<(usize, u32)>,
    synth: Vec<Synth>,
    synth_ix: HashMap<Synth, u32>,
    sp: u32,
    gp: u32,
}

const CALLOUT_IMPORT: u32 = 0;

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

    fn guest_addr(&self, input: usize, name: &str) -> Option<u32> {
        self.map.resolve(&self.inputs[input].container, name).map(|r| r.addr)
    }
}

/// Address from a generated symbol name: `psxw_f_[<container>_]<ADDR>`.
fn generated_addr(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("psxw_f_").or_else(|| name.strip_prefix("psxw_d_"))?;
    let hex = rest.rsplit('_').next()?;
    (hex.len() == 8).then(|| u32::from_str_radix(hex, 16).ok()).flatten()
}

struct InputResolver<'c, 'a> {
    ctx: &'c mut Ctx<'a>,
    input: usize,
}

const SERVICES: &[&str] = &[
    "psxw_bios",
    "psxw_kernel_call",
    "psxw_syscall",
    "__psxw_break",
    "psxw_trap_overflow",
    "psxw_bad_jump",
    "psxw_cop0_read",
    "psxw_cop0_write",
    "psxw_rfe",
    "psx_gte_mfc2",
    "psx_gte_cfc2",
    "psx_gte_mtc2",
    "psx_gte_ctc2",
    "psx_gte_exec",
];

impl Resolver for InputResolver<'_, '_> {
    fn call(&mut self, symbol: u32, sig: &FuncType, last_const: Option<i32>) -> Result<Lowered> {
        let inp = &self.ctx.inputs[self.input];
        let s = &inp.obj.symbols[symbol as usize];
        let name = s.name.clone();
        if name == "memcpy" && last_const.is_some() && inp.gcc.is_some_and(|g| g.builtin_memcpy) && !s.defined {
            return Ok(Lowered::Call(self.ctx.synth(Synth::InlineCopy)));
        }
        match name.as_str() {
            "psxw_sp_get" => return Ok(Lowered::Instrs(vec![I::GlobalGet(self.ctx.sp)])),
            "psxw_sp_set" => return Ok(Lowered::Instrs(vec![I::GlobalSet(self.ctx.sp)])),
            "psxw_gp_get" => return Ok(Lowered::Instrs(vec![I::GlobalGet(self.ctx.gp)])),
            "psxw_gp_set" => return Ok(Lowered::Instrs(vec![I::GlobalSet(self.ctx.gp)])),
            "psxw_call_indirect" => return Ok(Lowered::Call(self.ctx.synth(Synth::Indirect))),
            n if SERVICES.contains(&n) => {
                return Ok(Lowered::Call(self.ctx.synth(Synth::Service { name, sig: sig.clone() })));
            }
            n if HOST_SUPPORT.contains(&n) && !s.defined => {
                return Ok(Lowered::Call(self.ctx.synth(Synth::Support { name, sig: sig.clone() })));
            }
            _ => {}
        }
        if let Some(addr) = generated_addr(&name) {
            return Ok(Lowered::Call(self.ctx.synth(Synth::O32Call { addr })));
        }
        // A helper defined in the same object with no guest address (the
        // generated code's inline helpers) is called directly.
        if s.defined {
            if let Some(idx) = s.index {
                let local = idx.checked_sub(inp.obj.num_imported_funcs());
                let is_guest = matches!(classify_name(&name), NameClass::Guest(n) if self.ctx.guest_addr(self.input, n).is_some());
                if let Some(local) = local {
                    if inp.generated || !is_guest {
                        if let Some(&i) = self.ctx.guest.get(&(self.input, local)) {
                            return Ok(Lowered::Call(i));
                        }
                    }
                }
            }
        }
        let callee = match classify_name(&name) {
            NameClass::Guest(n) => n.to_string(),
            NameClass::SignatureThunk { target } => target.to_string(),
            NameClass::MainWrapper => bail!("call to clang's main wrapper {name}"),
        };
        let addr = self.ctx.guest_addr(self.input, &callee).with_context(|| format!("function {callee} has no address"))?;
        Ok(Lowered::Call(self.ctx.synth(Synth::CallStub { input: self.input, name: callee, addr, sig: sig.clone() })))
    }

    fn call_indirect(&mut self, sig: &FuncType) -> Result<u32> {
        Ok(self.ctx.synth(Synth::ICall { sig: sig.clone() }))
    }

    fn global(&mut self, symbol: u32) -> Result<u32> {
        let s = &self.ctx.inputs[self.input].obj.symbols[symbol as usize];
        match s.name.as_str() {
            "__stack_pointer" => Ok(self.ctx.sp),
            other => bail!("guest code uses global {other}"),
        }
    }

    fn data_addr(&mut self, symbol: u32) -> Result<u32> {
        self.ctx.layout.data_addr(&self.ctx.view, self.ctx.map, self.input, symbol)
    }

    fn func_addr(&mut self, symbol: u32) -> Result<u32> {
        func_addr(self.ctx, self.input, symbol)
    }

    fn type_index(&mut self, ty: u32) -> u32 {
        let t = self.ctx.inputs[self.input].obj.types[ty as usize].clone();
        self.ctx.intern(&t)
    }

    fn dirty_map(&self) -> Option<u32> {
        match self.ctx.mode {
            Mode::Harness { dirty, .. } => dirty,
        }
    }

    fn mark_range(&mut self) -> u32 {
        self.ctx.synth(Synth::MarkRange)
    }

    fn prologue(&mut self, func_symbol: Option<&str>) -> Result<Vec<I<'static>>> {
        let inp = &self.ctx.inputs[self.input];
        let is_main = func_symbol.is_some_and(|n| classify_name(n) == NameClass::Guest("main"));
        if !(is_main && inp.gcc.is_some_and(|g| g.main_calls_main)) {
            return Ok(Vec::new());
        }
        let Some(addr) = self.ctx.guest_addr(self.input, "__main") else { return Ok(Vec::new()) };
        let sig = FuncType { params: vec![], results: vec![] };
        let stub = self.ctx.synth(Synth::CallStub { input: self.input, name: "__main".into(), addr, sig });
        Ok(vec![I::Call(stub)])
    }

    fn division(&mut self, op: &'static str) -> Option<u32> {
        let d = self.ctx.inputs[self.input].gcc?.division;
        Some(self.ctx.synth(Synth::Division(op, d)))
    }
}

fn func_addr(ctx: &Ctx, input: usize, symbol: u32) -> Result<u32> {
    let s = &ctx.inputs[input].obj.symbols[symbol as usize];
    let name = match classify_name(&s.name) {
        NameClass::Guest(n) => n,
        NameClass::SignatureThunk { target } => target,
        NameClass::MainWrapper => bail!("address of clang's main wrapper"),
    };
    ctx.guest_addr(input, name).with_context(|| format!("function pointer to {name}, which has no address"))
}

pub fn link(
    map: &AddressMap,
    rodata: Option<&Rodata>,
    mode: Mode,
    inputs: &[Input],
    exports: &[ExportSpec],
) -> Result<Linked> {
    let view: Vec<(&str, &WasmObject)> = inputs.iter().map(|i| (i.container.as_str(), &i.obj)).collect();
    let layout = Layout::place(&view, map, rodata)?;
    let mut ctx = Ctx {
        map,
        inputs,
        view,
        layout,
        mode,
        types: Vec::new(),
        type_ix: HashMap::new(),
        nimports: 1,
        guest: HashMap::new(),
        guest_order: Vec::new(),
        synth: Vec::new(),
        synth_ix: HashMap::new(),
        sp: 0,
        gp: 1,
    };
    let callout_ty = ctx.intern(&FuncType {
        params: vec![wasmparser::ValType::I32; 8],
        results: vec![wasmparser::ValType::I64],
    });

    // Every defined function except clang's thunks and main wrappers.
    for (i, inp) in inputs.iter().enumerate() {
        for f in 0..inp.obj.funcs.len() as u32 {
            let index = f + inp.obj.num_imported_funcs();
            let skip = inp.obj.func_symbol(index).is_some_and(|s| {
                !matches!(classify_name(&s.name), NameClass::Guest(_))
            });
            if !skip {
                let out = ctx.nimports + ctx.guest_order.len() as u32;
                ctx.guest.insert((i, f), out);
                ctx.guest_order.push((i, f));
            }
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
        let body = rewrite::rewrite(obj, f as usize, &mut InputResolver { ctx: &mut ctx, input: i })
            .with_context(|| format!("{}: {name}", inputs[i].label))?;
        names.push((ctx.guest[&(i, f)], name));
        bodies.push((tix, body));
    }

    // Exports.
    let mut export_list: Vec<(String, u32)> = Vec::new();
    for e in exports {
        let obj = &inputs[e.input].obj;
        let sym = obj
            .symbols
            .iter()
            .find(|s| {
                s.kind == SymbolKind::Function && s.defined && classify_name(&s.name) == NameClass::Guest(&e.function)
            })
            .with_context(|| format!("{} defines no function {}", inputs[e.input].label, e.function))?;
        let local = sym.index.context("function symbol without index")? - obj.num_imported_funcs();
        let idx = if e.o32 {
            ctx.synth(Synth::O32Wrap { input: e.input, func: local, name: e.function.clone() })
        } else {
            ctx.guest[&(e.input, local)]
        };
        export_list.push((e.name.clone(), idx));
    }

    // Synthesized functions (their bodies request nothing further).
    let mut si = 0;
    while si < ctx.synth.len() {
        let s = ctx.synth[si].clone();
        let (ty, body, name) = synth_body(&mut ctx, &s, callout_ty)?;
        let tix = ctx.intern(&ty);
        names.push((ctx.nimports + ctx.guest_order.len() as u32 + si as u32, name));
        bodies.push((tix, body));
        si += 1;
    }

    // Module.
    let mut module = Module::new();
    let mut types = TypeSection::new();
    for t in &ctx.types {
        types.ty().function(t.params.iter().map(|&v| marshal::val(v)), t.results.iter().map(|&v| marshal::val(v)));
    }
    module.section(&types);
    let mut imports = ImportSection::new();
    imports.import("h", "callout", EntityType::Function(callout_ty));
    module.section(&imports);
    let mut functions = FunctionSection::new();
    for (t, _) in &bodies {
        functions.function(*t);
    }
    module.section(&functions);
    let mut memory = MemorySection::new();
    // A guard page past the window: bulk copies at random harness pointers
    // near the window's top must not trap where the original's word loads
    // wrap.
    let pages = (psxw_abi::RUNTIME_BASE as u64) / 65536 + 1;
    memory.memory(MemoryType { minimum: pages, maximum: Some(pages), memory64: false, shared: false, page_size_log2: None });
    module.section(&memory);
    let Mode::Harness { sp, gp, .. } = ctx.mode;
    let mut globals = GlobalSection::new();
    for init in [sp, gp] {
        globals.global(
            GlobalType { val_type: ValType::I32, mutable: true, shared: false },
            &ConstExpr::i32_const(init as i32),
        );
    }
    module.section(&globals);
    let mut ex = ExportSection::new();
    ex.export("memory", ExportKind::Memory, 0);
    ex.export("__guest_sp", ExportKind::Global, ctx.sp);
    ex.export("__guest_gp", ExportKind::Global, ctx.gp);
    for (n, i) in &export_list {
        ex.export(n, ExportKind::Func, *i);
    }
    module.section(&ex);
    let mut code = CodeSection::new();
    for (_, b) in &bodies {
        code.function(b);
    }
    module.section(&code);

    // AUX data.
    let mut data = DataSection::new();
    let mut aux = Vec::new();
    for &(i, si, addr) in &ctx.layout.aux.clone() {
        let fa = |input: usize, sym: u32| func_addr(&ctx, input, sym);
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
    ns.locals(&IndirectNameMap::new());
    module.section(&ns);

    let wasm = module.finish();
    let matched = ctx.layout.defined.values().filter(|p| p.how == How::Matched).count();
    Ok(Linked { wasm, aux, matched })
}

/// Push a call-out's operands and call it: kind, target, nwords, a0..a3, sp.
fn callout_call(f: &mut Function, kind: u32, target: &dyn Fn(&mut Function), nwords: u32, args: [Option<u32>; 4], sp: u32) {
    f.instruction(&I::I32Const(kind as i32));
    target(f);
    f.instruction(&I::I32Const(nwords as i32));
    for a in args {
        match a {
            Some(l) => f.instruction(&I::LocalGet(l)),
            None => f.instruction(&I::I32Const(0)),
        };
    }
    f.instruction(&I::GlobalGet(sp));
    f.instruction(&I::Call(CALLOUT_IMPORT));
}

fn o32_sig() -> FuncType {
    FuncType { params: vec![wasmparser::ValType::I32; 4], results: vec![wasmparser::ValType::I64] }
}

fn synth_body(ctx: &mut Ctx, s: &Synth, _callout_ty: u32) -> Result<(FuncType, Function, String)> {
    let sp = ctx.sp;
    Ok(match s {
        Synth::CallStub { input, name, addr, sig } => {
            let proto = ctx.inputs[*input].protos.get(name);
            let (kinds, variadic) = marshal::param_kinds(sig, proto);
            let (words, fixed) = marshal::words(&kinds, variadic);
            let addr = *addr;
            let body = marshal::c_to_o32(
                sig,
                &words,
                sp,
                &|f| {
                    f.instruction(&I::I32Const(callout::CALL as i32));
                    f.instruction(&I::I32Const(addr as i32));
                    f.instruction(&I::I32Const(fixed as i32));
                },
                &|f, new_sp| {
                    f.instruction(&I::LocalGet(new_sp));
                },
                &|f| {
                    f.instruction(&I::Call(CALLOUT_IMPORT));
                },
            );
            (sig.clone(), body, format!("call:{name}"))
        }
        Synth::O32Call { addr } => {
            let mut f = Function::new([]);
            let a = *addr;
            callout_call(&mut f, callout::CALL, &|f| {
                f.instruction(&I::I32Const(a as i32));
            }, 4, [Some(0), Some(1), Some(2), Some(3)], sp);
            f.instruction(&I::End);
            (o32_sig(), f, format!("call:{a:08X}"))
        }
        Synth::Indirect => {
            let mut f = Function::new([]);
            callout_call(&mut f, callout::INDIRECT, &|f| {
                f.instruction(&I::LocalGet(0));
            }, 4, [Some(1), Some(2), Some(3), Some(4)], sp);
            f.instruction(&I::End);
            let sig = FuncType { params: vec![wasmparser::ValType::I32; 5], results: vec![wasmparser::ValType::I64] };
            (sig, f, "call_indirect".into())
        }
        Synth::ICall { sig } => {
            let (kinds, _) = marshal::param_kinds(sig, None);
            let (words, fixed) = marshal::words(&kinds, false);
            let mut helper = sig.clone();
            helper.params.push(wasmparser::ValType::I32);
            let target = sig.params.len() as u32;
            let body = marshal::c_to_o32(
                &helper,
                &words,
                sp,
                &|f| {
                    f.instruction(&I::I32Const(callout::INDIRECT as i32));
                    f.instruction(&I::LocalGet(target));
                    f.instruction(&I::I32Const(fixed as i32));
                },
                &|f, new_sp| {
                    f.instruction(&I::LocalGet(new_sp));
                },
                &|f| {
                    f.instruction(&I::Call(CALLOUT_IMPORT));
                },
            );
            (helper, body, "icall".into())
        }
        Synth::Service { name, sig } => {
            let mut f = Function::new([]);
            let p = |n: u32| Some(n);
            let (kind, target, nwords, args, noreturn): (u32, Option<u32>, u32, [Option<u32>; 4], bool) =
                match name.as_str() {
                    "psxw_bios" => (callout::BIOS, None, 4, [p(2), p(3), p(4), p(5)], false),
                    "psxw_kernel_call" => (callout::KERNEL, p(0), 4, [p(1), p(2), p(3), p(4)], false),
                    "psxw_syscall" => (callout::SYSCALL, None, 4, [p(0), p(1), p(2), p(3)], false),
                    "__psxw_break" => (callout::BREAK, p(0), 0, [None; 4], false),
                    "psxw_trap_overflow" => (callout::TRAP_OVERFLOW, p(0), 0, [None; 4], true),
                    "psxw_bad_jump" => (callout::BAD_JUMP, p(0), 1, [p(1), None, None, None], true),
                    "psxw_cop0_read" => (callout::COP0_READ, p(0), 0, [None; 4], false),
                    "psxw_cop0_write" => (callout::COP0_WRITE, p(0), 1, [p(1), None, None, None], false),
                    "psxw_rfe" => (callout::RFE, None, 0, [None; 4], false),
                    "psx_gte_mfc2" => (callout::GTE_MFC2, p(0), 0, [None; 4], false),
                    "psx_gte_cfc2" => (callout::GTE_CFC2, p(0), 0, [None; 4], false),
                    "psx_gte_mtc2" => (callout::GTE_MTC2, p(0), 1, [p(1), None, None, None], false),
                    "psx_gte_ctc2" => (callout::GTE_CTC2, p(0), 1, [p(1), None, None, None], false),
                    "psx_gte_exec" => (callout::GTE_EXEC, p(0), 0, [None; 4], false),
                    other => return Err(anyhow!("no harness lowering for {other}")),
                };
            if name == "psxw_bios" {
                // target = table << 8 | function
                callout_call(&mut f, kind, &|f| {
                    f.instruction(&I::LocalGet(0));
                    f.instruction(&I::I32Const(8));
                    f.instruction(&I::I32Shl);
                    f.instruction(&I::LocalGet(1));
                    f.instruction(&I::I32Or);
                }, nwords, args, sp);
            } else {
                callout_call(&mut f, kind, &|f| {
                    match target {
                        Some(l) => f.instruction(&I::LocalGet(l)),
                        None => f.instruction(&I::I32Const(0)),
                    };
                }, nwords, args, sp);
            }
            if noreturn {
                f.instruction(&I::Unreachable);
            } else {
                marshal::from_o32_result(&mut f, &sig.results);
            }
            f.instruction(&I::End);
            (sig.clone(), f, name.clone())
        }
        Synth::Support { name, sig } => (sig.clone(), crate::bodies::support(name), name.clone()),
        Synth::MarkRange => {
            // for (p = (dst - BASE) >> S; p <= (dst - BASE + n - 1) >> S; p++) map[p] = 1
            let Mode::Harness { dirty, .. } = ctx.mode;
            let map = dirty.unwrap_or(0);
            let shift = rewrite::DIRTY_PAGE_SHIFT as i32;
            let mut f = Function::new([(2, ValType::I32)]);
            f.instruction(&I::LocalGet(1));
            f.instruction(&I::I32Eqz);
            f.instruction(&I::If(wasm_encoder::BlockType::Empty));
            f.instruction(&I::Return);
            f.instruction(&I::End);
            f.instruction(&I::LocalGet(0));
            f.instruction(&I::I32Const(WINDOW_BASE as i32));
            f.instruction(&I::I32Sub);
            f.instruction(&I::LocalTee(2));
            f.instruction(&I::LocalGet(1));
            f.instruction(&I::I32Add);
            f.instruction(&I::I32Const(1));
            f.instruction(&I::I32Sub);
            f.instruction(&I::I32Const(shift));
            f.instruction(&I::I32ShrU);
            f.instruction(&I::LocalSet(3));
            f.instruction(&I::LocalGet(2));
            f.instruction(&I::I32Const(shift));
            f.instruction(&I::I32ShrU);
            f.instruction(&I::LocalSet(2));
            f.instruction(&I::Loop(wasm_encoder::BlockType::Empty));
            f.instruction(&I::LocalGet(2));
            f.instruction(&I::I32Const(0xFFF));
            f.instruction(&I::I32And);
            f.instruction(&I::I32Const(1));
            f.instruction(&I::I32Store8(wasm_encoder::MemArg { offset: map as u64, align: 0, memory_index: 0 }));
            f.instruction(&I::LocalGet(2));
            f.instruction(&I::I32Const(1));
            f.instruction(&I::I32Add);
            f.instruction(&I::LocalTee(2));
            f.instruction(&I::LocalGet(3));
            f.instruction(&I::I32LeU);
            f.instruction(&I::BrIf(0));
            f.instruction(&I::End);
            f.instruction(&I::End);
            let sig = FuncType { params: vec![wasmparser::ValType::I32; 2], results: vec![] };
            (sig, f, "mark_range".into())
        }
        Synth::InlineCopy => {
            let Mode::Harness { dirty, .. } = ctx.mode;
            let mark = dirty.map(|_| ctx.synth(Synth::MarkRange));
            let sig = FuncType { params: vec![wasmparser::ValType::I32; 3], results: vec![wasmparser::ValType::I32] };
            (sig, crate::bodies::inline_copy(mark), "builtin_memcpy".into())
        }
        Synth::Division(op, conv) => {
            let brk = ctx.synth(Synth::Service {
                name: "__psxw_break".into(),
                sig: FuncType { params: vec![wasmparser::ValType::I32], results: vec![] },
            });
            let sig = FuncType { params: vec![wasmparser::ValType::I32; 2], results: vec![wasmparser::ValType::I32] };
            (sig, crate::bodies::division(op, *conv, brk), format!("r3000_{op}"))
        }
        Synth::O32Wrap { input, func, name } => {
            let obj = &ctx.inputs[*input].obj;
            let sig = obj.types[obj.funcs[*func as usize].ty as usize].clone();
            let proto = ctx.inputs[*input].protos.get(name);
            let callee = ctx.guest[&(*input, *func)];
            let body = marshal::o32_to_c(&sig, proto, callee, sp);
            (o32_sig(), body, format!("o32:{name}"))
        }
    })
}

