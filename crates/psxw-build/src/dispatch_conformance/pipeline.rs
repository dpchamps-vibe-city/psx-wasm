//! A fixture through the production compiler: function discovery from its
//! symbol files, the translator, the C tier for its C companions, the
//! shared tier and overlay planning ([`crate::game`]), the game-mode link,
//! and the production optimization. Only the inputs are synthetic.

use super::fixture::Variant;
use crate::game::{c_input, link_lifted, optimize, overlay_plan, thread_switches, validate};
use anyhow::{Context as _, Result, ensure};
use psxw_inputs::map::Container;
use psxw_inputs::originals::Originals;
use psxw_inputs::symbols::SymEntry;
use psxw_inputs::{AddressMap, Workspace};
use psxw_lift::program::{Program, Source};
use psxw_link::game::{GameSpec, Tier};
use psxw_link::object::{NameClass, SymbolKind, classify_name};
use psxw_link::{Gcc, Input};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// The candidates and sampled checks at shared entry addresses.
pub type Overlays = BTreeMap<u32, Vec<(String, Vec<(u32, u32)>)>>;

pub struct Built {
    /// The linked guest module (Asyncify-instrumented when it switches
    /// threads, as the build does before the merge).
    pub linked: Vec<u8>,
    /// The same after the production optimization.
    pub optimized: Vec<u8>,
    /// What the planner derived: evidence for a wrong dispatch.
    pub overlays: Overlays,
    /// `container 0xADDR` → tier.
    pub tiers: BTreeMap<String, &'static str>,
    /// Functions the translator couldn't lift, and why.
    pub untranslated: Vec<String>,
}

/// The address map and original images a fixture's inputs describe.
pub fn inputs(v: &Variant) -> (AddressMap, Originals) {
    let containers = v
        .images
        .iter()
        .map(|im| Container {
            id: im.id.clone(),
            is_main: im.id == v.main,
            vram: im.range(),
            tus: vec![],
            sdk: vec![],
            target_path: None,
            sha1: None,
            segments: vec![(0..im.bytes.len() as u32, im.base)],
            text: vec![im.base..im.base + im.text],
            text_starts: vec![im.base],
            rodata: vec![],
        })
        .collect();
    let symbols: HashMap<String, Vec<SymEntry>> = v
        .images
        .iter()
        .map(|im| {
            let entries = im
                .funcs
                .iter()
                .map(|(name, addr, size)| SymEntry { name: name.clone(), addr: *addr, size: *size, ty: Some("func".into()) })
                .collect();
            (im.id.clone(), entries)
        })
        .collect();
    let map = AddressMap::synthetic_with_symbols(&v.main, containers, symbols);
    let originals = Originals::from_images(v.images.iter().map(|im| (im.id.clone(), im.base, im.bytes.clone())).collect());
    (map, originals)
}

/// Build a variant's guest module in `dir`.
pub fn build(ws: &Workspace, v: &Variant, dir: &Path) -> Result<Built> {
    std::fs::create_dir_all(dir)?;
    let (map, originals) = inputs(v);
    let mut program = Program::from_map(&map);

    // The census: each C companion compiled through the C tier's
    // normalization; its definitions seed discovery as the decomp's do.
    let cdir = dir.join("c");
    std::fs::create_dir_all(&cdir)?;
    let clang = psxw_cc::Clang::new(ws.clang()?, cdir.clone(), vec![], vec![], &cdir)?;
    let mut census = Vec::new();
    for im in v.images.iter().filter(|im| im.c.is_some()) {
        let src = cdir.join(format!("{}.c", im.id));
        std::fs::write(&src, im.c.as_deref().unwrap_or_default())?;
        let compiled = psxw_cc::compile(&clang, &src, &cdir, &im.id, &[], None)?;
        ensure!(compiled.ok, "{}'s C doesn't compile: {:?}", im.id, compiled.errors);
        let obj = compiled.object.context("no object")?;
        for s in psxw_link::object::WasmObject::read(&obj)?.symbols.iter().filter(|s| s.kind == SymbolKind::Function && s.defined) {
            if let NameClass::Guest(n) = classify_name(&s.name) {
                if let Some(r) = map.resolve(&im.id, n) {
                    program.add(&im.id, r.addr, Some(n), Source::Defined, true);
                }
            }
        }
        census.push((im.id.clone(), obj));
    }

    let containers: Vec<String> = map.containers.iter().map(|c| c.id.clone()).collect();
    let outcome = psxw_lift::translate_all(&mut program, &originals, &containers);
    let mut translations = outcome.ok;
    translations.sort_by(|a, b| (&a.func.container, a.func.addr).cmp(&(&b.func.container, b.func.addr)));
    let untranslated = outcome.failed.iter().map(|(f, e)| format!("{} 0x{:08X}: {}", f.container, f.addr, e.message)).collect();

    let gcc = Gcc { division: psxw_difftest::division_convention(&translations), builtin_memcpy: true, main_calls_main: true };
    let calls = dir.join("calls");
    let mut link_inputs: Vec<Input> = census
        .iter()
        .map(|(c, obj)| c_input(&clang, c, obj, &calls.join(c), &[], None, gcc))
        .collect::<Result<_>>()?;
    let mut c_defs: BTreeMap<(String, u32), (usize, u32)> = BTreeMap::new();
    for (i, inp) in link_inputs.iter().enumerate() {
        for s in inp.obj.symbols.iter().filter(|s| s.kind == SymbolKind::Function && s.defined) {
            let NameClass::Guest(n) = classify_name(&s.name) else { continue };
            let (Some(r), Some(idx)) = (map.resolve(&inp.container, n), s.index) else { continue };
            c_defs.entry((inp.container.clone(), r.addr)).or_insert((i, idx - inp.obj.num_imported_funcs()));
        }
    }
    let mut tiers: BTreeMap<(String, u32), Tier> = BTreeMap::new();
    let mut lifted = Vec::new();
    for t in &translations {
        let key = (t.func.container.clone(), t.func.addr);
        match c_defs.get(&key) {
            Some(&(input, func)) => {
                tiers.insert(key, Tier::C { input, func });
            }
            None => lifted.push(t),
        }
    }
    let switches = thread_switches(&lifted);
    link_lifted(ws, &dir.join("lift"), &lifted, &mut link_inputs, &mut tiers)?;
    let overlays = overlay_plan(&program, &originals, &tiers)?;

    let defaults = crate::game::Options::new("dispatch");
    let spec = GameSpec {
        tiers: tiers.clone(),
        overlays: overlays.clone(),
        budget: defaults.budget,
        debug_io: false,
        device_funcs: Default::default(),
        main: v.main.clone(),
        min_pages: 1024,
        window_base: psxw_abi::WINDOW_BASE,
        debug_info: false,
        probes: vec![],
        mods: None,
        slots: None,
        threads: !switches.is_empty(),
    };
    ensure!(spec.threads == v.threads, "the fixture says threads = {}, the build found {} switches", v.threads, switches.len());
    let rodata = psxw_link::layout::Rodata::new(&map, &originals);
    let linked = psxw_link::game::link(&map, Some(&rodata), &spec, &link_inputs)?;
    validate(&linked.wasm).context("guest module fails validation")?;
    let guest = if spec.threads { crate::game::asyncify(ws, dir, &linked.wasm, &switches, false)? } else { linked.wasm.clone() };
    let guest_path = dir.join("guest.wasm");
    std::fs::write(&guest_path, &guest)?;
    let opt_path = dir.join("guest-opt.wasm");
    optimize(ws, &guest_path, &opt_path)?;
    let optimized = std::fs::read(&opt_path)?;
    validate(&optimized).context("optimized module fails validation")?;
    let tiers = tiers
        .iter()
        .map(|((c, a), t)| {
            let k = match t {
                Tier::C { .. } => "c",
                Tier::Lift { .. } => "lift",
                Tier::Adapter(_) => "adapter",
            };
            (format!("{c} 0x{a:08X}"), k)
        })
        .collect();
    Ok(Built { linked: guest, optimized, overlays, tiers, untranslated })
}
