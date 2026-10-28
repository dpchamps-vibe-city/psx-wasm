//! `psxw build <game>`: the guest module from every container (decompiled
//! C, generated C, adapter bindings), merged with `runtime.wasm` into
//! `out/<game>/game.wasm`.

use anyhow::{Context as _, Result, bail, ensure};
use psxw_cc::protos::Proto;
use psxw_inputs::Workspace;
use psxw_inputs::boundary::{Boundary, Policy};
use psxw_inputs::identity::SigDb;
use psxw_lift::program::Source;
use psxw_link::game::{GameSpec, Tier};
use psxw_link::mods::ModSpec;
use psxw_link::object::{NameClass, SymbolKind, WasmObject, classify_name};
use psxw_link::{Gcc, Input};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct Options {
    pub game: String,
    /// Trap on guest accesses to the I/O window.
    pub debug_io: bool,
    /// Loop iterations between safepoints.
    pub budget: u32,
    /// Compile the C with UBSan in trap mode (`game-ubsan.wasm`).
    pub ubsan: bool,
    /// SDK libraries (`libpad`, …) built from their original code instead
    /// of adapters, with I/O-window accesses routed to the runtime
    /// (`game-orig-<libs>.wasm`, for adapter conformance).
    pub original: Vec<String>,
    /// More functions, by name, whose entries count in the run report
    /// (`probe.<addr>`), besides the manifest's main loop.
    pub probes: Vec<String>,
    /// Mods to link in (plans/M11), as psxw-modkit planned them.
    pub mods: Option<ModPlan>,
    /// A moddable base (plans/M11 phase 2): every function a mod can hook
    /// checks a hook slot that mods loaded at run time fill
    /// (`game-moddable.wasm`).
    pub moddable: bool,
    /// A debug build (D53, `game-debug.wasm`): the C with DWARF, the window
    /// at psxw-abi `DEBUG_WINDOW_BASE` so a debugger's addresses are the
    /// game's, merged without binaryen.
    pub debug: bool,
    /// More functions, by name, built at their translated tier besides the
    /// manifest's `[tier]` pins (`game-lift-<names>.wasm`): a moddable base
    /// whose slot for one is o32 instead of C-typed (plans/M12).
    pub lift: Vec<String>,
}

/// What the modkit links into a build (plans/M11).
#[derive(Debug, Clone, Default)]
pub struct ModPlan {
    /// Where the build's outputs go, instead of `out/<game>/`.
    pub out: PathBuf,
    /// Mod objects; `spec` indexes them from 0.
    pub inputs: Vec<Input>,
    pub spec: ModSpec,
}

impl Options {
    pub fn new(game: &str) -> Self {
        Self {
            game: game.into(),
            debug_io: false,
            budget: 1000,
            ubsan: false,
            original: vec![],
            probes: vec![],
            mods: None,
            moddable: false,
            debug: false,
            lift: vec![],
        }
    }

    /// The output name's suffix: `-ubsan`, `-orig-libpad+libetc`.
    pub fn suffix(&self) -> String {
        let mut s = String::new();
        if !self.original.is_empty() {
            s += &format!("-orig-{}", self.original.join("+"));
        }
        if self.ubsan {
            s += "-ubsan";
        }
        if !self.probes.is_empty() {
            s += "-probed";
        }
        if self.moddable {
            s += "-moddable";
        }
        if !self.lift.is_empty() {
            s += &format!("-lift-{}", self.lift.join("+"));
        }
        if self.debug {
            s += "-debug";
        }
        s
    }
}

/// UBSan in trap mode: each check is an `unreachable`, located by the
/// trap's backtrace. (Signed overflow is defined under `-fwrapv`.)
pub const UBSAN_FLAGS: &[&str] = &["-fsanitize=undefined", "-fsanitize-trap=undefined", "-fno-sanitize=function"];

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub game: String,
    pub commit: String,
    /// Objects with adapters whose identity isn't validated (their
    /// functions were translated instead).
    pub unvalidated_objects: Vec<String>,
    /// SDK functions in adapter-policy libraries that touch hardware
    /// themselves but have no adapter; none is reachable from guest code
    /// (that fails the build).
    pub hardware_unbound: Vec<String>,
    /// Functions whose C is `void` but whose result C callers use: they
    /// run translated (`void_results`).
    pub void_results: Vec<String>,
    /// C calls whose declaration differs from the callee's definition in
    /// C-level kinds but not in wasm types (a struct by value against a
    /// scalar, say): routed through o32, which converts as the PS1 does.
    pub abi_mismatches: Vec<String>,
    /// Functions per tier.
    pub tiers: BTreeMap<String, usize>,
    /// Per container and tier.
    pub by_container: BTreeMap<String, BTreeMap<String, usize>>,
    pub adapters: Vec<String>,
    pub overlay_addresses: usize,
    pub aux_symbols: usize,
    pub matched_rodata: usize,
    pub debug_io: bool,
    pub guest_bytes: usize,
    pub game_bytes: usize,
    pub wasm: String,
    /// The unmerged guest module (the browser's dev mode).
    pub guest: String,
}

/// The `-all` minus features clang and rustc don't emit and hosts may lack.
const FEATURES: &[&str] = &[
    "--enable-bulk-memory",
    "--enable-bulk-memory-opt",
    "--enable-sign-ext",
    "--enable-mutable-globals",
    "--enable-nontrapping-float-to-int",
    "--enable-multivalue",
    "--enable-reference-types",
    "--enable-call-indirect-overlong",
    "--enable-exception-handling",
];

fn run(cmd: &mut Command) -> Result<()> {
    let out = cmd.output().with_context(|| format!("running {cmd:?}"))?;
    if !out.status.success() {
        bail!("{:?} failed:\n{}", cmd.get_program(), String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

pub fn validate(wasm: &[u8]) -> Result<()> {
    let features = wasmparser::WasmFeatures::default();
    wasmparser::Validator::new_with_features(features).validate_all(wasm)?;
    Ok(())
}

pub fn out_dir(ws: &Workspace, game: &str) -> PathBuf {
    ws.out_dir().join(game)
}

pub fn build(ws: &Workspace, opts: &Options) -> Result<(Report, PathBuf)> {
    let prep = prepare(ws, opts)?;
    finish(ws, opts, prep)
}

/// Everything about a build up to the link: the inputs, each function's
/// tier, the translations.
pub struct Prepared {
    pub inputs: psxw_lift::run::Inputs,
    pub commit: String,
    runtime_path: PathBuf,
    adapters: Vec<String>,
    pub link_inputs: Vec<Input>,
    pub tiers: BTreeMap<(String, u32), Tier>,
    pub translations: Vec<psxw_lift::Translation>,
    /// Functions the translator expands into their translated callers.
    pub expanded: BTreeSet<(String, u32)>,
    /// Each C definition's census object: (container, object stem).
    pub c_objects: BTreeMap<(String, u32), (String, String)>,
    /// Adapter names, by adapter id.
    pub adapter_names: Vec<String>,
    /// What GCC did that C inputs are linked to reproduce.
    pub gcc: Gcc,
    unvalidated: BTreeSet<String>,
    hardware: Vec<Unbound>,
    void_used: BTreeSet<(String, u32)>,
    overlays: BTreeMap<u32, Vec<(String, [u32; 2])>>,
    spec: GameSpec,
}

/// A build's inputs and tiers.
pub fn prepare(ws: &Workspace, opts: &Options) -> Result<Prepared> {
    // A debug build can be a moddable base (script mods, D56); mods linked
    // in would need building for its window.
    ensure!(!opts.debug || (!opts.ubsan && opts.mods.is_none()), "a debug build doesn't combine with --ubsan or linked-in mods yet");
    // Incremental: cheap when the runtime hasn't changed.
    let runtime_path = crate::runtime::build(ws)?;
    let runtime = std::fs::read(&runtime_path)?;
    let adapters = crate::runtime::adapters(&runtime)?;

    let mut inputs = psxw_lift::run::load(ws, &opts.game)?;
    let commit = inputs.commit.clone();
    eprintln!("build: {} @ {}", opts.game, &commit[..7]);

    // The SDK boundary (M8): policy per library and object; adapters bind
    // only to objects they are validated for.
    let boundary = match &inputs.manifest.game.boundary {
        Some(p) => Boundary::load(&ws.root.join(p))?,
        None => Boundary::default(),
    };
    let runtime_devices = crate::runtime::devices(&runtime)?;
    let mut device_libs = boundary.device_libraries();
    if let Some(l) = device_libs.iter().find(|l| !runtime_devices.contains(l)) {
        bail!("the boundary runs {l} over devices, but the runtime has no device support for it");
    }
    // Libraries built from their original code on request.
    device_libs.extend(opts.original.iter().cloned());
    let db = SigDb::pinned(ws)?;
    let identity = psxw_inputs::identity::identify(&opts.game, &inputs.map, &inputs.originals, &db);
    let keys: BTreeMap<&str, &str> =
        identity.identities.iter().filter_map(|i| Some((i.member.as_str(), i.key.as_deref()?))).collect();
    let validated: BTreeSet<(&str, &str)> = sdk_psyq::validated::VALIDATED.iter().copied().collect();
    let (translations, _, _, _, inline) = psxw_lift::run::translate(&mut inputs);
    let expanded: BTreeSet<(String, u32)> = inline
        .keys()
        .filter_map(|k| {
            let (c, a) = k.split_once(' ')?;
            Some((c.to_string(), psxw_inputs::parse_u32(a)?))
        })
        .collect();
    let lift_dir = psxw_lift::run::out_dir(ws, &inputs);

    // C inputs: every census object with its prototype facts.
    let clang = psxw_cc::Clang::new(ws.clang()?, lift_dir.clone(), vec![], vec![], &lift_dir)?;
    let gcc = Gcc {
        division: psxw_difftest::division_convention(&translations),
        builtin_memcpy: !inputs.manifest.reference.cc1flags.iter().any(|f| f == "-fno-builtin"),
        main_calls_main: true,
    };
    let mut objs: Vec<(String, PathBuf)> = Vec::new();
    for c in &inputs.map.containers {
        let Ok(rd) = std::fs::read_dir(inputs.census_obj.join(&c.id)) else { continue };
        let mut v: Vec<PathBuf> =
            rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == "o")).collect();
        v.sort();
        objs.extend(v.into_iter().map(|p| (c.id.clone(), p)));
    }
    let ubsan_dir = out_dir(ws, &opts.game).join("ubsan");
    // The manifest's documented reports, as a clang ignore list; objects
    // older than it are rebuilt.
    let ignorelist = ubsan_dir.join("ignorelist.txt");
    let mut ubsan_flags: Vec<String> = UBSAN_FLAGS.iter().map(|s| s.to_string()).collect();
    if opts.ubsan {
        std::fs::create_dir_all(&ubsan_dir)?;
        let text: String = inputs.manifest.ubsan.ignore.keys().map(|f| format!("fun:{f}\n")).collect();
        if std::fs::read_to_string(&ignorelist).ok().as_deref() != Some(text.as_str()) {
            std::fs::write(&ignorelist, &text)?;
        }
        ubsan_flags.push(format!("-fsanitize-ignorelist={}", ignorelist.display()));
    }
    let ubsan_flags: Vec<&str> = ubsan_flags.iter().map(String::as_str).collect();
    // A debug build's objects carry DWARF, the decomp's relative paths
    // under DEBUG_SOURCES' `source/game`. Objects compiled with other
    // debug flags are rebuilt.
    let debug_dir = out_dir(ws, &opts.game).join("debug");
    let debug_flags = ["-g".to_string(), format!("-fdebug-compilation-dir={}", DEBUG_SOURCES[0])];
    let debug_flags: Vec<&str> = debug_flags.iter().map(String::as_str).collect();
    if opts.debug {
        let stamp = debug_dir.join("flags");
        let want = format!("{}\n{}\n", debug_flags.join(" "), debug_lift_flags().join(" "));
        if std::fs::read_to_string(&stamp).ok().as_deref() != Some(want.as_str()) {
            let _ = std::fs::remove_dir_all(&debug_dir);
            std::fs::create_dir_all(&debug_dir)?;
            std::fs::write(&stamp, &want)?;
        }
    }
    // GCC 2.95's call structure (D50): every non-inline function weak and
    // noinline, as GCC at -O2 left every call to one. Each call the
    // original makes is then a call here (a mod's hook sees it), and
    // clang's inlining doesn't decide where loops spend the safepoint
    // budget, so hooking a function doesn't change the game's timing.
    let calls_dir = out_dir(ws, &opts.game).join("calls");
    let mut link_inputs: Vec<Input> = objs
        .par_iter()
        .map(|(c, o)| -> Result<Input> {
            let stem = o.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            let names: BTreeSet<String> = WasmObject::read(o)?
                .symbols
                .iter()
                .filter(|s| s.kind == SymbolKind::Function && s.defined)
                .map(|s| s.name.clone())
                .collect();
            let text = std::fs::read_to_string(o.with_extension("i"))?;
            let variant = psxw_cc::ast::hook_variant(&text, &names);
            let (dir, flags): (PathBuf, &[&str]) = if opts.ubsan {
                (ubsan_dir.join(c), &ubsan_flags)
            } else if opts.debug {
                (debug_dir.join(c), &debug_flags)
            } else {
                (calls_dir.join(c), &[])
            };
            std::fs::create_dir_all(&dir)?;
            let vi = dir.join(format!("{stem}.i"));
            let vo = dir.join(format!("{stem}.o"));
            let older = |a: &Path, b: &Path| -> bool {
                match (std::fs::metadata(a).and_then(|m| m.modified()), std::fs::metadata(b).and_then(|m| m.modified())) {
                    (Ok(x), Ok(y)) => x < y,
                    _ => true,
                }
            };
            let stale = std::fs::read_to_string(&vi).ok().as_deref() != Some(variant.as_str())
                || !vo.is_file()
                || (opts.ubsan && older(&vo, &ignorelist));
            if stale {
                std::fs::write(&vi, &variant)?;
                clang.compile_variant(&vi, &vo, flags)?;
            }
            let protos: BTreeMap<String, Proto> =
                psxw_cc::protos::cached(&clang, &vi)?.into_iter().fold(BTreeMap::new(), |mut m, p| {
                    if p.defined || !m.contains_key(&p.name) {
                        m.insert(p.name.clone(), p);
                    }
                    m
                });
            Ok(Input {
                label: format!("{c}/{stem}"),
                container: c.clone(),
                obj: WasmObject::read(&vo)?,
                protos,
                generated: false,
                gcc: Some(gcc),
            })
        })
        .collect::<Result<_>>()?;

    // C definitions by (container, address).
    let mut c_defs: BTreeMap<(String, u32), (usize, u32)> = BTreeMap::new();
    let mut c_objects: BTreeMap<(String, u32), (String, String)> = BTreeMap::new();
    for (i, inp) in link_inputs.iter().enumerate() {
        for s in inp.obj.symbols.iter().filter(|s| s.kind == SymbolKind::Function && s.defined) {
            let NameClass::Guest(n) = classify_name(&s.name) else { continue };
            let Some(r) = inputs.map.resolve(&inp.container, n) else { continue };
            let Some(idx) = s.index else { continue };
            c_defs.entry((inp.container.clone(), r.addr)).or_insert((i, idx - inp.obj.num_imported_funcs()));
            let stem = objs[i].1.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            c_objects.entry((inp.container.clone(), r.addr)).or_insert((inp.container.clone(), stem));
        }
    }

    let void_used = void_results(&inputs.map, &inputs.originals, &link_inputs, &c_defs, &translations);
    if !void_used.is_empty() {
        eprintln!("build: {} void C functions whose result callers use run translated", void_used.len());
    }

    // Tiers.
    let pinned: BTreeSet<&str> = inputs
        .manifest
        .tier
        .iter()
        .filter(|(_, p)| p.tier == "lift")
        .map(|(n, _)| n.as_str())
        .chain(opts.lift.iter().map(String::as_str))
        .collect();
    let adapter_id: BTreeMap<&str, u32> = adapters.iter().enumerate().map(|(i, n)| (n.as_str(), i as u32)).collect();
    let mut tiers: BTreeMap<(String, u32), Tier> = BTreeMap::new();
    let mut lifted = Vec::new();
    let mut unvalidated: BTreeSet<String> = BTreeSet::new();
    for t in &translations {
        let key = (t.func.container.clone(), t.func.addr);
        let member = inputs.map.sdk_at(&t.func.container, t.func.addr).map(|p| p.member.as_str());
        let original = member.is_some_and(|m| device_libs.iter().any(|l| m.split('/').next() == Some(l.as_str())));
        let policy = member.map(|m| boundary.policy(m).unwrap_or(Policy::Adapter));
        let adapter = adapter_id.get(t.func.name.as_str()).filter(|_| policy == Some(Policy::Adapter) && !original);
        let object_ok = member.is_some_and(|m| keys.get(m).is_some_and(|k| validated.contains(&(m, *k))));
        if let (Some(_), Some(m), false) = (adapter, member, object_ok) {
            unvalidated.insert(format!("{m} ({})", keys.get(m).unwrap_or(&"no identity key")));
        }
        let tier = if let (Some(&id), true) = (adapter, object_ok) {
            Tier::Adapter(id)
        } else if let (Some(&(input, func)), false) =
            (c_defs.get(&key), pinned.contains(t.func.name.as_str()) || void_used.contains(&key))
        {
            debug_assert!(t.func.sources.contains(&Source::Defined));
            Tier::C { input, func }
        } else {
            lifted.push(t);
            continue;
        };
        tiers.insert(key, tier);
    }
    let hardware = hardware_unbound(&inputs.map, &boundary, &translations, &tiers);
    if let Some(bad) = Some(hardware.iter().filter(|h| h.reachable).map(|h| h.to_string()).collect::<Vec<_>>()).filter(|v| !v.is_empty()) {
        bail!(
            "functions that touch hardware themselves are reachable from guest code with no validated adapter \
             (their objects need adapters, or a device policy):\n  {}",
            bad.join("\n  ")
        );
    }
    bios_calls(&out_dir(ws, &opts.game), &lifted)?;
    let failures = psxw_lift::run::write_and_compile(ws, &lift_dir, &lifted)?;
    if !failures.is_empty() {
        bail!("{} translations don't compile; run `psxw lift {}`", failures.len(), opts.game);
    }
    if opts.debug {
        debug_lift(&clang, &lift_dir, &debug_dir.join("lift"), &lifted)?;
    }
    for t in &lifted {
        let path = if opts.debug { debug_dir.join("lift") } else { lift_dir.clone() }
            .join(&t.func.container)
            .join(format!("{:08X}.o", t.func.addr));
        let obj = WasmObject::read(&path)?;
        let local = obj
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Function && s.defined && s.name == t.cname)
            .and_then(|s| s.index)
            .with_context(|| format!("{} defines no {}", path.display(), t.cname))?
            - obj.num_imported_funcs();
        link_inputs.push(Input {
            label: format!("lift {} {:08X}", t.func.container, t.func.addr),
            container: t.func.container.clone(),
            obj,
            protos: BTreeMap::new(),
            generated: true,
            gcc: None,
        });
        tiers.insert((t.func.container.clone(), t.func.addr), Tier::Lift { input: link_inputs.len() - 1, func: local });
    }

    // Addresses several containers define (overlay slots).
    let mut by_addr: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for (c, a) in tiers.keys() {
        by_addr.entry(*a).or_default().push(c.clone());
    }
    let mut overlays = BTreeMap::new();
    for (a, cs) in by_addr.into_iter().filter(|(_, cs)| cs.len() > 1) {
        let mut cands = Vec::new();
        for c in cs {
            let w0 = inputs.originals.word(&c, a).context("overlay code")?;
            let w1 = inputs.originals.word(&c, a + 4).unwrap_or(0);
            cands.push((c, [w0, w1]));
        }
        overlays.insert(a, cands);
    }

    let spec = GameSpec {
        tiers: tiers.clone(),
        overlays: overlays.clone(),
        budget: opts.budget,
        debug_io: opts.debug_io || !opts.original.is_empty(),
        device_funcs: tiers
            .keys()
            .filter(|(c, a)| {
                inputs.map.sdk_at(c, *a).is_some_and(|p| device_libs.iter().any(|l| p.member.split('/').next() == Some(l.as_str())))
            })
            .cloned()
            .collect(),
        main: inputs.map.main.clone(),
        min_pages: if opts.debug { psxw_abi::window_pages(psxw_abi::DEBUG_WINDOW_BASE) } else { 1024 },
        window_base: if opts.debug { psxw_abi::DEBUG_WINDOW_BASE } else { psxw_abi::WINDOW_BASE },
        debug_info: opts.debug,
        mods: None,
        // Every function a mod can hook, by (container, address): not the
        // ones the translator expands into its callers.
        slots: opts.moddable.then(|| tiers.keys().filter(|k| !expanded.contains(*k)).cloned().collect()),
        probes: inputs
            .manifest
            .run
            .main_loop_symbol
            .iter()
            // libetc's VSync: the boot check counts the game's frame waits
            // (the SDK's own code runs it, D45, so no adapter counts them).
            .chain(std::iter::once(&"VSync".to_string()))
            .chain(&opts.probes)
            .filter_map(|n| {
                let r = inputs.map.resolve(&inputs.map.main, n)?;
                let c = r.container.clone().unwrap_or_else(|| inputs.map.main.clone());
                Some((c, r.addr))
            })
            .collect(),
    };
    Ok(Prepared {
        inputs,
        commit,
        runtime_path,
        adapter_names: adapters.clone(),
        adapters,
        gcc,
        link_inputs,
        tiers,
        translations,
        expanded,
        c_objects,
        unvalidated,
        hardware,
        void_used,
        overlays,
        spec,
    })
}

/// Where a debug build's DWARF puts the C (D53): the decomp's sources
/// under the first, the generated C under the second. The paths are
/// relative, so a debugger resolves them against game.wasm's URL, and the
/// web bundle serves the files there ([`debug_source_roots`]).
pub const DEBUG_SOURCES: [&str; 2] = ["source/game", "source/lift"];

fn debug_lift_flags() -> [String; 2] {
    ["-g".to_string(), format!("-fdebug-compilation-dir={}", DEBUG_SOURCES[1])]
}

/// The directories [`DEBUG_SOURCES`] stand for: the decomp's snapshot and
/// the generated C.
pub fn debug_source_roots(ws: &Workspace, game: &str) -> Result<[(&'static str, PathBuf); 2]> {
    let inputs = psxw_lift::run::load(ws, game)?;
    let snap = psxw_inputs::snapshot::ensure(ws, &inputs.manifest.game.input, ws.game_pin(&inputs.manifest)?)?;
    Ok([(DEBUG_SOURCES[0], snap), (DEBUG_SOURCES[1], psxw_lift::run::out_dir(ws, &inputs))])
}

/// The translations again with DWARF, into `dir` (a debug build): each
/// object is rebuilt when its generated C is newer. Clang runs in
/// `lift_dir` and is given relative paths, which DWARF then has under
/// `source/lift`.
fn debug_lift(clang: &psxw_cc::Clang, lift_dir: &Path, dir: &Path, lifted: &[&psxw_lift::Translation]) -> Result<()> {
    let failed: Vec<String> = lifted
        .par_iter()
        .map(|t| -> Result<Option<String>> {
            let name = format!("{:08X}", t.func.addr);
            let src = lift_dir.join(&t.func.container).join(format!("{name}.c"));
            let obj = dir.join(&t.func.container).join(format!("{name}.o"));
            let fresh = match (std::fs::metadata(&src).and_then(|m| m.modified()), std::fs::metadata(&obj).and_then(|m| m.modified())) {
                (Ok(s), Ok(o)) => o >= s,
                _ => false,
            };
            if fresh {
                return Ok(None);
            }
            std::fs::create_dir_all(obj.parent().context("object directory")?)?;
            let rel = Path::new(&t.func.container).join(format!("{name}.c"));
            let flags = debug_lift_flags();
            let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
            let (ok, stderr) = clang.compile_generated(&rel, &obj, Path::new("."), &flags)?;
            Ok((!ok).then(|| format!("{}: {}", src.display(), stderr.lines().next().unwrap_or_default())))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    ensure!(failed.is_empty(), "translations don't compile with -g:\n  {}", failed.join("\n  "));
    Ok(())
}

/// The link, the merge with the runtime, and the reports.
pub fn finish(ws: &Workspace, opts: &Options, prep: Prepared) -> Result<(Report, PathBuf)> {
    let Prepared {
        inputs,
        commit,
        runtime_path,
        adapters,
        mut link_inputs,
        tiers,
        translations,
        unvalidated,
        hardware,
        void_used,
        overlays,
        mut spec,
        ..
    } = prep;
    // Mods: their objects after the game's.
    if let Some(plan) = &opts.mods {
        let base = link_inputs.len();
        link_inputs.extend(plan.inputs.iter().cloned());
        spec.mods = Some(plan.spec.clone().offset(base));
    }
    let rodata = psxw_link::layout::Rodata::new(&inputs.map, &inputs.originals);
    eprintln!("build: linking {} inputs", link_inputs.len());
    let linked = psxw_link::game::link(&inputs.map, Some(&rodata), &spec, &link_inputs)?;
    if !linked.abi_mismatches.is_empty() {
        eprintln!(
            "build: {} C calls declare their callee differently from its definition; they go through o32 (build.json abi_mismatches)",
            linked.abi_mismatches.len()
        );
    }
    validate(&linked.wasm).context("guest module fails validation")?;

    let dir = opts.mods.as_ref().map_or_else(|| out_dir(ws, &opts.game), |m| m.out.clone());
    std::fs::create_dir_all(&dir)?;
    let suffix = opts.suffix();
    let guest = dir.join(format!("guest{suffix}.wasm"));
    // The adapter table the guest's ids index, so a runtime swapped in
    // unmerged (the browser's dev mode) can be checked against it.
    let mut guest_bytes = linked.wasm.clone();
    {
        use wasm_encoder::Section;
        let data = adapters.join("\n").into_bytes();
        wasm_encoder::CustomSection { name: crate::runtime::ADAPTERS_SECTION.into(), data: data.into() }.append_to(&mut guest_bytes);
    }
    std::fs::write(&guest, &guest_bytes)?;
    let merged = dir.join("merged.wasm");
    let game = dir.join(format!("game{suffix}.wasm"));
    if opts.debug {
        // Binaryen can't carry DWARF through a merge: our own, then the
        // guest's DWARF to where the merge put its code.
        eprintln!("build: merging with {} (debug)", runtime_path.display());
        let m = psxw_link::merge::merge(&guest_bytes, &std::fs::read(&runtime_path)?)?;
        let mut dwarf = psxw_link::dwarf::Dwarf::new();
        dwarf.add_module(&psxw_link::dwarf::sections(&guest_bytes)?, &m.code)?;
        let mut bytes = m.wasm;
        for (name, data) in dwarf.finish()? {
            use wasm_encoder::Section;
            wasm_encoder::CustomSection { name: name.into(), data: data.into() }.append_to(&mut bytes);
        }
        std::fs::write(&game, &bytes)?;
    } else {
        eprintln!("build: merging with {}", runtime_path.display());
        run(Command::new(ws.tool("binaryen", "bin/wasm-merge")?)
            .args(FEATURES)
            .arg(&guest)
            .arg(psxw_abi::guest::MODULE)
            .arg(&runtime_path)
            .arg(psxw_abi::psx::MODULE)
            .arg("-g")
            .arg("-o")
            .arg(&merged))?;
        eprintln!("build: optimizing");
        // `-g` keeps function names, so trap backtraces name guest functions.
        run(Command::new(ws.tool("binaryen", "bin/wasm-opt")?).args(FEATURES).arg("-O2").arg("-g").arg(&merged).arg("-o").arg(&game))?;
    }
    let mut bytes = std::fs::read(&game)?;
    validate(&bytes).context("game.wasm fails validation")?;
    check_abi(&bytes)?;
    // A moddable base names its slots and the game it is, for the loader.
    if let Some(slots) = &spec.slots {
        use wasm_encoder::Section;
        let mut lines = String::new();
        for (c, a) in slots {
            let flavour = match tiers.get(&(c.clone(), *a)) {
                Some(Tier::C { input, func }) => {
                    let o = &link_inputs[*input].obj;
                    let t = &o.types[o.funcs[*func as usize].ty as usize];
                    format!("c {}", psxw_link::mods::sig_text(t))
                }
                _ => "o32".to_string(),
            };
            lines += &format!("{c} 0x{a:08X} {flavour}\n");
        }
        wasm_encoder::CustomSection { name: psxw_link::game::exports::SLOTS_SECTION.into(), data: lines.into_bytes().into() }.append_to(&mut bytes);
        // Script mods (plans/M12) refuse to replace an adapter, and patch
        // an overlay's data once its first code words are resident.
        let tiers_text: String = slots
            .iter()
            .map(|k| match tiers.get(k) {
                Some(Tier::C { .. }) => "c\n",
                Some(Tier::Lift { .. }) => "lift\n",
                Some(Tier::Adapter(_)) => "adapter\n",
                None => "?\n",
            })
            .collect();
        wasm_encoder::CustomSection { name: psxw_link::game::exports::TIERS_SECTION.into(), data: tiers_text.into_bytes().into() }.append_to(&mut bytes);
        let mut resident = String::new();
        for c in inputs.map.containers.iter().filter(|c| !c.is_main) {
            if let Some(at) = c.text.first().map(|r| r.start) {
                let w = |a: u32| inputs.originals.word(&c.id, a).unwrap_or(0);
                resident += &format!("{} 0x{at:08X} 0x{:08X} 0x{:08X}\n", c.id, w(at), w(at + 4));
            }
        }
        wasm_encoder::CustomSection { name: psxw_link::game::exports::RESIDENT_SECTION.into(), data: resident.into_bytes().into() }.append_to(&mut bytes);
        let identity = identity_json(ws, &inputs)?;
        wasm_encoder::CustomSection { name: psxw_link::mods::GAME_SECTION.into(), data: identity.into_bytes().into() }.append_to(&mut bytes);
        std::fs::write(&game, &bytes)?;
        validate(&bytes).context("game.wasm fails validation")?;
    }

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_container: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut bound: BTreeSet<String> = BTreeSet::new();
    for ((c, a), t) in &tiers {
        let k = match t {
            Tier::C { .. } => "c",
            Tier::Lift { .. } => "lift",
            Tier::Adapter(id) => {
                bound.insert(adapters[*id as usize].clone());
                let _ = a;
                "adapter"
            }
        };
        *counts.entry(k.into()).or_default() += 1;
        *by_container.entry(c.clone()).or_default().entry(k.into()).or_default() += 1;
    }
    let report = Report {
        schema_version: SCHEMA_VERSION,
        game: opts.game.clone(),
        commit,
        unvalidated_objects: unvalidated.into_iter().collect(),
        hardware_unbound: hardware.iter().map(|h| h.to_string()).collect(),
        void_results: void_used.iter().map(|(c, a)| format!("{c} {a:#010x}")).collect(),
        abi_mismatches: linked.abi_mismatches.clone(),
        tiers: counts,
        by_container,
        adapters: bound.into_iter().collect(),
        overlay_addresses: overlays.len(),
        aux_symbols: linked.aux.len(),
        matched_rodata: linked.matched,
        debug_io: opts.debug_io || !opts.original.is_empty(),
        guest_bytes: linked.wasm.len(),
        game_bytes: bytes.len(),
        wasm: game.display().to_string(),
        guest: guest.display().to_string(),
    };
    let tier_list: Vec<serde_json::Value> = tiers
        .iter()
        .map(|((c, a), t)| {
            let name = translations
                .iter()
                .find(|x| x.func.container == *c && x.func.addr == *a)
                .map_or_else(String::new, |x| x.func.name.clone());
            serde_json::json!({
                "container": c,
                "addr": format!("0x{a:08X}"),
                "name": name,
                "tier": match t { Tier::C { .. } => "c", Tier::Lift { .. } => "lift", Tier::Adapter(_) => "adapter" },
                "sdk": inputs.map.sdk_at(c, *a).map(|p| p.member.clone()),
            })
        })
        .collect();
    std::fs::write(dir.join(format!("tiers{suffix}.json")), serde_json::to_string_pretty(&tier_list)?)?;
    let json = dir.join(format!("build{suffix}.json"));
    std::fs::write(&json, serde_json::to_string_pretty(&report)?)?;
    let _ = std::fs::remove_file(&merged);
    Ok((report, json))
}

/// The game's identity, as mods record it: its id and its files' hashes.
pub fn identity_json(ws: &Workspace, inputs: &psxw_lift::run::Inputs) -> Result<String> {
    let pin = ws.game_pin(&inputs.manifest)?;
    let containers: BTreeMap<&str, Option<&str>> = inputs.map.containers.iter().map(|c| (c.id.as_str(), c.sha1.as_deref())).collect();
    Ok(serde_json::to_string(&serde_json::json!({
        "game": inputs.manifest.game.id,
        "exe_sha256": pin.disc.as_ref().map(|d| d.exe_sha256.clone()),
        "containers": containers,
    }))?)
}

/// `game.wasm` must export the entry points the host calls and import only
/// the host interface.
fn check_abi(wasm: &[u8]) -> Result<()> {
    let mut exports = BTreeSet::new();
    for p in wasmparser::Parser::new(0).parse_all(wasm) {
        match p? {
            wasmparser::Payload::ImportSection(r) => {
                for i in r.into_imports() {
                    let i = i?;
                    if i.module != psxw_abi::host::MODULE {
                        bail!("game.wasm still imports {}.{}: unresolved by the merge", i.module, i.name);
                    }
                }
            }
            wasmparser::Payload::ExportSection(r) => {
                for e in r {
                    exports.insert(e?.name.to_string());
                }
            }
            _ => {}
        }
    }
    use psxw_abi::exports as x;
    for need in [x::MEMORY, x::ABI_VERSION, x::BOOT, x::ALLOC, x::RAM_HASH, x::EXECUTE] {
        if !exports.contains(need) {
            bail!("game.wasm doesn't export {need}");
        }
    }
    Ok(())
}

/// An SDK function that accesses I/O or kernel RAM itself but runs as
/// guest code in an adapter-policy library.
struct Unbound {
    name: String,
    member: String,
    addr: u32,
    what: &'static str,
    /// Reachable from game code by direct calls that never pass through an
    /// adapter.
    reachable: bool,
}

impl std::fmt::Display for Unbound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let how = if self.reachable { "reachable" } else { "unreachable" };
        write!(f, "{} 0x{:08X} in {} ({}; {how})", self.name, self.addr, self.member, self.what)
    }
}

/// C functions defined `void` whose result a caller may read: C declaring
/// it with a result, translated code reading v0 after calling it, or, for
/// one whose address is taken (callers through function tables can't be
/// seen), returning its last callee's v0. The original still returns that
/// v0, and matching C keeps it, but clang's wasm can't. Such a function runs
/// as its translation, which returns the original's v0.
fn void_results(
    map: &psxw_inputs::AddressMap,
    originals: &psxw_inputs::originals::Originals,
    link_inputs: &[Input],
    c_defs: &BTreeMap<(String, u32), (usize, u32)>,
    translations: &[psxw_lift::Translation],
) -> BTreeSet<(String, u32)> {
    let defined_void = |key: &(String, u32)| {
        c_defs.get(key).is_some_and(|&(i, f)| {
            let o = &link_inputs[i].obj;
            o.types[o.funcs[f as usize].ty as usize].results.is_empty()
        })
    };
    // Callers' uses as (caller's container, callee address).
    let mut uses: Vec<(&str, u32)> = Vec::new();
    for inp in link_inputs {
        for s in inp.obj.symbols.iter().filter(|s| s.kind == SymbolKind::Function && !s.defined) {
            let (Some(idx), NameClass::Guest(n)) = (s.index, classify_name(&s.name)) else { continue };
            if inp.obj.func_type(idx).results.is_empty() {
                continue;
            }
            if let Some(r) = map.resolve(&inp.container, n) {
                uses.push((&inp.container, r.addr));
            }
        }
    }
    for t in translations.iter().filter(|t| !c_defs.contains_key(&(t.func.container.clone(), t.func.addr))) {
        uses.extend(t.facts.result_uses.iter().map(|&a| (t.func.container.as_str(), a)));
    }
    // Function pointers reach C functions too, and nothing says whether
    // their callers read v0. Addresses are taken in the original's data
    // (function tables), as constants in translated code, or by C.
    let mut taken: BTreeSet<u32> = BTreeSet::new();
    for c in &map.containers {
        if let Some(b) = originals.file(&c.id) {
            taken.extend(b.chunks_exact(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])));
        }
    }
    for t in translations {
        taken.extend(&t.facts.code_consts);
    }
    for inp in link_inputs {
        let o = &inp.obj;
        let syms = o.data_relocs.iter().filter(|r| r.kind == psxw_link::object::RelocKind::TableIndexI32).map(|r| r.symbol).chain(
            o.code_relocs
                .iter()
                .filter(|r| matches!(r.ty, wasmparser::RelocationType::TableIndexSleb | wasmparser::RelocationType::TableIndexI32))
                .map(|r| r.symbol),
        );
        for i in syms {
            if let Some(NameClass::Guest(n)) = o.symbols.get(i as usize).map(|s| classify_name(&s.name)) {
                taken.extend(map.resolve(&inp.container, n).map(|r| r.addr));
            }
        }
    }
    // A void C function whose address is taken and whose every exit leaves
    // its last callee's v0 (a dropped `return g();`, which GCC compiles
    // identically) runs translated whoever calls it.
    let mut out: BTreeSet<(String, u32)> = translations
        .iter()
        .filter(|t| t.facts.returns_callee_v0 && taken.contains(&t.func.addr))
        .map(|t| (t.func.container.clone(), t.func.addr))
        .filter(|k| defined_void(k))
        .collect();
    for (from, addr) in uses {
        // The callee's home, as the linker finds it: the caller's own
        // container, the main one, or any overlay holding the address.
        let has = |c: &str| map.container(c).is_some_and(|k| k.text.iter().any(|t| t.contains(&addr)));
        let homes: Vec<String> = if has(from) {
            vec![from.to_string()]
        } else if has(&map.main) {
            vec![map.main.clone()]
        } else {
            map.containers.iter().filter(|c| has(&c.id)).map(|c| c.id.clone()).collect()
        };
        out.extend(homes.into_iter().map(|c| (c, addr)).filter(|k| defined_void(k)));
    }
    out
}

fn hardware_unbound(
    map: &psxw_inputs::AddressMap,
    boundary: &Boundary,
    translations: &[psxw_lift::Translation],
    tiers: &BTreeMap<(String, u32), Tier>,
) -> Vec<Unbound> {
    // Direct calls, from game code, stopping at adapters.
    let by_key: BTreeMap<(&str, u32), &psxw_lift::Translation> =
        translations.iter().map(|t| ((t.func.container.as_str(), t.func.addr), t)).collect();
    let is_adapter = |c: &str, a: u32| matches!(tiers.get(&(c.to_string(), a)), Some(Tier::Adapter(_)));
    let mut seen: BTreeSet<(&str, u32)> = BTreeSet::new();
    let mut work: Vec<(&str, u32)> = translations
        .iter()
        .filter(|t| map.sdk_at(&t.func.container, t.func.addr).is_none())
        .map(|t| (t.func.container.as_str(), t.func.addr))
        .collect();
    while let Some(k) = work.pop() {
        if !seen.insert(k) || is_adapter(k.0, k.1) {
            continue;
        }
        let Some(t) = by_key.get(&k) else { continue };
        for &(_, target, _) in &t.facts.call_sites {
            for c in [k.0, map.main.as_str()] {
                if by_key.contains_key(&(c, target)) {
                    work.push((c, target));
                    break;
                }
            }
        }
    }
    let mut out = Vec::new();
    for t in translations {
        let Some(p) = map.sdk_at(&t.func.container, t.func.addr) else { continue };
        if boundary.policy(&p.member).unwrap_or(Policy::Adapter) != Policy::Adapter || is_adapter(&t.func.container, t.func.addr) {
            continue;
        }
        let what = match (!t.facts.io.is_empty(), !t.facts.kernel.is_empty()) {
            (true, true) => "I/O and kernel RAM",
            (true, false) => "I/O",
            (false, true) => "kernel RAM",
            (false, false) => continue,
        };
        out.push(Unbound {
            name: t.func.name.clone(),
            member: p.member.clone(),
            addr: t.func.addr,
            what,
            reachable: seen.contains(&(t.func.container.as_str(), t.func.addr)),
        });
    }
    out
}

/// The BIOS calls translated code makes (M2 task 8), by table and number,
/// with their call sites; `bios-calls.json`. Each must be one the runtime's
/// dispatcher implements, with a constant function number.
fn bios_calls(dir: &Path, lifted: &[&psxw_lift::Translation]) -> Result<()> {
    #[derive(Serialize, Default)]
    struct Call {
        name: &'static str,
        implemented: bool,
        sites: Vec<String>,
    }
    let mut calls: BTreeMap<String, Call> = BTreeMap::new();
    for t in lifted {
        for c in psxw_lift::bios_calls(t) {
            let site = format!("{} 0x{:08X}", t.func.name, c.pc);
            let (key, name, ok) = match c.func {
                Some(f) => (format!("{:02X}h:{f:02X}h", c.table), psx_bios::bios::name(c.table as u32, f), psx_bios::bios::implemented(c.table as u32, f)),
                None => (format!("{:02X}h:?", c.table), "(function number not constant)", false),
            };
            let e = calls.entry(key).or_insert_with(|| Call { name, implemented: ok, sites: vec![] });
            e.sites.push(site);
        }
    }
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("bios-calls.json"), serde_json::to_string_pretty(&calls)?)?;
    let missing: Vec<String> =
        calls.iter().filter(|(_, c)| !c.implemented).map(|(k, c)| format!("{k} {} at {}", c.name, c.sites.join(", "))).collect();
    ensure!(missing.is_empty(), "translated code makes BIOS calls the runtime doesn't implement:\n  {}", missing.join("\n  "));
    eprintln!("build: {} BIOS calls, all implemented", calls.len());
    Ok(())
}

pub fn game_path(ws: &Workspace, game: &str) -> PathBuf {
    out_dir(ws, game).join("game.wasm")
}

/// The guest address of the manifest's `[oracle] frame_counter`: the clock
/// version 2 replays index by.
pub fn frame_counter(ws: &Workspace, game: &str, inputs: &psxw_lift::run::Inputs) -> Result<u32> {
    let manifest = ws.game(game)?;
    let expr = manifest.oracle.frame_counter.as_deref().with_context(|| format!("games/{game}/game.toml [oracle] frame_counter is unset"))?;
    let r = inputs.map.resolve(&inputs.map.main, expr).with_context(|| format!("[oracle] frame_counter {expr} does not resolve"))?;
    Ok(r.addr)
}
