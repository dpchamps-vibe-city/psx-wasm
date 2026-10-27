//! `psxw mod …` (plans/M11-modkit.md): checking, building, testing and
//! playing mods, inside a mods workspace (`psxmods.json` above the current
//! directory) or inside psx-wasm with the game and mod directories named.

use anyhow::{Context as _, Result, bail, ensure};
use psxw_build::game::{Options, Report};
use psxw_host_native::Stop;
use psxw_inputs::Workspace;
use psxw_modkit::build::{self, Built, Layout};
use psxw_modkit::decl::{Mod, Target};
use psxw_modkit::plan::Plan;
use psxw_modkit::symbols::SymbolTable;
use crate::modtest::{Builds, Cache};
use psxw_modkit::script::AnyMod;
use psxw_modkit::workspace::ModsWorkspace;
use std::path::{Path, PathBuf};

pub const USAGE: &str = "mod <command>; in a mods workspace (psxmods.json): \
    mod sdk [--bump] | mod check [mod|set] | mod build [mod|set] [--modules] | mod test [mod|set] \
    | mod play <mod|set> [--dynamic] [--port N] [--test] | mod watch <mod|set> [--port N] \
    | mod new <id> | mod show <symbol> | mod writers <symbol>; \
    in psx-wasm: mod check <game> <dir>… | mod build <game> <dir>… [--modules] [--out D] | mod test <game> <dir>… \
    | mod watch <game> <dir>… [--port N] | mod ci <game> \
    | mod new <game> <id> [--dir D] | mod show <game> <symbol> | mod writers <game> <symbol> \
    | mod pass-through <game> --out <dir>";

pub fn run(args: &[&str]) -> Result<bool> {
    match ModsWorkspace::discover()? {
        Some(m) => in_workspace(&m, args),
        None => in_psx(&Workspace::discover()?, args),
    }
}

/// The mods named by directory.
pub fn load(dirs: &[&str]) -> Result<Vec<Mod>> {
    dirs.iter().map(|d| Mod::load(Path::new(d))).collect()
}

pub fn label(mods: &[Mod]) -> String {
    if mods.is_empty() { "none".into() } else { mods.iter().map(|m| m.id()).collect::<Vec<_>>().join("+") }
}

fn label_any(mods: &[AnyMod]) -> String {
    if mods.is_empty() { "none".into() } else { mods.iter().map(|m| m.id()).collect::<Vec<_>>().join("+") }
}

/// The mods named by directory, of either kind.
pub fn load_any(dirs: &[&str]) -> Result<Vec<AnyMod>> {
    dirs.iter().map(|d| AnyMod::load(Path::new(d))).collect()
}

fn print_plan(plan: &Plan, mods: &[Mod]) -> bool {
    let order: Vec<&str> = plan.order.iter().map(|&i| mods[i].id()).collect();
    println!("load order: {}", if order.is_empty() { "(no mods)".into() } else { order.join(", ") });
    let chains = plan.chain_table(mods);
    if !chains.is_empty() {
        println!("chains (outermost link first):");
        for c in chains {
            println!("  {c}");
        }
    }
    if !plan.data.is_empty() {
        println!("data patches (applied in this order):");
        for d in &plan.data {
            println!("  {}: {} ({} 0x{:08X}, {} bytes)", mods[d.module].id(), d.what, d.container, d.addr, d.size);
        }
    }
    for w in &plan.warnings {
        println!("warning: {w}");
    }
    for e in &plan.errors {
        println!("error: {e}");
    }
    plan.ok()
}

fn in_workspace(m: &ModsWorkspace, args: &[&str]) -> Result<bool> {
    match args {
        ["sdk", rest @ ..] => {
            let bump = rest.contains(&"--bump");
            let out = psxw_modkit::workspace::sdk(m, bump)?;
            println!("mod SDK: {} (game {} @ {}, psx-wasm {})", m.sdk_dir().display(), out.lock.game, &out.lock.decomp[..7], &out.lock.psx_wasm[..7.min(out.lock.psx_wasm.len())]);
            for mv in &out.moved {
                println!("error: {mv}");
            }
            let ok = print_plan(&out.plan, &out.mods) && out.moved.is_empty();
            Ok(ok)
        }
        ["check", rest @ ..] => {
            let (_, any) = m.select_any(rest.first().copied())?;
            let table = m.table()?;
            let (plan, mods) = plan_any(&m.psx()?, &table, &any)?;
            let ok = print_plan(&plan, &mods);
            if ok {
                // clangd's view: every mod's mod.h and compile commands.
                let all = m.all()?;
                let full = psxw_modkit::plan::plan(&table, &all);
                if full.ok() {
                    m.generate(&table, &all, &full)?;
                }
            }
            Ok(ok)
        }
        ["build", rest @ ..] => {
            let name = rest.iter().find(|a| !a.starts_with("--")).copied();
            if rest.contains(&"--modules") {
                let psx = m.psx()?;
                m.check_pins(&psx)?;
                let (label, mods) = m.select(name)?;
                let inc = m.state.join("mods");
                let layout = Layout { out: m.build_dir(&label).join("modules"), include: Box::new(move |id| inc.join(id).join("include")), flags: Some(m.flags()?) };
                print_modules(&build::build_modules(&psx, m.game(), &mods, &layout)?);
                return Ok(true);
            }
            let (_, _, built, _) = build_ws(m, name)?;
            summary(&built);
            Ok(true)
        }
        ["test", rest @ ..] => {
            let name = rest.iter().find(|a| !a.starts_with("--")).copied();
            let psx = m.psx()?;
            m.check_pins(&psx)?;
            let (label, mods) = m.select_any(name)?;
            let table = m.table()?;
            let cache = Cache::default();
            let b = InWorkspace { m, psx: &psx, cache: &cache };
            crate::modtest::run_tests(&b, &cache, &table, &mods, None, &m.build_dir(&label).join("tests"))
        }
        ["play", name, rest @ ..] => {
            let (label, any) = m.select_any(Some(name))?;
            // Script mods load at run time, so their set does too.
            if rest.contains(&"--dynamic") || any.iter().any(AnyMod::is_script) {
                let psx = m.psx()?;
                m.check_pins(&psx)?;
                let rest: Vec<&str> = rest.iter().copied().filter(|a| *a != "--dynamic").collect();
                let modules = modules_ws(m, &psx, &label, &psxw_modkit::script::compiled(&any))?;
                let (base, _) = psxw_build::game::build(&psx, &Options { moddable: true, ..Options::new(m.game()) })?;
                return play(&psx, m.game(), &base, &m.state.join("web").join(format!("{label}-dynamic")), &rest, &page_mods(&any, &modules));
            }
            let (psx, _, built, label) = build_ws(m, Some(name))?;
            summary(&built);
            play(&psx, m.game(), &built.report, &m.state.join("web").join(&label), rest, &[])
        }
        ["watch", name, rest @ ..] => {
            let psx = m.psx()?;
            m.check_pins(&psx)?;
            let port = match rest {
                ["--port", p] => p.parse()?,
                [] => 8081,
                _ => bail!("usage: psxw mod watch <mod|set> [--port N]"),
            };
            let (label, _) = m.select_any(Some(name))?;
            let dir = m.state.join("web").join(format!("{label}-watch"));
            watch(&psx, m.game(), &dir, port, &|| {
                let (_, any) = m.select_any(Some(name))?;
                let modules = modules_ws(m, &psx, &label, &psxw_modkit::script::compiled(&any))?;
                Ok((any, modules))
            })
        }
        ["new", id] => {
            let dir = m.mods_dir().join(id);
            new_mod(&dir, id, m.game(), Some("../../.psxw/sdk/schema/mod.schema.json"), false)?;
            println!("created {}; declare hooks in its mod.json, then psxw mod check {id}", dir.display());
            Ok(true)
        }
        ["new", id, "--script"] => {
            let dir = m.mods_dir().join(id);
            new_mod(&dir, id, m.game(), None, true)?;
            let jsconfig = m.root.join("jsconfig.json");
            if !jsconfig.exists() {
                std::fs::write(&jsconfig, JSCONFIG)?;
            }
            println!("created {}; write its hooks in mod.js, then psxw mod check {id}", dir.display());
            Ok(true)
        }
        ["show", sym] => show(&m.table()?, sym),
        ["writers", sym] => crate::writers::run(&m.psx()?, m.game(), sym),
        _ => bail!("usage: psxw {USAGE}"),
    }
}

/// Script mods' definitions (plans/M12): each factory runs against a stub
/// API (`web/test/check-mods.mjs`) in Node, in the browsers image. A
/// factory that throws gives its error.
pub fn script_defs(ws: &Workspace, scripts: &[&psxw_modkit::script::ScriptMod]) -> Result<Vec<Result<psxw_modkit::script::Definition, String>>> {
    if scripts.is_empty() {
        return Ok(vec![]);
    }
    let image = crate::webtest::image(ws)?;
    let mut mounts = vec![(ws.root.join("web/test"), "/check".to_string(), true)];
    let mut args: Vec<String> = vec!["/check/check-mods.mjs".into()];
    for (i, s) in scripts.iter().enumerate() {
        mounts.push((std::path::absolute(&s.dir)?, format!("/mods/{i}"), true));
        args.push(format!("/mods/{i}"));
    }
    let m: Vec<(PathBuf, &str, bool)> = mounts.iter().map(|(h, g, ro)| (h.clone(), g.as_str(), *ro)).collect();
    let user = psxw_oracle::images::owner_of(&ws.root)?;
    let argv = psxw_oracle::images::docker_run(&image, &user, &m, Some("node"), &args);
    let out: Vec<serde_json::Value> = serde_json::from_str(&psxw_oracle::images::docker_output(&argv)?)?;
    out.into_iter()
        .map(|v| match v.get("error").and_then(|e| e.as_str()) {
            Some(e) => Ok(Err(e.lines().take(4).collect::<Vec<_>>().join(" | "))),
            None => Ok(Ok(serde_json::from_value(v["def"].clone())?)),
        })
        .collect()
}

/// Plan a selection of either kind: script mods' definitions (from their
/// factories) with compiled mods' declarations, in order (plans/M12).
pub fn plan_any(ws: &Workspace, table: &SymbolTable, any: &[AnyMod]) -> Result<(Plan, Vec<Mod>)> {
    let scripts: Vec<&psxw_modkit::script::ScriptMod> = any
        .iter()
        .filter_map(|m| match m {
            AnyMod::Script(s) => Some(s),
            AnyMod::Compiled(_) => None,
        })
        .collect();
    let mut defs = script_defs(ws, &scripts)?.into_iter();
    let mut mods = Vec::new();
    let mut errors = Vec::new();
    for m in any {
        match m {
            AnyMod::Compiled(c) => mods.push(c.clone()),
            AnyMod::Script(s) => match defs.next().context("a definition per script mod")? {
                Ok(def) => match s.as_mod(&def, &table.game, &table.main) {
                    Ok(x) => mods.push(x),
                    Err(e) => errors.push(format!("{}: {e:#}", s.id)),
                },
                Err(e) => errors.push(format!("{}: its factory threw: {e}", s.id)),
            },
        }
    }
    let mut plan = psxw_modkit::plan::plan(table, &mods);
    plan.errors.extend(errors);
    Ok((plan, mods))
}

/// The web bundle around a moddable base, with the names script mods
/// resolve (plans/M12): the symbol-list frontend's snapshot,
/// `snapshot.json`.
pub fn moddable_bundle(ws: &Workspace, game: &str, base: &Report, dir: PathBuf) -> Result<PathBuf> {
    let dir = psxw_build::web::bundle(ws, game, base, dir)?;
    write_snapshot(ws, game, &dir)?;
    Ok(dir)
}

/// `snapshot.json` in a bundle: the game's names (the symbol-list
/// frontend).
pub fn write_snapshot(ws: &Workspace, game: &str, dir: &Path) -> Result<()> {
    let (_, table) = build::table(ws, game)?;
    let (_, identity) = psxw_modkit::workspace::pins(ws, game)?;
    let snap = psxw_modkit::snapshot::from_symbols(&table, identity);
    std::fs::write(dir.join("snapshot.json"), snap.to_json())?;
    Ok(())
}

/// A workspace's mods as modules (phase 2).
fn modules_ws(m: &ModsWorkspace, psx: &Workspace, label: &str, mods: &[Mod]) -> Result<Vec<PathBuf>> {
    let inc = m.state.join("mods");
    let layout = Layout { out: m.build_dir(label).join("modules"), include: Box::new(move |id| inc.join(id).join("include")), flags: Some(m.flags()?) };
    let built = build::build_modules(psx, m.game(), mods, &layout)?;
    print_modules(&built);
    Ok(built.into_iter().map(|b| b.path).collect())
}

/// `psxw mod watch`: a moddable bundle served with the mods loaded at
/// start. Each time a compiled mod's files change, its module is rebuilt
/// and published, and the page swaps it in at the next vblank; each time a
/// script mod's do, it is published at a new versioned URL
/// (`mods/<id>@<seq>/mod.js`, so its relative imports reload with it), and
/// the page reloads it in place (plans/M12).
fn watch(ws: &Workspace, game: &str, dir: &Path, port: u16, rebuild: &dyn Fn() -> Result<(Vec<AnyMod>, Vec<PathBuf>)>) -> Result<bool> {
    let (mods, modules) = rebuild()?;
    let (base, _) = psxw_build::game::build(ws, &Options { moddable: true, ..Options::new(game) })?;
    let dir = moddable_bundle(ws, game, &base, dir.to_path_buf())?;
    std::fs::create_dir_all(dir.join("mods"))?;
    let script_url = |id: &str, seq: u64| -> Result<String> {
        let at = format!("{id}@{seq}");
        let m = mods.iter().find(|m| m.id() == id).context("script mod")?;
        crate::webtest::copy_js(m.dir(), &dir.join("mods").join(&at))?;
        Ok(format!("mods/{at}/mod.js"))
    };
    let mut urls = Vec::new();
    let mut hashes: Vec<Vec<u8>> = Vec::new();
    let mut k = 0;
    for m in &mods {
        match m {
            AnyMod::Compiled(_) => {
                let p = &modules[k];
                k += 1;
                let name = p.file_name().context("module")?.to_string_lossy().into_owned();
                std::fs::copy(p, dir.join("mods").join(&name))?;
                urls.push(format!("mods/{name}"));
                hashes.push(std::fs::read(p)?);
            }
            AnyMod::Script(s) => {
                urls.push(script_url(&s.id, 0)?);
                hashes.push(vec![]);
            }
        }
    }
    let mut seq = 0u64;
    let publish = |what: serde_json::Value| std::fs::write(dir.join("mods").join("watch.json"), what.to_string());
    publish(serde_json::json!({ "seq": seq, "index": 0, "url": "" }))?;
    let root = dir.clone();
    std::thread::spawn(move || {
        if let Err(e) = crate::serve::serve(root, port) {
            eprintln!("serve: {e:#}");
        }
    });
    eprintln!("watching {}; open http://localhost:{port}/?mods={}&watch=mods/watch.json", mods.iter().map(|m| m.id()).collect::<Vec<_>>().join(", "), urls.join(","));
    // Each mod's files: (path, modified, length).
    let stamp = |m: &AnyMod| -> Vec<(PathBuf, std::time::SystemTime, u64)> {
        let files: Vec<PathBuf> = match m {
            AnyMod::Compiled(c) => {
                let mut f = c.sources().unwrap_or_default();
                f.push(c.dir.join("mod.json"));
                f
            }
            AnyMod::Script(s) => js_files(&s.dir),
        };
        files
            .into_iter()
            .filter_map(|f| std::fs::metadata(&f).ok().map(|md| (f, md.modified().unwrap_or(std::time::UNIX_EPOCH), md.len())))
            .collect()
    };
    let mut last: Vec<_> = mods.iter().map(stamp).collect();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let now: Vec<_> = mods.iter().map(stamp).collect();
        if now == last {
            continue;
        }
        let changed: Vec<usize> = (0..mods.len()).filter(|&i| now[i] != last[i]).collect();
        last = now;
        // Script mods publish as they are; compiled ones rebuild.
        for &i in &changed {
            if let AnyMod::Script(s) = &mods[i] {
                seq += 1;
                let url = script_url(&s.id, seq)?;
                publish(serde_json::json!({ "seq": seq, "index": i, "kind": "script", "id": s.id, "url": url }))?;
                eprintln!("watch: published {url}; the page reloads {} at its next vblank", s.id);
            }
        }
        if !changed.iter().any(|&i| !mods[i].is_script()) {
            continue;
        }
        match rebuild() {
            Ok((_, modules)) => {
                let mut k = 0;
                for (i, m) in mods.iter().enumerate() {
                    if m.is_script() {
                        continue;
                    }
                    let p = &modules[k];
                    k += 1;
                    let bytes = std::fs::read(p)?;
                    if hashes[i] == bytes {
                        continue;
                    }
                    seq += 1;
                    let name = format!("{}.{seq}.mod.wasm", p.file_name().context("module")?.to_string_lossy().trim_end_matches(".mod.wasm"));
                    std::fs::write(dir.join("mods").join(&name), &bytes)?;
                    publish(serde_json::json!({ "seq": seq, "index": i, "kind": "module", "url": format!("mods/{name}") }))?;
                    eprintln!("watch: published {name}; the page swaps it in at its next vblank");
                    hashes[i] = bytes;
                }
            }
            Err(e) => eprintln!("watch: {e:#}"),
        }
    }
}

/// A script mod's JavaScript files, its tests left out.
fn js_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() && p.file_name().is_some_and(|n| n != "tests" && n != "node_modules") {
            out.extend(js_files(&p));
        } else if p.extension().is_some_and(|x| x == "js" || x == "mjs" || x == "json") {
            out.push(p);
        }
    }
    out.sort();
    out
}

/// The page's mods: compiled ones as their modules (`modules`, in order).
fn page_mods(any: &[AnyMod], modules: &[PathBuf]) -> Vec<crate::webtest::PageMod> {
    let mut k = 0;
    any.iter()
        .map(|m| match m {
            AnyMod::Compiled(_) => {
                k += 1;
                crate::webtest::PageMod::Module(modules[k - 1].clone())
            }
            AnyMod::Script(s) => crate::webtest::PageMod::Script(s.dir.clone()),
        })
        .collect()
}

/// Build a mod or set of a workspace: (psx-wasm, mods, the build, label).
fn build_ws(m: &ModsWorkspace, name: Option<&str>) -> Result<(Workspace, Vec<Mod>, Built, String)> {
    let psx = m.psx()?;
    m.check_pins(&psx)?;
    let (label, mods) = m.select(name)?;
    let inc = m.state.join("mods");
    let layout = Layout { out: m.build_dir(&label), include: Box::new(move |id| inc.join(id).join("include")), flags: Some(m.flags()?) };
    let built = build::build(&psx, &Options::new(m.game()), &mods, &layout)?;
    Ok((psx, mods, built, label))
}

fn summary(b: &Built) {
    println!("game.wasm: {} ({} bytes); mods.json: {}", b.report.wasm, b.report.game_bytes, b.mods_json.display());
}

/// Build `mods` in psx-wasm, into `out/<game>/mods/<label>/`.
pub fn build_direct(ws: &Workspace, game: &str, mods: &[Mod], out: Option<PathBuf>) -> Result<Built> {
    let out = std::path::absolute(out.unwrap_or_else(|| psxw_build::game::out_dir(ws, game).join("mods").join(label(mods))))?;
    let inc = out.join("mods");
    let layout = Layout { out: out.clone(), include: Box::new(move |id| inc.join(id).join("include")), flags: None };
    build::build(ws, &Options::new(game), mods, &layout)
}

fn print_modules(built: &[build::BuiltModule]) {
    for b in built {
        println!("{}: {} ({} bytes, {} hooked functions)", b.id, b.path.display(), b.bytes, b.targets);
    }
}

/// Each mod as its own module (phase 2), into `out/<game>/modules/`.
pub fn build_modules_direct(ws: &Workspace, game: &str, mods: &[Mod], out: Option<PathBuf>) -> Result<Vec<build::BuiltModule>> {
    let out = std::path::absolute(out.unwrap_or_else(|| psxw_build::game::out_dir(ws, game).join("modules")))?;
    let inc = out.join("mods");
    let layout = Layout { out, include: Box::new(move |id| inc.join(id).join("include")), flags: None };
    build::build_modules(ws, game, mods, &layout)
}

fn in_psx(ws: &Workspace, args: &[&str]) -> Result<bool> {
    match args {
        ["check", game, dirs @ ..] => {
            let any = load_any(dirs)?;
            let (_, table) = build::table(ws, game)?;
            let (plan, mods) = plan_any(ws, &table, &any)?;
            Ok(print_plan(&plan, &mods))
        }
        ["build", game, rest @ ..] => {
            let modules = rest.contains(&"--modules");
            let rest: Vec<&str> = rest.iter().copied().filter(|a| *a != "--modules").collect();
            let (dirs, out) = split_out(&rest)?;
            let mods = load(&dirs)?;
            if modules {
                print_modules(&build_modules_direct(ws, game, &mods, out)?);
                return Ok(true);
            }
            let built = build_direct(ws, game, &mods, out)?;
            summary(&built);
            Ok(true)
        }
        ["test", game, dirs @ ..] => {
            let mods = load_any(dirs)?;
            let (_, table) = build::table(ws, game)?;
            let cache = Cache::default();
            let b = Direct { ws, game, out_root: None, cache: &cache };
            let out = psxw_build::game::out_dir(ws, game).join("mods").join(label_any(&mods)).join("tests");
            crate::modtest::run_tests(&b, &cache, &table, &mods, None, &std::path::absolute(out)?)
        }
        ["new", game, id, rest @ ..] => {
            let script = rest.contains(&"--script");
            let rest: Vec<&str> = rest.iter().copied().filter(|a| *a != "--script").collect();
            let base = match rest.as_slice() {
                ["--dir", d] => PathBuf::from(d),
                [] => PathBuf::from("."),
                _ => bail!("usage: psxw mod new <game> <id> [--dir D] [--script]"),
            };
            let dir = base.join(id);
            new_mod(&dir, id, game, None, script)?;
            println!("created {}", dir.display());
            Ok(true)
        }
        ["show", game, sym] => show(&build::table(ws, game)?.1, sym),
        ["watch", game, rest @ ..] => {
            let mut dirs: Vec<&str> = Vec::new();
            let mut port = 8081u16;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--port" => port = it.next().context("--port needs a value")?.parse()?,
                    d => dirs.push(d),
                }
            }
            let dir = ws.out_dir().join("web").join(format!("{game}-watch"));
            watch(ws, game, &dir, port, &|| {
                let any = load_any(&dirs)?;
                let built = build_modules_direct(ws, game, &psxw_modkit::script::compiled(&any), None)?;
                print_modules(&built);
                Ok((any, built.into_iter().map(|b| b.path).collect()))
            })
        }
        ["writers", game, sym] => crate::writers::run(ws, game, sym),
        ["ci", game] => {
            let (pass, detail) = ci(ws, game)?;
            println!("mods gate: {} ({detail})", if pass { "PASS" } else { "FAIL" });
            Ok(pass)
        }
        ["pass-through", game, "--out", out, rest @ ..] => {
            let tiers: Option<Vec<&str>> = match rest {
                ["--tiers", t] => Some(t.split(',').collect()),
                [] => None,
                _ => bail!("usage: psxw mod pass-through <game> --out <dir> [--tiers c,lift,adapter]"),
            };
            let n = crate::passthrough::generate(ws, game, Path::new(out), tiers.as_deref())?;
            println!("pass-through mod on {n} functions: {out}");
            Ok(true)
        }
        _ => bail!("usage: psxw {USAGE}"),
    }
}

fn split_out<'a>(rest: &[&'a str]) -> Result<(Vec<&'a str>, Option<PathBuf>)> {
    let mut dirs = Vec::new();
    let mut out = None;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match *a {
            "--out" => out = Some(PathBuf::from(it.next().context("--out needs a directory")?)),
            d => dirs.push(d),
        }
    }
    Ok((dirs, out))
}

/// `jsconfig.json` at a workspace's root: the editor checks script mods
/// against the SDK's declarations (`psxw mod sdk`).
const JSCONFIG: &str = r#"{
  "compilerOptions": {
    "checkJs": true,
    "allowJs": true,
    "noEmit": true,
    "module": "esnext",
    "target": "es2022",
    "moduleResolution": "bundler",
    "types": []
  },
  "include": ["mods/**/*.js", ".psxw/sdk/psx.d.ts", ".psxw/sdk/game.d.ts"]
}
"#;

fn new_mod(dir: &Path, id: &str, game: &str, schema: Option<&str>, script: bool) -> Result<()> {
    ensure!(!dir.exists(), "{} already exists", dir.display());
    ensure!(!id.is_empty() && id.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'), "id {id:?}: lowercase letters, digits and '-'");
    if script {
        std::fs::create_dir_all(dir.join("tests"))?;
        let text = format!(
            "// @ts-check\n// {id}: a script mod for {game} (plans/M12). The loader calls this\n// factory with the mod's `game` and the API; it returns the definition.\n\nexport default mod(({{ game, hook, log }}) => ({{\n  id: \"{id}\",\n  version: \"0.1.0\",\n  hooks: [],\n  commands: {{}},\n}}));\n"
        );
        std::fs::write(dir.join("mod.js"), text)?;
        return Ok(());
    }
    std::fs::create_dir_all(dir.join("src"))?;
    std::fs::create_dir_all(dir.join("tests"))?;
    let mut decl = serde_json::json!({ "id": id, "version": "0.1.0", "game": game, "hooks": [] });
    if let Some(s) = schema {
        decl.as_object_mut().expect("object").insert("$schema".into(), s.into());
    }
    std::fs::write(dir.join("mod.json"), serde_json::to_string_pretty(&decl)? + "\n")?;
    let file = id.replace('-', "_");
    std::fs::write(
        dir.join("src").join(format!("{file}.c")),
        "#include \"mod.h\"\n\n/* Each hook mod.json declares is a function here; mod.h gives it its type. */\n",
    )?;
    Ok(())
}

fn show(table: &SymbolTable, sym: &str) -> Result<bool> {
    let t = Target { symbol: Some(sym.to_string()), ..Default::default() };
    match table.resolve_func(&t) {
        Ok(f) => {
            let info = table.function(&f.container, f.addr).context("function")?;
            let tier = serde_json::to_value(info.tier).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
            println!("{}  {} 0x{:08X}  tier {tier}", info.name(), f.container, f.addr);
            if info.names.len() > 1 {
                println!("  also named {}", info.names[1..].join(", "));
            }
            match table.decl(info) {
                Some(d) => println!("  declared in {}: {}", d.header, d.text),
                None => println!("  no header declares it: a hook on it needs a signature"),
            }
            if let Some(s) = &info.source {
                println!("  defined in {s}");
            } else {
                println!("  no C: it runs translated from the original's instructions");
            }
            if info.expanded {
                println!("  expanded into its translated callers: it can't be hooked");
            }
            let callers: Vec<String> = info.callers.iter().map(|(c, a)| table.display(c, *a)).collect();
            println!("  callers ({}): {}", callers.len(), if callers.is_empty() { "none direct (maybe through pointers)".into() } else { callers.join(", ") });
            Ok(true)
        }
        Err(fe) => match table.resolve_data(&t) {
            Ok((c, a, size, _)) => {
                println!("{sym}  {c} 0x{a:08X}{}", size.map(|s| format!("  {s} bytes")).unwrap_or_default());
                if let Some(d) = table.headers.data.get(sym) {
                    println!("  declared in {}: {}", d.header, d.text);
                }
                println!("  find what writes it: psxw mod writers {sym}");
                Ok(true)
            }
            Err(_) => {
                println!("{fe:#}");
                Ok(false)
            }
        },
    }
}

/// Serve the bundle with the mods, or (`--test`) run the browser
/// harness on it (title-to-gameplay unless `--replay` says).
fn play(ws: &Workspace, game: &str, report: &Report, dir: &Path, rest: &[&str], mods: &[crate::webtest::PageMod]) -> Result<bool> {
    if rest.contains(&"--test") {
        let mut args: Vec<&str> = rest.iter().copied().filter(|a| *a != "--test").collect();
        if !args.contains(&"--replay") {
            args.extend(["--replay", "title-to-gameplay", "--hash", "300,1200,2400,3600,4800,6000", "--timeout", "3600"]);
        }
        let mut modules = Vec::new();
        for m in mods {
            match m {
                crate::webtest::PageMod::Module(p) => modules.push(p.clone()),
                crate::webtest::PageMod::Script(_) => bail!("--test compares the browsers with the native host, which runs no script mods: test them with psxw mod test"),
            }
        }
        return crate::webtest::game_with(ws, game, &args, Some(report), &modules);
    }
    let mut port = 8081u16;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match *a {
            "--port" => port = it.next().context("--port needs a value")?.parse()?,
            other => bail!("unknown play option {other}"),
        }
    }
    let dir = psxw_build::web::bundle(ws, game, report, dir.to_path_buf())?;
    let mut query = String::new();
    if !mods.is_empty() {
        write_snapshot(ws, game, &dir)?;
        let urls = mods.iter().map(|m| m.place(&dir)).collect::<Result<Vec<_>>>()?;
        query = format!("?mods={}", urls.join(","));
    }
    eprintln!("serving {} on http://localhost:{port}/{query} (record a replay with the page's checkbox)", dir.display());
    crate::serve::serve(dir, port)?;
    Ok(true)
}

/// `psxw record <game> --mod <dir>…`: the bundle with the mods, served so
/// the page records replays with them applied.
pub fn record(ws: &Workspace, game: &str, args: &[&str]) -> Result<bool> {
    let mut dirs = Vec::new();
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--mod" => dirs.push(*it.next().context("--mod needs a directory")?),
            other => rest.push(other),
        }
    }
    let mods = load(&dirs)?;
    let built = build_direct(ws, game, &mods, None)?;
    let dir = ws.out_dir().join("web").join(format!("{game}-record-{}", label(&mods)));
    play(ws, game, &built.report, &dir, &rest, &[])
}

fn fresh(dir: &Path) -> Result<PathBuf> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    Ok(dir.to_path_buf())
}

/// Tests' builds in psx-wasm, under `out/<game>/mods/` and
/// `out/<game>/modules/` (or `out_root`).
pub struct Direct<'a> {
    pub ws: &'a Workspace,
    pub game: &'a str,
    pub out_root: Option<PathBuf>,
    pub cache: &'a Cache,
}

impl Builds for Direct<'_> {
    fn ws(&self) -> &Workspace {
        self.ws
    }
    fn game(&self) -> &str {
        self.game
    }
    fn linked(&self, mods: &[Mod]) -> Result<Report> {
        self.cache.linked(mods, &|mods, label| build_direct(self.ws, self.game, mods, self.out_root.as_ref().map(|r| r.join(label))))
    }
    fn modules(&self, mods: &[Mod]) -> Result<Vec<PathBuf>> {
        self.cache.modules(mods, &|mods| {
            let out = self.out_root.as_ref().map(|r| r.join("modules"));
            Ok(build_modules_direct(self.ws, self.game, mods, out)?.into_iter().map(|b| (b.id, b.path)).collect())
        })
    }
}

/// Tests' builds in a mods workspace, under `.psxw/build/`.
pub struct InWorkspace<'a> {
    pub m: &'a ModsWorkspace,
    pub psx: &'a Workspace,
    pub cache: &'a Cache,
}

impl Builds for InWorkspace<'_> {
    fn ws(&self) -> &Workspace {
        self.psx
    }
    fn game(&self) -> &str {
        self.m.game()
    }
    fn linked(&self, mods: &[Mod]) -> Result<Report> {
        self.cache.linked(mods, &|mods, label| {
            let inc = self.m.state.join("mods");
            let layout = Layout { out: self.m.build_dir(label), include: Box::new(move |id| inc.join(id).join("include")), flags: Some(self.m.flags()?) };
            build::build(self.psx, &Options::new(self.m.game()), mods, &layout)
        })
    }
    fn modules(&self, mods: &[Mod]) -> Result<Vec<PathBuf>> {
        self.cache.modules(mods, &|mods| {
            let inc = self.m.state.join("mods");
            let layout = Layout { out: self.m.build_dir("modules"), include: Box::new(move |id| inc.join(id).join("include")), flags: Some(self.m.flags()?) };
            let built = build::build_modules(self.psx, self.m.game(), mods, &layout)?;
            print_modules(&built);
            Ok(built.into_iter().map(|b| (b.id, b.path)).collect())
        })
    }
}

/// One replay run for a comparison: RAM, VRAM and audio hashes.
fn hashes(ws: &Workspace, game: &str, wasm: &str, replay: &[u8], frames: u64, at: &[u64], storage: PathBuf) -> Result<(Stop, String)> {
    let (mut o, _) = crate::run::options(ws, game, &[])?;
    o.wasm = wasm.into();
    o.frames = frames;
    o.replay = Some(replay.to_vec());
    o.storage_dir = Some(fresh(&storage)?);
    o.hash_at = at.to_vec();
    o.vram_hash_at = at.to_vec();
    o.render = Some(crate::render::server(ws));
    let r = psxw_host_native::run(o)?;
    let text = format!("ram {:?} vram {:?} audio {} {}", r.ram_hashes, r.vram_hashes, r.audio.frames, r.audio.hash);
    Ok((r.stop, text))
}

/// The CI gate `mods` (plans/M11 task 12).
pub fn ci(ws: &Workspace, game: &str) -> Result<(bool, String)> {
    let mut pass = true;
    let mut detail = Vec::new();
    let mut note = |ok: bool, what: String| {
        pass &= ok;
        detail.push(format!("{}{what}", if ok { "" } else { "FAIL " }));
    };
    let dir = std::path::absolute(ws.out_dir().join("ci").join(game).join("mods"))?;
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;

    // An empty mod set links exactly the plain build.
    let (plain, _) = psxw_build::game::build(ws, &Options::new(game))?;
    let empty = build_direct(ws, game, &[], Some(dir.join("empty")))?;
    let same = std::fs::read(&plain.wasm)? == std::fs::read(&empty.report.wasm)?;
    note(same, format!("empty mod set {} the plain game.wasm", if same { "is byte-identical to" } else { "differs from" }));

    // A no-op before, after and wrapping patch on every hookable function:
    // the game's replays stay bit-identical.
    let pt_dir = dir.join("pass-through-mod");
    let n = crate::passthrough::generate(ws, game, &pt_dir, None)?;
    let pt = build_direct(ws, game, &[Mod::load(&pt_dir)?], Some(dir.join("pass-through")))?;
    let mut replays: Vec<String> = std::fs::read_dir(ws.root.join("games").join(game).join("replays"))
        .map(|rd| rd.filter_map(|e| e.ok()?.path().file_stem().map(|s| s.to_string_lossy().into_owned())).collect())
        .unwrap_or_default();
    // Committed replays only: specs (.toml) and recordings git tracks.
    replays.retain(|r| {
        let base = ws.root.join("games").join(game).join("replays");
        base.join(format!("{r}.toml")).is_file()
            || std::process::Command::new("git").arg("-C").arg(&ws.root).args(["ls-files", "--error-unmatch"]).arg(base.join(format!("{r}.rpl"))).output().is_ok_and(|o| o.status.success())
    });
    replays.sort();
    replays.dedup();
    for r in &replays {
        let (bytes, _) = crate::wholegame::replay(ws, game, Some(r))?;
        let frames = psxw_abi::replay::Header::parse(&bytes).map_or(600, |h| h.frames as u64);
        let at: Vec<u64> = (1..=4).map(|k| frames * k / 4).collect();
        let (s1, a) = hashes(ws, game, &plain.wasm, &bytes, frames, &at, dir.join("storage-plain"))?;
        let (s2, b) = hashes(ws, game, &pt.report.wasm, &bytes, frames, &at, dir.join("storage-pt"))?;
        let ok = a == b && s1 == Stop::FrameLimit && s2 == Stop::FrameLimit;
        if !ok {
            std::fs::write(dir.join(format!("pass-through-{r}.txt")), format!("plain {s1:?} {a}\npass-through {s2:?} {b}\n"))?;
        }
        note(ok, format!("pass-through on {n} functions: {r} ({frames} frames) {}", if ok { "bit-identical" } else { "differs" }));
    }

    // The game's own test mods: a conflict (each of two replacements runs
    // when applied last, and check names both) and the feature mods.
    let mods_dir = ws.root.join("games").join(game).join("mods");
    let (a, b) = (mods_dir.join("conflict-a"), mods_dir.join("conflict-b"));
    let cache = Cache::default();
    let direct = Direct { ws, game, out_root: Some(dir.clone()), cache: &cache };
    if a.is_dir() && b.is_dir() {
        let (_, table) = build::table(ws, game)?;
        for (first, last) in [(&a, &b), (&b, &a)] {
            let mods = vec![Mod::load(first)?, Mod::load(last)?];
            let plan = psxw_modkit::plan::plan(&table, &mods);
            let (fi, li) = (mods[0].id().to_string(), mods[1].id().to_string());
            let warned = plan.warnings.iter().any(|w| w.contains(&fi) && w.contains(&li) && w.contains(&format!("{li}'s runs")));
            note(warned, format!("{fi} then {li}: check warns naming both, {li} wins"));
            let any: Vec<AnyMod> = mods.into_iter().map(AnyMod::Compiled).collect();
            let ok = crate::modtest::run_tests(&direct, &cache, &table, &any, Some(&li), &dir.join(format!("{fi}+{li}")).join("tests"))?;
            note(ok, format!("{fi} then {li}: {li}'s replacement is the one that runs"));
        }
        let mut others: Vec<PathBuf> = std::fs::read_dir(&mods_dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.join("mod.json").is_file() && p != &a && p != &b)
            .collect();
        others.sort();
        for m in others {
            let any = vec![AnyMod::Compiled(Mod::load(&m)?)];
            let id = any[0].id().to_string();
            let ok = crate::modtest::run_tests(&direct, &cache, &table, &any, None, &dir.join(&id).join("tests"))?;
            note(ok, format!("{id}: its tests"));
        }
    }

    // Phase 2: a moddable base without mods runs as the plain build; each
    // test mod loaded at run time runs as it does linked in; an identical
    // swap changes nothing. (A mod's own tests check what a swap does:
    // `load_at`.)
    if mods_dir.is_dir() {
        let (base, _) = psxw_build::game::build(ws, &Options { moddable: true, ..Options::new(game) })?;
        let (bytes, _) = crate::wholegame::replay(ws, game, Some("title-to-gameplay"))?;
        let at: Vec<u64> = vec![300, 1200, 2400];
        let run = |wasm: &str, mods: &[PathBuf], swaps: Vec<psxw_host_native::ModSwap>, _trace: Option<u32>, tag: &str| -> Result<psxw_host_native::RunReport> {
            let (mut o, _) = crate::run::options(ws, game, &[])?;
            o.wasm = wasm.into();
            o.frames = 2400;
            o.replay = Some(bytes.clone());
            o.storage_dir = Some(fresh(&dir.join(format!("storage-{tag}")))?);
            o.hash_at = at.clone();
            o.mods = mods.to_vec();
            o.swaps = swaps;
            psxw_host_native::run(o)
        };
        let key = |r: &psxw_host_native::RunReport| format!("{:?} {:?} {} {}", r.stop, r.ram_hashes, r.audio.frames, r.audio.hash);
        let plain_run = run(&plain.wasm, &[], vec![], None, "plain")?;
        let base_run = run(&base.wasm, &[], vec![], None, "base")?;
        note(key(&plain_run) == key(&base_run), "moddable base without mods runs as the plain build".into());
        let mut examples: Vec<PathBuf> = std::fs::read_dir(&mods_dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.join("mod.json").is_file() && p != &a && p != &b)
            .collect();
        examples.sort();
        let mut modules = Vec::new();
        for m in &examples {
            let mods = vec![Mod::load(m)?];
            let id = mods[0].id().to_string();
            let stat = build_direct(ws, game, &mods, Some(dir.join(format!("static-{id}"))))?;
            let module = build_modules_direct(ws, game, &mods, Some(dir.join("modules")))?.remove(0).path;
            let s = run(&stat.report.wasm, &[], vec![], None, &format!("static-{id}"))?;
            let d = run(&base.wasm, &[module.clone()], vec![], None, &format!("dynamic-{id}"))?;
            let same = key(&s) == key(&d) && s.stop == Stop::FrameLimit;
            note(same, format!("{id}: loaded at run time, the same RAM and audio as linked in"));
            modules.push((id, module));
        }
        // Swapping a mod for the identical module mid-run changes nothing.
        if let Some((id, m)) = modules.first() {
            let once = run(&base.wasm, &[m.clone()], vec![], None, "once")?;
            let swapped = run(&base.wasm, &[m.clone()], vec![psxw_host_native::ModSwap { frame: 1200, index: 0, path: m.clone() }], None, "same")?;
            note(key(&once) == key(&swapped), format!("{id}: swapped for the identical module at vblank 1200, the run is bit-identical"));
        }
    }

    // The pinned mods repository: check, build and test, against this
    // psx-wasm.
    if let Some(pin) = ws.lock.mods.get(game) {
        let snap = psxw_inputs::snapshot::ensure(ws, &format!("{game}-mods"), pin)?;
        let m = ModsWorkspace::open(&snap, Some(dir.join("workspace")), Some(ws.root.clone()))?;
        m.check_pins(ws)?;
        let sdk = psxw_modkit::workspace::sdk_into(&m, ws)?;
        let (label, mods) = m.select_any(m.set("all")?.map(|_| "all"))?;
        let compiled = psxw_modkit::script::compiled(&mods);
        let plan = psxw_modkit::plan::plan(&sdk, &compiled);
        let clean = plan.ok() && plan.warnings.is_empty();
        note(clean, format!("{} @ {}: check {} ({} mods, {} warnings)", game, pin.short(), if plan.ok() { "ok" } else { "errors" }, mods.len(), plan.warnings.len()));
        let b = InWorkspace { m: &m, psx: ws, cache: &cache };
        b.linked(&compiled)?;
        let ok = crate::modtest::run_tests(&b, &cache, &sdk, &mods, None, &m.build_dir(&label).join("tests"))?;
        note(ok, format!("{game} mods @ {}: build and tests", pin.short()));
    }
    Ok((pass, detail.join("; ")))
}
