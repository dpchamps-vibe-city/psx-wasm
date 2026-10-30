//! `psxw lift <game>`: find every function of every container, translate
//! it to C, compile the C, and report (M1 exit criterion 1).
//!
//! Function starts are seeded from the inputs (symbol files, object
//! boundaries, the EXE entry) and from the C the census compiled (every
//! defined function and lift marker, resolved through the address map).

use crate::program::{Program, Source};
use crate::{FunctionReport, Translation};
use anyhow::{Context as _, Result, bail};
use psxw_cc::Clang;
use psxw_inputs::{AddressMap, GameManifest, Workspace, originals::Originals, psxexe, snapshot};
use psxw_link::object::{NameClass, SymbolKind, WasmObject, classify_name};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;

pub struct Options {
    pub game: String,
    /// Only write and compile this function (name or address).
    pub function: Option<String>,
    /// Only write and compile this container.
    pub container: Option<String>,
}

/// The inputs every lift-tier consumer needs, loaded once.
pub struct Inputs {
    pub manifest: GameManifest,
    pub commit: String,
    pub map: AddressMap,
    pub originals: Originals,
    pub program: Program,
    /// The main EXE's header.
    pub exe: psxexe::PsxExe,
    /// `out/census/<game>/<sha7>/obj`.
    pub census_obj: PathBuf,
}

pub fn load(ws: &Workspace, game: &str) -> Result<Inputs> {
    let manifest = ws.game(game)?;
    let pin = ws.game_pin(&manifest)?.clone();
    let snap = snapshot::ensure(ws, &manifest.game.input, &pin)?;
    let map = AddressMap::load(&snap, &manifest)?;
    let originals = Originals::load(&ws.checkout(&pin), &map)?;
    if !originals.missing.is_empty() {
        bail!(
            "no original bytes for {} (each splat config's target_path must exist in {})",
            originals.missing.join(", "),
            ws.checkout(&pin).display()
        );
    }
    let mut program = Program::from_map(&map);
    let exe_bytes = originals.file(&map.main).context("main container has no original file")?;
    let exe = psxexe::parse(exe_bytes).context("main container")?;
    program.add(&map.main, exe.pc0, None, Source::Entry, true);

    let census_obj = ws.out_dir().join("census").join(game).join(pin.short()).join("obj");
    if !census_obj.is_dir() {
        bail!("{} missing; run `psxw census {game}` first", census_obj.display());
    }
    for (container, name, marker) in defined_functions(&census_obj, &map)? {
        if let Some(r) = map.resolve(&container, &name) {
            let source = if marker { Source::Marker } else { Source::Defined };
            program.add(&container, r.addr, Some(&name), source, true);
        }
    }
    Ok(Inputs { manifest, commit: pin.commit.clone(), map, originals, program, exe, census_obj })
}

/// Every function the census objects define, plus every lift marker's
/// function, as (container, name, is a lift marker).
pub fn defined_functions(census_obj: &Path, map: &AddressMap) -> Result<BTreeSet<(String, String, bool)>> {
    let mut out = BTreeSet::new();
    for c in &map.containers {
        let dir = census_obj.join(&c.id);
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut paths: Vec<PathBuf> =
            entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == "o")).collect();
        paths.sort();
        let objs: Vec<WasmObject> = paths.par_iter().map(|p| WasmObject::read(p)).collect::<Result<_>>()?;
        for obj in objs {
            for s in obj.symbols.iter().filter(|s| s.defined) {
                match s.kind {
                    SymbolKind::Function => {
                        if let NameClass::Guest(n) = classify_name(&s.name) {
                            out.insert((c.id.clone(), n.to_string(), false));
                        }
                    }
                    SymbolKind::Data => {
                        if let Some(n) = s.name.strip_prefix(psxw_cc::LIFT_MARKER_PREFIX) {
                            out.insert((c.id.clone(), n.to_string(), true));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ContainerSummary {
    pub functions: usize,
    pub translated: usize,
    pub failed: usize,
    pub insns: usize,
    pub jump_tables: usize,
    pub indirect_tail_calls: usize,
    pub indirect_calls: usize,
    pub bios_calls: usize,
    pub io_sites: usize,
    pub kernel_sites: usize,
    pub falls_into_next: usize,
    pub discovered_starts: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompileFailure {
    pub function: String,
    pub source: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub pass: bool,
    pub failures: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub game: String,
    pub commit: String,
    pub rounds: u32,
    /// Constants the code writes to `gp`.
    pub gp: Vec<String>,
    /// Functions expanded at their call sites, and why.
    pub inline: BTreeMap<String, String>,
    pub containers: BTreeMap<String, ContainerSummary>,
    /// I/O accesses in functions outside any SDK object.
    pub io_outside_sdk: Vec<String>,
    pub compile_failures: Vec<CompileFailure>,
    pub functions: Vec<FunctionReport>,
    pub verdict: Verdict,
}

/// Translate everything; returns the translations alongside the report.
pub fn translate(
    inputs: &mut Inputs,
) -> (Vec<Translation>, Vec<FunctionReport>, u32, BTreeSet<u32>, BTreeMap<String, String>) {
    let containers: Vec<String> = inputs.map.containers.iter().map(|c| c.id.clone()).collect();
    let outcome = crate::translate_all(&mut inputs.program, &inputs.originals, &containers);
    let member = |c: &str, a: u32| inputs.map.sdk_at(c, a).map(|p| p.member.clone());
    let mut reports = Vec::new();
    let mut gp = BTreeSet::new();
    for t in &outcome.ok {
        gp.extend(t.facts.gp_writes.iter().copied());
        reports.push(FunctionReport::new(&t.func, member(&t.func.container, t.func.addr), Some(t), None));
    }
    for (f, e) in &outcome.failed {
        reports.push(FunctionReport::new(f, member(&f.container, f.addr), None, Some(e)));
    }
    reports.sort_by(|a, b| (&a.container, &a.addr).cmp(&(&b.container, &b.addr)));
    let mut ok = outcome.ok;
    ok.sort_by(|a, b| (&a.func.container, a.func.addr).cmp(&(&b.func.container, b.func.addr)));
    let inline = outcome.inline_reasons.iter().map(|((c, a), why)| (format!("{c} 0x{a:08X}"), why.clone())).collect();
    (ok, reports, outcome.rounds, gp, inline)
}

/// Where generated C and objects go.
pub fn out_dir(ws: &Workspace, inputs: &Inputs) -> PathBuf {
    ws.out_dir().join("lift").join(&inputs.manifest.game.id).join(&inputs.commit[..7])
}

/// Where the generated C's DWARF puts it (D53). Relative, so a debugger
/// resolves it against game.wasm's URL.
pub const DEBUG_SOURCE: &str = "source/lift";

/// Write each translation's C file and compile it; returns failures. The
/// objects carry DWARF under [`DEBUG_SOURCE`]: release and debug builds
/// link the same ones, and only a debug build keeps the DWARF.
pub fn write_and_compile(ws: &Workspace, dir: &Path, translations: &[&Translation]) -> Result<Vec<CompileFailure>> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(crate::RT_HEADER_NAME), crate::RT_HEADER)?;
    let compilation_dir = format!("-fdebug-compilation-dir={DEBUG_SOURCE}");
    let flags = ["-g", compilation_dir.as_str()];
    psxw_cc::objects_compiled_with(dir, &flags)?;
    let clang = Clang::new(ws.clang()?, dir.to_path_buf(), Vec::new(), Vec::new(), dir)?;
    let failures: Vec<Option<CompileFailure>> = translations
        .par_iter()
        .map(|t| -> Result<Option<CompileFailure>> {
            let cdir = dir.join(&t.func.container);
            std::fs::create_dir_all(&cdir)?;
            let src = cdir.join(format!("{:08X}.c", t.func.addr));
            let obj = cdir.join(format!("{:08X}.o", t.func.addr));
            let mut text = format!("#include \"{}\"\n\n", crate::RT_HEADER_NAME);
            for e in &t.externs {
                text.push_str(&format!("uint64_t {e}(uint32_t, uint32_t, uint32_t, uint32_t);\n"));
            }
            text.push('\n');
            text.push_str(&t.c);
            // Only rewrite when changed, so objects stay fresh.
            let fresh = std::fs::read_to_string(&src).is_ok_and(|old| old == text) && obj.is_file();
            if fresh {
                return Ok(None);
            }
            std::fs::write(&src, &text)?;
            // Clang runs in `dir`; the relative path is what DWARF records.
            let rel = Path::new(&t.func.container).join(format!("{:08X}.c", t.func.addr));
            let (ok, stderr) = clang.compile_generated(&rel, &obj, Path::new("."), &flags)?;
            Ok((!ok).then(|| CompileFailure {
                function: format!("{} 0x{:08X} {}", t.func.container, t.func.addr, t.func.name),
                source: src.display().to_string(),
                stderr: stderr.lines().take(20).collect::<Vec<_>>().join("\n"),
            }))
        })
        .collect::<Result<_>>()?;
    Ok(failures.into_iter().flatten().collect())
}

pub fn run(ws: &Workspace, opts: &Options) -> Result<(Report, PathBuf)> {
    let mut inputs = load(ws, &opts.game)?;
    eprintln!("lift: {} @ {}", inputs.manifest.game.id, &inputs.commit[..7]);
    let seeded: BTreeMap<String, usize> =
        inputs.program.containers.iter().map(|(id, c)| (id.clone(), c.starts.len())).collect();
    let (translations, functions, rounds, gp, inline) = translate(&mut inputs);

    let wanted = |t: &Translation| {
        opts.container.as_deref().is_none_or(|c| t.func.container == c)
            && opts.function.as_deref().is_none_or(|f| {
                t.func.name == f || psxw_inputs::parse_u32(f).is_some_and(|a| a == t.func.addr)
            })
    };
    let selected: Vec<&Translation> = translations.iter().filter(|t| wanted(t)).collect();
    if selected.is_empty() && (opts.function.is_some() || opts.container.is_some()) {
        bail!("no translated function matches the selection");
    }
    let dir = out_dir(ws, &inputs);
    eprintln!("lift: compiling {} functions", selected.len());
    let compile_failures = write_and_compile(ws, &dir, &selected)?;

    let mut containers: BTreeMap<String, ContainerSummary> = BTreeMap::new();
    let mut io_outside_sdk = Vec::new();
    for f in &functions {
        let s = containers.entry(f.container.clone()).or_default();
        s.functions += 1;
        if f.error.is_some() {
            s.failed += 1;
            continue;
        }
        s.translated += 1;
        s.insns += f.insns;
        s.jump_tables += f.jump_tables.len();
        s.indirect_tail_calls += f.indirect_tail_calls.len();
        s.indirect_calls += f.indirect_calls.len();
        s.bios_calls += f.bios_calls.len();
        s.io_sites += f.io.len();
        s.falls_into_next += usize::from(f.falls_into.is_some());
        s.kernel_sites += f.kernel.len();
        if (!f.io.is_empty() || !f.kernel.is_empty()) && f.sdk_member.is_none() {
            io_outside_sdk.push(format!("{} {} {}", f.container, f.addr, f.name));
        }
    }
    for (id, s) in containers.iter_mut() {
        s.discovered_starts = inputs.program.containers[id].starts.len() - seeded[id];
    }

    let mut failures = Vec::new();
    for f in functions.iter().filter(|f| f.error.is_some()) {
        failures.push(format!("{} {} {}: {}", f.container, f.addr, f.name, f.error.as_deref().unwrap_or("")));
    }
    for c in &compile_failures {
        failures.push(format!("{}: C does not compile", c.function));
    }
    for s in &io_outside_sdk {
        failures.push(format!("{s}: I/O or kernel access outside an SDK object"));
    }
    let report = Report {
        schema_version: SCHEMA_VERSION,
        game: inputs.manifest.game.id.clone(),
        commit: inputs.commit.clone(),
        rounds,
        gp: gp.iter().map(|g| format!("0x{g:08X}")).collect(),
        inline,
        containers,
        io_outside_sdk,
        compile_failures,
        functions,
        verdict: Verdict { pass: failures.is_empty(), failures },
    };
    let json = dir.join("lift.json");
    std::fs::write(&json, serde_json::to_string_pretty(&report)?)?;
    Ok((report, json))
}

impl Report {
    pub fn summary(&self) -> String {
        use std::fmt::Write;
        let mut s = String::new();
        let _ = writeln!(s, "lift {} @ {} ({} discovery rounds)", self.game, &self.commit[..7], self.rounds);
        let _ = writeln!(
            s,
            "{:<10} {:>6} {:>6} {:>6} {:>8} {:>6} {:>8} {:>8} {:>6} {:>4} {:>6} {:>5}",
            "container", "funcs", "ok", "failed", "insns", "tables", "ind-tail", "ind-call", "bios", "io", "fall", "disc"
        );
        let mut total = ContainerSummary::default();
        for (id, c) in &self.containers {
            let _ = writeln!(
                s,
                "{:<10} {:>6} {:>6} {:>6} {:>8} {:>6} {:>8} {:>8} {:>6} {:>4} {:>6} {:>5}",
                id,
                c.functions,
                c.translated,
                c.failed,
                c.insns,
                c.jump_tables,
                c.indirect_tail_calls,
                c.indirect_calls,
                c.bios_calls,
                c.io_sites,
                c.falls_into_next,
                c.discovered_starts
            );
            total.functions += c.functions;
            total.translated += c.translated;
            total.failed += c.failed;
            total.insns += c.insns;
            total.jump_tables += c.jump_tables;
            total.indirect_tail_calls += c.indirect_tail_calls;
            total.indirect_calls += c.indirect_calls;
            total.bios_calls += c.bios_calls;
            total.io_sites += c.io_sites;
            total.falls_into_next += c.falls_into_next;
            total.discovered_starts += c.discovered_starts;
        }
        let _ = writeln!(
            s,
            "{:<10} {:>6} {:>6} {:>6} {:>8} {:>6} {:>8} {:>8} {:>6} {:>4} {:>6} {:>5}",
            "total",
            total.functions,
            total.translated,
            total.failed,
            total.insns,
            total.jump_tables,
            total.indirect_tail_calls,
            total.indirect_calls,
            total.bios_calls,
            total.io_sites,
            total.falls_into_next,
            total.discovered_starts
        );
        let _ = writeln!(s, "gp values written: {}", self.gp.join(", "));
        for (what, pick) in [
            ("I/O", (|f: &FunctionReport| !f.io.is_empty()) as fn(&FunctionReport) -> bool),
            ("kernel", |f: &FunctionReport| !f.kernel.is_empty()),
        ] {
            let fs: Vec<&FunctionReport> = self.functions.iter().filter(|f| pick(f)).collect();
            let members: BTreeSet<&str> = fs.iter().filter_map(|f| f.sdk_member.as_deref()).collect();
            let _ = writeln!(
                s,
                "{what} access: {} functions, in SDK objects {}",
                fs.len(),
                members.into_iter().collect::<Vec<_>>().join(" ")
            );
        }
        if self.verdict.pass {
            let _ = writeln!(s, "verdict: PASS");
        } else {
            let _ = writeln!(s, "verdict: FAIL ({} problems)", self.verdict.failures.len());
            for f in self.verdict.failures.iter().take(40) {
                let _ = writeln!(s, "  {f}");
            }
        }
        s
    }
}
