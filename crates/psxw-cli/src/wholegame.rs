//! `psxw oracle play` and `psxw oracle ramdiff`: the whole-game RAM oracle
//! (plans/completed/M03-oracle-harness.md, tasks 8 and 9).
//!
//! ```text
//! psxw oracle play <game> [--replay R] [--frames N] [--dump-at F,..] [--dump-counter V,..] [--frames-every N] [--check determinism]
//! psxw oracle ramdiff <game> [--replay R] [--frames N] [--every F] [--at V,..] [--require-equal]
//! ```
//!
//! `--replay R` takes a replay file or a named replay: a press list,
//! `games/<game>/replays/R.toml` (the same syntax as `[oracle.replay]`,
//! which is used without `--replay`), or a recording from the browser
//! page, `games/<game>/replays/R.rpl`. Both sides
//! trace the manifest's `[oracle] frame_counter` every frame; `ramdiff`
//! dumps RAM on both at the same counter values (by default the counter at
//! every 600th psx-wasm frame) and compares them by symbol.

use anyhow::{Context, Result, bail, ensure};
use psxw_inputs::Workspace;
use psxw_oracle::play::{self, PlayOptions, Played};
use psxw_oracle::ramdiff::{self, KERNEL, Layout, OverlayLayout, PointDiff};
use serde::Serialize;
use std::path::PathBuf;

pub const USAGE: &str = "play <game> [--replay R] [--frames N] [--dump-at F,..] [--dump-counter V,..] [--frames-every N] [--check determinism] \
                         | ramdiff <game> [--replay R] [--frames N] [--every F] [--at V,..] [--require-equal]";

const DEFAULT_STACK: u32 = 0x1_0000;

fn list<T: std::str::FromStr>(s: Option<&&str>, what: &str) -> Result<Vec<T>> {
    let s = s.with_context(|| format!("{what} needs a value"))?;
    s.split(',').map(|v| v.parse::<T>().map_err(|_| anyhow::anyhow!("{what}: bad value {v}"))).collect()
}

/// The replay to run: a replay file, a named replay
/// (`games/<game>/replays/<name>.toml`), or the manifest's
/// `[oracle.replay]`.
pub fn replay(ws: &Workspace, game: &str, arg: Option<&str>) -> Result<(Vec<u8>, String)> {
    let named = match arg {
        Some(a) => match ws.replay_spec(game, a)? {
            Some(spec) => Some((a.to_string(), spec)),
            // A recorded replay (`games/<game>/replays/<name>.rpl`).
            None if ws.root.join("games").join(game).join("replays").join(format!("{a}.rpl")).is_file() => {
                let p = ws.root.join("games").join(game).join("replays").join(format!("{a}.rpl"));
                let bytes = std::fs::read(&p)?;
                ensure!(psxw_abi::replay::Header::parse(&bytes).is_some(), "{} is not a replay", p.display());
                return Ok((bytes, format!("replays/{a}.rpl")));
            }
            None => {
                let bytes = std::fs::read(a).with_context(|| format!("{a} is neither a replay file nor games/{game}/replays/{a}.toml"))?;
                ensure!(psxw_abi::replay::Header::parse(&bytes).is_some(), "{a} is not a replay");
                return Ok((bytes, a.to_string()));
            }
        },
        None => None,
    };
    let (name, spec) = match named {
        Some(n) => n,
        None => {
            let manifest = ws.game(game)?;
            let spec = manifest.oracle.replay.with_context(|| format!("games/{game}/game.toml has no [oracle.replay]; pass --replay"))?;
            (String::new(), spec)
        }
    };
    // Presses are frame-counter values (the replay's clock), so both runs
    // see them at the same game state whatever their load times.
    let inputs = psxw_lift::run::load(ws, game)?;
    let clock = counter_addr(ws, game, &inputs)?;
    let what = if name.is_empty() { "[oracle.replay]".to_string() } else { format!("replays/{name}.toml") };
    let bytes = play::replay_from_presses(game, spec.frames, &spec.press, clock).map_err(|e| anyhow::anyhow!("{what}: {e}"))?;
    let file = if name.is_empty() { format!("{game}.rpl") } else { format!("{game}-{name}.rpl") };
    let path = ws.out_dir().join("oracle").join("replays").join(file);
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, &bytes)?;
    Ok((bytes, format!("{what} ({})", path.display())))
}

fn frames_of(replay: &[u8]) -> u64 {
    psxw_abi::replay::Header::parse(replay).map_or(0, |h| h.frames as u64)
}

/// The guest address of `[oracle] frame_counter`.
fn counter_addr(ws: &Workspace, game: &str, inputs: &psxw_lift::run::Inputs) -> Result<u32> {
    psxw_build::game::frame_counter(ws, game, inputs)
}

/// Symbols of every container, residency probes for overlays, and the
/// excluded ranges: kernel RAM, the stack below `sp`, manifest exclusions.
fn layout(ws: &Workspace, game: &str, inputs: &psxw_lift::run::Inputs, sp: u32) -> Result<Layout> {
    let manifest = ws.game(game)?;
    let entries = |id: &str| -> Vec<(String, u32, Option<u32>)> {
        inputs.map.symbols(id).iter().map(|s| (s.name.clone(), s.addr, s.size)).collect()
    };
    let main = ramdiff::syms(entries(&inputs.map.main));
    let mut overlays = Vec::new();
    for c in inputs.map.containers.iter().filter(|c| !c.is_main) {
        let Some(t) = c.text.first() else { continue };
        let Some(probe) = inputs.originals.read(&c.id, t.start, 16) else { continue };
        let lo = c.segments.iter().map(|(_, v)| *v).min().unwrap_or(c.vram.start);
        let hi = c.segments.iter().map(|(f, v)| v + (f.end - f.start)).max().unwrap_or(c.vram.end);
        overlays.push(OverlayLayout {
            id: c.id.clone(),
            range: (lo & 0x1F_FFFF)..(hi & 0x1F_FFFF),
            probe_at: t.start & 0x1F_FFFF,
            probe: probe.to_vec(),
            syms: ramdiff::syms(entries(&c.id)),
        });
    }
    let mut excluded = vec![(KERNEL, "kernel RAM: the HLE BIOS and OpenBIOS keep different state there".to_string())];
    let stack = manifest.oracle.stack_size.unwrap_or(DEFAULT_STACK);
    let top = sp & 0x1F_FFFF;
    excluded.push((top.saturating_sub(stack)..(top + 0x100).min(ramdiff::RAM_SIZE as u32), format!("stack ({stack:#x} bytes below sp {sp:#010x})")));
    for (name, reason) in &manifest.oracle.exclude_symbols {
        let sym = main.iter().chain(overlays.iter().flat_map(|o| o.syms.iter())).find(|s| &s.name == name);
        let sym = sym.with_context(|| format!("[oracle] exclude_symbols: {name} is not a known symbol"))?;
        excluded.push((sym.range.clone(), format!("{name}: {reason}")));
    }
    Ok(Layout { main, overlays, excluded })
}

fn emu_disc(ws: &Workspace, game: &str) -> Result<PathBuf> {
    ws.disc(game)
}

struct PlayArgs {
    replay: Option<String>,
    frames: Option<u64>,
    dump_at: Vec<u64>,
    dump_counter: Vec<u32>,
    frames_every: Option<u64>,
    every: u64,
    at: Vec<u32>,
    determinism: bool,
    require_equal: bool,
}

fn parse(args: &[&str]) -> Result<PlayArgs> {
    let mut a = PlayArgs {
        replay: None,
        frames: None,
        dump_at: vec![],
        dump_counter: vec![],
        frames_every: None,
        every: 600,
        at: vec![],
        determinism: false,
        require_equal: false,
    };
    let mut it = args.iter();
    while let Some(k) = it.next() {
        match *k {
            "--replay" => a.replay = Some(it.next().context("--replay needs a value")?.to_string()),
            "--frames" => a.frames = Some(list::<u64>(it.next(), "--frames")?[0]),
            "--dump-at" => a.dump_at = list(it.next(), "--dump-at")?,
            "--dump-counter" => a.dump_counter = list(it.next(), "--dump-counter")?,
            "--frames-every" => a.frames_every = Some(list::<u64>(it.next(), "--frames-every")?[0]),
            "--every" => a.every = list::<u64>(it.next(), "--every")?[0],
            "--at" => a.at = list(it.next(), "--at")?,
            "--check" => match it.next() {
                Some(&"determinism") => a.determinism = true,
                other => bail!("unknown check {other:?} (determinism)"),
            },
            "--require-equal" => a.require_equal = true,
            other => bail!("unknown option {other}"),
        }
    }
    Ok(a)
}

fn emu_play(ws: &Workspace, game: &str, replay: &[u8], frames: u64, trace: u32, a: &PlayArgs, dir: PathBuf) -> Result<Played> {
    let o = PlayOptions {
        disc: emu_disc(ws, game)?,
        replay: replay.to_vec(),
        frames,
        trace_u32: Some(trace),
        dump_at: a.dump_at.clone(),
        dump_counter: a.dump_counter.clone(),
        frames_every: a.frames_every,
        out_dir: dir,
    };
    let t = std::time::Instant::now();
    let p = play::play(ws, &o)?;
    ppm_to_png(&p.out_dir.join("frames"))?;
    eprintln!("play: {} frames in {:.1}s; {} dumps in {}", p.run.frames, t.elapsed().as_secs_f64(), p.run.dumps.len(), p.out_dir.display());
    Ok(p)
}

/// The host's `frame_*.ppm` pictures in `dir` as PNGs, like the native
/// host's `--dump-frames`.
pub fn ppm_to_png(dir: &std::path::Path) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Ok(()) };
    for e in entries {
        let p = e?.path();
        if p.extension().is_none_or(|x| x != "ppm") {
            continue;
        }
        let b = std::fs::read(&p)?;
        let mut fields = b.splitn(4, |c| c.is_ascii_whitespace());
        let (Some(b"P6"), Some(w), Some(h), Some(rest)) = (fields.next(), fields.next(), fields.next(), fields.next()) else {
            bail!("{}: not a binary PPM", p.display());
        };
        let (w, h): (u32, u32) = (std::str::from_utf8(w)?.parse()?, std::str::from_utf8(h)?.parse()?);
        let rgb = &rest[rest.iter().position(|c| c.is_ascii_whitespace()).context("PPM header")? + 1..];
        ensure!(rgb.len() == (w * h * 3) as usize, "{}: {} pixel bytes for {w}x{h}", p.display(), rgb.len());
        let rgba: Vec<u8> = rgb.chunks_exact(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect();
        std::fs::write(p.with_extension("png"), psxw_host_native::render::png(w, h, &rgba))?;
        std::fs::remove_file(&p)?;
    }
    Ok(())
}

/// `psxw oracle play`: exit criterion 4 with `--check determinism`.
pub fn play_cmd(ws: &Workspace, game: &str, args: &[&str]) -> Result<bool> {
    let a = parse(args)?;
    let (replay, label) = replay(ws, game, a.replay.as_deref())?;
    let frames = a.frames.unwrap_or(frames_of(&replay));
    let inputs = psxw_lift::run::load(ws, game)?;
    let trace = counter_addr(ws, game, &inputs)?;
    let root = ws.out_dir().join("oracle").join("play").join(game);
    eprintln!("play: {game}, replay {label}, {frames} frames, counter at {trace:#010x}");
    let one = emu_play(ws, game, &replay, frames, trace, &a, root.join("run-1"))?;
    for d in &one.run.dumps {
        println!("dump {:<22} frame {:>6} counter {:>8} sha1 {}", d.file, d.frame, d.counter.map_or("-".into(), |c| c.to_string()), d.sha1);
    }
    println!("counter at the last frame: {:?}", one.trace.last());
    if !a.determinism {
        return Ok(true);
    }
    let two = emu_play(ws, game, &replay, frames, trace, &a, root.join("run-2"))?;
    let same = one.run.dumps == two.run.dumps && one.trace == two.trace;
    println!(
        "check determinism {} ({} dumps, {} trace words)",
        if same { "ok" } else { "FAIL" },
        one.run.dumps.len(),
        one.trace.len()
    );
    Ok(same)
}

#[derive(Serialize)]
struct RamdiffReport {
    schema_version: u32,
    game: String,
    replay: String,
    frames: u64,
    frame_counter: String,
    excluded: Vec<String>,
    points: Vec<Point>,
    equal: bool,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Point {
    Compared(PointDiff),
    Missing { counter: u32, frame_wasm: Option<u64>, frame_emu: Option<u64> },
}

/// `psxw oracle ramdiff`: exit criterion 5 (equality from M4 on, with
/// `--require-equal`).
pub fn ramdiff_cmd(ws: &Workspace, game: &str, args: &[&str]) -> Result<bool> {
    let a = parse(args)?;
    let (replay, label) = replay(ws, game, a.replay.as_deref())?;
    let frames = a.frames.unwrap_or(frames_of(&replay));
    let inputs = psxw_lift::run::load(ws, game)?;
    let trace = counter_addr(ws, game, &inputs)?;
    let root = ws.out_dir().join("oracle").join("ramdiff").join(game);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;

    // psx-wasm: a traced run picks the points, a second one dumps there.
    let (mut o, _) = crate::run::options(ws, game, &[])?;
    o.frames = frames;
    o.replay = Some(replay.clone());
    o.trace_u32 = Some(trace);
    let first = psxw_host_native::run(o.clone())?;
    ensure!(
        matches!(first.stop, psxw_host_native::Stop::FrameLimit),
        "psx-wasm stopped early: {:?} (frame {})",
        first.stop,
        first.frames
    );
    let values = if a.at.is_empty() { ramdiff::points(&first.trace, a.every) } else { a.at.clone() };
    ensure!(!values.is_empty(), "the frame counter never left zero in {frames} frames; nothing to align on");
    o.dump_counter = values.clone();
    o.dump_dir = Some(root.join("wasm"));
    let wasm = psxw_host_native::run(o)?;
    // Same layout as the emulator's trace.bin: one u32 LE per frame.
    std::fs::write(root.join("wasm").join("trace.bin"), wasm.trace.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<_>>())?;
    let sp = wasm.boot.as_ref().and_then(|b| b["sp"].as_u64()).context("the run reported no boot sp")? as u32;
    eprintln!("ramdiff: psx-wasm traced {} frames; aligning on counter values {values:?}", wasm.trace.len());

    let emu_args = PlayArgs { dump_counter: values.clone(), dump_at: vec![], ..a };
    let emu = emu_play(ws, game, &replay, frames, trace, &emu_args, root.join("emu"))?;

    let layout = layout(ws, game, &inputs, sp)?;
    let mut points = Vec::new();
    let mut equal = true;
    for (v, fw, fe) in ramdiff::align(&wasm.trace, &emu.trace, &values) {
        let dw = wasm.dumps.iter().find(|d| d.counter == Some(v));
        let de = emu.run.dumps.iter().find(|d| d.counter == Some(v));
        match (dw, de) {
            (Some(dw), Some(de)) => {
                let ra = play::read_dump(&root.join("wasm"), &dw.file)?;
                let rb = play::read_dump(&emu.out_dir, &de.file)?;
                let p = ramdiff::diff(&layout, v, dw.frame, &ra, de.frame, &rb);
                println!(
                    "counter {v:>8}: psx-wasm frame {:>6}, emulator frame {:>6}: {} bytes differ in {} symbols ({} excluded)",
                    p.frame_a, p.frame_b, p.differing_bytes, p.differing_symbols, p.excluded_differing_bytes
                );
                for s in p.symbols.iter().take(8) {
                    println!("    {:<10} {:<40} {} +{:<6} {:>6} bytes, first at {}", s.container, s.name, s.addr, s.size, s.bytes, s.first);
                }
                equal &= p.differing_bytes == 0;
                points.push(Point::Compared(p));
            }
            _ => {
                println!("counter {v:>8}: not reached (psx-wasm frame {fw:?}, emulator frame {fe:?})");
                equal = false;
                points.push(Point::Missing { counter: v, frame_wasm: fw, frame_emu: fe });
            }
        }
    }
    let report = RamdiffReport {
        schema_version: 1,
        game: game.into(),
        replay: label,
        frames,
        frame_counter: format!("{trace:#010x}"),
        excluded: layout.excluded.iter().map(|(r, why)| format!("{:#x}..{:#x}: {why}", r.start, r.end)).collect(),
        points,
        equal,
    };
    let json = root.join("report.json");
    std::fs::write(&json, serde_json::to_string_pretty(&report)?)?;
    eprintln!("ramdiff: {} ({})", if equal { "RAM equal at every point" } else { "differences" }, json.display());
    Ok(equal || !a.require_equal)
}
