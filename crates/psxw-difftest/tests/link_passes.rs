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

#[test]
fn guest_passes_and_adapters() {
    let Ok(ws) = Workspace::discover() else { return };
    let Ok(clang_path) = ws.clang() else {
        eprintln!("skipping: run `psxw toolchain fetch`");
        return;
    };
    let dir = std::env::temp_dir().join("psxw-link-passes");
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("t.c");
    std::fs::write(&src, SRC).unwrap();
    let clang = psxw_cc::Clang::new(clang_path, dir.clone(), vec![], vec![], &dir).unwrap();
    let obj_path = dir.join("t.o");
    clang.compile_variant(&src, &obj_path, &[]).unwrap();
    let protos: BTreeMap<String, psxw_cc::protos::Proto> =
        psxw_cc::protos::for_tu(&clang, &src).unwrap().into_iter().map(|p| (p.name.clone(), p)).collect();
    assert!(protos["func_80012100"].variadic);

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
    let r = h.run(&f, 1, [5, b, 0, 0], &[], &[0; 8]);
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
