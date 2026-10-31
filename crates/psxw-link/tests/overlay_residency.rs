//! Overlay residency (D69) in a linked game: two overlays whose code
//! starts with the same words share a slot, and one of them has a function
//! the other doesn't. Calls go to whichever overlay the disc last wrote
//! there, and a call to code that isn't resident traps naming what is.
//! Skips without the pinned clang.

use psxw_abi::{WINDOW_BASE, WINDOW_MASK};
use psxw_inputs::map::Container;
use psxw_inputs::originals::Originals;
use psxw_inputs::{AddressMap, Workspace};
use psxw_link::Input;
use psxw_link::game::{GameSpec, Tier, link, residency};
use psxw_link::object::{SymbolKind, WasmObject};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use wasmtime::{Caller, Config, Engine, Linker, Module, Store};

const SLOT: Range<u32> = 0x8010_0000..0x8010_0200;

const EXE: &str = "int func_80100000(int x);\nint func_80100100(int x);\n\
    int func_80010000(int x) { return func_80100000(x); }\n\
    int func_80010100(int x) { return func_80100100(x); }\n";
const OVL_A: &str = "int func_80100000(int x) { return x + 1; }\n";
const OVL_B: &str = "int func_80100000(int x) { return x + 2; }\nint func_80100100(int x) { return x + 3; }\n";

fn container(id: &str, text: Range<u32>) -> Container {
    Container {
        id: id.into(),
        is_main: id == "exe",
        vram: text.clone(),
        tus: vec![],
        sdk: vec![],
        target_path: None,
        sha1: None,
        segments: vec![],
        text: vec![text],
        text_starts: vec![],
        rodata: vec![],
    }
}

/// An overlay's "original code": the same two first words in both, then
/// its own.
fn image(fill: u32) -> Vec<u8> {
    let mut words = vec![0x27BD_FFE8u32, 0xAFBF_0010];
    words.resize((SLOT.end - SLOT.start) as usize / 4, fill);
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

#[test]
fn calls_go_to_the_overlay_the_disc_wrote() {
    let Ok(ws) = Workspace::discover() else { return };
    let Ok(clang_path) = ws.clang() else {
        eprintln!("skipping: run `psxw toolchain fetch`");
        return;
    };
    let dir = std::env::temp_dir().join("psxw-overlay-residency");
    std::fs::create_dir_all(&dir).unwrap();
    let clang = psxw_cc::Clang::new(clang_path, dir.clone(), vec![], vec![], &dir).unwrap();
    let mut inputs = Vec::new();
    let mut tiers = BTreeMap::new();
    for (id, src) in [("exe", EXE), ("ovl_a", OVL_A), ("ovl_b", OVL_B)] {
        let c = dir.join(format!("{id}.c"));
        let o = dir.join(format!("{id}.o"));
        std::fs::write(&c, src).unwrap();
        clang.compile_variant(&c, &o, &[]).unwrap();
        let obj = WasmObject::read(&o).unwrap();
        for s in obj.symbols.iter().filter(|s| s.kind == SymbolKind::Function && s.defined) {
            let addr = u32::from_str_radix(s.name.strip_prefix("func_").unwrap(), 16).unwrap();
            let func = s.index.unwrap() - obj.num_imported_funcs();
            tiers.insert((id.to_string(), addr), Tier::C { input: inputs.len(), func });
        }
        let protos = psxw_cc::protos::for_tu(&clang, &c).unwrap().into_iter().map(|p| (p.name.clone(), p)).collect();
        inputs.push(Input { label: id.into(), container: id.into(), obj, protos, generated: false, gcc: None });
    }
    let map = AddressMap::synthetic(
        "exe",
        vec![container("exe", 0x8001_0000..0x8001_1000), container("ovl_a", SLOT), container("ovl_b", SLOT)],
    );
    let (a, b) = (image(0x1111_1111), image(0x2222_2222));
    let originals = Originals::from_images(vec![("ovl_a".into(), SLOT.start, a.clone()), ("ovl_b".into(), SLOT.start, b.clone())]);
    let spec = GameSpec {
        tiers,
        resident: residency(&map, &originals).unwrap(),
        budget: 1000,
        debug_io: false,
        device_funcs: BTreeSet::new(),
        main: "exe".into(),
        min_pages: 1024,
        window_base: WINDOW_BASE,
        debug_info: false,
        probes: vec![],
        mods: None,
        slots: None,
        threads: false,
        costs: Default::default(),
    };
    let linked = link(&map, None, &spec, &inputs).unwrap();
    wasmparser::Validator::new().validate_all(&linked.wasm).unwrap();

    let mut config = Config::new();
    config.wasm_exceptions(true);
    let engine = Engine::new(&config).unwrap();
    let module = Module::new(&engine, &linked.wasm).unwrap();
    // The trap's address and the name of what is resident there.
    let mut linker: Linker<Option<(u32, String)>> = Linker::new(&engine);
    linker
        .func_wrap("psx", "not_resident", |mut c: Caller<'_, Option<(u32, String)>>, addr: u32, name: u32| -> wasmtime::Result<()> {
            let mem = c.get_export("memory").unwrap().into_memory().unwrap();
            let at = (WINDOW_BASE + (name & WINDOW_MASK)) as usize;
            let bytes = &mem.data(&c)[at..at + 16];
            let text = if name == 0 { "nothing".into() } else { String::from_utf8_lossy(&bytes[..bytes.iter().position(|&b| b == 0).unwrap()]).into_owned() };
            *c.data_mut() = Some((addr, text));
            wasmtime::bail!("not resident")
        })
        .unwrap();
    linker.define_unknown_imports_as_traps(&module).unwrap();
    let mut store = Store::new(&engine, None);
    let inst = linker.instantiate(&mut store, &module).unwrap();
    let mem = inst.get_memory(&mut store, "memory").unwrap();
    let invoke = inst.get_typed_func::<(u32, u32, u32, u32, u32), u64>(&mut store, "guest_invoke").unwrap();
    let loaded = inst.get_typed_func::<(u32, u32), ()>(&mut store, "psxmod_loaded").unwrap();
    inst.get_typed_func::<u32, ()>(&mut store, "guest_sp_set").unwrap().call(&mut store, 0x801F_F000).unwrap();
    let call = |store: &mut Store<_>, addr: u32| invoke.call(store, (addr, 5, 0, 0, 0)).map(|v| v as u32);
    // What the disc writes, as the runtime reports it.
    let load = |store: &mut Store<_>, image: &[u8]| {
        mem.write(&mut *store, (WINDOW_BASE + (SLOT.start & WINDOW_MASK)) as usize, image).unwrap();
        loaded.call(store, (SLOT.start, image.len() as u32)).unwrap();
    };

    // Nothing loaded yet.
    assert!(call(&mut store, 0x8001_0000).is_err());
    assert_eq!(store.data().clone(), Some((0x8010_0000, "nothing".into())));

    // ovl_b, then ovl_a, in the slot: the same first words, different code.
    load(&mut store, &b);
    assert_eq!(call(&mut store, 0x8001_0000).unwrap(), 7);
    assert_eq!(call(&mut store, 0x8001_0100).unwrap(), 8);
    load(&mut store, &a);
    assert_eq!(call(&mut store, 0x8001_0000).unwrap(), 6);
    // Through a function pointer too.
    assert_eq!(call(&mut store, SLOT.start).unwrap(), 6);

    // Only ovl_b has code at 0x80100100, and ovl_a is resident.
    *store.data_mut() = None;
    assert!(call(&mut store, 0x8001_0100).is_err());
    assert_eq!(store.data().clone(), Some((0x8010_0100, "ovl_a".into())));
}
