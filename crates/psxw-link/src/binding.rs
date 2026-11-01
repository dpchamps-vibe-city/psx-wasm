//! Explicit binding (plans/dispatch-binding A). Covered guest addresses are
//! changing slots: their dispatch-table entry is the binding, and every
//! route into one (direct, tail, pointer, host) looks it up. Boot binds the
//! boot image's entries; generated operation wrappers publish and
//! invalidate ranges as declared software operations complete. Nothing
//! reads code words to choose an implementation.
//!
//! A per-slot global holds which candidate is bound (1-based; 0 none), so
//! an invalidation unbinds only a slot whose bound candidate it overlaps.

use crate::marshal::{self, Word};
use psxw_abi::{WINDOW_MASK, binding_cause, table_index};
use std::collections::BTreeMap;
use std::ops::Range;
use wasm_encoder::{BlockType, Function, HeapType, Instruction as I, MemArg, ValType};

/// The binding plan the linker generates (built in psxw-build).
#[derive(Debug, Clone, Default)]
pub struct Binding {
    /// Covered addresses, each with every implementation compiled there.
    pub slots: BTreeMap<u32, Vec<Candidate>>,
    pub placements: Vec<Placement>,
    pub rules: Vec<Rule>,
    /// Instrumented operations: (container, address) → indices of `rules`.
    pub operations: BTreeMap<(String, u32), Vec<usize>>,
    /// `c_marker` sites authored C calls (`psxw_binding_site_<marker>`):
    /// marker → its rule, whose values are the call's arguments
    /// (destination, length, source offset, selector).
    pub markers: BTreeMap<String, usize>,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub container: String,
    /// Window offsets (`addr & WINDOW_MASK`) the implementation depends
    /// on; it is bound only when one publication covers them all.
    pub footprint: Vec<Range<u32>>,
    /// The boot image supplies it.
    pub boot: bool,
}

#[derive(Debug, Clone)]
pub struct Placement {
    pub id: String,
    pub container: String,
    /// Per segment: its range in the container's source image, and the
    /// guest address it loads at.
    pub segments: Vec<(Range<u32>, u32)>,
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub id: String,
    pub event: Event,
    pub effect: Effect,
    pub destination: Value,
    pub length: Value,
    pub success: Success,
    /// At a `c_marker` call: argument words the rule declares literally
    /// (`const`, or a `global` read at the call), which the C's values must
    /// equal.
    pub checks: Vec<(u32, Value)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Enter,
    Return,
    /// At a `c_marker` call, right after the write it describes.
    AfterWrite,
}

#[derive(Debug, Clone)]
pub enum Effect {
    Invalidate,
    Publish { target: Target, source_offset: Offset },
}

#[derive(Debug, Clone)]
pub enum Target {
    Placement(usize),
    /// Value-to-placement cases; any other value is rejected.
    Selector { value: Value, cases: Vec<(u32, usize)> },
}

#[derive(Debug, Clone)]
pub enum Offset {
    Value(Value),
    /// Derived from the destination by the placement's segments.
    Derive,
}

/// A scalar the wrapper reads at the rule's event.
#[derive(Debug, Clone)]
pub enum Value {
    Const(u32),
    /// o32 argument word, captured at entry.
    Arg(u32),
    /// The first o32 return word.
    Return,
    /// The u32 at a guest address.
    Load(u32),
    /// The u32 `offset` bytes through argument `arg`.
    Field { arg: u32, offset: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Success {
    Always,
    Zero,
    Nonzero,
}

impl Binding {
    /// The candidate bound at boot, 1-based.
    pub fn boot(&self, addr: u32) -> Option<usize> {
        self.slots.get(&addr)?.iter().position(|c| c.boot).map(|k| k + 1)
    }
}

/// Function and global indices the bodies refer to.
pub(crate) struct Refs<'a> {
    pub binding: &'a Binding,
    /// Per slot, its candidate global.
    pub globals: &'a BTreeMap<u32, u32>,
    pub untranslated: u32,
    pub fault: u32,
    pub o32_ty: u32,
    pub window: u32,
}

const WINDOW: i32 = (WINDOW_MASK as i32).wrapping_add(1);

fn memarg(window: u32) -> MemArg {
    MemArg { offset: window as u64, align: 2, memory_index: 0 }
}

/// `(a0..a3) -> i64` through the binding at `addr`; an empty one is
/// untranslated there.
pub(crate) fn lookup(r: &Refs, addr: u32) -> Function {
    let mut f = Function::new([]);
    let slot = table_index(addr) as i32;
    f.instruction(&I::I32Const(slot));
    f.instruction(&I::TableGet(0));
    f.instruction(&I::RefIsNull);
    f.instruction(&I::If(BlockType::Empty));
    f.instruction(&I::I32Const(addr as i32));
    f.instruction(&I::Call(r.untranslated));
    f.instruction(&I::Return);
    f.instruction(&I::End);
    for l in 0..4 {
        f.instruction(&I::LocalGet(l));
    }
    f.instruction(&I::I32Const(slot));
    f.instruction(&I::CallIndirect { type_index: r.o32_ty, table_index: 0 });
    f.instruction(&I::End);
    f
}

/// Locals 0, 1: the window offset and length; local 2 becomes the end
/// (the length clamped to the window).
fn end_local(f: &mut Function) {
    f.instruction(&I::LocalGet(1));
    f.instruction(&I::I32Const(WINDOW));
    f.instruction(&I::LocalGet(1));
    f.instruction(&I::I32Const(WINDOW));
    f.instruction(&I::I32LtU);
    f.instruction(&I::Select);
    f.instruction(&I::LocalGet(0));
    f.instruction(&I::I32Add);
    f.instruction(&I::LocalSet(2));
}

/// `(offset, len) -> ()`: unbind every slot whose bound candidate depends
/// on a byte in the range. An empty range changes nothing.
pub(crate) fn invalidate(r: &Refs) -> Function {
    let mut f = Function::new([(1, ValType::I32)]);
    f.instruction(&I::LocalGet(1));
    f.instruction(&I::I32Eqz);
    f.instruction(&I::If(BlockType::Empty));
    f.instruction(&I::Return);
    f.instruction(&I::End);
    end_local(&mut f);
    for (addr, cands) in &r.binding.slots {
        let g = r.globals[addr];
        for (k, c) in cands.iter().enumerate() {
            f.instruction(&I::GlobalGet(g));
            f.instruction(&I::I32Const(k as i32 + 1));
            f.instruction(&I::I32Eq);
            f.instruction(&I::I32Const(0));
            for range in &c.footprint {
                // range.start < end && offset < range.end
                f.instruction(&I::I32Const(range.start as i32));
                f.instruction(&I::LocalGet(2));
                f.instruction(&I::I32LtU);
                f.instruction(&I::LocalGet(0));
                f.instruction(&I::I32Const(range.end as i32));
                f.instruction(&I::I32LtU);
                f.instruction(&I::I32And);
                f.instruction(&I::I32Or);
            }
            f.instruction(&I::I32And);
            f.instruction(&I::If(BlockType::Empty));
            f.instruction(&I::I32Const(0));
            f.instruction(&I::GlobalSet(g));
            f.instruction(&I::I32Const(table_index(*addr) as i32));
            f.instruction(&I::RefNull(HeapType::FUNC));
            f.instruction(&I::TableSet(0));
            f.instruction(&I::End);
        }
    }
    f.instruction(&I::End);
    f
}

/// `(offset, len) -> ()`: invalidate the range, then bind each of
/// placement `p`'s candidates whose whole footprint it covers. `entry`
/// gives a candidate's o32 entry. One update: no safepoint or callback
/// between its table writes.
pub(crate) fn publish(r: &Refs, p: usize, invalidate: u32, entry: &mut dyn FnMut(&str, u32) -> u32) -> Function {
    let container = &r.binding.placements[p].container;
    let mut f = Function::new([(1, ValType::I32)]);
    f.instruction(&I::LocalGet(0));
    f.instruction(&I::LocalGet(1));
    f.instruction(&I::Call(invalidate));
    end_local(&mut f);
    for (addr, cands) in &r.binding.slots {
        for (k, c) in cands.iter().enumerate().filter(|(_, c)| c.container == *container) {
            f.instruction(&I::I32Const(1));
            for range in &c.footprint {
                // offset <= range.start && range.end <= end
                f.instruction(&I::LocalGet(0));
                f.instruction(&I::I32Const(range.start as i32));
                f.instruction(&I::I32LeU);
                f.instruction(&I::I32Const(range.end as i32));
                f.instruction(&I::LocalGet(2));
                f.instruction(&I::I32LeU);
                f.instruction(&I::I32And);
                f.instruction(&I::I32And);
            }
            f.instruction(&I::If(BlockType::Empty));
            f.instruction(&I::I32Const(k as i32 + 1));
            f.instruction(&I::GlobalSet(r.globals[addr]));
            f.instruction(&I::I32Const(table_index(*addr) as i32));
            f.instruction(&I::RefFunc(entry(&c.container, *addr)));
            f.instruction(&I::TableSet(0));
            f.instruction(&I::End);
        }
    }
    f.instruction(&I::End);
    f
}

/// How an instrumented operation is called.
pub(crate) enum Shape {
    /// `(a0..a3) -> i64`.
    O32,
    /// A C function: its parameters, where each o32 argument word comes
    /// from, and its result.
    C { params: u32, words: Vec<Word>, result: Option<wasmparser::ValType> },
}

/// The wrapper's locals.
struct Locals {
    params: u32,
    res: Option<u32>,
    sp0: u32,
    dst: u32,
    len: u32,
    so: u32,
    sel: u32,
    ok: u32,
    rel: u32,
    /// Argument words captured at entry, before the body can change them:
    /// o32 stack words, and a C function's (a struct copy is memory).
    captured: BTreeMap<u32, u32>,
}

/// An instrumented operation: `inner` with the same signature, the rules'
/// effects at entry and on return. Arguments are read as o32 words; the
/// call itself is unchanged.
#[allow(clippy::too_many_arguments)]
pub(crate) fn operation(
    r: &Refs,
    name: &str,
    shape: &Shape,
    sig_results: &[ValType],
    inner: u32,
    rules: &[usize],
    sp_global: u32,
    publish: &dyn Fn(usize) -> u32,
    invalidate: u32,
) -> anyhow::Result<Function> {
    let rules: Vec<(usize, &Rule)> = rules.iter().map(|&i| (i, &r.binding.rules[i])).collect();
    let mut used: Vec<&Value> = Vec::new();
    for (_, rule) in &rules {
        used.extend([&rule.destination, &rule.length]);
        if let Effect::Publish { target, source_offset } = &rule.effect {
            if let Target::Selector { value, .. } = target {
                used.push(value);
            }
            if let Offset::Value(v) = source_offset {
                used.push(v);
            }
        }
    }
    let params = match shape {
        Shape::O32 => 4,
        Shape::C { params, .. } => *params,
    };
    let result_ty = sig_results.first().copied();
    let mut next = params;
    let res = result_ty.map(|_| {
        next += 1;
        next - 1
    });
    let first = next;
    let mut l = Locals { params, res, sp0: first, dst: first + 1, len: first + 2, so: first + 3, sel: first + 4, ok: first + 5, rel: first + 6, captured: BTreeMap::new() };
    next = first + 7;
    for v in &used {
        let arg = match v {
            Value::Arg(a) | Value::Field { arg: a, .. } => *a,
            _ => continue,
        };
        if let Shape::C { words, .. } = shape {
            anyhow::ensure!(
                (arg as usize) < words.len(),
                "binding.invalid_fact: {name} has {} argument words; a rule reads arg:{arg}",
                words.len()
            );
        }
        let capture = matches!(shape, Shape::C { .. }) || arg >= 4;
        if capture && !l.captured.contains_key(&arg) {
            l.captured.insert(arg, next);
            next += 1;
        }
        if matches!(v, Value::Return) {
            anyhow::ensure!(result_ty.is_some(), "binding.invalid_fact: {name} returns nothing; a rule reads `return`");
        }
    }
    for (_, rule) in &rules {
        anyhow::ensure!(
            rule.success == Success::Always || result_ty.is_some(),
            "binding.invalid_fact: rule `{}` tests the result of {name}, which returns nothing",
            rule.id
        );
    }
    let mut groups = Vec::new();
    if let Some(t) = result_ty {
        groups.push((1, t));
    }
    groups.push((next - first, ValType::I32));
    let mut f = Function::new(groups);
    // Capture argument words before the call can change them.
    f.instruction(&I::GlobalGet(sp_global));
    f.instruction(&I::LocalSet(l.sp0));
    for (&arg, &local) in &l.captured {
        match shape {
            Shape::O32 => marshal::load_word(&mut f, r.window, l.sp0, 16 + 4 * (arg - 4)),
            Shape::C { words, .. } => c_word(&mut f, r.window, words, arg),
        }
        f.instruction(&I::LocalSet(local));
    }
    for &(i, rule) in rules.iter().filter(|(_, r)| r.event == Event::Enter) {
        apply(&mut f, r, shape, &l, i, rule, publish, invalidate);
    }
    for p in 0..l.params {
        f.instruction(&I::LocalGet(p));
    }
    f.instruction(&I::Call(inner));
    if let Some(res) = l.res {
        f.instruction(&I::LocalSet(res));
    }
    for &(i, rule) in rules.iter().filter(|(_, r)| r.event == Event::Return) {
        apply(&mut f, r, shape, &l, i, rule, publish, invalidate);
    }
    if let Some(res) = l.res {
        f.instruction(&I::LocalGet(res));
    }
    f.instruction(&I::End);
    Ok(f)
}

/// `psxw_binding_site_<marker>(destination, length, source_offset,
/// selector, request_key)`: the rule's effect with the authored C's
/// normalized values, checked against the placement's layout and the
/// rule's cases like any other publication.
pub(crate) fn marker(r: &Refs, rule: usize, publish: &dyn Fn(usize) -> u32, invalidate: u32) -> Function {
    let shape = Shape::C { params: 5, words: (0..5).map(Word::Param).collect(), result: None };
    let first = 5;
    let l = Locals {
        params: 5,
        res: None,
        sp0: first,
        dst: first + 1,
        len: first + 2,
        so: first + 3,
        sel: first + 4,
        ok: first + 5,
        rel: first + 6,
        captured: BTreeMap::new(),
    };
    let mut f = Function::new([(7, ValType::I32)]);
    let rule_ = &r.binding.rules[rule];
    // What the rule declares literally, the C must pass.
    for (word, expected) in &rule_.checks {
        f.instruction(&I::LocalGet(*word));
        value(&mut f, r.window, &shape, &l, expected);
        f.instruction(&I::I32Ne);
        f.instruction(&I::If(BlockType::Empty));
        fault(&mut f, r, rule, binding_cause::MARKER_MISMATCH, *word);
        f.instruction(&I::End);
    }
    apply(&mut f, r, &shape, &l, rule, rule_, publish, invalidate);
    f.instruction(&I::End);
    f
}

/// Push the first return word.
fn return_word(f: &mut Function, shape: &Shape, l: &Locals) {
    let res = l.res.expect("validated: a result");
    f.instruction(&I::LocalGet(res));
    match shape {
        Shape::O32 => {
            f.instruction(&I::I32WrapI64);
        }
        Shape::C { result, .. } => match result {
            Some(wasmparser::ValType::I64) => {
                f.instruction(&I::I32WrapI64);
            }
            Some(wasmparser::ValType::F32) => {
                f.instruction(&I::I32ReinterpretF32);
            }
            Some(wasmparser::ValType::F64) => {
                f.instruction(&I::I64ReinterpretF64);
                f.instruction(&I::I32WrapI64);
            }
            _ => {}
        },
    }
}

/// Push o32 argument word `k`: as captured at entry, else from the
/// parameters (o32 `a0..a3`, or a `c_marker`'s values).
fn arg_word(f: &mut Function, window: u32, shape: &Shape, l: &Locals, k: u32) {
    if let Some(&local) = l.captured.get(&k) {
        f.instruction(&I::LocalGet(local));
        return;
    }
    match shape {
        Shape::O32 => {
            f.instruction(&I::LocalGet(k));
        }
        Shape::C { words, .. } => c_word(f, window, words, k),
    }
}

/// Push a C function's o32 argument word `k` from its parameters.
fn c_word(f: &mut Function, window: u32, words: &[Word], k: u32) {
    {
        match words[k as usize] {
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
            Word::F64Lo(n) | Word::F64Hi(n) => {
                f.instruction(&I::LocalGet(n));
                f.instruction(&I::I64ReinterpretF64);
                if matches!(words[k as usize], Word::F64Hi(_)) {
                    f.instruction(&I::I64Const(32));
                    f.instruction(&I::I64ShrU);
                }
                f.instruction(&I::I32WrapI64);
            }
            Word::Mem(n, off) => marshal::load_word(f, window, n, off),
            Word::Pad => {
                f.instruction(&I::I32Const(0));
            }
        }
    }
}

fn value(f: &mut Function, window: u32, shape: &Shape, l: &Locals, v: &Value) {
    match v {
        Value::Const(c) => {
            f.instruction(&I::I32Const(*c as i32));
        }
        Value::Arg(k) => arg_word(f, window, shape, l, *k),
        Value::Return => return_word(f, shape, l),
        Value::Load(addr) => {
            f.instruction(&I::I32Const((addr & WINDOW_MASK) as i32));
            f.instruction(&I::I32Load(memarg(window)));
        }
        Value::Field { arg, offset } => {
            arg_word(f, window, shape, l, *arg);
            f.instruction(&I::I32Const(*offset as i32));
            f.instruction(&I::I32Add);
            f.instruction(&I::I32Const(WINDOW_MASK as i32));
            f.instruction(&I::I32And);
            f.instruction(&I::I32Load(memarg(window)));
        }
    }
}

fn fault(f: &mut Function, r: &Refs, rule: usize, cause: u32, value_local: u32) {
    f.instruction(&I::I32Const(rule as i32));
    f.instruction(&I::I32Const(cause as i32));
    f.instruction(&I::LocalGet(value_local));
    f.instruction(&I::Call(r.fault));
    f.instruction(&I::Unreachable);
}

/// One rule's effect, if its operation succeeded.
#[allow(clippy::too_many_arguments)]
fn apply(f: &mut Function, r: &Refs, shape: &Shape, l: &Locals, index: usize, rule: &Rule, publish: &dyn Fn(usize) -> u32, invalidate: u32) {
    let w = r.window;
    match rule.success {
        Success::Always => {
            f.instruction(&I::I32Const(1));
        }
        Success::Zero | Success::Nonzero => {
            return_word(f, shape, l);
            f.instruction(&I::I32Eqz);
            if rule.success == Success::Nonzero {
                f.instruction(&I::I32Eqz);
            }
        }
    }
    f.instruction(&I::If(BlockType::Empty));
    value(f, w, shape, l, &rule.destination);
    f.instruction(&I::LocalSet(l.dst));
    value(f, w, shape, l, &rule.length);
    f.instruction(&I::LocalSet(l.len));
    let call = |f: &mut Function, func: u32| {
        f.instruction(&I::LocalGet(l.dst));
        f.instruction(&I::I32Const(WINDOW_MASK as i32));
        f.instruction(&I::I32And);
        f.instruction(&I::LocalGet(l.len));
        f.instruction(&I::Call(func));
    };
    match &rule.effect {
        Effect::Invalidate => call(f, invalidate),
        Effect::Publish { target, source_offset } => {
            if let Offset::Value(v) = source_offset {
                value(f, w, shape, l, v);
                f.instruction(&I::LocalSet(l.so));
            }
            let publish_to = |f: &mut Function, p: usize| {
                check_destination(f, r, l, index, &r.binding.placements[p], source_offset);
                call(f, publish(p));
            };
            match target {
                Target::Placement(p) => publish_to(f, *p),
                Target::Selector { value: sel, cases } => {
                    value(f, w, shape, l, sel);
                    f.instruction(&I::LocalSet(l.sel));
                    f.instruction(&I::Block(BlockType::Empty));
                    for &(v, p) in cases {
                        f.instruction(&I::LocalGet(l.sel));
                        f.instruction(&I::I32Const(v as i32));
                        f.instruction(&I::I32Eq);
                        f.instruction(&I::If(BlockType::Empty));
                        publish_to(f, p);
                        f.instruction(&I::Br(1));
                        f.instruction(&I::End);
                    }
                    fault(f, r, index, binding_cause::UNKNOWN_SELECTOR, l.sel);
                    f.instruction(&I::End);
                }
            }
        }
    }
    f.instruction(&I::End);
}

/// Reject a publication outside the placement's layout: the destination
/// must be where its source offset loads, within one segment.
fn check_destination(f: &mut Function, r: &Refs, l: &Locals, rule: usize, p: &Placement, offset: &Offset) {
    let dstm = |f: &mut Function| {
        f.instruction(&I::LocalGet(l.dst));
        f.instruction(&I::I32Const(WINDOW_MASK as i32));
        f.instruction(&I::I32And);
    };
    f.instruction(&I::I32Const(0));
    f.instruction(&I::LocalSet(l.ok));
    for (file, vram) in &p.segments {
        let base = vram & WINDOW_MASK;
        let size = file.end - file.start;
        match offset {
            Offset::Value(_) => {
                // file.start <= so < file.end
                f.instruction(&I::LocalGet(l.so));
                f.instruction(&I::I32Const(file.start as i32));
                f.instruction(&I::I32Sub);
                f.instruction(&I::I32Const(size as i32));
                f.instruction(&I::I32LtU);
                // dst == base + (so - file.start)
                dstm(f);
                f.instruction(&I::LocalGet(l.so));
                f.instruction(&I::I32Const(file.start as i32));
                f.instruction(&I::I32Sub);
                f.instruction(&I::I32Const(base as i32));
                f.instruction(&I::I32Add);
                f.instruction(&I::I32Eq);
                f.instruction(&I::I32And);
                // len <= file.end - so
                f.instruction(&I::LocalGet(l.len));
                f.instruction(&I::I32Const(file.end as i32));
                f.instruction(&I::LocalGet(l.so));
                f.instruction(&I::I32Sub);
                f.instruction(&I::I32LeU);
                f.instruction(&I::I32And);
            }
            Offset::Derive => {
                // rel = dst - base; rel <= size && len <= size - rel
                dstm(f);
                f.instruction(&I::I32Const(base as i32));
                f.instruction(&I::I32Sub);
                f.instruction(&I::LocalTee(l.rel));
                f.instruction(&I::I32Const(size as i32));
                f.instruction(&I::I32LeU);
                f.instruction(&I::LocalGet(l.len));
                f.instruction(&I::I32Const(size as i32));
                f.instruction(&I::LocalGet(l.rel));
                f.instruction(&I::I32Sub);
                f.instruction(&I::I32LeU);
                f.instruction(&I::I32And);
            }
        }
        f.instruction(&I::If(BlockType::Empty));
        f.instruction(&I::I32Const(1));
        f.instruction(&I::LocalSet(l.ok));
        f.instruction(&I::End);
    }
    f.instruction(&I::LocalGet(l.ok));
    f.instruction(&I::I32Eqz);
    f.instruction(&I::If(BlockType::Empty));
    fault(f, r, rule, binding_cause::BAD_DESTINATION, l.dst);
    f.instruction(&I::End);
}
