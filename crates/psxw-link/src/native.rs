//! Native references (plans/dispatch-binding B): which function references
//! a C object takes are lowered to native handles into the linker's private
//! `native_functions` table, and which keep their guest address.
//!
//! A reference site is a function address an object takes: an `i32.const`
//! with a table-index relocation in code, or a table-index word in one of
//! its compiler-owned pointer tables (AUX data, layout.rs). Every value its
//! functions compute is a node. What moves a value without observing it
//! connects nodes: locals, block results and branches, `select`, a call to
//! one of the object's source-only helpers (its parameters and results), a
//! word loaded from one of its pointer tables (the table's cells). A
//! connected flow is therefore everything a reference can reach, and
//! everything else that reaches the same consumers.
//!
//! A table's cell is read only by a 32-bit load whose offset into the table
//! is proven a multiple of 4: constants, relocation addends, shifts, masks
//! and sums carry each value's residue modulo 4. Any other read of a
//! table's bytes (narrower, wider, or at an offset not proven aligned)
//! observes the bytes of its cells, so its flow keeps guest addresses. An
//! aligned index past the table's end is C's undefined behaviour; AUX
//! placement has no original counterpart, so neither representation
//! preserves what such a read sees.
//!
//! A flow is native only when every target has a fixed binding and every
//! use is accounted for: `call_indirect` (the native call), a null test,
//! an equality between references that names distinct functions by
//! distinct addresses, and moves. Anything else keeps the whole flow at its
//! guest addresses, with the first reason, in this order:
//!
//! - `late_binding`: a target is a changing slot (its binding decides, at
//!   each call, what runs) or a legacy overlay entry;
//! - `unstable_dependency`: a target's binding isn't established (an image
//!   without a placement, no compiled implementation);
//! - `opaque_escape`: the value leaves to, or meets values from, a guest
//!   boundary: guest memory, a global, an indirect call, a function the
//!   guest can call (its parameters and results);
//! - `numeric_observation`: arithmetic, an ordering, a memory address, a
//!   number joining the flow, an equality that could differ;
//! - `missing_uses`: something the analysis doesn't model.
//!
//! A call into a lifted translation that enters it directly can pass a
//! handle in an argument register the lifter proved is only moved, tested
//! against zero, called through and returned (`LiftedParam::Native`): the
//! call sets that register's bit in the representation word the
//! translation takes on entry, its `jalr`s through the value call the
//! native table under the bit, and a returned value is the call's result.
//!
//! There are no conversions between the representations: a native flow
//! never meets a guest value, so no handle reaches guest-visible state and
//! no guest value reaches the native table.

use crate::link::Input;
use crate::object::{NameClass, RelocKind, SymbolKind, WasmObject, classify_name};
use anyhow::{Context as _, Result, bail, ensure};
use psxw_inputs::AddressMap;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use wasmparser::{BinaryReader, BlockType, FunctionBody, Operator, RelocationType};

/// A function a native handle names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Identity {
    /// A guest-associated function: (container, original entry).
    Guest { container: String, addr: u32 },
    /// A source-only C helper: (container, translation unit, symbol). No
    /// original address is invented for it.
    Symbol { container: String, tu: String, symbol: String },
}

impl std::fmt::Display for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Identity::Guest { container, addr } => write!(f, "{container} 0x{addr:08X}"),
            Identity::Symbol { tu, symbol, .. } => write!(f, "{tu}:{symbol}"),
        }
    }
}

/// Why a flow keeps its guest representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    LateBinding,
    UnstableDependency,
    OpaqueEscape,
    NumericObservation,
    MissingUses,
}

impl Reason {
    pub fn name(self) -> &'static str {
        match self {
            Reason::LateBinding => "late_binding",
            Reason::UnstableDependency => "unstable_dependency",
            Reason::OpaqueEscape => "opaque_escape",
            Reason::NumericObservation => "numeric_observation",
            Reason::MissingUses => "missing_uses",
        }
    }
}

/// A reference's target, as the linker binds it.
pub enum Target {
    /// Bound at every invocation: its identity, how it is implemented
    /// (`C`, `lift`, ...), why its binding is fixed, and for a source-only
    /// helper its (input, function).
    Fixed { id: Identity, tier: String, evidence: String, helper: Option<(usize, u32)> },
    /// Only its guest address keeps its meaning.
    Guest(Reason, String),
}

/// A lifted function's argument register, as the lifter proved it
/// (plans/dispatch-binding B; `psxw_lift::analysis::transport`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiftedParam {
    /// Its entry value is only moved, null-tested, called through by these
    /// `jalr`s (native calls when the representation word says the argument
    /// is a native handle) and, if `returned`, returned in `v0` on every way
    /// out.
    Native { jalrs: Vec<u32>, returned: bool },
    /// A native handle can't pass there: why, and whether that is an
    /// observation of the value (`numeric_observation`), its escape
    /// (`opaque_escape`), or a use the proof doesn't follow (`missing_uses`).
    Refused(Reason, String),
}

/// A call to a lifted function whose translation it enters directly (no
/// hook chain or wrapper): where each C argument goes.
#[derive(Debug, Clone)]
pub struct Transport {
    /// The translation's function index, the call's target.
    pub entry: u32,
    pub callee: String,
    /// Per C argument.
    pub args: Vec<ArgFate>,
}

#[derive(Debug, Clone)]
pub enum ArgFate {
    /// Into argument register `reg`, which can carry a handle to these
    /// `jalr`s, and back as the call's result if `returned`.
    Native { reg: u8, jalrs: Vec<u32>, returned: bool },
    /// Into an argument register that can't.
    Refused(Reason, String),
    /// Anywhere else (the stack, part of a wider value): a guest boundary.
    Escape,
}

/// What the link lowers natively.
#[derive(Debug, Default)]
pub struct Plan {
    /// Handle `k + 1` names `handles[k]`; 0 is null.
    pub handles: Vec<Identity>,
    /// Source-only helpers' (input, function).
    pub helpers: HashMap<Identity, (usize, u32)>,
    /// Code reference sites lowered to a handle: (input, CODE relocation
    /// offset) → handle.
    pub code: HashMap<(usize, u32), u32>,
    /// Pointer-table cells holding a handle: (input, DATA relocation
    /// offset) → handle.
    pub data: HashMap<(usize, u32), u32>,
    /// `call_indirect` sites through the native table: (input, CODE offset
    /// of the operator).
    pub calls: HashSet<(usize, u32)>,
    /// Calls to lifted functions passing native handles: (input, CODE offset
    /// of the operator) → (target entry, representation word: bit k for a
    /// handle in argument register k).
    pub transports: HashMap<(usize, u32), (u32, u32)>,
    /// Retained code sites: why, for diagnostics.
    pub retained: HashMap<(usize, u32), String>,
    pub report: Report,
}

/// The native-reference part of the build and conformance reports.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    /// The native table, by handle: `<handle> <identity>`.
    pub handles: Vec<String>,
    /// Reference sites lowered to handles (code and data).
    pub native_sites: usize,
    /// `call_indirect` sites through the native table.
    pub native_calls: usize,
    /// Handles passed into lifted functions' argument registers, whose
    /// `jalr`s then call through the native table.
    pub native_transports: usize,
    /// Reference sites keeping their guest address.
    pub retained_sites: usize,
    /// Each connected flow holding a reference site.
    pub flows: Vec<Flow>,
    /// Devirtualized lifted pointer calls, not native references: a `jalr`
    /// whose target the lifter's dataflow proves (carried across an inlined
    /// callee's register passing, or a join), bound as the dispatch table
    /// binds a pointer to it (filled by the build):
    /// `<container> <function> 0x<pc>: jalr → 0x<target>`.
    pub devirtualized: Vec<String>,
    /// Lifted argument registers a native handle can pass in (filled by the
    /// build): `<container> <function> a<k>: jalr 0x..`. A native caller
    /// says which through the representation word.
    pub lifted_carriers: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Flow {
    /// `<input label>#<n>`, in site order.
    pub id: String,
    /// `native` or `guest`.
    pub representation: &'static str,
    pub reason: Option<Reason>,
    pub detail: Option<String>,
    /// `<function> code+0x..: &<symbol>`, `<table> data+0x..: &<symbol>`.
    pub sites: Vec<String>,
    /// Each target, its stability evidence or why it has none.
    pub targets: Vec<String>,
    /// Its `call_indirect` sites.
    pub consumers: Vec<String>,
    /// Where it crosses between representations or tiers: a native call
    /// entering a target's o32 entry; the guest boundary that kept it.
    pub crossings: Vec<String>,
}

/// A function symbol of `inp` naming one of its own source-only helpers:
/// defined there, with no guest address (or any of generated code's local
/// functions). Its local function index. The linker calls these directly.
pub fn local_helper(map: &AddressMap, inp: &Input, symbol: u32) -> Option<u32> {
    let s = inp.obj.symbols.get(symbol as usize)?;
    if s.kind != SymbolKind::Function || !s.defined {
        return None;
    }
    let NameClass::Guest(name) = classify_name(&s.name) else { return None };
    if map.resolve(&inp.container, name).is_some() && !inp.generated {
        return None;
    }
    s.index.map(|i| i - inp.obj.num_imported_funcs())
}

/// [`local_helper`] when no other symbol names the same function. An alias
/// is its function, which may have a guest address, hooks and guest
/// callers: only a function with one name is a source-only helper, with one
/// identity.
pub fn helper_function(map: &AddressMap, inp: &Input, symbol: u32) -> Option<u32> {
    let f = local_helper(map, inp, symbol)?;
    let index = f + inp.obj.num_imported_funcs();
    let names = inp.obj.symbols.iter().filter(|s| s.kind == SymbolKind::Function && s.defined && s.index == Some(index)).count();
    (names == 1).then_some(f)
}

/// What [`analyze`] needs from the link.
pub struct Facts<'a> {
    /// Inputs whose code is analysed: the game's decompiled C.
    pub analysed: &'a dyn Fn(usize) -> bool,
    /// Whether data symbol `s` of input `i` is placed in AUX.
    pub aux: &'a dyn Fn(usize, u32) -> bool,
    /// [`helper_function`], for functions the link emits.
    pub helper: &'a dyn Fn(usize, u32) -> Option<u32>,
    /// The target of a reference to function symbol `s` of input `i`.
    pub target: &'a dyn Fn(usize, u32) -> Target,
    /// A call from input `i` to function symbol `s` (signature given) that
    /// enters a lifted function's translation directly.
    pub transport: &'a dyn Fn(usize, u32, &crate::object::FuncType) -> Option<Transport>,
}

/// Classify every reference flow and assign handles.
pub fn analyze(inputs: &[Input], facts: &Facts) -> Result<Plan> {
    let mut objects = Vec::new();
    for (i, inp) in inputs.iter().enumerate().filter(|(i, _)| (facts.analysed)(*i)) {
        objects.push(Object::analyze(i, inp, facts).with_context(|| format!("{}: native references", inp.label))?);
    }
    // Targets first, so handles are assigned in identity order.
    let mut targets: BTreeMap<(usize, u32), Target> = BTreeMap::new();
    for o in &objects {
        for s in o.code_sites.iter().chain(&o.data_sites) {
            targets.entry((o.input, s.symbol)).or_insert_with(|| (facts.target)(o.input, s.symbol));
        }
    }
    let mut plan = Plan::default();
    let mut flows: Vec<(Grouped, usize)> = Vec::new();
    for mut o in objects {
        flows.extend(o.flows(&targets).into_iter().map(|g| (g, o.input)));
    }
    let mut ids: BTreeSet<Identity> = BTreeSet::new();
    for ((sites, data, _, _, verdict), i) in &flows {
        if verdict.is_none() {
            for s in sites.iter().chain(data) {
                if let Some(Target::Fixed { id, helper, .. }) = targets.get(&(*i, s.symbol)) {
                    ids.insert(id.clone());
                    if let Some(h) = helper {
                        plan.helpers.insert(id.clone(), *h);
                    }
                }
            }
        }
    }
    plan.handles = ids.into_iter().collect();
    let handle: HashMap<&Identity, u32> = plan.handles.iter().enumerate().map(|(k, id)| (id, k as u32 + 1)).collect();
    let mut counter: BTreeMap<usize, usize> = BTreeMap::new();
    for ((sites, data, calls, carried, verdict), i) in flows {
        let label = &inputs[i].label;
        let n = counter.entry(i).or_default();
        *n += 1;
        let site_text = |s: &Site, kind: &str| format!("{} {kind}+0x{:X}: &{}", s.at, s.offset, s.name);
        let mut target_text = Vec::new();
        let mut crossings = Vec::new();
        for s in sites.iter().chain(&data) {
            let line = match &targets[&(i, s.symbol)] {
                Target::Fixed { id, tier, evidence, .. } => {
                    if verdict.is_none() {
                        crossings.push(format!("native calls enter {id}'s o32 entry ({tier})"));
                    }
                    format!("{id} {} ({tier}; fixed: {evidence})", s.name)
                }
                Target::Guest(r, why) => format!("{} ({}: {why})", s.name, r.name()),
            };
            target_text.push(line);
        }
        target_text.sort();
        target_text.dedup();
        crossings.sort();
        crossings.dedup();
        let native = verdict.is_none();
        if native {
            for s in &sites {
                plan.code.insert((i, s.offset), handle[target_id(&targets, i, s)]);
            }
            for s in &data {
                plan.data.insert((i, s.offset), handle[target_id(&targets, i, s)]);
            }
            plan.calls.extend(calls.iter().map(|(at, _)| (i, *at)));
            for c in &carried {
                let e = plan.transports.entry((i, c.at)).or_insert((c.entry, 0));
                e.1 |= 1 << c.reg;
                crossings.push(format!("{}: a native handle under the representation word", c.what));
            }
            plan.report.native_sites += sites.len() + data.len();
            plan.report.native_calls += calls.len();
            plan.report.native_transports += carried.len();
        } else {
            let (r, why) = verdict.as_ref().expect("retained");
            for s in &sites {
                plan.retained.insert((i, s.offset), format!("{}: {why}", r.name()));
            }
            plan.report.retained_sites += sites.len() + data.len();
            crossings.push(format!("kept at guest addresses: {why}"));
        }
        plan.report.flows.push(Flow {
            id: format!("{label}#{n}"),
            representation: if native { "native" } else { "guest" },
            reason: verdict.as_ref().map(|v| v.0),
            detail: verdict.map(|v| v.1),
            sites: sites.iter().map(|s| site_text(s, "code")).chain(data.iter().map(|s| site_text(s, "data"))).collect(),
            targets: target_text,
            consumers: calls.into_iter().map(|(_, c)| c).chain(carried.into_iter().map(|c| c.what)).collect(),
            crossings,
        });
    }
    plan.report.handles = plan.handles.iter().enumerate().map(|(k, id)| format!("{} {id}", k + 1)).collect();
    Ok(plan)
}

/// One connected flow: its code sites, data sites, `call_indirect` sites
/// (CODE offset, where), and why it stays guest (`None`: native).
type Grouped = (Vec<Site>, Vec<Site>, Vec<(u32, String)>, Vec<Carried>, Option<(Reason, String)>);

/// A handle a C call passes into a lifted argument register.
#[derive(Debug, Clone)]
struct Carried {
    /// The call's CODE offset, its target, the register.
    at: u32,
    entry: u32,
    reg: u8,
    node: u32,
    what: String,
}

fn target_id<'a>(targets: &'a BTreeMap<(usize, u32), Target>, i: usize, s: &Site) -> &'a Identity {
    match &targets[&(i, s.symbol)] {
        Target::Fixed { id, .. } => id,
        Target::Guest(..) => unreachable!("a native flow's targets are fixed"),
    }
}

/// A reference site.
#[derive(Debug, Clone)]
struct Site {
    /// Its relocation's offset (CODE or DATA payload).
    offset: u32,
    symbol: u32,
    name: String,
    /// The function or table holding it.
    at: String,
    node: u32,
}

/// Union-find over value nodes, each class with the first reason of each
/// kind it met, and whether references in it are compared for equality.
#[derive(Default)]
struct Nodes {
    parent: Vec<u32>,
    why: Vec<BTreeMap<Reason, String>>,
    eq: Vec<bool>,
}

impl Nodes {
    fn node(&mut self) -> u32 {
        self.parent.push(self.parent.len() as u32);
        self.why.push(BTreeMap::new());
        self.eq.push(false);
        self.parent.len() as u32 - 1
    }

    fn marked(&mut self, r: Reason, why: impl FnOnce() -> String) -> u32 {
        let n = self.node();
        self.mark(n, r, why);
        n
    }

    fn find(&mut self, mut x: u32) -> u32 {
        while self.parent[x as usize] != x {
            let p = self.parent[self.parent[x as usize] as usize];
            self.parent[x as usize] = p;
            x = p;
        }
        x
    }

    fn union(&mut self, a: u32, b: u32) -> u32 {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.parent[b as usize] = a;
            let moved = std::mem::take(&mut self.why[b as usize]);
            for (r, w) in moved {
                self.why[a as usize].entry(r).or_insert(w);
            }
            self.eq[a as usize] |= self.eq[b as usize];
        }
        a
    }

    fn mark(&mut self, x: u32, r: Reason, why: impl FnOnce() -> String) {
        let x = self.find(x);
        self.why[x as usize].entry(r).or_insert_with(why);
    }
}

/// A value on the abstract operand stack: its node, its constant (a null
/// test compares with 0), the pointer table it points into, if any, and
/// its residue modulo 4 where known (for a pointer into a table: its
/// offset's, tables being word-aligned), so a load is proven to read a
/// whole cell.
#[derive(Debug, Clone, Copy)]
struct V {
    n: u32,
    k: Option<i32>,
    table: Option<u32>,
    rem: Option<u32>,
}

/// The residue modulo 4 of a binary operator's result, from its operands'.
fn residue(op: &Operator, a: &V, b: &V) -> Option<u32> {
    use Operator as O;
    let (x, y) = (a.rem, b.rem);
    let both = |f: fn(u32, u32) -> u32| Some(f(x?, y?) & 3);
    match op {
        O::I32Add => both(|x, y| x.wrapping_add(y)),
        O::I32Sub => both(|x, y| x.wrapping_sub(y)),
        O::I32Mul if x == Some(0) || y == Some(0) => Some(0),
        O::I32Mul => both(|x, y| x.wrapping_mul(y)),
        O::I32And if x == Some(0) || y == Some(0) => Some(0),
        O::I32And => both(|x, y| x & y),
        O::I32Or => both(|x, y| x | y),
        O::I32Xor => both(|x, y| x ^ y),
        O::I32Shl => match b.k.map(|s| s as u32 & 31) {
            Some(s) if s >= 2 => Some(0),
            Some(s) => Some((x? << s) & 3),
            None => None,
        },
        _ => None,
    }
}

struct Frame {
    /// The values a branch to it carries (a loop's: none).
    branch: Vec<u32>,
    /// Its results, at its end.
    results: Vec<u32>,
    height: usize,
    dead: bool,
    /// Each local's value where a branch to it arrives: at a loop's head,
    /// at any other block's end (none for the function's).
    joins: Vec<u32>,
    /// An `if`: the locals at its start, until its `else`.
    entry: Option<Vec<u32>>,
    is_loop: bool,
}

/// One object's flows.
struct Object<'a> {
    input: usize,
    obj: &'a WasmObject,
    nodes: Nodes,
    /// Per local function: its parameters' and results' nodes.
    params: Vec<Vec<u32>>,
    results: Vec<Vec<u32>>,
    /// Pointer tables (data symbol → its cells' node).
    tables: BTreeMap<u32, u32>,
    code_sites: Vec<Site>,
    data_sites: Vec<Site>,
    /// `call_indirect` sites: (CODE offset, callee node, where).
    calls: Vec<(u32, u32, String)>,
    /// Values passed into lifted argument registers that can carry handles.
    carried: Vec<Carried>,
}

/// The arity of plain operators; anything needing module state is handled
/// explicitly or not modelled.
struct NoModule;
impl wasmparser::ModuleArity for NoModule {
    fn sub_type_at(&self, _: u32) -> Option<&wasmparser::SubType> {
        None
    }
    fn tag_type_arity(&self, _: u32) -> Option<(u32, u32)> {
        None
    }
    fn type_index_of_function(&self, _: u32) -> Option<u32> {
        None
    }
    fn func_type_of_cont_type(&self, _: &wasmparser::ContType) -> Option<&wasmparser::FuncType> {
        None
    }
    fn sub_type_of_ref_type(&self, _: &wasmparser::RefType) -> Option<&wasmparser::SubType> {
        None
    }
    fn control_stack_height(&self) -> u32 {
        0
    }
    fn label_block(&self, _: u32) -> Option<(BlockType, wasmparser::FrameKind)> {
        None
    }
}

/// A load or store: (is a store, reads or writes one 32-bit word).
fn access(op: &Operator) -> Option<(bool, bool)> {
    use Operator as O;
    Some(match op {
        O::I32Load { .. } => (false, true),
        O::I64Load { .. }
        | O::F32Load { .. }
        | O::F64Load { .. }
        | O::I32Load8S { .. }
        | O::I32Load8U { .. }
        | O::I32Load16S { .. }
        | O::I32Load16U { .. }
        | O::I64Load8S { .. }
        | O::I64Load8U { .. }
        | O::I64Load16S { .. }
        | O::I64Load16U { .. }
        | O::I64Load32S { .. }
        | O::I64Load32U { .. } => (false, false),
        O::I32Store { .. } => (true, true),
        O::I64Store { .. } | O::F32Store { .. } | O::F64Store { .. } | O::I32Store8 { .. } | O::I32Store16 { .. } | O::I64Store8 { .. } | O::I64Store16 { .. } | O::I64Store32 { .. } => {
            (true, false)
        }
        _ => return None,
    })
}

/// A load's or store's memarg.
fn memarg(op: &Operator) -> Option<wasmparser::MemArg> {
    use Operator as O;
    match op {
        O::I32Load { memarg }
        | O::I64Load { memarg }
        | O::F32Load { memarg }
        | O::F64Load { memarg }
        | O::I32Load8S { memarg }
        | O::I32Load8U { memarg }
        | O::I32Load16S { memarg }
        | O::I32Load16U { memarg }
        | O::I64Load8S { memarg }
        | O::I64Load8U { memarg }
        | O::I64Load16S { memarg }
        | O::I64Load16U { memarg }
        | O::I64Load32S { memarg }
        | O::I64Load32U { memarg }
        | O::I32Store { memarg }
        | O::I64Store { memarg }
        | O::F32Store { memarg }
        | O::F64Store { memarg }
        | O::I32Store8 { memarg }
        | O::I32Store16 { memarg }
        | O::I64Store8 { memarg }
        | O::I64Store16 { memarg }
        | O::I64Store32 { memarg } => Some(*memarg),
        _ => None,
    }
}

/// The code relocations on the operator at `pos..end` (object positions),
/// advancing `ri` through `relocs` (sorted, CODE-payload offsets from
/// `base`).
fn relocs_at(relocs: &[crate::object::CodeReloc], ri: &mut usize, pos: u32, end: u32, base: u32) -> Vec<crate::object::CodeReloc> {
    let mut here = Vec::new();
    while *ri < relocs.len() && relocs[*ri].offset + base < end {
        if relocs[*ri].offset + base >= pos {
            here.push(relocs[*ri]);
        }
        *ri += 1;
    }
    here
}

/// A table-index relocation of any kind.
fn table_index(ty: RelocationType) -> bool {
    use RelocationType as R;
    matches!(ty, R::TableIndexSleb | R::TableIndexI32 | R::TableIndexRelSleb | R::TableIndexSleb64 | R::TableIndexI64 | R::TableIndexRelSleb64)
}

/// Plain arithmetic, comparisons and conversions of i32, i64, f32 and
/// f64 values: numbers in, numbers out. Not loads or stores (handled
/// apart), atomics, lanes (SIMD, `I32x4…`), references, tables, memory
/// management or control.
fn plain(op: &Operator) -> bool {
    let name = op_name(op);
    ["I32", "I64", "F32", "F64"].iter().any(|p| name.starts_with(p))
        && name.as_bytes().get(3).is_some_and(u8::is_ascii_uppercase)
        && !["Atomic", "Load", "Store"].iter().any(|w| name.contains(w))
}

/// An operator's name, for reasons.
fn op_name(op: &Operator) -> String {
    let s = format!("{op:?}");
    s.split([' ', '{', '(']).next().unwrap_or_default().to_string()
}

impl<'a> Object<'a> {
    fn analyze(input: usize, inp: &'a Input, facts: &Facts) -> Result<Self> {
        let obj = &inp.obj;
        let nimp = obj.num_imported_funcs();
        let mut o = Object {
            input,
            obj,
            nodes: Nodes::default(),
            params: Vec::new(),
            results: Vec::new(),
            tables: BTreeMap::new(),
            code_sites: Vec::new(),
            data_sites: Vec::new(),
            calls: Vec::new(),
            carried: Vec::new(),
        };
        // Which of its functions are its own helpers, and which of those
        // the object calls through a pointer.
        let mut helper_of: HashMap<u32, u32> = HashMap::new();
        for (si, _) in obj.symbols.iter().enumerate() {
            if let Some(f) = (facts.helper)(input, si as u32) {
                helper_of.insert(f, si as u32);
            }
        }
        // Any table-index relocation takes an address, of whatever kind
        // (only `i32.const`'s and data's are reference sites), and any data
        // relocation naming a function.
        let mut address_taken: HashSet<u32> = HashSet::new();
        let ref_relocs = obj
            .code_relocs
            .iter()
            .filter(|r| table_index(r.ty))
            .map(|r| r.symbol)
            .chain(obj.data_relocs.iter().map(|r| r.symbol));
        for s in ref_relocs {
            let f = obj.symbols.get(s as usize).filter(|s| s.kind == SymbolKind::Function);
            if let Some(i) = f.and_then(|s| s.index).filter(|&i| i >= nimp) {
                address_taken.insert(i - nimp);
            }
        }
        for f in 0..obj.funcs.len() as u32 {
            let ty = &obj.types[obj.funcs[f as usize].ty as usize];
            let name = function_name(obj, f);
            let mut params = Vec::new();
            let mut results = Vec::new();
            // A function the guest can call (it has an address, so an o32
            // entry and a dispatch slot), or one called through a pointer:
            // its parameters take values from outside every flow here, and
            // what it returns leaves them.
            let opaque = match helper_of.get(&f) {
                None => Some(format!("{name}, a function the guest can call")),
                Some(_) if address_taken.contains(&f) => Some(format!("{name}, a helper called through a pointer")),
                Some(_) => None,
            };
            for k in 0..ty.params.len() {
                let n = o.nodes.node();
                if let Some(w) = &opaque {
                    o.nodes.mark(n, Reason::OpaqueEscape, || format!("parameter {k} of {w}, takes guest values"));
                }
                params.push(n);
            }
            for _ in &ty.results {
                let n = o.nodes.node();
                if let Some(w) = &opaque {
                    o.nodes.mark(n, Reason::OpaqueEscape, || format!("returned from {w}"));
                }
                results.push(n);
            }
            o.params.push(params);
            o.results.push(results);
        }
        o.tables(input, facts);
        for f in 0..obj.funcs.len() as u32 {
            // Clang's signature thunks and main wrappers aren't linked.
            if obj.func_symbol(f + nimp).is_some_and(|s| !matches!(classify_name(&s.name), NameClass::Guest(_))) {
                continue;
            }
            match o.unsupported(f)? {
                // Not modelled: every flow it touches keeps its guest form.
                Some(why) => o.unmodelled(f, &format!("{}: {why} isn't modelled", function_name(obj, f)), facts),
                None => o.function(f, facts)?,
            }
        }
        Ok(o)
    }

    /// The object's pointer tables: AUX data holding function references,
    /// read only through loads of whole words at their address. All cells
    /// of a table are one node; a word that isn't a reference or null
    /// joins it as what it is.
    fn tables(&mut self, input: usize, facts: &Facts) {
        let obj = self.obj;
        for (si, s) in obj.symbols.iter().enumerate() {
            let Some(def) = s.data.filter(|_| s.kind == SymbolKind::Data && s.defined) else { continue };
            let relocs: Vec<(u32, &crate::object::Reloc)> = obj.relocs_in(def).collect();
            if !relocs.iter().any(|(_, r)| r.kind == RelocKind::TableIndexI32) || !(facts.aux)(input, si as u32) {
                continue;
            }
            let n = self.nodes.node();
            self.tables.insert(si as u32, n);
            let name = s.name.clone();
            if !s.local {
                self.nodes.mark(n, Reason::MissingUses, || format!("table {name} is visible to other objects"));
            }
            if def.size % 4 != 0 {
                self.nodes.mark(n, Reason::MissingUses, || format!("table {name} isn't whole words"));
            }
            // Another symbol over its bytes reads or writes them where
            // nothing here sees.
            let overlaps = obj.symbols.iter().enumerate().any(|(j, t)| {
                j != si && t.data.is_some_and(|o| o.segment == def.segment && o.offset < def.offset + def.size && def.offset < o.offset + o.size)
            });
            if overlaps {
                self.nodes.mark(n, Reason::MissingUses, || format!("table {name} overlaps another data symbol"));
            }
            let bytes = obj.data_bytes(def);
            for at in (0..def.size).step_by(4) {
                let over: Vec<&(u32, &crate::object::Reloc)> = relocs.iter().filter(|(o, _)| *o + 4 > at && *o < at + 4).collect();
                match over.as_slice() {
                    [(o, r)] if *o == at && r.kind == RelocKind::TableIndexI32 => {
                        let target = obj.symbols.get(r.symbol as usize).map_or_else(String::new, |t| t.name.clone());
                        self.data_sites.push(Site { offset: r.offset, symbol: r.symbol, name: target, at: name.clone(), node: n });
                    }
                    [] if bytes.get(at as usize..at as usize + 4) == Some(&[0; 4][..]) => {}
                    _ => self.nodes.mark(n, Reason::NumericObservation, || format!("table {name} also holds numbers or data addresses")),
                }
            }
        }
        // Its address stored in data, by any relocation: read where nothing
        // here sees.
        for r in &obj.data_relocs {
            if let Some(&n) = self.tables.get(&r.symbol) {
                self.nodes.mark(n, Reason::MissingUses, || "the table's address is stored in data".into());
            }
        }
    }

    /// A function the analysis can't follow: its references, its pointer
    /// tables, the helpers it calls and its own parameters and results keep
    /// their guest form.
    fn unmodelled(&mut self, f: u32, why: &str, facts: &Facts) {
        let obj = self.obj;
        let body = &obj.funcs[f as usize].body;
        let at = function_name(obj, f);
        for r in obj.relocs_for(body) {
            // A type relocation's index is a type's, not a symbol's.
            if r.ty == RelocationType::TypeIndexLeb {
                continue;
            }
            if r.ty == RelocationType::TableIndexSleb {
                let n = self.nodes.marked(Reason::MissingUses, || why.to_string());
                let name = obj.symbols.get(r.symbol as usize).map_or_else(String::new, |s| s.name.clone());
                self.code_sites.push(Site { offset: r.offset, symbol: r.symbol, name, at: at.clone(), node: n });
            }
            if let Some(&n) = self.tables.get(&r.symbol) {
                self.nodes.mark(n, Reason::MissingUses, || why.to_string());
            }
            if let Some(h) = (facts.helper)(self.input, r.symbol) {
                for n in self.params[h as usize].clone().into_iter().chain(self.results[h as usize].clone()) {
                    self.nodes.mark(n, Reason::MissingUses, || why.to_string());
                }
            }
        }
        for n in self.params[f as usize].clone().into_iter().chain(self.results[f as usize].clone()) {
            self.nodes.mark(n, Reason::MissingUses, || why.to_string());
        }
    }

    /// What in function `f` is outside the analysis's domain, if anything:
    /// an operator not in its closed list, or a relocation other than the
    /// one the rewrite lowers on that operator (docs/design.md §2, "The
    /// acceptance boundary").
    fn unsupported(&self, f: u32) -> Result<Option<String>> {
        use RelocationType as R;
        let obj = self.obj;
        let func = &obj.funcs[f as usize];
        let body = FunctionBody::new(BinaryReader::new(&obj.bytes[func.body.clone()], func.body.start as u64));
        let relocs = obj.relocs_for(&func.body);
        let base = obj.code_start as u32;
        let mut ri = 0;
        let mut ops = body.get_operators_reader()?;
        while !ops.eof() {
            use Operator as O;
            let pos = ops.original_position() as u32;
            let op = ops.read()?;
            let here = relocs_at(&relocs, &mut ri, pos, ops.original_position() as u32, base);
            let modelled = match &op {
                O::Block { blockty } | O::Loop { blockty } | O::If { blockty } => self.block(*blockty)?.0 == 0,
                O::Nop | O::Unreachable | O::Else | O::End | O::Br { .. } | O::BrIf { .. } | O::BrTable { .. } | O::Return => true,
                O::Call { .. } | O::CallIndirect { .. } | O::Drop | O::Select | O::TypedSelect { .. } => true,
                O::LocalGet { .. } | O::LocalSet { .. } | O::LocalTee { .. } | O::GlobalGet { .. } | O::GlobalSet { .. } => true,
                O::MemoryCopy { .. } | O::MemoryFill { .. } => true,
                op if access(op).is_some() => true,
                op => plain(op),
            };
            if !modelled {
                return Ok(Some(op_name(&op)));
            }
            let lowered: &[R] = match &op {
                O::I32Const { .. } => &[R::TableIndexSleb, R::MemoryAddrSleb],
                O::Call { .. } => &[R::FunctionIndexLeb],
                O::CallIndirect { .. } => &[R::TypeIndexLeb, R::TableNumberLeb],
                O::GlobalGet { .. } | O::GlobalSet { .. } => &[R::GlobalIndexLeb],
                op if memarg(op).is_some() => &[R::MemoryAddrLeb],
                _ => &[],
            };
            if let Some(r) = here.iter().find(|r| !lowered.contains(&r.ty)) {
                return Ok(Some(format!("a {:?} relocation on {}", r.ty, op_name(&op))));
            }
        }
        Ok(None)
    }

    fn function(&mut self, f: u32, facts: &Facts) -> Result<()> {
        let obj = self.obj;
        let func = &obj.funcs[f as usize];
        let fname = function_name(obj, f);
        let body = FunctionBody::new(BinaryReader::new(&obj.bytes[func.body.clone()], func.body.start as u64));
        // Each local's current value: clang reuses locals for unrelated
        // values, so a write starts a new one; values meet only where
        // control flow joins.
        let mut locals = self.params[f as usize].clone();
        for l in body.get_locals_reader()? {
            let (n, _) = l?;
            for _ in 0..n {
                // Zero at entry: null.
                locals.push(self.nodes.node());
            }
        }
        let relocs = obj.relocs_for(&func.body);
        let base = obj.code_start as u32;
        let mut ri = 0;
        let results = self.results[f as usize].clone();
        let top = Frame { branch: results.clone(), results, height: 0, dead: false, joins: Vec::new(), entry: None, is_loop: false };
        // Parameters' residues are unknown; declared locals start at zero.
        let rems = (0..locals.len()).map(|l| (l >= self.params[f as usize].len()).then_some(0)).collect();
        let mut it = Interp { o: self, stack: Vec::new(), frames: vec![top], locals, rems };
        let mut direct: Option<u32> = None;
        let mut ops = body.get_operators_reader()?;
        while !ops.eof() {
            let pos = ops.original_position() as u32;
            let op = ops.read()?;
            let here = relocs_at(&relocs, &mut ri, pos, ops.original_position() as u32, base);
            let reloc = |t: RelocationType| here.iter().find(|r| r.ty == t).copied();
            let at = pos - base;
            let what = |s: &str| format!("{s} in {fname} at code+0x{at:X}");
            use Operator as O;
            match op {
                O::Nop => {}
                O::Unreachable => it.kill(),
                O::Block { blockty } | O::Loop { blockty } | O::If { blockty } => {
                    let entry = match op {
                        // Its condition: a null test of a reference.
                        O::If { .. } => {
                            let c = it.pop()?;
                            it.consume(c, &what("a condition"));
                            Some(it.locals.clone())
                        }
                        _ => None,
                    };
                    let (params, n) = it.o.block(blockty)?;
                    ensure!(params == 0, "a block with parameters");
                    let results: Vec<u32> = (0..n).map(|_| it.o.nodes.node()).collect();
                    let is_loop = matches!(op, O::Loop { .. });
                    let branch = if is_loop { Vec::new() } else { results.clone() };
                    let joins: Vec<u32> = (0..it.locals.len()).map(|_| it.o.nodes.node()).collect();
                    if is_loop {
                        // The head: entered from before, and from each branch back.
                        it.join(&joins);
                        it.locals = joins.clone();
                        it.rems.fill(None);
                    }
                    let height = it.stack.len();
                    it.frames.push(Frame { branch, results, height, dead: false, joins, entry, is_loop });
                }
                O::Else => {
                    it.end_values()?;
                    let fr = it.frames.last().context("else outside a block")?;
                    let (joins, dead, height) = (fr.joins.clone(), fr.dead, fr.height);
                    if !dead {
                        it.join(&joins);
                    }
                    let fr = it.frames.last_mut().expect("a frame");
                    it.locals = fr.entry.take().context("else outside an if")?;
                    it.rems.fill(None);
                    it.stack.truncate(height);
                    fr.dead = false;
                }
                O::End => {
                    it.end_values()?;
                    let fr = it.frames.pop().context("unbalanced end")?;
                    it.stack.truncate(fr.height);
                    if it.frames.is_empty() {
                        ensure!(ops.eof(), "code after the function's end");
                        break;
                    }
                    if !fr.is_loop {
                        if !fr.dead {
                            it.join(&fr.joins);
                        }
                        // An `if` without `else` falls through from its start.
                        if let Some(entry) = &fr.entry {
                            for (a, b) in entry.iter().zip(&fr.joins) {
                                it.o.nodes.union(*a, *b);
                            }
                        }
                        it.locals = fr.joins;
                        it.rems.fill(None);
                    }
                    for n in fr.results {
                        it.stack.push(V { n, k: None, table: None, rem: None });
                    }
                }
                O::Br { relative_depth } => {
                    it.branch(relative_depth)?;
                    it.kill();
                }
                O::BrIf { relative_depth } => {
                    let c = it.pop()?;
                    it.consume(c, &what("a condition"));
                    it.branch(relative_depth)?;
                }
                O::BrTable { targets } => {
                    let index = it.pop()?;
                    it.numeric(index, &what("a br_table index"));
                    for d in targets.targets() {
                        it.branch(d?)?;
                    }
                    it.branch(targets.default())?;
                    it.kill();
                }
                O::Return => {
                    it.branch(it.frames.len() as u32 - 1)?;
                    it.kill();
                }
                O::Call { function_index } => {
                    let rl = reloc(RelocationType::FunctionIndexLeb).context("call without relocation")?;
                    let sig = obj.func_type(function_index).clone();
                    it.call(rl.symbol, &sig, facts, at, &what)?;
                }
                O::CallIndirect { type_index, .. } => {
                    let sig = obj.types[type_index as usize].clone();
                    match direct.take() {
                        // FastISel's call through a constant: the rewrite
                        // calls the symbol directly.
                        Some(symbol) => it.call(symbol, &sig, facts, at, &what)?,
                        None => {
                            let callee = it.pop()?;
                            it.consume(callee, &what("a call target"));
                            let n = callee.n;
                            it.o.calls.push((at, n, what("call_indirect")));
                            for _ in &sig.params {
                                let a = it.pop()?;
                                it.escape(a, &what("passed to an indirect call"));
                            }
                            for _ in &sig.results {
                                let w = what("the result of an indirect call");
                                let n = it.o.nodes.marked(Reason::OpaqueEscape, || w);
                                it.stack.push(V { n, k: None, table: None, rem: None });
                            }
                        }
                    }
                }
                O::Drop => {
                    it.pop()?;
                }
                O::Select | O::TypedSelect { .. } => {
                    let c = it.pop()?;
                    it.consume(c, &what("a condition"));
                    let b = it.pop()?;
                    let a = it.pop()?;
                    it.consume(a, &what("select"));
                    it.consume(b, &what("select"));
                    let n = it.o.nodes.union(a.n, b.n);
                    it.stack.push(V { n, k: if a.k == b.k { a.k } else { None }, table: None, rem: None });
                }
                O::LocalGet { local_index } => {
                    let n = *it.locals.get(local_index as usize).context("local out of range")?;
                    let rem = it.rems[local_index as usize];
                    it.stack.push(V { n, k: None, table: None, rem });
                }
                O::LocalSet { local_index } | O::LocalTee { local_index } => {
                    let v = it.pop()?;
                    it.consume(v, &what("a local"));
                    *it.locals.get_mut(local_index as usize).context("local out of range")? = v.n;
                    it.rems[local_index as usize] = v.rem;
                    if matches!(op, O::LocalTee { .. }) {
                        it.stack.push(V { n: v.n, k: v.k, table: None, rem: v.rem });
                    }
                }
                O::GlobalGet { .. } => {
                    let w = what("a global's value");
                    let n = it.o.nodes.marked(Reason::OpaqueEscape, || w);
                    it.stack.push(V { n, k: None, table: None, rem: None });
                }
                O::GlobalSet { .. } => {
                    let v = it.pop()?;
                    it.escape(v, &what("stored to a global"));
                }
                O::I32Const { value } => {
                    if let Some(rl) = reloc(RelocationType::TableIndexSleb) {
                        if matches!(ops.clone().read(), Ok(O::CallIndirect { .. })) {
                            direct = Some(rl.symbol);
                            continue;
                        }
                        let n = it.o.nodes.node();
                        let name = obj.symbols.get(rl.symbol as usize).map_or_else(String::new, |s| s.name.clone());
                        it.o.code_sites.push(Site { offset: rl.offset, symbol: rl.symbol, name, at: fname.clone(), node: n });
                        it.stack.push(V { n, k: None, table: None, rem: None });
                    } else if let Some(rl) = reloc(RelocationType::MemoryAddrSleb) {
                        let w = what("a data address");
                        let n = it.o.nodes.marked(Reason::NumericObservation, || w);
                        // A table's address plus the relocation's addend.
                        let table = it.o.tables.contains_key(&rl.symbol).then_some(rl.symbol);
                        let rem = table.map(|_| rl.addend as u32 & 3);
                        it.stack.push(V { n, k: None, table, rem });
                    } else {
                        let n = it.o.nodes.node();
                        if value != 0 {
                            it.o.nodes.mark(n, Reason::NumericObservation, || format!("the number {value:#x} in {fname} at code+0x{at:X}"));
                        }
                        it.stack.push(V { n, k: Some(value), table: None, rem: Some(value as u32 & 3) });
                    }
                }
                O::I32Eqz => {
                    let a = it.pop()?;
                    it.consume(a, &what("a null test"));
                    it.push_number(&what("a comparison's result"));
                }
                O::I32Eq | O::I32Ne => {
                    let b = it.pop()?;
                    let a = it.pop()?;
                    it.consume(a, &what("a comparison"));
                    it.consume(b, &what("a comparison"));
                    // With 0: a null test. Otherwise an equality that must
                    // come out the same in either representation.
                    if a.k != Some(0) && b.k != Some(0) {
                        let r = it.o.nodes.union(a.n, b.n);
                        it.o.nodes.eq[r as usize] = true;
                    }
                    it.push_number(&what("a comparison's result"));
                }
                O::I32Add => {
                    let b = it.pop()?;
                    let a = it.pop()?;
                    // A table's address plus an index still points into it.
                    let table = match (a.table, b.table) {
                        (Some(t), None) | (None, Some(t)) => Some(t),
                        (Some(_), Some(_)) => {
                            it.consume(a, &what("arithmetic"));
                            it.consume(b, &what("arithmetic"));
                            None
                        }
                        (None, None) => None,
                    };
                    it.numeric(a, &what("arithmetic"));
                    it.numeric(b, &what("arithmetic"));
                    let w = what("an arithmetic result");
                    let n = it.o.nodes.marked(Reason::NumericObservation, || w);
                    it.stack.push(V { n, k: None, table, rem: residue(&op, &a, &b) });
                }
                ref op if access(op).is_some() => {
                    let (store, word) = access(op).expect("an access");
                    let memarg_reloc = reloc(RelocationType::MemoryAddrLeb);
                    let memarg_table = memarg_reloc.and_then(|r| it.o.tables.contains_key(&r.symbol).then_some(r.symbol));
                    // The byte offset the access adds to its address operand:
                    // a relocated one is the symbol's address plus its addend.
                    let offset = match memarg_reloc {
                        Some(r) => r.addend as u32,
                        None => memarg(op).map_or(0, |m| m.offset as u32),
                    };
                    if store {
                        let v = it.pop()?;
                        it.escape(v, &what("stored to guest memory"));
                    }
                    let addr = it.pop()?;
                    it.numeric(addr, &what("a memory address"));
                    let table = match (addr.table, memarg_table) {
                        (Some(t), None) | (None, Some(t)) => Some(t),
                        (Some(t), Some(u)) => {
                            it.o.mark_table(t, || "two tables in one address".into());
                            it.o.mark_table(u, || "two tables in one address".into());
                            None
                        }
                        (None, None) => None,
                    };
                    // A cell is read only by a word load at a word offset into
                    // the table: anything else reads bytes of its cells.
                    let whole = word && addr.rem.is_some_and(|r| r.wrapping_add(offset) & 3 == 0);
                    match table {
                        Some(t) if store => it.o.mark_table(t, || what("a store into the table")),
                        Some(t) if !whole => {
                            let w = what("a read of table bytes not proven to be one whole cell");
                            let n = it.o.tables[&t];
                            it.o.nodes.mark(n, Reason::NumericObservation, || w);
                            it.push_number(&what("a load"));
                        }
                        // A cell of the table.
                        Some(t) => {
                            let n = it.o.tables[&t];
                            it.stack.push(V { n, k: None, table: None, rem: None });
                        }
                        None if store => {}
                        None => {
                            let w = what("loaded from guest memory");
                            let n = it.o.nodes.marked(Reason::OpaqueEscape, || w);
                            it.stack.push(V { n, k: None, table: None, rem: None });
                        }
                    }
                }
                // Plain arithmetic, `memory.copy` and `memory.fill`
                // ([`Object::unsupported`] admits nothing else): numbers in,
                // numbers out.
                other => {
                    let (pops, pushes) = other.operator_arity(&NoModule).with_context(|| format!("{} isn't modelled", op_name(&other)))?;
                    ensure!(here.is_empty(), "an unmodelled relocation on {}", op_name(&other));
                    let name = op_name(&other);
                    let mut args = Vec::new();
                    for _ in 0..pops {
                        let v = it.pop()?;
                        it.consume(v, &what(&name));
                        it.numeric(v, &what(&name));
                        args.push(v);
                    }
                    for _ in 0..pushes {
                        it.push_number(&what(&name));
                    }
                    // What the result is modulo 4, for an offset into a table.
                    if let ([b, a], 1) = (args.as_slice(), pushes) {
                        if let Some(top) = it.stack.last_mut() {
                            top.rem = residue(&other, a, b);
                        }
                    }
                }
            }
        }
        ensure!(it.frames.is_empty(), "the function doesn't end");
        Ok(())
    }

    fn block(&self, ty: BlockType) -> Result<(usize, usize)> {
        Ok(match ty {
            BlockType::Empty => (0, 0),
            BlockType::Type(_) => (0, 1),
            BlockType::FuncType(t) => {
                let t = self.obj.types.get(t as usize).context("block type out of range")?;
                (t.params.len(), t.results.len())
            }
        })
    }

    fn mark_table(&mut self, t: u32, why: impl FnOnce() -> String) {
        let n = self.tables[&t];
        self.nodes.mark(n, Reason::MissingUses, why);
    }

    /// The object's connected flows holding references.
    fn flows(&mut self, targets: &BTreeMap<(usize, u32), Target>) -> Vec<Grouped> {
        let mut by: BTreeMap<u32, Grouped> = BTreeMap::new();
        let mut order: Vec<u32> = Vec::new();
        for s in self.code_sites.clone() {
            let r = self.nodes.find(s.node);
            if !by.contains_key(&r) {
                order.push(r);
            }
            by.entry(r).or_default().0.push(s);
        }
        for s in self.data_sites.clone() {
            let r = self.nodes.find(s.node);
            if !by.contains_key(&r) {
                order.push(r);
            }
            by.entry(r).or_default().1.push(s);
        }
        for (at, n, what) in self.calls.clone() {
            let r = self.nodes.find(n);
            if let Some(e) = by.get_mut(&r) {
                e.2.push((at, what));
            }
        }
        for c in self.carried.clone() {
            let r = self.nodes.find(c.node);
            if let Some(e) = by.get_mut(&r) {
                e.3.push(c);
            }
        }
        let mut out = Vec::new();
        for r in order {
            let (sites, data, calls, carried, _) = by.remove(&r).expect("a flow");
            let mut why: BTreeMap<Reason, String> = self.nodes.why[r as usize].clone();
            // A target whose binding isn't fixed.
            let mut ids: BTreeMap<Option<u32>, BTreeSet<&Identity>> = BTreeMap::new();
            for s in sites.iter().chain(&data) {
                match &targets[&(self.input, s.symbol)] {
                    Target::Guest(reason, w) => {
                        why.entry(*reason).or_insert_with(|| format!("&{}: {w}", s.name));
                    }
                    Target::Fixed { id, .. } => {
                        let addr = match id {
                            Identity::Guest { addr, .. } => Some(*addr),
                            Identity::Symbol { .. } => None,
                        };
                        ids.entry(addr).or_default().insert(id);
                    }
                }
            }
            // Equality between references holds in both representations
            // only where distinct functions have distinct addresses.
            let shared = ids.iter().find_map(|(a, set)| a.filter(|_| set.len() > 1));
            if let (true, Some(a)) = (self.nodes.eq[r as usize], shared) {
                why.entry(Reason::NumericObservation).or_insert_with(|| format!("an equality between references to functions sharing 0x{a:08X}"));
            }
            let verdict = why.into_iter().next();
            out.push((sites, data, calls, carried, verdict));
        }
        out
    }
}

/// The function's symbol name, or its index.
fn function_name(obj: &WasmObject, f: u32) -> String {
    obj.func_symbol(f + obj.num_imported_funcs()).map_or_else(|| format!("#{f}"), |s| s.name.clone())
}

/// The operand stack and locals of one function.
struct Interp<'o, 'a> {
    o: &'o mut Object<'a>,
    stack: Vec<V>,
    frames: Vec<Frame>,
    locals: Vec<u32>,
    /// Each local's current value's residue modulo 4, where known.
    rems: Vec<Option<u32>>,
}

impl Interp<'_, '_> {
    fn pop(&mut self) -> Result<V> {
        let fr = self.frames.last().context("no frame")?;
        if self.stack.len() > fr.height {
            return Ok(self.stack.pop().expect("a value"));
        }
        // Unreachable code pops what isn't there.
        ensure!(fr.dead, "operand stack underflow");
        Ok(V { n: self.o.nodes.node(), k: None, table: None, rem: None })
    }

    fn kill(&mut self) {
        if let Some(fr) = self.frames.last_mut() {
            self.stack.truncate(fr.height);
            fr.dead = true;
        }
    }

    /// The locals' values flow to `joins`.
    fn join(&mut self, joins: &[u32]) {
        for (l, j) in self.locals.clone().into_iter().zip(joins) {
            self.o.nodes.union(l, *j);
        }
    }

    /// The values a branch to the frame `depth` out carries flow to it,
    /// and the locals to where it arrives.
    fn branch(&mut self, depth: u32) -> Result<()> {
        let k = self.frames.len().checked_sub(1 + depth as usize).context("branch out of range")?;
        if !self.frames.last().is_some_and(|f| f.dead) {
            let joins = self.frames[k].joins.clone();
            self.join(&joins);
        }
        let targets = self.frames[k].branch.clone();
        let mut vals = Vec::new();
        for _ in &targets {
            vals.push(self.pop()?);
        }
        vals.reverse();
        for (v, t) in vals.iter().zip(&targets) {
            self.consume(*v, "a branch value");
            self.o.nodes.union(v.n, *t);
        }
        // br_if leaves them in place.
        self.stack.extend(vals);
        Ok(())
    }

    /// At `else` or `end`: what's on the stack flows to the block's results.
    fn end_values(&mut self) -> Result<()> {
        let fr = self.frames.last().context("no frame")?;
        if fr.dead {
            return Ok(());
        }
        let results = fr.results.clone();
        ensure!(self.stack.len() >= fr.height + results.len(), "a block ends without its results");
        let vals = self.stack.split_off(self.stack.len() - results.len());
        for (v, t) in vals.iter().zip(&results) {
            self.consume(*v, "a block result");
            self.o.nodes.union(v.n, *t);
        }
        Ok(())
    }

    /// A call to function symbol `symbol`: one of the object's helpers
    /// connects its arguments and results; anything else is a guest
    /// boundary.
    fn call(&mut self, symbol: u32, sig: &crate::object::FuncType, facts: &Facts, at: u32, what: &dyn Fn(&str) -> String) -> Result<()> {
        let mut args = Vec::new();
        for _ in &sig.params {
            args.push(self.pop()?);
        }
        args.reverse();
        let callee = self.o.obj.symbols.get(symbol as usize).map_or_else(String::new, |s| s.name.clone());
        let helper = (facts.helper)(self.o.input, symbol);
        if let Some(h) = helper.filter(|&h| self.o.params[h as usize].len() != args.len() || self.o.results[h as usize].len() != sig.results.len()) {
            // Called with another signature: values the analysis can't
            // connect reach it.
            for n in self.o.params[h as usize].clone().into_iter().chain(self.o.results[h as usize].clone()) {
                self.o.nodes.mark(n, Reason::MissingUses, || what("a helper called with another signature"));
            }
        }
        match helper {
            Some(h) if self.o.params[h as usize].len() == args.len() && self.o.results[h as usize].len() == sig.results.len() => {
                for (a, p) in args.iter().zip(self.o.params[h as usize].clone()) {
                    self.consume(*a, "a helper's argument");
                    self.o.nodes.union(a.n, p);
                }
                for n in self.o.results[h as usize].clone() {
                    self.stack.push(V { n, k: None, table: None, rem: None });
                }
            }
            _ => {
                // A lifted function's argument register can carry a handle
                // where the lifter proved what it does with it.
                let transport = (facts.transport)(self.o.input, symbol, sig);
                // The argument the call returns, when it returns one.
                let mut back: Option<u32> = None;
                for (k, a) in args.into_iter().enumerate() {
                    match transport.as_ref().and_then(|t| t.args.get(k).map(|f| (t, f))) {
                        Some((t, ArgFate::Native { reg, jalrs, returned })) => {
                            self.consume(a, "a lifted argument");
                            let pcs: Vec<String> = jalrs.iter().map(|p| format!("0x{p:08X}")).collect();
                            let ret = if *returned { ", returned in v0" } else { "" };
                            let what = what(&format!("a{reg} of {} (its jalrs [{}]{ret})", t.callee, pcs.join(", ")));
                            self.o.carried.push(Carried { at, entry: t.entry, reg: *reg, node: a.n, what });
                            if *returned {
                                back = Some(a.n);
                            }
                        }
                        Some((t, ArgFate::Refused(r, why))) => {
                            self.consume(a, "a lifted argument");
                            let w = what(&format!("passed to {} ({why})", t.callee));
                            self.o.nodes.mark(a.n, *r, || w);
                        }
                        _ => self.escape(a, &what(&format!("passed to {callee}"))),
                    }
                }
                match (back, sig.results.as_slice()) {
                    // v0 is that argument, in either representation.
                    (Some(n), [wasmparser::ValType::I32]) => self.stack.push(V { n, k: None, table: None, rem: None }),
                    _ => {
                        for _ in &sig.results {
                            let w = what(&format!("the result of {callee}"));
                            let n = self.o.nodes.marked(Reason::OpaqueEscape, || w);
                            if let Some(b) = back {
                                self.o.nodes.mark(b, Reason::MissingUses, || format!("{callee} returns it as a result other than one word"));
                            }
                            self.stack.push(V { n, k: None, table: None, rem: None });
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// A use other than as a table's address in a load: the table's cells
    /// are read where nothing here sees.
    fn consume(&mut self, v: V, what: &str) {
        if let Some(t) = v.table {
            self.o.mark_table(t, || format!("the table's address is used as {what}"));
        }
    }

    fn escape(&mut self, v: V, what: &str) {
        self.consume(v, what);
        self.o.nodes.mark(v.n, Reason::OpaqueEscape, || what.to_string());
    }

    fn numeric(&mut self, v: V, what: &str) {
        self.o.nodes.mark(v.n, Reason::NumericObservation, || format!("used in {what}"));
    }

    fn push_number(&mut self, what: &str) {
        let n = self.o.nodes.marked(Reason::NumericObservation, || what.to_string());
        self.stack.push(V { n, k: None, table: None, rem: None });
    }
}

/// Check a plan against its report (tests and debug builds).
pub fn check(plan: &Plan) -> Result<()> {
    for (k, id) in plan.handles.iter().enumerate() {
        if let Identity::Symbol { .. } = id {
            ensure!(plan.helpers.contains_key(id), "handle {} names a helper without a function", k + 1);
        }
    }
    let n = plan.handles.len() as u32;
    if let Some(h) = plan.code.values().chain(plan.data.values()).find(|&&h| h == 0 || h > n) {
        bail!("a site holds handle {h}, outside 1..={n}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use wasmparser::{MemArg, Operator as O};

    /// The default arm of the flow analysis takes only numbers in and out.
    #[test]
    fn only_plain_arithmetic_takes_the_default_arm() {
        let m = MemArg { align: 2, max_align: 2, offset: 0, memory: 0 };
        for op in [O::I32Mul, O::I64Eqz, O::F64ConvertI32S, O::I32WrapI64, O::I32Extend8S, O::I32TruncSatF32S, O::I64Const { value: 1 }] {
            assert!(super::plain(&op), "{op:?}");
        }
        let outside =
            [O::I32AtomicLoad { memarg: m }, O::I32x4Add, O::TableGet { table: 0 }, O::MemoryGrow { mem: 0 }, O::I32Load { memarg: m }, O::ReturnCall { function_index: 0 }, O::RefIsNull];
        for op in outside {
            assert!(!super::plain(&op), "{op:?}");
        }
    }
}
