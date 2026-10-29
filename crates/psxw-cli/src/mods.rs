//! `psxw mod …` (plans/completed/M11-modkit.md): checking, building, testing and
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

const USAGE: &str = "mod <command> [arguments] [options]\n\nRun `psxw mod --help` for commands and options.";

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
            for w in &out.merge_warnings {
                println!("warning: {w}");
            }
            if let Some(b) = &out.bump {
                println!("snapshot {}: {} renamed, {} layout changes, {} removed", out.lock.snapshot, b.renames.len(), b.layouts.len(), b.removals.len());
                for (o, n, c, a) in &b.renames {
                    println!("renamed: {o} -> {n} ({c} 0x{a:08X}); {o} stays a deprecated alias");
                }
                for l in &b.layouts {
                    println!("layout: {l}");
                }
                for r in &b.removals {
                    println!("gone: {r}");
                }
            }
            let ok = print_plan(&out.plan, &out.mods) && out.moved.is_empty();
            Ok(ok)
        }
        ["check", rest @ ..] => {
            let (_, any) = m.select_any(rest.first().copied())?;
            let table = m.table()?;
            let psx = m.psx()?;
            let snap = m.snapshot_path().ok().and_then(|p| psxw_modkit::snapshot::Snapshot::load(&p).ok());
            let (plan, mods) = plan_any_in(&psx, &table, &any, snap.as_ref())?;
            let mut ok = print_plan(&plan, &mods);
            let scripts = scripts_of(&any);
            for e in typecheck(&psx, &m.sdk_dir(), &scripts)? {
                println!("error: {e}");
                ok = false;
            }
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
            // Script mods load at run time: the moddable base, and the
            // compiled mods as modules beside them.
            let (label, any) = m.select_any(name)?;
            if any.iter().any(AnyMod::is_script) {
                let psx = m.psx()?;
                m.check_pins(&psx)?;
                modules_ws(m, &psx, &label, &psxw_modkit::script::compiled(&any))?;
                let (base, _) = psxw_build::game::build(&psx, &Options { moddable: true, ..Options::new(m.game()) })?;
                println!("moddable base: {} ({} bytes); script mods need no build", base.wasm, base.game_bytes);
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
            let cache = test_cache(&psx, m.game(), &mods, rest.contains(&"--debug"))?;
            let b = InWorkspace { m, psx: &psx, cache: &cache };
            crate::modtest::run_tests(&b, &cache, &table, &mods, None, &m.build_dir(&label).join("tests"))
        }
        ["play", name, rest @ ..] => {
            let (label, any) = m.select_any(Some(name))?;
            let debug = rest.contains(&"--debug");
            // Script mods load at run time, so their set does too.
            if rest.contains(&"--dynamic") || debug || any.iter().any(AnyMod::is_script) {
                let psx = m.psx()?;
                m.check_pins(&psx)?;
                no_compiled_in_debug(&any, debug)?;
                let rest: Vec<&str> = rest.iter().copied().filter(|a| *a != "--dynamic" && *a != "--debug").collect();
                let modules = modules_ws(m, &psx, &label, &psxw_modkit::script::compiled(&any))?;
                let (base, _) = psxw_build::game::build(&psx, &base_options(m.game(), debug))?;
                let dir = m.state.join("web").join(format!("{label}-dynamic{}", if debug { "-debug" } else { "" }));
                return play(&psx, m.game(), &base, &dir, &rest, &page_mods(&any, &modules, Some(&m.snapshot_path()?)));
            }
            let (psx, _, built, label) = build_ws(m, Some(name))?;
            summary(&built);
            play(&psx, m.game(), &built.report, &m.state.join("web").join(&label), rest, &[])
        }
        ["watch", name, rest @ ..] => {
            let psx = m.psx()?;
            m.check_pins(&psx)?;
            let (port, debug) = watch_args(rest)?;
            let (label, _) = m.select_any(Some(name))?;
            let dir = m.state.join("web").join(format!("{label}-watch{}", if debug { "-debug" } else { "" }));
            let snapshot = m.snapshot_path().ok();
            watch(&psx, m.game(), &dir, port, debug, snapshot.as_deref(), &|| {
                let (_, any) = m.select_any(Some(name))?;
                let modules = modules_ws(m, &psx, &label, &psxw_modkit::script::compiled(&any))?;
                Ok((any, modules))
            })
        }
        ["new", id, "--compiled"] => {
            let dir = m.mods_dir().join(id);
            new_mod(&dir, id, m.game(), Some("../../.psxw/sdk/schema/mod.schema.json"), false)?;
            println!("created {}; declare hooks in its mod.json, then psxw mod check {id}", dir.display());
            println!("note: compiled mods are deprecated (D56); `psxw mod new {id}` makes a script mod");
            Ok(true)
        }
        ["new", id] | ["new", id, "--script"] => {
            let dir = m.mods_dir().join(id);
            new_mod(&dir, id, m.game(), None, true)?;
            let jsconfig = m.root.join("jsconfig.json");
            if !jsconfig.exists() {
                std::fs::write(&jsconfig, JSCONFIG)?;
            }
            println!("created {}; write its hooks in mod.js, then psxw mod check {id}", dir.display());
            Ok(true)
        }
        ["pack", id] => {
            let any = AnyMod::load(&m.mods_dir().join(id))?;
            let AnyMod::Script(s) = any else { bail!("{id} is a compiled mod: psxw mod build --modules makes its module") };
            let snap = psxw_modkit::snapshot::Snapshot::load(&m.snapshot_path()?)?;
            let out = m.state.join("packs").join(format!("{id}.psxpack.json"));
            write_pack(&s, &snap, &out)
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

/// Type-check script mods (plans/M12): `tsc --noEmit` with `checkJs` over
/// each mod's JavaScript, against `psx.d.ts` and `game.d.ts` in `sdk`, in
/// the browsers image. Returns tsc's errors, with host paths.
pub fn typecheck(ws: &Workspace, sdk: &Path, scripts: &[&psxw_modkit::script::ScriptMod]) -> Result<Vec<String>> {
    if scripts.is_empty() {
        return Ok(vec![]);
    }
    let image = crate::webtest::image(ws)?;
    let mut files: Vec<String> = vec!["/sdk/psx.d.ts".into(), "/sdk/game.d.ts".into()];
    let mut mounts = vec![(std::path::absolute(sdk)?, "/sdk".to_string(), true)];
    for (i, s) in scripts.iter().enumerate() {
        let dir = std::path::absolute(&s.dir)?;
        for f in walk_js(&dir) {
            files.push(format!("/mods/{i}/{}", f.strip_prefix(&dir)?.to_string_lossy()));
        }
        mounts.push((dir, format!("/mods/{i}"), true));
    }
    let config = serde_json::json!({
        "compilerOptions": {
            "checkJs": true, "allowJs": true, "noEmit": true, "module": "esnext", "target": "es2022",
            "moduleResolution": "node", "lib": ["es2022"], "types": [], "strict": false
        },
        "files": files,
    });
    let check = sdk.join("check");
    std::fs::create_dir_all(&check)?;
    std::fs::write(check.join("tsconfig.json"), serde_json::to_string_pretty(&config)?)?;
    let m: Vec<(PathBuf, &str, bool)> = mounts.iter().map(|(h, g, ro)| (h.clone(), g.as_str(), *ro)).collect();
    let user = psxw_oracle::images::owner_of(sdk)?;
    let argv = psxw_oracle::images::docker_run(&image, &user, &m, Some("tsc"), &["-p".into(), "/sdk/check/tsconfig.json".into(), "--pretty".into(), "false".into()]);
    let out = std::process::Command::new("docker").args(&argv).output().context("running tsc in the browsers image")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut errors: Vec<String> = text.lines().filter(|l| l.contains(": error TS")).map(str::to_string).collect();
    // tsc names files relative to its working directory, `/`.
    for e in &mut errors {
        let path = e.trim_start_matches('/').to_string();
        for (i, s) in scripts.iter().enumerate() {
            if let Some(rest) = path.strip_prefix(&format!("mods/{i}/")) {
                *e = format!("{}/{rest}", s.dir.display());
            }
        }
        if let Some(rest) = path.strip_prefix("sdk/") {
            *e = format!("{}/{rest}", sdk.display());
        }
    }
    if !out.status.success() && errors.is_empty() {
        bail!("tsc failed: {}{}", text.trim(), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(errors)
}

/// A script mod's JavaScript files, its tests left out.
fn walk_js(dir: &Path) -> Vec<PathBuf> {
    js_files(dir).into_iter().filter(|p| p.extension().is_some_and(|x| x == "js" || x == "mjs")).collect()
}

/// Phase 2 exit criterion 2 (plans/M12): the snapshot against the
/// census's layout probe and the address map.
pub fn verify_snapshot(ws: &Workspace, game: &str, snap: &psxw_modkit::snapshot::Snapshot) -> Result<psxw_modkit::cfront::Agreement> {
    let manifest = ws.game(game)?;
    let pin = ws.game_pin(&manifest)?;
    let checkout = ws.checkout(pin);
    let tool = |p: &Path| if p.components().count() > 1 { checkout.join(p) } else { p.to_path_buf() };
    let reference = psxw_cc::probe::Reference {
        cpp: tool(&manifest.reference.cpp),
        cppflags: manifest.reference.cppflags.clone(),
        cc1: tool(&manifest.reference.cc1),
        cc1flags: manifest.reference.cc1flags.clone(),
    };
    let src = build::snapshot(ws, game)?;
    let map = psxw_inputs::AddressMap::load(&src, &manifest)?;
    let work = ws.out_dir().join("modkit").join(game).join("verify");
    let clang = psxw_cc::Clang::new(ws.clang()?, src.clone(), manifest.c.include.clone(), manifest.c.defines.clone(), &work)?;
    psxw_modkit::cfront::verify(snap, &clang, &reference, &src, &manifest, &map, &work.join("probe"))
}

/// A definition's names that `snap` keeps only as deprecated aliases: each
/// becomes what it is now, with a warning.
fn aliases(def: &mut psxw_modkit::script::Definition, snap: &psxw_modkit::snapshot::Snapshot, id: &str) -> Vec<String> {
    let mut out = Vec::new();
    let targets = def.hooks.iter_mut().map(|h| &mut h.target).chain(def.data.iter_mut().map(|d| &mut d.target));
    for t in targets {
        let Some(name) = t.symbol.clone() else { continue };
        if let Some(now) = psxw_modkit::snapshot::renamed(snap, &name) {
            out.push(format!("{id}: {name} was renamed {now} (snapshot {}); {name} still works, as a deprecated alias", snap.hash()));
            t.symbol = Some(now.to_string());
        }
    }
    out
}

/// `psxw mod pack`: the mod and the part of `snap` it uses, in one file.
fn write_pack(s: &psxw_modkit::script::ScriptMod, snap: &psxw_modkit::snapshot::Snapshot, out: &Path) -> Result<bool> {
    let pack = psxw_modkit::script::pack(s, snap)?;
    if let Some(d) = out.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(out, serde_json::to_string(&pack)?)?;
    let sub = &pack["snapshot"];
    println!(
        "packed {}: {} ({} functions, {} globals, {} types from snapshot {}); load it in the page's mod panel",
        s.id,
        out.display(),
        sub["functions"].as_array().map_or(0, Vec::len),
        sub["globals"].as_array().map_or(0, Vec::len),
        sub["types"].as_object().map_or(0, |o| o.len()),
        snap.hash()
    );
    Ok(true)
}

/// The script mods of a selection.
fn scripts_of(any: &[AnyMod]) -> Vec<&psxw_modkit::script::ScriptMod> {
    any.iter()
        .filter_map(|m| match m {
            AnyMod::Script(s) => Some(s),
            AnyMod::Compiled(_) => None,
        })
        .collect()
}

/// The script mod SDK at psx-wasm's pin, outside a workspace:
/// `out/modkit/<game>/sdk/` (snapshot, `game.d.ts`, `psx.d.ts`).
pub fn direct_sdk(ws: &Workspace, game: &str) -> Result<PathBuf> {
    let dir = std::path::absolute(ws.out_dir().join("modkit").join(game).join("sdk"))?;
    let (_, table) = build::table(ws, game)?;
    let (_, snap) = build::snapshots(ws, game)?;
    psxw_modkit::workspace::write_types(&dir, &snap, &table)?;
    Ok(dir)
}

/// Plan a selection of either kind: script mods' definitions (from their
/// factories) with compiled mods' declarations, in order (plans/M12).
pub fn plan_any(ws: &Workspace, table: &SymbolTable, any: &[AnyMod]) -> Result<(Plan, Vec<Mod>)> {
    plan_any_in(ws, table, any, None)
}

/// The same, script mods' names resolved through `snapshot` (D55): a name
/// it keeps only as a deprecated alias warns, naming what it is now.
pub fn plan_any_in(ws: &Workspace, table: &SymbolTable, any: &[AnyMod], snapshot: Option<&psxw_modkit::snapshot::Snapshot>) -> Result<(Plan, Vec<Mod>)> {
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
    let mut warnings = Vec::new();
    for m in any {
        match m {
            AnyMod::Compiled(c) => mods.push(c.clone()),
            AnyMod::Script(s) => match defs.next().context("a definition per script mod")? {
                Ok(mut def) => match {
                    if let Some(snap) = snapshot {
                        warnings.extend(aliases(&mut def, snap, &s.id));
                    }
                    s.as_mod(&def, &table.game, &table.main)
                } {
                    Ok(x) => mods.push(x),
                    Err(e) => errors.push(format!("{}: {e:#}", s.id)),
                },
                Err(e) => errors.push(format!("{}: its factory threw: {e}", s.id)),
            },
        }
    }
    let mut plan = psxw_modkit::plan::plan(table, &mods);
    plan.errors.extend(errors);
    plan.warnings.extend(warnings);
    Ok((plan, mods))
}

/// The web bundle around a moddable base, with the names and types script
/// mods resolve (plans/M12): `snapshot.json`.
pub fn moddable_bundle(ws: &Workspace, game: &str, base: &Report, dir: PathBuf) -> Result<PathBuf> {
    let dir = psxw_build::web::bundle(ws, game, base, dir)?;
    write_snapshot(ws, game, &dir)?;
    Ok(dir)
}

/// `snapshot.json` in a bundle: the game's names and types at psx-wasm's
/// pin (the C-header frontend's snapshot), which script mods without one
/// of their own resolve through.
pub fn write_snapshot(ws: &Workspace, game: &str, dir: &Path) -> Result<()> {
    let (_, snap) = build::snapshots(ws, game)?;
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
fn watch(ws: &Workspace, game: &str, dir: &Path, port: u16, debug: bool, snapshot: Option<&Path>, rebuild: &dyn Fn() -> Result<(Vec<AnyMod>, Vec<PathBuf>)>) -> Result<bool> {
    let (mods, modules) = rebuild()?;
    no_compiled_in_debug(&mods, debug)?;
    let (base, _) = psxw_build::game::build(ws, &base_options(game, debug))?;
    let dir = moddable_bundle(ws, game, &base, dir.to_path_buf())?;
    std::fs::create_dir_all(dir.join("mods"))?;
    let script_url = |id: &str, seq: u64| -> Result<String> {
        let at = format!("{id}@{seq}");
        let m = mods.iter().find(|m| m.id() == id).context("script mod")?;
        crate::webtest::place_script(m.dir(), snapshot, &dir, &dir.join("mods").join(&at))?;
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

/// The moddable base, or (`debug`) its debug build (D53): the C's DWARF
/// in DevTools, script mods loaded at run time.
pub fn base_options(game: &str, debug: bool) -> Options {
    Options { moddable: true, debug, ..Options::new(game) }
}

/// Compiled mods (deprecated, D56) don't load into a debug build: their
/// code has the default window's address built in.
fn no_compiled_in_debug(mods: &[AnyMod], debug: bool) -> Result<()> {
    if let Some(c) = mods.iter().find(|m| !m.is_script()).filter(|_| debug) {
        bail!("{} is a compiled mod: compiled mods are deprecated (D56) and don't load into a debug build; leave out --debug, or use a script mod", c.id());
    }
    Ok(())
}

/// `watch`'s options: `--port N`, `--debug`.
fn watch_args(rest: &[&str]) -> Result<(u16, bool)> {
    let mut port = 8081;
    let mut debug = false;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match *a {
            "--port" => port = it.next().context("--port needs a value")?.parse()?,
            "--debug" => debug = true,
            other => bail!("usage: psxw mod watch <mod|set> [--port N] [--debug] (not {other})"),
        }
    }
    Ok((port, debug))
}

/// Tests' cache: with `--debug`, the debug moddable base, and no `same_as`
/// (the mods it would compare with are compiled, and don't load there).
fn test_cache(ws: &Workspace, game: &str, mods: &[AnyMod], debug: bool) -> Result<Cache> {
    if !debug {
        return Ok(Cache::default());
    }
    no_compiled_in_debug(mods, debug)?;
    let (base, _) = psxw_build::game::build(ws, &base_options(game, true))?;
    Ok(Cache::with_base(base, true))
}

/// The page's mods: compiled ones as their modules (`modules`, in order),
/// script ones with `snapshot`.
fn page_mods(any: &[AnyMod], modules: &[PathBuf], snapshot: Option<&Path>) -> Vec<crate::webtest::PageMod> {
    let mut k = 0;
    any.iter()
        .map(|m| match m {
            AnyMod::Compiled(_) => {
                k += 1;
                crate::webtest::PageMod::Module(modules[k - 1].clone())
            }
            AnyMod::Script(s) => crate::webtest::PageMod::Script(s.dir.clone(), snapshot.map(Path::to_path_buf)),
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
            let mut ok = print_plan(&plan, &mods);
            let scripts = scripts_of(&any);
            if !scripts.is_empty() {
                let sdk = direct_sdk(ws, game)?;
                for e in typecheck(ws, &sdk, &scripts)? {
                    println!("error: {e}");
                    ok = false;
                }
            }
            Ok(ok)
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
            let debug = dirs.contains(&"--debug");
            let dirs: Vec<&str> = dirs.iter().copied().filter(|d| *d != "--debug").collect();
            let mods = load_any(&dirs)?;
            let (_, table) = build::table(ws, game)?;
            let cache = test_cache(ws, game, &mods, debug)?;
            let b = Direct { ws, game, out_root: None, cache: &cache, snapshot: None };
            let out = psxw_build::game::out_dir(ws, game).join("mods").join(label_any(&mods)).join("tests");
            crate::modtest::run_tests(&b, &cache, &table, &mods, None, &std::path::absolute(out)?)
        }
        ["new", game, id, rest @ ..] => {
            let script = !rest.contains(&"--compiled");
            let rest: Vec<&str> = rest.iter().copied().filter(|a| *a != "--script" && *a != "--compiled").collect();
            let base = match rest.as_slice() {
                ["--dir", d] => PathBuf::from(d),
                [] => PathBuf::from("."),
                _ => bail!("usage: psxw mod new <game> <id> [--dir D] [--compiled]"),
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
            let mut debug = false;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--port" => port = it.next().context("--port needs a value")?.parse()?,
                    "--debug" => debug = true,
                    d => dirs.push(d),
                }
            }
            let dir = ws.out_dir().join("web").join(format!("{game}-watch{}", if debug { "-debug" } else { "" }));
            watch(ws, game, &dir, port, debug, None, &|| {
                let any = load_any(&dirs)?;
                let built = build_modules_direct(ws, game, &psxw_modkit::script::compiled(&any), None)?;
                print_modules(&built);
                Ok((any, built.into_iter().map(|b| b.path).collect()))
            })
        }
        ["writers", game, sym] => crate::writers::run(ws, game, sym),
        ["pack", game, dir, rest @ ..] => {
            let out = match rest {
                ["--out", o] => PathBuf::from(o),
                [] => ws.out_dir().join("modkit").join(game).join("packs").join(format!("{}.psxpack.json", Path::new(dir).file_name().unwrap_or_default().to_string_lossy())),
                _ => bail!("usage: psxw mod pack <game> <dir> [--out F]"),
            };
            let s = psxw_modkit::script::ScriptMod::load(Path::new(dir))?;
            let (_, snap) = build::snapshots(ws, game)?;
            write_pack(&s, &snap, &out)
        }
        ["snapshot", game, rest @ ..] => {
            let mut source = "c";
            let mut out: Option<PathBuf> = None;
            let mut check = false;
            let mut agree = false;
            let mut at: Option<&str> = None;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--source" => source = it.next().context("--source symbols|c|dwarf")?,
                    "--out" => out = Some(PathBuf::from(it.next().context("--out needs a file")?)),
                    "--verify" => check = true,
                    "--agree" => agree = true,
                    "--at" => at = Some(it.next().context("--at needs a decomp commit")?),
                    other => bail!("unknown snapshot option {other}"),
                }
            }
            let (symbols, c) = match at {
                Some(commit) => build::snapshots_at(ws, game, commit)?,
                None => build::snapshots(ws, game)?,
            };
            if agree {
                let view = build::dwarf_view(ws, game)?;
                let (n, d) = psxw_modkit::dwarf::agree(&c, &view);
                println!("DWARF ({} units) and the headers agree on {n} records and globals both describe; {} disagreements", view.units, d.len());
                for x in d.iter().take(30) {
                    println!("  {x}");
                }
                return Ok(d.is_empty());
            }
            let snap = match source {
                "symbols" => symbols,
                "c" => c,
                "dwarf" => build::dwarf_snapshot(ws, game, &c)?,
                other => bail!("--source {other}: symbols, c or dwarf"),
            };
            let records = snap.types.values().filter(|t| matches!(t, psxw_modkit::snapshot::TypeDef::Struct { .. } | psxw_modkit::snapshot::TypeDef::Union { .. })).count();
            println!(
                "snapshot {} ({} {}): {} functions ({} with signatures), {} globals ({} typed), {} records, {} dropped",
                snap.hash(),
                snap.source.kind,
                &snap.source.version[..7.min(snap.source.version.len())],
                snap.functions.len(),
                snap.functions.iter().filter(|f| f.signature.is_some()).count(),
                snap.globals.len(),
                snap.globals.iter().filter(|g| g.ty.is_some()).count(),
                records,
                snap.dropped.len()
            );
            if let Some(o) = out {
                std::fs::write(&o, snap.to_json())?;
                println!("wrote {}", o.display());
            }
            if check {
                let a = verify_snapshot(ws, game, &snap)?;
                println!("agrees with the census probe on {} layouts and the address map on {} addresses; {} disagreements", a.layouts, a.addresses, a.disagreements.len());
                for d in a.disagreements.iter().take(20) {
                    println!("  {d}");
                }
                return Ok(a.disagreements.is_empty());
            }
            Ok(true)
        }
        ["ci", game] => {
            let (pass, detail) = ci(ws, game)?;
            println!("mods gate: {} ({detail})", if pass { "PASS" } else { "FAIL" });
            Ok(pass)
        }
        ["pass-through", game, "--out", out, "--script"] => {
            let n = crate::passthrough::generate_script(ws, game, Path::new(out))?;
            println!("script pass-through mod on {n} functions: {out}");
            Ok(true)
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
    "moduleResolution": "node",
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
                crate::webtest::PageMod::Script(..) => bail!("--test compares the browsers with the native host, which runs no script mods: test them with psxw mod test"),
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
    /// The snapshot script mods' `game` comes from (the bundle's when none).
    pub snapshot: Option<PathBuf>,
}

impl Builds for Direct<'_> {
    fn ws(&self) -> &Workspace {
        self.ws
    }
    fn snapshot(&self) -> Option<PathBuf> {
        self.snapshot.clone()
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
    fn snapshot(&self) -> Option<PathBuf> {
        self.m.snapshot_path().ok()
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
    let direct = Direct { ws, game, out_root: Some(dir.clone()), cache: &cache, snapshot: None };
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

    // M12: script mods, in Chromium and Firefox.
    if mods_dir.is_dir() {
        script_gate(ws, game, &dir, &mods_dir, &cache, &mut note)?;
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
        // Script mods (plans/M12): the repo's types, and every mod outside
        // that selection tested alone (script twins in the browsers, each
        // `same_as` its compiled mod).
        psxw_modkit::workspace::sdk_types(&m, ws, None)?;
        let all = m.all_any()?;
        let scripts = scripts_of(&all);
        let errors = typecheck(ws, &m.sdk_dir(), &scripts)?;
        note(errors.is_empty(), format!("{game} mods @ {}: {} script mods type-check{}", pin.short(), scripts.len(), errors.first().map(|e| format!(": {e}")).unwrap_or_default()));
        for x in all.iter().filter(|x| !mods.iter().any(|y| y.id() == x.id())) {
            let ok = crate::modtest::run_tests(&b, &cache, &sdk, std::slice::from_ref(x), None, &m.build_dir(x.id()).join("tests"))?;
            note(ok, format!("{game} mods @ {}: {}'s tests{}", pin.short(), x.id(), if x.is_script() { " in both browsers" } else { "" }));
        }
    }
    Ok((pass, detail.join("; ")))
}

/// The `mods` gate's script part (plans/M12), in Chromium and Firefox. Every
/// game fact comes from the game's test mods and their tests:
///
/// - phase 2: the C snapshot against the census probe and the address map;
///   the script mods type-check, the fixture (`fixtures/bad-types`) fails
///   with exactly its marked errors;
/// - the script twins' tests (each `same_as` its compiled mod), pinned to
///   the symbol-list snapshot: names alone are enough;
/// - the script pass-through over title-to-gameplay;
/// - conflicts across both kinds (the `conflict-a`/`conflict-b` pairs);
/// - phase 3: decomp iterations (`iterations/`), the DWARF and C frontends
///   agreeing, and a twin's tests on a base where its target's slot is o32.
fn script_gate(ws: &Workspace, game: &str, dir: &Path, mods_dir: &Path, cache: &Cache, note: &mut dyn FnMut(bool, String)) -> Result<()> {
    use crate::webtest::{PageMod, PageRun};
    let (_, table) = build::table(ws, game)?;
    let mut twins: Vec<PathBuf> = std::fs::read_dir(mods_dir)?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.join("mod.js").is_file()).collect();
    twins.sort();
    if twins.is_empty() {
        return Ok(());
    }

    // Phase 2: snapshots and types.
    let (symbols, snap) = build::snapshots(ws, game)?;
    let a = verify_snapshot(ws, game, &snap)?;
    if !a.disagreements.is_empty() {
        std::fs::write(dir.join("snapshot-disagreements.txt"), a.disagreements.join("\n"))?;
    }
    note(
        a.disagreements.is_empty() && a.layouts > 0,
        format!("C snapshot {}: agrees with the census probe on {} layouts and the address map on {} addresses ({} disagreements)", snap.hash(), a.layouts, a.addresses, a.disagreements.len()),
    );
    let sdk = direct_sdk(ws, game)?;
    let scripts: Vec<psxw_modkit::script::ScriptMod> = twins.iter().map(|t| psxw_modkit::script::ScriptMod::load(t)).collect::<Result<_>>()?;
    let errors = typecheck(ws, &sdk, &scripts.iter().collect::<Vec<_>>())?;
    note(errors.is_empty(), format!("{} script mods type-check against game.d.ts{}", scripts.len(), errors.first().map(|e| format!(": {e}")).unwrap_or_default()));
    let fixture = ws.root.join("games").join(game).join("fixtures").join("bad-types");
    if fixture.join("mod.js").is_file() {
        let f = psxw_modkit::script::ScriptMod::load(&fixture)?;
        let text = std::fs::read_to_string(fixture.join("mod.js"))?;
        let marked: Vec<usize> = text.lines().enumerate().filter(|(_, l)| l.contains("// error:")).map(|(i, _)| i + 1).collect();
        let errors = typecheck(ws, &sdk, &[&f])?;
        let lines: Vec<usize> = errors.iter().filter_map(|e| e.split_once(".js(").and_then(|(_, r)| r.split(',').next()).and_then(|n| n.parse().ok())).collect();
        let ok = errors.len() == marked.len() && lines == marked;
        note(ok, format!("the type-check fixture fails with exactly its {} marked errors (got {}: lines {lines:?})", marked.len(), errors.len()));
        if !ok {
            std::fs::write(dir.join("typecheck-fixture.txt"), errors.join("\n"))?;
        }
    }

    // The twins' tests, pinned to the symbol-list snapshot (phase 3: names
    // alone are enough); a conflict twin with its pair applied first.
    let snaps = dir.join("snapshots");
    std::fs::create_dir_all(&snaps)?;
    let symbols_path = snaps.join(format!("{}.json", symbols.hash()));
    std::fs::write(&symbols_path, symbols.to_json())?;
    let names_only = Direct { ws, game, out_root: Some(dir.to_path_buf()), cache, snapshot: Some(symbols_path.clone()) };
    let pair = |id: &str| -> Option<String> {
        let rest = id.strip_prefix("conflict-")?;
        let (x, suffix) = rest.split_once('-').map_or((rest, ""), |(x, s)| (x, s));
        let other = if x == "a" { "b" } else { "a" };
        Some(if suffix.is_empty() { format!("conflict-{other}") } else { format!("conflict-{other}-{suffix}") })
    };
    for t in &twins {
        let m = AnyMod::load(t)?;
        let id = m.id().to_string();
        let mut sel = vec![m];
        if let Some(p) = pair(&id).map(|p| mods_dir.join(p)).filter(|p| p.is_dir()) {
            sel.insert(0, AnyMod::load(&p)?);
        }
        let ok = crate::modtest::run_tests(&names_only, cache, &table, &sel, Some(&id), &dir.join(format!("script-{id}")).join("tests"))?;
        note(ok, format!("{id}: its tests in both browsers, by names alone (symbol-list snapshot {})", symbols.hash()));
    }

    let base = cache.base(ws, game)?;
    let (replay, _) = crate::wholegame::replay(ws, game, Some("title-to-gameplay"))?;
    let frames = psxw_abi::replay::Header::parse(&replay).map_or(6000, |h| h.frames as u64);
    let at: Vec<u64> = (1..=4).map(|k| frames * k / 4).collect();
    let browsers: Vec<String> = crate::modtest::BROWSERS.iter().map(|s| s.to_string()).collect();
    let page = |tag: &str, mods: Vec<PageMod>, frames: u64, at: Vec<u64>| PageRun {
        tag: format!("gate-{tag}"),
        mods,
        swaps: vec![],
        replay: replay.clone(),
        frames,
        hash_at: at,
        watch: vec![],
        commands: vec![],
        browsers: browsers.clone(),
        timeout: (frames / 4 + 600) as u32,
    };

    // The pass-through on every hookable function: bit-identical to the
    // plain build, with waits held while JS was on the stack.
    let pt_dir = dir.join("pass-through-js");
    let n = crate::passthrough::generate_script(ws, game, &pt_dir)?;
    let plain = cache.plain(ws, game)?;
    let (s0, want) = hashes(ws, game, &plain.wasm, &replay, frames, &at, dir.join("storage-plain-js"))?;
    for r in &crate::webtest::page_runs(ws, game, &base, &page("pass-through", vec![PageMod::script(pt_dir.clone())], frames, at.clone()))? {
        let same = r.reached_limit() && s0 == Stop::FrameLimit && r.hashes() == want;
        let held = r.report().and_then(|e| e["held_waits"].as_u64()).unwrap_or(0);
        if !same {
            std::fs::write(dir.join(format!("pass-through-js-{}.txt", r.browser)), format!("plain {s0:?} {want}\n{} {} {}\n", r.browser, r.stop, r.hashes()))?;
        }
        note(same, format!("{}: script pass-through on {n} functions: title-to-gameplay ({frames} frames) {} ({:.0} s)", r.browser, if same { "bit-identical to the plain build" } else { "differs" }, r.seconds));
        note(held > 0, format!("{}: {held} vblank waits held while JS was on the stack", r.browser));
    }

    // Conflicts: script over script, and script over compiled. The check
    // and the loader each warn naming both mods; the last applied runs
    // (its own tests, with its pair applied first).
    for (first, last) in [("conflict-a-js", "conflict-b-js"), ("conflict-a", "conflict-b-js")] {
        let (fd, ld) = (mods_dir.join(first), mods_dir.join(last));
        if !fd.is_dir() || !ld.is_dir() {
            continue;
        }
        let sel = vec![AnyMod::load(&fd)?, AnyMod::load(&ld)?];
        let warned = |w: &str| w.contains(first) && w.contains(last) && w.contains(&format!("{last}'s runs"));
        let (plan, _) = plan_any(ws, &table, &sel)?;
        note(plan.warnings.iter().any(|w| warned(w)), format!("{first} then {last}: psxw mod check warns naming both, {last} wins"));
        let page_mods = page_mods(&sel, &direct_modules(ws, game, dir, cache, &sel)?, None);
        for r in &crate::webtest::page_runs(ws, game, &base, &page(&format!("conflict-{first}"), page_mods, 60, vec![60]))? {
            let w = r.events.iter().filter(|e| e["event"] == "mods").flat_map(|e| e["warnings"].as_array().cloned().unwrap_or_default()).any(|w| warned(w.as_str().unwrap_or("")));
            note(w, format!("{}: {first} then {last}: the loader warns naming both, {last} wins", r.browser));
        }
        let ok = crate::modtest::run_tests(&names_only, cache, &table, &sel, Some(last), &dir.join(format!("conflict-{first}")).join("tests"))?;
        note(ok, format!("{first} then {last}: {last}'s replacement is the one that runs"));
    }

    phase3_gate(ws, game, dir, mods_dir, &twins, &table, &snap, cache, note)
}

/// The compiled mods of a selection as modules, for the gate's page runs.
fn direct_modules(ws: &Workspace, game: &str, dir: &Path, cache: &Cache, sel: &[AnyMod]) -> Result<Vec<PathBuf>> {
    let d = Direct { ws, game, out_root: Some(dir.to_path_buf()), cache, snapshot: None };
    d.modules(&psxw_modkit::script::compiled(sel))
}

/// Phase 3 of the gate: decomp iterations, the frontends agreeing, slot
/// flavours.
#[allow(clippy::too_many_arguments)]
fn phase3_gate(
    ws: &Workspace,
    game: &str,
    dir: &Path,
    mods_dir: &Path,
    twins: &[PathBuf],
    table: &SymbolTable,
    snap: &psxw_modkit::snapshot::Snapshot,
    cache: &Cache,
    note: &mut dyn FnMut(bool, String),
) -> Result<()> {
    let manifest = ws.game(game)?;
    let pin = ws.game_pin(&manifest)?.commit.clone();
    let snaps = dir.join("snapshots");

    // Iterations: the previous decomp pin's snapshot against this one's.
    let iterations = ws.root.join("games").join(game).join("iterations");
    if let (Some(prev), true) = (manifest.mods.previous_decomp.clone(), iterations.is_dir()) {
        let (_, old) = build::snapshots_at(ws, game, &prev)?;
        let mut bumped = snap.clone();
        let b = psxw_modkit::snapshot::bump(&old, &mut bumped);
        note(
            !b.renames.is_empty(),
            format!("--bump {} -> {}: {} renamed (old names kept as aliases), {} layout changes, {} gone", &prev[..7], &pin[..7], b.renames.len(), b.layouts.len(), b.removals.len()),
        );
        let save = |s: &psxw_modkit::snapshot::Snapshot| -> Result<PathBuf> {
            let p = snaps.join(format!("{}.json", s.hash()));
            std::fs::write(&p, s.to_json())?;
            Ok(p)
        };
        let (old_path, new_path, bumped_path) = (save(&old)?, save(snap)?, save(&bumped)?);
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&iterations)?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.join("mod.js").is_file()).collect();
        dirs.sort();
        let pinned = |p: &Path, to: &Path| -> Result<AnyMod> {
            let mut s = psxw_modkit::script::ScriptMod::load(p)?;
            s.snapshot = Some(to.to_path_buf());
            Ok(AnyMod::Script(s))
        };
        let olds: Vec<&PathBuf> = dirs.iter().filter(|d| d.file_name().is_some_and(|n| n.to_string_lossy().starts_with("old-"))).collect();
        // A mod using an old name: its check warns, and it runs on the
        // bumped snapshot, the old name an alias.
        for d in &olds {
            let m = pinned(d, &bumped_path)?;
            let (plan, _) = plan_any_in(ws, table, std::slice::from_ref(&m), Some(&bumped))?;
            note(plan.warnings.iter().any(|w| w.contains("was renamed")), format!("{}: its check warns its old name was renamed ({})", m.id(), plan.warnings.join("; ")));
            let ok = crate::modtest::run_tests(&Direct { ws, game, out_root: Some(dir.to_path_buf()), cache, snapshot: None }, cache, table, &[m.clone()], None, &dir.join(format!("iter-{}", m.id())).join("tests"))?;
            note(ok, format!("{}: runs on the current snapshot, by its old name", m.id()));
        }
        // Mods pinned to each snapshot, loaded together.
        let sel: Vec<AnyMod> = dirs
            .iter()
            .map(|d| if olds.contains(&d) { pinned(d, &old_path) } else { pinned(d, &new_path) })
            .collect::<Result<_>>()?;
        let ok = crate::modtest::run_tests(&Direct { ws, game, out_root: Some(dir.to_path_buf()), cache, snapshot: None }, cache, table, &sel, None, &dir.join("iterations").join("tests"))?;
        note(ok, format!("mods pinned to snapshots {} ({}) and {} ({}) load together and pass their tests", old.hash(), &prev[..7], snap.hash(), &pin[..7]));
    }

    // The DWARF and C frontends agree.
    let view = build::dwarf_view(ws, game)?;
    let (n, d) = psxw_modkit::dwarf::agree(snap, &view);
    note(d.is_empty() && n > 0, format!("DWARF ({} units) and the C frontend agree on the {n} records and globals both describe{}", view.units, d.first().map(|x| format!(": {x}")).unwrap_or_default()));

    // Slot flavours: the first twin hooking a C-typed function passes its
    // tests on a base built with that function translated (its slot o32).
    let defs = script_defs(ws, &twins.iter().map(|t| psxw_modkit::script::ScriptMod::load(t)).collect::<Result<Vec<_>>>()?.iter().collect::<Vec<_>>())?;
    let base = cache.base(ws, game)?;
    let slots = |wasm: &str| -> Result<std::collections::BTreeMap<(String, u32), String>> {
        let bytes = std::fs::read(wasm)?;
        let mut out = std::collections::BTreeMap::new();
        for p in wasmparser::Parser::new(0).parse_all(&bytes) {
            if let wasmparser::Payload::CustomSection(c) = p? {
                if c.name() == psxw_link::game::exports::SLOTS_SECTION {
                    for l in String::from_utf8_lossy(c.data()).lines() {
                        let mut it = l.splitn(3, ' ');
                        if let (Some(c), Some(a), Some(f)) = (it.next(), it.next(), it.next()) {
                            out.insert((c.to_string(), psxw_inputs::parse_u32(a).unwrap_or(0)), f.to_string());
                        }
                    }
                }
            }
        }
        Ok(out)
    };
    let normal = slots(&base.wasm)?;
    let found = twins.iter().zip(&defs).find_map(|(t, def)| {
        let def = def.as_ref().ok()?;
        def.hooks.iter().find_map(|h| {
            let name = h.target.symbol.clone()?;
            let f = table.resolve_func(&h.target).ok()?;
            normal.get(&(f.container.clone(), f.addr)).filter(|fl| fl.starts_with("c ")).map(|_| (t.clone(), name, f.container.clone(), f.addr))
        })
    });
    if let Some((twin, name, c, addr)) = found {
        let (lift, _) = psxw_build::game::build(ws, &Options { moddable: true, lift: vec![name.clone()], ..Options::new(game) })?;
        let flavour = slots(&lift.wasm)?.get(&(c.clone(), addr)).cloned().unwrap_or_default();
        note(flavour == "o32", format!("a base built with {name} translated: its slot is {flavour:?} (C-typed in the moddable base)"));
        let lift_cache = Cache::with_base(lift, true);
        let m = AnyMod::load(&twin)?;
        let id = m.id().to_string();
        let ok = crate::modtest::run_tests(&Direct { ws, game, out_root: Some(dir.join("lift")), cache: &lift_cache, snapshot: None }, &lift_cache, table, &[m], None, &dir.join(format!("lift-{id}")).join("tests"))?;
        note(ok, format!("{id}: its tests pass on both bases ({name}'s slot C-typed, then o32)"));
    }
    let _ = mods_dir;
    Ok(())
}
