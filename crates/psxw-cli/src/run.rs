//! `psxw run <game>`: run `out/<game>/game.wasm` headless on the user's
//! disc, write `out/run/<game>/run.json`, and evaluate checks:
//!
//! - `--check boot` (M2 exit criterion 2): the loaded EXE equals the disc's,
//!   every sector passes EDC, the disc matches its pin, every overlay load
//!   is byte-exact, no trap, at least 600 VSync waits, the main loop
//!   entered;
//! - `--check determinism` (criterion 3; M6 task 11): two runs, identical
//!   RAM hashes at frames 60, 300 and 600, and identical audio (frames and
//!   hash);
//! - `--check no-trap`: the run reaches its frame limit (the UBSan gate,
//!   with `--wasm out/<game>/game-ubsan.wasm`).

use anyhow::{Context as _, Result, bail, ensure};
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
            "--replay" => o.replay = Some(crate::wholegame::replay(ws, game, Some(&val()?))?.0),
            "--cd" => o.boot.cd_timing = if val()? == "instant" { 1 } else { 0 },
            "--hash-at" => o.hash_at = val()?.split(',').map(str::parse).collect::<Result<_, _>>()?,
            "--dump-at" => o.dump_at = val()?.split(',').map(str::parse).collect::<Result<_, _>>()?,
            "--dump-dir" => o.dump_dir = Some(val()?.into()),
            "--trace-u32" => o.trace_u32 = psxw_inputs::parse_u32(&val()?),
            "--wasm" => o.wasm = val()?.into(),
            // `psxw build <game> --debug`'s module.
            "--debug" => o.wasm = psxw_build::game::out_dir(ws, game).join("game-debug.wasm"),
            // A moddable base's mods (plans/M11 phase 2).
            "--load-mod" => o.mods.push(val()?.into()),
            "--swap-mod" => {
                let v = val()?;
                let mut p = v.splitn(3, ':');
                let (Some(f), Some(i), Some(path)) = (p.next(), p.next(), p.next()) else { bail!("--swap-mod FRAME:INDEX:PATH") };
                o.swaps.push(psxw_host_native::ModSwap { frame: f.parse()?, index: i.parse()?, path: path.into() });
            }
            // A call into the game at a vblank: FRAME:FUNC[:ARG,ARG..],
            // FUNC an 0x address or a loaded mod's global function.
            "--execute" => {
                let v = val()?;
                let mut p = v.splitn(3, ':');
                let (Some(f), Some(function)) = (p.next(), p.next()) else { bail!("--execute FRAME:FUNC[:ARG,ARG..]") };
                let args = p.next().map_or(Ok(vec![]), |a| a.split(',').map(|w| psxw_inputs::parse_u32(w).with_context(|| format!("bad argument {w}"))).collect::<Result<Vec<_>>>())?;
                ensure!(args.len() <= 4, "--execute takes up to four argument words");
                o.execute.push(psxw_host_native::Execute { frame: f.parse()?, function: function.into(), args });
            }
            "--check" => checks.push(val()?),
            "--verbose" => o.verbose = true,
            "--render" => {
                if val()? == "server" {
                    o.render = Some(crate::render::server(ws));
                }
            }
            "--vram-hash-at" => o.vram_hash_at = val()?.split(',').map(str::parse).collect::<Result<_, _>>()?,
            "--dump-frames" => o.dump_frames = Some((val()?.into(), 1)),
            "--audio-wav" => o.audio_wav = Some(val()?.into()),
            "--dump-every" => {
                let n: u64 = val()?.parse()?;
                if let Some(d) = &mut o.dump_frames {
                    d.1 = n;
                }
            }
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
    for e in r.events.iter().filter(|e| e["event"] == "mod-function") {
        s += &format!("mod function {} at {}\n", e["name"].as_str().unwrap_or_default(), e["address"].as_str().unwrap_or_default());
    }
    for e in r.events.iter().filter(|e| e["event"] == "execute") {
        s += &format!("execute #{} returned {}\n", e["id"], e["value"].as_str().unwrap_or_default());
    }
    for l in r.logs.iter().rev().take(10).collect::<Vec<_>>().into_iter().rev() {
        s += &format!("  log {l}\n");
    }
    s
}

#[derive(Serialize)]
pub struct Check {
    pub name: &'static str,
    pub pass: bool,
    pub detail: String,
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
    // Calls to libetc's VSync (a build probe): at least one per two frames.
    let inputs = psxw_lift::run::load(ws, game)?;
    let vsync = inputs.map.resolve(&inputs.map.main, "VSync").map(|r| r.addr);
    let waits = vsync.map_or(0, |a| count(r, &format!("probe.{a:08X}")));
    v.push(Check {
        name: "vsync-waits",
        pass: waits >= 600,
        detail: if vsync.is_some() { format!("{waits} VSync calls") } else { "the map names no VSync".into() },
    });
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

/// Runs the game with `args` and evaluates the requested checks; writes
/// `out/run/<game>/run.json` (and `checks.json` when there are checks).
pub fn evaluate(ws: &Workspace, game: &str, args: &[&str]) -> Result<(RunReport, Vec<Check>)> {
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
    std::fs::write(dir.join("run.json"), serde_json::to_string_pretty(&r)?)?;
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
        let (a, b) = (&r.audio, &again.audio);
        results.push(Check {
            name: "audio-determinism",
            pass: a.frames == b.frames && a.hash == b.hash && a.frames > 0,
            detail: format!("{} frames {} vs {} frames {}", a.frames, a.hash, b.frames, b.hash),
        });
    }
    if checks.iter().any(|c| c == "no-trap") {
        results.push(Check { name: "no-trap", pass: r.stop == Stop::FrameLimit, detail: format!("{:?}", r.stop) });
    }
    if checks.iter().any(|c| c == "card") {
        results.extend(card_checks(&o)?);
    }
    for c in &checks {
        if !["boot", "determinism", "no-trap", "card"].contains(&c.as_str()) {
            bail!("unknown check {c} (boot, determinism, no-trap, card)");
        }
    }
    if !results.is_empty() {
        std::fs::write(dir.join("checks.json"), serde_json::to_string_pretty(&results)?)?;
    }
    Ok((r, results))
}

/// `--check card` (M7 exit criterion 1): every memory card image in the
/// run's storage validates (the "MC" header and every directory frame's
/// checksum), and the files on it are listed.
fn card_checks(o: &Options) -> Result<Vec<Check>> {
    let Some(dir) = &o.storage_dir else { return Ok(vec![]) };
    let mut out = Vec::new();
    for name in ["card0.mcr", "card1.mcr"] {
        let Ok(bytes) = std::fs::read(dir.join(name)) else { continue };
        let card = psx_devices::memcard::Card::from_image(&bytes);
        let files: Vec<String> = card.iter().flat_map(|c| c.files()).filter(|f| !f.deleted).map(|f| format!("{} ({} bytes)", f.name, f.size)).collect();
        out.push(Check { name: "card", pass: card.is_some_and(|c| c.valid()), detail: format!("{name}: {}", if files.is_empty() { "no files".to_string() } else { files.join(", ") }) });
    }
    Ok(out)
}

pub fn run(ws: &Workspace, game: &str, args: &[&str]) -> Result<bool> {
    let (r, results) = evaluate(ws, game, args)?;
    print!("{}", summary(&r));
    eprintln!("full report: {}", ws.out_dir().join("run").join(game).join("run.json").display());
    let mut ok = matches!(r.stop, Stop::FrameLimit | Stop::Exited);
    for c in &results {
        println!("check {:<12} {:<4} {}", c.name, if c.pass { "ok" } else { "FAIL" }, c.detail);
        ok &= c.pass;
    }
    Ok(ok)
}
