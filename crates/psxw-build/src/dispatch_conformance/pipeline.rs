//! A fixture through the production compiler: function discovery from its
//! symbol files, the translator, the C tier for its C companions, the
//! shared tier and overlay planning ([`crate::game`]), the game-mode link,
//! and the production optimization. Only the inputs are synthetic.

use super::fixture::Variant;
use crate::game::{c_input, link_lifted, optimize, thread_switches, validate};
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

pub use crate::binding::Overlays;

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
    /// The explicit-binding plan and what stays legacy.
    pub binding: crate::binding::Report,
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
    // The facts go through the same schema as a game's `[dispatch]`; the
    // C includes the header it generates, as the census arranges.
    let section = v.dispatch.as_deref().map(psxw_inputs::dispatch::parse).transpose()?;
    if let Some(s) = &section {
        std::fs::write(cdir.join(psxw_inputs::dispatch::HEADER), psxw_inputs::dispatch::header(s))?;
    }
    let clang = psxw_cc::Clang::new(ws.clang()?, cdir.clone(), vec![cdir.clone()], vec![], &cdir)?;
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
    // With DWARF, as every production object has it (D53).
    let calls = dir.join("calls");
    std::fs::create_dir_all(&calls)?;
    psxw_cc::objects_compiled_with(&calls, &["-g"])?;
    let mut link_inputs: Vec<Input> = census
        .iter()
        .map(|(c, obj)| c_input(&clang, c, obj, &calls.join(c), &["-g"], None, gcc))
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
    // Supplied C stays the implementation: no dispatch or binding concern
    // selects another tier for it.
    for key in c_defs.keys().filter(|k| translations.iter().any(|t| (&t.func.container, t.func.addr) == (&k.0, k.1))) {
        ensure!(matches!(tiers.get(key), Some(Tier::C { .. })), "{} {:#010x}: supplied C is not linked at the C tier", key.0, key.1);
    }
    let (binding_plan, overlays, mut binding) =
        crate::binding::dispatch(section.as_ref(), &map, &program, &originals, &translations, &tiers, &link_inputs, false)?;

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
        binding: binding_plan,
    };
    ensure!(spec.threads == v.threads, "the fixture says threads = {}, the build found {} switches", v.threads, switches.len());
    let rodata = psxw_link::layout::Rodata::new(&map, &originals);
    let linked = psxw_link::game::link(&map, Some(&rodata), &spec, &link_inputs)?;
    validate(&linked.wasm).context("guest module fails validation")?;
    binding.references = linked.references.clone();
    if let Some(b) = &spec.binding {
        // Covered paths must not select implementations by code words.
        let sampled: Vec<String> = sampled_stubs(&linked.wasm)?.into_iter().filter(|a| b.slots.contains_key(a)).map(|a| format!("{a:#010x}")).collect();
        ensure!(sampled.is_empty(), "resident-word selectors at covered addresses: {}", sampled.join(", "));
        // Supplied C keeps its source locations through instrumentation.
        if !c_defs.is_empty() {
            let debug = psxw_link::game::link(&map, Some(&rodata), &GameSpec { debug_info: true, ..spec.clone() }, &link_inputs)?;
            debug_locations(&debug.wasm)?;
        }
    }
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
    Ok(Built { linked: guest, optimized, overlays, tiers, untranslated, binding })
}

/// Each C subprogram's DWARF starts in the body the name section gives that
/// function, never in a generated wrapper (the operation, its o32 entry):
/// breakpoints refer to the C that actually runs.
fn debug_locations(wasm: &[u8]) -> Result<()> {
    let subs = psxw_link::dwarf::subprograms(&psxw_link::dwarf::sections(wasm)?)?;
    ensure!(!subs.is_empty(), "the debug link carries no C subprograms");
    let starts = psxw_link::dwarf::body_starts(wasm)?;
    let mut imported = 0u32;
    let mut names: BTreeMap<u32, String> = BTreeMap::new();
    for p in wasmparser::Parser::new(0).parse_all(wasm) {
        match p? {
            wasmparser::Payload::ImportSection(r) => {
                imported += r.into_imports().filter(|i| i.as_ref().is_ok_and(|i| matches!(i.ty, wasmparser::TypeRef::Func(_)))).count() as u32;
            }
            wasmparser::Payload::CustomSection(c) => {
                let wasmparser::KnownCustom::Name(n) = c.as_known() else { continue };
                for n in n {
                    if let wasmparser::Name::Function(map) = n? {
                        for f in map {
                            let f = f?;
                            names.insert(f.index, f.name.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for (name, low) in subs {
        // The body holding it; its size field precedes its locals.
        let k = starts.iter().rposition(|&s| u64::from(s) <= low + 5).context("a subprogram before the code")?;
        let body = names.get(&(imported + k as u32)).cloned().unwrap_or_default();
        ensure!(body.ends_with(&format!(":{name}")), "{name}'s debug location is in `{body}`, not its C body");
    }
    Ok(())
}

/// Addresses of the linked module's sampled-word overlay stubs, by their
/// names (`overlay:<addr>`).
fn sampled_stubs(wasm: &[u8]) -> Result<Vec<u32>> {
    let mut out = Vec::new();
    for p in wasmparser::Parser::new(0).parse_all(wasm) {
        let wasmparser::Payload::CustomSection(c) = p? else { continue };
        let wasmparser::KnownCustom::Name(names) = c.as_known() else { continue };
        for n in names {
            let wasmparser::Name::Function(map) = n? else { continue };
            for f in map {
                if let Some(a) = f?.name.strip_prefix("overlay:").and_then(|h| u32::from_str_radix(h, 16).ok()) {
                    out.push(a);
                }
            }
        }
    }
    Ok(out)
}
