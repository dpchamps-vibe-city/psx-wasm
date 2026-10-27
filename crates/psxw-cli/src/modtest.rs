//! `psxw mod test` (plans/M11 task 10, M12 task 11): every test of every mod
//! in a selection.
//!
//! - **A compiled mod's tests run natively,** on the build with the
//!   selection's compiled mods linked in; with `load_at` or `commands`, on
//!   the moddable base with their modules loaded at run time.
//! - **A script mod's tests run in headless Chromium and Firefox,** on the
//!   moddable base with the selection's mods of both kinds loaded at run
//!   time (`web-test`'s page runs). So does a test whose `same_as` names a
//!   script mod.
//!
//! Both hosts report in the same shapes, so expectations and check
//! commands read them alike; a browser's checks are named after it.

use crate::webtest::{BrowserRun, PageMod, PageRun};
use anyhow::{Context as _, Result, ensure};
use psxw_build::game::{Options, Report};
use psxw_host_native::Stop;
use psxw_inputs::Workspace;
use psxw_modkit::build::Built;
use psxw_modkit::decl::{Mod, Target};
use psxw_modkit::script::AnyMod;
use psxw_modkit::symbols::SymbolTable;
use psxw_modkit::test::{Expect, ReplayRef, Test, ValueType};
use serde::Serialize;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const BROWSERS: &[&str] = &["chromium", "firefox"];

/// How a context (psx-wasm, or a mods workspace) builds what tests run.
pub trait Builds {
    fn ws(&self) -> &Workspace;
    fn game(&self) -> &str;
    /// The game with these compiled mods linked in.
    fn linked(&self, mods: &[Mod]) -> Result<Report>;
    /// The snapshot script mods' `game` comes from (the bundle's when none).
    fn snapshot(&self) -> Option<PathBuf> {
        None
    }
    /// Each mod as a module (M11 phase 2), in order.
    fn modules(&self, mods: &[Mod]) -> Result<Vec<PathBuf>>;
}

/// What every context shares: the moddable base and the plain build, built
/// once per run.
#[derive(Default)]
pub struct Cache {
    /// Tests compare nothing with `same_as` (a base other mods' modules
    /// weren't built for).
    pub skip_same_as: bool,
    base: RefCell<Option<Report>>,
    plain: RefCell<Option<Report>>,
    linked: RefCell<BTreeMap<String, Report>>,
    modules: RefCell<BTreeMap<String, PathBuf>>,
}

impl Cache {
    /// A cache whose moddable base is `base`.
    pub fn with_base(base: Report, skip_same_as: bool) -> Self {
        Cache { skip_same_as, base: RefCell::new(Some(base)), ..Default::default() }
    }

    pub fn base(&self, ws: &Workspace, game: &str) -> Result<Report> {
        if let Some(r) = self.base.borrow().clone() {
            return Ok(r);
        }
        let (r, _) = psxw_build::game::build(ws, &Options { moddable: true, ..Options::new(game) })?;
        *self.base.borrow_mut() = Some(r.clone());
        Ok(r)
    }

    pub fn plain(&self, ws: &Workspace, game: &str) -> Result<Report> {
        if let Some(r) = self.plain.borrow().clone() {
            return Ok(r);
        }
        let (r, _) = psxw_build::game::build(ws, &Options::new(game))?;
        *self.plain.borrow_mut() = Some(r.clone());
        Ok(r)
    }

    /// `build` for a label not built yet.
    pub fn linked(&self, mods: &[Mod], build: &dyn Fn(&[Mod], &str) -> Result<Built>) -> Result<Report> {
        let label = crate::mods::label(mods);
        if let Some(r) = self.linked.borrow().get(&label) {
            return Ok(r.clone());
        }
        let b = build(mods, &label)?;
        println!("game.wasm: {} ({} bytes); mods.json: {}", b.report.wasm, b.report.game_bytes, b.mods_json.display());
        self.linked.borrow_mut().insert(label, b.report.clone());
        Ok(b.report)
    }

    /// `build` for the mods without a module yet.
    pub fn modules(&self, mods: &[Mod], build: &dyn Fn(&[Mod]) -> Result<Vec<(String, PathBuf)>>) -> Result<Vec<PathBuf>> {
        let missing: Vec<Mod> = mods.iter().filter(|m| !self.modules.borrow().contains_key(m.id())).cloned().collect();
        if !missing.is_empty() {
            for (id, p) in build(&missing)? {
                self.modules.borrow_mut().insert(id, p);
            }
        }
        mods.iter().map(|m| self.modules.borrow().get(m.id()).cloned().with_context(|| format!("no module for {}", m.id()))).collect()
    }
}

#[derive(Serialize)]
struct Outcome {
    test: String,
    pass: bool,
    checks: Vec<(String, bool, String)>,
}

/// Run every test of every mod in `mods` (a selection, in order), or only
/// `only`'s.
pub fn run_tests(b: &dyn Builds, cache: &Cache, table: &SymbolTable, mods: &[AnyMod], only: Option<&str>, out: &Path) -> Result<bool> {
    std::fs::create_dir_all(out)?;
    let mut all = Vec::new();
    for m in mods.iter().filter(|m| only.is_none_or(|id| m.id() == id)) {
        for t in psxw_modkit::test::tests(m.dir())? {
            let name = format!("{}/{}", m.id(), t.name);
            eprintln!("test {name}: {} frames{}", t.spec.frames, if m.is_script() { " (in the browsers)" } else { "" });
            let checks = one_test(b, cache, table, mods, m, &t, &out.join(m.id()).join(&t.name)).with_context(|| format!("test {name}"))?;
            for (what, pass, detail) in &checks {
                println!("{name}: {what:<14} {:<4} {detail}", if *pass { "ok" } else { "FAIL" });
            }
            all.push(Outcome { test: name, pass: checks.iter().all(|c| c.1), checks });
        }
    }
    if all.is_empty() {
        println!("no tests (each mod's tests/*.json)");
    }
    std::fs::write(out.join("tests.json"), serde_json::to_string_pretty(&all)?)?;
    let pass = all.iter().all(|o| o.pass);
    println!("tests: {} ({} of {} pass; {})", if pass { "PASS" } else { "FAIL" }, all.iter().filter(|o| o.pass).count(), all.len(), out.join("tests.json").display());
    Ok(pass)
}

fn fresh(dir: &Path) -> Result<PathBuf> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    Ok(dir.to_path_buf())
}

/// One host's run, as a test reads it.
#[derive(Debug, Clone, Default)]
struct Run {
    /// `native`, or the browser.
    host: String,
    error: Option<String>,
    limit: bool,
    stop: String,
    frames: u64,
    watched: Vec<Vec<u32>>,
    logs: Vec<String>,
    /// RAM, VRAM and audio.
    hashes: String,
    /// Every hash asked for is there.
    complete: bool,
}

impl Run {
    fn from_native(r: &psxw_host_native::RunReport, n: usize) -> Self {
        Run {
            host: "native".into(),
            error: None,
            limit: r.stop == Stop::FrameLimit,
            stop: format!("{:?}", r.stop),
            frames: r.frames,
            watched: r.watched.clone(),
            logs: r.logs.clone(),
            hashes: format!("ram {:?} vram {:?} audio {} {}", r.ram_hashes, r.vram_hashes, r.audio.frames, r.audio.hash),
            complete: r.ram_hashes.len() == n,
        }
    }

    fn from_browser(r: &BrowserRun, n: usize) -> Self {
        Run {
            host: r.browser.clone(),
            error: r.error.clone(),
            limit: r.reached_limit(),
            stop: r.error.clone().unwrap_or_else(|| r.stop.clone()),
            frames: r.frames,
            watched: r.watched.clone(),
            logs: r.logs.clone(),
            hashes: r.hashes(),
            complete: r.ram_hashes.len() == n && r.vram_hashes.len() == n,
        }
    }

    /// A check's name: a browser's are prefixed with it.
    fn name(&self, what: &str) -> String {
        if self.host == "native" { what.to_string() } else { format!("{}: {what}", self.host) }
    }
}

/// One test: its runs, then its checks.
fn one_test(b: &dyn Builds, cache: &Cache, table: &SymbolTable, sel: &[AnyMod], m: &AnyMod, t: &Test, dir: &Path) -> Result<Vec<(String, bool, String)>> {
    let (ws, game) = (b.ws(), b.game());
    let spec = &t.spec;
    let replay = match &spec.replay {
        ReplayRef::Name { name } => crate::wholegame::replay(ws, game, Some(name))?.0,
        ReplayRef::File { file } => {
            let p = t.path.parent().unwrap_or(Path::new(".")).join(file);
            std::fs::read(&p).with_context(|| format!("reading {}", p.display()))?
        }
    };
    // Traced symbols, and those expectations read: (key, type, address).
    let mut watch: Vec<(String, ValueType, u32)> = Vec::new();
    let mut add = |sym: &str, offset: u32, key: String, ty: ValueType| -> Result<usize> {
        if let Some(i) = watch.iter().position(|(k, t, _)| *k == key && *t == ty) {
            return Ok(i);
        }
        // `SYMBOL+0xOFFSET`, as a trace's key spells it.
        let (sym, offset) = match sym.split_once('+') {
            Some((b, o)) => (b, offset + psxw_inputs::parse_u32(o).with_context(|| format!("{sym}: offset {o}"))?),
            None => (sym, offset),
        };
        // A trace reads any RAM: a splat auto-name (`D_<address>`) outside
        // every container's data (a work area past the executable's bss)
        // reads where it says.
        let (addr, size) = match table.resolve_data(&Target { symbol: Some(sym.to_string()), ..Default::default() }) {
            Ok((_, addr, size, _)) => (addr, size),
            Err(e) => match sym.rsplit_once('_').and_then(|(_, h)| u32::from_str_radix(h, 16).ok()).filter(|a| (0x8000_0000..0x8020_0000).contains(a)) {
                Some(a) if sym.len() > 9 => (a, None),
                _ => return Err(e),
            },
        };
        // An offset reaches past the symbol on purpose (a work area).
        if let (Some(s), 0) = (size, offset) {
            ensure!(ty.size() <= s, "{sym} is {s} bytes; {ty:?} reads {}", ty.size());
        }
        watch.push((key, ty, addr.wrapping_add(offset)));
        Ok(watch.len() - 1)
    };
    for s in &spec.trace {
        add(&s.symbol, s.offset.unwrap_or(0), s.key(), s.ty)?;
    }
    let mut symbol_checks = Vec::new();
    for e in &spec.expect {
        if let Expect::Symbol { at, symbol, ty, op, value } = e {
            let ty = ty.unwrap_or(ValueType::U32);
            ensure!(psxw_modkit::test::compare(0, op, 0).is_some(), "unknown op {op}");
            ensure!(*at >= 1 && *at <= spec.frames, "symbol {symbol}: at {at} is outside 1..{}", spec.frames);
            symbol_checks.push((add(symbol, 0, symbol.clone(), ty)?, *at, symbol.clone(), op.clone(), *value, ty));
        }
    }
    let same_as: Option<AnyMod> = spec
        .expect
        .iter()
        .find_map(|e| match e {
            Expect::SameAs { with } => Some(with.clone()),
            _ => None,
        })
        .map(|id| {
            let d = m.dir().parent().unwrap_or(Path::new(".")).join(&id);
            AnyMod::load(&d).with_context(|| format!("same_as {id}: no mod beside {} ({})", m.id(), d.display()))
        })
        .transpose()?;
    let in_browser = m.is_script() || (!cache.skip_same_as && same_as.as_ref().is_some_and(AnyMod::is_script));
    let compare = spec.expect.iter().any(|e| matches!(e, Expect::Deterministic | Expect::SameAs { .. }));
    let hash: Vec<u64> = [1, 2, 3, 4].iter().map(|k| (spec.frames * k / 4).max(1)).collect();

    // The test's swaps: mods beside this one, in its place at vblanks.
    let index = sel.iter().position(|x| x.id() == m.id()).unwrap_or(0);
    let swaps: Vec<(u64, usize, AnyMod)> = spec
        .swap
        .iter()
        .map(|w| {
            let d = m.dir().parent().unwrap_or(Path::new(".")).join(&w.with);
            Ok((w.at, index, AnyMod::load(&d).with_context(|| format!("swap {}: no mod beside {}", w.with, m.id()))?))
        })
        .collect::<Result<_>>()?;
    let in_browser = in_browser || swaps.iter().any(|w| w.2.is_script());
    // Runs the selection with `with` in `m`'s place throughout (no swaps),
    // or as the test says.
    let run = |with: Option<&AnyMod>, tag: &str| -> Result<Vec<Run>> {
        let mods: Vec<AnyMod> = sel.iter().map(|x| if x.id() == m.id() { with.cloned().unwrap_or_else(|| x.clone()) } else { x.clone() }).collect();
        let swaps: &[(u64, usize, AnyMod)] = if with.is_some() { &[] } else { &swaps };
        if in_browser {
            browser_run(b, cache, &mods, swaps, spec, &replay, &hash, &watch, &format!("{}-{}-{tag}", m.id(), t.name))
        } else {
            native_run(b, cache, &mods, swaps, spec, &replay, &hash, &watch, compare, &dir.join(format!("storage-{tag}")))
        }
    };
    let first = run(None, "1")?;
    let mut checks = Vec::new();
    for r in &first {
        if let Some(e) = &r.error {
            checks.push((r.name("run"), false, e.clone()));
        }
    }
    for e in &spec.expect {
        match e {
            Expect::NoTrap => {
                for r in &first {
                    checks.push((r.name("no_trap"), r.limit, format!("{} after {} frames", r.stop, r.frames)));
                }
            }
            Expect::Deterministic => {
                let again = run(None, "2")?;
                for (a, b) in first.iter().zip(&again) {
                    let same = a.hashes == b.hashes && a.complete && b.complete;
                    checks.push((a.name("deterministic"), same, format!("{} / {}", a.hashes, b.hashes)));
                }
            }
            Expect::SameAs { .. } if cache.skip_same_as => {}
            Expect::SameAs { with } => {
                let other = run(same_as.as_ref(), "same-as")?;
                for (a, b) in first.iter().zip(&other) {
                    let same = a.hashes == b.hashes && a.complete && a.limit && b.limit;
                    checks.push((a.name(&format!("same_as {with}")), same, if same { a.hashes.clone() } else { format!("{} ({}) / {with}: {} ({})", a.hashes, a.stop, b.hashes, b.stop) }));
                }
            }
            Expect::Log { contains } => {
                for r in &first {
                    let hit = r.logs.iter().find(|l| l.contains("mod: ") && l.contains(contains.as_str()));
                    checks.push((r.name(&format!("log {contains:?}")), hit.is_some(), hit.cloned().unwrap_or_else(|| "no such mod log line".into())));
                }
            }
            Expect::Symbol { .. } => {}
        }
    }
    for (i, at, symbol, op, value, ty) in &symbol_checks {
        for r in &first {
            let got = r.watched.get(*at as usize - 1).and_then(|row| row.get(*i)).map(|&raw| ty.value(raw));
            let pass = got.is_some_and(|g| psxw_modkit::test::compare(g, op, *value) == Some(true));
            checks.push((r.name(&format!("{symbol}@{at}")), pass, format!("{got:?} {op} {value}")));
        }
    }
    if let Some(check) = &spec.check {
        let trace_of = |r: &Run| -> serde_json::Value {
            let mut obj = serde_json::Map::new();
            for (k, (key, ty, _)) in watch.iter().enumerate() {
                let vals: Vec<i64> = r.watched.iter().map(|row| ty.value(row.get(k).copied().unwrap_or(0))).collect();
                obj.insert(key.clone(), vals.into());
            }
            serde_json::Value::Object(obj)
        };
        let baseline = if check.baseline {
            let plain = cache.plain(ws, game)?;
            let (mut o, _) = crate::run::options(ws, game, &[])?;
            o.wasm = plain.wasm.clone().into();
            o.frames = spec.frames;
            o.replay = Some(replay.clone());
            o.storage_dir = Some(fresh(&dir.join("storage-baseline"))?);
            o.watch = watch.iter().map(|(_, ty, a)| (*a, ty.size())).collect();
            let r = Run::from_native(&psxw_host_native::run(o)?, 0);
            Some(serde_json::json!({ "frames": r.frames, "trace": trace_of(&r) }))
        } else {
            None
        };
        for r in &first {
            let mut input = serde_json::json!({ "frames": r.frames, "trace": trace_of(r), "load_at": spec.load_at, "host": r.host });
            if let Some(bl) = &baseline {
                input["baseline"] = bl.clone();
            }
            std::fs::create_dir_all(dir)?;
            std::fs::write(dir.join(format!("trace-{}.json", r.host)), serde_json::to_string(&input)?)?;
            let (prog, args) = check.command.split_first().context("check.command is empty")?;
            let cwd = t.path.parent().unwrap_or(Path::new("."));
            let mut child = std::process::Command::new(prog)
                .args(args)
                .current_dir(cwd)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .with_context(|| format!("running {}", check.command.join(" ")))?;
            {
                use std::io::Write as _;
                let mut stdin = child.stdin.take().context("stdin")?;
                stdin.write_all(serde_json::to_string(&input)?.as_bytes())?;
            }
            let out = child.wait_with_output()?;
            let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            let last: Vec<&str> = text.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
            checks.push((r.name(&format!("check {}", check.command.join(" "))), out.status.success(), last.join(" | ")));
        }
    }
    Ok(checks)
}

/// The compiled mods of `mods`, natively: linked in, or (with `load_at` or
/// `commands`) loaded into the moddable base at run time.
#[allow(clippy::too_many_arguments)]
fn native_run(
    b: &dyn Builds,
    cache: &Cache,
    mods: &[AnyMod],
    swaps: &[(u64, usize, AnyMod)],
    spec: &psxw_modkit::test::TestSpec,
    replay: &[u8],
    hash: &[u64],
    watch: &[(String, ValueType, u32)],
    vram: bool,
    storage: &Path,
) -> Result<Vec<Run>> {
    let (ws, game) = (b.ws(), b.game());
    let compiled = psxw_modkit::script::compiled(mods);
    let (mut o, _) = crate::run::options(ws, game, &[])?;
    if spec.load_at.is_some() || !spec.commands.is_empty() || !swaps.is_empty() {
        o.wasm = cache.base(ws, game)?.wasm.into();
        let modules = b.modules(&compiled)?;
        match spec.load_at {
            Some(at) => o.swaps = modules.iter().enumerate().map(|(i, p)| psxw_host_native::ModSwap { frame: at, index: i, path: p.clone() }).collect(),
            None => o.mods = modules,
        }
        for (at, i, w) in swaps {
            let AnyMod::Compiled(c) = w else { anyhow::bail!("a native run swaps in compiled mods only") };
            let path = b.modules(std::slice::from_ref(c))?.remove(0);
            o.swaps.push(psxw_host_native::ModSwap { frame: *at, index: *i, path });
        }
    } else {
        o.wasm = b.linked(&compiled)?.wasm.into();
    }
    for c in &spec.commands {
        let args = c.args.iter().map(|a| a.as_i64().map(|v| v as u32).with_context(|| format!("command {}: natively, arguments are words", c.name))).collect::<Result<Vec<_>>>()?;
        o.execute.push(psxw_host_native::Execute { frame: c.at, function: c.name.clone(), args });
    }
    o.frames = spec.frames;
    o.replay = Some(replay.to_vec());
    o.storage_dir = Some(fresh(storage)?);
    o.watch = watch.iter().map(|(_, ty, a)| (*a, ty.size())).collect();
    o.hash_at = hash.to_vec();
    if vram {
        o.render = Some(crate::render::server(ws));
        o.vram_hash_at = hash.to_vec();
    }
    let r = psxw_host_native::run(o)?;
    let mut run = Run::from_native(&r, hash.len());
    if vram {
        run.complete &= r.vram_hashes.len() == hash.len();
    }
    Ok(vec![run])
}

/// `mods` in each browser, on the moddable base, all loaded at run time.
#[allow(clippy::too_many_arguments)]
fn browser_run(
    b: &dyn Builds,
    cache: &Cache,
    mods: &[AnyMod],
    test_swaps: &[(u64, usize, AnyMod)],
    spec: &psxw_modkit::test::TestSpec,
    replay: &[u8],
    hash: &[u64],
    watch: &[(String, ValueType, u32)],
    tag: &str,
) -> Result<Vec<Run>> {
    let (ws, game) = (b.ws(), b.game());
    let compiled = psxw_modkit::script::compiled(mods);
    let modules = b.modules(&compiled)?;
    let mut page_mods = Vec::new();
    let mut k = 0;
    for m in mods {
        page_mods.push(match m {
            AnyMod::Compiled(_) => {
                k += 1;
                PageMod::Module(modules[k - 1].clone())
            }
            AnyMod::Script(s) => PageMod::Script(s.dir.clone(), s.snapshot.clone().or_else(|| b.snapshot())),
        });
    }
    let (start, mut swaps): (Vec<PageMod>, Vec<(u64, usize, PageMod)>) = match spec.load_at {
        Some(at) => (vec![], page_mods.into_iter().enumerate().map(|(i, p)| (at, i, p)).collect()),
        None => (page_mods, vec![]),
    };
    for (at, i, w) in test_swaps {
        swaps.push((
            *at,
            *i,
            match w {
                AnyMod::Compiled(c) => PageMod::Module(b.modules(std::slice::from_ref(c))?.remove(0)),
                AnyMod::Script(s) => PageMod::Script(s.dir.clone(), s.snapshot.clone().or_else(|| b.snapshot())),
            },
        ));
    }
    let page = PageRun {
        tag: tag.replace('/', "-"),
        mods: start,
        swaps,
        replay: replay.to_vec(),
        frames: spec.frames,
        hash_at: hash.to_vec(),
        watch: watch.iter().map(|(_, ty, a)| (*a, ty.size())).collect(),
        commands: spec.commands.iter().map(|c| (c.at, c.name.clone(), c.args.clone())).collect(),
        browsers: BROWSERS.iter().map(|s| s.to_string()).collect(),
        timeout: (spec.frames / 20 + 300) as u32,
    };
    let base = cache.base(ws, game)?;
    Ok(crate::webtest::page_runs(ws, game, &base, &page)?.iter().map(|r| Run::from_browser(r, hash.len())).collect())
}
