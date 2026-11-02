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
    main_unit(lay, extra).image(None, &[])
}

/// [`main_image`] before it is assembled, for data after its text.
fn main_unit(lay: &Layout, extra: impl FnOnce(&mut Unit)) -> Unit {
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
    u
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
    native: Option<NativeExpect>,
}

impl Fixture {
    fn new(images: Vec<Image>, script: Script) -> Self {
        Self { images, init: Vec::new(), script, build: Build::Links, threads: false, dispatch: None, native: None }
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
                native: f.native,
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
    /// Only `load_c` is C: it copies through `memcpy` with a constant
    /// length, which a rule instruments.
    MemcpyC,
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
        // memcpy(dst, src, bytes): a word copy, returning dst.
        u.func("memcpy").mov(V0, A0);
        u.a.label("mc_loop");
        u.a.lw(T0, 0, A1).addiu(A1, A1, 4).sw(T0, 0, A0).addiu(A2, A2, -4).bne(A2, ZERO, "mc_loop").addiu(A0, A0, 4);
        u.a.jr(RA).nop();
        u.func("clear").beq(A1, ZERO, "clr_done").nop();
        u.a.label("clr_loop");
        u.a.sw(ZERO, 0, A0).addiu(A1, A1, -4).bne(A1, ZERO, "clr_loop").addiu(A0, A0, 4);
        u.a.label("clr_done");
        u.a.jr(RA).ori(V0, ZERO, 0);
    });
    if loader == Loader::MemcpyC {
        // A constant-length memcpy GCC would have inlined: the rule on
        // memcpy must still see the call. (Every load_c call here copies
        // 0x80 bytes, as the length passed says.)
        main.c = Some(format!(
            "void *memcpy(void *dst, const void *src, unsigned len);\n\
             void flush(void);\n\
             int load_c(unsigned sel, unsigned *dst, unsigned len) {{\n\
             \x20   if (sel >= {STAGED}u) return 1;\n\
             \x20   memcpy(dst, (void *)(0x{stage:08X}u + sel * 0x400u), 0x80u);\n\
             \x20   flush();\n\
             \x20   return 0;\n\
             }}\n",
            stage = lay.stage,
        ));
    } else if loader != Loader::Lifted {
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
    let (a, b, c) = explicit_images(lay, n, !matches!(loader, Loader::Lifted | Loader::MemcpyC));
    let len = |im: &Image| im.bytes.len() as u32;
    let edit: &[u32] = if loader == Loader::EditedC { &[EDIT_MARK] } else { &[] };
    let (lm, load_c) = (main.addr("load_module"), main.addr("load_c"));
    let (direct, pointer) = if matches!(loader, Loader::Lifted | Loader::MemcpyC) { ("call_slot", "call_ptr") } else { ("drive_c", "drive_cp") };
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
        // B arrives through load_c's memcpy, whose completed return
        // publishes it.
        Loader::MemcpyC => format!(
            "{}\n[[dispatch.writers]]\nid = \"copier\"\ncontainer = \"main\"\nfunction = \"memcpy\"\n\
             destinations = [{{ start = 0x{:08X}, end = 0x{:08X} }}]\n\n\
             [[dispatch.rules]]\nid = \"copy-completed\"\ncontainer = \"main\"\nfunction = \"memcpy\"\nevent = \"return\"\n\
             effect = \"publish\"\nplacement = \"slot-b\"\ndestination = \"arg:0\"\nlength = \"arg:2\"\nsource_offset = \"const:0\"\n",
            return_rule("load_module"),
            lay.slot,
            lay.slot + SLOT_BYTES
        ),
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
fn d05_explicit_memcpy(lay: &Layout, n: &Names) -> Fixture {
    d05_explicit(lay, n, Loader::MemcpyC)
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

/// The slot function is named `abs`, which the linker otherwise lowers to
/// an intrinsic: a changing slot's call must stay a lookup.
fn d05_explicit_intrinsic_slot(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::MatchingC);
    let slot_name = format!("func_{:08X}", lay.slot);
    for im in &mut f.images {
        if im.id == n.of("a") || im.id == n.of("b") {
            for func in im.funcs.iter_mut().filter(|func| func.0 == "f") {
                func.0 = "abs".into();
            }
        }
        im.c = im.c.take().map(|c| c.replace(&slot_name, "abs").replace("int f(", "int abs(").replace("= f(a)", "= abs(a)"));
    }
    f
}

/// save_ptr stores into the slot's code, a write analysis knows; the only
/// rule at its site invalidates an unrelated constant range.
fn d05_explicit_uncovered_write(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::Lifted);
    let main = &mut f.images[0];
    let at = (main.addr("save_ptr") - main.base) as usize;
    let dest = lay.slot + 4;
    // save_ptr's `li t0, saved` (lui; ori) now names the slot's code.
    main.bytes[at..at + 4].copy_from_slice(&(0x3C08_0000 | dest >> 16).to_le_bytes());
    main.bytes[at + 4..at + 8].copy_from_slice(&(0x3508_0000 | dest & 0xFFFF).to_le_bytes());
    let rule = format!(
        "\n[[dispatch.rules]]\nid = \"unrelated-range\"\ncontainer = \"main\"\nfunction = \"save_ptr\"\nevent = \"return\"\n\
         effect = \"invalidate\"\ndestination = \"const:0x{:08X}\"\nlength = \"const:4\"\n",
        lay.stage
    );
    f.dispatch = f.dispatch.take().map(|d| d + &rule);
    f.script = Script::default();
    f.build = Build::Rejected("binding.uncovered_writer: writer `pointer-store`".into());
    f
}

/// Main's C declares the slot function `const`: clang may merge two calls
/// to it across a load, so the build refuses the attribute.
fn d05_explicit_const_callee(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::MatchingC);
    let decl = format!("int func_{:08X}(int a);", lay.slot);
    let c = f.images[0].c.take().unwrap_or_default();
    assert!(c.contains(&decl), "main's C declares the slot function");
    f.images[0].c = Some(c.replace(&decl, &decl.replace(");", ") __attribute__((const));")));
    f.script = Script::default();
    f.build = Build::Rejected("binding.elidable_call".into());
    f
}

/// A's untouched k calls the slot. A's f writes nothing, but B's f clears
/// the second region's code: k's writes are whatever the slot has bound.
fn d05_explicit_callee_candidates(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::Lifted);
    let mut u = Unit::new(&n.of("a"), lay.slot);
    leaf(&mut u, "f", |a| {
        a.addiu(V0, A0, 0x100);
    });
    u.func_at("k", lay.slot + 0x80);
    u.a.addiu(SP, SP, -24).sw(RA, 16, SP).jal_abs(lay.slot).nop().lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
    let a = u.image(Some(lay.slot + 0x100), &[]);
    let mut u = Unit::new(&n.of("b"), lay.slot);
    leaf(&mut u, "f", |a| {
        a.li(T0, lay.slot2).sw(ZERO, 0, T0);
    });
    let b = u.image(Some(lay.slot + 0x80), &[]);
    for im in &mut f.images {
        if im.id == a.id {
            *im = a.clone();
        } else if im.id == b.id {
            *im = b.clone();
        }
    }
    let writer = format!("\n[[dispatch.writers]]\nid = \"slot-caller\"\ncontainer = \"{}\"\nfunction = \"k\"\n", n.of("a"));
    f.dispatch = f.dispatch.take().map(|d| d + &writer);
    f.script = Script::default();
    f.build = Build::Rejected("binding.uncovered_writer: writer `slot-caller`".into());
    f
}

/// A's symbol file sizes its f to end before its return's delay slot, so
/// the original doesn't translate (the CPU runs it regardless); its
/// supplied C is still what the slot binds.
fn d05_explicit_c_untranslatable(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::MatchingC);
    let a = f.images.iter_mut().find(|im| im.id == n.of("a")).expect("module a");
    let jr_ra = 0x03E0_0008u32.to_le_bytes();
    let ret = a.bytes.chunks(4).position(|w| w == jr_ra).expect("f returns") as u32 * 4;
    for func in a.funcs.iter_mut().filter(|func| func.0 == "f") {
        func.2 = Some(ret + 4);
    }
    f
}

/// Main's C defines the marker function itself: it would run instead of
/// the publication the rule describes.
fn d05_explicit_defined_marker(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::MarkerC);
    let c = f.images[0].c.take().unwrap_or_default();
    f.images[0].c = Some(c + "void psxw_binding_site_module_written(unsigned d, unsigned l, unsigned o, unsigned s, unsigned k) {}\n");
    f.script = Script::default();
    f.build = Build::Rejected("binding.defined_marker".into());
    f
}

/// A rule invalidating the address space's last word: its range ends at
/// 2^32, and changes no placement.
fn d05_explicit_top_word(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::Lifted);
    let rule = "\n[[dispatch.rules]]\nid = \"top-word\"\ncontainer = \"main\"\nfunction = \"save_ptr\"\nevent = \"return\"\n\
                effect = \"invalidate\"\ndestination = \"const:0xFFFFFFFC\"\nlength = \"const:4\"\n";
    f.dispatch = f.dispatch.take().map(|d| d + rule);
    f
}

/// The pointer store's bound, declared through the uncached alias of the
/// address it writes: the same bytes (D5).
fn d05_explicit_alias_bounds(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::Lifted);
    let alias = (lay.stage + 0x1000) | 0x2000_0000;
    let declared = format!("function = \"save_ptr\"\ndestinations = [{{ start = 0x{alias:08X}, end = 0x{:08X} }}]\n", alias + 4);
    f.dispatch = f.dispatch.take().map(|d| d.replace("function = \"save_ptr\"\n", &declared));
    f
}

// The same facts in another representation: each variant below restates an
// earlier one through a different container, address alias, register, range
// encoding or o32 ABI route, and must mean the same thing.

/// Append function `name` (its symbol moves there) to `im`'s text.
fn append(im: &mut Image, name: &str, body: impl FnOnce(&mut Asm)) -> u32 {
    assert_eq!(im.text as usize, im.bytes.len(), "{}: appended code follows the text", im.id);
    let at = im.base + im.text;
    let mut a = Asm::new(at);
    body(&mut a);
    let bytes = a.bytes().expect("appended code assembles");
    im.bytes.extend_from_slice(&bytes);
    im.text = im.bytes.len() as u32;
    im.funcs.retain(|f| f.0 != name);
    im.funcs.push((name.to_string(), at, Some(bytes.len() as u32)));
    at
}

/// [`d05_explicit_const_callee`] from another container: module a's C
/// declares main's loader, a declared operation, `const`.
fn d05_explicit_overlay_const(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::MatchingC);
    let a = f.images.iter_mut().find(|im| im.id == n.of("a")).expect("module a");
    let c = a.c.take().expect("module a's C");
    a.c = Some(format!(
        "struct load_req {{ unsigned sel; unsigned *dst; int len; }};\nint load_module(struct load_req r) __attribute__((const));\n{c}"
    ));
    f.script = Script::default();
    f.build = Build::Rejected("binding.elidable_call".into());
    f
}

/// [`d05_explicit_callee_candidates`] with module b compiled at the slot's
/// KSEG1 alias: the same slot, so k's call can still reach it.
fn d05_explicit_alias_candidate(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit_callee_candidates(lay, n);
    let b = f.images.iter_mut().find(|im| im.id == n.of("b")).expect("module b");
    b.base |= 0x2000_0000;
    for func in &mut b.funcs {
        func.1 |= 0x2000_0000;
    }
    f
}

/// [`d05_explicit_uncovered_write`]'s store into the slot's code, through
/// `$sp` holding the slot's address (saved and restored around it).
fn d05_explicit_sp_write(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::Lifted);
    append(&mut f.images[0], "sp_store", |a| {
        a.mov(T1, SP).li(SP, lay.slot + 4).sw(ZERO, 0, SP).mov(SP, T1).jr(RA).ori(V0, ZERO, 0);
    });
    let writer = "\n[[dispatch.writers]]\nid = \"sp-writer\"\ncontainer = \"main\"\nfunction = \"sp_store\"\n";
    f.dispatch = f.dispatch.take().map(|d| d + writer);
    f.script = Script::default();
    f.build = Build::Rejected("binding.uncovered_writer: writer `sp-writer`".into());
    f
}

/// [`d05_explicit_cleared`]'s invalidation as a constant range that wraps
/// the window (D5): from its last word through the slot's bytes.
fn d05_explicit_wrapped_range(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::Lifted);
    let len = 4 + (lay.slot & psxw_abi::WINDOW_MASK) + SLOT_BYTES;
    let rule = format!(
        "\n[[dispatch.rules]]\nid = \"wrapped\"\ncontainer = \"main\"\nfunction = \"save_ptr\"\nevent = \"return\"\n\
         effect = \"invalidate\"\ndestination = \"const:0x{:08X}\"\nlength = \"const:0x{len:X}\"\n",
        0x8000_0000 | psxw_abi::WINDOW_MASK & !3
    );
    f.dispatch = f.dispatch.take().map(|d| d + &rule);
    let main = &f.images[0];
    let (lm, slot) = (main.addr("load_module"), main.addr("call_slot"));
    let len = f.images[1].bytes.len() as u32;
    let mut s = Script::default();
    s.call(lm, [0, lay.slot, len, 0], 0, &[]).call(slot, [1, 0, 0, 0], 0x101, &[0x101]);
    s.call(main.addr("save_ptr"), [lay.slot, 0, 0, 0], 0, &[]).untranslated(slot, [1, 0, 0, 0], lay.slot);
    f.script = s;
    f
}

/// [`d05_explicit_mixed`]'s loader as `load_module(unsigned tag, ...)`,
/// taking a selector (`cookie`: then an unused 64-bit word), the destination
/// and the length, each load made from C (`load_c`) and from lifted code
/// (`load_lift`, the same MIPS with no C). A 64-bit selector: o32 pads before
/// it (words 2, 3) and clang's va-buffer doesn't. A 32-bit selector and the
/// cookie: clang's va-buffer pads before the cookie and o32 doesn't (selector
/// word 1). Destination and length are words 4, 5. The rule names the
/// original's o32 words, whichever route the arguments come by.
fn d05_explicit_variadic(lay: &Layout, n: &Names, cookie: bool) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::MatchingC);
    let main = &mut f.images[0];
    let lm = main.addr("load_module");
    let sel = if cookie { A1 } else { A2 };
    let mut a = Asm::new(lm);
    a.addiu(SP, SP, -24).sw(RA, 16, SP).lw(T3, 40, SP).lw(T2, 44, SP).sll(T0, sel, 10).li(T1, lay.stage).addu(T1, T1, T0);
    a.beq(T2, ZERO, "lm_done").nop();
    a.label("lm_loop");
    a.lw(T0, 0, T1).addiu(T1, T1, 4).sw(T0, 0, T3).addiu(T2, T2, -4).bne(T2, ZERO, "lm_loop").addiu(T3, T3, 4);
    a.label("lm_done");
    a.jal_abs(main.addr("flush")).nop().lw(RA, 16, SP).ori(V0, ZERO, 0).jr(RA).addiu(SP, SP, 24);
    let bytes = a.bytes().expect("load_module assembles");
    let next = main.funcs.iter().map(|f| f.1).filter(|&x| x > lm).min().expect("a function follows load_module");
    assert!(lm + bytes.len() as u32 <= next, "the variadic load_module fits in place");
    let at = (lm - main.base) as usize;
    main.bytes[at..at + bytes.len()].copy_from_slice(&bytes);
    main.funcs.iter_mut().find(|f| f.0 == "load_module").expect("load_module").2 = Some(bytes.len() as u32);
    // (sel, dst, len): load_module(0, sel, [0ull,] dst, len). Word 6, which
    // nothing passes, is zero.
    let caller = |a: &mut Asm| {
        a.addiu(SP, SP, -40).sw(RA, 32, SP).sw(A1, 16, SP).sw(A2, 20, SP).sw(ZERO, 24, SP).mov(sel, A0);
        if cookie {
            a.ori(A2, ZERO, 0);
        }
        a.ori(A3, ZERO, 0).ori(A0, ZERO, 0).jal_abs(lm).nop().lw(RA, 32, SP).nop().jr(RA).addiu(SP, SP, 40);
    };
    let load_c = append(main, "load_c", caller);
    let load_lift = append(main, "load_lift", caller);
    let (take, pass) = if cookie {
        ("sel = __builtin_va_arg(ap, unsigned);\n    (void)__builtin_va_arg(ap, unsigned long long);", "sel, 0ull")
    } else {
        ("sel = (unsigned)__builtin_va_arg(ap, unsigned long long);", "(unsigned long long)sel")
    };
    main.c = Some(format!(
        "void flush(void);\n\
         int load_module(unsigned tag, ...) {{\n\
         \x20   __builtin_va_list ap;\n\
         \x20   unsigned sel, *dst, *src, len;\n\
         \x20   __builtin_va_start(ap, tag);\n\
         \x20   {take}\n\
         \x20   dst = __builtin_va_arg(ap, unsigned *);\n\
         \x20   len = __builtin_va_arg(ap, unsigned);\n\
         \x20   __builtin_va_end(ap);\n\
         \x20   src = (unsigned *)(0x{stage:08X}u + sel * 0x400u);\n\
         \x20   while (len != 0) {{ *dst++ = *src++; len -= 4; }}\n\
         \x20   flush();\n\
         \x20   return 0;\n\
         }}\n\
         int load_c(unsigned sel, unsigned *dst, unsigned len) {{ return load_module(0u, {pass}, dst, len); }}\n",
        stage = lay.stage
    ));
    let word = if cookie { "selector = \"arg:1\"" } else { "selector = \"arg:2\"" };
    f.dispatch = f.dispatch.take().map(|d| {
        d.replace("selector = \"arg:0\"", word)
            .replace("destination = \"arg:1\"\nlength = \"arg:2\"\nsource_offset", "destination = \"arg:4\"\nlength = \"arg:5\"\nsource_offset")
    });
    let len_a = f.images[1].bytes.len() as u32;
    let mut s = Script::default();
    for (via, sel, len, k) in [(load_c, 0, len_a, 0x100), (load_lift, 1, 0x80, 0x200), (load_lift, 0, len_a, 0x100), (load_c, 1, 0x80, 0x200)] {
        s.call(via, [sel, lay.slot, len, 0], 0, &[]).call(lay.slot, [1, 0, 0, 0], k + 1, &[k + 1]);
    }
    f.script = s;
    f
}

fn d05_explicit_variadic_selector(lay: &Layout, n: &Names) -> Fixture {
    d05_explicit_variadic(lay, n, false)
}
fn d05_explicit_variadic_padding(lay: &Layout, n: &Names) -> Fixture {
    d05_explicit_variadic(lay, n, true)
}

/// [`d05_explicit_sp_write`]'s store moved into a helper: the writer points
/// `$sp` at the slot's code and calls it. The helper's `$sp` is no stack, so
/// analysis can't bound the writer, which declares no bounds.
fn d05_explicit_sp_callee(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::Lifted);
    let main = &mut f.images[0];
    let helper = append(main, "sp_helper", |a| {
        a.sw(ZERO, 0, SP).jr(RA).ori(V0, ZERO, 0);
    });
    append(main, "sp_caller", |a| {
        a.mov(S0, SP).mov(S1, RA).li(SP, lay.slot).jal_abs(helper).nop().mov(SP, S0).mov(RA, S1).jr(RA).nop();
    });
    let writer = "\n[[dispatch.writers]]\nid = \"sp-call-writer\"\ncontainer = \"main\"\nfunction = \"sp_caller\"\n";
    f.dispatch = f.dispatch.take().map(|d| d + writer);
    f.script = Script::default();
    f.build = Build::Rejected("binding.invalid_fact: writer `sp-call-writer`: analysis can't bound its writes".into());
    f
}

/// [`d05_explicit_cleared`] with a clearer returning a struct: o32 returns
/// its hidden destination in v0, which `success = "nonzero"` tests.
fn d05_explicit_sret(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::Lifted);
    let main = &mut f.images[0];
    // clear(ret, dst, len): zero len bytes at dst; *ret = {1, 2}; v0 = ret.
    let clear = append(main, "clear", |a| {
        a.beq(A2, ZERO, "clr_done").nop();
        a.label("clr_loop");
        a.sw(ZERO, 0, A1).addiu(A2, A2, -4).bne(A2, ZERO, "clr_loop").addiu(A1, A1, 4);
        a.label("clr_done");
        a.ori(T0, ZERO, 1).sw(T0, 0, A0).ori(T0, ZERO, 2).sw(T0, 4, A0).jr(RA).mov(V0, A0);
    });
    main.c = Some(
        "struct cleared { unsigned lo, hi; };\n\
         struct cleared clear(unsigned *dst, unsigned len) {\n\
         \x20   struct cleared r;\n\
         \x20   while (len != 0) { *dst++ = 0; len -= 4; }\n\
         \x20   r.lo = 1;\n\
         \x20   r.hi = 2;\n\
         \x20   return r;\n\
         }\n"
            .into(),
    );
    f.dispatch = f.dispatch.take().map(|d| {
        d.replace("effect = \"invalidate\"\ndestination = \"arg:0\"\nlength = \"arg:1\"", "effect = \"invalidate\"\ndestination = \"arg:1\"\nlength = \"arg:2\"\nsuccess = \"nonzero\"")
    });
    let main = &f.images[0];
    let (lm, slot) = (main.addr("load_module"), main.addr("call_slot"));
    let (len, out) = (f.images[1].bytes.len() as u32, lay.stage + 0x1100);
    let mut s = Script::default();
    s.call(lm, [0, lay.slot, len, 0], 0, &[]).call(slot, [1, 0, 0, 0], 0x101, &[0x101]);
    s.call(clear, [out, lay.slot, len, 0], out, &[]).untranslated(slot, [1, 0, 0, 0], lay.slot);
    f.script = s;
    f
}

/// [`d05_explicit_marker`]'s rule stating its destination literally, as
/// the KSEG1 alias of the address the C passes: the same bytes.
fn d05_explicit_marker_alias(lay: &Layout, n: &Names) -> Fixture {
    let mut f = d05_explicit(lay, n, Loader::MarkerC);
    let rule = write_rule(&f.images[0], Some("module_written"), "placement");
    let alias = format!("destination = \"const:0x{:08X}\"", lay.slot2 | 0x2000_0000);
    f.dispatch = Some(explicit_facts(n, lay, &rule.replace("destination = \"write-address\"", &alias)));
    let main = &f.images[0];
    let (lm, slot2) = (main.addr("load_module"), main.addr("call_slot2"));
    let len = f.images[3].bytes.len() as u32;
    let mut s = Script::default();
    s.call(lm, [2, lay.slot2, len, 0], 0, &[]).call(slot2, [0; 4], 0xC1, &[0xC1]);
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


// ---- D06 native references (plans/dispatch-binding B) -------------------

/// The targets every native-reference fixture takes the address of:
/// `twice` (C), `thrice` (lifted) and `quad` (C), each logging and
/// returning 2a, 3a and 4a.
fn native_targets(u: &mut Unit, lay: &Layout) {
    let a = u.func("twice");
    a.addu(V0, A0, A0);
    mark(a, lay, V0);
    a.jr(RA).nop();
    let a = u.func("thrice");
    a.addu(V0, A0, A0).addu(V0, V0, A0);
    mark(a, lay, V0);
    a.jr(RA).nop();
    let a = u.func("quad");
    a.sll(V0, A0, 2);
    mark(a, lay, V0);
    a.jr(RA).nop();
}

/// The C of [`native_targets`] (thrice stays lifted), the log macro and
/// `fn_t`. With `edited`, twice also logs [`EDIT_MARK`], which its MIPS
/// doesn't.
fn native_c(lay: &Layout, edited: bool) -> String {
    let edit = if edited { format!("LOG(0x{EDIT_MARK:X}u); ") } else { String::new() };
    format!(
        "#define LOG(v) (*(*(unsigned **)0x{log:08X}u)++ = (unsigned)(v))\n\
         typedef int (*fn_t)(int);\n\
         int thrice(int a);\n\
         int twice(int a) {{ int v = a + a; {edit}LOG(v); return v; }}\n\
         int quad(int a) {{ int v = a << 2; LOG(v); return v; }}\n",
        log = lay.log
    )
}

/// Fixed address-taking in C: `use_fixed` takes its target from a helper's
/// return (`pick`), compares it with `&twice` and calls it through another
/// helper's parameter (`apply`), which tests it for null: a closed flow of
/// fixed targets, native. The helpers are source-only: no address, as a
/// decomp's own helpers are. Controls in the same image keep their guest
/// addresses: `escape` saves `&twice` where lifted code reads, logs and
/// calls it (opaque escape: the log shows the original address), and
/// `numeric` adds `&thrice`'s low bits to its call's result (numeric
/// observation).
fn native_fixed_with(lay: &Layout, edited: bool) -> Fixture {
    let saved = lay.stage + 0x1000;
    let mut main = main_image(lay, |u| {
        native_targets(u, lay);
        u.func("save_ptr").li(T0, saved).sw(A0, 0, T0).jr(RA).ori(V0, ZERO, 0);
        let a = u.func("call_saved");
        a.addiu(SP, SP, -24).sw(RA, 16, SP).li(T0, saved).lw(T1, 0, T0).nop();
        mark(a, lay, T1);
        a.jalr(T1).nop().lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
        // use_fixed(which, a): f = which ? thrice : twice; 0xEE if f is
        // twice; f(a) + 1.
        let a = u.func("use_fixed");
        a.addiu(SP, SP, -24).sw(RA, 16, SP).la(T1, "thrice").bne(A0, ZERO, "uf_picked").nop().la(T1, "twice");
        a.label("uf_picked");
        a.la(T2, "twice").bne(T1, T2, "uf_call").nop();
        mark_k(a, lay, 0xEE);
        a.label("uf_call");
        a.jalr(T1).mov(A0, A1).lw(RA, 16, SP).addiu(V0, V0, 1).jr(RA).addiu(SP, SP, 24);
        // escape(a): save &twice, then call_saved(a).
        let a = u.func("escape");
        a.addiu(SP, SP, -24).sw(RA, 16, SP).sw(A0, 20, SP).la(A0, "twice").jal("save_ptr").nop();
        a.lw(A0, 20, SP).jal("call_saved").nop().lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
        // numeric(a): thrice(a) + (&thrice & 0xFFF).
        let a = u.func("numeric");
        a.addiu(SP, SP, -24).sw(RA, 16, SP).la(T0, "thrice").andi(T1, T0, 0xFFF).sw(T1, 20, SP).jalr(T0).nop();
        a.lw(T1, 20, SP).lw(RA, 16, SP).addu(V0, V0, T1).jr(RA).addiu(SP, SP, 24);
    });
    main.c = Some(format!(
        "{}void save_ptr(fn_t p);\n\
         int call_saved(int a);\n\
         fn_t pick(int which) {{ return which ? thrice : twice; }}\n\
         int apply(fn_t f, int a) {{ return f ? f(a) + 1 : -1; }}\n\
         int use_fixed(int which, int a) {{\n\
         \x20   fn_t f = pick(which);\n\
         \x20   if (f == twice) LOG(0xEEu);\n\
         \x20   return apply(f, a);\n\
         }}\n\
         int escape(int a) {{ save_ptr(twice); return call_saved(a); }}\n\
         int observe(fn_t f, int a) {{ return f(a) + (int)((unsigned)f & 0xFFFu); }}\n\
         int numeric(int a) {{ return observe(thrice, a); }}\n",
        native_c(lay, edited)
    ));
    let (twice, thrice) = (main.addr("twice"), main.addr("thrice"));
    let edit: &[u32] = if edited { &[EDIT_MARK] } else { &[] };
    let mut s = Script::default();
    let uf = main.addr("use_fixed");
    s.call(uf, [0, 5, 0, 0], 11, &[[0xEE].as_slice(), edit, &[10]].concat());
    s.call(uf, [1, 5, 0, 0], 16, &[15]);
    s.call(main.addr("escape"), [6, 0, 0, 0], 12, &[[twice].as_slice(), edit, &[12]].concat());
    s.call(main.addr("numeric"), [7, 0, 0, 0], 21 + (thrice & 0xFFF), &[21]);
    let mut f = Fixture::new(vec![main], s);
    f.native = Some(NativeExpect {
        handles: vec![(format!("main 0x{twice:08X}"), "c"), (format!("main 0x{thrice:08X}"), "lift")],
        calls: 1,
        native: pairs(&[("pick", "thrice"), ("pick", "twice"), ("use_fixed", "twice")]),
        retained: kept(&[("escape", "twice", "opaque_escape"), ("numeric", "thrice", "numeric_observation")]),
        ..Default::default()
    });
    f
}

fn native_fixed(lay: &Layout, _: &Names) -> Fixture {
    native_fixed_with(lay, false)
}

/// The same flows with twice's C deliberately edited: the native call
/// enters the authored C, not its original's translation.
fn native_edited(lay: &Layout, _: &Names) -> Fixture {
    native_fixed_with(lay, true)
}

/// A relocated pointer table: C's `static const` table of `twice`,
/// `thrice`, null and `quad` lives in AUX (clang's, no original address),
/// read only by loads of whole cells. Its cells hold handles and its calls
/// go through the native table; the original's table is in its data.
/// Calling the null cell without a test is a native-handle rejection.
fn native_table(lay: &Layout, _: &Names) -> Fixture {
    let mut u = main_unit(lay, |u| {
        native_targets(u, lay);
        // call_op(i, a): f = ops[i & 3]; f ? f(a) : -1.
        let a = u.func("call_op");
        a.addiu(SP, SP, -24).sw(RA, 16, SP).andi(T0, A0, 3).sll(T0, T0, 2).la(T1, "ops").addu(T1, T1, T0).lw(T1, 0, T1).nop();
        a.beq(T1, ZERO, "op_null").nop().jalr(T1).mov(A0, A1).lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
        a.label("op_null");
        a.lw(RA, 16, SP).addiu(V0, ZERO, -1).jr(RA).addiu(SP, SP, 24);
        // call_op_raw(i, a): ops[i & 3](a).
        let a = u.func("call_op_raw");
        a.addiu(SP, SP, -24).sw(RA, 16, SP).andi(T0, A0, 3).sll(T0, T0, 2).la(T1, "ops").addu(T1, T1, T0).lw(T1, 0, T1).nop();
        a.jalr(T1).mov(A0, A1).lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
    });
    let at = |n: &str| u.a.addr_of(n).expect("a target");
    let table = [at("twice"), at("thrice"), 0, at("quad")];
    let ops = u.a.here();
    u.a.define("ops", ops);
    let mut main = u.image(None, &words(&table));
    main.c = Some(format!(
        "{}int quad(int a);\n\
         static const fn_t ops[4] = {{ twice, thrice, 0, quad }};\n\
         int call_op(int i, int a) {{ fn_t f = ops[i & 3]; return f ? f(a) : -1; }}\n\
         int call_op_raw(int i, int a) {{ return ops[i & 3](a); }}\n",
        native_c(lay, false)
    ));
    let (op, raw) = (main.addr("call_op"), main.addr("call_op_raw"));
    let mut s = Script::default();
    s.call(op, [0, 5, 0, 0], 10, &[10]);
    s.call(op, [1, 5, 0, 0], 15, &[15]);
    s.call(op, [2, 5, 0, 0], u32::MAX, &[]);
    s.call(op, [7, 5, 0, 0], 20, &[20]);
    s.call(raw, [1, 7, 0, 0], 21, &[21]);
    // The original jumps to address 0: outside the supported domain.
    s.reject(raw, [2, 7, 0, 0], "binding.native_handle 0x00000000".into());
    let mut f = Fixture::new(vec![main], s);
    f.native = Some(NativeExpect {
        handles: vec![(format!("main 0x{:08X}", table[0]), "c"), (format!("main 0x{:08X}", table[1]), "lift"), (format!("main 0x{:08X}", table[3]), "c")],
        calls: 2,
        native: pairs(&[("ops", "twice"), ("ops", "thrice"), ("ops", "quad")]),
        retained: vec![],
        ..Default::default()
    });
    f
}

/// A mixed-tier flow: C calls a lifted six-argument function through a
/// helper's parameter, natively, its stack words and result through o32 as
/// before. C calling what lifted code returns keeps a guest address: a
/// lifted return doesn't carry the representation word.
fn native_mixed(lay: &Layout, _: &Names) -> Fixture {
    let mut main = main_image(lay, |u| {
        native_targets(u, lay);
        // sum6(a..f) = a + .. + f, marked.
        let a = u.func("sum6");
        a.lw(T0, 16, SP).lw(T1, 20, SP).addu(V0, A0, A1).addu(V0, V0, A2).addu(V0, V0, A3).addu(V0, V0, T0).addu(V0, V0, T1);
        mark(a, lay, V0);
        a.jr(RA).nop();
        // use_sum6(a) = sum6(a, 2..6) + sum6(a + 10, 2..6).
        let a = u.func("use_sum6");
        a.addiu(SP, SP, -40).sw(RA, 32, SP).sw(A0, 28, SP);
        a.ori(T0, ZERO, 5).sw(T0, 16, SP).ori(T0, ZERO, 6).sw(T0, 20, SP).ori(A1, ZERO, 2).ori(A2, ZERO, 3).jal("sum6").ori(A3, ZERO, 4);
        a.sw(V0, 24, SP).lw(A0, 28, SP);
        a.ori(T0, ZERO, 5).sw(T0, 16, SP).ori(T0, ZERO, 6).sw(T0, 20, SP).addiu(A0, A0, 10).ori(A1, ZERO, 2).ori(A2, ZERO, 3);
        a.jal("sum6").ori(A3, ZERO, 4).lw(T1, 24, SP).lw(RA, 32, SP).addu(V0, V0, T1).jr(RA).addiu(SP, SP, 40);
        // get_fn() = &thrice; from_lift(a) = get_fn()(a).
        u.func("get_fn").la(V0, "thrice").jr(RA).nop();
        let a = u.func("from_lift");
        a.addiu(SP, SP, -24).sw(RA, 16, SP).sw(A0, 20, SP).jal("get_fn").nop().lw(A0, 20, SP).jalr(V0).nop();
        a.lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
    });
    main.c = Some(format!(
        "{}typedef int (*fn6_t)(int, int, int, int, int, int);\n\
         int sum6(int a, int b, int c, int d, int e, int f);\n\
         fn_t get_fn(void);\n\
         int apply6(fn6_t f, int a) {{ return f(a, 2, 3, 4, 5, 6); }}\n\
         int use_sum6(int a) {{ int x = apply6(sum6, a); return x + apply6(sum6, a + 10); }}\n\
         int from_lift(int a) {{ return get_fn()(a); }}\n",
        native_c(lay, false)
    ));
    let mut s = Script::default();
    s.call(main.addr("use_sum6"), [1, 0, 0, 0], 52, &[21, 31]);
    s.call(main.addr("from_lift"), [5, 0, 0, 0], 15, &[15]);
    let mut f = Fixture::new(vec![main.clone()], s);
    f.native = Some(NativeExpect {
        handles: vec![(format!("main 0x{:08X}", main.addr("sum6")), "lift")],
        calls: 1,
        native: pairs(&[("use_sum6", "sum6")]),
        ..Default::default()
    });
    f
}

/// Devirtualization, not native references: a lifted pointer call's target
/// passed to an inlined callee in a temporary (outside the o32 ABI:
/// `reg_call` reads `t0`), and one an inlined callee leaves in a temporary
/// for its caller (`get_reg` sets `t1`). The lifter's dataflow over the
/// expanded translation proves both targets, so the calls bind as the
/// dispatch table would bind those pointers (`psxw_p_`), without a lookup.
/// A target in an ABI argument (`call_ptr`'s) is a run-time value: still a
/// dispatch-table call.
fn lift_devirtualized(lay: &Layout, _: &Names) -> Fixture {
    let main = main_image(lay, |u| {
        native_targets(u, lay);
        let a = u.func("reg_call");
        a.addiu(SP, SP, -24).sw(RA, 16, SP).jalr(T0).nop().lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
        let a = u.func("pass_reg");
        a.addiu(SP, SP, -24).sw(RA, 16, SP).la(T0, "thrice").jal("reg_call").nop().lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
        u.func("get_reg").la(T1, "quad").jr(RA).nop();
        let a = u.func("use_ret");
        a.addiu(SP, SP, -24).sw(RA, 16, SP).sw(A0, 20, SP).jal("get_reg").nop().lw(A0, 20, SP).jalr(T1).nop();
        a.lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
        let a = u.func("pass_abi");
        a.addiu(SP, SP, -24).sw(RA, 16, SP).mov(A1, A0).la(A0, "twice").jal("call_ptr").nop().lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
    });
    let mut s = Script::default();
    s.call(main.addr("pass_reg"), [4, 0, 0, 0], 12, &[12]);
    s.call(main.addr("use_ret"), [5, 0, 0, 0], 20, &[20]);
    s.call(main.addr("pass_abi"), [6, 0, 0, 0], 12, &[12]);
    let (thrice, quad) = (main.addr("thrice"), main.addr("quad"));
    let mut f = Fixture::new(vec![main], s);
    f.native = Some(NativeExpect {
        devirtualized: vec![("pass_reg".into(), thrice), ("use_ret".into(), quad)],
        not_devirtualized: vec!["call_ptr".into(), "pass_abi".into()],
        ..Default::default()
    });
    f
}

/// Native-handle transport through a lifted argument register: C passes
/// `&twice` to lifted `call_ptr`, which only moves its `a0` to `t0` and
/// calls through it. The C call sets the representation word's bit 0, and
/// `call_ptr`'s jalr calls through the native table; a guest pointer from
/// the host takes the same body through the dispatch table (bit 0 clear).
/// And back: lifted `ident` returns its `a0`, so `pass_through`'s handle
/// returns to C, which calls it natively; the host's guest pointer comes
/// back as the address it was.
/// Controls: `log_call` stores its pointer in the log (an escape: the log
/// shows the original address), and `forward` passes it on to `call_ptr`,
/// a use the proof doesn't follow (not proven), so both keep guest
/// addresses.
fn native_lift_transport(lay: &Layout, _: &Names) -> Fixture {
    let mut main = main_image(lay, |u| {
        native_targets(u, lay);
        // log_call(f, a): log f, then f(a).
        let a = u.func("log_call");
        a.addiu(SP, SP, -24).sw(RA, 16, SP).mov(T0, A0);
        mark(a, lay, T0);
        a.jalr(T0).mov(A0, A1).lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
        // forward(f, a) = call_ptr(f, a).
        let a = u.func("forward");
        a.addiu(SP, SP, -24).sw(RA, 16, SP).jal("call_ptr").nop().lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
        // ident(f) = f.
        u.func("ident").jr(RA).mov(V0, A0);
        // pass_through(a) = ident(&twice)(a) + 1.
        let a = u.func("pass_through");
        a.addiu(SP, SP, -24).sw(RA, 16, SP).sw(A0, 20, SP).la(A0, "twice").jal("ident").nop().lw(A0, 20, SP).jalr(V0).nop();
        a.addiu(V0, V0, 1).lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
        for (name, target, callee) in [("pass_lift", "twice", "call_ptr"), ("pass_logged", "thrice", "log_call"), ("pass_forward", "quad", "forward")] {
            let a = u.func(name);
            a.addiu(SP, SP, -24).sw(RA, 16, SP).mov(A1, A0).la(A0, target).jal(callee).nop();
            a.lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
        }
    });
    main.c = Some(format!(
        "{}int call_ptr(fn_t f, int a, int b, int c);\n\
         int log_call(fn_t f, int a);\n\
         int forward(fn_t f, int a);\n\
         fn_t ident(fn_t f);\n\
         int pass_lift(int a) {{ return call_ptr(twice, a, 0, 0); }}\n\
         int pass_through(int a) {{ fn_t f = ident(twice); return f(a) + 1; }}\n\
         int pass_logged(int a) {{ return log_call(thrice, a); }}\n\
         int pass_forward(int a) {{ return forward(quad, a); }}\n",
        native_c(lay, false)
    ));
    let (twice, thrice, quad) = (main.addr("twice"), main.addr("thrice"), main.addr("quad"));
    let mut s = Script::default();
    s.call(main.addr("pass_lift"), [4, 0, 0, 0], 8, &[8]);
    s.call(main.addr("pass_logged"), [5, 0, 0, 0], 15, &[thrice, 15]);
    s.call(main.addr("pass_forward"), [6, 0, 0, 0], 24, &[24]);
    s.call(main.addr("call_ptr"), [quad, 7, 0, 0], 28, &[28]);
    s.call(main.addr("pass_through"), [9, 0, 0, 0], 19, &[18]);
    s.call(main.addr("ident"), [quad, 0, 0, 0], quad, &[]);
    let mut f = Fixture::new(vec![main], s);
    f.native = Some(NativeExpect {
        handles: vec![(format!("main 0x{twice:08X}"), "c")],
        native: pairs(&[("pass_lift", "twice"), ("pass_through", "twice")]),
        retained: kept(&[("pass_logged", "thrice", "opaque_escape"), ("pass_forward", "quad", "missing_uses")]),
        calls: 1,
        transports: 2,
        carriers: vec![("call_ptr".into(), 0), ("ident".into(), 0)],
        ..Default::default()
    });
    f
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

fn kept(v: &[(&str, &str, &'static str)]) -> Vec<(String, String, &'static str)> {
    v.iter().map(|(a, b, r)| (a.to_string(), b.to_string(), *r)).collect()
}

/// A changing-slot control under explicit binding: C takes the slot's
/// address (`drive_ref`), which keeps its guest address and follows the
/// binding A -> B -> A, while each load goes through a native reference to
/// the declared loader (`load_ref`): its instrumentation still publishes.
fn native_slot(lay: &Layout, n: &Names) -> Fixture {
    let mut main = explicit_main(lay, Loader::Lifted);
    append(&mut main, "drive_ref", |a| {
        a.addiu(SP, SP, -24).sw(RA, 16, SP).li(T0, lay.slot).jalr(T0).nop().lw(RA, 16, SP).nop().jr(RA).addiu(SP, SP, 24);
    });
    let lm = main.addr("load_module");
    append(&mut main, "load_ref", |a| {
        a.j_abs(lm).nop();
    });
    let slot_fn = format!("func_{:08X}", lay.slot);
    main.c = Some(format!(
        "typedef int (*fn_t)(int);\n\
         typedef int (*load_t)(unsigned sel, unsigned *dst, int len);\n\
         int {slot_fn}(int a);\n\
         int load_module(unsigned sel, unsigned *dst, int len);\n\
         int apply(fn_t f, int a) {{ return f(a); }}\n\
         int load_via(load_t l, unsigned sel, unsigned *dst, int len) {{ return l(sel, dst, len); }}\n\
         int drive_ref(int a) {{ return apply({slot_fn}, a); }}\n\
         int load_ref(unsigned sel, unsigned *dst, int len) {{ return load_via(load_module, sel, dst, len); }}\n"
    ));
    let (a, b, c) = explicit_images(lay, n, false);
    let len = |im: &Image| im.bytes.len() as u32;
    let (load, drive) = (main.addr("load_ref"), main.addr("drive_ref"));
    let mut s = Script::default();
    for (arg, sel, im, k) in [(1, 0, &a, 0x100), (2, 1, &b, 0x200), (3, 0, &a, 0x100)] {
        s.call(load, [sel, lay.slot, len(im), 0], 0, &[]);
        s.call(drive, [arg, 0, 0, 0], arg + k, &[arg + k]);
    }
    let init = vec![(lay.stage, a.bytes.clone()), (lay.stage + 0x400, b.bytes.clone()), (lay.stage + 0x800, c.bytes.clone())];
    let mut f = Fixture::new(vec![main, a, b, c], s);
    f.init = init;
    f.dispatch = Some(explicit_facts(n, lay, &return_rule("load_module")));
    f.native = Some(NativeExpect {
        handles: vec![(format!("main 0x{lm:08X}"), "operation")],
        calls: 1,
        native: pairs(&[("load_ref", "load_module")]),
        retained: kept(&[("drive_ref", &slot_fn, "late_binding")]),
        ..Default::default()
    });
    f
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
        d("D05", "explicit-binding-memcpy", "supported: explicit binding on a constant-length memcpy", REF, d05_explicit_memcpy),
        d("D05", "explicit-binding-c-marker", "supported: explicit binding from authored C's c_marker write site", REF, d05_explicit_marker),
        d("D05", "explicit-binding-missing-c-site", "rejected facts: a C write site without its c_marker", REF, d05_explicit_missing_c_site),
        d("D05", "explicit-binding-marker-elsewhere", "rejected facts: the marker call isn't in the function holding the write", REF, d05_explicit_marker_elsewhere),
        d("D05", "explicit-binding-marker-mismatch", "port rejection: a marker value contradicts its rule", REF, d05_explicit_marker_mismatch),
        d("D05", "explicit-binding-marker-offset", "port rejection: a marker's source offset contradicts the placement", REF, d05_explicit_marker_offset),
        d("D05", "explicit-binding-cleared", "port rejection: an invalidated module isn't callable", REF, d05_explicit_cleared),
        d("D05", "explicit-binding-intrinsic-slot", "supported: a changing slot named like an intrinsic is still looked up", REF, d05_explicit_intrinsic_slot),
        d("D05", "explicit-binding-uncovered-write", "rejected facts: a known code write its rule's constant range misses", REF, d05_explicit_uncovered_write),
        d("D05", "explicit-binding-c-untranslatable", "supported: supplied C binds where its original doesn't translate", REF, d05_explicit_c_untranslatable),
        d("D05", "explicit-binding-top-word", "supported: a constant range ending at 2^32", REF, d05_explicit_top_word),
        d("D05", "explicit-binding-alias-bounds", "supported: a writer bound declared through an address alias", REF, d05_explicit_alias_bounds),
        d("D05", "explicit-binding-const-callee", "rejected facts: C declares a changing slot's function const", REF, d05_explicit_const_callee),
        d("D05", "explicit-binding-callee-candidates", "rejected facts: a writer's slot call can reach another module's code write", REF, d05_explicit_callee_candidates),
        d("D05", "explicit-binding-defined-marker", "rejected facts: C defines its compiler-owned marker function", REF, d05_explicit_defined_marker),
        d("D05", "explicit-binding-overlay-const", "rejected facts: an overlay's C declares the resident loader const", REF, d05_explicit_overlay_const),
        d("D05", "explicit-binding-alias-candidate", "rejected facts: a slot call reaches a module compiled at the slot's KSEG1 alias", REF, d05_explicit_alias_candidate),
        d("D05", "explicit-binding-sp-write", "rejected facts: a known code write through $sp", REF, d05_explicit_sp_write),
        d("D05", "explicit-binding-sp-callee", "rejected facts: a callee's $sp store where its caller made $sp no stack", REF, d05_explicit_sp_callee),
        d(
            "D05",
            "explicit-binding-wrapped-range",
            "port rejection: a constant range wrapping the window invalidates what it reaches",
            Evidence::ClosedForm("a range wrapping the 16 MB window (D5) has no hardware counterpart"),
            d05_explicit_wrapped_range,
        ),
        d("D05", "explicit-binding-variadic", "supported: a variadic C loader's rule reads its o32 words, from C and lifted callers", REF, d05_explicit_variadic_selector),
        d(
            "D05",
            "explicit-binding-variadic-padding",
            "supported: a variadic C loader whose va-buffer pads where o32 doesn't, from C and lifted callers",
            REF,
            d05_explicit_variadic_padding,
        ),
        d("D05", "explicit-binding-sret", "port rejection: a struct-returning C clearer's success reads its o32 v0", REF, d05_explicit_sret),
        d("D05", "explicit-binding-marker-alias", "supported: a c_marker rule's literal destination names the C's through an alias", REF, d05_explicit_marker_alias),
        d("D05", "explicit-binding-wrong-writer", "rejected facts: a declared writer bound contradicts a known write", REF, d05_explicit_wrong_writer),
        d("D05", "explicit-binding-unknown-selector", "port rejection: a completed load names no placement", REF, d05_explicit_unknown_selector),
        d("D05", "explicit-binding-bad-destination", "port rejection: a load outside its placement's layout", REF, d05_explicit_bad_destination),
        d("D05", "explicit-binding-missing-placement", "rejected facts: an image overlapping a placement has none", REF, d05_explicit_missing_placement),
        d("D05", "explicit-binding-unimplemented", "rejected facts: an after-write rule is milestone C", REF, d05_explicit_unimplemented),
        d("D06", "lifted-routes", "supported", REF, d06_lifted),
        d("D06", "interior-entry", "supported", REF, d06_interior),
        d("D06", "c-routes", "supported", REF, d06_c),
        d("D06", "native-reference-fixed", "supported: fixed address-taking in C, native; escaping and numeric controls keep guest addresses", REF, native_fixed),
        d("D06", "native-reference-table", "supported: a relocated pointer table of handles; its null cell called is a native-handle rejection", REF, native_table),
        d("D06", "native-reference-mixed", "supported: C calls a lifted function natively, its stack words and result through o32", REF, native_mixed),
        d("D06", "native-reference-lift-transport", "supported: a native handle passes into a lifted argument register and its jalr calls natively; a lifted function storing or forwarding the pointer keeps it guest", REF, native_lift_transport),
        d("D06", "lift-devirtualized", "supported: lifted pointer calls whose targets dataflow proves across inlined callees are devirtualized; an ABI argument's stays a table call", REF, lift_devirtualized),
        d("D06", "native-reference-slot", "supported: a changing slot's address keeps its binding; a native reference reaches the loader's instrumentation", REF, native_slot),
        d(
            "D06",
            "native-reference-edited-c",
            "supported: a native reference enters the deliberately edited C",
            Evidence::ClosedForm("authored C edit: its log entry is absent from the original binary"),
            native_edited,
        ),
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
