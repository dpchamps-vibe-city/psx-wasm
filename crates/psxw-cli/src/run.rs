//! `psxw run <game>`: run `out/<game>/game.wasm` headless on the user's
//! disc, write `out/run/<game>/run.json`, and evaluate checks:
//!
//! - `--check boot` (M2 exit criterion 2): the loaded EXE equals the disc's,
//!   every sector passes EDC, the disc matches its pin, every overlay load
//!   is byte-exact, no trap, at least 600 VSync waits, the main loop
//!   entered;
//! - `--check determinism` (criterion 3): two runs, identical RAM hashes at
//!   frames 60, 300 and 600.

use anyhow::{Result, bail};
use psxw_host_native::{Options, Overlay, RunReport, Stop};
use psxw_inputs::Workspace;
use serde::Serialize;

pub fn options(ws: &Workspace, game: &str, args: &[&str]) -> Result<(Options, Vec<String>)> {
    let manifest = ws.game(game)?;
    let mut o = Options {
        wasm: psxw_build::game::game_path(ws, game),
        disc: ws.disc(game)?,
        frames: 600,
        storage_dir: Some(ws.out_dir().join("run").join(game).join("storage")),
        ..Default::default()
    };
    if manifest.run.cd_timing.as_deref() == Some("instant") {
        o.boot.cd_timing = 1;
    }
    let mut checks = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().map(|s| s.to_string()).ok_or_else(|| anyhow::anyhow!("{a} needs a value"));
        match *a {
            "--headless" => {}
            "--frames" => o.frames = val()?.parse()?,
            "--replay" => o.replay = Some(std::fs::read(val()?)?),
            "--cd" => o.boot.cd_timing = if val()? == "instant" { 1 } else { 0 },
            "--hash-at" => o.hash_at = val()?.split(',').map(str::parse).collect::<Result<_, _>>()?,
            "--dump-at" => o.dump_at = val()?.split(',').map(str::parse).collect::<Result<_, _>>()?,
            "--dump-dir" => o.dump_dir = Some(val()?.into()),
            "--trace-u32" => o.trace_u32 = psxw_inputs::parse_u32(&val()?),
            "--wasm" => o.wasm = val()?.into(),
            "--check" => checks.push(val()?),
            "--verbose" => o.verbose = true,
            other => bail!("unknown run option {other}"),
        }
    }
    Ok((o, checks))
}

pub fn summary(r: &RunReport) -> String {
    let mut s = format!("stop: {:?} after {} frames\n", r.stop, r.frames);
    if let Some(b) = &r.boot {
        s += &format!("boot: {} (text matches disc: {:?})\n", b["exe"], r.boot_text_matches);
    }
    if let Some(rep) = &r.report {
        s += &format!(
            "vblanks {}, cd sectors {}, edc errors {}\n",
            rep["vblanks"],
            rep["cd_sectors"],
            rep["edc_errors"].as_array().map_or(0, Vec::len)
        );
    }
    for l in r.logs.iter().rev().take(10).collect::<Vec<_>>().into_iter().rev() {
        s += &format!("  log {l}\n");
    }
    s
}

#[derive(Serialize)]
struct Check {
    name: &'static str,
    pass: bool,
    detail: String,
}

fn count(r: &RunReport, key: &str) -> u64 {
    r.report.as_ref().and_then(|v| v["counts"][key].as_u64()).unwrap_or(0)
}

fn overlays(ws: &Workspace, game: &str) -> Result<Vec<Overlay>> {
    let inputs = psxw_lift::run::load(ws, game)?;
    let mut out = Vec::new();
    for c in inputs.map.containers.iter().filter(|c| !c.is_main) {
        let text: Vec<(u32, Vec<u8>)> = c
            .text
            .iter()
            .filter_map(|r| inputs.originals.read(&c.id, r.start, (r.end - r.start) as usize).map(|b| (r.start, b.to_vec())))
            .collect();
        if !text.is_empty() {
            out.push(Overlay { container: c.id.clone(), text });
        }
    }
    Ok(out)
}

/// The disc's file sha1s against `inputs.lock`; prints the entry to add
/// when the disc isn't pinned yet.
fn disc_pin(ws: &Workspace, game: &str, o: &Options) -> Result<Check> {
    let manifest = ws.game(game)?;
    let disc = psxw_host_native::disc::Disc::open(&o.disc)?;
    let got = disc.file_sha1s()?;
    let pinned = ws.game_pin(&manifest)?.disc.as_ref().map(|d| d.files.clone()).unwrap_or_default();
    if pinned.is_empty() {
        let mut entry = format!("[[game.{}.disc.files]]\n", manifest.game.input);
        for (n, h) in &got {
            entry += &format!("name = {n:?}\nsha1 = {h:?}\n");
        }
        eprintln!("the disc is not pinned; add to inputs.lock under [game.{}.disc]:\n{entry}", manifest.game.input);
        return Ok(Check { name: "disc-pinned", pass: false, detail: "not pinned yet".into() });
    }
    let same = pinned.len() == got.len() && pinned.iter().zip(&got).all(|(p, (n, h))| p.name == *n && p.sha1 == *h);
    Ok(Check { name: "disc-pinned", pass: same, detail: format!("{} file(s)", got.len()) })
}

fn boot_checks(ws: &Workspace, game: &str, o: &Options, r: &RunReport) -> Result<Vec<Check>> {
    let manifest = ws.game(game)?;
    let mut v = vec![
        Check {
            name: "exe-loaded",
            pass: r.boot_text_matches == Some(true),
            detail: format!("{:?}", r.boot.as_ref().map(|b| b["exe"].clone())),
        },
        disc_pin(ws, game, o)?,
    ];
    let edc = r.report.as_ref().and_then(|v| v["edc_errors"].as_array().cloned()).unwrap_or_default();
    let sectors = r.report.as_ref().and_then(|v| v["cd_sectors"].as_u64()).unwrap_or(0);
    v.push(Check { name: "edc", pass: edc.is_empty() && sectors > 0, detail: format!("{sectors} sectors, {} errors", edc.len()) });
    let bad: Vec<String> =
        r.events.iter().filter(|e| e["event"] == "overlay-mismatch").map(|e| e["container"].to_string()).collect();
    let loaded = r.events.iter().filter(|e| e["event"] == "overlay-resident").count();
    v.push(Check { name: "overlays", pass: bad.is_empty(), detail: format!("{loaded} loads verified; mismatches {bad:?}") });
    v.push(Check { name: "no-trap", pass: r.stop == Stop::FrameLimit, detail: format!("{:?}", r.stop) });
    let waits = count(r, "vsync.waits");
    v.push(Check { name: "vsync-waits", pass: waits >= 600, detail: format!("{waits}") });
    let main_loop = match &manifest.run.main_loop_symbol {
        Some(sym) => {
            let inputs = psxw_lift::run::load(ws, game)?;
            let addr = inputs.map.resolve(&inputs.map.main, sym).map(|r| r.addr);
            let n = addr.map_or(0, |a| count(r, &format!("probe.{a:08X}")));
            Check { name: "main-loop", pass: n > 0, detail: format!("{sym}: entered {n} times") }
        }
        None => Check { name: "main-loop", pass: false, detail: "game.toml [run] main_loop_symbol unset".into() },
    };
    v.push(main_loop);
    Ok(v)
}

pub fn run(ws: &Workspace, game: &str, args: &[&str]) -> Result<bool> {
    let (mut o, checks) = options(ws, game, args)?;
    if checks.iter().any(|c| c == "boot") {
        o.overlays = overlays(ws, game)?;
        o.frames = o.frames.max(1200);
    }
    if checks.iter().any(|c| c == "determinism") {
        o.frames = o.frames.max(600);
        for f in [60, 300, 600] {
            if !o.hash_at.contains(&f) {
                o.hash_at.push(f);
            }
        }
    }
    let r = psxw_host_native::run(o.clone())?;
    let dir = ws.out_dir().join("run").join(game);
    std::fs::create_dir_all(&dir)?;
    let json = dir.join("run.json");
    std::fs::write(&json, serde_json::to_string_pretty(&r)?)?;
    print!("{}", summary(&r));
    eprintln!("full report: {}", json.display());
    let mut results = Vec::new();
    if checks.iter().any(|c| c == "boot") {
        results.extend(boot_checks(ws, game, &o, &r)?);
    }
    if checks.iter().any(|c| c == "determinism") {
        let again = psxw_host_native::run(o.clone())?;
        let same = r.ram_hashes == again.ram_hashes && r.ram_hashes.len() >= 3;
        results.push(Check {
            name: "determinism",
            pass: same,
            detail: format!("{:?} vs {:?}", r.ram_hashes, again.ram_hashes),
        });
    }
    let mut ok = matches!(r.stop, Stop::FrameLimit | Stop::Exited);
    for c in &results {
        println!("check {:<12} {:<4} {}", c.name, if c.pass { "ok" } else { "FAIL" }, c.detail);
        ok &= c.pass;
    }
    if !results.is_empty() {
        std::fs::write(dir.join("checks.json"), serde_json::to_string_pretty(&results)?)?;
    }
    Ok(ok)
}
