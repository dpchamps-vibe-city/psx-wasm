//! `psxw conform <game> --modes [--replay R]`: conformance level 4 (D59).
//! The game's replay runs twice: as built (each chip native where it can
//! be) and with every chip on its device (`--device all`). Both runs must
//! pass the replay's checks (its `expect` lines, no trap, no shadow I/O)
//! and reach every checkpoint. RAM is compared by symbol at the same
//! frame-counter values, as `psxw oracle ramdiff` does against the
//! emulator, and reported; it is not gated, because the two modes may
//! legitimately keep library state in different places (adapters in the
//! runtime, the original in RAM) and settle a frame apart at a sync point.

use anyhow::{Context, Result};
use psxw_inputs::Workspace;
use psxw_oracle::{play, ramdiff};
use serde::Serialize;

#[derive(Serialize)]
struct ModeRun {
    mode: String,
    wasm: String,
    emulated: String,
    pass: bool,
    checks: Vec<crate::run::Check>,
    /// What the run sent the host: renderer commands, displays, audio
    /// (hashes over the whole run), and the frame counter at the end.
    output: Output,
}

#[derive(Serialize)]
struct ModesReport {
    schema_version: u32,
    game: String,
    replay: String,
    pass: bool,
    /// The renderer commands, displays, audio and final frame counter are
    /// the same in both modes.
    same_output: bool,
    runs: Vec<ModeRun>,
    points: Vec<ramdiff::PointDiff>,
    unreached: Vec<u32>,
}

pub fn run(ws: &Workspace, game: &str, replay: Option<&str>) -> Result<bool> {
    let manifest = ws.game(game)?;
    // The replay: given, else the CI one, else the milestones' title-to-gameplay.
    let fallback = ws.root.join("games").join(game).join("replays").join("title-to-gameplay.toml").is_file().then(|| "title-to-gameplay".to_string());
    let name = replay.map(str::to_string).or(manifest.ci.replay.clone()).or(fallback).context("no replay: pass --replay or set [ci] replay")?;
    let spec = ws.replay_spec(game, &name)?.with_context(|| format!("games/{game}/replays/{name}.toml not found"))?;
    let inputs = psxw_lift::run::load(ws, game)?;
    let counter = crate::wholegame::counter_addr(ws, game, &inputs)?;
    // Checkpoints: counter values a first traced run (native) reaches, one
    // every 600 frames (dumps happen where the counter equals a value).
    let (native, _) = psxw_build::game::build(ws, &psxw_build::game::Options::new(game))?;
    let (traced, _) = crate::run::evaluate_with(ws, game, &["--replay", &name, "--wasm", &native.wasm], |o| o.trace_u32 = Some(counter))?;
    let mut points = ramdiff::points(&traced.trace, 600);
    points.sort();
    points.dedup();
    let _ = spec;
    let root = ws.out_dir().join("conform").join(game).join("modes");
    let _ = std::fs::remove_dir_all(&root);
    let mut runs = Vec::new();
    let mut dumps = Vec::new();
    for (mode, device) in [("native", vec![]), ("device", vec!["all".to_string()])] {
        let opts = psxw_build::game::Options { device, ..psxw_build::game::Options::new(game) };
        let (built, _) = psxw_build::game::build(ws, &opts)?;
        let dir = root.join(mode);
        let (points, dir2) = (points.clone(), dir.clone());
        let (r, checks) = crate::run::evaluate_with(ws, game, &["--replay", &name, "--wasm", &built.wasm, "--check", "no-trap", "--check", "no-shadow-io"], move |o| {
            o.trace_u32 = Some(counter);
            o.dump_counter = points;
            o.dump_dir = Some(dir2);
        })?;
        let pass = checks.iter().all(|c| c.pass);
        eprintln!("modes: {game} {mode} ({}): {}", built.emulated, if pass { "replay checks pass" } else { "replay checks FAIL" });
        for c in checks.iter().filter(|c| !c.pass) {
            eprintln!("  {}: {}", c.name, c.detail);
        }
        let output = Output {
            render: format!("{:016x} ({} words)", r.render.hash, r.render.words),
            displays: format!("{:016x}", r.render.present_hash),
            audio: format!("{} ({} frames)", r.audio.hash, r.audio.frames),
            counter: r.trace.last().copied().unwrap_or(0),
        };
        dumps.push((dir, r));
        runs.push(ModeRun { mode: mode.into(), wasm: built.wasm, emulated: built.emulated, pass, checks, output });
    }
    let sp = dumps[0].1.boot.as_ref().and_then(|b| b["sp"].as_u64()).context("the run reported no boot sp")? as u32;
    let layout = crate::wholegame::layout(ws, game, &inputs, sp)?;
    let mut diffs = Vec::new();
    let mut unreached = Vec::new();
    for &v in &points {
        let (da, db) = (&dumps[0], &dumps[1]);
        let (Some(a), Some(b)) = (da.1.dumps.iter().find(|d| d.counter == Some(v)), db.1.dumps.iter().find(|d| d.counter == Some(v))) else {
            unreached.push(v);
            continue;
        };
        let ra = play::read_dump(&da.0, &a.file)?;
        let rb = play::read_dump(&db.0, &b.file)?;
        let p = ramdiff::diff(&layout, v, a.frame, &ra, b.frame, &rb);
        println!(
            "counter {v:>8}: native frame {:>6}, device frame {:>6}: {} bytes differ in {} symbols",
            p.frame_a, p.frame_b, p.differing_bytes, p.differing_symbols
        );
        for s in p.symbols.iter().take(6) {
            println!("    {:<10} {:<40} {} {:>6} bytes", s.container, s.name, s.addr, s.bytes);
        }
        diffs.push(p);
    }
    let pass = runs.iter().all(|r| r.pass) && unreached.is_empty();
    let same_output = runs[0].output == runs[1].output;
    for (r, o) in runs.iter().map(|r| (&r.mode, &r.output)) {
        println!("{r:>7}: render {}, displays {}, audio {}, counter {}", o.render, o.displays, o.audio, o.counter);
    }
    println!("output: {}", if same_output { "identical in both modes" } else { "differs between the modes" });
    let report = ModesReport { schema_version: 1, game: game.into(), replay: name, pass, same_output, runs, points: diffs, unreached };
    std::fs::create_dir_all(&root)?;
    let json = root.join("report.json");
    std::fs::write(&json, serde_json::to_string_pretty(&report)?)?;
    println!("conform {game} modes: {} ({})", if pass { "PASS" } else { "FAIL" }, json.display());
    Ok(pass)
}

#[derive(Serialize, PartialEq, Eq)]
struct Output {
    render: String,
    displays: String,
    audio: String,
    counter: u32,
}
