//! o32 marshaling between a C function's wasm signature and the uniform
//! o32 form `(a0, a1, a2, a3) -> v0 | v1 << 32` (docs/design.md §2, §4).
//!
//! Arguments become o32 words: 64-bit values (and doubles, soft-float) take
//! an even-aligned pair, structs passed by value are copied word by word,
//! words past the fourth live on the guest stack at `sp + 4k`. Variadic
//! calls copy a fixed number of words out of clang's va-buffer; the callee
//! reads only what its format asks for.

use crate::object::FuncType;
use psxw_abi::{WINDOW_BASE, WINDOW_MASK};
use psxw_cc::protos::{Kind, Proto};
use wasm_encoder::{Function, Instruction as I, MemArg, ValType};

/// Words copied from a va-buffer after the fixed arguments.
pub const VARIADIC_WORDS: u32 = 8;

/// Where one o32 argument word comes from (C→o32) or goes (o32→C).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Word {
    /// i32 parameter `n`.
    Param(u32),
    /// Low / high half of i64 parameter `n`.
    Lo(u32),
    Hi(u32),
    /// Bits of f32 parameter `n`.
    F32(u32),
    /// Low / high half of the bits of f64 parameter `n`.
    F64Lo(u32),
    F64Hi(u32),
    /// Word `off` bytes into the guest memory parameter `n` points at
    /// (a struct copy or a va-buffer).
    Mem(u32, u32),
    /// Alignment padding.
    Pad,
}

/// Kinds of a signature's parameters: the prototype's when it agrees with
/// the wasm signature, else derived from the wasm value types.
pub fn param_kinds(sig: &FuncType, proto: Option<&Proto>) -> (Vec<Kind>, bool) {
    if let Some(p) = proto {
        let n = p.params.len() + usize::from(p.variadic);
        if n == sig.params.len() {
            return (p.params.clone(), p.variadic);
        }
    }
    let kinds = sig
        .params
        .iter()
        .map(|t| match t {
            wasmparser::ValType::I64 => Kind::I64,
            wasmparser::ValType::F32 => Kind::F32,
            wasmparser::ValType::F64 => Kind::F64,
            _ => Kind::I32,
        })
        .collect();
    (kinds, false)
}

/// o32 words for parameters of the given kinds. Returns the words and how
/// many come from fixed (non-variadic) parameters.
pub fn words(kinds: &[Kind], variadic: bool) -> (Vec<Word>, u32) {
    let mut w = Vec::new();
    let even = |w: &mut Vec<Word>| {
        if w.len() % 2 == 1 {
            w.push(Word::Pad);
        }
    };
    for (i, k) in kinds.iter().enumerate() {
        let i = i as u32;
        match *k {
            Kind::I64 => {
                even(&mut w);
                w.push(Word::Lo(i));
                w.push(Word::Hi(i));
            }
            Kind::F64 => {
                even(&mut w);
                w.push(Word::F64Lo(i));
                w.push(Word::F64Hi(i));
            }
            Kind::F32 => w.push(Word::F32(i)),
            Kind::ByVal { size, align } => {
                if align >= 8 {
                    even(&mut w);
                }
                for k in 0..size.div_ceil(4) {
                    w.push(Word::Mem(i, 4 * k));
                }
            }
            Kind::Void => {}
            _ => w.push(Word::Param(i)),
        }
    }
    let fixed = w.len() as u32;
    if variadic {
        let va = kinds.len() as u32;
        for k in 0..VARIADIC_WORDS {
            w.push(Word::Mem(va, 4 * k));
        }
    }
    (w, fixed)
}

fn memarg(align: u32) -> MemArg {
    MemArg { offset: WINDOW_BASE as u64, align, memory_index: 0 }
}

/// Mask the guest address on top of the stack into the window.
pub fn mask(f: &mut Function) {
    f.instruction(&I::I32Const(WINDOW_MASK as i32));
    f.instruction(&I::I32And);
}

/// Push guest word at `local + off`.
pub fn load_word(f: &mut Function, local: u32, off: u32) {
    f.instruction(&I::LocalGet(local));
    if off != 0 {
        f.instruction(&I::I32Const(off as i32));
        f.instruction(&I::I32Add);
    }
    mask(f);
    f.instruction(&I::I32Load(memarg(0)));
}

/// Push word `w` from the C side's parameters.
pub fn push_word(f: &mut Function, w: Word) {
    match w {
        Word::Param(n) => {
            f.instruction(&I::LocalGet(n));
        }
        Word::Lo(n) => {
            f.instruction(&I::LocalGet(n));
            f.instruction(&I::I32WrapI64);
        }
        Word::Hi(n) => {
            f.instruction(&I::LocalGet(n));
            f.instruction(&I::I64Const(32));
            f.instruction(&I::I64ShrU);
            f.instruction(&I::I32WrapI64);
        }
        Word::F32(n) => {
            f.instruction(&I::LocalGet(n));
            f.instruction(&I::I32ReinterpretF32);
        }
        Word::F64Lo(n) => {
            f.instruction(&I::LocalGet(n));
            f.instruction(&I::I64ReinterpretF64);
            f.instruction(&I::I32WrapI64);
        }
        Word::F64Hi(n) => {
            f.instruction(&I::LocalGet(n));
            f.instruction(&I::I64ReinterpretF64);
            f.instruction(&I::I64Const(32));
            f.instruction(&I::I64ShrU);
            f.instruction(&I::I32WrapI64);
        }
        Word::Mem(n, off) => load_word(f, n, off),
        Word::Pad => {
            f.instruction(&I::I32Const(0));
        }
    }
}

/// Convert an o32 result (i64 on the stack) to a C result type.
pub fn from_o32_result(f: &mut Function, results: &[wasmparser::ValType]) {
    match results.first() {
        None => {
            f.instruction(&I::Drop);
        }
        Some(wasmparser::ValType::I32) => {
            f.instruction(&I::I32WrapI64);
        }
        Some(wasmparser::ValType::I64) => {}
        Some(wasmparser::ValType::F32) => {
            f.instruction(&I::I32WrapI64);
            f.instruction(&I::F32ReinterpretI32);
        }
        Some(wasmparser::ValType::F64) => {
            f.instruction(&I::F64ReinterpretI64);
        }
        Some(_) => {
            f.instruction(&I::Unreachable);
        }
    }
}

/// Convert a C result (on the stack) to an o32 i64.
pub fn to_o32_result(f: &mut Function, results: &[wasmparser::ValType]) {
    match results.first() {
        None => {
            f.instruction(&I::I64Const(0));
        }
        Some(wasmparser::ValType::I32) => {
            f.instruction(&I::I64ExtendI32U);
        }
        Some(wasmparser::ValType::I64) => {}
        Some(wasmparser::ValType::F32) => {
            f.instruction(&I::I32ReinterpretF32);
            f.instruction(&I::I64ExtendI32U);
        }
        Some(wasmparser::ValType::F64) => {
            f.instruction(&I::I64ReinterpretF64);
        }
        Some(_) => {
            f.instruction(&I::Unreachable);
        }
    }
}

pub fn val(t: wasmparser::ValType) -> ValType {
    match t {
        wasmparser::ValType::I32 => ValType::I32,
        wasmparser::ValType::I64 => ValType::I64,
        wasmparser::ValType::F32 => ValType::F32,
        wasmparser::ValType::F64 => ValType::F64,
        _ => ValType::I32,
    }
}

/// Body of a C→o32 adapter with signature `sig`: marshal into o32 words,
/// reserve the outgoing argument area on the guest stack, store words past
/// the fourth there, call `call` with (a0..a3) (plus whatever `call` pushes
/// first), restore the stack, convert the result.
///
/// `before_args` pushes leading operands of the callee (e.g. call-out kind
/// and target); `after_args` pushes trailing ones (e.g. the new sp).
pub fn c_to_o32(
    sig: &FuncType,
    words: &[Word],
    sp_global: u32,
    before_args: &dyn Fn(&mut Function),
    after_args: &dyn Fn(&mut Function, u32),
    call: &dyn Fn(&mut Function),
) -> Function {
    let n = sig.params.len() as u32;
    let old_sp = n;
    let new_sp = n + 1;
    let mut f = Function::new([(2, ValType::I32)]);
    let frame = ((words.len().max(4) as u32) * 4).div_ceil(8) * 8;
    f.instruction(&I::GlobalGet(sp_global));
    f.instruction(&I::LocalTee(old_sp));
    f.instruction(&I::I32Const(frame as i32));
    f.instruction(&I::I32Sub);
    f.instruction(&I::LocalSet(new_sp));
    for (k, &w) in words.iter().enumerate().skip(4) {
        f.instruction(&I::LocalGet(new_sp));
        f.instruction(&I::I32Const(4 * k as i32));
        f.instruction(&I::I32Add);
        mask(&mut f);
        push_word(&mut f, w);
        f.instruction(&I::I32Store(memarg(2)));
    }
    f.instruction(&I::LocalGet(new_sp));
    f.instruction(&I::GlobalSet(sp_global));
    before_args(&mut f);
    for k in 0..4 {
        push_word(&mut f, words.get(k).copied().unwrap_or(Word::Pad));
    }
    after_args(&mut f, new_sp);
    call(&mut f);
    f.instruction(&I::LocalGet(old_sp));
    f.instruction(&I::GlobalSet(sp_global));
    from_o32_result(&mut f, &sig.results);
    f.instruction(&I::End);
    f
}

/// Body of an o32→C wrapper `(a0..a3) -> i64` calling a C function with
/// signature `sig` (function index `callee`). Structs by value and
/// va-buffers are rebuilt on the guest stack below the caller's frame.
pub fn o32_to_c(sig: &FuncType, proto: Option<&Proto>, callee: u32, sp_global: u32) -> Function {
    let (kinds, variadic) = param_kinds(sig, proto);
    let (ws, _) = words(&kinds, variadic);
    // Locals: 0..3 = a0..a3, 4 = entry sp, 5 = scratch base.
    let entry_sp = 4;
    let scratch = 5;
    let mut f = Function::new([(2, ValType::I32)]);
    // word(k): a0..a3, else the caller's stack at entry_sp + 4k.
    let word = |f: &mut Function, k: usize| {
        if k < 4 {
            f.instruction(&I::LocalGet(k as u32));
        } else {
            load_word(f, entry_sp, 4 * k as u32);
        }
    };
    // Memory-passed parameters (struct copies, va-buffer) get scratch space.
    let mut mem_params: Vec<(u32, Vec<usize>)> = Vec::new();
    for (k, w) in ws.iter().enumerate() {
        if let Word::Mem(n, _) = w {
            match mem_params.iter_mut().find(|(m, _)| m == n) {
                Some((_, ks)) => ks.push(k),
                None => mem_params.push((*n, vec![k])),
            }
        }
    }
    let scratch_bytes: u32 = mem_params.iter().map(|(_, ks)| ks.len() as u32 * 4).sum();
    let scratch_bytes = scratch_bytes.div_ceil(8) * 8;
    f.instruction(&I::GlobalGet(sp_global));
    f.instruction(&I::LocalTee(entry_sp));
    f.instruction(&I::I32Const(scratch_bytes as i32));
    f.instruction(&I::I32Sub);
    f.instruction(&I::LocalTee(scratch));
    f.instruction(&I::GlobalSet(sp_global));
    let mut offsets: Vec<(u32, u32)> = Vec::new();
    let mut at = 0u32;
    for (n, ks) in &mem_params {
        offsets.push((*n, at));
        for (j, &k) in ks.iter().enumerate() {
            f.instruction(&I::LocalGet(scratch));
            f.instruction(&I::I32Const((at + 4 * j as u32) as i32));
            f.instruction(&I::I32Add);
            mask(&mut f);
            word(&mut f, k);
            f.instruction(&I::I32Store(memarg(2)));
        }
        at += 4 * ks.len() as u32;
    }
    let pos = |pred: &dyn Fn(&Word) -> bool| ws.iter().position(pred);
    for (i, t) in sig.params.iter().enumerate() {
        let i = i as u32;
        if let Some((_, off)) = offsets.iter().find(|(n, _)| *n == i) {
            f.instruction(&I::LocalGet(scratch));
            f.instruction(&I::I32Const(*off as i32));
            f.instruction(&I::I32Add);
            continue;
        }
        match t {
            wasmparser::ValType::I64 => {
                let lo = pos(&|w| *w == Word::Lo(i)).unwrap_or(0);
                word(&mut f, lo);
                f.instruction(&I::I64ExtendI32U);
                word(&mut f, lo + 1);
                f.instruction(&I::I64ExtendI32U);
                f.instruction(&I::I64Const(32));
                f.instruction(&I::I64Shl);
                f.instruction(&I::I64Or);
            }
            wasmparser::ValType::F64 => {
                let lo = pos(&|w| *w == Word::F64Lo(i)).unwrap_or(0);
                word(&mut f, lo);
                f.instruction(&I::I64ExtendI32U);
                word(&mut f, lo + 1);
                f.instruction(&I::I64ExtendI32U);
                f.instruction(&I::I64Const(32));
                f.instruction(&I::I64Shl);
                f.instruction(&I::I64Or);
                f.instruction(&I::F64ReinterpretI64);
            }
            wasmparser::ValType::F32 => {
                word(&mut f, pos(&|w| *w == Word::F32(i)).unwrap_or(0));
                f.instruction(&I::F32ReinterpretI32);
            }
            _ => word(&mut f, pos(&|w| *w == Word::Param(i)).unwrap_or(0)),
        }
    }
    f.instruction(&I::Call(callee));
    let sret = matches!(kinds.first(), Some(Kind::Sret { .. }));
    if sret {
        // o32 returns the struct's address in v0.
        to_o32_result(&mut f, &sig.results);
        f.instruction(&I::Drop);
        f.instruction(&I::LocalGet(0));
        f.instruction(&I::I64ExtendI32U);
    } else {
        to_o32_result(&mut f, &sig.results);
    }
    f.instruction(&I::LocalGet(entry_sp));
    f.instruction(&I::GlobalSet(sp_global));
    f.instruction(&I::End);
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_layout() {
        let (w, fixed) = words(&[Kind::I32, Kind::I64, Kind::ByVal { size: 6, align: 2 }], false);
        assert_eq!(w, vec![Word::Param(0), Word::Pad, Word::Lo(1), Word::Hi(1), Word::Mem(2, 0), Word::Mem(2, 4)]);
        assert_eq!(fixed, 6);
        let (w, fixed) = words(&[Kind::Ptr], true);
        assert_eq!(fixed, 1);
        assert_eq!(w.len() as u32, 1 + VARIADIC_WORDS);
        assert_eq!(w[1], Word::Mem(1, 0));
    }
}
