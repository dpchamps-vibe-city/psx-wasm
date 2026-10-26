//! Function-level differential harness (M1, docs/design.md "Oracles from
//! the original code"). For every function that has both decompiled C and
//! original code, the C (F_c) and the translation of the original (F_t)
//! run from the same memory with the same arguments; every call they make
//! is logged and answered identically. Return values, memory and the
//! call-out logs must agree.
//!
//! The decomp is byte-matching, so its C and the original MIPS define the
//! same behaviour: a difference is a translator bug, clang UB in the C, or
//! a nondeterminism the original has too.

pub mod runner;

use anyhow::{Context as _, Result, bail};
use psxw_cc::protos::{Kind, Proto};
use psxw_inputs::{AddressMap, Workspace};
use psxw_lift::program::Source;
use psxw_link::object::{NameClass, SymbolKind, WasmObject, classify_name};
use psxw_link::{ExportSpec, Input, Mode};
use rayon::prelude::*;
use runner::{Harness, Outcome, Run};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

pub const SCHEMA_VERSION: u32 = 1;
const PAIRS_PER_SHARD: usize = 48;
const FUEL: u64 = 20_000_000;
/// The C side compiles each function as if alone: no automatic inlining,
/// and no tail-call or tail-recursion rewriting of the calls it makes.
const HARNESS_FLAGS: &[&str] = &["-fno-inline-functions", "-fno-optimize-sibling-calls"];

pub struct Options {
    pub game: String,
    /// Only pairs whose function name or address contains this.
    pub only: Option<String>,
    pub seeds: u32,
    /// Keep the linked harness modules.
    pub keep: bool,
    /// Print the first failing seed's inputs, logs and memory differences.
    pub verbose: bool,
}

#[derive(Debug, Clone)]
struct Pair {
    container: String,
    addr: u32,
    name: String,
    object: PathBuf,
    lifted: PathBuf,
    ret: Kind,
    /// Declared parameters: callers extend char/short arguments (o32).
    params: Vec<Kind>,
    /// Data addresses the C references (argument candidates).
    refs: Vec<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PairReport {
    pub container: String,
    pub addr: String,
    pub name: String,
    /// pass | mismatch | trap | fuel | link-error
    pub verdict: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// Seeds where both sides ran out of fuel.
    #[serde(skip_serializing_if = "is_zero")]
    pub both_fuel: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub game: String,
    pub commit: String,
    pub seeds: u32,
    pub base_seed: u64,
    pub counts: BTreeMap<String, usize>,
    pub shards: usize,
    pub wasm_opt: String,
    pub pairs: Vec<PairReport>,
}

impl Report {
    pub fn summary(&self) -> String {
        use std::fmt::Write;
        let mut s = String::new();
        let _ = writeln!(s, "difftest {} @ {}: {} seeds, {} shards", self.game, &self.commit[..7], self.seeds, self.shards);
        for (k, n) in &self.counts {
            let _ = writeln!(s, "  {k:<11} {n}");
        }
        let bad: Vec<&PairReport> =
            self.pairs.iter().filter(|p| !matches!(p.verdict.as_str(), "pass" | "pinned" | "excluded")).collect();
        for p in bad.iter().take(40) {
            let _ = writeln!(
                s,
                "  {} {} {} {}: {}",
                p.verdict,
                p.container,
                p.addr,
                p.name,
                p.detail.as_deref().unwrap_or("")
            );
        }
        if bad.len() > 40 {
            let _ = writeln!(s, "  ... {} more in the report", bad.len() - 40);
        }
        s
    }

    /// Every pair passes or carries a recorded disposition.
    pub fn pass(&self) -> bool {
        self.pairs.iter().all(|p| matches!(p.verdict.as_str(), "pass" | "pinned" | "excluded"))
    }
}

fn bits_of(k: &Kind) -> (u32, bool) {
    match k {
        Kind::I8 { signed } => (8, *signed),
        Kind::I16 { signed } => (16, *signed),
        Kind::I64 | Kind::F64 => (0, false),
        Kind::Ptr => (1, false),
        _ => (32, false),
    }
}

pub fn run(ws: &Workspace, opts: &Options) -> Result<(Report, PathBuf)> {
    let mut inputs = psxw_lift::run::load(ws, &opts.game)?;
    let commit = inputs.commit.clone();
    eprintln!("difftest: {} @ {}", opts.game, &commit[..7]);
    let (translations, _, _, _, _) = psxw_lift::run::translate(&mut inputs);
    let lift_dir = psxw_lift::run::out_dir(ws, &inputs);
    let all: Vec<&psxw_lift::Translation> = translations.iter().collect();
    let failures = psxw_lift::run::write_and_compile(ws, &lift_dir, &all)?;
    if !failures.is_empty() {
        bail!("{} translations don't compile; run `psxw lift {}`", failures.len(), opts.game);
    }
    let gcc = psxw_link::Gcc {
        division: division_convention(&translations),
        builtin_memcpy: !inputs.manifest.reference.cc1flags.iter().any(|f| f == "-fno-builtin"),
        main_calls_main: true,
    };
    let translated: BTreeSet<(String, u32)> = translations
        .iter()
        .filter(|t| t.func.sources.contains(&Source::Defined))
        .map(|t| (t.func.container.clone(), t.func.addr))
        .collect();

    // Prototype facts for every census TU.
    let out = ws.out_dir().join("difftest").join(&opts.game).join(&commit[..7]);
    std::fs::create_dir_all(&out)?;
    let clang = psxw_cc::Clang::new(ws.clang()?, out.clone(), Vec::new(), Vec::new(), &out)?;
    let mut tus: Vec<(String, PathBuf)> = Vec::new();
    for c in &inputs.map.containers {
        let dir = inputs.census_obj.join(&c.id);
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut v: Vec<PathBuf> =
            rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == "o")).collect();
        v.sort();
        tus.extend(v.into_iter().map(|p| (c.id.clone(), p)));
    }
    let protos: Vec<Vec<Proto>> = tus
        .par_iter()
        .map(|(_, o)| psxw_cc::protos::cached(&clang, &o.with_extension("i")))
        .collect::<Result<_>>()?;
    // The harness compares function by function, so the C side must not
    // inline its callees (functions marked `inline` still are, as GCC
    // 2.95 did at -O2).
    let hobj = out.join("obj");
    let tus: Vec<(String, PathBuf)> = tus
        .par_iter()
        .map(|(c, o)| -> Result<(String, PathBuf)> {
            let dir = hobj.join(c);
            std::fs::create_dir_all(&dir)?;
            let i = o.with_extension("i");
            let h = dir.join(o.file_name().unwrap_or_default());
            let variant = h.with_extension("i");
            let text = format!(
                "/* psxw harness variant: {HARNESS_FLAGS:?} */\n{}",
                psxw_cc::ast::harness_variant(&std::fs::read_to_string(&i)?)
            );
            let same = std::fs::read_to_string(&variant).is_ok_and(|old| old == text);
            if !same || !h.is_file() {
                std::fs::write(&variant, &text)?;
                clang.compile_variant(&variant, &h, HARNESS_FLAGS)?;
            }
            Ok((c.clone(), h))
        })
        .collect::<Result<_>>()?;

    // Return kinds of every function by (container, address), definitions
    // preferred over declarations.
    let mut ret_ext: BTreeMap<(String, u32), (bool, (u32, bool))> = BTreeMap::new();
    let mut arg_masks: BTreeMap<(String, u32), (bool, Vec<u32>)> = BTreeMap::new();
    for ((c, _), ps) in tus.iter().zip(&protos) {
        for p in ps {
            let Some(r) = inputs.map.resolve(c, &p.name) else { continue };
            let Some(psxw_lift::program::Located::Code(home)) = inputs.program.locate(c, r.addr) else { continue };
            let e = ret_ext.entry((home.clone(), r.addr)).or_insert((p.defined, bits_of(&p.ret)));
            if p.defined && !e.0 {
                *e = (true, bits_of(&p.ret));
            }
            let m = arg_masks.entry((home, r.addr)).or_insert((p.defined, word_masks(p)));
            if p.defined && !m.0 {
                *m = (true, word_masks(p));
            }
        }
    }

    // Pairs.
    let mut pairs: Vec<Pair> = Vec::new();
    for ((c, o), ps) in tus.iter().zip(&protos) {
        let obj = WasmObject::read(o)?;
        let by_name: BTreeMap<&str, &Proto> = ps.iter().filter(|p| p.defined).map(|p| (p.name.as_str(), p)).collect();
        for s in obj.symbols.iter().filter(|s| s.kind == SymbolKind::Function && s.defined) {
            let NameClass::Guest(n) = classify_name(&s.name) else { continue };
            let Some(r) = inputs.map.resolve(c, n) else { continue };
            if !translated.contains(&(c.clone(), r.addr)) {
                continue;
            }
            let label = format!("{n} {:08X}", r.addr);
            if opts.only.as_deref().is_some_and(|f| !label.contains(f)) {
                continue;
            }
            let refs = data_refs(&obj, s.index.unwrap_or(0), c, &inputs.map);
            pairs.push(Pair {
                container: c.clone(),
                addr: r.addr,
                name: n.to_string(),
                object: o.clone(),
                lifted: lift_dir.join(c).join(format!("{:08X}.o", r.addr)),
                ret: by_name.get(n).map_or(Kind::I32, |p| p.ret),
                params: by_name.get(n).map_or_else(Vec::new, |p| p.params.clone()),
                refs,
            });
        }
    }
    // A function can carry two names (clang's `__original_main` and `main`).
    let mut seen = BTreeSet::new();
    pairs.retain(|p| seen.insert((p.container.clone(), p.addr)));
    // Functions pinned to the lift tier don't use their C; excluded ones
    // carry a recorded reason.
    let mut pinned_reports = Vec::new();
    pairs.retain(|p| {
        let pin = inputs.manifest.tier.get(&p.name).filter(|t| t.tier == "lift").map(|t| ("pinned", t.reason.clone()));
        let excl = inputs.manifest.difftest.exclude.get(&p.name).map(|r| ("excluded", r.clone()));
        match pin.or(excl) {
            Some((verdict, reason)) => {
                pinned_reports.push(PairReport {
                    container: p.container.clone(),
                    addr: format!("0x{:08X}", p.addr),
                    name: p.name.clone(),
                    verdict: verdict.into(),
                    detail: Some(reason),
                    seed: None,
                    both_fuel: 0,
                });
                false
            }
            None => true,
        }
    });
    if pairs.is_empty() {
        bail!("no function has both C and a translation");
    }
    eprintln!("difftest: {} pairs", pairs.len());

    // gp as the EXE entry function sets it.
    let gp = translations
        .iter()
        .find(|t| t.func.container == inputs.map.main && t.func.addr == inputs.exe.pc0)
        .and_then(|t| t.facts.gp_writes.iter().next().copied())
        .unwrap_or(0);

    // Original images per container: the EXE always, plus the overlay.
    let exe_file = inputs.originals.file(&inputs.map.main).context("main image")?;
    let exe_text = inputs.exe.text(exe_file).to_vec();
    let image_of = |c: &str| -> Vec<(u32, Vec<u8>)> {
        let mut v = vec![(inputs.exe.t_addr, exe_text.clone())];
        if c != inputs.map.main {
            if let (Some(file), Some(cont)) = (inputs.originals.file(c), inputs.map.container(c)) {
                for (range, vram) in &cont.segments {
                    v.push((*vram, file[range.start as usize..range.end as usize].to_vec()));
                }
            }
        }
        v
    };

    // Shards: whole objects of one container, up to PAIRS_PER_SHARD pairs.
    let mut shards: Vec<Vec<Pair>> = Vec::new();
    {
        let mut cur: Vec<Pair> = Vec::new();
        let mut i = 0;
        while i < pairs.len() {
            let obj = pairs[i].object.clone();
            let group: Vec<Pair> = pairs[i..].iter().take_while(|p| p.object == obj).cloned().collect();
            i += group.len();
            let fits = cur.first().is_none_or(|f| f.container == group[0].container)
                && cur.len() + group.len() <= PAIRS_PER_SHARD;
            if !fits && !cur.is_empty() {
                shards.push(std::mem::take(&mut cur));
            }
            cur.extend(group);
        }
        if !cur.is_empty() {
            shards.push(cur);
        }
    }

    let wasm_opt = ws.tool("binaryen", "bin/wasm-opt")?;
    let rodata = psxw_link::layout::Rodata::new(&inputs.map, &inputs.originals);
    let protos_by_obj: BTreeMap<PathBuf, BTreeMap<String, Proto>> = tus
        .iter()
        .zip(&protos)
        .map(|((_, o), ps)| {
            let mut m: BTreeMap<String, Proto> = BTreeMap::new();
            for p in ps {
                if p.defined || !m.contains_key(&p.name) {
                    m.insert(p.name.clone(), p.clone());
                }
            }
            (o.clone(), m)
        })
        .collect();
    let base_seed = 0x5053_5857u64;
    let done = AtomicUsize::new(0);
    let nshards = shards.len();
    let shard_dir = out.join("shards");
    std::fs::create_dir_all(&shard_dir)?;
    let reports: Vec<Vec<PairReport>> = shards
        .par_iter()
        .enumerate()
        .map(|(si, shard)| {
            let r = run_shard(
                si,
                shard,
                &inputs.map,
                &rodata,
                &protos_by_obj,
                &ret_ext,
                &arg_masks,
                gcc,
                &image_of(&shard[0].container),
                gp,
                &wasm_opt,
                &shard_dir,
                opts,
                base_seed,
            );
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 10 == 0 || n == nshards {
                eprintln!("difftest: {n}/{nshards} shards");
            }
            r.unwrap_or_else(|e| {
                shard
                    .iter()
                    .map(|p| PairReport {
                        container: p.container.clone(),
                        addr: format!("0x{:08X}", p.addr),
                        name: p.name.clone(),
                        verdict: "link-error".into(),
                        detail: Some(format!("{e:#}")),
                        seed: None,
                        both_fuel: 0,
                    })
                    .collect()
            })
        })
        .collect();
    let mut pairs: Vec<PairReport> = reports.into_iter().flatten().chain(pinned_reports).collect();
    pairs.sort_by(|a, b| (&a.container, &a.addr).cmp(&(&b.container, &b.addr)));
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for p in &pairs {
        *counts.entry(p.verdict.clone()).or_default() += 1;
    }
    let report = Report {
        schema_version: SCHEMA_VERSION,
        game: opts.game.clone(),
        commit,
        seeds: opts.seeds,
        base_seed,
        counts,
        shards: nshards,
        wasm_opt: wasm_opt.display().to_string(),
        pairs,
    };
    let json = out.join("difftest.json");
    std::fs::write(&json, serde_json::to_string_pretty(&report)?)?;
    Ok((report, json))
}

/// The division checks the game's own C was assembled with, read from the
/// `break` codes in the translations of its C-defined functions.
pub fn division_convention(translations: &[psxw_lift::Translation]) -> psxw_link::Division {
    let mut codes: BTreeMap<u32, usize> = BTreeMap::new();
    for t in translations.iter().filter(|t| t.func.sources.contains(&Source::Defined)) {
        for d in t.graph.nodes.values() {
            if d.insn.op == psxw_lift::decode::Op::Break {
                *codes.entry((d.insn.word >> 6) & 0xF_FFFF).or_default() += 1;
            }
        }
    }
    let n = |c: u32| codes.get(&c).copied().unwrap_or(0);
    if n(7 << 10) > n(7) {
        psxw_link::Division { zero: 7 << 10, overflow: (n(6 << 10) > 0).then_some(6 << 10) }
    } else {
        psxw_link::Division { zero: 7, overflow: (n(6) > 0).then_some(6) }
    }
}

/// Data addresses a function's C body references.
fn data_refs(obj: &WasmObject, func_index: u32, container: &str, map: &AddressMap) -> Vec<u32> {
    let Some(local) = func_index.checked_sub(obj.num_imported_funcs()) else { return Vec::new() };
    let Some(f) = obj.funcs.get(local as usize) else { return Vec::new() };
    let mut out = BTreeSet::new();
    for r in obj.relocs_for(&f.body) {
        use wasmparser::RelocationType as R;
        if matches!(r.ty, R::MemoryAddrLeb | R::MemoryAddrSleb) {
            if let Some(s) = obj.symbols.get(r.symbol as usize) {
                if let Some(a) = map.resolve(container, &s.name) {
                    out.insert(a.addr.wrapping_add(r.addend as u32));
                }
            }
        }
    }
    out.into_iter().collect()
}

#[allow(clippy::too_many_arguments)]
fn run_shard(
    si: usize,
    shard: &[Pair],
    map: &AddressMap,
    rodata: &psxw_link::layout::Rodata,
    protos: &BTreeMap<PathBuf, BTreeMap<String, Proto>>,
    ret_ext: &BTreeMap<(String, u32), (bool, (u32, bool))>,
    arg_masks: &BTreeMap<(String, u32), (bool, Vec<u32>)>,
    gcc: psxw_link::Gcc,
    images: &[(u32, Vec<u8>)],
    gp: u32,
    wasm_opt: &Path,
    dir: &Path,
    opts: &Options,
    base_seed: u64,
) -> Result<Vec<PairReport>> {
    let container = shard[0].container.clone();
    let mut inputs: Vec<Input> = Vec::new();
    let mut obj_index: BTreeMap<PathBuf, usize> = BTreeMap::new();
    let mut exports = Vec::new();
    for p in shard {
        let ci = match obj_index.get(&p.object) {
            Some(&i) => i,
            None => {
                inputs.push(Input {
                    label: p.object.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
                    container: container.clone(),
                    obj: WasmObject::read(&p.object)?,
                    protos: protos.get(&p.object).cloned().unwrap_or_default(),
                    generated: false,
                    gcc: Some(gcc),
                });
                obj_index.insert(p.object.clone(), inputs.len() - 1);
                inputs.len() - 1
            }
        };
        inputs.push(Input {
            label: format!("lift {:08X}", p.addr),
            container: container.clone(),
            obj: WasmObject::read(&p.lifted)?,
            protos: BTreeMap::new(),
            generated: true,
            gcc: None,
        });
        let ti = inputs.len() - 1;
        exports.push(ExportSpec { name: format!("c.{:08X}", p.addr), input: ci, function: p.name.clone(), o32: true });
        exports.push(ExportSpec {
            name: format!("t.{:08X}", p.addr),
            input: ti,
            function: psxw_lift::program::translated_symbol(&map.main, &container, p.addr),
            o32: false,
        });
    }
    let mode = Mode::Harness { sp: runner::STACK_TOP, gp, dirty: Some(runner::DIRTY_MAP) };
    let linked = psxw_link::link(map, Some(rodata), mode, &inputs, &exports)?;
    wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::default())
        .validate_all(&linked.wasm)
        .context("linked harness module fails validation")?;
    let raw = dir.join(format!("shard_{si:04}.wasm"));
    let opt = dir.join(format!("shard_{si:04}.opt.wasm"));
    std::fs::write(&raw, &linked.wasm)?;
    let st = std::process::Command::new(wasm_opt)
        .args(["-O2", "--enable-bulk-memory", "--enable-sign-ext", "--enable-nontrapping-float-to-int"])
        .args(["--enable-mutable-globals", "--enable-multivalue"])
        .arg(&raw)
        .arg("-o")
        .arg(&opt)
        .output()?;
    if !st.status.success() {
        bail!("wasm-opt failed: {}", String::from_utf8_lossy(&st.stderr));
    }
    let wasm = std::fs::read(&opt)?;
    if !opts.keep {
        let _ = std::fs::remove_file(&raw);
        let _ = std::fs::remove_file(&opt);
    }
    let ext: BTreeMap<u32, (u32, bool)> = ret_ext
        .iter()
        .filter(|((c, _), _)| *c == container || *c == map.main)
        .map(|((_, a), (_, e))| (*a, *e))
        .collect();
    let imgs: Vec<(u32, &[u8])> = images.iter().map(|(a, b)| (*a, b.as_slice())).collect();
    let mut h = Harness::new(&wasm, &imgs, ext, FUEL)?;
    let masks: BTreeMap<u32, Vec<u32>> = arg_masks
        .iter()
        .filter(|((c, _), _)| *c == container || *c == map.main)
        .map(|((_, a), (_, m))| (*a, m.clone()))
        .collect();
    let mut out = Vec::new();
    for p in shard {
        out.push(run_pair(&mut h, p, opts.seeds, base_seed, &masks, opts.verbose)?);
    }
    Ok(out)
}

/// Per o32 argument word, the bits a callee's declared parameter covers.
fn word_masks(p: &Proto) -> Vec<u32> {
    let (words, _) = psxw_link::marshal::words(&p.params, false);
    words
        .iter()
        .map(|w| match w {
            psxw_link::marshal::Word::Param(i) => match p.params[*i as usize] {
                Kind::I8 { .. } => 0xFF,
                Kind::I16 { .. } => 0xFFFF,
                _ => u32::MAX,
            },
            psxw_link::marshal::Word::Pad => 0,
            _ => u32::MAX,
        })
        .collect()
}

fn run_pair(
    h: &mut Harness,
    p: &Pair,
    seeds: u32,
    base_seed: u64,
    masks: &BTreeMap<u32, Vec<u32>>,
    verbose: bool,
) -> Result<PairReport> {
    let fc = h.func(&format!("c.{:08X}", p.addr))?;
    let ft = h.func(&format!("t.{:08X}", p.addr))?;
    let mut report = PairReport {
        container: p.container.clone(),
        addr: format!("0x{:08X}", p.addr),
        name: p.name.clone(),
        verdict: "pass".into(),
        detail: None,
        seed: None,
        both_fuel: 0,
    };
    for k in 0..seeds {
        let seed = runner::mix(base_seed ^ ((p.addr as u64) << 16) ^ k as u64);
        let (mut args, scratch, mut stack) = gen_inputs(seed, &p.refs);
        // The o32 caller extends narrow arguments to their declared type,
        // and passes real pointers (or NULL) for pointer parameters.
        let (words, _) = psxw_link::marshal::words(&p.params, false);
        for (k, w) in words.iter().enumerate().take(12) {
            let psxw_link::marshal::Word::Param(i) = w else { continue };
            let slot = if k < 4 { &mut args[k] } else { &mut stack[k - 4] };
            *slot = match p.params[*i as usize] {
                Kind::I8 { signed: true } => *slot as u8 as i8 as i32 as u32,
                Kind::I8 { signed: false } => *slot as u8 as u32,
                Kind::I16 { signed: true } => *slot as u16 as i16 as i32 as u32,
                Kind::I16 { signed: false } => *slot as u16 as u32,
                Kind::Ptr | Kind::Sret { .. } => pointer(runner::mix(seed ^ k as u64), &p.refs),
                _ => *slot,
            };
        }
        let c = h.run(&fc, seed, args, &scratch, &stack);
        let c_pages: BTreeMap<usize, Vec<u8>> = c.dirty.iter().map(|&pg| (pg, h.page(pg).to_vec())).collect();
        h.restore(&c.dirty);
        let t = h.run(&ft, seed, args, &scratch, &stack);
        let verdict = compare(h, p, &c, &t, &c_pages, masks);
        let mut all = c.dirty.clone();
        all.extend(t.dirty.iter().copied());
        if verdict.is_err() && verbose {
            explain(h, p, &args, &stack, &c, &t, &c_pages);
        }
        h.restore(&all);
        match verdict {
            Ok(Agree::Same) => {}
            Ok(Agree::BothFuel) => report.both_fuel += 1,
            Err((verdict, detail)) => {
                report.verdict = verdict.into();
                report.detail = Some(detail);
                report.seed = Some(seed);
                break;
            }
        }
    }
    Ok(report)
}

fn explain(h: &Harness, p: &Pair, args: &[u32; 4], stack: &[u32; 8], c: &Run, t: &Run, c_pages: &BTreeMap<usize, Vec<u8>>) {
    eprintln!("=== {} {:08X}: args {args:08X?} stack {stack:08X?}", p.name, p.addr);
    for (side, r) in [("C", c), ("T", t)] {
        eprintln!("  {side}: {:?}", r.outcome);
        for (i, e) in r.log.iter().enumerate().take(24) {
            eprintln!(
                "    {i}: {} {:08X} n={} {:08X?} stack {:08X?} -> {:X}",
                psxw_link::callout::name(e.kind),
                e.target,
                e.nwords,
                e.args,
                &e.stack[..4],
                e.ret
            );
        }
    }
    let mut pages: BTreeSet<usize> = c.dirty.clone();
    pages.extend(t.dirty.iter().copied());
    let mut shown = 0;
    for pg in pages {
        let tb = h.page(pg);
        let cb: &[u8] = c_pages.get(&pg).map_or_else(|| h.snapshot_page(pg), |b| b.as_slice());
        let snap = h.snapshot_page(pg);
        for i in 0..runner::PAGE {
            let (x, y) = (cb.get(i).copied().unwrap_or(0), tb[i]);
            if x != y && shown < 24 {
                let s = snap.get(i).copied().unwrap_or(0);
                eprintln!("  mem 0x{:08X}: C {x:02X} T {y:02X} (was {s:02X})", 0x8000_0000 | (pg * runner::PAGE + i) as u32);
                shown += 1;
            }
        }
    }
}

enum Agree {
    Same,
    BothFuel,
}

fn compare(
    h: &Harness,
    p: &Pair,
    c: &Run,
    t: &Run,
    c_pages: &BTreeMap<usize, Vec<u8>>,
    masks: &BTreeMap<u32, Vec<u32>>,
) -> std::result::Result<Agree, (&'static str, String)> {
    // Pointers into the stack can't be compared: clang and GCC lay out
    // frames differently. Any two stack addresses are equivalent.
    let stack = (runner::STACK_TOP - runner::STACK_EXCLUDE)..(runner::STACK_TOP + 64);
    let word_eq = |a: u32, b: u32, mask: u32| a & mask == b & mask || (stack.contains(&a) && stack.contains(&b));
    let diverges = |o: &Outcome| matches!(o, Outcome::Fuel) || *o == Outcome::Trap("call-out limit".into());
    match (&c.outcome, &t.outcome) {
        // Both sides loop forever on this input (e.g. waiting for a flag
        // only another function sets); compare what they did before.
        (a, b) if diverges(a) && diverges(b) => {
            if c.log.iter().zip(&t.log).take(64).any(|(x, y)| x.kind != y.kind || x.target != y.target) {
                return Err(("mismatch", "both diverge, with different call-outs".into()));
            }
            return Ok(Agree::BothFuel);
        }
        (Outcome::Fuel, _) | (_, Outcome::Fuel) => {
            return Err(("fuel", format!("C {:?} vs translated {:?}", c.outcome, t.outcome)));
        }
        (Outcome::Trap(a), Outcome::Trap(b)) if a == b => {}
        (Outcome::Trap(_), _) | (_, Outcome::Trap(_)) => {
            return Err(("trap", format!("C {:?} vs translated {:?}", c.outcome, t.outcome)));
        }
        (Outcome::Returned(a), Outcome::Returned(b)) => {
            let mask: u64 = match p.ret {
                Kind::Void | Kind::Sret { .. } => 0,
                Kind::I8 { .. } => 0xFF,
                Kind::I16 { .. } => 0xFFFF,
                Kind::I64 | Kind::F64 => u64::MAX,
                _ => 0xFFFF_FFFF,
            };
            if a & mask != b & mask {
                return Err(("mismatch", format!("return C 0x{:X} vs translated 0x{:X}", a & mask, b & mask)));
            }
        }
    }
    // `break` is a diagnostic with no effect on state, and C may legally
    // move, merge or duplicate a trapping division (UB on a zero divisor):
    // breaks are compared as a set of codes, everything else in order.
    let breaks = |log: &[runner::Callout]| {
        log.iter().filter(|e| e.kind == psxw_link::callout::BREAK).map(|e| e.target).collect::<BTreeSet<u32>>()
    };
    if breaks(&c.log) != breaks(&t.log) {
        return Err(("mismatch", format!("breaks C {:X?} vs translated {:X?}", breaks(&c.log), breaks(&t.log))));
    }
    let clog: Vec<runner::Callout> = c.log.iter().filter(|e| e.kind != psxw_link::callout::BREAK).cloned().collect();
    let tlog: Vec<runner::Callout> = t.log.iter().filter(|e| e.kind != psxw_link::callout::BREAK).cloned().collect();
    let (c_log, t_log) = (&clog, &tlog);
    for (i, (a, b)) in c_log.iter().zip(t_log).enumerate() {
        let n = a.nwords.min(4) as usize;
        let ns = a.nwords.saturating_sub(4).min(8) as usize;
        let m = if a.kind == psxw_link::callout::CALL { masks.get(&a.target) } else { None };
        let mask = |k: usize| m.and_then(|m| m.get(k)).copied().unwrap_or(u32::MAX);
        let same = a.kind == b.kind
            && a.target == b.target
            && (0..n).all(|k| word_eq(a.args[k], b.args[k], mask(k)))
            && (0..ns).all(|k| word_eq(a.stack[k], b.stack[k], mask(4 + k)));
        if !same {
            return Err((
                "mismatch",
                format!(
                    "call-out {i}: C {} {:08X} {:08X?} vs translated {} {:08X} {:08X?}",
                    psxw_link::callout::name(a.kind),
                    a.target,
                    &a.args[..n],
                    psxw_link::callout::name(b.kind),
                    b.target,
                    &b.args[..n]
                ),
            ));
        }
    }
    if c_log.len() != t_log.len() {
        let extra = if c_log.len() > t_log.len() { &c_log[t_log.len()] } else { &t_log[c_log.len()] };
        return Err((
            "mismatch",
            format!(
                "call-out count C {} vs translated {} (first extra: {} {:08X})",
                c_log.len(),
                t_log.len(),
                psxw_link::callout::name(extra.kind),
                extra.target
            ),
        ));
    }
    // Memory: every page either side touched.
    let lo = ((runner::STACK_TOP - runner::STACK_EXCLUDE) & 0x00FF_FFFF) as usize;
    let hi = ((runner::STACK_TOP + 64) & 0x00FF_FFFF) as usize;
    let mut pages: BTreeSet<usize> = c.dirty.clone();
    pages.extend(t.dirty.iter().copied());
    for pg in pages {
        let tb = h.page(pg);
        let cb: &[u8] = match c_pages.get(&pg) {
            Some(b) => b,
            None => h.snapshot_page(pg),
        };
        for (i, (x, y)) in cb.iter().zip(tb).enumerate() {
            let a = pg * runner::PAGE + i;
            if x != y && !(lo..hi).contains(&a) {
                return Err((
                    "mismatch",
                    format!("memory at 0x{:08X}: C {x:02X} vs translated {y:02X}", 0x8000_0000 | a as u32),
                ));
            }
        }
    }
    Ok(Agree::Same)
}

/// A plausible pointer: NULL, a data symbol the function uses, or scratch.
fn pointer(r: u64, refs: &[u32]) -> u32 {
    match r % 8 {
        0 => 0,
        1 | 2 | 3 if !refs.is_empty() => refs[(r >> 8) as usize % refs.len()],
        _ => runner::SCRATCH + ((r >> 16) as u32 % (runner::SCRATCH_LEN - 64) & !3),
    }
}

/// Arguments, scratch memory and stack words for one seed.
fn gen_inputs(seed: u64, refs: &[u32]) -> ([u32; 4], Vec<u8>, [u32; 8]) {
    let mut s = seed;
    let mut next = || {
        s = runner::mix(s);
        s
    };
    let scratch_ptr = |r: u64| runner::SCRATCH + ((r as u32) % (runner::SCRATCH_LEN - 64) & !3);
    let word = |next: &mut dyn FnMut() -> u64| -> u32 {
        let r = next();
        match r % 10 {
            0 => 0,
            1 => (r >> 8) as u32 % 16,
            2 => ((r >> 8) as u32 % 4).wrapping_neg(),
            3 | 4 if !refs.is_empty() => refs[(r >> 8) as usize % refs.len()],
            3 | 4 | 5 | 6 => scratch_ptr(r >> 8),
            7 => (r >> 8) as u32 % 256,
            8 => (r >> 8) as u32 % 0x1_0000,
            _ => (r >> 16) as u32,
        }
    };
    let args = [word(&mut next), word(&mut next), word(&mut next), word(&mut next)];
    let stack = [
        word(&mut next),
        word(&mut next),
        word(&mut next),
        word(&mut next),
        word(&mut next),
        word(&mut next),
        word(&mut next),
        word(&mut next),
    ];
    let mut scratch = Vec::with_capacity(runner::SCRATCH_LEN as usize);
    for _ in 0..runner::SCRATCH_LEN / 4 {
        let r = next();
        let w = match r % 4 {
            0 | 1 => (r >> 8) as u32 % 64,
            2 => scratch_ptr(r >> 8),
            _ => (r >> 16) as u32,
        };
        scratch.extend_from_slice(&w.to_le_bytes());
    }
    (args, scratch, stack)
}
