//! The original decides a function's domain: hand-assembled MIPS is
//! translated, compiled by the pinned clang and linked as both sides of a
//! pair, then run through `run_pair`. Seeds on which it faults, or whose
//! result moves with its stack, are replaced and counted, not compared.
//!
//! Skips when the pinned toolchains haven't been fetched.

use super::*;
use psxw_inputs::map::Container;
use psxw_inputs::originals::Originals;
use psxw_lift::program::{Code, Program, Start};

const BASE: u32 = 0x8001_0000;
const V0: u32 = 2;
const A0: u32 = 4;
const T0: u32 = 8;
const SP: u32 = 29;
const RA: u32 = 31;
const SEEDS: u32 = 16;

fn r(rs: u32, rt: u32, rd: u32, f: u32) -> u32 {
    (rs << 21) | (rt << 16) | (rd << 11) | f
}
fn i(op: u32, rs: u32, rt: u32, imm: i32) -> u32 {
    (op << 26) | (rs << 21) | (rt << 16) | (imm as u32 & 0xFFFF)
}
fn addu(rd: u32, rs: u32, rt: u32) -> u32 {
    r(rs, rt, rd, 0x21)
}
const JR_RA: u32 = (RA << 21) | 0x08;
const NOP: u32 = 0;

/// `words` at `BASE`, translated and linked as both the C and the original
/// of one pair taking an `int`. `None` when the toolchains aren't available.
fn pair(words: &[u32]) -> Option<(Harness, Pair)> {
    let ws = Workspace::discover().ok()?;
    if ws.clang().is_err() || ws.tool("binaryen", "bin/wasm-opt").is_err() {
        eprintln!("skipping: run `psxw toolchain fetch`");
        return None;
    }
    let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let text = BASE..BASE + code.len() as u32;
    let mut starts = BTreeMap::new();
    starts.insert(
        BASE,
        Start { name: Some("f".into()), size: Some(code.len() as u32), sources: [Source::Symbol].into(), boundary: true },
    );
    let program = Program {
        main: "test".into(),
        through_pointers: Default::default(),
        containers: [("test".to_string(), Code { id: "test".into(), text: vec![text.clone()], data: vec![], starts })].into(),
    };
    let originals = Originals::from_images(vec![("test".into(), BASE, code.clone())]);
    let func = program.functions("test").remove(0);
    let t = psxw_lift::translate(&program, &originals, &func, &BTreeMap::new(), &Default::default()).expect("translates");
    let key: u64 = words.iter().fold(0xcbf2_9ce4_8422_2325, |h, w| (h ^ *w as u64).wrapping_mul(0x100_0000_01b3));
    let dir = std::env::temp_dir().join(format!("psxw-domain-{key:016x}"));
    let failures = psxw_lift::run::write_and_compile(&ws, &dir, &[&t]).expect("compiles");
    assert!(failures.is_empty(), "{failures:?}");
    let lifted = dir.join("test").join(format!("{BASE:08X}.o"));
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
    let obj = WasmObject::read(&lifted).expect("object");
    let input = Input { label: "f".into(), container: "test".into(), obj, protos: BTreeMap::new(), generated: true, gcc: None };
    let exports: Vec<ExportSpec> = ["c", "t"]
        .iter()
        .map(|side| ExportSpec { name: format!("{side}.{BASE:08X}"), input: 0, function: t.cname.clone(), o32: false })
        .collect();
    let mode = Mode::Harness { sp: runner::STACK_TOP, gp: 0, dirty: Some(runner::DIRTY_MAP), costs: None };
    let linked = psxw_link::link(&map, None, mode, &[input], &exports).expect("links");
    let h = Harness::new(&linked.wasm, &[(BASE, &code)], BTreeMap::new(), 1_000_000).expect("instantiates");
    let p = Pair {
        container: "test".into(),
        addr: BASE,
        name: "f".into(),
        object: PathBuf::new(),
        lifted,
        ret: Kind::I32,
        params: vec![Kind::I32],
        refs: vec![],
    };
    Some((h, p))
}

fn run(words: &[u32]) -> Option<PairReport> {
    let (mut h, p) = pair(words)?;
    Some(run_pair(&mut h, &p, SEEDS, 0x5053_5857, &BTreeMap::new(), &|_, _| false, false).expect("runs"))
}

#[test]
fn a_result_that_moves_with_the_stack_is_outside() {
    // Return the argument when it's even, sp when it's odd.
    let w = [i(0x0C, A0, T0, 1), i(0x04, T0, 0, 2), addu(V0, A0, 0), addu(V0, SP, 0), JR_RA, NOP];
    let Some(r) = run(&w) else { return };
    assert_eq!(r.verdict, "pass", "{r:?}");
    assert_eq!(r.accepted, SEEDS);
    assert!(r.outside.get(STACK_DEPENDENT).is_some_and(|&n| n > 0), "{r:?}");
    assert!(!r.outside.contains_key(FAULT), "{r:?}");
}

#[test]
fn a_bad_pointer_the_original_faults_on_is_outside() {
    // Load a word through the argument.
    let w = [i(0x23, A0, V0, 0), JR_RA, NOP];
    let Some(r) = run(&w) else { return };
    assert_eq!(r.verdict, "pass", "{r:?}");
    assert_eq!(r.accepted, SEEDS);
    assert!(r.outside.get(FAULT).is_some_and(|&n| n > 0), "{r:?}");
    assert!(!r.outside.contains_key(STACK_DEPENDENT), "{r:?}");
}
