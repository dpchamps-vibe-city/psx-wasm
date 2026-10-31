//! Function bodies are decoded with `wasmparser` and re-encoded with
//! `wasm-encoder`, applying relocations and the guest passes on the way
//! (docs/design.md §8):
//!
//! 1. **Masking.** Every load, store, `memory.copy` and `memory.fill`
//!    address becomes `(addr + offset) & WINDOW_MASK`, with the memarg
//!    offset set to the window's base (`WINDOW_BASE`, or a debug build's).
//!    A bulk copy or fill reaches the I/O hook as its bytes would, and
//!    spends the safepoint budget (`Resolver::bulk_spend`).
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
    /// `memcpy(dst, src, n) -> dst` as a bulk copy (GCC's builtin, D36).
    Copy,
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
    /// Instructions after a bulk copy or fill of the bytes counted in local
    /// `n`: the safepoint budget the loop that moves them spends.
    fn bulk_spend(&mut self, _n: u32) -> Vec<I<'static>> {
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
    /// A data symbol whose address is relative to a global's value (a
    /// dynamically loaded mod's own data, placed when it loads): (global,
    /// offset). `None`: [`Resolver::data_addr`] is absolute.
    fn data_rel(&mut self, _symbol: u32) -> Result<Option<(u32, u32)>> {
        Ok(None)
    }
    /// The same for a function's address (a mod's own functions).
    fn func_rel(&mut self, _symbol: u32) -> Result<Option<(u32, u32)>> {
        Ok(None)
    }
    /// The wasm address of the guest window (psxw-abi `guest::WINDOW`).
    fn window_base(&self) -> u32 {
        WINDOW_BASE
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

fn masked(window: u32, align: u32) -> MemArg {
    MemArg { offset: window as u64, align, memory_index: 0 }
}

/// `kind` bit of an I/O hook call for stores.
pub const IO_STORE: u32 = 0x100;

/// Page granularity of the dirty map.
pub const DIRTY_PAGE_SHIFT: u32 = 12;

/// Rewrite defined function `func` (index into `obj.funcs`).
pub fn rewrite<R: Resolver>(obj: &WasmObject, func: usize, r: &mut R) -> Result<Function> {
    rewrite_mapped(obj, func, r, None)
}

/// [`rewrite`], also recording where each input instruction landed:
/// (offset in the input body, offset in the output body), both from the
/// body's start (its locals), ascending, ending with both bodies' lengths.
/// Debug information follows the code through it ([`crate::dwarf`]).
pub fn rewrite_mapped<R: Resolver>(
    obj: &WasmObject,
    func: usize,
    r: &mut R,
    mut offsets: Option<&mut Vec<(u32, u32)>>,
) -> Result<Function> {
    let dirty = r.dirty_map();
    let window = r.window_base();
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
    // Bulk memory: destination, index and byte (see `Bulk`).
    let bulk = Bulk { n: t_i32b, from: t_i32, dst: nlocals + 6, i: nlocals + 7, byte: nlocals + 8, addr: t_addr, window };
    locals.extend([(2, ValType::I32), (1, ValType::I64), (1, ValType::F32), (1, ValType::F64), (4, ValType::I32)]);
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
        if let Some(v) = offsets.as_deref_mut() {
            v.push((pos - f.body.start as u32, out.byte_len() as u32));
        }
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
        // A memarg's relocated offset: absolute, or relative to a global.
        let mem_reloc = |r: &mut R| -> Result<Option<(u64, Option<u32>)>> {
            match reloc_of(RelocationType::MemoryAddrLeb) {
                Some(rl) => match r.data_rel(rl.symbol)? {
                    Some((g, o)) => Ok(Some((o.wrapping_add(rl.addend as u32) as u64, Some(g)))),
                    None => Ok(Some((r.data_addr(rl.symbol)?.wrapping_add(rl.addend as u32) as u64, None))),
                },
                None => Ok(None),
            }
        };
        let add_off = |out: &mut Function, off: u64, rel: Option<u32>| {
            if off != 0 {
                out.instruction(&I::I32Const(off as u32 as i32));
                out.instruction(&I::I32Add);
            }
            if let Some(g) = rel {
                out.instruction(&I::GlobalGet(g));
                out.instruction(&I::I32Add);
            }
        };
        macro_rules! load {
            ($memarg:expr, $ins:ident, $size:expr) => {{
                let (off, rel) = mem_reloc(r)?.unwrap_or(($memarg.offset, None));
                add_off(&mut out, off, rel);
                out.instruction(&I::I32Const(WINDOW_MASK as i32));
                out.instruction(&I::I32And);
                if let Some(check) = io {
                    io_guard(&mut out, t_addr, check, $size);
                }
                out.instruction(&I::$ins(masked(window, $memarg.align as u32)));
            }};
        }
        macro_rules! store {
            ($memarg:expr, $ins:ident, $tmp:expr, $size:expr) => {{
                let (off, rel) = mem_reloc(r)?.unwrap_or(($memarg.offset, None));
                out.instruction(&I::LocalSet($tmp));
                add_off(&mut out, off, rel);
                out.instruction(&I::I32Const(WINDOW_MASK as i32));
                out.instruction(&I::I32And);
                if dirty.is_some() || io.is_some() {
                    out.instruction(&I::LocalTee(t_addr));
                }
                out.instruction(&I::LocalGet($tmp));
                out.instruction(&I::$ins(masked(window, $memarg.align as u32)));
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
                    Lowered::Copy => {
                        bulk.emit(&mut out, r, true, io, dirty)?;
                        out.instruction(&I::LocalGet(bulk.dst));
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
                let (v, rel) = if let Some(rl) = reloc_of(RelocationType::MemoryAddrSleb) {
                    match r.data_rel(rl.symbol)? {
                        Some((g, o)) => (o.wrapping_add(rl.addend as u32) as i32, Some(g)),
                        None => (r.data_addr(rl.symbol)?.wrapping_add(rl.addend as u32) as i32, None),
                    }
                } else if let Some(rl) = reloc_of(RelocationType::TableIndexSleb) {
                    match r.func_rel(rl.symbol)? {
                        Some((g, o)) => (o as i32, Some(g)),
                        None => (r.func_addr(rl.symbol)? as i32, None),
                    }
                } else {
                    (value, None)
                };
                out.instruction(&I::I32Const(v));
                if let Some(g) = rel {
                    out.instruction(&I::GlobalGet(g));
                    out.instruction(&I::I32Add);
                }
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
            Operator::MemoryCopy { .. } => bulk.emit(&mut out, r, true, io, dirty)?,
            Operator::MemoryFill { .. } => bulk.emit(&mut out, r, false, io, dirty)?,
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
    if let Some(v) = offsets {
        v.push(((f.body.end - f.body.start) as u32, out.byte_len() as u32));
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

/// Scratch locals for `memory.copy` and `memory.fill`: byte count, source
/// address (a fill's value), destination, index, the byte moved, and the
/// masked address of an access.
#[derive(Clone, Copy)]
struct Bulk {
    n: u32,
    from: u32,
    dst: u32,
    i: u32,
    byte: u32,
    addr: u32,
    window: u32,
}

impl Bulk {
    /// A bulk copy (`[dst, src, n]` on the stack) or fill (`[dst, value,
    /// n]`): masked into the window, the destination's pages marked dirty,
    /// and the budget spent ([`Resolver::bulk_spend`]). With an I/O hook,
    /// one whose bytes reach the I/O window moves them one at a time
    /// through it, as the loads and stores it stands for would.
    fn emit<R: Resolver>(self, out: &mut Function, r: &mut R, copy: bool, io: Option<u32>, dirty: Option<u32>) -> Result<()> {
        out.instruction(&I::LocalSet(self.n));
        out.instruction(&I::LocalSet(self.from));
        out.instruction(&I::LocalSet(self.dst));
        if let Some(check) = io {
            self.touches_io(out, self.dst);
            if copy {
                self.touches_io(out, self.from);
                out.instruction(&I::I32Or);
            }
            out.instruction(&I::If(wasm_encoder::BlockType::Empty));
            self.bytes(out, copy, check, dirty);
            out.instruction(&I::Else);
        }
        out.instruction(&I::LocalGet(self.dst));
        mask_plus_base(out, self.window);
        if dirty.is_some() {
            mark(out, self.addr, self.n, r.mark_range());
        }
        out.instruction(&I::LocalGet(self.from));
        if copy {
            mask_plus_base(out, self.window);
            out.instruction(&I::LocalGet(self.n));
            out.instruction(&I::MemoryCopy { src_mem: 0, dst_mem: 0 });
        } else {
            out.instruction(&I::LocalGet(self.n));
            out.instruction(&I::MemoryFill(0));
        }
        if io.is_some() {
            out.instruction(&I::End);
        }
        for i in r.bulk_spend(self.n) {
            out.instruction(&i);
        }
        Ok(())
    }

    /// Whether `[addr, addr + n)`, masked, reaches the I/O window.
    fn touches_io(self, out: &mut Function, addr: u32) {
        let io = psxw_abi::phys::IO;
        out.instruction(&I::LocalGet(addr));
        out.instruction(&I::I32Const(WINDOW_MASK as i32));
        out.instruction(&I::I32And);
        out.instruction(&I::LocalTee(self.addr));
        out.instruction(&I::I32Const(io.end as i32));
        out.instruction(&I::I32LtU);
        out.instruction(&I::LocalGet(self.addr));
        out.instruction(&I::LocalGet(self.n));
        out.instruction(&I::I32Add);
        out.instruction(&I::I32Const(io.start as i32));
        out.instruction(&I::I32GtU);
        out.instruction(&I::I32And);
    }

    /// Byte by byte, each load and store through the I/O hook.
    fn bytes(self, out: &mut Function, copy: bool, check: u32, dirty: Option<u32>) {
        let at = |out: &mut Function, base: u32| {
            out.instruction(&I::LocalGet(base));
            out.instruction(&I::LocalGet(self.i));
            out.instruction(&I::I32Add);
            out.instruction(&I::I32Const(WINDOW_MASK as i32));
            out.instruction(&I::I32And);
        };
        out.instruction(&I::I32Const(0));
        out.instruction(&I::LocalSet(self.i));
        out.instruction(&I::Block(wasm_encoder::BlockType::Empty));
        out.instruction(&I::Loop(wasm_encoder::BlockType::Empty));
        out.instruction(&I::LocalGet(self.i));
        out.instruction(&I::LocalGet(self.n));
        out.instruction(&I::I32GeU);
        out.instruction(&I::BrIf(1));
        if copy {
            at(out, self.from);
            io_guard(out, self.addr, check, 1);
            out.instruction(&I::I32Load8U(masked(self.window, 0)));
        } else {
            out.instruction(&I::LocalGet(self.from));
        }
        out.instruction(&I::LocalSet(self.byte));
        at(out, self.dst);
        out.instruction(&I::LocalTee(self.addr));
        out.instruction(&I::LocalGet(self.byte));
        out.instruction(&I::I32Store8(masked(self.window, 0)));
        io_after_store(out, self.addr, check, 1);
        if let Some(map) = dirty {
            out.instruction(&I::LocalGet(self.addr));
            out.instruction(&I::I32Const(DIRTY_PAGE_SHIFT as i32));
            out.instruction(&I::I32ShrU);
            out.instruction(&I::I32Const(1));
            out.instruction(&I::I32Store8(MemArg { offset: map as u64, align: 0, memory_index: 0 }));
        }
        out.instruction(&I::LocalGet(self.i));
        out.instruction(&I::I32Const(1));
        out.instruction(&I::I32Add);
        out.instruction(&I::LocalSet(self.i));
        out.instruction(&I::Br(0));
        out.instruction(&I::End);
        out.instruction(&I::End);
    }
}

/// Mark `[dst, dst + n)` dirty, keeping `dst` (a wasm address) on the stack.
fn mark(out: &mut Function, t_addr: u32, n: u32, helper: u32) {
    out.instruction(&I::LocalTee(t_addr));
    out.instruction(&I::LocalGet(n));
    out.instruction(&I::Call(helper));
    out.instruction(&I::LocalGet(t_addr));
}

/// `(addr & WINDOW_MASK) + window`, for bulk-memory operands.
fn mask_plus_base(out: &mut Function, window: u32) {
    out.instruction(&I::I32Const(WINDOW_MASK as i32));
    out.instruction(&I::I32And);
    out.instruction(&I::I32Const(window as i32));
    out.instruction(&I::I32Add);
}
