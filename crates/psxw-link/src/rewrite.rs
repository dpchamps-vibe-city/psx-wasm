//! Function bodies are decoded with `wasmparser` and re-encoded with
//! `wasm-encoder`, applying relocations and the guest passes on the way
//! (docs/design.md §8):
//!
//! 1. **Masking.** Every load, store, `memory.copy` and `memory.fill`
//!    address becomes `(addr + offset) & WINDOW_MASK`, with the memarg
//!    offset set to `WINDOW_BASE`.
//! 2. **Guest SP.** `__stack_pointer` becomes the guest's `$sp` global.
//! 3. **Calls** are resolved by the linker, which may lower intrinsics to
//!    plain instructions.
//! 4. **`call_indirect`** sites call an o32 helper for their signature,
//!    because function pointers are MIPS addresses.

use crate::object::{FuncType, WasmObject};
use anyhow::{Context as _, Result, bail};
use psxw_abi::{WINDOW_BASE, WINDOW_MASK};
use wasm_encoder::reencode::Reencode;
use wasm_encoder::{Function, Instruction as I, MemArg, ValType};
use wasmparser::{BinaryReader, FunctionBody, Operator, RelocationType};

/// How a call to a symbol is lowered.
pub enum Lowered {
    Call(u32),
    Instrs(Vec<I<'static>>),
}

/// What the rewriter needs from the linker.
pub trait Resolver {
    /// A `call` to function symbol `symbol` with the object's `sig`;
    /// `last_const` is the immediately preceding `i32.const`, if any (the
    /// last argument).
    fn call(&mut self, symbol: u32, sig: &FuncType, last_const: Option<i32>) -> Result<Lowered>;
    /// The helper replacing `call_indirect` with signature `sig`; it takes
    /// the MIPS address as an extra trailing i32.
    fn call_indirect(&mut self, sig: &FuncType) -> Result<u32>;
    /// Output global for global symbol `symbol`.
    fn global(&mut self, symbol: u32) -> Result<u32>;
    /// Guest address of data symbol `symbol`.
    fn data_addr(&mut self, symbol: u32) -> Result<u32>;
    /// MIPS address of function symbol `symbol` (a function pointer).
    fn func_addr(&mut self, symbol: u32) -> Result<u32>;
    /// Output type index for an object type index (block types).
    fn type_index(&mut self, ty: u32) -> u32;
    /// When set, every store marks its 4 KiB page in a byte map at this
    /// wasm address (harness builds compare only touched pages).
    fn dirty_map(&self) -> Option<u32>;
    /// Helper `(dst, n)` marking a byte range dirty (bulk memory ops).
    fn mark_range(&mut self) -> u32;
    /// For decompiled C: the helper replacing a 32-bit division or
    /// remainder (`op` is `div_s`, `div_u`, `rem_s` or `rem_u`) with the
    /// R3000's results and GCC's `break` checks, instead of a wasm trap.
    /// `None` keeps the instruction.
    fn division(&mut self, op: &'static str) -> Option<u32>;
    /// Instructions to run at the start of a function (GCC's `main` calls
    /// `__main`).
    fn prologue(&mut self, func_symbol: Option<&str>) -> Result<Vec<I<'static>>>;
    /// Instructions at the top of every loop body: the safepoint budget.
    fn loop_entry(&mut self) -> Vec<I<'static>> {
        Vec::new()
    }
    /// When set, the function (`(window offset, kind) -> ()`) every access
    /// to the I/O window calls: a load before reading (so a device model
    /// can put the register's value in place), a store after writing (so
    /// it can take the value). `kind` is the size in bytes, plus
    /// [`IO_STORE`] for stores.
    fn io_check(&mut self) -> Option<u32> {
        None
    }
}

struct TypeMap<'r, R: Resolver> {
    r: &'r mut R,
}

impl<R: Resolver> Reencode for TypeMap<'_, R> {
    type Error = std::convert::Infallible;
    fn type_index(&mut self, ty: u32) -> Result<u32, wasm_encoder::reencode::Error<Self::Error>> {
        Ok(self.r.type_index(ty))
    }
}

fn masked(align: u32) -> MemArg {
    MemArg { offset: WINDOW_BASE as u64, align, memory_index: 0 }
}

/// `kind` bit of an I/O hook call for stores.
pub const IO_STORE: u32 = 0x100;

/// Page granularity of the dirty map.
pub const DIRTY_PAGE_SHIFT: u32 = 12;

/// Rewrite defined function `func` (index into `obj.funcs`).
pub fn rewrite<R: Resolver>(obj: &WasmObject, func: usize, r: &mut R) -> Result<Function> {
    let dirty = r.dirty_map();
    let io = r.io_check();
    let f = &obj.funcs[func];
    let ty = &obj.types[f.ty as usize];
    let body = FunctionBody::new(BinaryReader::new(&obj.bytes[f.body.clone()], f.body.start as u64));
    let mut locals: Vec<(u32, ValType)> = Vec::new();
    let mut nlocals = ty.params.len() as u32;
    for l in body.get_locals_reader()? {
        let (n, t) = l?;
        locals.push((n, crate::marshal::val(t)));
        nlocals += n;
    }
    // Scratch locals for the masking pass.
    let t_i32 = nlocals;
    let t_i32b = nlocals + 1;
    let t_i64 = nlocals + 2;
    let t_f32 = nlocals + 3;
    let t_f64 = nlocals + 4;
    let t_addr = nlocals + 5;
    locals.extend([(2, ValType::I32), (1, ValType::I64), (1, ValType::F32), (1, ValType::F64), (1, ValType::I32)]);
    let mut out = Function::new(locals);
    let index = func as u32 + obj.num_imported_funcs();
    for i in r.prologue(obj.func_symbol(index).map(|s| s.name.as_str()))? {
        out.instruction(&i);
    }

    let relocs = obj.relocs_for(&f.body);
    let mut ri = 0;
    let mut last_const: Option<i32> = None;
    let mut ops = body.get_operators_reader()?;
    while !ops.eof() {
        let pos = ops.original_position() as u32;
        let op = ops.read()?;
        let end = ops.original_position() as u32;
        let base = obj.code_start as u32;
        // Relocations inside this operator's immediates.
        let mut here = Vec::new();
        while ri < relocs.len() && relocs[ri].offset + base < end {
            if relocs[ri].offset + base >= pos {
                here.push(relocs[ri]);
            }
            ri += 1;
        }
        let reloc_of = |t: RelocationType| here.iter().find(|r| r.ty == t).copied();
        let prev_const = last_const.take();
        if let Operator::I32Const { value } = op {
            if here.is_empty() {
                last_const = Some(value);
            }
        }
        let mem_reloc = |r: &mut R| -> Result<Option<u64>> {
            match reloc_of(RelocationType::MemoryAddrLeb) {
                Some(rl) => Ok(Some(r.data_addr(rl.symbol)?.wrapping_add(rl.addend as u32) as u64)),
                None => Ok(None),
            }
        };
        macro_rules! load {
            ($memarg:expr, $ins:ident, $size:expr) => {{
                let off = mem_reloc(r)?.unwrap_or($memarg.offset);
                if off != 0 {
                    out.instruction(&I::I32Const(off as u32 as i32));
                    out.instruction(&I::I32Add);
                }
                out.instruction(&I::I32Const(WINDOW_MASK as i32));
                out.instruction(&I::I32And);
                if let Some(check) = io {
                    io_guard(&mut out, t_addr, check, $size);
                }
                out.instruction(&I::$ins(masked($memarg.align as u32)));
            }};
        }
        macro_rules! store {
            ($memarg:expr, $ins:ident, $tmp:expr, $size:expr) => {{
                let off = mem_reloc(r)?.unwrap_or($memarg.offset);
                out.instruction(&I::LocalSet($tmp));
                if off != 0 {
                    out.instruction(&I::I32Const(off as u32 as i32));
                    out.instruction(&I::I32Add);
                }
                out.instruction(&I::I32Const(WINDOW_MASK as i32));
                out.instruction(&I::I32And);
                if dirty.is_some() || io.is_some() {
                    out.instruction(&I::LocalTee(t_addr));
                }
                out.instruction(&I::LocalGet($tmp));
                out.instruction(&I::$ins(masked($memarg.align as u32)));
                if let Some(check) = io {
                    io_after_store(&mut out, t_addr, check, $size);
                }
                if let Some(map) = dirty {
                    out.instruction(&I::LocalGet(t_addr));
                    out.instruction(&I::I32Const(DIRTY_PAGE_SHIFT as i32));
                    out.instruction(&I::I32ShrU);
                    out.instruction(&I::I32Const(1));
                    out.instruction(&I::I32Store8(MemArg { offset: map as u64, align: 0, memory_index: 0 }));
                }
            }};
        }
        match op {
            Operator::Call { function_index } => {
                let rl = reloc_of(RelocationType::FunctionIndexLeb).context("call without relocation")?;
                let sig = obj.func_type(function_index).clone();
                match r.call(rl.symbol, &sig, prev_const)? {
                    Lowered::Call(i) => {
                        out.instruction(&I::Call(i));
                    }
                    Lowered::Instrs(v) => {
                        for i in &v {
                            out.instruction(i);
                        }
                    }
                }
            }
            Operator::CallIndirect { type_index, .. } => {
                let sig = obj.types[type_index as usize].clone();
                let helper = r.call_indirect(&sig)?;
                out.instruction(&I::Call(helper));
            }
            Operator::GlobalGet { .. } => {
                let rl = reloc_of(RelocationType::GlobalIndexLeb).context("global.get without relocation")?;
                let g = r.global(rl.symbol)?;
                out.instruction(&I::GlobalGet(g));
            }
            Operator::GlobalSet { .. } => {
                let rl = reloc_of(RelocationType::GlobalIndexLeb).context("global.set without relocation")?;
                let g = r.global(rl.symbol)?;
                out.instruction(&I::GlobalSet(g));
            }
            Operator::I32Const { value } => {
                let v = if let Some(rl) = reloc_of(RelocationType::MemoryAddrSleb) {
                    r.data_addr(rl.symbol)?.wrapping_add(rl.addend as u32) as i32
                } else if let Some(rl) = reloc_of(RelocationType::TableIndexSleb) {
                    r.func_addr(rl.symbol)? as i32
                } else {
                    value
                };
                out.instruction(&I::I32Const(v));
            }
            Operator::I32Load { memarg } => load!(memarg, I32Load, 4),
            Operator::I64Load { memarg } => load!(memarg, I64Load, 8),
            Operator::F32Load { memarg } => load!(memarg, F32Load, 4),
            Operator::F64Load { memarg } => load!(memarg, F64Load, 8),
            Operator::I32Load8S { memarg } => load!(memarg, I32Load8S, 1),
            Operator::I32Load8U { memarg } => load!(memarg, I32Load8U, 1),
            Operator::I32Load16S { memarg } => load!(memarg, I32Load16S, 2),
            Operator::I32Load16U { memarg } => load!(memarg, I32Load16U, 2),
            Operator::I64Load8S { memarg } => load!(memarg, I64Load8S, 1),
            Operator::I64Load8U { memarg } => load!(memarg, I64Load8U, 1),
            Operator::I64Load16S { memarg } => load!(memarg, I64Load16S, 2),
            Operator::I64Load16U { memarg } => load!(memarg, I64Load16U, 2),
            Operator::I64Load32S { memarg } => load!(memarg, I64Load32S, 4),
            Operator::I64Load32U { memarg } => load!(memarg, I64Load32U, 4),
            Operator::I32Store { memarg } => store!(memarg, I32Store, t_i32, 4),
            Operator::I32Store8 { memarg } => store!(memarg, I32Store8, t_i32, 1),
            Operator::I32Store16 { memarg } => store!(memarg, I32Store16, t_i32, 2),
            Operator::I64Store { memarg } => store!(memarg, I64Store, t_i64, 8),
            Operator::I64Store8 { memarg } => store!(memarg, I64Store8, t_i64, 1),
            Operator::I64Store16 { memarg } => store!(memarg, I64Store16, t_i64, 2),
            Operator::I64Store32 { memarg } => store!(memarg, I64Store32, t_i64, 4),
            Operator::F32Store { memarg } => store!(memarg, F32Store, t_f32, 4),
            Operator::F64Store { memarg } => store!(memarg, F64Store, t_f64, 8),
            Operator::MemoryCopy { .. } => {
                // [dst, src, n]
                out.instruction(&I::LocalSet(t_i32b));
                out.instruction(&I::LocalSet(t_i32));
                mask_plus_base(&mut out);
                if dirty.is_some() {
                    mark(&mut out, t_addr, t_i32b, r.mark_range());
                }
                out.instruction(&I::LocalGet(t_i32));
                mask_plus_base(&mut out);
                out.instruction(&I::LocalGet(t_i32b));
                out.instruction(&I::MemoryCopy { src_mem: 0, dst_mem: 0 });
            }
            Operator::MemoryFill { .. } => {
                // [dst, value, n]
                out.instruction(&I::LocalSet(t_i32b));
                out.instruction(&I::LocalSet(t_i32));
                mask_plus_base(&mut out);
                if dirty.is_some() {
                    mark(&mut out, t_addr, t_i32b, r.mark_range());
                }
                out.instruction(&I::LocalGet(t_i32));
                out.instruction(&I::LocalGet(t_i32b));
                out.instruction(&I::MemoryFill(0));
            }
            Operator::I32DivS | Operator::I32DivU | Operator::I32RemS | Operator::I32RemU => {
                let name = match op {
                    Operator::I32DivS => "div_s",
                    Operator::I32DivU => "div_u",
                    Operator::I32RemS => "rem_s",
                    _ => "rem_u",
                };
                match r.division(name) {
                    Some(h) => {
                        out.instruction(&I::Call(h));
                    }
                    None => {
                        let ins = TypeMap { r: &mut *r }.instruction(op).map_err(|e| anyhow::anyhow!("{e:?}"))?;
                        out.instruction(&ins);
                    }
                }
            }
            Operator::MemorySize { .. } | Operator::MemoryGrow { .. } => {
                bail!("guest code uses memory.size/grow")
            }
            Operator::ReturnCall { .. } | Operator::ReturnCallIndirect { .. } => {
                bail!("guest code uses tail calls")
            }
            other => {
                if !here.is_empty() {
                    bail!("unhandled relocation {:?} on {other:?}", here[0].ty);
                }
                let is_loop = matches!(other, Operator::Loop { .. });
                let ins = TypeMap { r: &mut *r }.instruction(other).map_err(|e| anyhow::anyhow!("{e:?}"))?;
                out.instruction(&ins);
                if is_loop {
                    for i in r.loop_entry() {
                        out.instruction(&i);
                    }
                }
            }
        }
    }
    Ok(out)
}

/// With the masked address on the stack: call `check` when it lies in the
/// I/O window (a load of `size` bytes), leaving the address in place.
fn io_guard(out: &mut Function, t_addr: u32, check: u32, size: u32) {
    out.instruction(&I::LocalTee(t_addr));
    io_call(out, t_addr, check, size);
    out.instruction(&I::LocalGet(t_addr));
}

/// After a store of `size` bytes to the address in `t_addr`: call `check`
/// when it lies in the I/O window.
fn io_after_store(out: &mut Function, t_addr: u32, check: u32, size: u32) {
    out.instruction(&I::LocalGet(t_addr));
    io_call(out, t_addr, check, size | IO_STORE);
}

/// With the address on the stack (consumed): `check(addr, kind)` when it
/// lies in the I/O window.
fn io_call(out: &mut Function, t_addr: u32, check: u32, kind: u32) {
    let io = psxw_abi::phys::IO;
    out.instruction(&I::I32Const(io.start as i32));
    out.instruction(&I::I32Sub);
    out.instruction(&I::I32Const((io.end - io.start) as i32));
    out.instruction(&I::I32LtU);
    out.instruction(&I::If(wasm_encoder::BlockType::Empty));
    out.instruction(&I::LocalGet(t_addr));
    out.instruction(&I::I32Const(kind as i32));
    out.instruction(&I::Call(check));
    out.instruction(&I::End);
}

/// Mark `[dst, dst + n)` dirty, keeping `dst` (a wasm address) on the stack.
fn mark(out: &mut Function, t_addr: u32, n: u32, helper: u32) {
    out.instruction(&I::LocalTee(t_addr));
    out.instruction(&I::LocalGet(n));
    out.instruction(&I::Call(helper));
    out.instruction(&I::LocalGet(t_addr));
}

/// `(addr & WINDOW_MASK) + WINDOW_BASE`, for bulk-memory operands.
fn mask_plus_base(out: &mut Function) {
    out.instruction(&I::I32Const(WINDOW_MASK as i32));
    out.instruction(&I::I32And);
    out.instruction(&I::I32Const(WINDOW_BASE as i32));
    out.instruction(&I::I32Add);
}
