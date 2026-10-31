//! The linker's guest passes and o32 adapters on a handcrafted C object:
//! window masking, the guest stack, structs by value, 64-bit argument
//! pairs, variadic calls, `call_indirect` through the dispatch ABI, and
//! string literals placed in AUX. Skips without the pinned toolchains.

use psxw_difftest::runner::{Harness, Outcome, STACK_TOP};
use psxw_inputs::map::Container;
use psxw_inputs::{AddressMap, Workspace};
use psxw_link::object::WasmObject;
use psxw_link::{ExportSpec, Input, Mode, callout};
use std::collections::BTreeMap;

const SRC: &str = r#"
struct S { int a, b, c; };
extern int D_80030000;
extern int func_80012000(int x, struct S s, long long y);
extern int func_80012100(const char *fmt, ...);
typedef int (*fp)(int, int);

int func_80011000(int a, int b) {
    struct S s;
    fp f;
    int r, q;
    D_80030000 = a + b;
    s.a = a; s.b = b; s.c = 7;
    r = func_80012000(a, s, 0x1122334455667788LL);
    q = func_80012100("%d %d", a, b);
    f = (fp)b;
    return r + q + f(a, 3);
}
"#;

/// Compile `src` as container `t` (text at 8001_0000..8002_0000), in its
/// own temp directory `name`. None without the pinned toolchain.
fn compile(name: &str, src: &str) -> Option<(AddressMap, Input)> {
    compile_with(name, src, false)
}

/// [`compile`], through the C tier's normalization when `normalize`.
fn compile_with(name: &str, src: &str, normalize: bool) -> Option<(AddressMap, Input)> {
    let Ok(ws) = Workspace::discover() else { return None };
    let Ok(clang_path) = ws.clang() else {
        eprintln!("skipping: run `psxw toolchain fetch`");
        return None;
    };
    let dir = std::env::temp_dir().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let src_path = dir.join("t.c");
    std::fs::write(&src_path, src).unwrap();
    let clang = psxw_cc::Clang::new(clang_path, dir.clone(), vec![], vec![], &dir).unwrap();
    let obj_path = dir.join("t.o");
    if normalize {
        assert!(psxw_cc::compile(&clang, &src_path, &dir, "t", &[], None).unwrap().ok);
    } else {
        clang.compile_variant(&src_path, &obj_path, &[]).unwrap();
    }
    let protos: BTreeMap<String, psxw_cc::protos::Proto> =
        psxw_cc::protos::for_tu(&clang, &src_path).unwrap().into_iter().map(|p| (p.name.clone(), p)).collect();
    let text = 0x8001_0000..0x8002_0000;
    let container = Container {
        id: "t".into(),
        is_main: true,
        vram: text.clone(),
        tus: vec![],
        sdk: vec![],
        target_path: None,
        sha1: None,
        segments: vec![],
        text: vec![text],
        text_starts: vec![],
        rodata: vec![],
    };
    let map = AddressMap::synthetic("t", vec![container]);
    let input = Input {
        label: "t".into(),
        container: "t".into(),
        obj: WasmObject::read(&obj_path).unwrap(),
        protos,
        generated: false,
        gcc: None,
    };
    Some((map, input))
}

#[test]
fn guest_passes_and_adapters() {
    let Some((map, input)) = compile("psxw-link-passes", SRC) else { return };
    assert!(input.protos["func_80012100"].variadic);
    let export = ExportSpec { name: "c".into(), input: 0, function: "func_80011000".into(), o32: true };
    let linked =
        psxw_link::link(&map, None, Mode::Harness { sp: STACK_TOP, gp: 0, dirty: None }, &[input], &[export]).unwrap();
    wasmparser::Validator::new().validate_all(&linked.wasm).unwrap();
    // The format string has no original address: AUX.
    assert_eq!(linked.aux.len(), 1);
    let fmt = linked.aux[0].2;
    assert_eq!(fmt & 0xFFF0_0000, 0x8020_0000);

    let mut h = Harness::new(&linked.wasm, &[], BTreeMap::new(), 1_000_000).unwrap();
    let f = h.func("c").unwrap();
    let b = 0x8001_5000u32;
    let r = h.run(&f, 1, [5, b, 0, 0], &[], &[0; 8], 1);
    assert!(matches!(r.outcome, Outcome::Returned(_)), "{:?}", r.outcome);

    // Masked store at the original address.
    assert_eq!(u32::from_le_bytes(h.read(0x8003_0000, 4).try_into().unwrap()), 5u32.wrapping_add(b));
    // The guest string is at its AUX address, in the window.
    assert_eq!(&h.read(fmt, 6), b"%d %d\0");

    assert_eq!(r.log.len(), 3, "{:#?}", r.log);
    // int, struct {a, b, c} by value in a1..a3, then the i64 pair on the
    // stack at an even word.
    let c0 = &r.log[0];
    assert_eq!((c0.kind, c0.target, c0.nwords), (callout::CALL, 0x8001_2000, 6));
    assert_eq!(c0.args, [5, 5, b, 7]);
    assert_eq!(&c0.stack[..2], &[0x5566_7788, 0x1122_3344]);
    // Variadic: the fixed format pointer, then words from the va-buffer.
    let c1 = &r.log[1];
    assert_eq!((c1.kind, c1.target, c1.nwords), (callout::CALL, 0x8001_2100, 1));
    assert_eq!(c1.args[..3], [fmt, 5, b]);
    // A C function pointer is a MIPS address, called through the o32 ABI.
    let c2 = &r.log[2];
    assert_eq!((c2.kind, c2.target), (callout::INDIRECT, b));
    assert_eq!(c2.args[..2], [5, 3]);
}

/// A variadic call passes every word, a 64-bit argument after an odd
/// number of fixed words at an even word, as o32 lays them out.
#[test]
fn variadic_words() {
    let src = r#"
extern int func_80012100(const char *fmt, ...);
int func_80011000(int a) { return func_80012100("x", a, 0x1122334455667788LL, a, a, a, a, a, a, a, a); }
"#;
    let Some((map, input)) = compile("psxw-link-variadic", src) else { return };
    let export = ExportSpec { name: "c".into(), input: 0, function: "func_80011000".into(), o32: true };
    let linked =
        psxw_link::link(&map, None, Mode::Harness { sp: STACK_TOP, gp: 0, dirty: None }, &[input], &[export]).unwrap();
    let fmt = linked.aux[0].2;
    let mut h = Harness::new(&linked.wasm, &[], BTreeMap::new(), 1_000_000).unwrap();
    let f = h.func("c").unwrap();
    let r = h.run(&f, 1, [5, 0, 0, 0], &[], &[0; 8], 1);
    let c = &r.log[0];
    assert_eq!(c.args, [fmt, 5, 0x5566_7788, 0x1122_3344], "{c:?}");
    assert_eq!(c.stack, [5; 8]);
}

/// FastISel may call a prototype-less function through the table when one
/// call passes an integer and another a pointer. Both still name one callee.
#[test]
fn prototype_less_calls_are_direct() {
    let src = r#"
extern void *func_80012200();
void func_80011000(int a, char *p) { func_80012200(a, 0, 8); func_80012200(p, 0, 8); }
"#;
    let Some((map, input)) = compile("psxw-link-noproto", src) else { return };
    let export = ExportSpec { name: "c".into(), input: 0, function: "func_80011000".into(), o32: true };
    let linked =
        psxw_link::link(&map, None, Mode::Harness { sp: STACK_TOP, gp: 0, dirty: None }, &[input], &[export]).unwrap();
    let mut h = Harness::new(&linked.wasm, &[], BTreeMap::new(), 1_000_000).unwrap();
    let f = h.func("c").unwrap();
    let r = h.run(&f, 1, [5, 0x8001_5000, 0, 0], &[], &[0; 8], 1);
    assert!(matches!(r.outcome, Outcome::Returned(_)), "{:?}", r.outcome);
    let calls: Vec<_> = r.log.iter().map(|c| (c.kind, c.target, c.args[0])).collect();
    assert_eq!(calls, [(callout::CALL, 0x8001_2200, 5), (callout::CALL, 0x8001_2200, 0x8001_5000)]);
}

/// A writable static the address map doesn't name has an original address
/// the link can't know: it fails naming it, rather than placing a second
/// copy in AUX.
#[test]
fn unnamed_writable_data_fails_the_link() {
    let src = "static int counter;\nint func_80011000(void) { return ++counter; }\n";
    let Some((map, input)) = compile("psxw-link-unnamed", src) else { return };
    let export = ExportSpec { name: "c".into(), input: 0, function: "func_80011000".into(), o32: true };
    let err = psxw_link::link(&map, None, Mode::Harness { sp: STACK_TOP, gp: 0, dirty: None }, &[input], &[export]).unwrap_err();
    assert!(err.to_string().contains("t counter"), "{err}");
}

/// A division by a variable stays where the C puts it (D36): a zero
/// divisor breaks as the original's assembler checks, and gives the
/// R3000's quotient, though only one path uses the result (clang would
/// sink a plain division into that path).
#[test]
fn divisions_break_where_the_source_divides() {
    let src = "int func_80011000(int a, int b, int c) { int q = a / b; if (c) return q; return 0; }\n";
    let Some((map, mut input)) = compile_with("psxw-link-division", src, true) else { return };
    let division = psxw_link::Division { zero: 7, overflow: None };
    input.gcc = Some(psxw_link::Gcc { division, builtin_memcpy: false, main_calls_main: false });
    let export = ExportSpec { name: "c".into(), input: 0, function: "func_80011000".into(), o32: true };
    let linked =
        psxw_link::link(&map, None, Mode::Harness { sp: STACK_TOP, gp: 0, dirty: None }, &[input], &[export]).unwrap();
    let mut h = Harness::new(&linked.wasm, &[], BTreeMap::new(), 1_000_000).unwrap();
    let f = h.func("c").unwrap();
    for (c, ret) in [(0, 0), (1, 0xFFFF_FFFF)] {
        let r = h.run(&f, 1, [5, 0, c, 0], &[], &[0; 8], 1);
        let breaks: Vec<_> = r.log.iter().map(|e| (e.kind, e.target)).collect();
        assert_eq!(breaks, [(callout::BREAK, 7)], "c = {c}");
        assert!(matches!(r.outcome, Outcome::Returned(v) if v as u32 == ret), "{:?}", r.outcome);
    }
}
