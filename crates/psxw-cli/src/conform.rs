//! `psxw conform <game> --lib L`: adapter conformance (docs/design.md
//! "Oracles from the original code"; M4 tasks 7-9). The same scenario of
//! SDK calls and vblanks runs twice without a disc: on `game.wasm`, where
//! the library is adapters, and on `game-orig-L.wasm`, where it is the
//! game's own translated library talking to register-level devices
//! (`psx-bios::io`). What the game can observe after each step (return
//! values, the SDK buffers it reads) must be identical.
//!
//! ```text
//! psxw conform <game> --lib libpad [--frames N]
//! ```

use anyhow::{Context, Result, bail};
use psxw_host_native::{Conform, Options};
use psxw_inputs::Workspace;
use serde::Serialize;

pub const USAGE: &str = "conform <game> --lib libpad|libgpu|libmcrd [--frames N] [--render server] [--dump-frames DIR]";

/// Libraries with a scenario (the `psxw ci` gate runs each).
pub const LIBRARIES: &[&str] = &["libpad", "libgpu", "libmcrd"];

/// Free RAM for scenario buffers: below the stack, above any game data.
const SCRATCH: u32 = 0x801C_0000;

/// One step's observations.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct Observation {
    step: String,
    values: Vec<String>,
}

struct Side {
    c: Conform,
    log: Vec<Observation>,
}

struct Ctx<'a> {
    inputs: &'a psxw_lift::run::Inputs,
}

impl Ctx<'_> {
    fn addr(&self, name: &str) -> Result<u32> {
        let r = self.inputs.map.resolve(&self.inputs.map.main, name).with_context(|| format!("{name} is not in the main container"))?;
        Ok(r.addr)
    }
}

impl Side {
    fn call(&mut self, ctx: &Ctx, name: &str, args: [u32; 4]) -> Result<u32> {
        let v = self.c.call(ctx.addr(name)?, args)? as u32;
        self.log.push(Observation { step: format!("{name}({:#x}, {:#x}, {:#x}, {:#x})", args[0], args[1], args[2], args[3]), values: vec![format!("{v:#x}")] });
        Ok(v)
    }

    fn observe(&mut self, step: String, values: Vec<String>) {
        self.log.push(Observation { step, values });
    }

    /// Ends the run, writing its report (counters, recent I/O) next to
    /// the conformance report; keeps the observations.
    fn finish(self, dir: &std::path::Path, which: &str) -> Result<Finished> {
        let r = self.c.finish();
        std::fs::write(dir.join(format!("run-{which}.json")), serde_json::to_string_pretty(&r)?)?;
        Ok(Finished { log: self.log })
    }
}

struct Finished {
    log: Vec<Observation>,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// libpad: PadInitDirect, PadStartCom, then each frame the two buffers,
/// PadGetState and the current ID for both ports; PadStopCom.
fn libpad(ctx: &Ctx, s: &mut Side, frames: u32) -> Result<()> {
    let (b0, b1) = (SCRATCH, SCRATCH + 0x40);
    s.c.write(b0, &[0xAA; 0x80]);
    s.call(ctx, "PadInitDirect", [b0, b1, 0, 0])?;
    s.observe("after PadInitDirect".into(), vec![hex(&s.c.read(b0, 8)), hex(&s.c.read(b1, 8))]);
    s.call(ctx, "PadStartCom", [0; 4])?;
    for f in 0..frames {
        s.c.run_vblanks(1)?;
        let mut v = vec![hex(&s.c.read(b0, 8)), hex(&s.c.read(b1, 8))];
        for port in [0x00, 0x10] {
            v.push(format!("state {:#x}", s.c.call(ctx.addr("PadGetState")?, [port, 0, 0, 0])? as u32));
            v.push(format!("id {:#x}", s.c.call(ctx.addr("PadInfoMode")?, [port, 1, 0, 0])? as u32));
        }
        s.observe(format!("frame {f}"), v);
    }
    s.call(ctx, "PadStopCom", [0; 4])?;
    Ok(())
}

/// libgpu (a device library, D42): reset, display and draw environments
/// (with a background fill), an ordering table with one tile, a VRAM
/// readback, the display mask, then vblanks with the field. With a real
/// renderer the readback shows the fill and the tile.
fn libgpu(ctx: &Ctx, s: &mut Side, frames: u32) -> Result<()> {
    use sdk_psyq::layout::{dispenv, drawenv};
    let (disp, draw, ot, tile, r, buf) = (SCRATCH, SCRATCH + 0x40, SCRATCH + 0x100, SCRATCH + 0x200, SCRATCH + 0x300, SCRATCH + 0x400);
    let rect_bytes = |x: u16, y: u16, w: u16, h: u16| [x.to_le_bytes(), y.to_le_bytes(), w.to_le_bytes(), h.to_le_bytes()].concat();
    s.call(ctx, "ResetGraph", [0; 4])?;
    s.c.write(disp, &[0; dispenv::SIZEOF as usize]);
    s.c.write(disp + dispenv::DISP, &rect_bytes(0, 0, 320, 240));
    s.call(ctx, "PutDispEnv", [disp, 0, 0, 0])?;
    s.c.write(draw, &[0; drawenv::SIZEOF as usize]);
    s.c.write(draw + drawenv::CLIP, &rect_bytes(0, 0, 320, 240));
    s.c.write(draw + drawenv::DTD, &[1, 1, 1, 0x20, 0x40, 0x80]);
    s.call(ctx, "PutDrawEnv", [draw, 0, 0, 0])?;
    s.call(ctx, "ClearOTagR", [ot, 8, 0, 0])?;
    // TILE (tag; r g b code; x y; w h): SetTile sets its length and code.
    s.call(ctx, "SetTile", [tile, 0, 0, 0])?;
    s.c.write(tile + 4, &[0xFF, 0x10, 0x10]);
    s.c.write(tile + 8, &[4, 0, 1, 0, 8, 0, 2, 0]);
    s.call(ctx, "AddPrim", [ot + 4 * 7, tile, 0, 0])?;
    s.observe("ordering table".into(), vec![hex(&s.c.read(ot, 32)), hex(&s.c.read(tile, 4))]);
    s.call(ctx, "DrawOTag", [ot + 4 * 7, 0, 0, 0])?;
    s.call(ctx, "DrawSync", [0; 4])?;
    s.c.write(r, &rect_bytes(0, 0, 16, 4));
    s.c.write(buf, &[0xAA; 128]);
    s.call(ctx, "StoreImage", [r, buf, 0, 0])?;
    s.call(ctx, "DrawSync", [0; 4])?;
    s.observe("StoreImage 16x4 at (0,0)".into(), vec![hex(&s.c.read(buf, 128))]);
    s.call(ctx, "SetDispMask", [1, 0, 0, 0])?;
    for f in 0..frames {
        s.c.run_vblanks(1)?;
        let ode = s.c.call(ctx.addr("GetODE")?, [0; 4])? as u32;
        s.observe(format!("frame {f}"), vec![format!("GetODE {ode}")]);
    }
    Ok(())
}

/// Poll `MemCardSync(1, &cmd, &result)` once per vblank until a command
/// finishes; (frames waited, cmd, result).
fn mc_sync(ctx: &Ctx, s: &mut Side, what: &str) -> Result<()> {
    let (cmd, res) = (SCRATCH + 0x7F0, SCRATCH + 0x7F4);
    for f in 0..600 {
        let v = s.c.call(ctx.addr("MemCardSync")?, [1, cmd, res, 0])? as i32;
        if v == 1 {
            let word = |a: u32| s.c.read(a, 4).iter().rev().fold(0u32, |w, &b| w << 8 | b as u32);
            let (c, r) = (word(cmd), word(res));
            s.observe(format!("{what}: sync"), vec![format!("after {f} vblanks"), format!("cmd {c}"), format!("result {r}")]);
            return Ok(());
        }
        s.c.run_vblanks(1)?;
    }
    s.observe(format!("{what}: sync"), vec!["never finished".into()]);
    Ok(())
}

/// libmcrd (guest code over the BIOS card functions, M7): a new card in
/// slot 1 (Exist reports it, Accept takes it), a file created, opened,
/// written, read back and listed; slot 2 is empty.
fn libmcrd(ctx: &Ctx, s: &mut Side, _frames: u32) -> Result<()> {
    let (name, data, back, dir, files) = (SCRATCH, SCRATCH + 0x100, SCRATCH + 0x200, SCRATCH + 0x300, SCRATCH + 0x6F0);
    s.c.write(name, b"BASLUS-00000PSXW\0");
    let pattern: Vec<u8> = (0..128u32).map(|i| (i * 7 + 3) as u8).collect();
    s.c.write(data, &pattern);
    s.c.write(back, &[0; 128]);
    // A game has its interrupt callbacks set up first.
    s.call(ctx, "ResetCallback", [0; 4])?;
    s.call(ctx, "MemCardInit", [0; 4])?;
    s.call(ctx, "MemCardStart", [0; 4])?;
    s.call(ctx, "MemCardExist", [0x00, 0, 0, 0])?;
    mc_sync(ctx, s, "Exist slot 1")?;
    s.call(ctx, "MemCardAccept", [0x00, 0, 0, 0])?;
    mc_sync(ctx, s, "Accept slot 1")?;
    s.call(ctx, "MemCardExist", [0x10, 0, 0, 0])?;
    mc_sync(ctx, s, "Exist slot 2")?;
    s.call(ctx, "MemCardCreateFile", [0x00, name, 1, 0])?;
    s.call(ctx, "MemCardOpen", [0x00, name, 3, 0])?;
    s.call(ctx, "MemCardWriteData", [data, 0x100, 128, 0])?;
    mc_sync(ctx, s, "WriteData")?;
    s.call(ctx, "MemCardReadData", [back, 0x100, 128, 0])?;
    mc_sync(ctx, s, "ReadData")?;
    s.observe("read back".into(), vec![(s.c.read(back, 128) == pattern).to_string()]);
    s.call(ctx, "MemCardClose", [0; 4])?;
    let v = s.c.call_args(ctx.addr("MemCardGetDirentry")?, &[0x00, name, dir, files, 0, 15])? as u32;
    s.observe("MemCardGetDirentry(0, name, dir, files, 0, 15)".into(), vec![format!("{v:#x}")]);
    s.observe("direntry".into(), vec![hex(&s.c.read(dir, 0x28)), hex(&s.c.read(files, 4))]);
    Ok(())
}

/// The scenario's pad input: nothing, then single buttons, chords and a
/// long hold (port 1); port 2 idle.
fn replay(frames: u32) -> Vec<u8> {
    let presses: Vec<String> = ["START@20", "CROSS@30:6", "UP+CIRCLE@40", "SQUARE+L1+R2@50:10", "SELECT@70:20"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    psxw_oracle::play::replay_from_presses("conform", frames + 2, &presses).expect("valid presses")
}

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    game: String,
    library: String,
    /// The library's objects in this game (member, identity key): what
    /// the verdict covers (M8 task 7).
    objects: Vec<(String, String)>,
    steps: usize,
    pass: bool,
    first_differences: Vec<(Observation, Observation)>,
    adapter_log: Vec<Observation>,
    original_log: Vec<Observation>,
}

pub fn run(ws: &Workspace, game: &str, args: &[&str]) -> Result<bool> {
    let mut lib = None;
    let mut frames = 120u32;
    let mut server = false;
    let mut dump: Option<std::path::PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--lib" => lib = it.next().map(|s| s.to_string()),
            "--frames" => frames = it.next().context("--frames needs a value")?.parse()?,
            "--render" => server = it.next().is_some_and(|r| *r == "server"),
            "--dump-frames" => dump = it.next().map(|d| std::path::PathBuf::from(*d)),
            other => bail!("unknown conform option {other}"),
        }
    }
    let lib = lib.context("--lib is required")?;
    let scenario: fn(&Ctx, &mut Side, u32) -> Result<()> = match lib.as_str() {
        "libpad" => libpad,
        "libgpu" => libgpu,
        "libmcrd" => libmcrd,
        other => bail!("no conformance scenario for {other} yet (libpad, libgpu, libmcrd)"),
    };

    let inputs = psxw_lift::run::load(ws, game)?;
    let exe = inputs.exe;
    let file = inputs.originals.file(&inputs.map.main).context("the main EXE image is missing")?;
    let text = file.get(0x800..0x800 + exe.t_size as usize).context("EXE shorter than its header")?.to_vec();
    let ram = vec![(exe.t_addr, text)];
    let sp = if exe.s_addr != 0 { exe.s_addr + exe.s_size } else { 0x801F_FF00 };

    let adapters = psxw_build::game::build(ws, &psxw_build::game::Options::new(game))?.0.wasm;
    let opts = psxw_build::game::Options { original: vec![lib.clone()], ..psxw_build::game::Options::new(game) };
    let original = psxw_build::game::build(ws, &opts)?.0.wasm;

    let ctx = Ctx { inputs: &inputs };
    let dir = ws.out_dir().join("conform").join(game);
    std::fs::create_dir_all(&dir)?;
    let side = |wasm: &str| -> Result<Side> {
        let render = server.then(|| crate::render::server(ws));
        let dump_frames = dump.clone().map(|d| (d.join(if wasm.ends_with("game.wasm") { "adapter" } else { "original" }), 1));
        let o = Options { wasm: wasm.into(), replay: Some(replay(frames)), deadline_secs: 20, render, dump_frames, ..Default::default() };
        Ok(Side { c: Conform::new(&o, &ram, sp, exe.gp0)?, log: vec![] })
    };
    let mut a = side(&adapters)?;
    let mut o = side(&original)?;
    eprintln!("conform: adapter side ({adapters})");
    let ra = scenario(&ctx, &mut a, frames);
    eprintln!("conform: original side ({original})");
    let ro = scenario(&ctx, &mut o, frames);
    if let Err(e) = &ra {
        eprintln!("conform: the adapter side failed: {e:#}");
    }
    if let Err(e) = &ro {
        eprintln!("conform: the original side failed: {e:#}");
    }
    let (a, o) = (a.finish(&dir, "adapter")?, o.finish(&dir, "original")?);
    let diffs: Vec<(Observation, Observation)> =
        a.log.iter().zip(&o.log).filter(|(x, y)| x != y).take(16).map(|(x, y)| (x.clone(), y.clone())).collect();
    let pass = ra.is_ok() && ro.is_ok() && diffs.is_empty() && a.log.len() == o.log.len();
    for (x, y) in diffs.iter().take(8) {
        println!("differs at {}:\n  adapter  {:?}\n  original {:?}", x.step, x.values, y.values);
    }
    let db = psxw_inputs::identity::SigDb::pinned(ws)?;
    let ident = psxw_inputs::identity::identify(game, &inputs.map, &inputs.originals, &db);
    let objects = ident
        .identities
        .iter()
        .filter(|i| i.member.split('/').next() == Some(lib.as_str()))
        .map(|i| (i.member.clone(), i.key.clone().unwrap_or_else(|| "(subsegment only)".into())))
        .collect();
    let report = Report {
        schema_version: 1,
        game: game.into(),
        library: lib.clone(),
        objects,
        steps: a.log.len().max(o.log.len()),
        pass,
        first_differences: diffs,
        adapter_log: a.log,
        original_log: o.log,
    };
    let json = dir.join(format!("{lib}.json"));
    std::fs::write(&json, serde_json::to_string_pretty(&report)?)?;
    println!("conform {game} {lib}: {} ({} steps; {})", if pass { "PASS" } else { "FAIL" }, report.steps, json.display());
    Ok(pass)
}
