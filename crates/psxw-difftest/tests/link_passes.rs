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

/// Shared prologues must not route an overlay call to another scene's code.
#[test]
fn game_overlay_dispatch_checks_beyond_the_prologue() {
    use psxw_abi::{WINDOW_BASE, guest};
    use psxw_link::game::{GameSpec, Tier};
    let Some((mut map, mut a)) = compile("psxw-overlay-a", "int func_80011000(void) { return 11; }") else { return };
    let Some((_, mut b)) = compile("psxw-overlay-b", "int func_80011000(void) { return 22; }") else { return };
    a.container = "a".into();
    b.container = "b".into();
    for id in ["a", "b"] {
        let mut c = map.containers[0].clone();
        c.id = id.into();
        c.is_main = false;
        map.containers.push(c);
    }
    let inputs = [a, b];
    let addr = 0x8001_1000;
    let tiers = inputs.iter().enumerate().map(|(i, input)| {
        let func = input.obj.symbols.iter().find(|s| s.name == "func_80011000").unwrap().index.unwrap()
            - input.obj.num_imported_funcs();
        ((input.container.clone(), addr), Tier::C { input: i, func })
    }).collect();
    let prologue = [(addr, 0x27BD_FFE8), (addr + 4, 0x3C02_8011)];
    let cand = |c: &str, word: u32| {
        (c.into(), prologue.into_iter().chain([(addr + 0x80, word)]).collect())
    };
    let spec = GameSpec {
        tiers, overlays: BTreeMap::from([(addr, vec![cand("a", 1), cand("b", 2)])]),
        budget: 1000, debug_io: false, device_funcs: Default::default(), main: "t".into(),
        min_pages: psxw_abi::window_pages(WINDOW_BASE), window_base: WINDOW_BASE,
        debug_info: false, probes: vec![], mods: None, slots: None, threads: false, binding: None, lifted_params: BTreeMap::new(),
    };
    let linked = psxw_link::game::link(&map, None, &spec, &inputs).unwrap();
    let mut config = wasmtime::Config::new();
    config.wasm_exceptions(true);
    let engine = wasmtime::Engine::new(&config).unwrap();
    let module = wasmtime::Module::new(&engine, &linked.wasm).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let mut linker = wasmtime::Linker::new(&engine);
    linker.define_unknown_imports_as_traps(&module).unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let memory = instance.get_memory(&mut store, guest::MEMORY).unwrap();
    let invoke = instance.get_typed_func::<(u32, u32, u32, u32, u32), u64>(&mut store, guest::INVOKE).unwrap();
    for (at, word) in prologue {
        memory.write(&mut store, (WINDOW_BASE + (at & 0x1F_FFFF)) as usize, &word.to_le_bytes()).unwrap();
    }
    for (word, result) in [(2u32, 22), (1, 11), (2, 22)] {
        memory.write(&mut store, (WINDOW_BASE + ((addr + 0x80) & 0x1F_FFFF)) as usize, &word.to_le_bytes()).unwrap();
        assert_eq!(invoke.call(&mut store, (addr, 0, 0, 0, 0)).unwrap() as u32, result);
    }
    memory.write(&mut store, (WINDOW_BASE + ((addr + 0x80) & 0x1F_FFFF)) as usize, &3u32.to_le_bytes()).unwrap();
    assert!(invoke.call(&mut store, (addr, 0, 0, 0, 0)).is_err(), "unknown code must not choose the first overlay");
}

/// Saved native BIOS entries remain callable after a table patch. Exercise
/// C's dynamic/constant pointers, a lifted direct call, and the o32 return.
#[test]
fn game_native_saved_entry_thunks() {
    use psxw_abi::{WINDOW_BASE, guest, kernel};
    use psxw_link::game::{GameSpec, Tier};
    let src = r#"
typedef unsigned long long (*fp)(unsigned, unsigned, unsigned, unsigned);
unsigned long long func_80011100(unsigned a, unsigned b, unsigned c, unsigned d) { return 42; }
unsigned long long func_80011000(unsigned a, unsigned b, unsigned c, unsigned d) {
    fp saved = *(fp volatile *)0x200;
    *(fp volatile *)0x200 = func_80011100;
    return saved(a, b, c, d);
}
unsigned long long func_80011200(unsigned a, unsigned b, unsigned c, unsigned d) {
    return ((fp)0xBFC0A000)(a, b, c, d);
}
extern unsigned long long psxw_f_BFC0A000(unsigned, unsigned, unsigned, unsigned);
unsigned long long func_80011300(unsigned a, unsigned b, unsigned c, unsigned d) {
    return psxw_f_BFC0A000(a, b, c, d);
}
unsigned long long func_80011400(unsigned a, unsigned b, unsigned c, unsigned d) {
    ((fp)0xBFC0A35C)(a, b, c, d);
    return 77; /* ReturnFromException must never return here. */
}
unsigned long long func_80011500(unsigned a, unsigned b, unsigned c, unsigned d) {
    fp saved = *(fp volatile *)0x7138;
    *(fp volatile *)0x7138 = func_80011100;
    return saved(a, b, c, d);
}
unsigned long long func_80011600(unsigned a, unsigned b, unsigned c, unsigned d) {
    return ((fp)0xBFC08018)(a, b, c, d);
}
extern unsigned long long psxw_f_BFC08018(unsigned, unsigned, unsigned, unsigned);
unsigned long long func_80011700(unsigned a, unsigned b, unsigned c, unsigned d) {
    return psxw_f_BFC08018(a, b, c, d);
}
unsigned long long func_80011800(unsigned a, unsigned b, unsigned c, unsigned d) {
    ((fp)0xBFC0801C)(a, b, c, d);
    return 77;
}
extern unsigned long long psxw_kernel_call(unsigned, unsigned, unsigned, unsigned, unsigned);
unsigned long long func_80011900(unsigned a, unsigned b, unsigned c, unsigned d) {
    psxw_kernel_call(0xBFC0801C, a, b, c, d);
    return 77;
}
"#;
    let Some((map, input)) = compile("psxw-link-bios-thunks", src) else {
        return;
    };
    let tiers = (0..input.obj.funcs.len() as u32)
        .filter_map(|f| {
            let name = &input.obj.func_symbol(f + input.obj.num_imported_funcs())?.name;
            let addr = u32::from_str_radix(name.strip_prefix("func_")?, 16).ok()?;
            Some((("t".into(), addr), Tier::C { input: 0, func: f }))
        })
        .collect();
    let mut spec = GameSpec {
        tiers,
        overlays: BTreeMap::new(),
        budget: 1000,
        debug_io: false,
        device_funcs: Default::default(),
        main: "t".into(),
        min_pages: psxw_abi::window_pages(WINDOW_BASE),
        window_base: WINDOW_BASE,
        debug_info: false,
        probes: vec![],
        mods: None,
        slots: None,
        threads: false,
        binding: None, lifted_params: BTreeMap::new(),
    };
    let inputs = [input];
    let linked = psxw_link::game::link(&map, None, &spec, &inputs).unwrap();
    let mut config = wasmtime::Config::new();
    config.wasm_exceptions(true);
    let engine = wasmtime::Engine::new(&config).unwrap();
    let module = wasmtime::Module::new(&engine, &linked.wasm).unwrap();
    let mut store = wasmtime::Store::new(&engine, 0u32);
    let mut linker = wasmtime::Linker::new(&engine);
    const RESULT: u64 = 0xFEDC_BA98_7654_3210;
    linker
        .func_wrap("psx", "bios", |mut caller: wasmtime::Caller<'_, u32>, t: u32, f: u32, a: u32, b: u32, c: u32, d: u32| {
            assert_eq!((t, f), (psxw_abi::BIOS_STAND_INS.start, 0));
            assert_eq!([a, b, c, d], [11, 22, 33, 44]);
            *caller.data_mut() += 1;
            RESULT
        })
        .unwrap();
    linker.func_wrap("psx", "return_from_exception", |mut c: wasmtime::Caller<'_, u32>| *c.data_mut() += 100).unwrap();
    linker.func_wrap("psx", "kernel_call", |mut c: wasmtime::Caller<'_, u32>, addr: u32, a: u32, b: u32, x: u32, d: u32| -> wasmtime::Result<u64> {
        if addr == 0xBFC0_801C {
            // The native flow calls a guest which itself invokes RFE. The
            // tag must cross both native and guest frames to invoke_irq,
            // not be caught by the inner guest_invoke and resume here.
            let invoke = c.get_export(guest::INVOKE).unwrap().into_func().unwrap().typed::<(u32, u32, u32, u32, u32), u64>(&c)?;
            invoke.call(&mut c, (0x8001_1400, 0, 0, 0, 0))?;
            panic!("kernel flow continued after RFE");
        }
        assert_eq!(addr, 0xBFC0_8018);
        assert_eq!([a, b, x, d], [11, 22, 33, 44]);
        *c.data_mut() += 1;
        Ok(RESULT)
    }).unwrap();
    linker.define_unknown_imports_as_traps(&module).unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    instance.get_typed_func::<u32, ()>(&mut store, "guest_sp_set").unwrap().call(&mut store, STACK_TOP).unwrap();
    let memory = instance.get_memory(&mut store, guest::MEMORY).unwrap();
    memory.write(&mut store, (WINDOW_BASE + kernel::A0_TABLE) as usize, &psxw_abi::BIOS_STAND_INS.start.to_le_bytes()).unwrap();
    let invoke = instance.get_typed_func::<(u32, u32, u32, u32, u32), u64>(&mut store, guest::INVOKE).unwrap();
    for addr in [0x8001_1000, 0x8001_1200, 0x8001_1300, psxw_abi::BIOS_STAND_INS.start] {
        assert_eq!(invoke.call(&mut store, (addr, 11, 22, 33, 44)).unwrap(), RESULT);
    }
    assert_eq!(*store.data(), 4);
    // The second call through the table reaches the patch, not the saved native entry.
    assert_eq!(invoke.call(&mut store, (0x8001_1000, 11, 22, 33, 44)).unwrap(), 42);
    assert_eq!(*store.data(), 4);
    let irq = instance.get_typed_func::<(u32, u32, u32, u32, u32), u64>(&mut store, guest::INVOKE_IRQ).unwrap();
    assert_eq!(irq.call(&mut store, (0x8001_1400, 0, 0, 0, 0)).unwrap(), 0);
    assert_eq!(*store.data(), 104);
    memory.write(&mut store, (WINDOW_BASE + 0x7138) as usize, &0xBFC0_8018u32.to_le_bytes()).unwrap();
    for addr in [0x8001_1500, 0x8001_1600, 0x8001_1700, 0xBFC0_8018] {
        assert_eq!(invoke.call(&mut store, (addr, 11, 22, 33, 44)).unwrap(), RESULT);
    }
    assert_eq!(*store.data(), 108);
    assert_eq!(invoke.call(&mut store, (0x8001_1500, 11, 22, 33, 44)).unwrap(), 42);
    for addr in [0x8001_1800, 0x8001_1900] {
        assert_eq!(irq.call(&mut store, (addr, 0, 0, 0, 0)).unwrap(), 0);
    }
    assert_eq!(*store.data(), 308);
    spec.tiers.insert(("t".into(), 0x8000_8000), spec.tiers[&("t".into(), 0x8001_1000)]);
    let err = psxw_link::game::link(&map, None, &spec, &inputs).unwrap_err();
    assert!(err.to_string().contains("collides with kernel stand-in"), "{err:#}");
    spec.tiers.remove(&("t".into(), 0x8000_8000));
    spec.tiers.insert(("t".into(), 0x8000_A000), spec.tiers[&("t".into(), 0x8001_1000)]);
    let err = psxw_link::game::link(&map, None, &spec, &inputs).unwrap_err();
    assert!(err.to_string().contains("collides with BIOS stand-in"), "{err:#}");
}

/// A direct syscall has the same unwind/rewind contract as ChangeThread.
/// Run real clang/link/Asyncify output, retaining locals across the import.
#[test]
fn game_syscall_asyncify_resume() {
    use psxw_abi::{WINDOW_BASE, guest};
    use psxw_link::game::{GameSpec, Tier};
    use std::process::Command;
    let src = r#"
extern unsigned psxw_syscall(unsigned, unsigned, unsigned, unsigned);
int func_80011100(int x) { return x + psxw_syscall(3, 0xA000E0D0, 0, 0); }
int func_80011000(int x) { return x + func_80011100(x * 2); }
"#;
    let Some((map, input)) = compile("psxw-link-syscall-asyncify", src) else { return };
    let ws = Workspace::discover().unwrap();
    let Ok(tool) = ws.tool("binaryen", "bin/wasm-opt") else {
        eprintln!("skipping: run `psxw toolchain fetch` for Binaryen");
        return;
    };
    let tiers = (0..input.obj.funcs.len() as u32).filter_map(|f| {
        let name = &input.obj.func_symbol(f + input.obj.num_imported_funcs())?.name;
        let addr = u32::from_str_radix(name.strip_prefix("func_")?, 16).ok()?;
        Some((("t".into(), addr), Tier::C { input: 0, func: f }))
    }).collect();
    let spec = GameSpec {
        tiers, overlays: BTreeMap::new(), budget: 1000, debug_io: false,
        device_funcs: Default::default(), main: "t".into(),
        min_pages: psxw_abi::window_pages(WINDOW_BASE), window_base: WINDOW_BASE,
        debug_info: false, probes: vec![], mods: None, slots: None, threads: true, binding: None, lifted_params: BTreeMap::new(),
    };
    let linked = psxw_link::game::link(&map, None, &spec, &[input]).unwrap();
    let scope = psxw_link::asyncify::scope(&linked.wasm, &["t:func_80011100".to_string()].into()).unwrap();
    let dir = std::env::temp_dir().join("psxw-link-syscall-asyncify");
    let raw = dir.join("guest.wasm");
    let out = dir.join("asyncify.wasm");
    std::fs::write(&raw, &linked.wasm).unwrap();
    let features = ["--enable-bulk-memory", "--enable-bulk-memory-opt", "--enable-sign-ext",
        "--enable-mutable-globals", "--enable-nontrapping-float-to-int", "--enable-multivalue",
        "--enable-reference-types", "--enable-call-indirect-overlong", "--enable-exception-handling"];
    let names = Command::new(&tool).args(features).arg("--print-function-map").arg(&raw).output().unwrap();
    assert!(names.status.success(), "{}", String::from_utf8_lossy(&names.stderr));
    let names: BTreeMap<u32, String> = String::from_utf8(names.stdout).unwrap().lines().filter_map(|s| {
        let (i, n) = s.split_once(':')?;
        Some((i.parse().ok()?, n.into()))
    }).collect();
    let list = dir.join("only.txt");
    std::fs::write(&list, scope.instrument.iter().map(|i| names[i].as_str()).collect::<Vec<_>>().join("\n")).unwrap();
    let status = Command::new(&tool).args(features).arg("--asyncify")
        .arg(format!("--pass-arg=asyncify-imports@{}", psxw_link::asyncify::IMPORTS))
        .arg(format!("--pass-arg=asyncify-onlylist@@{}", list.display()))
        .arg(&raw).arg("-o").arg(&out).status().unwrap();
    assert!(status.success());
    let mut config = wasmtime::Config::new();
    config.wasm_exceptions(true);
    let engine = wasmtime::Engine::new(&config).unwrap();
    let module = wasmtime::Module::from_file(&engine, out).unwrap();
    let mut store = wasmtime::Store::new(&engine, 0u32);
    let mut linker = wasmtime::Linker::new(&engine);
    linker.func_wrap("psx", "syscall", |mut c: wasmtime::Caller<'_, u32>, n: u32, addr: u32, _: u32, _: u32| -> wasmtime::Result<u32> {
        assert_eq!((n, addr), (3, 0xA000_E0D0));
        let state = c.get_export(guest::ASYNCIFY_GET_STATE).unwrap().into_func().unwrap().typed::<(), i32>(&c)?.call(&mut c, ())?;
        if state == 2 {
            c.get_export(guest::ASYNCIFY_STOP_REWIND).unwrap().into_func().unwrap().typed::<(), ()>(&c)?.call(&mut c, ())?;
        } else {
            *c.data_mut() += 1;
            c.get_export(guest::ASYNCIFY_START_UNWIND).unwrap().into_func().unwrap().typed::<u32, ()>(&c)?.call(&mut c, 0x1000)?;
        }
        Ok(1)
    }).unwrap();
    linker.define_unknown_imports_as_traps(&module).unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    instance.get_typed_func::<u32, ()>(&mut store, "guest_sp_set").unwrap().call(&mut store, STACK_TOP).unwrap();
    let mem = instance.get_memory(&mut store, guest::MEMORY).unwrap();
    mem.write(&mut store, 0x1000, &[0x08, 0x10, 0, 0, 0, 0, 1, 0]).unwrap();
    let invoke = instance.get_typed_func::<(u32, u32, u32, u32, u32), u64>(&mut store, guest::INVOKE).unwrap();
    let state = instance.get_typed_func::<(), i32>(&mut store, guest::ASYNCIFY_GET_STATE).unwrap();
    invoke.call(&mut store, (0x8001_1000, 7, 0, 0, 0)).unwrap();
    assert_eq!(state.call(&mut store, ()).unwrap(), 1, "the whole call stack suspended");
    instance.get_typed_func::<(), ()>(&mut store, guest::ASYNCIFY_STOP_UNWIND).unwrap().call(&mut store, ()).unwrap();
    instance.get_typed_func::<u32, ()>(&mut store, guest::ASYNCIFY_START_REWIND).unwrap().call(&mut store, 0x1000).unwrap();
    assert_eq!(invoke.call(&mut store, (0x8001_1000, 99, 0, 0, 0)).unwrap(), 22, "locals restored, not recomputed from the new argument");
    assert_eq!(state.call(&mut store, ()).unwrap(), 0);
    assert_eq!(*store.data(), 1, "rewinding must not request another switch");
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

/// A native reference (plans/dispatch-binding B) enters its target's hook
/// chain, as its guest address would: the handle names the function, and
/// its table entry is the function's generic entry, a mod's patch included.
/// A flow that stores the address keeps it, and calls it by address.
#[test]
fn native_reference_keeps_hooks() {
    use psxw_abi::{WINDOW_BASE, guest};
    use psxw_link::game::{GameSpec, Tier};
    use psxw_link::mods::{Chains, Impl, Layer, ModInputs, ModSpec, Patch};
    // Weak and noinline, as the C tier makes them: clang can't see
    // through the helper to the one function it's called with.
    let src = r#"
typedef int (*fp)(int);
__attribute__((weak, noinline)) int func_80011100(int a) { return a + 1; }
__attribute__((weak, noinline)) int apply(fp f, int a) { return f ? f(a) : -1; }
__attribute__((weak, noinline)) int func_80011000(int a) { return apply(func_80011100, a) * 10; }
__attribute__((weak, noinline)) int func_80011200(int a) {
    *(fp volatile *)0x80018000 = func_80011100;
    return (*(fp volatile *)0x80018000)(a) * 10;
}
"#;
    let Some((map, game)) = compile("psxw-link-native-hooks", src) else { return };
    let Some((_, mut patch)) = compile("psxw-link-native-hooks-mod", "int patch(int a) { return a + 100; }\n") else { return };
    patch.label = "mod m".into();
    let tiers = (0..game.obj.funcs.len() as u32)
        .filter_map(|f| {
            let name = &game.obj.func_symbol(f + game.obj.num_imported_funcs())?.name;
            let addr = u32::from_str_radix(name.strip_prefix("func_")?, 16).ok()?;
            Some((("t".into(), addr), Tier::C { input: 0, func: f }))
        })
        .collect();
    let layer = Layer { module: 0, befores: vec![], patch: Some(Patch { imp: Impl { input: 1, function: "patch".into() }, original: None }), afters: vec![] };
    let mods = ModSpec {
        mods: vec![ModInputs { id: "m".into(), inputs: vec![1] }],
        targets: BTreeMap::from([(("t".into(), 0x8001_1100), Chains { returns: true, default: vec![layer], from: BTreeMap::new() })]),
        data: vec![],
    };
    let spec = GameSpec {
        tiers,
        overlays: BTreeMap::new(),
        budget: 1000,
        debug_io: false,
        device_funcs: Default::default(),
        main: "t".into(),
        min_pages: psxw_abi::window_pages(WINDOW_BASE),
        window_base: WINDOW_BASE,
        debug_info: false,
        probes: vec![],
        mods: Some(mods),
        slots: None,
        threads: false,
        binding: None, lifted_params: BTreeMap::new(),
    };
    let linked = psxw_link::game::link(&map, None, &spec, &[game, patch]).unwrap();
    wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::all()).validate_all(&linked.wasm).unwrap();
    let n = &linked.native;
    assert_eq!(n.handles, ["1 t 0x80011100"], "{n:#?}");
    assert_eq!((n.native_sites, n.native_calls, n.retained_sites), (1, 1, 1), "{n:#?}");
    let stored = n.flows.iter().find(|f| f.representation == "guest").unwrap();
    assert_eq!(stored.reason, Some(psxw_link::native::Reason::OpaqueEscape), "{stored:#?}");
    let mut config = wasmtime::Config::new();
    config.wasm_exceptions(true);
    let engine = wasmtime::Engine::new(&config).unwrap();
    let module = wasmtime::Module::new(&engine, &linked.wasm).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let mut linker = wasmtime::Linker::new(&engine);
    linker.define_unknown_imports_as_traps(&module).unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    instance.get_typed_func::<u32, ()>(&mut store, "guest_sp_set").unwrap().call(&mut store, STACK_TOP).unwrap();
    let invoke = instance.get_typed_func::<(u32, u32, u32, u32, u32), u64>(&mut store, guest::INVOKE).unwrap();
    // Through the handle, and through the stored address: the patch both ways.
    assert_eq!(invoke.call(&mut store, (0x8001_1000, 5, 0, 0, 0)).unwrap() as u32, 1050);
    assert_eq!(invoke.call(&mut store, (0x8001_1200, 5, 0, 0, 0)).unwrap() as u32, 1050);
    let memory = instance.get_memory(&mut store, guest::MEMORY).unwrap();
    let mut word = [0u8; 4];
    memory.read(&store, (WINDOW_BASE + 0x1_8000) as usize, &mut word).unwrap();
    assert_eq!(u32::from_le_bytes(word), 0x8001_1100, "the stored reference is its guest address, not a handle");
}

/// A pointer table whose bytes are read across a cell boundary keeps its
/// guest addresses (plans/dispatch-binding B): a native handle's bytes
/// aren't the address's. Its aligned calls go through the guest table.
#[test]
fn native_reference_table_bytes_stay_guest() {
    use psxw_abi::{WINDOW_BASE, guest};
    use psxw_link::game::{GameSpec, Tier};
    let src = r#"
typedef int (*fp)(int);
__attribute__((weak, noinline)) int func_80011100(int a) { return a + 1; }
__attribute__((weak, noinline)) int func_80011200(int a) { return a + 2; }
static fp const ops[] = { func_80011100, func_80011200, 0 };
__attribute__((weak, noinline)) int func_80011000(int i) {
    unsigned x;
    __builtin_memcpy(&x, (const unsigned char *)ops + 4 * (i & 1) + 1, 4);
    return x != 0;
}
__attribute__((weak, noinline)) int func_80011300(int i) { fp f = ops[i & 1]; return f(5); }
"#;
    let Some((map, game)) = compile("psxw-link-native-table-bytes", src) else { return };
    let tiers = (0..game.obj.funcs.len() as u32)
        .filter_map(|f| {
            let name = &game.obj.func_symbol(f + game.obj.num_imported_funcs())?.name;
            let addr = u32::from_str_radix(name.strip_prefix("func_")?, 16).ok()?;
            Some((("t".into(), addr), Tier::C { input: 0, func: f }))
        })
        .collect();
    let spec = GameSpec {
        tiers,
        overlays: BTreeMap::new(),
        budget: 1000,
        debug_io: false,
        device_funcs: Default::default(),
        main: "t".into(),
        min_pages: psxw_abi::window_pages(WINDOW_BASE),
        window_base: WINDOW_BASE,
        debug_info: false,
        probes: vec![],
        mods: None,
        slots: None,
        threads: false,
        binding: None, lifted_params: BTreeMap::new(),
    };
    let linked = psxw_link::game::link(&map, None, &spec, &[game]).unwrap();
    let mut config = wasmtime::Config::new();
    config.wasm_exceptions(true);
    let engine = wasmtime::Engine::new(&config).unwrap();
    let module = wasmtime::Module::new(&engine, &linked.wasm).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let mut linker = wasmtime::Linker::new(&engine);
    linker.define_unknown_imports_as_traps(&module).unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    instance.get_typed_func::<u32, ()>(&mut store, "guest_sp_set").unwrap().call(&mut store, STACK_TOP).unwrap();
    let invoke = instance.get_typed_func::<(u32, u32, u32, u32, u32), u64>(&mut store, guest::INVOKE).unwrap();
    // The bytes straddling cells 1 and 2 hold part of 0x80011200.
    for i in [0, 1] {
        assert_eq!(invoke.call(&mut store, (0x8001_1000, i, 0, 0, 0)).unwrap() as u32, 1, "i = {i}");
    }
    assert_eq!(invoke.call(&mut store, (0x8001_1300, 1, 0, 0, 0)).unwrap() as u32, 7);
    let n = &linked.native;
    assert!(n.handles.is_empty(), "{n:#?}");
    let table = n.flows.iter().find(|f| f.sites.iter().any(|s| s.starts_with("ops data+"))).expect("the table's flow");
    assert_eq!((table.representation, table.reason), ("guest", Some(psxw_link::native::Reason::NumericObservation)), "{table:#?}");
    assert!(table.detail.as_deref().is_some_and(|d| d.contains("not proven to be one whole cell")), "{table:#?}");
}

/// An alias names its function, not a second one: `&run_local` and
/// `&func_80011400` are equal in C, and a call through either enters the
/// function's hooks. Only a function no other symbol names is a source-only
/// helper (plans/dispatch-binding B), so the alias's reference isn't
/// promoted. It has no guest address either, so the link refuses it, as
/// it refuses any reference without one.
#[test]
fn native_reference_alias_is_its_function() {
    use psxw_abi::WINDOW_BASE;
    use psxw_link::game::{GameSpec, Tier};
    let src = r#"
typedef int (*fp)(int);
__attribute__((weak, noinline)) int func_80011400(int a) { return a + 2; }
extern int run_local(int a) __attribute__((alias("func_80011400")));
__attribute__((weak, noinline)) int same(fp f, fp g) { return f == g; }
__attribute__((weak, noinline)) int func_80011000(int a) { return same(run_local, func_80011400); }
"#;
    let Some((map, game)) = compile("psxw-link-native-alias", src) else { return };
    let tiers = (0..game.obj.funcs.len() as u32)
        .filter_map(|f| {
            let name = &game.obj.func_symbol(f + game.obj.num_imported_funcs())?.name;
            let addr = u32::from_str_radix(name.strip_prefix("func_")?, 16).ok()?;
            Some((("t".into(), addr), Tier::C { input: 0, func: f }))
        })
        .collect();
    let spec = GameSpec {
        tiers,
        overlays: BTreeMap::new(),
        budget: 1000,
        debug_io: false,
        device_funcs: Default::default(),
        main: "t".into(),
        min_pages: psxw_abi::window_pages(WINDOW_BASE),
        window_base: WINDOW_BASE,
        debug_info: false,
        probes: vec![],
        mods: None,
        slots: None,
        threads: false,
        binding: None, lifted_params: BTreeMap::new(),
    };
    let err = match psxw_link::game::link(&map, None, &spec, &[game]) {
        Ok(linked) => panic!("the alias was promoted: {:#?}", linked.native),
        Err(e) => format!("{e:#}"),
    };
    assert!(err.contains("run_local") && err.contains("keeps the guest representation (missing_uses"), "{err}");
}

/// A devirtualized pointer call (`psxw_p_`) binds as the dispatch table
/// does: the hooked function's generic entry, not a caller's scoped hook,
/// which applies to a direct call (`psxw_f_`) from the same caller
/// (plans/dispatch-binding B).
#[test]
fn devirtualized_pointer_call_skips_scoped_hooks() {
    use psxw_abi::{WINDOW_BASE, guest};
    use psxw_link::game::{GameSpec, Tier};
    use psxw_link::mods::{Chains, Impl, Layer, ModInputs, ModSpec, Patch};
    let Some((map, game)) = compile("psxw-link-scoped-pointer", "__attribute__((weak, noinline)) int func_80011100(int a) { return a * 4; }\n") else {
        return;
    };
    // Generated code (the lift tier's form): a direct call and a pointer call.
    let lifted = r#"
typedef unsigned int uint32_t;
typedef unsigned long long uint64_t;
uint64_t psxw_f_80011100(uint32_t, uint32_t, uint32_t, uint32_t);
uint64_t psxw_p_80011100(uint32_t, uint32_t, uint32_t, uint32_t);
uint64_t psxw_t_80011000(uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3) { return psxw_f_80011100(a0, a1, a2, a3); }
uint64_t psxw_t_80011200(uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3) { return psxw_p_80011100(a0, a1, a2, a3); }
"#;
    let Some((_, mut generated)) = compile("psxw-link-scoped-pointer-lift", lifted) else { return };
    generated.generated = true;
    let Some((_, mut patch)) = compile("psxw-link-scoped-pointer-mod", "int patch(int a) { return 777; }\n") else { return };
    patch.label = "mod m".into();
    let func = |o: &psxw_link::object::WasmObject, name: &str| {
        let s = o.symbols.iter().find(|s| s.defined && s.name == name).unwrap();
        s.index.unwrap() - o.num_imported_funcs()
    };
    let tiers = BTreeMap::from([
        (("t".into(), 0x8001_1100), Tier::C { input: 0, func: func(&game.obj, "func_80011100") }),
        (("t".into(), 0x8001_1000), Tier::Lift { input: 1, func: func(&generated.obj, "psxw_t_80011000") }),
        (("t".into(), 0x8001_1200), Tier::Lift { input: 1, func: func(&generated.obj, "psxw_t_80011200") }),
    ]);
    let layer = || Layer { module: 0, befores: vec![], patch: Some(Patch { imp: Impl { input: 2, function: "patch".into() }, original: None }), afters: vec![] };
    let scoped = BTreeMap::from([(("t".to_string(), 0x8001_1000), vec![layer()]), (("t".to_string(), 0x8001_1200), vec![layer()])]);
    let mods = ModSpec {
        mods: vec![ModInputs { id: "m".into(), inputs: vec![2] }],
        targets: BTreeMap::from([(("t".into(), 0x8001_1100), Chains { returns: true, default: vec![], from: scoped })]),
        data: vec![],
    };
    let spec = GameSpec {
        tiers,
        overlays: BTreeMap::new(),
        budget: 1000,
        debug_io: false,
        device_funcs: Default::default(),
        main: "t".into(),
        min_pages: psxw_abi::window_pages(WINDOW_BASE),
        window_base: WINDOW_BASE,
        debug_info: false,
        probes: vec![],
        mods: Some(mods),
        slots: None,
        threads: false,
        binding: None, lifted_params: BTreeMap::new(),
    };
    let linked = psxw_link::game::link(&map, None, &spec, &[game, generated, patch]).unwrap();
    let mut config = wasmtime::Config::new();
    config.wasm_exceptions(true);
    let engine = wasmtime::Engine::new(&config).unwrap();
    let module = wasmtime::Module::new(&engine, &linked.wasm).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let mut linker = wasmtime::Linker::new(&engine);
    linker.define_unknown_imports_as_traps(&module).unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    instance.get_typed_func::<u32, ()>(&mut store, "guest_sp_set").unwrap().call(&mut store, STACK_TOP).unwrap();
    let invoke = instance.get_typed_func::<(u32, u32, u32, u32, u32), u64>(&mut store, guest::INVOKE).unwrap();
    assert_eq!(invoke.call(&mut store, (0x8001_1000, 5, 0, 0, 0)).unwrap() as u32, 777, "a direct call takes its caller's scoped patch");
    assert_eq!(invoke.call(&mut store, (0x8001_1200, 5, 0, 0, 0)).unwrap() as u32, 20, "a pointer call enters the generic entry");
}

/// A lifted translation that takes the representation word (a native
/// reference carrier, plans/dispatch-binding B) links and runs in the
/// differential harness: no caller there passes a native handle, so its
/// pointer call is an ordinary indirect call-out.
#[test]
fn harness_links_carrier_translations() {
    use psxw_lift::program::{Code, Function, Program, Source, Start};
    use std::collections::BTreeSet;
    let Ok(ws) = Workspace::discover() else { return };
    let Ok(clang_path) = ws.clang() else {
        eprintln!("skipping: run `psxw toolchain fetch`");
        return;
    };
    // call_ptr(f, a): addiu sp, -24; sw ra; move t0, a0; jalr t0 (move a0, a1); lw ra; jr ra.
    let base = 0x8001_1200u32;
    let words = [0x27BD_FFE8u32, 0xAFBF_0010, 0x0080_4021, 0x0100_F809, 0x00A0_2021, 0x8FBF_0010, 0, 0x03E0_0008, 0x27BD_0018];
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let end = base + bytes.len() as u32;
    let f = Function { container: "t".into(), addr: base, end, name: "call_ptr".into(), sources: BTreeSet::from([Source::Symbol]), interior_of: None };
    let start = Start { name: Some("call_ptr".into()), size: Some(end - base), sources: f.sources.clone(), boundary: true };
    let program = Program {
        main: "t".into(),
        containers: BTreeMap::from([("t".into(), Code { id: "t".into(), text: vec![base..end], data: vec![], starts: BTreeMap::from([(base, start)]) })]),
    };
    let originals = psxw_inputs::originals::Originals::from_images(vec![("t".into(), base, bytes)]);
    let t = psxw_lift::translate(&program, &originals, &f, &BTreeMap::new(), &BTreeSet::new()).unwrap();
    assert!(t.transport[&0].is_ok() && t.c.contains("psxw_rep_take") && t.c.contains("psxw_call_native"), "{}", t.c);
    let dir = std::env::temp_dir().join("psxw-harness-carrier");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(psxw_lift::RT_HEADER_NAME), psxw_lift::RT_HEADER).unwrap();
    let src = dir.join("t.c");
    std::fs::write(&src, format!("#include \"{}\"\n{}", psxw_lift::RT_HEADER_NAME, t.c)).unwrap();
    let clang = psxw_cc::Clang::new(clang_path, dir.clone(), vec![], vec![], &dir).unwrap();
    let obj = dir.join("t.o");
    clang.compile_variant(&src, &obj, &[]).unwrap();
    let input = Input { label: "lift t".into(), container: "t".into(), obj: WasmObject::read(&obj).unwrap(), protos: BTreeMap::new(), generated: true, gcc: None };
    let text = 0x8001_0000..0x8002_0000;
    let container = Container {
        id: "t".into(), is_main: true, vram: text.clone(), tus: vec![], sdk: vec![], target_path: None, sha1: None,
        segments: vec![], text: vec![text], text_starts: vec![], rodata: vec![],
    };
    let map = AddressMap::synthetic("t", vec![container]);
    let export = ExportSpec { name: "lifted".into(), input: 0, function: t.cname.clone(), o32: false };
    let linked = psxw_link::link(&map, None, Mode::Harness { sp: STACK_TOP, gp: 0, dirty: None }, &[input], &[export]).unwrap();
    let mut h = Harness::new(&linked.wasm, &[], BTreeMap::new(), 1_000_000).unwrap();
    let lifted = h.func("lifted").unwrap();
    let r = h.run(&lifted, 1, [0x8001_1100, 7, 0, 0], &[], &[0; 8], 1);
    assert!(matches!(r.outcome, Outcome::Returned(_)), "{:?}", r.outcome);
    assert_eq!(r.log.len(), 1, "{:#?}", r.log);
    assert_eq!((r.log[0].kind, r.log[0].target, r.log[0].args[0]), (callout::INDIRECT, 0x8001_1100, 7));
}
