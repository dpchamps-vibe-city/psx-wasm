//! R3000 semantics through the whole lift path: hand-assembled MIPS is
//! translated, compiled by the pinned clang, linked with the guest passes
//! and run under wasmtime. Expected values are psx-spx's ("CPU Arithmetic
//! Instructions", "CPU Load/Store", "CPU Branch/Jump"). These are debugging
//! aids for the translator; the oracle is the differential harness.
//!
//! Skips when the pinned toolchains haven't been fetched.

use psxw_difftest::runner::{Callout, Harness, Outcome, STACK_TOP};
use psxw_inputs::map::Container;
use psxw_inputs::{AddressMap, Workspace, originals::Originals};
use psxw_lift::program::{Code, Program, Source, Start};
use psxw_link::object::WasmObject;
use psxw_link::{ExportSpec, Input, Mode};
use std::collections::BTreeMap;

const BASE: u32 = 0x8001_0000;
const DATA: u32 = 0x8002_0000;
const V0: u32 = 2;
const V1: u32 = 3;
const A0: u32 = 4;
const A1: u32 = 5;
const S0: u32 = 16;
const RA: u32 = 31;

fn r(rs: u32, rt: u32, rd: u32, sa: u32, f: u32) -> u32 {
    (rs << 21) | (rt << 16) | (rd << 11) | (sa << 6) | f
}
fn i(op: u32, rs: u32, rt: u32, imm: i32) -> u32 {
    (op << 26) | (rs << 21) | (rt << 16) | (imm as u32 & 0xFFFF)
}
fn jr(rs: u32) -> u32 {
    r(rs, 0, 0, 0, 0x08)
}
fn div(rs: u32, rt: u32) -> u32 {
    r(rs, rt, 0, 0, 0x1A)
}
fn divu(rs: u32, rt: u32) -> u32 {
    r(rs, rt, 0, 0, 0x1B)
}
fn mflo(rd: u32) -> u32 {
    r(0, 0, rd, 0, 0x12)
}
fn mfhi(rd: u32) -> u32 {
    r(0, 0, rd, 0, 0x10)
}
fn addu(rd: u32, rs: u32, rt: u32) -> u32 {
    r(rs, rt, rd, 0, 0x21)
}
fn add(rd: u32, rs: u32, rt: u32) -> u32 {
    r(rs, rt, rd, 0, 0x20)
}
fn ori(rt: u32, rs: u32, imm: i32) -> u32 {
    i(0x0D, rs, rt, imm)
}
fn lui(rt: u32, imm: i32) -> u32 {
    i(0x0F, 0, rt, imm)
}
fn addiu(rt: u32, rs: u32, imm: i32) -> u32 {
    i(0x09, rs, rt, imm)
}
fn lw(rt: u32, off: i32, base: u32) -> u32 {
    i(0x23, base, rt, off)
}
fn lwl(rt: u32, off: i32, base: u32) -> u32 {
    i(0x22, base, rt, off)
}
fn lwr(rt: u32, off: i32, base: u32) -> u32 {
    i(0x26, base, rt, off)
}
fn swl(rt: u32, off: i32, base: u32) -> u32 {
    i(0x2A, base, rt, off)
}
fn swr(rt: u32, off: i32, base: u32) -> u32 {
    i(0x2E, base, rt, off)
}
fn beq(rs: u32, rt: u32, words: i32) -> u32 {
    i(0x04, rs, rt, words)
}
fn bne(rs: u32, rt: u32, words: i32) -> u32 {
    i(0x05, rs, rt, words)
}
fn jal(target: u32) -> u32 {
    (3 << 26) | ((target & 0x0FFF_FFFF) >> 2)
}
const NOP: u32 = 0;

struct Run {
    outcome: Outcome,
    #[allow(dead_code)]
    log: Vec<Callout>,
    h: Harness,
}

impl Run {
    fn v0(&self) -> u32 {
        match self.outcome {
            Outcome::Returned(v) => v as u32,
            ref o => panic!("didn't return: {o:?}"),
        }
    }
    fn v1(&self) -> u32 {
        match self.outcome {
            Outcome::Returned(v) => (v >> 32) as u32,
            ref o => panic!("didn't return: {o:?}"),
        }
    }
}

/// Translate, compile, link and run `words` as a function at `BASE` with
/// `data` at `DATA`. `None` when the toolchains aren't available.
fn run(words: &[u32], args: [u32; 4], data: &[u8]) -> Option<Run> {
    let ws = Workspace::discover().ok()?;
    if ws.clang().is_err() || ws.tool("binaryen", "bin/wasm-opt").is_err() {
        eprintln!("skipping: run `psxw toolchain fetch`");
        return None;
    }
    let mut code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    code.resize(0x200, 0);
    let text = BASE..BASE + code.len() as u32;
    let mut starts = BTreeMap::new();
    starts.insert(
        BASE,
        Start { name: Some("f".into()), size: Some(4 * words.len() as u32), sources: [Source::Symbol].into(), boundary: true },
    );
    let program = Program {
        main: "test".into(),
        containers: [("test".to_string(), Code { id: "test".into(), text: vec![text.clone()], data: vec![], starts })].into(),
    };
    let originals = Originals::from_images(vec![("test".into(), BASE, code.clone())]);
    let func = program.functions("test").remove(0);
    let t = psxw_lift::translate(&program, &originals, &func, &BTreeMap::new(), &Default::default()).expect("translates");
    let key: u64 = words.iter().fold(0xcbf2_9ce4_8422_2325, |h, w| (h ^ *w as u64).wrapping_mul(0x100_0000_01b3));
    let dir = std::env::temp_dir().join(format!("psxw-semantics-{key:016x}"));
    let failures = psxw_lift::run::write_and_compile(&ws, &dir, &[&t]).expect("compiles");
    assert!(failures.is_empty(), "{failures:?}");
    let obj = WasmObject::read(&dir.join("test").join(format!("{BASE:08X}.o"))).expect("object");
    let container = Container {
        id: "test".into(),
        is_main: true,
        vram: text.clone(),
        tus: vec![],
        sdk: vec![],
        target_path: None,
        sha1: None,
        segments: vec![(0..code.len() as u32, BASE)],
        text: vec![text],
        text_starts: vec![BASE],
        rodata: vec![],
    };
    let map = AddressMap::synthetic("test", vec![container]);
    let input = Input { label: "f".into(), container: "test".into(), obj, protos: BTreeMap::new(), generated: true, gcc: None };
    let export = ExportSpec { name: "t".into(), input: 0, function: t.cname.clone(), o32: false };
    let linked = psxw_link::link(&map, None, Mode::Harness { sp: STACK_TOP, gp: 0, dirty: None }, &[input], &[export])
        .expect("links");
    wasmparser::Validator::new().validate_all(&linked.wasm).expect("valid module");
    let mut h = Harness::new(&linked.wasm, &[(BASE, &code), (DATA, data)], BTreeMap::new(), 1_000_000).expect("instantiates");
    let f = h.func("t").expect("export");
    let r = h.run(&f, 1, args, &[], &[0; 8], 1);
    Some(Run { outcome: r.outcome, log: r.log, h })
}

/// v0 = lo, v1 = hi; return.
fn ret() -> Vec<u32> {
    vec![mflo(V0), mfhi(V1), jr(RA), NOP]
}

#[test]
fn division_edge_cases() {
    let cases: &[(bool, u32, u32, u32, u32)] = &[
        // (signed, n, d, lo, hi)
        (true, 5, 0, 0xFFFF_FFFF, 5),
        (true, (-5i32) as u32, 0, 1, (-5i32) as u32),
        (true, 0, 0, 0xFFFF_FFFF, 0),
        (true, 0x8000_0000, 0xFFFF_FFFF, 0x8000_0000, 0),
        (true, (-7i32) as u32, 2, (-3i32) as u32, (-1i32) as u32),
        (false, 5, 0, 0xFFFF_FFFF, 5),
        (false, 0xFFFF_FFFF, 2, 0x7FFF_FFFF, 1),
    ];
    for &(signed, n, d, lo, hi) in cases {
        let op = if signed { div(A0, A1) } else { divu(A0, A1) };
        let mut w = vec![op];
        w.extend(ret());
        let Some(r) = run(&w, [n, d, 0, 0], &[]) else { return };
        assert_eq!((r.v0(), r.v1()), (lo, hi), "{} {n:#x} / {d:#x}", if signed { "div" } else { "divu" });
    }
}

#[test]
fn unaligned_loads_at_every_alignment() {
    let data: Vec<u8> = (0..16).map(|b| 0x10 + b as u8).collect();
    for k in 0..4u32 {
        // lwl/lwr pair: the unaligned word at DATA + k.
        let w = vec![lui(A0, (DATA >> 16) as i32), ori(A0, A0, k as i32), lwl(V0, 3, A0), lwr(V0, 0, A0), jr(RA), NOP];
        let Some(r) = run(&w, [0; 4], &data) else { return };
        let want = u32::from_le_bytes([data[k as usize], data[k as usize + 1], data[k as usize + 2], data[k as usize + 3]]);
        assert_eq!(r.v0(), want, "lwl/lwr at +{k}");
        // Single lwl / lwr merge into 0xAABBCCDD.
        for (op, name) in [(0x22u32, "lwl"), (0x26, "lwr")] {
            let w = vec![
                lui(A0, (DATA >> 16) as i32),
                ori(A0, A0, k as i32),
                lui(V0, 0xAABB),
                ori(V0, V0, 0xCCDD),
                i(op, A0, V0, 0),
                jr(RA),
                NOP,
            ];
            let Some(r) = run(&w, [0; 4], &data) else { return };
            let mem = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            let sh = 8 * k;
            let want = if op == 0x22 {
                (0xAABB_CCDDu32 & (0x00FF_FFFFu32 >> sh)) | (mem << (24 - sh))
            } else {
                (0xAABB_CCDDu32 & (0xFFFF_FF00u64 << (24 - sh)) as u32) | (mem >> sh)
            };
            assert_eq!(r.v0(), want, "{name} at +{k}");
        }
    }
}

#[test]
fn unaligned_stores_at_every_alignment() {
    for k in 0..4u32 {
        let w = vec![
            lui(A0, (DATA >> 16) as i32),
            ori(A0, A0, k as i32),
            lui(A1, 0x1122),
            ori(A1, A1, 0x3344),
            swl(A1, 3, A0),
            swr(A1, 0, A0),
            jr(RA),
            NOP,
        ];
        let Some(r) = run(&w, [0; 4], &[0xEE; 12]) else { return };
        let got = r.h.read(DATA, 12);
        let mut want = vec![0xEE; 12];
        want[k as usize..k as usize + 4].copy_from_slice(&0x1122_3344u32.to_le_bytes());
        assert_eq!(got, want, "swl/swr at +{k}");
    }
}

#[test]
fn load_delay_slot_sees_the_old_value() {
    let data = 0xDEAD_BEEFu32.to_le_bytes();
    // lw v0; addu v1, v0, zero -> v1 is v0 before the load.
    let w = vec![
        lui(A0, (DATA >> 16) as i32),
        ori(V0, 0, 0x1234),
        lw(V0, 0, A0),
        addu(V1, V0, 0),
        jr(RA),
        NOP,
    ];
    let Some(r) = run(&w, [0; 4], &data) else { return };
    assert_eq!(r.v1(), 0x1234);
    assert_eq!(r.v0(), 0xDEAD_BEEF);
    // A write in the load delay slot wins over the load.
    let w = vec![lui(A0, (DATA >> 16) as i32), lw(V0, 0, A0), ori(V0, 0, 7), jr(RA), NOP];
    let Some(r) = run(&w, [0; 4], &data) else { return };
    assert_eq!(r.v0(), 7);
}

#[test]
fn delay_slots() {
    // The slot executes before a taken branch lands.
    let w = vec![beq(0, 0, 2), ori(V0, 0, 1), ori(V0, 0, 2), jr(RA), addiu(V0, V0, 10)];
    let Some(r) = run(&w, [0; 4], &[]) else { return };
    assert_eq!(r.v0(), 11);
    // The condition is read before the slot changes its register.
    let w = vec![bne(A0, 0, 3), addu(A0, 0, 0), ori(V0, 0, 1), jr(RA), NOP, ori(V0, 0, 2), jr(RA), NOP];
    let Some(r) = run(&w, [5, 0, 0, 0], &[]) else { return };
    assert_eq!(r.v0(), 2);
    // jal writes ra before its delay slot runs.
    let w = vec![jal(BASE + 0x100), addu(S0, RA, 0), addu(V0, S0, 0), jr(RA), NOP];
    let Some(r) = run(&w, [0; 4], &[]) else { return };
    assert_eq!(r.v0(), BASE + 8);
}

#[test]
fn add_overflow_traps_and_addu_wraps() {
    let w = vec![add(V0, A0, A1), jr(RA), NOP];
    let Some(r) = run(&w, [0x7FFF_FFFF, 1, 0, 0], &[]) else { return };
    assert!(matches!(&r.outcome, Outcome::Trap(m) if m.contains("overflow")), "{:?}", r.outcome);
    let w = vec![addu(V0, A0, A1), jr(RA), NOP];
    let Some(r) = run(&w, [0x7FFF_FFFF, 1, 0, 0], &[]) else { return };
    assert_eq!(r.v0(), 0x8000_0000);
}
