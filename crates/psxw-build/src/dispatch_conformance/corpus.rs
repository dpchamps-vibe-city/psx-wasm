//! The fixture corpus, D01–D10 (plans/dispatch-conformance.md). Each
//! fixture is authored here as MIPS through [`super::asm`]; its variants
//! apply the name, order, address and stale-memory transformations.
//!
//! Conventions: overlay functions begin with the same two instructions
//! (`addiu sp,sp,-24; move v0,zero`, an authored prologue); each marks its
//! identity in the log and returns it. Marks are closed-form, so every
//! expectation is too; the reference run must agree with them.

use super::asm::*;
use super::fixture::*;

pub const PROLOGUE: [u32; 2] = [0x27BD_FFE8, 0x0000_1021];
/// Bytes an overlay slot spans: what a variant initializes there.
pub const SLOT_BYTES: u32 = 0x400;
/// An opcode the translator can't lift (opcode 3Fh).
const INVALID: u32 = 0xFC00_0000;

/// Append `reg` to the log. Clobbers t8, t9.
fn mark(a: &mut Asm, lay: &Layout, reg: u8) {
    let (hi, lo) = hi_lo(lay.log);
    a.lui(T9, hi).lw(T8, lo as i32, T9).nop().sw(reg, 0, T8).addiu(T8, T8, 4).sw(T8, lo as i32, T9);
}

/// Append `k` to the log. Clobbers t7, t8, t9.
fn mark_k(a: &mut Asm, lay: &Layout, k: u32) {
    a.li(T7, k);
    mark(a, lay, T7);
}

/// A container under construction: its code, symbols and data.
struct Unit {
    id: String,
    a: Asm,
    funcs: Vec<(String, u32, Option<u32>)>,
}

impl Unit {
    fn new(id: &str, base: u32) -> Self {
        Self { id: id.into(), a: Asm::new(base), funcs: Vec::new() }
    }

    /// Start function `name` here (its symbol, and a label of that name).
    fn func(&mut self, name: &str) -> &mut Asm {
        let at = self.a.label(name);
        self.funcs.push((name.into(), at, None));
        &mut self.a
    }

    /// Start function `name` at `at`, padding with zeros up to it.
    fn func_at(&mut self, name: &str, at: u32) -> &mut Asm {
        assert!(self.a.here() <= at, "{}: {name} at {at:#x} overlaps the code before it", self.id);
        self.a.pad_to(at);
        self.func(name)
    }

    /// The image: the code, then `data` (after padding the text to
    /// `text_end` when given).
    fn image(mut self, text_end: Option<u32>, data: &[u8]) -> Image {
        if let Some(e) = text_end {
            assert!(self.a.here() <= e, "{}: code runs past its text end {e:#x}", self.id);
            self.a.pad_to(e);
        }
        let mut bytes = self.a.bytes().expect("fixture assembles");
        let text = bytes.len() as u32;
        bytes.extend_from_slice(data);
        let base = self.a.here() - text;
        Image { id: self.id, base, bytes, text, funcs: self.funcs, c: None }
    }
}

fn words(ws: &[u32]) -> Vec<u8> {
    ws.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// The main executable: the driver functions every fixture calls, then
/// whatever `extra` adds.
fn main_image(lay: &Layout, extra: impl FnOnce(&mut Unit)) -> Image {
    let mut u = Unit::new("main", lay.main);
    // call_slot(a0..a3): the slot's function, by a direct call.
    u.func("call_slot").addiu(SP, SP, -24).sw(RA, 16, SP).jal_abs(lay.slot).nop().lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
    // call_ptr(fp, a1..a3 → a0..a2): through a function pointer.
    u.func("call_ptr").addiu(SP, SP, -24).sw(RA, 16, SP).mov(T0, A0).mov(A0, A1).mov(A1, A2).jalr(T0).mov(A2, A3);
    u.a.lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
    // call_tail(a0..a3): a tail jump into the slot.
    u.func("call_tail").j_abs(lay.slot).nop();
    // flush(): FlushCache, the defined way to make written code executable.
    u.func("flush").ori(T2, ZERO, 0xA0).jr(T2).ori(T1, ZERO, 0x44);
    // copy(dst, src, bytes): a word copy loop, then flush.
    u.func("copy");
    u.a.label("copy_loop");
    u.a.lw(T0, 0, A1).addiu(A1, A1, 4).sw(T0, 0, A0).addiu(A2, A2, -4).bne(A2, ZERO, "copy_loop").addiu(A0, A0, 4);
    u.a.j("flush").nop();
    // unpack(dst, src, words, key): xor-decode, then flush.
    u.func("unpack");
    u.a.label("unpack_loop");
    u.a.lw(T0, 0, A1).addiu(A1, A1, 4).xor(T0, T0, A3).sw(T0, 0, A0).addiu(A2, A2, -1).bne(A2, ZERO, "unpack_loop").addiu(A0, A0, 4);
    u.a.j("flush").nop();
    // poke(addr, word): one guest store, flush, return 0.
    u.func("poke").addiu(SP, SP, -24).sw(RA, 16, SP).sw(A1, 0, A0).jal("flush").nop().lw(RA, 16, SP).ori(V0, ZERO, 0).jr(RA).addiu(SP, SP, 24);
    extra(&mut u);
    u.image(None, &[])
}

/// An overlay function at the unit's current address: the prologue, then
/// `body`, then the epilogue (`v0` is the body's).
fn leaf(u: &mut Unit, name: &str, body: impl FnOnce(&mut Asm)) {
    let a = u.func(name);
    for w in PROLOGUE {
        a.word(w);
    }
    body(a);
    a.jr(RA).addiu(SP, SP, 24);
}

/// `v0 = k`, marked.
fn ret_k(a: &mut Asm, lay: &Layout, k: u32) {
    a.ori(V0, ZERO, k);
    mark(a, lay, V0);
}

/// The steps of a variant, with the cumulative log it expects.
#[derive(Default)]
struct Script {
    steps: Vec<Step>,
    log: Vec<u32>,
}

impl Script {
    fn load(&mut self, im: &Image) -> &mut Self {
        self.steps.push(Step::Write { at: im.base, bytes: im.bytes.clone() });
        self
    }
    fn write(&mut self, at: u32, bytes: Vec<u8>) -> &mut Self {
        self.steps.push(Step::Write { at, bytes });
        self
    }
    /// A call returning `v0` after logging `marks`.
    fn call(&mut self, entry: u32, args: [u32; 4], v0: u32, marks: &[u32]) -> &mut Self {
        self.log.extend_from_slice(marks);
        self.steps.push(Step::Call { entry, args, expect: Expect::v0(v0, &self.log) });
        self
    }
    /// A call whose return value isn't defined (a driver tail-calling
    /// FlushCache).
    fn run(&mut self, entry: u32, args: [u32; 4], marks: &[u32]) -> &mut Self {
        self.log.extend_from_slice(marks);
        self.steps.push(Step::Call { entry, args, expect: Expect::Value { v0: None, v1: None, log: Some(self.log.clone()) } });
        self
    }
    fn reject(&mut self, entry: u32, args: [u32; 4], diag: String) -> &mut Self {
        self.steps.push(Step::Call { entry, args, expect: Expect::reject(diag, &self.log) });
        self
    }
    fn untranslated(&mut self, entry: u32, args: [u32; 4], at: u32) -> &mut Self {
        self.reject(entry, args, format!("untranslated 0x{at:08X}"))
    }
}

/// Container names for a fixture's roles (`a`, `b`, ...).
#[derive(Clone)]
struct Names(Vec<(&'static str, String)>);

impl Names {
    fn of(&self, role: &str) -> String {
        self.0.iter().find(|(r, _)| *r == role).map_or_else(|| format!("ovl_{role}"), |(_, n)| n.clone())
    }
}

/// How a variant transforms its fixture.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tf {
    Base,
    /// Containers named the other way round: name order can't choose.
    SwapNames,
    /// The address map lists the containers in reverse.
    Reorder,
    /// Every address moves (a legal load address).
    Shift,
    /// Memory nothing has loaded yet holds seeded garbage, not zeros.
    Stale,
}

impl Tf {
    fn suffix(self) -> &'static str {
        match self {
            Tf::Base => "",
            Tf::SwapNames => "@swap-names",
            Tf::Reorder => "@reorder",
            Tf::Shift => "@shift",
            Tf::Stale => "@stale",
        }
    }
}

const ALL: &[Tf] = &[Tf::Base, Tf::SwapNames, Tf::Reorder, Tf::Shift, Tf::Stale];
const BASE: &[Tf] = &[Tf::Base];

/// Deterministic bytes (splitmix64).
fn garbage(seed: u64, n: usize) -> Vec<u8> {
    let mut s = seed;
    (0..n.div_ceil(8))
        .flat_map(|_| {
            s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = s;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            (z ^ (z >> 31)).to_le_bytes()
        })
        .take(n)
        .collect()
}

fn seed_of(id: &str) -> u64 {
    id.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// What a fixture builds for a layout and naming.
struct Fixture {
    images: Vec<Image>,
    init: Vec<(u32, Vec<u8>)>,
    script: Script,
    build: Build,
    threads: bool,
    dispatch: Option<String>,
}

impl Fixture {
    fn new(images: Vec<Image>, script: Script) -> Self {
        Self { images, init: Vec::new(), script, build: Build::Links, threads: false, dispatch: None }
    }
}

struct Def {
    case: &'static str,
    name: &'static str,
    note: &'static str,
    evidence: Evidence,
    tfs: &'static [Tf],
    make: fn(&Layout, &Names) -> Fixture,
    review_required: Option<&'static str>,
}

fn expand(d: &Def) -> Vec<Variant> {
    d.tfs
        .iter()
        .map(|&tf| {
            let lay = Layout::at(if tf == Tf::Shift { 0x8000 } else { 0 });
            let names = match tf {
                Tf::SwapNames => Names(vec![("a", "ovl_b".into()), ("b", "ovl_a".into()), ("a2", "ovl_b2".into()), ("b2", "ovl_a2".into())]),
                _ => Names(vec![]),
            };
            let mut f = (d.make)(&lay, &names);
            if tf == Tf::Reorder {
                f.images.reverse();
            }
            let name = format!("{}{}", d.name, tf.suffix());
            let seed = seed_of(&format!("{}/{name}", d.case));
            // The slots' contents before anything loads: zeros, or garbage.
            let fill = |k: u64| if tf == Tf::Stale { garbage(seed ^ k, SLOT_BYTES as usize) } else { vec![0; SLOT_BYTES as usize] };
            let mut init = vec![(lay.slot, fill(1)), (lay.slot2, fill(2))];
            init.extend(f.init);
            Variant {
                case: d.case,
                name,
                seed,
                note: d.note,
                evidence: d.evidence.clone(),
                layout: lay,
                main: "main".into(),
                images: f.images,
                init,
                steps: f.script.steps,
                build: f.build,
                threads: f.threads,
                review_required: d.review_required,
                dispatch: f.dispatch,
            }
        })
        .collect()
}

// ---- D01: prefix collisions --------------------------------------------

/// An overlay whose slot function runs `common` filler words after the
/// prologue, then `ori v0, zero, k` (its first own word), marked.
fn d01_overlay(lay: &Layout, id: String, filler: usize, k: u32) -> Image {
    let mut u = Unit::new(&id, lay.slot);
    leaf(&mut u, "f", |a| {
        for _ in 0..filler {
            a.addiu(T0, ZERO, 7);
        }
        ret_k(a, lay, k);
    });
    u.image(Some(lay.slot + 0x80), &[])
}

fn d01_history(lay: &Layout, main: &Image, ovls: &[(&Image, u32)]) -> Script {
    let mut s = Script::default();
    let (slot, ptr) = (main.addr("call_slot"), main.addr("call_ptr"));
    let order: Vec<usize> = (0..ovls.len()).chain([0]).collect();
    for i in order {
        let (im, k) = ovls[i];
        s.load(im).call(slot, [0; 4], k, &[k]).call(ptr, [lay.slot, 0, 0, 0], k, &[k]);
    }
    s
}

fn d01_prefix(lay: &Layout, n: &Names, filler: usize, count: usize) -> Fixture {
    let main = main_image(lay, |_| {});
    let roles = ["a", "b", "c"];
    let ovls: Vec<(Image, u32)> =
        (0..count).map(|i| (d01_overlay(lay, n.of(roles[i]), filler, 0xA1 + 0x10 * i as u32), 0xA1 + 0x10 * i as u32)).collect();
    let refs: Vec<(&Image, u32)> = ovls.iter().map(|(im, k)| (im, *k)).collect();
    let s = d01_history(lay, &main, &refs);
    let mut images = vec![main];
    images.extend(ovls.into_iter().map(|(im, _)| im));
    Fixture::new(images, s)
}

fn d01_late(lay: &Layout, n: &Names) -> Fixture {
    let main = main_image(lay, |_| {});
    let ovl = |role: &str, k: u32| {
        let mut u = Unit::new(&n.of(role), lay.slot);
        let a = u.func("f");
        for w in PROLOGUE {
            a.word(w);
        }
        mark_k(a, lay, 0xF0);
        for _ in 0..6 {
            a.addiu(T0, ZERO, 7);
        }
        // The only difference is the function's last word.
        a.addiu(SP, SP, 24).jr(RA).ori(V0, ZERO, k);
        u.image(Some(lay.slot + 0x80), &[])
    };
    let (a, b) = (ovl("a", 0xA1), ovl("b", 0xB1));
    let mut s = Script::default();
    let slot = main.addr("call_slot");
    s.load(&a).call(slot, [0; 4], 0xA1, &[0xF0]).load(&b).call(slot, [0; 4], 0xB1, &[0xF0]).load(&a).call(slot, [0; 4], 0xA1, &[0xF0]);
    Fixture::new(vec![main, a, b], s)
}

/// Identical slot functions; the overlays differ in a second function.
fn d01_equivalent(lay: &Layout, n: &Names) -> Fixture {
    let main = main_image(lay, |_| {});
    let ovl = |role: &str, k: u32| {
        let mut u = Unit::new(&n.of(role), lay.slot);
        leaf(&mut u, "f", |a| ret_k(a, lay, 0x99));
        u.a.pad_to(lay.slot + 0x40);
        leaf(&mut u, "g", |a| ret_k(a, lay, k));
        u.image(Some(lay.slot + 0x80), &[])
    };
    let (a, b) = (ovl("a", 0xA2), ovl("b", 0xB2));
    let mut s = Script::default();
    let (slot, ptr) = (main.addr("call_slot"), main.addr("call_ptr"));
    for (im, k) in [(&a, 0xA2), (&b, 0xB2), (&a, 0xA2)] {
        s.load(im).call(slot, [0; 4], 0x99, &[0x99]).call(ptr, [lay.slot + 0x40, 0, 0, 0], k, &[k]);
    }
    Fixture::new(vec![main, a, b], s)
}

// ---- D02: translation dependencies -------------------------------------

/// The slot function calls a second function at slot+0x40 (`jal`); the
/// overlays' callees differ, their callers' words don't.
fn d02_callee(lay: &Layout, n: &Names) -> Fixture {
    let main = main_image(lay, |_| {});
    let ovl = |role: &str, k: u32| {
        let mut u = Unit::new(&n.of(role), lay.slot);
        leaf(&mut u, "f", |a| {
            a.sw(RA, 16, SP).jal_abs(lay.slot + 0x40).nop().lw(RA, 16, SP).nop();
        });
        u.func_at("g", lay.slot + 0x40);
        u.a.ori(V0, ZERO, k);
        mark(&mut u.a, lay, V0);
        u.a.jr(RA).nop();
        u.image(Some(lay.slot + 0x80), &[])
    };
    let (a, b) = (ovl("a", 0xA2), ovl("b", 0xB2));
    let mut s = Script::default();
    let slot = main.addr("call_slot");
    s.load(&a).call(slot, [0; 4], 0xA2, &[0xA2]).load(&b).call(slot, [0; 4], 0xB2, &[0xB2]).load(&a).call(slot, [0; 4], 0xA2, &[0xA2]);
    Fixture::new(vec![main, a, b], s)
}

/// The slot function passes t0 to a helper that reads it on entry: the
/// translator expands the helper into it. The overlays' helpers differ.
fn d02_inlined(lay: &Layout, n: &Names) -> Fixture {
    let main = main_image(lay, |_| {});
    let ovl = |role: &str, k: i32| {
        let mut u = Unit::new(&n.of(role), lay.slot);
        leaf(&mut u, "f", |a| {
            a.sw(RA, 16, SP).ori(T0, ZERO, 5).jal_abs(lay.slot + 0x40).nop().lw(RA, 16, SP).nop();
        });
        u.func_at("h", lay.slot + 0x40);
        u.a.addiu(V0, T0, k);
        mark(&mut u.a, lay, V0);
        u.a.jr(RA).nop();
        u.image(Some(lay.slot + 0x80), &[])
    };
    let (a, b) = (ovl("a", 0x100), ovl("b", 0x200));
    let mut s = Script::default();
    let slot = main.addr("call_slot");
    s.load(&a).call(slot, [0; 4], 0x105, &[0x105]).load(&b).call(slot, [0; 4], 0x205, &[0x205]).load(&a).call(slot, [0; 4], 0x105, &[0x105]);
    Fixture::new(vec![main, a, b], s)
}

/// The slot function switches on a0 through GCC's jump table idiom; the
/// table is in the overlay's data. The code is identical in both
/// overlays; `table` picks each one's targets by case index.
fn d02_table(lay: &Layout, n: &Names, tables: [[usize; 4]; 2], expect: [[u32; 2]; 2]) -> Fixture {
    let main = main_image(lay, |_| {});
    let table_at = lay.slot + 0x100;
    let ovl = |role: &str, order: [usize; 4]| {
        let mut u = Unit::new(&n.of(role), lay.slot);
        let (hi, lo) = hi_lo(table_at);
        leaf(&mut u, "f", |a| {
            a.sltiu(V0, A0, 4).beq(V0, ZERO, "dflt").sll(V0, A0, 2).lui(AT, hi).addu(AT, AT, V0).lw(V0, lo as i32, AT).nop().jr(V0).nop();
            for c in 0..4 {
                a.label(&format!("case{c}"));
                a.b("done").ori(V0, ZERO, 0x10 + c);
            }
            a.label("dflt");
            a.ori(V0, ZERO, 0x99);
            a.label("done");
            mark(a, lay, V0);
        });
        let case = |c: usize| u.a.addr_of(&format!("case{c}")).expect("case label");
        let table: Vec<u32> = order.iter().map(|&c| case(c)).collect();
        u.image(Some(table_at), &words(&table))
    };
    let (a, b) = (ovl("a", tables[0]), ovl("b", tables[1]));
    let mut s = Script::default();
    let slot = main.addr("call_slot");
    for (im, e) in [(&a, expect[0]), (&b, expect[1]), (&a, expect[0])] {
        s.load(im).call(slot, [0, 0, 0, 0], e[0], &[e[0]]).call(slot, [1, 0, 0, 0], e[1], &[e[1]]);
    }
    Fixture::new(vec![main, a, b], s)
}

fn d02_table_permuted(lay: &Layout, n: &Names) -> Fixture {
    d02_table(lay, n, [[0, 1, 2, 3], [3, 2, 1, 0]], [[0x10, 0x11], [0x13, 0x12]])
}

fn d02_table_disjoint(lay: &Layout, n: &Names) -> Fixture {
    d02_table(lay, n, [[0, 1, 0, 1], [2, 3, 2, 3]], [[0x10, 0x11], [0x12, 0x13]])
}

/// The slot function reads a variable in its overlay's data. Writing it
/// changes the result, not which code is resident.
fn d02_data(lay: &Layout, n: &Names) -> Fixture {
    let main = main_image(lay, |_| {});
    let var = lay.slot + 0x100;
    let ovl = |role: &str, k: i32, init: u32| {
        let mut u = Unit::new(&n.of(role), lay.slot);
        let (hi, lo) = hi_lo(var);
        leaf(&mut u, "f", |a| {
            a.lui(T0, hi).lw(V0, lo as i32, T0).nop().addiu(V0, V0, k);
            mark(a, lay, V0);
        });
        u.image(Some(var), &words(&[init]))
    };
    let (a, b) = (ovl("a", 0x100, 0x40), ovl("b", 0x200, 0x60));
    let mut s = Script::default();
    let slot = main.addr("call_slot");
    s.load(&a).call(slot, [0; 4], 0x140, &[0x140]).write(var, words(&[0x50])).call(slot, [0; 4], 0x150, &[0x150]);
    s.load(&b).call(slot, [0; 4], 0x260, &[0x260]).write(var, words(&[0x50])).call(slot, [0; 4], 0x250, &[0x250]);
    Fixture::new(vec![main, a, b], s)
}

// ---- D03: overlapping layouts ------------------------------------------

/// A long overlay and a short one sharing the slot function's prologue.
fn d03_lengths(lay: &Layout, n: &Names) -> Fixture {
    let main = main_image(lay, |_| {});
    let mut u = Unit::new(&n.of("a"), lay.slot);
    leaf(&mut u, "f", |a| ret_k(a, lay, 0xA1));
    u.a.pad_to(lay.slot + 0x80);
    leaf(&mut u, "g", |a| ret_k(a, lay, 0xA2));
    let a = u.image(Some(lay.slot + 0x100), &[]);
    let mut u = Unit::new(&n.of("b"), lay.slot);
    leaf(&mut u, "f", |a| ret_k(a, lay, 0xB1));
    let b = u.image(Some(lay.slot + 0x40), &[]);
    let mut s = Script::default();
    let slot = main.addr("call_slot");
    s.load(&a).call(slot, [0; 4], 0xA1, &[0xA1]).load(&b).call(slot, [0; 4], 0xB1, &[0xB1]).load(&a).call(slot, [0; 4], 0xA1, &[0xA1]);
    Fixture::new(vec![main, a, b], s)
}

/// Overlay a's second function starts where overlay b's first is still
/// running: an entry only one image has.
fn d03_boundaries_images(lay: &Layout, n: &Names) -> (Image, Image, Image) {
    let main = main_image(lay, |_| {});
    let mut u = Unit::new(&n.of("a"), lay.slot);
    leaf(&mut u, "f", |a| ret_k(a, lay, 0xA1));
    u.func_at("g", lay.slot + 0x40);
    u.a.ori(V0, ZERO, 0xA2);
    mark(&mut u.a, lay, V0);
    u.a.jr(RA).nop();
    let a = u.image(Some(lay.slot + 0x80), &[]);
    let mut u = Unit::new(&n.of("b"), lay.slot);
    leaf(&mut u, "f", |a| {
        for _ in 0..8 {
            a.addiu(T0, ZERO, 7);
        }
        ret_k(a, lay, 0xB1);
    });
    let b = u.image(Some(lay.slot + 0x80), &[]);
    (main, a, b)
}

fn d03_boundaries(lay: &Layout, n: &Names) -> Fixture {
    let (main, a, b) = d03_boundaries_images(lay, n);
    let mut s = Script::default();
    let ptr = main.addr("call_ptr");
    let g = lay.slot + 0x40;
    s.load(&a).call(ptr, [g, 0, 0, 0], 0xA2, &[0xA2]).load(&b).call(main.addr("call_slot"), [0; 4], 0xB1, &[0xB1]);
    s.untranslated(ptr, [g, 0, 0, 0], g);
    Fixture::new(vec![main, a, b], s)
}

/// Main's own code at an address an overlay is later loaded over.
fn d03_main_overlap(lay: &Layout, n: &Names, direct: bool) -> Fixture {
    let at = lay.main + 0x400;
    let main = main_image(lay, |u| {
        u.func("call_m").addiu(SP, SP, -24).sw(RA, 16, SP).jal("m").nop().lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
        u.a.pad_to(at);
        leaf(u, "m", |a| ret_k(a, lay, 0x31));
        u.a.pad_to(at + 0x40);
    });
    let mut u = Unit::new(&n.of("a"), at);
    leaf(&mut u, "o", |a| ret_k(a, lay, 0x41));
    let o = u.image(Some(at + 0x40), &[]);
    let mut s = Script::default();
    let (entry, args) = if direct { (main.addr("call_m"), [0; 4]) } else { (main.addr("call_ptr"), [at, 0, 0, 0]) };
    s.call(entry, args, 0x31, &[0x31]).load(&o).call(entry, args, 0x41, &[0x41]);
    Fixture::new(vec![main, o], s)
}

fn d03_main_overlap_direct(lay: &Layout, n: &Names) -> Fixture {
    d03_main_overlap(lay, n, true)
}

fn d03_main_overlap_pointer(lay: &Layout, n: &Names) -> Fixture {
    d03_main_overlap(lay, n, false)
}

/// Two slots, each with two candidates; the first slot's function calls
/// into the second.
fn d03_two_regions(lay: &Layout, n: &Names) -> Fixture {
    let main = main_image(lay, |_| {});
    let first = |role: &str, k: i32| {
        let mut u = Unit::new(&n.of(role), lay.slot);
        leaf(&mut u, "f", |a| {
            a.sw(RA, 16, SP).jal_abs(lay.slot2).nop().lw(RA, 16, SP).addiu(V0, V0, k);
            mark(a, lay, V0);
        });
        u.image(Some(lay.slot + 0x80), &[])
    };
    let second = |role: &str, k: u32| {
        let mut u = Unit::new(&n.of(role), lay.slot2);
        leaf(&mut u, "g", |a| ret_k(a, lay, k));
        u.image(Some(lay.slot2 + 0x80), &[])
    };
    let (a1, b1, a2, b2) = (first("a", 0x100), first("b", 0x200), second("a2", 0x2A), second("b2", 0x2B));
    let mut s = Script::default();
    let slot = main.addr("call_slot");
    s.load(&a1).load(&a2).call(slot, [0; 4], 0x12A, &[0x2A, 0x12A]);
    s.load(&b2).call(slot, [0; 4], 0x12B, &[0x2B, 0x12B]);
    s.load(&b1).call(slot, [0; 4], 0x22B, &[0x2B, 0x22B]);
    s.load(&a2).call(slot, [0; 4], 0x22A, &[0x2A, 0x22A]);
    Fixture::new(vec![main, a1, b1, a2, b2], s)
}

// ---- D04: replacement history ------------------------------------------

/// A long overlay whose tail function calls the slot function; a short
/// one loaded over it leaves that tail intact.
fn d04_stale_tail(lay: &Layout, n: &Names) -> Fixture {
    let main = main_image(lay, |_| {});
    let mut u = Unit::new(&n.of("a"), lay.slot);
    leaf(&mut u, "f", |a| ret_k(a, lay, 0xA1));
    u.func_at("k", lay.slot + 0x80);
    u.a.addiu(SP, SP, -24).sw(RA, 16, SP).jal_abs(lay.slot).nop().lw(RA, 16, SP).addiu(V0, V0, 0x1000);
    mark(&mut u.a, lay, V0);
    u.a.jr(RA).addiu(SP, SP, 24);
    let a = u.image(Some(lay.slot + 0x100), &[]);
    let mut u = Unit::new(&n.of("b"), lay.slot);
    leaf(&mut u, "f", |a| ret_k(a, lay, 0xB1));
    let b = u.image(Some(lay.slot + 0x40), &[]);
    let mut s = Script::default();
    let ptr = main.addr("call_ptr");
    let k = lay.slot + 0x80;
    s.load(&a).call(ptr, [k, 0, 0, 0], 0x10A1, &[0xA1, 0x10A1]);
    s.load(&b).call(ptr, [k, 0, 0, 0], 0x10B1, &[0xB1, 0x10B1]);
    Fixture::new(vec![main, a, b], s)
}

/// Two overlays each with a short function `g` and a long `f` whose
/// first and last parts both differ.
fn d04_chunk_images(lay: &Layout, n: &Names) -> (Image, Image, Image) {
    let main = main_image(lay, |_| {});
    let ovl = |role: &str, early: u32, late: i32, gk: u32| {
        let mut u = Unit::new(&n.of(role), lay.slot);
        leaf(&mut u, "g", |a| ret_k(a, lay, gk));
        u.func_at("f", lay.slot + 0x30);
        u.a.addiu(SP, SP, -24).ori(V1, ZERO, early);
        for _ in 0..16 {
            u.a.addiu(T0, ZERO, 7);
        }
        u.a.addiu(V1, V1, late);
        mark(&mut u.a, lay, V1);
        u.a.mov(V0, V1).jr(RA).addiu(SP, SP, 24);
        u.image(Some(lay.slot + 0x100), &[])
    };
    (main, ovl("a", 0xA00, 0xA, 0xA1), ovl("b", 0xB00, 0xB, 0xB1))
}

/// b loaded over a in two chunks; calls at the intermediate state into
/// what is complete, then into the rest once it is.
fn d04_chunked(lay: &Layout, n: &Names) -> Fixture {
    let (main, a, b) = d04_chunk_images(lay, n);
    let mut s = Script::default();
    let ptr = main.addr("call_ptr");
    let (g, f) = (lay.slot, lay.slot + 0x30);
    s.load(&a).call(ptr, [f, 0, 0, 0], 0xA0A, &[0xA0A]);
    s.write(lay.slot, b.bytes[..0x40].to_vec()).call(ptr, [g, 0, 0, 0], 0xB1, &[0xB1]);
    s.write(lay.slot + 0x40, b.bytes[0x40..].to_vec()).call(ptr, [f, 0, 0, 0], 0xB0B, &[0xB0B]);
    Fixture::new(vec![main, a, b], s)
}

/// The same, but `f` is called while half of it is b's and half a's.
fn d04_chunked_mixed(lay: &Layout, n: &Names) -> Fixture {
    let (main, a, b) = d04_chunk_images(lay, n);
    let mut s = Script::default();
    let ptr = main.addr("call_ptr");
    let (g, f) = (lay.slot, lay.slot + 0x30);
    s.load(&a).write(lay.slot, b.bytes[..0x40].to_vec()).call(ptr, [g, 0, 0, 0], 0xB1, &[0xB1]);
    s.untranslated(ptr, [f, 0, 0, 0], f);
    Fixture::new(vec![main, a, b], s)
}

/// a resident, then one of its late instructions changed by a guest
/// store: code no translation was made from.
fn d04_patched(lay: &Layout, n: &Names) -> Fixture {
    let (main, a, b) = d04_chunk_images(lay, n);
    let mut s = Script::default();
    let ptr = main.addr("call_ptr");
    let f = lay.slot + 0x30;
    // `addiu v1, v1, 0xA` is f's 19th word, in the second chunk.
    let at = f + 4 * 18;
    s.load(&a).call(ptr, [f, 0, 0, 0], 0xA0A, &[0xA0A]);
    s.call(main.addr("poke"), [at, 0x2463_0077, 0, 0], 0, &[]);
    s.untranslated(ptr, [f, 0, 0, 0], f);
    Fixture::new(vec![main, a, b], s)
}

fn d04_history(lay: &Layout, n: &Names) -> Fixture {
    let main = main_image(lay, |_| {});
    let (a, b) = (d01_overlay(lay, n.of("a"), 0, 0xA1), d01_overlay(lay, n.of("b"), 0, 0xB1));
    let mut s = Script::default();
    let slot = main.addr("call_slot");
    for (im, k) in [(&a, 0xA1), (&b, 0xB1), (&a, 0xA1), (&b, 0xB1), (&b, 0xB1), (&a, 0xA1)] {
        s.load(im).call(slot, [0; 4], k, &[k]);
    }
    Fixture::new(vec![main, a, b], s)
}

// ---- D05: arrival mechanism --------------------------------------------

#[derive(Clone, Copy)]
enum Arrive {
    Host,
    Stores,
    Copy,
    Unpack,
}

const KEY: u32 = 0x5A5A_1234;

fn d05(lay: &Layout, n: &Names, how: Arrive) -> Fixture {
    let (a, b) = (d01_overlay(lay, n.of("a"), 4, 0xA1), d01_overlay(lay, n.of("b"), 4, 0xB1));
    let text = |im: &Image| -> Vec<u32> { im.bytes.chunks(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect() };
    let (wa, wb) = (text(&a), text(&b));
    // Guest stores: a function per overlay writing its words to a0.
    let main = main_image(lay, |u| {
        for (name, ws) in [("store_a", &wa), ("store_b", &wb)] {
            u.func(name);
            for (i, w) in ws.iter().enumerate() {
                u.a.li(T0, *w).sw(T0, 4 * i as i32, A0);
            }
            u.a.j("flush").nop();
        }
    });
    let stage_b = lay.stage + 0x400;
    let packed = |ws: &[u32]| words(&ws.iter().map(|w| w ^ KEY).collect::<Vec<_>>());
    let init = match how {
        Arrive::Copy => vec![(lay.stage, a.bytes.clone()), (stage_b, b.bytes.clone())],
        Arrive::Unpack => vec![(lay.stage, packed(&wa)), (stage_b, packed(&wb))],
        _ => vec![],
    };
    let mut s = Script::default();
    let (slot, ptr) = (main.addr("call_slot"), main.addr("call_ptr"));
    for (im, k, stage, store) in [(&a, 0xA1, lay.stage, "store_a"), (&b, 0xB1, stage_b, "store_b"), (&a, 0xA1, lay.stage, "store_a")] {
        let len = im.bytes.len() as u32;
        match how {
            Arrive::Host => {
                s.load(im);
            }
            Arrive::Stores => {
                s.run(main.addr(store), [lay.slot, 0, 0, 0], &[]);
            }
            Arrive::Copy => {
                s.run(main.addr("copy"), [lay.slot, stage, len, 0], &[]);
            }
            Arrive::Unpack => {
                s.run(main.addr("unpack"), [lay.slot, stage, len / 4, KEY], &[]);
            }
        }
        s.call(slot, [0; 4], k, &[k]).call(ptr, [lay.slot, 0, 0, 0], k, &[k]);
    }
    let mut f = Fixture::new(vec![main, a, b], s);
    f.init = init;
    f
}

fn d05_host(lay: &Layout, n: &Names) -> Fixture {
    d05(lay, n, Arrive::Host)
}
fn d05_stores(lay: &Layout, n: &Names) -> Fixture {
    d05(lay, n, Arrive::Stores)
}
fn d05_copy(lay: &Layout, n: &Names) -> Fixture {
    d05(lay, n, Arrive::Copy)
}
fn d05_unpack(lay: &Layout, n: &Names) -> Fixture {
    d05(lay, n, Arrive::Unpack)
}

// ---- D05 explicit binding: real loading operations publish -------------

/// Modules staged for the resident loader, at `stage + 0x400 * selector`.
const STAGED: u32 = 3;
/// The edited C's extra log entry: an effect the original binary lacks.
const EDIT_MARK: u32 = 0xED17;
/// `load_module`'s store into the destination, from its entry.
const LM_STORE: u32 = 0x2C;

/// How the executable's loader and drivers are implemented.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Loader {
    /// All lifted MIPS.
    Lifted,
    /// Matching C for the loader and two drivers; the rest lifted.
    MatchingC,
    /// The loader's C deliberately logs [`EDIT_MARK`] after loading.
    EditedC,
    /// Matching C that calls its `c_marker` site after the copy.
    MarkerC,
    /// The marker call is in an unrelated helper, not the loader.
    MarkerElsewhere,
    /// The marker passes source offset 4 where the placement maps 0.
    MarkerBadOffset,
}

/// The executable: a resident loader, `load_module(sel, dst, len)`, that
/// copies staged module `sel` to `dst` (its store at [`LM_STORE`]) and
/// returns 0, or returns 1 before writing anything when `sel` names no
/// staged module; `load_c`, the same through a call; `clear(dst, len)`,
/// which zeroes `len` bytes; a saved function pointer; drivers into both
/// regions.
///
/// The C variants write the loader `inline`, taking its request as a
/// struct by value (the same three o32 words), and call it from `load_c`:
/// a declared operation must keep its boundary and read its arguments as
/// they were at entry.
fn explicit_main(lay: &Layout, loader: Loader) -> Image {
    let saved = lay.stage + 0x1000;
    let mut main = main_image(lay, |u| {
        let a = u.func("load_module");
        a.addiu(SP, SP, -24).sw(RA, 16, SP).sltiu(T0, A0, STAGED as i32).beq(T0, ZERO, "lm_fail").nop();
        a.sll(T0, A0, 10).li(T1, lay.stage).addu(T1, T1, T0);
        a.label("lm_loop");
        a.lw(T0, 0, T1).addiu(T1, T1, 4).sw(T0, 0, A1).addiu(A2, A2, -4).bne(A2, ZERO, "lm_loop").addiu(A1, A1, 4);
        a.jal("flush").nop().lw(RA, 16, SP).ori(V0, ZERO, 0).jr(RA).addiu(SP, SP, 24);
        a.label("lm_fail");
        a.lw(RA, 16, SP).ori(V0, ZERO, 1).jr(RA).addiu(SP, SP, 24);
        // save_ptr(p): a function pointer kept in data.
        u.func("save_ptr").li(T0, saved).sw(A0, 0, T0).jr(RA).ori(V0, ZERO, 0);
        // call_saved(a): log the saved pointer, then call it.
        let a = u.func("call_saved");
        a.addiu(SP, SP, -24).sw(RA, 16, SP).li(T0, saved).lw(T1, 0, T0).nop();
        mark(a, lay, T1);
        a.jalr(T1).nop().lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
        u.func("call_slot2").addiu(SP, SP, -24).sw(RA, 16, SP).jal_abs(lay.slot2).nop().lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
        u.func("drive_c").addiu(SP, SP, -24).sw(RA, 16, SP).jal_abs(lay.slot).nop().lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
        u.func("drive_cp").addiu(SP, SP, -24).sw(RA, 16, SP).mov(T0, A0).jalr(T0).mov(A0, A1);
        u.a.lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
        u.func("load_c").j("load_module").nop();
        u.func("clear").beq(A1, ZERO, "clr_done").nop();
        u.a.label("clr_loop");
        u.a.sw(ZERO, 0, A0).addiu(A1, A1, -4).bne(A1, ZERO, "clr_loop").addiu(A0, A0, 4);
        u.a.label("clr_done");
        u.a.jr(RA).ori(V0, ZERO, 0);
    });
    if loader != Loader::Lifted {
        let edit = match loader {
            // An effect the binary lacks, and the request overwritten after
            // the copy: the rule still reads the selector passed in.
            Loader::EditedC => format!(
                "    {{ unsigned **log = (unsigned **)0x{:08X}u; *(*log)++ = 0x{EDIT_MARK:X}u; }}\n    {{ volatile unsigned *v = &r.sel; *v = 99u; }}\n",
                lay.log
            ),
            Loader::MarkerC => "    psxw_binding_site_module_written((unsigned)r.dst, (unsigned)r.len, 0u, r.sel, 0u);\n".to_string(),
            Loader::MarkerBadOffset => "    psxw_binding_site_module_written((unsigned)r.dst, (unsigned)r.len, 4u, r.sel, 0u);\n".to_string(),
            _ => String::new(),
        };
        let marked = matches!(loader, Loader::MarkerC | Loader::MarkerElsewhere | Loader::MarkerBadOffset);
        let include = if marked { "#include \"psxw_binding.h\"\n" } else { "" };
        let helper = if loader == Loader::MarkerElsewhere {
            "void unused_helper(void) { psxw_binding_site_module_written(0u, 0u, 0u, 0u, 0u); }\n"
        } else {
            ""
        };
        main.c = Some(format!(
            "{include}void flush(void);\n\
             int func_{slot:08X}(int a);\n\
             struct load_req {{ unsigned sel; unsigned *dst; int len; }};\n\
             inline int load_module(struct load_req r) {{\n\
             \x20   unsigned *src, *to = r.dst;\n\
             \x20   int n = r.len;\n\
             \x20   if (r.sel >= {STAGED}u) return 1;\n\
             \x20   src = (unsigned *)(0x{stage:08X}u + r.sel * 0x400u);\n\
             \x20   do {{ *to++ = *src++; n -= 4; }} while (n != 0);\n\
             {edit}    flush();\n\
             \x20   return 0;\n\
             }}\n\
             int load_c(unsigned sel, unsigned *dst, int len) {{\n\
             \x20   struct load_req r;\n\
             \x20   r.sel = sel;\n\
             \x20   r.dst = dst;\n\
             \x20   r.len = len;\n\
             \x20   return load_module(r);\n\
             }}\n\
             int drive_c(int a) {{ return func_{slot:08X}(a); }}\n\
             int drive_cp(int (*f)(int), int a) {{ return f(a); }}\n\
             {helper}",
            slot = lay.slot,
            stage = lay.stage,
        ));
    }
    main
}

/// The loader's completed return publishes the selected module where it
/// was loaded. `function` names it: by its symbol, or by its address
/// alias (`func_<ADDR>`).
fn return_rule(function: &str) -> String {
    format!(
        r#"[[dispatch.rules]]
id = "load-completed"
container = "main"
function = "{function}"
event = "return"
effect = "publish"
placement = "selector"
selector = "arg:0"
cases = {{ "0" = "slot-a", "1" = "slot-b", "2" = "region-c" }}
destination = "arg:1"
length = "arg:2"
source_offset = "const:0"
success = "zero"
request = "call"
"#
    )
}

/// The loader's write, at its original store: authored C states its
/// completed range at a `c_marker` call (`marker`), or the build asks for
/// one. The C must pass `source_offset` as declared.
fn write_rule(main: &Image, marker: Option<&str>, source_offset: &str) -> String {
    let at = main.addr("load_module") + LM_STORE;
    let word = main.bytes[(at - main.base) as usize..][..4].try_into().map(u32::from_le_bytes).unwrap_or_default();
    assert_eq!(word >> 26, 0x2B, "LM_STORE names load_module's sw");
    let marker = marker.map_or(String::new(), |m| format!("c_marker = \"{m}\"\n"));
    format!(
        "[[dispatch.rules]]\nid = \"module-written\"\ncontainer = \"main\"\npc = 0x{at:08X}\nevent = \"after-write\"\n{marker}\
         effect = \"publish\"\nplacement = \"selector\"\nselector = \"arg:0\"\n\
         cases = {{ \"0\" = \"slot-a\", \"1\" = \"slot-b\", \"2\" = \"region-c\" }}\n\
         destination = \"write-address\"\nlength = \"write-size\"\nsource_offset = \"{source_offset}\"\n"
    )
}

/// The facts a game would state: three placements, writer summaries (the
/// loader's declared, the pointer store's inferred), and `rule`.
fn explicit_facts(n: &Names, lay: &Layout, rule: &str) -> String {
    format!(
        r#"[dispatch]
schema_version = 1
require_explicit = false

[[dispatch.placements]]
id = "slot-a"
container = "{a}"

[[dispatch.placements]]
id = "slot-b"
container = "{b}"

[[dispatch.placements]]
id = "region-c"
container = "{c}"

[[dispatch.writers]]
id = "loader"
container = "main"
function = "load_module"
destinations = [{{ start = 0x{s1:08X}, end = 0x{e1:08X} }}, {{ start = 0x{s2:08X}, end = 0x{e2:08X} }}]

[[dispatch.writers]]
id = "clearer"
container = "main"
function = "clear"
destinations = [{{ start = 0x{s1:08X}, end = 0x{e1:08X} }}, {{ start = 0x{s2:08X}, end = 0x{e2:08X} }}]

[[dispatch.writers]]
id = "pointer-store"
container = "main"
function = "save_ptr"

{rule}
[[dispatch.rules]]
id = "cleared"
container = "main"
function = "clear"
event = "return"
effect = "invalidate"
destination = "arg:0"
length = "arg:1"
"#,
        a = n.of("a"),
        b = n.of("b"),
        c = n.of("c"),
        s1 = lay.slot,
        e1 = lay.slot + SLOT_BYTES,
        s2 = lay.slot2,
        e2 = lay.slot2 + SLOT_BYTES,
    )
}

/// The slot's modules, each with f(a) = a + k; a is long, its tail
/// function k at slot+0x80 calling the slot (b's shorter load leaves it in
/// place); the second region's module, h() = 0xC1. With `c`, a has
/// matching C, its f `inline`: k's call must still look the slot up.
fn explicit_images(lay: &Layout, n: &Names, c: bool) -> (Image, Image, Image) {
    let mut u = Unit::new(&n.of("a"), lay.slot);
    leaf(&mut u, "f", |a| {
        a.addiu(V0, A0, 0x100);
        mark(a, lay, V0);
    });
    u.func_at("k", lay.slot + 0x80);
    u.a.addiu(SP, SP, -24).sw(RA, 16, SP).jal_abs(lay.slot).nop().lw(RA, 16, SP).addiu(V0, V0, 0x1000);
    mark(&mut u.a, lay, V0);
    u.a.jr(RA).addiu(SP, SP, 24);
    let mut a = u.image(Some(lay.slot + 0x100), &[]);
    if c {
        a.c = Some(format!(
            "inline int f(int a) {{\n\
             \x20   unsigned **log = (unsigned **)0x{log:08X}u;\n\
             \x20   int v = a + 0x100;\n\
             \x20   *(*log)++ = v;\n\
             \x20   return v;\n\
             }}\n\
             int k(int a) {{\n\
             \x20   unsigned **log = (unsigned **)0x{log:08X}u;\n\
             \x20   int v = f(a) + 0x1000;\n\
             \x20   *(*log)++ = v;\n\
             \x20   return v;\n\
             }}\n",
            log = lay.log
        ));
    }
    let mut u = Unit::new(&n.of("c"), lay.slot2);
    leaf(&mut u, "h", |a| ret_k(a, lay, 0xC1));
    let c = u.image(Some(lay.slot2 + 0x80), &[]);
    (a, d06_overlay(lay, n.of("b"), 0x200), c)
}

/// A -> B -> A through the resident loader (B's load, and one that fails
/// before writing, through `load_c`), with a saved pointer, direct, tail,
/// pointer and host calls, A's untouched tail calling into B, an
/// unaffected second region, and an empty clear that changes nothing.
fn d05_explicit(lay: &Layout, n: &Names, loader: Loader) -> Fixture {
    let main = explicit_main(lay, loader);
    let (a, b, c) = explicit_images(lay, n, loader != Loader::Lifted);
    let len = |im: &Image| im.bytes.len() as u32;
    let edit: &[u32] = if loader == Loader::EditedC { &[EDIT_MARK] } else { &[] };
    let (lm, load_c) = (main.addr("load_module"), main.addr("load_c"));
    let (direct, pointer) = if loader == Loader::Lifted { ("call_slot", "call_ptr") } else { ("drive_c", "drive_cp") };
    let (direct, pointer, saved, slot2) = (main.addr(direct), main.addr(pointer), main.addr("call_saved"), main.addr("call_slot2"));
    let mut s = Script::default();
    s.call(main.addr("save_ptr"), [lay.slot, 0, 0, 0], 0, &[]);
    s.call(lm, [2, lay.slot2, len(&c), 0], 0, edit);
    for (sel, im, k) in [(0, &a, 0x100), (1, &b, 0x200), (0, &a, 0x100)] {
        s.call(if sel == 1 { load_c } else { lm }, [sel, lay.slot, len(im), 0], 0, edit);
        s.call(direct, [1, 0, 0, 0], k + 1, &[k + 1]);
        s.call(pointer, [lay.slot, 2, 0, 0], k + 2, &[k + 2]);
        s.call(saved, [3, 0, 0, 0], k + 3, &[lay.slot, k + 3]);
        s.call(main.addr("call_tail"), [4, 0, 0, 0], k + 4, &[k + 4]);
        s.call(lay.slot, [5, 0, 0, 0], k + 5, &[k + 5]);
        s.call(slot2, [0; 4], 0xC1, &[0xC1]);
        // A's tail stays bound across B's shorter load; its call into the
        // slot follows the slot's binding.
        s.call(pointer, [lay.slot + 0x80, 6, 0, 0], k + 0x1006, &[k + 6, k + 0x1006]);
        if sel == 1 {
            // Fails before writing: B stays bound.
            s.call(load_c, [STAGED, lay.slot, len(&a), 0], 1, &[]);
            s.call(saved, [3, 0, 0, 0], k + 3, &[lay.slot, k + 3]);
        }
    }
    // Clearing nothing inside A leaves it bound.
    s.call(main.addr("clear"), [lay.slot + 4, 0, 0, 0], 0, &[]);
    s.call(saved, [3, 0, 0, 0], 0x103, &[lay.slot, 0x103]);
    let init = vec![(lay.stage, a.bytes.clone()), (lay.stage + 0x400, b.bytes.clone()), (lay.stage + 0x800, c.bytes.clone())];
    let rule = match loader {
        Loader::MarkerC | Loader::MarkerElsewhere | Loader::MarkerBadOffset => write_rule(&main, Some("module_written"), "placement"),
        Loader::Lifted => return_rule("load_module"),
        _ => return_rule(&format!("func_{:08X}", main.addr("load_module"))),
    };
    let mut f = Fixture::new(vec![main, a, b, c], s);
    f.init = init;
    f.dispatch = Some(explicit_facts(n, lay, &rule));
    f
}

fn d05_explicit_lifted(lay: &Layout, n: &Names) -> Fixture {
    d05_explicit(lay, n, Loader::Lifted)
}
fn d05_explicit_mixed(lay: &Layout, n: &Names) -> Fixture {
    d05_explicit(lay, n, Loader::MatchingC)
}
fn d05_explicit_edited(lay: &Layout, n: &Names) -> Fixture {
    d05_explicit(lay, n, Loader::EditedC)
}
fn d05_explicit_marker(lay: &Layout, n: &Names) -> Fixture {
    d05_explicit(lay, n, Loader::MarkerC)
}

/// A write rule inside supplied C with no c_marker: no verified mapping
/// exists, so the build asks for the marker instead of lifting the C.
fn d05_explicit_missing_c_site(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::MatchingC);
    f.dispatch = Some(explicit_facts(n, lay, &write_rule(&f.images[0], None, "placement")));
    f.script = Script::default();
    f.build = Build::Rejected("binding.missing_c_write_site: rule `module-written`".into());
    f
}

/// The marker call is in a helper, not the function holding the write.
fn d05_explicit_marker_elsewhere(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::MarkerElsewhere);
    f.script = Script::default();
    f.build = Build::Rejected("binding.missing_c_write_site: rule `module-written`".into());
    f
}

/// The rule declares the write's source offset 4; the C passes 0.
fn d05_explicit_marker_mismatch(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::MarkerC);
    f.dispatch = Some(explicit_facts(n, lay, &write_rule(&f.images[0], Some("module_written"), "const:4")));
    let main = &f.images[0];
    let (lm, save) = (main.addr("load_module"), main.addr("save_ptr"));
    let len = f.images[3].bytes.len() as u32;
    let mut s = Script::default();
    s.call(save, [lay.slot, 0, 0, 0], 0, &[]).reject(lm, [2, lay.slot2, len, 0], "binding.marker_mismatch #0 0x00000000".into());
    f.script = s;
    f
}

/// The C passes source offset 4 where the placement maps the destination
/// to 0: the publication is rejected.
fn d05_explicit_marker_offset(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::MarkerBadOffset);
    let main = &f.images[0];
    let (lm, save) = (main.addr("load_module"), main.addr("save_ptr"));
    let len = f.images[3].bytes.len() as u32;
    let mut s = Script::default();
    s.call(save, [lay.slot, 0, 0, 0], 0, &[]).reject(lm, [2, lay.slot2, len, 0], format!("binding.bad_destination #0 0x{:08X}", lay.slot2));
    f.script = s;
    f
}

/// Clearing a loaded module invalidates it: the slot rejects the call.
fn d05_explicit_cleared(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::Lifted);
    let main = &f.images[0];
    let (lm, slot, clear) = (main.addr("load_module"), main.addr("call_slot"), main.addr("clear"));
    let len = f.images[1].bytes.len() as u32;
    let mut s = Script::default();
    s.call(lm, [0, lay.slot, len, 0], 0, &[]).call(slot, [1, 0, 0, 0], 0x101, &[0x101]);
    s.call(clear, [lay.slot, len, 0, 0], 0, &[]).untranslated(slot, [1, 0, 0, 0], lay.slot);
    f.script = s;
    f
}

/// A declared writer bound its known store falls outside.
fn d05_explicit_wrong_writer(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::Lifted);
    let facts = f.dispatch.take().unwrap_or_default();
    let wrong = format!("function = \"save_ptr\"\ndestinations = [{{ start = 0x{:08X}, end = 0x{:08X} }}]\n", lay.stage + 0x1008, lay.stage + 0x100C);
    f.dispatch = Some(facts.replace("function = \"save_ptr\"\n", &wrong));
    f.script = Script::default();
    f.build = Build::Rejected("binding.invalid_fact: writer `pointer-store` declares".into());
    f
}

/// Facts that leave module b without a placement: its arrival would leave
/// a stale binding, so the build refuses rather than probing code.
fn d05_explicit_missing_placement(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::Lifted);
    let facts = f.dispatch.take().unwrap_or_default();
    let placement = format!("[[dispatch.placements]]\nid = \"slot-b\"\ncontainer = \"{}\"\n\n", n.of("b"));
    f.dispatch = Some(facts.replace(&placement, "").replace(", \"1\" = \"slot-b\"", ""));
    f.script = Script::default();
    f.build = Build::Rejected("binding.missing_placement".into());
    f
}

/// A completed load whose selector no case names: the publication is
/// rejected at run time, never guessed.
fn d05_explicit_unknown_selector(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::Lifted);
    let facts = f.dispatch.take().unwrap_or_default();
    f.dispatch = Some(facts.replace(", \"2\" = \"region-c\"", ""));
    let main = &f.images[0];
    let (lm, direct) = (main.addr("load_module"), main.addr("call_slot"));
    let len = f.images[3].bytes.len() as u32;
    let mut s = Script::default();
    s.call(lm, [0, lay.slot, f.images[1].bytes.len() as u32, 0], 0, &[]).call(direct, [1, 0, 0, 0], 0x101, &[0x101]);
    s.reject(lm, [2, lay.slot2, len, 0], "binding.unknown_selector #0 0x00000002".into());
    f.script = s;
    f
}

/// A completed load of module a to the second region: not where slot-a's
/// layout loads, so the publication is rejected.
fn d05_explicit_bad_destination(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::Lifted);
    let main = &f.images[0];
    let lm = main.addr("load_module");
    let len = f.images[1].bytes.len() as u32;
    let mut s = Script::default();
    s.reject(lm, [0, lay.slot2, len, 0], format!("binding.bad_destination #0 0x{:08X}", lay.slot2));
    f.script = s;
    f
}

/// A rule this milestone doesn't implement fails the build by name.
fn d05_explicit_unimplemented(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::Lifted);
    let facts = f.dispatch.take().unwrap_or_default();
    let rule = format!(
        "\n[[dispatch.rules]]\nid = \"copy-wrote\"\ncontainer = \"main\"\npc = 0x{:08X}\nevent = \"after-write\"\neffect = \"invalidate\"\ndestination = \"write-address\"\nlength = \"write-size\"\n",
        lay.main + 0x30
    );
    f.dispatch = Some(facts + &rule);
    f.script = Script::default();
    f.build = Build::Rejected("binding.unimplemented_rule: rule `copy-wrote`".into());
    f
}

// ---- D06: transfer routes ----------------------------------------------

/// The slot function returns a0 + k, marked.
fn d06_overlay(lay: &Layout, id: String, k: i32) -> Image {
    let mut u = Unit::new(&id, lay.slot);
    leaf(&mut u, "f", |a| {
        a.addiu(V0, A0, k);
        mark(a, lay, V0);
    });
    u.image(Some(lay.slot + 0x80), &[])
}

fn d06_lifted(lay: &Layout, n: &Names) -> Fixture {
    let main = main_image(lay, |u| {
        // A jal whose delay slot sets the argument.
        u.func("call_delay").addiu(SP, SP, -24).sw(RA, 16, SP).jal_abs(lay.slot).addiu(A0, A0, 5).lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
        // A jalr whose delay slot sets the argument.
        u.func("call_ptr_delay").addiu(SP, SP, -24).sw(RA, 16, SP).mov(T0, A0).jalr(T0).addiu(A0, A1, 7);
        u.a.lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
        // Marks around the call: its effects keep their order.
        let a = u.func("ordered");
        a.addiu(SP, SP, -24).sw(RA, 16, SP);
        mark_k(a, lay, 1);
        a.jal_abs(lay.slot).nop();
        mark_k(a, lay, 2);
        a.lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
    });
    let (a, b) = (d06_overlay(lay, n.of("a"), 0x100), d06_overlay(lay, n.of("b"), 0x200));
    let mut s = Script::default();
    for (im, k) in [(&a, 0x100), (&b, 0x200)] {
        s.load(im);
        s.call(main.addr("call_slot"), [1, 0, 0, 0], k + 1, &[k + 1]);
        s.call(main.addr("call_ptr"), [lay.slot, 2, 0, 0], k + 2, &[k + 2]);
        s.call(main.addr("call_tail"), [3, 0, 0, 0], k + 3, &[k + 3]);
        s.call(main.addr("call_delay"), [4, 0, 0, 0], k + 9, &[k + 9]);
        s.call(main.addr("call_ptr_delay"), [lay.slot, 5, 0, 0], k + 12, &[k + 12]);
        s.call(main.addr("ordered"), [6, 0, 0, 0], k + 6, &[1, k + 6, 2]);
        // The host's own call into the slot (the runtime calling a guest
        // function pointer).
        s.call(lay.slot, [7, 0, 0, 0], k + 7, &[k + 7]);
    }
    Fixture::new(vec![main, a, b], s)
}

/// An entry point inside the slot function's sized extent.
fn d06_interior(lay: &Layout, n: &Names) -> Fixture {
    let main = main_image(lay, |_| {});
    let ovl = |role: &str, k: i32| {
        let mut u = Unit::new(&n.of(role), lay.slot);
        u.func("f").lui(V0, 0x8011).ori(A0, ZERO, 0x100).nop().nop();
        u.func("f_mid").addiu(V1, A0, k);
        mark(&mut u.a, lay, V1);
        u.a.mov(V0, V1).jr(RA).nop();
        u.funcs[0].2 = Some(0x40);
        u.image(Some(lay.slot + 0x80), &[])
    };
    let (a, b) = (ovl("a", 0xA), ovl("b", 0xB));
    let mut s = Script::default();
    let ptr = main.addr("call_ptr");
    let mid = lay.slot + 0x10;
    for (im, k) in [(&a, 0xA), (&b, 0xB), (&a, 0xA)] {
        s.load(im).call(ptr, [lay.slot, 0, 0, 0], 0x100 + k, &[0x100 + k]).call(ptr, [mid, 0x200, 0, 0], 0x200 + k, &[0x200 + k]);
    }
    Fixture::new(vec![main, a, b], s)
}

/// Decompiled C in the executable calls the slot directly and through a
/// pointer; the executable's code is the same calls, in MIPS.
fn d06_c(lay: &Layout, n: &Names) -> Fixture {
    let mut main = main_image(lay, |u| {
        u.func("drive_c").addiu(SP, SP, -24).sw(RA, 16, SP).jal_abs(lay.slot).nop().lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
        u.func("drive_cp").addiu(SP, SP, -24).sw(RA, 16, SP).mov(T0, A0).jalr(T0).mov(A0, A1);
        u.a.lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
    });
    main.c = Some(format!(
        "int func_{:08X}(int a);\nint drive_c(int a) {{ return func_{:08X}(a); }}\nint drive_cp(int (*f)(int), int a) {{ return f(a); }}\n",
        lay.slot, lay.slot
    ));
    let (a, b) = (d06_overlay(lay, n.of("a"), 0x100), d06_overlay(lay, n.of("b"), 0x200));
    let mut s = Script::default();
    for (im, k) in [(&a, 0x100), (&b, 0x200), (&a, 0x100)] {
        s.load(im).call(main.addr("drive_c"), [1, 0, 0, 0], k + 1, &[k + 1]).call(main.addr("drive_cp"), [lay.slot, 2, 0, 0], k + 2, &[k + 2]);
    }
    Fixture::new(vec![main, a, b], s)
}

// ---- D07: addresses and namespaces -------------------------------------

/// Overlay a resident; one call through `ptr`, expecting `expect`.
fn d07(lay: &Layout, n: &Names, ptr: u32, expect: Option<(u32, u32)>) -> Fixture {
    let main = main_image(lay, |_| {});
    let (a, b) = (d01_overlay(lay, n.of("a"), 0, 0xA1), d01_overlay(lay, n.of("b"), 0, 0xB1));
    let mut s = Script::default();
    let call = main.addr("call_ptr");
    s.load(&a).call(call, [lay.slot, 0, 0, 0], 0xA1, &[0xA1]);
    match expect {
        Some((v0, mark)) => {
            let marks: Vec<u32> = (mark != 0).then_some(mark).into_iter().collect();
            s.call(call, [ptr, 0, 0, 0], v0, &marks);
        }
        None => {
            s.untranslated(call, [ptr, 0, 0, 0], ptr);
        }
    }
    Fixture::new(vec![main, a, b], s)
}

fn d07_kseg1(lay: &Layout, n: &Names) -> Fixture {
    d07(lay, n, lay.slot & 0x1FFF_FFFF | 0xA000_0000, Some((0xA1, 0xA1)))
}
fn d07_kuseg(lay: &Layout, n: &Names) -> Fixture {
    d07(lay, n, lay.slot & 0x1FFF_FFFF, Some((0xA1, 0xA1)))
}
fn d07_misaligned(lay: &Layout, n: &Names) -> Fixture {
    d07(lay, n, lay.slot + 2, None)
}
fn d07_mirror_2mb(lay: &Layout, n: &Names) -> Fixture {
    d07(lay, n, lay.slot + 0x20_0000, None)
}
fn d07_mirror_4mb(lay: &Layout, n: &Names) -> Fixture {
    d07(lay, n, lay.slot + 0x40_0000, None)
}
fn d07_unmapped(lay: &Layout, n: &Names) -> Fixture {
    d07(lay, n, lay.slot + 0x80_0000, None)
}
fn d07_stand_in(lay: &Layout, n: &Names) -> Fixture {
    let p = psxw_abi::BIOS_STAND_INS.start + 4;
    d07(lay, n, p, Some((super::wasm::STAND_IN_RESULT + 4, 0)))
}
/// Kernel RAM whose dispatch slot the BIOS stand-ins reuse.
fn d07_kernel_collision(lay: &Layout, n: &Names) -> Fixture {
    d07(lay, n, 0x8000_0000 | (psxw_abi::BIOS_STAND_INS.start + 4) & 0x003F_FFFF, None)
}

// ---- D08: calling state ------------------------------------------------

/// The slot function sums six o32 arguments (two on the stack) plus k.
fn d08_sum6(lay: &Layout, id: String, k: i32) -> Image {
    let mut u = Unit::new(&id, lay.slot);
    leaf(&mut u, "f", |a| {
        a.lw(T0, 24 + 16, SP).lw(T1, 24 + 20, SP).addu(V0, A0, A1).addu(V0, V0, A2).addu(V0, V0, A3).addu(V0, V0, T0).addu(V0, V0, T1);
        a.addiu(V0, V0, k);
        mark(a, lay, V0);
    });
    u.image(Some(lay.slot + 0x80), &[])
}

fn d08_c(lay: &Layout, n: &Names) -> Fixture {
    let mut main = main_image(lay, |u| {
        // drive_args(): func(1, 2, 3, 4, 5, 6).
        let a = u.func("drive_args");
        a.addiu(SP, SP, -32).sw(RA, 24, SP).ori(T0, ZERO, 5).sw(T0, 16, SP).ori(T0, ZERO, 6).sw(T0, 20, SP);
        a.ori(A0, ZERO, 1).ori(A1, ZERO, 2).ori(A2, ZERO, 3).jal_abs(lay.slot).ori(A3, ZERO, 4).lw(RA, 24, SP).nop().jr(RA).addiu(SP, SP, 32);
        // drive_args_p(f): f(1, 2, 3, 4, 5, 6).
        let a = u.func("drive_args_p");
        a.addiu(SP, SP, -32).sw(RA, 24, SP).ori(T0, ZERO, 5).sw(T0, 16, SP).ori(T0, ZERO, 6).sw(T0, 20, SP).mov(T1, A0);
        a.ori(A0, ZERO, 1).ori(A1, ZERO, 2).ori(A2, ZERO, 3).jalr(T1).ori(A3, ZERO, 4).lw(RA, 24, SP).nop().jr(RA).addiu(SP, SP, 32);
        // helper(a, b, c, d, e) = a + b + c + d + 16e, for overlay code.
        u.func("helper").lw(T0, 16, SP).addu(V0, A0, A1).addu(V0, V0, A2).addu(V0, V0, A3).sll(T0, T0, 4).jr(RA).addu(V0, V0, T0);
    });
    main.c = Some(format!(
        "int func_{s:08X}(int, int, int, int, int, int);\n\
         int drive_args(void) {{ return func_{s:08X}(1, 2, 3, 4, 5, 6); }}\n\
         int drive_args_p(int (*f)(int, int, int, int, int, int)) {{ return f(1, 2, 3, 4, 5, 6); }}\n\
         int helper(int a, int b, int c, int d, int e) {{ return a + b + c + d + e * 16; }}\n",
        s = lay.slot
    ));
    let helper = main.addr("helper");
    // The second overlay's function calls the executable's C helper with
    // five arguments.
    let mut u = Unit::new(&n.of("b"), lay.slot);
    leaf(&mut u, "f", |a| {
        a.sw(RA, 20, SP).ori(T0, ZERO, 9).sw(T0, 16, SP).ori(A0, ZERO, 1).ori(A1, ZERO, 2).ori(A2, ZERO, 3).jal_abs(helper).ori(A3, ZERO, 4);
        a.lw(RA, 20, SP).addiu(V0, V0, 0x200);
        mark(a, lay, V0);
    });
    let b = u.image(Some(lay.slot + 0x80), &[]);
    let a = d08_sum6(lay, n.of("a"), 0x100);
    let mut s = Script::default();
    s.load(&a).call(main.addr("drive_args"), [0; 4], 0x115, &[0x115]).call(main.addr("drive_args_p"), [lay.slot, 0, 0, 0], 0x115, &[0x115]);
    s.load(&b).call(main.addr("drive_args"), [0; 4], 154 + 0x200, &[154 + 0x200]);
    Fixture::new(vec![main, a, b], s)
}

/// v1 and callee-saved registers across a call into the slot.
fn d08_registers(lay: &Layout, n: &Names) -> Fixture {
    let main = main_image(lay, |u| {
        let a = u.func("preserve");
        a.addiu(SP, SP, -48).sw(RA, 40, SP);
        for r in 0..8u8 {
            a.sw(S0 + r, 8 + 4 * r as i32, SP).li(S0 + r, 0x5000 + r as u32);
        }
        a.jal_abs(lay.slot).nop();
        for r in 0..8u8 {
            mark(a, lay, S0 + r);
        }
        mark(a, lay, SP);
        for r in 0..8u8 {
            a.lw(S0 + r, 8 + 4 * r as i32, SP);
        }
        a.lw(RA, 40, SP).nop().jr(RA).addiu(SP, SP, 48);
    });
    let ovl = |role: &str, k: u32| {
        let mut u = Unit::new(&n.of(role), lay.slot);
        leaf(&mut u, "f", |a| {
            a.sw(S0, 16, SP).sw(S0 + 1, 20, SP).ori(S0, ZERO, 1).ori(S0 + 1, ZERO, 2).lui(V1, 0x1234).ori(V1, V1, k);
            a.ori(V0, ZERO, k);
            mark(a, lay, V0);
            a.lw(S0, 16, SP).lw(S0 + 1, 20, SP);
        });
        u.image(Some(lay.slot + 0x80), &[])
    };
    let (a, b) = (ovl("a", 0xA1), ovl("b", 0xB1));
    let mut s = Script::default();
    let saved: Vec<u32> = (0..8).map(|r| 0x5000 + r).collect();
    for (im, k) in [(&a, 0xA1u32), (&b, 0xB1)] {
        s.load(im);
        s.log.push(k);
        s.log.extend(&saved);
        s.log.push(lay.stack - 48);
        s.steps.push(Step::Call { entry: main.addr("preserve"), args: [0; 4], expect: Expect::Value { v0: Some(k), v1: None, log: Some(s.log.clone()) } });
        s.log.push(k);
        s.steps.push(Step::Call { entry: lay.slot, args: [0; 4], expect: Expect::Value { v0: Some(k), v1: Some(0x1234_0000 | k), log: Some(s.log.clone()) } });
    }
    Fixture::new(vec![main, a, b], s)
}

// ---- D09: re-entry and suspension --------------------------------------

/// The slot function calls a native flow (the first kernel stand-in) with
/// a pointer into the second slot; the flow calls it back.
fn d09_nested(lay: &Layout, n: &Names) -> Fixture {
    let main = main_image(lay, |_| {});
    let mut u = Unit::new(&n.of("a"), lay.slot);
    leaf(&mut u, "f", |a| {
        a.sw(RA, 16, SP).li(A0, lay.slot2).ori(A1, ZERO, 7).li(T0, psxw_abi::KERNEL_STAND_INS.start).jalr(T0).nop();
        a.lw(RA, 16, SP).addiu(V0, V0, 0x1000);
        mark(a, lay, V0);
    });
    let a = u.image(Some(lay.slot + 0x80), &[]);
    let second = |role: &str, k: i32| {
        let mut u = Unit::new(&n.of(role), lay.slot2);
        leaf(&mut u, "g", |a| {
            a.addiu(V0, A0, k);
            mark(a, lay, V0);
        });
        u.image(Some(lay.slot2 + 0x80), &[])
    };
    let (a2, b2) = (second("a2", 0x20), second("b2", 0x30));
    let mut s = Script::default();
    let slot = main.addr("call_slot");
    s.load(&a).load(&a2).call(slot, [0; 4], 0x1027, &[0x27, 0x1027]).load(&b2).call(slot, [0; 4], 0x1037, &[0x37, 0x1037]);
    Fixture::new(vec![main, a, a2, b2], s)
}

/// The slot function switches threads (ChangeThread) and, once switched
/// back, calls into the second slot. While it is suspended the host loads
/// `during` (into the second slot, or over the suspended code itself).
fn d09_suspend(lay: &Layout, n: &Names, replace: bool) -> Fixture {
    let main = main_image(lay, |_| {});
    let first = |role: &str, k: u32| {
        let mut u = Unit::new(&n.of(role), lay.slot);
        leaf(&mut u, "f", |a| {
            a.sw(RA, 16, SP);
            mark_k(a, lay, k);
            a.ori(T2, ZERO, 0xB0).jalr(T2).ori(T1, ZERO, 0x10);
            a.jal_abs(lay.slot2).nop().lw(RA, 16, SP).addiu(V0, V0, k as i32);
            mark(a, lay, V0);
        });
        u.image(Some(lay.slot + 0x80), &[])
    };
    let second = |role: &str, k: u32| {
        let mut u = Unit::new(&n.of(role), lay.slot2);
        leaf(&mut u, "g", |a| ret_k(a, lay, k));
        u.image(Some(lay.slot2 + 0x80), &[])
    };
    let (a, b, a2, b2) = (first("a", 0x100), first("b", 0x200), second("a2", 0x2A), second("b2", 0x2B));
    let mut s = Script::default();
    s.load(&a).load(&a2);
    s.steps.push(Step::Call { entry: main.addr("call_slot"), args: [0; 4], expect: Expect::Suspend { log: vec![0x100] } });
    s.log.push(0x100);
    if replace {
        // Proposed active-replacement contract: the continuation would
        // execute b's remaining instructions. Keep the mismatch visible;
        // do not reclassify it as unsupported merely to pass the suite.
        s.load(&b);
        s.log.extend([0x2A, 0x22A]);
        s.steps.push(Step::Resume { expect: Expect::v0(0x22A, &s.log) });
    } else {
        s.load(&b2);
        s.log.extend([0x2B, 0x12B]);
        s.steps.push(Step::Resume { expect: Expect::v0(0x12B, &s.log) });
    }
    let mut f = Fixture::new(vec![main, a, b, a2, b2], s);
    f.threads = true;
    f
}

fn d09_suspend_retained(lay: &Layout, n: &Names) -> Fixture {
    d09_suspend(lay, n, false)
}
fn d09_suspend_replaced(lay: &Layout, n: &Names) -> Fixture {
    d09_suspend(lay, n, true)
}

/// The host calls overlay functions directly (a native callback into the
/// guest) while the overlays change under it.
fn d09_callback(lay: &Layout, n: &Names) -> Fixture {
    let main = main_image(lay, |_| {});
    let (a, b) = (d06_overlay(lay, n.of("a"), 0x100), d06_overlay(lay, n.of("b"), 0x200));
    let mut s = Script::default();
    for (im, k) in [(&a, 0x100), (&b, 0x200), (&a, 0x100)] {
        s.load(im).call(lay.slot, [1, 0, 0, 0], k + 1, &[k + 1]);
    }
    Fixture::new(vec![main, a, b], s)
}

// ---- D10: failure and ambiguity ----------------------------------------

/// Nothing is loaded in the slot.
fn d10_nothing(lay: &Layout, n: &Names) -> Fixture {
    let main = main_image(lay, |_| {});
    let (a, b) = (d01_overlay(lay, n.of("a"), 0, 0xA1), d01_overlay(lay, n.of("b"), 0, 0xB1));
    let mut s = Script::default();
    s.untranslated(main.addr("call_slot"), [0; 4], lay.slot);
    Fixture::new(vec![main, a, b], s)
}

/// An entry only one overlay has, before that overlay is loaded.
fn d10_unique_unloaded(lay: &Layout, n: &Names) -> Fixture {
    let (main, a, b) = d03_boundaries_images(lay, n);
    let mut s = Script::default();
    let g = lay.slot + 0x40;
    s.untranslated(main.addr("call_ptr"), [g, 0, 0, 0], g);
    Fixture::new(vec![main, a, b], s)
}

/// b is a's first 0x40 bytes: their slot functions are word for word the
/// same, but a's calls a helper beyond b's end that reads t0 (expanded
/// into a's caller). With only b loaded the helper isn't there.
fn d10_ambiguous(lay: &Layout, n: &Names) -> Fixture {
    let main = main_image(lay, |_| {});
    let mut u = Unit::new(&n.of("a"), lay.slot);
    leaf(&mut u, "f", |a| {
        a.sw(RA, 16, SP).ori(T0, ZERO, 5).jal_abs(lay.slot + 0x80).nop().lw(RA, 16, SP).nop();
    });
    u.func_at("h", lay.slot + 0x80);
    u.a.addiu(V0, T0, 0x100);
    mark(&mut u.a, lay, V0);
    u.a.jr(RA).nop();
    let a = u.image(Some(lay.slot + 0x100), &[]);
    let b = Image {
        id: n.of("b"),
        base: lay.slot,
        bytes: a.bytes[..0x40].to_vec(),
        text: 0x40,
        funcs: vec![("f".into(), lay.slot, None)],
        c: None,
    };
    let mut s = Script::default();
    s.load(&b).untranslated(main.addr("call_slot"), [0; 4], lay.slot + 0x80);
    Fixture::new(vec![main, a, b], s)
}

/// Code with no symbol that nothing references.
fn d10_no_metadata(lay: &Layout, n: &Names) -> Fixture {
    let main = main_image(lay, |_| {});
    let mut u = Unit::new(&n.of("a"), lay.slot);
    leaf(&mut u, "f", |a| ret_k(a, lay, 0xA1));
    u.a.pad_to(lay.slot + 0x40);
    u.a.ori(V0, ZERO, 0xA9).jr(RA).nop();
    let a = u.image(Some(lay.slot + 0x80), &[]);
    let mut s = Script::default();
    let hidden = lay.slot + 0x40;
    s.load(&a).call(main.addr("call_slot"), [0; 4], 0xA1, &[0xA1]).untranslated(main.addr("call_ptr"), [hidden, 0, 0, 0], hidden);
    Fixture::new(vec![main, a], s)
}

/// a's slot function can't be translated; b's can.
fn d10_untranslatable(lay: &Layout, n: &Names) -> Fixture {
    let main = main_image(lay, |_| {});
    let mut u = Unit::new(&n.of("a"), lay.slot);
    leaf(&mut u, "f", |a| {
        a.word(INVALID);
        ret_k(a, lay, 0xA1);
    });
    let a = u.image(Some(lay.slot + 0x80), &[]);
    let b = d01_overlay(lay, n.of("b"), 0, 0xB1);
    let mut s = Script::default();
    s.load(&b).call(main.addr("call_slot"), [0; 4], 0xB1, &[0xB1]).load(&a).untranslated(main.addr("call_slot"), [0; 4], lay.slot);
    Fixture::new(vec![main, a, b], s)
}

// ---- The corpus ---------------------------------------------------------

const REF: Evidence = Evidence::Reference;

fn defs() -> Vec<Def> {
    let d = |case, name, note, evidence, make| Def { case, name, note, evidence, tfs: BASE, make, review_required: None };
    // Named cases only. Apply the transformations once to the collision
    // control, and swap names once for ambiguity; no cross-product sweep.
    let mut defs = vec![
        d("D01", "two-short-prefix", "supported", REF, |l, n| d01_prefix(l, n, 0, 2)),
        d("D01", "two-long-prefix", "supported", REF, |l, n| d01_prefix(l, n, 10, 2)),
        d("D01", "three-short-prefix", "supported", REF, |l, n| d01_prefix(l, n, 0, 3)),
        d("D01", "late-difference", "supported", REF, d01_late),
        d("D01", "equivalent", "supported", REF, d01_equivalent),
        d("D02", "callee", "supported", REF, d02_callee),
        d("D02", "inlined", "supported", REF, d02_inlined),
        d("D02", "jump-table-permuted", "supported", REF, d02_table_permuted),
        d("D02", "jump-table-disjoint", "supported", REF, d02_table_disjoint),
        d("D02", "data", "supported", REF, d02_data),
        d("D03", "unequal-lengths", "supported", REF, d03_lengths),
        d("D03", "boundaries", "proposed rejection: the last call enters b mid-function at an undiscovered entry", REF, d03_boundaries),
        d("D03", "main-overlap-direct", "supported", REF, d03_main_overlap_direct),
        d("D03", "main-overlap-pointer", "supported", REF, d03_main_overlap_pointer),
        d("D03", "two-regions", "supported", REF, d03_two_regions),
        d("D04", "history", "supported", REF, d04_history),
        d("D04", "stale-tail", "supported", REF, d04_stale_tail),
        d("D04", "chunked", "supported", REF, d04_chunked),
        d("D04", "chunked-mixed", "proposed rejection: code half one image, half the other", REF, d04_chunked_mixed),
        d("D04", "patched", "proposed rejection: code no image has (a changed instruction)", REF, d04_patched),
        d("D05", "host", "supported", REF, d05_host),
        d("D05", "stores", "supported", REF, d05_stores),
        d("D05", "copy", "supported", REF, d05_copy),
        d("D05", "unpack", "supported", REF, d05_unpack),
        d("D05", "explicit-binding-lifted", "supported: explicit binding, lifted loader", REF, d05_explicit_lifted),
        d("D05", "explicit-binding-mixed", "supported: explicit binding, matching C loader and drivers", REF, d05_explicit_mixed),
        d(
            "D05",
            "explicit-binding-edited-c",
            "supported: explicit binding keeps a deliberately edited C loader",
            Evidence::ClosedForm("authored C edit: its log entry is absent from the original binary"),
            d05_explicit_edited,
        ),
        d("D05", "explicit-binding-c-marker", "supported: explicit binding from authored C's c_marker write site", REF, d05_explicit_marker),
        d("D05", "explicit-binding-missing-c-site", "rejected facts: a C write site without its c_marker", REF, d05_explicit_missing_c_site),
        d("D05", "explicit-binding-marker-elsewhere", "rejected facts: the marker call isn't in the function holding the write", REF, d05_explicit_marker_elsewhere),
        d("D05", "explicit-binding-marker-mismatch", "port rejection: a marker value contradicts its rule", REF, d05_explicit_marker_mismatch),
        d("D05", "explicit-binding-marker-offset", "port rejection: a marker's source offset contradicts the placement", REF, d05_explicit_marker_offset),
        d("D05", "explicit-binding-cleared", "port rejection: an invalidated module isn't callable", REF, d05_explicit_cleared),
        d("D05", "explicit-binding-wrong-writer", "rejected facts: a declared writer bound contradicts a known write", REF, d05_explicit_wrong_writer),
        d("D05", "explicit-binding-unknown-selector", "port rejection: a completed load names no placement", REF, d05_explicit_unknown_selector),
        d("D05", "explicit-binding-bad-destination", "port rejection: a load outside its placement's layout", REF, d05_explicit_bad_destination),
        d("D05", "explicit-binding-missing-placement", "rejected facts: an image overlapping a placement has none", REF, d05_explicit_missing_placement),
        d("D05", "explicit-binding-unimplemented", "rejected facts: an after-write rule is milestone C", REF, d05_explicit_unimplemented),
        d("D06", "lifted-routes", "supported", REF, d06_lifted),
        d("D06", "interior-entry", "supported", REF, d06_interior),
        d("D06", "c-routes", "supported", REF, d06_c),
        d("D07", "kseg1-alias", "supported: KSEG1 views the same RAM", REF, d07_kseg1),
        d("D07", "kuseg-alias", "supported: KUSEG views the same RAM", REF, d07_kuseg),
        d("D07", "misaligned", "proposed port rejection, not a console address-error model", REF, d07_misaligned),
        d("D07", "mirror-2mb", "proposed port rejection for a pointer outside the 2 MB RAM window", REF, d07_mirror_2mb),
        d("D07", "mirror-4mb", "proposed port rejection for a pointer colliding under the dispatch mask", REF, d07_mirror_4mb),
        d("D07", "unmapped", "proposed port rejection, not a console bus-error model", REF, d07_unmapped),
        d("D07", "bios-stand-in", "supported (port ABI): a saved native BIOS entry", Evidence::ClosedForm("port-only native stand-in"), d07_stand_in),
        d("D07", "kernel-collision", "proposed rejection: kernel RAM shares a stand-in's dispatch slot", REF, d07_kernel_collision),
        d("D08", "c-and-lift", "supported", REF, d08_c),
        d("D08", "registers", "supported", REF, d08_registers),
        d("D09", "callback", "supported", REF, d09_callback),
        d("D09", "nested", "supported (port ABI): a native flow calls back into the guest", Evidence::ClosedForm("port-only native callback"), d09_nested),
        d("D09", "suspend-retained", "supported: the suspended code stays resident", Evidence::ClosedForm("thread switch through Asyncify"), d09_suspend_retained),
        Def {
            review_required: Some("active-code replacement needs a reviewed lifetime contract; do not hide the observed mismatch"),
            ..d("D09", "suspend-replaced", "support classification requires operator review", Evidence::ClosedForm("thread switch through Asyncify"), d09_suspend_replaced)
        },
        d("D10", "nothing-resident", "no code is resident at the slot", REF, d10_nothing),
        d("D10", "unique-unloaded", "proposed rejection: an entry only an unloaded overlay has", REF, d10_unique_unloaded),
        d("D10", "ambiguous", "proposed rejection: matching candidates depend on absent code", REF, d10_ambiguous),
        d("D10", "no-metadata", "code no symbol or reference names has no translation", REF, d10_no_metadata),
        d("D10", "untranslatable", "proposed rejection: the resident candidate couldn't be translated", REF, d10_untranslatable),
    ];
    defs[0].tfs = ALL;
    defs.iter_mut().find(|d| d.name == "ambiguous").unwrap().tfs = &[Tf::Base, Tf::SwapNames];
    for d in &mut defs {
        if d.note.starts_with("proposed") {
            d.review_required = Some("proposed rejection contract: operator review required before changing D5/D6/D7");
        }
    }
    defs
}

/// Every variant, or those whose id starts with `filter` (`D01`,
/// `D01/two-short-prefix`).
pub fn corpus(filter: Option<&str>) -> Vec<Variant> {
    defs().iter().flat_map(expand).filter(|v| filter.is_none_or(|f| v.id().starts_with(f))).collect()
}
