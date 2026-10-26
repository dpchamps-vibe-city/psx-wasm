//! `psxw build <game>`: the guest module from every container (decompiled
//! C, generated C, adapter bindings), merged with `runtime.wasm` into
//! `out/<game>/game.wasm`.

use anyhow::{Context as _, Result, bail, ensure};
use psxw_cc::protos::Proto;
use psxw_inputs::Workspace;
use psxw_lift::program::Source;
use psxw_link::game::{GameSpec, Tier};
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
}

impl Options {
    pub fn new(game: &str) -> Self {
        Self { game: game.into(), debug_io: false, budget: 1000, ubsan: false }
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
    // Incremental: cheap when the runtime hasn't changed.
    let runtime_path = crate::runtime::build(ws)?;
    let runtime = std::fs::read(&runtime_path)?;
    let adapters = crate::runtime::adapters(&runtime)?;

    let mut inputs = psxw_lift::run::load(ws, &opts.game)?;
    let commit = inputs.commit.clone();
    eprintln!("build: {} @ {}", opts.game, &commit[..7]);
    let (translations, _, _, _, _) = psxw_lift::run::translate(&mut inputs);
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
    let mut link_inputs: Vec<Input> = objs
        .par_iter()
        .map(|(c, o)| -> Result<Input> {
            let obj_path = if opts.ubsan {
                let dir = ubsan_dir.join(c);
                std::fs::create_dir_all(&dir)?;
                let out = dir.join(o.file_name().unwrap_or_default());
                let i = o.with_extension("i");
                let fresh = match (std::fs::metadata(&out), std::fs::metadata(&i)) {
                    (Ok(a), Ok(b)) => a.modified()? >= b.modified()?,
                    _ => false,
                };
                if !fresh {
                    clang.compile_variant(&i, &out, UBSAN_FLAGS)?;
                }
                out
            } else {
                o.clone()
            };
            let protos: BTreeMap<String, Proto> = psxw_cc::protos::cached(&clang, &o.with_extension("i"))?
                .into_iter()
                .fold(BTreeMap::new(), |mut m, p| {
                    if p.defined || !m.contains_key(&p.name) {
                        m.insert(p.name.clone(), p);
                    }
                    m
                });
            Ok(Input {
                label: format!("{c}/{}", o.file_stem().unwrap_or_default().to_string_lossy()),
                container: c.clone(),
                obj: WasmObject::read(&obj_path)?,
                protos,
                generated: false,
                gcc: Some(gcc),
            })
        })
        .collect::<Result<_>>()?;

    // C definitions by (container, address).
    let mut c_defs: BTreeMap<(String, u32), (usize, u32)> = BTreeMap::new();
    for (i, inp) in link_inputs.iter().enumerate() {
        for s in inp.obj.symbols.iter().filter(|s| s.kind == SymbolKind::Function && s.defined) {
            let NameClass::Guest(n) = classify_name(&s.name) else { continue };
            let Some(r) = inputs.map.resolve(&inp.container, n) else { continue };
            let Some(idx) = s.index else { continue };
            c_defs.entry((inp.container.clone(), r.addr)).or_insert((i, idx - inp.obj.num_imported_funcs()));
        }
    }

    // Tiers.
    let pinned: BTreeSet<&str> =
        inputs.manifest.tier.iter().filter(|(_, p)| p.tier == "lift").map(|(n, _)| n.as_str()).collect();
    let adapter_id: BTreeMap<&str, u32> = adapters.iter().enumerate().map(|(i, n)| (n.as_str(), i as u32)).collect();
    let mut tiers: BTreeMap<(String, u32), Tier> = BTreeMap::new();
    let mut lifted = Vec::new();
    for t in &translations {
        let key = (t.func.container.clone(), t.func.addr);
        let in_sdk = inputs.map.sdk_at(&t.func.container, t.func.addr).is_some();
        let tier = if let (true, Some(&id)) = (in_sdk, adapter_id.get(t.func.name.as_str())) {
            Tier::Adapter(id)
        } else if let (Some(&(input, func)), false) = (c_defs.get(&key), pinned.contains(t.func.name.as_str())) {
            debug_assert!(t.func.sources.contains(&Source::Defined));
            Tier::C { input, func }
        } else {
            lifted.push(t);
            continue;
        };
        tiers.insert(key, tier);
    }
    bios_calls(&out_dir(ws, &opts.game), &lifted)?;
    let failures = psxw_lift::run::write_and_compile(ws, &lift_dir, &lifted)?;
    if !failures.is_empty() {
        bail!("{} translations don't compile; run `psxw lift {}`", failures.len(), opts.game);
    }
    for t in &lifted {
        let path = lift_dir.join(&t.func.container).join(format!("{:08X}.o", t.func.addr));
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
        debug_io: opts.debug_io,
        main: inputs.map.main.clone(),
        min_pages: 1024,
        probes: inputs
            .manifest
            .run
            .main_loop_symbol
            .iter()
            .filter_map(|n| {
                let r = inputs.map.resolve(&inputs.map.main, n)?;
                let c = r.container.clone().unwrap_or_else(|| inputs.map.main.clone());
                Some((c, r.addr))
            })
            .collect(),
    };
    let rodata = psxw_link::layout::Rodata::new(&inputs.map, &inputs.originals);
    eprintln!("build: linking {} inputs", link_inputs.len());
    let linked = psxw_link::game::link(&inputs.map, Some(&rodata), &spec, &link_inputs)?;
    validate(&linked.wasm).context("guest module fails validation")?;

    let dir = out_dir(ws, &opts.game);
    std::fs::create_dir_all(&dir)?;
    let suffix = if opts.ubsan { "-ubsan" } else { "" };
    let guest = dir.join(format!("guest{suffix}.wasm"));
    std::fs::write(&guest, &linked.wasm)?;
    let merged = dir.join("merged.wasm");
    let game = dir.join(format!("game{suffix}.wasm"));
    eprintln!("build: merging with {}", runtime_path.display());
    run(Command::new(ws.tool("binaryen", "bin/wasm-merge")?)
        .args(FEATURES)
        .arg(&guest)
        .arg(psxw_abi::guest::MODULE)
        .arg(&runtime_path)
        .arg(psxw_abi::psx::MODULE)
        .arg("-o")
        .arg(&merged))?;
    eprintln!("build: optimizing");
    run(Command::new(ws.tool("binaryen", "bin/wasm-opt")?).args(FEATURES).arg("-O2").arg(&merged).arg("-o").arg(&game))?;
    let bytes = std::fs::read(&game)?;
    validate(&bytes).context("game.wasm fails validation")?;
    check_abi(&bytes)?;

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
        tiers: counts,
        by_container,
        adapters: bound.into_iter().collect(),
        overlay_addresses: overlays.len(),
        aux_symbols: linked.aux.len(),
        matched_rodata: linked.matched,
        debug_io: opts.debug_io,
        guest_bytes: linked.wasm.len(),
        game_bytes: bytes.len(),
        wasm: game.display().to_string(),
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
    for need in [x::MEMORY, x::ABI_VERSION, x::BOOT, x::ALLOC, x::RAM_HASH] {
        if !exports.contains(need) {
            bail!("game.wasm doesn't export {need}");
        }
    }
    Ok(())
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
