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

pub const USAGE: &str = "conform <game> --lib libpad [--frames N]";

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
    steps: usize,
    pass: bool,
    first_differences: Vec<(Observation, Observation)>,
    adapter_log: Vec<Observation>,
    original_log: Vec<Observation>,
}

pub fn run(ws: &Workspace, game: &str, args: &[&str]) -> Result<bool> {
    let mut lib = None;
    let mut frames = 120u32;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--lib" => lib = it.next().map(|s| s.to_string()),
            "--frames" => frames = it.next().context("--frames needs a value")?.parse()?,
            other => bail!("unknown conform option {other}"),
        }
    }
    let lib = lib.context("--lib is required")?;
    let scenario: fn(&Ctx, &mut Side, u32) -> Result<()> = match lib.as_str() {
        "libpad" => libpad,
        other => bail!("no conformance scenario for {other} yet (libpad)"),
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
    let side = |wasm: &str, io: bool| -> Result<Side> {
        let o = Options { wasm: wasm.into(), replay: Some(replay(frames)), ..Default::default() };
        Ok(Side { c: Conform::new(&o, &ram, sp, exe.gp0, io)?, log: vec![] })
    };
    let mut a = side(&adapters, false)?;
    let mut o = side(&original, true)?;
    let ra = scenario(&ctx, &mut a, frames);
    let ro = scenario(&ctx, &mut o, frames);
    if let Err(e) = &ra {
        eprintln!("conform: the adapter side failed: {e:#}");
    }
    if let Err(e) = &ro {
        eprintln!("conform: the original side failed: {e:#}");
    }
    let diffs: Vec<(Observation, Observation)> =
        a.log.iter().zip(&o.log).filter(|(x, y)| x != y).take(16).map(|(x, y)| (x.clone(), y.clone())).collect();
    let pass = ra.is_ok() && ro.is_ok() && diffs.is_empty() && a.log.len() == o.log.len();
    for (x, y) in diffs.iter().take(8) {
        println!("differs at {}:\n  adapter  {:?}\n  original {:?}", x.step, x.values, y.values);
    }
    let report = Report {
        schema_version: 1,
        game: game.into(),
        library: lib.clone(),
        steps: a.log.len().max(o.log.len()),
        pass,
        first_differences: diffs,
        adapter_log: a.log,
        original_log: o.log,
    };
    let dir = ws.out_dir().join("conform").join(game);
    std::fs::create_dir_all(&dir)?;
    let json = dir.join(format!("{lib}.json"));
    std::fs::write(&json, serde_json::to_string_pretty(&report)?)?;
    println!("conform {game} {lib}: {} ({} steps; {})", if pass { "PASS" } else { "FAIL" }, report.steps, json.display());
    Ok(pass)
}
