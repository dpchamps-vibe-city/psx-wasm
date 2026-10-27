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
use psxw_modkit::test::{Expect, ReplayRef, Test};
use psxw_modkit::workspace::ModsWorkspace;
use serde::Serialize;
use std::path::{Path, PathBuf};

pub const USAGE: &str = "mod <command>; in a mods workspace (psxmods.json): \
    mod sdk [--bump] | mod check [mod|set] | mod build [mod|set] | mod test [mod|set] \
    | mod play <mod|set> [--port N] [--test] | mod new <id> | mod show <symbol> | mod writers <symbol>; \
    in psx-wasm: mod check <game> <dir>… | mod build <game> <dir>… | mod test <game> <dir>… \
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

fn label(mods: &[Mod]) -> String {
    if mods.is_empty() { "none".into() } else { mods.iter().map(|m| m.id()).collect::<Vec<_>>().join("+") }
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
            let (_, mods) = m.select(rest.first().copied())?;
            let table = m.table()?;
            let plan = psxw_modkit::plan::plan(&table, &mods);
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
            let (psx, mods, built, _) = build_ws(m, name)?;
            summary(&built);
            let table = m.table()?;
            run_tests(&psx, m.game(), &table, &mods, &built)
        }
        ["play", name, rest @ ..] => {
            let (psx, _, built, label) = build_ws(m, Some(name))?;
            summary(&built);
            play(&psx, m.game(), &built.report, &m.state.join("web").join(&label), rest)
        }
        ["new", id] => {
            let dir = m.mods_dir().join(id);
            new_mod(&dir, id, m.game(), Some("../../.psxw/sdk/schema/mod.schema.json"))?;
            println!("created {}; declare hooks in its mod.json, then psxw mod check {id}", dir.display());
            Ok(true)
        }
        ["show", sym] => show(&m.table()?, sym),
        ["writers", sym] => crate::writers::run(&m.psx()?, m.game(), sym),
        _ => bail!("usage: psxw {USAGE}"),
    }
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
            let mods = load(dirs)?;
            let (_, table) = build::table(ws, game)?;
            Ok(print_plan(&psxw_modkit::plan::plan(&table, &mods), &mods))
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
            let mods = load(dirs)?;
            let built = build_direct(ws, game, &mods, None)?;
            summary(&built);
            let (_, table) = build::table(ws, game)?;
            run_tests(ws, game, &table, &mods, &built)
        }
        ["new", game, id, rest @ ..] => {
            let base = match rest {
                ["--dir", d] => PathBuf::from(d),
                [] => PathBuf::from("."),
                _ => bail!("usage: psxw mod new <game> <id> [--dir D]"),
            };
            let dir = base.join(id);
            new_mod(&dir, id, game, None)?;
            println!("created {}", dir.display());
            Ok(true)
        }
        ["show", game, sym] => show(&build::table(ws, game)?.1, sym),
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

fn new_mod(dir: &Path, id: &str, game: &str, schema: Option<&str>) -> Result<()> {
    ensure!(!dir.exists(), "{} already exists", dir.display());
    ensure!(!id.is_empty() && id.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'), "id {id:?}: lowercase letters, digits and '-'");
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
fn play(ws: &Workspace, game: &str, report: &Report, dir: &Path, rest: &[&str]) -> Result<bool> {
    if rest.contains(&"--test") {
        let mut args: Vec<&str> = rest.iter().copied().filter(|a| *a != "--test").collect();
        if !args.contains(&"--replay") {
            args.extend(["--replay", "title-to-gameplay", "--hash", "300,1200,2400,3600,4800,6000", "--timeout", "3600"]);
        }
        return crate::webtest::game_with(ws, game, &args, Some(report));
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
    eprintln!("serving {} on http://localhost:{port}/ (record a replay with ?record)", dir.display());
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
    play(ws, game, &built.report, &dir, &rest)
}

#[derive(Serialize)]
struct Outcome {
    test: String,
    pass: bool,
    checks: Vec<(String, bool, String)>,
}

fn fresh(dir: &Path) -> Result<PathBuf> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    Ok(dir.to_path_buf())
}

/// Run every test of every mod in `mods` on `built`.
fn run_tests(ws: &Workspace, game: &str, table: &SymbolTable, mods: &[Mod], built: &Built) -> Result<bool> {
    let out = PathBuf::from(&built.report.wasm).parent().context("build directory")?.join("tests");
    std::fs::create_dir_all(&out)?;
    let mut all = Vec::new();
    let mut plain: Option<PathBuf> = None;
    for m in mods {
        for t in psxw_modkit::test::tests(&m.dir)? {
            let name = format!("{}/{}", m.id(), t.name);
            eprintln!("test {name}: {} frames", t.spec.frames);
            let o = one_test(ws, game, table, &t, built, &out.join(m.id()).join(&t.name), &mut plain)
                .with_context(|| format!("test {name}"))?;
            for (what, pass, detail) in &o.0 {
                println!("{name}: {what:<14} {:<4} {detail}", if *pass { "ok" } else { "FAIL" });
            }
            all.push(Outcome { test: name, pass: o.0.iter().all(|c| c.1), checks: o.0 });
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

type Checks = (Vec<(String, bool, String)>,);

fn one_test(ws: &Workspace, game: &str, table: &SymbolTable, t: &Test, built: &Built, dir: &Path, plain: &mut Option<PathBuf>) -> Result<Checks> {
    let spec = &t.spec;
    let replay = match &spec.replay {
        ReplayRef::Name { name } => crate::wholegame::replay(ws, game, Some(name))?.0,
        ReplayRef::File { file } => {
            let p = t.path.parent().unwrap_or(Path::new(".")).join(file);
            std::fs::read(&p).with_context(|| format!("reading {}", p.display()))?
        }
    };
    // Traced symbols, and those expectations read.
    let mut watch: Vec<(String, psxw_modkit::test::ValueType, u32)> = Vec::new();
    let mut add = |sym: &str, ty: psxw_modkit::test::ValueType| -> Result<usize> {
        if let Some(i) = watch.iter().position(|(s, t, _)| s == sym && *t == ty) {
            return Ok(i);
        }
        let (_, addr, size, _) = table.resolve_data(&Target { symbol: Some(sym.to_string()), ..Default::default() })?;
        if let Some(s) = size {
            ensure!(ty.size() <= s, "{sym} is {s} bytes; {ty:?} reads {}", ty.size());
        }
        watch.push((sym.to_string(), ty, addr));
        Ok(watch.len() - 1)
    };
    for s in &spec.trace {
        add(&s.symbol, s.ty)?;
    }
    let mut symbol_checks = Vec::new();
    for e in &spec.expect {
        if let Expect::Symbol { at, symbol, ty, op, value } = e {
            let ty = ty.unwrap_or(psxw_modkit::test::ValueType::U32);
            ensure!(psxw_modkit::test::compare(0, op, 0).is_some(), "unknown op {op}");
            ensure!(*at >= 1 && *at <= spec.frames, "symbol {symbol}: at {at} is outside 1..{}", spec.frames);
            symbol_checks.push((add(symbol, ty)?, *at, symbol.clone(), op.clone(), *value, ty));
        }
    }
    let deterministic = spec.expect.iter().any(|e| matches!(e, Expect::Deterministic));
    let hash: Vec<u64> = [1, 2, 3, 4].iter().map(|k| (spec.frames * k / 4).max(1)).collect();
    let options = |wasm: &str, storage: PathBuf| -> Result<psxw_host_native::Options> {
        let (mut o, _) = crate::run::options(ws, game, &[])?;
        o.wasm = wasm.into();
        o.frames = spec.frames;
        o.replay = Some(replay.clone());
        o.storage_dir = Some(storage);
        o.watch = watch.iter().map(|(_, ty, a)| (*a, ty.size())).collect();
        o.hash_at = hash.clone();
        if deterministic {
            o.render = Some(crate::render::server(ws));
            o.vram_hash_at = hash.clone();
        }
        Ok(o)
    };
    let first = psxw_host_native::run(options(&built.report.wasm, fresh(&dir.join("storage-1"))?)?)?;
    let mut checks = Vec::new();
    for e in &spec.expect {
        match e {
            Expect::NoTrap => checks.push(("no_trap".into(), first.stop == Stop::FrameLimit, format!("{:?} after {} frames", first.stop, first.frames))),
            Expect::Deterministic => {
                let again = psxw_host_native::run(options(&built.report.wasm, fresh(&dir.join("storage-2"))?)?)?;
                let same = first.ram_hashes == again.ram_hashes
                    && first.vram_hashes == again.vram_hashes
                    && first.audio.hash == again.audio.hash
                    && first.audio.frames == again.audio.frames
                    && first.ram_hashes.len() == hash.len()
                    && first.vram_hashes.len() == hash.len();
                checks.push((
                    "deterministic".into(),
                    same,
                    format!(
                        "RAM {:?} / {:?}; VRAM {} / {} frames; audio {} {} / {} {}",
                        first.ram_hashes.values().collect::<Vec<_>>(),
                        again.ram_hashes.values().collect::<Vec<_>>(),
                        first.vram_hashes.len(),
                        again.vram_hashes.len(),
                        first.audio.frames,
                        first.audio.hash,
                        again.audio.frames,
                        again.audio.hash
                    ),
                ));
            }
            Expect::Log { contains } => {
                let hit = first.logs.iter().find(|l| l.contains("mod: ") && l.contains(contains.as_str()));
                checks.push((format!("log {contains:?}"), hit.is_some(), hit.cloned().unwrap_or_else(|| "no such mod log line".into())));
            }
            Expect::Symbol { .. } => {}
        }
    }
    for (i, at, symbol, op, value, ty) in symbol_checks {
        let got = first.watched.get(at as usize - 1).and_then(|r| r.get(i)).map(|&raw| ty.value(raw));
        let pass = got.is_some_and(|g| psxw_modkit::test::compare(g, &op, value) == Some(true));
        checks.push((format!("{symbol}@{at}"), pass, format!("{got:?} {op} {value}")));
    }
    if let Some(check) = &spec.check {
        let trace_of = |r: &psxw_host_native::RunReport| -> serde_json::Value {
            let mut m = serde_json::Map::new();
            for (k, (sym, ty, _)) in watch.iter().enumerate() {
                let vals: Vec<i64> = r.watched.iter().map(|row| ty.value(row.get(k).copied().unwrap_or(0))).collect();
                m.insert(sym.clone(), vals.into());
            }
            serde_json::Value::Object(m)
        };
        let mut input = serde_json::json!({ "frames": first.frames, "trace": trace_of(&first) });
        if check.baseline {
            let wasm = match plain {
                Some(p) => p.clone(),
                None => {
                    let (r, _) = psxw_build::game::build(ws, &Options::new(game))?;
                    *plain = Some(PathBuf::from(&r.wasm));
                    PathBuf::from(r.wasm)
                }
            };
            let mut o = options(&wasm.display().to_string(), fresh(&dir.join("storage-baseline"))?)?;
            o.render = None;
            o.vram_hash_at.clear();
            let base = psxw_host_native::run(o)?;
            input["baseline"] = serde_json::json!({ "frames": base.frames, "trace": trace_of(&base) });
        }
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join("trace.json"), serde_json::to_string(&input)?)?;
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
        checks.push((format!("check {}", check.command.join(" ")), out.status.success(), last.join(" | ")));
    }
    Ok((checks,))
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
    if a.is_dir() && b.is_dir() {
        let (_, table) = build::table(ws, game)?;
        for (first, last) in [(&a, &b), (&b, &a)] {
            let mods = vec![Mod::load(first)?, Mod::load(last)?];
            let plan = psxw_modkit::plan::plan(&table, &mods);
            let (fi, li) = (mods[0].id().to_string(), mods[1].id().to_string());
            let warned = plan.warnings.iter().any(|w| w.contains(&fi) && w.contains(&li) && w.contains(&format!("{li}'s runs")));
            note(warned, format!("{fi} then {li}: check warns naming both, {li} wins"));
            let built = build_direct(ws, game, &mods, Some(dir.join(format!("{fi}+{li}"))))?;
            let ok = run_tests(ws, game, &table, &mods[1..], &built)?;
            note(ok, format!("{fi} then {li}: {li}'s replacement is the one that runs"));
        }
        let mut others: Vec<PathBuf> = std::fs::read_dir(&mods_dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.join("mod.json").is_file() && p != &a && p != &b)
            .collect();
        others.sort();
        for m in others {
            let mods = vec![Mod::load(&m)?];
            let built = build_direct(ws, game, &mods, Some(dir.join(mods[0].id())))?;
            let ok = run_tests(ws, game, &table, &mods, &built)?;
            note(ok, format!("{}: its tests", mods[0].id()));
        }
    }

    // The pinned mods repository: check, build and test, against this
    // psx-wasm.
    if let Some(pin) = ws.lock.mods.get(game) {
        let snap = psxw_inputs::snapshot::ensure(ws, &format!("{game}-mods"), pin)?;
        let m = ModsWorkspace::open(&snap, Some(dir.join("workspace")), Some(ws.root.clone()))?;
        m.check_pins(ws)?;
        let sdk = psxw_modkit::workspace::sdk_into(&m, ws)?;
        let (label, mods) = m.select(m.set("all")?.map(|_| "all"))?;
        let plan = psxw_modkit::plan::plan(&sdk, &mods);
        let clean = plan.ok() && plan.warnings.is_empty();
        note(clean, format!("{} @ {}: check {} ({} mods, {} warnings)", game, pin.short(), if plan.ok() { "ok" } else { "errors" }, mods.len(), plan.warnings.len()));
        let inc = m.state.join("mods");
        let layout = Layout { out: m.build_dir(&label), include: Box::new(move |id| inc.join(id).join("include")), flags: Some(m.flags()?) };
        let built = build::build(ws, &Options::new(game), &mods, &layout)?;
        let ok = run_tests(ws, game, &sdk, &mods, &built)?;
        note(ok, format!("{game} mods @ {}: build and tests", pin.short()));
    }
    Ok((pass, detail.join("; ")))
}
