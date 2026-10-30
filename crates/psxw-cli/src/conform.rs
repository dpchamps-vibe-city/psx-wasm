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
//! psxw conform <game> --chip gpu [--seeds N]   (D59: the chip's adapter cases, native vs device)
//! psxw conform <game> --modes [--replay R]     (D59 level 4: the replay native and all-device)
//! ```

use anyhow::{Context, Result, bail};
use psxw_host_native::{Conform, Options};
use psxw_inputs::Workspace;
use serde::Serialize;

/// Libraries with a scenario (the `psxw ci` gate runs each).
pub const LIBRARIES: &[&str] = &["libpad", "libgpu", "libmcrd", "libetc", "libsnd", "libspu"];

/// Functions a scenario counts calls to (build probes): libsnd's tick.
pub fn probes(lib: &str) -> &'static [&'static str] {
    match lib {
        "libsnd" => &["SsSeqCalledTbyT"],
        _ => &[],
    }
}

/// Free RAM for scenario buffers: below the stack, above any game data.
const SCRATCH: u32 = 0x801C_0000;

/// One step's observations.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct Observation {
    step: String,
    values: Vec<String>,
}

struct Side {
    c: Traced,
    log: Vec<Observation>,
}

/// One step of a scenario and what it returned: enough to replay it on
/// another host and compare (`psxw web-test conform`).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Step {
    Write { addr: u32, hex: String },
    Read { addr: u32, len: usize, hex: String },
    /// Arguments past the fourth go on the stack at `sp + 4 * i`.
    Call { addr: u32, args: Vec<u32>, ret: String },
    Vblanks { n: u32 },
}

/// The conformance handle, recording every step.
struct Traced {
    c: Conform,
    trace: std::cell::RefCell<Vec<Step>>,
}

impl Traced {
    fn write(&mut self, addr: u32, bytes: &[u8]) {
        self.c.write(addr, bytes);
        self.trace.borrow_mut().push(Step::Write { addr, hex: hex(bytes) });
    }

    fn read(&self, addr: u32, len: usize) -> Vec<u8> {
        let b = self.c.read(addr, len);
        self.trace.borrow_mut().push(Step::Read { addr, len, hex: hex(&b) });
        b
    }

    fn call(&mut self, addr: u32, args: [u32; 4]) -> Result<u64> {
        let v = self.c.call(addr, args)?;
        self.trace.borrow_mut().push(Step::Call { addr, args: args.to_vec(), ret: format!("{v:#x}") });
        Ok(v)
    }

    fn call_args(&mut self, addr: u32, args: &[u32]) -> Result<u64> {
        let v = self.c.call_args(addr, args)?;
        self.trace.borrow_mut().push(Step::Call { addr, args: args.to_vec(), ret: format!("{v:#x}") });
        Ok(v)
    }

    fn run_vblanks(&mut self, n: u32) -> Result<()> {
        self.c.run_vblanks(n)?;
        self.trace.borrow_mut().push(Step::Vblanks { n });
        Ok(())
    }

    /// Calls so far to a probed function (not a traced step).
    fn probe_count(&mut self, addr: u32) -> Result<u64> {
        Ok(self.c.counts()?.get(&format!("probe.{addr:08X}")).copied().unwrap_or(0))
    }
}

struct Ctx<'a> {
    inputs: &'a psxw_lift::run::Inputs,
}

impl Ctx<'_> {
    fn addr(&self, name: &str) -> Result<u32> {
        let r = self.inputs.map.resolve(&self.inputs.map.main, name).with_context(|| format!("{name} is not in the main container"))?;
        Ok(r.addr)
    }

    /// The manifest's `[conform]` callback and the word it changes.
    fn callback(&self) -> Result<(u32, u32)> {
        let c = &self.inputs.manifest.conform;
        let f = c.callback.as_deref().context("game.toml [conform] has no callback")?;
        let st = c.callback_state.as_deref().context("game.toml [conform] has no callback_state")?;
        Ok((self.addr(f)?, self.addr(st)?))
    }
}

fn word(b: &[u8]) -> u32 {
    b.iter().rev().fold(0u32, |w, &x| w << 8 | x as u32)
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
    fn finish(mut self, dir: &std::path::Path, which: &str) -> Result<Finished> {
        let r = self.c.c.finish();
        std::fs::write(dir.join(format!("run-{which}.json")), serde_json::to_string_pretty(&r)?)?;
        // Memory cards written back: every directory frame's checksum must
        // validate, and the files listed are part of what both sides saw.
        let mut cards_valid = true;
        let mut cards: Vec<_> = std::fs::read_dir(dir.join(format!("storage-{which}")))
            .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "mcr")).collect())
            .unwrap_or_default();
        cards.sort();
        for p in cards {
            let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let card = psx_devices::memcard::Card::from_image(&std::fs::read(&p)?);
            let valid = card.as_ref().is_some_and(|c| c.valid());
            cards_valid &= valid;
            let files: Vec<String> = card.iter().flat_map(|c| c.files()).map(|f| format!("{} {} bytes", f.name, f.size)).collect();
            self.log.push(Observation { step: format!("{name} written back"), values: vec![format!("valid {valid}"), format!("files {files:?}")] });
        }
        self.log.push(Observation { step: "audio".into(), values: vec![format!("{} frames, {} audible, {}", r.audio.frames, r.audio.audible_frames, r.audio.hash)] });
        Ok(Finished { log: self.log, cards_valid, audible: r.audio.audible_frames })
    }
}

struct Finished {
    log: Vec<Observation>,
    /// Every card image the run wrote back has valid checksums.
    cards_valid: bool,
    /// Audio frames with sound in them.
    audible: u64,
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

/// libetc (M4 task 8): the interrupt layer. `ResetCallback`, the video
/// mode and interrupt mask, then the manifest's callback on vblank: each
/// frame `VSync(-1)` and the callback's word; `VSync` waits of 0, 1 and n
/// frames; the callback removed; then as a GPU DMA callback around a
/// `DrawOTag`. On the original side every interrupt goes through the
/// BIOS's custom exit into libetc's own dispatcher, which ends in
/// ReturnFromException (D45). `ResetCallback`'s result points at libetc's
/// private context, which adapters don't have (D59): seen as zero or not.
fn libetc(ctx: &Ctx, s: &mut Side, frames: u32) -> Result<()> {
    let (cb, state) = ctx.callback()?;
    let ot = SCRATCH;
    s.c.write(state, &0x1234_5678u32.to_le_bytes());
    let observe_state = |s: &mut Side, what: String| {
        let v = word(&s.c.read(state, 4));
        s.observe(what, vec![format!("state {v:#010x}")]);
    };
    let v = s.c.call(ctx.addr("ResetCallback")?, [0; 4])? as u32;
    s.observe("ResetCallback() is a pointer".into(), vec![(v != 0).to_string()]);
    s.call(ctx, "CheckCallback", [0; 4])?;
    s.call(ctx, "GetVideoMode", [0; 4])?;
    s.call(ctx, "SetVideoMode", [1, 0, 0, 0])?;
    s.call(ctx, "GetVideoMode", [0; 4])?;
    s.call(ctx, "SetVideoMode", [0, 0, 0, 0])?;
    s.call(ctx, "GetIntrMask", [0; 4])?;
    s.call(ctx, "VSync", [u32::MAX, 0, 0, 0])?;
    s.call(ctx, "VSyncCallback", [cb, 0, 0, 0])?;
    for f in 0..frames.min(30) {
        s.c.run_vblanks(1)?;
        let n = s.c.call(ctx.addr("VSync")?, [u32::MAX, 0, 0, 0])? as u32;
        let v = word(&s.c.read(state, 4));
        s.observe(format!("frame {f}"), vec![format!("VSync(-1) {n}"), format!("state {v:#010x}")]);
    }
    for mode in [0, 1, 3] {
        s.call(ctx, "VSync", [mode, 0, 0, 0])?;
        let n = s.c.call(ctx.addr("VSync")?, [u32::MAX, 0, 0, 0])? as u32;
        s.observe(format!("after VSync({mode})"), vec![format!("VSync(-1) {n}")]);
        observe_state(s, format!("after VSync({mode})"));
    }
    s.call(ctx, "VSyncCallback", [0; 4])?;
    s.c.run_vblanks(3)?;
    observe_state(s, "callback removed, 3 vblanks".into());
    s.call(ctx, "ResetGraph", [0; 4])?;
    s.call(ctx, "DMACallback", [2, cb, 0, 0])?;
    s.call(ctx, "ClearOTagR", [ot, 8, 0, 0])?;
    s.call(ctx, "DrawOTag", [ot + 4 * 7, 0, 0, 0])?;
    s.call(ctx, "DrawSync", [0; 4])?;
    s.c.run_vblanks(1)?;
    observe_state(s, "GPU DMA callback".into());
    s.call(ctx, "DMACallback", [2, 0, 0, 0])?;
    s.call(ctx, "GetIntrMask", [0; 4])?;
    s.call(ctx, "CheckCallback", [0; 4])?;
    Ok(())
}

/// libspu (D47): the original library over the SPU. A looping square-wave
/// ADPCM sample goes to SPU RAM by DMA (`SpuWritePartly`); the master
/// volume and voice 0 are set (`SpuSetCommonAttr`, `SpuSetVoiceAttr`); the
/// voice is keyed on for 30 vblanks. The key status is observed, and the
/// run's audio must be audible (checked when the run ends).
fn libspu(ctx: &Ctx, s: &mut Side, _frames: u32) -> Result<()> {
    use sdk_psyq::layout::{spu_common_attr as common, spu_voice_attr as voice};
    let (data, cattr, vattr) = (SCRATCH, SCRATCH + 0x100, SCRATCH + 0x200);
    // Four 16-byte blocks: shift 0, filter 0; the first marks the loop
    // start, the last ends and repeats. Seven then minus eight, 8 samples
    // each: a loud square wave.
    let mut adpcm = Vec::new();
    for b in 0..4u8 {
        adpcm.extend([0x00, if b == 0 { 0x04 } else if b == 3 { 0x03 } else { 0x00 }]);
        adpcm.extend([0x77, 0x77, 0x77, 0x77, 0x88, 0x88, 0x88, 0x88, 0x77, 0x77, 0x77, 0x77, 0x88, 0x88]);
    }
    s.c.write(data, &adpcm);
    s.call(ctx, "ResetCallback", [0; 4])?;
    s.call(ctx, "SpuInit", [0; 4])?;
    s.call(ctx, "SpuSetTransferMode", [0, 0, 0, 0])?;
    s.call(ctx, "SpuSetTransferStartAddr", [0x1010, 0, 0, 0])?;
    s.call(ctx, "SpuWritePartly", [data, adpcm.len() as u32, 0, 0])?;
    s.call(ctx, "SpuIsTransferCompleted", [1, 0, 0, 0])?;
    let mut c = vec![0u8; common::SIZEOF as usize];
    c[common::MASK as usize..][..4].copy_from_slice(&3u32.to_le_bytes());
    c[common::MVOL as usize..][..4].copy_from_slice(&[0xFF, 0x3F, 0xFF, 0x3F]);
    s.c.write(cattr, &c);
    s.call(ctx, "SpuSetCommonAttr", [cattr, 0, 0, 0])?;
    let mut v = vec![0u8; voice::SIZEOF as usize];
    let mask: u32 = 0x1 | 0x2 | 0x10 | 0x80 | 1 << 17 | 1 << 18;
    v[voice::VOICE as usize..][..4].copy_from_slice(&1u32.to_le_bytes());
    v[voice::MASK as usize..][..4].copy_from_slice(&mask.to_le_bytes());
    v[voice::VOLUME as usize..][..4].copy_from_slice(&[0xFF, 0x3F, 0xFF, 0x3F]);
    v[voice::PITCH as usize..][..2].copy_from_slice(&0x1000u16.to_le_bytes());
    v[voice::ADDR as usize..][..4].copy_from_slice(&0x1010u32.to_le_bytes());
    v[voice::ADSR1 as usize..][..2].copy_from_slice(&0x000Fu16.to_le_bytes());
    v[voice::ADSR2 as usize..][..2].copy_from_slice(&0u16.to_le_bytes());
    s.c.write(vattr, &v);
    s.call(ctx, "SpuSetVoiceAttr", [vattr, 0, 0, 0])?;
    s.call(ctx, "SpuSetKey", [1, 1, 0, 0])?;
    s.c.run_vblanks(30)?;
    s.call(ctx, "SpuGetKeyStatus", [1, 0, 0, 0])?;
    Ok(())
}

/// libsnd (M6 task 5): the sequencer's tick, which libsnd drives itself
/// from the vblank (`SS_TICK60` on NTSC) or from root counter 2's events
/// (`SS_TICK240`). With libetc's interrupts and the BIOS events under it,
/// `SsSeqCalledTbyT` must run at the mode's rate: 60 or 240 times over 60
/// vblanks (one NTSC second), give or take a tick.
fn libsnd(ctx: &Ctx, s: &mut Side, _frames: u32) -> Result<()> {
    let tick = ctx.addr("SsSeqCalledTbyT")?;
    s.call(ctx, "ResetCallback", [0; 4])?;
    s.call(ctx, "SsInit", [0; 4])?;
    for (mode, name, rate) in [(1u32, "SS_TICK60", 60i64), (2, "SS_TICK240", 240)] {
        s.call(ctx, "SsSetTickMode", [mode, 0, 0, 0])?;
        s.call(ctx, "SsStart", [0; 4])?;
        let before = s.c.probe_count(tick)?;
        s.c.run_vblanks(60)?;
        let ticks = (s.c.probe_count(tick)? - before) as i64;
        s.observe(format!("{name}: ticks over 60 vblanks"), vec![ticks.to_string()]);
        if (ticks - rate).abs() > 1 {
            bail!("{name}: SsSeqCalledTbyT ran {ticks} times over 60 vblanks, expected {rate}");
        }
        s.call(ctx, "SsEnd", [0; 4])?;
    }
    Ok(())
}

/// The scenario's pad input: nothing, then single buttons, chords and a
/// long hold (port 1); port 2 idle.
fn replay(frames: u32) -> Vec<u8> {
    replay_pads(frames, 2)
}

/// The same with `pads` plugged (1: port 2 empty).
fn replay_pads(frames: u32, pads: u32) -> Vec<u8> {
    let presses: Vec<String> = ["START@20", "CROSS@30:6", "UP+CIRCLE@40", "SQUARE+L1+R2@50:10", "SELECT@70:20"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    psxw_oracle::play::replay_from_presses("conform", frames + 2, &presses, 0, pads).expect("valid presses")
}

type Scenario = fn(&Ctx, &mut Side, u32) -> Result<()>;

fn scenario_for(lib: &str) -> Result<Scenario> {
    Ok(match lib {
        "libpad" => libpad,
        "libgpu" => libgpu,
        "libmcrd" => libmcrd,
        "libetc" => libetc,
        "libsnd" => libsnd,
        "libspu" => libspu,
        other => bail!("no conformance scenario for {other} yet ({})", LIBRARIES.join(", ")),
    })
}

/// What a scenario starts from: the EXE's text in RAM, its stack and gp.
fn start(inputs: &psxw_lift::run::Inputs) -> Result<(Vec<(u32, Vec<u8>)>, u32, u32)> {
    let exe = inputs.exe;
    let file = inputs.originals.file(&inputs.map.main).context("the main EXE image is missing")?;
    let text = file.get(0x800..0x800 + exe.t_size as usize).context("EXE shorter than its header")?.to_vec();
    let sp = if exe.s_addr != 0 { exe.s_addr + exe.s_size } else { 0x801F_FF00 };
    Ok((vec![(exe.t_addr, text)], sp, exe.gp0))
}

/// The overlays that hold functions a chip case calls (libpress, in bof4),
/// as RAM to write before it starts: resident, as when the game calls
/// them (docs/design.md §3: the bytes in RAM decide). An overlay over the
/// cases' scratch RAM, or over another one the case needs, is an error.
fn resident(inputs: &psxw_lift::run::Inputs, needs: &[&str]) -> Result<Vec<(u32, Vec<u8>)>> {
    let map = &inputs.map;
    let ids: std::collections::BTreeSet<String> =
        needs.iter().filter_map(|n| map.resolve(&map.main, n)?.container).filter(|c| *c != map.main).collect();
    let scratch = sdk_psyq::cases::SCRATCH..sdk_psyq::cases::SCRATCH + sdk_psyq::cases::SCRATCH_LEN;
    let mut out: Vec<(u32, Vec<u8>)> = Vec::new();
    for id in ids {
        let file = inputs.originals.file(&id).with_context(|| format!("no original bytes for overlay {id}"))?;
        for (range, vram) in inputs.originals.segments(&id) {
            let bytes = file.get(range.start as usize..range.end as usize).with_context(|| format!("overlay {id} shorter than its segments"))?;
            let at = *vram..vram + bytes.len() as u32;
            if at.start < scratch.end && scratch.start < at.end {
                bail!("overlay {id} ({:#010x}..{:#010x}) is over the chip cases' scratch RAM", at.start, at.end);
            }
            if let Some((a, b)) = out.iter().find(|(a, b)| at.start < a + b.len() as u32 && *a < at.end) {
                bail!("overlay {id} at {:#010x} is over another a case needs, at {a:#010x}..{:#010x}", at.start, a + b.len() as u32);
            }
            out.push((*vram, bytes.to_vec()));
        }
    }
    Ok(out)
}

/// A scenario run on `game.wasm` with the server renderer, recorded for
/// replay on another host: its steps and results, then the final VRAM and
/// the audio it produced.
#[derive(Debug, Serialize)]
pub struct Recorded {
    pub library: String,
    pub sp: u32,
    pub gp: u32,
    /// Where the EXE text goes (the bundle's `ram.bin`).
    pub ram_addr: u32,
    pub steps: Vec<Step>,
    /// FNV-1a of VRAM after the last step.
    pub vram: String,
    pub audio_frames: u64,
    pub audio_hash: String,
    #[serde(skip)]
    pub ram: Vec<u8>,
    #[serde(skip)]
    pub replay: Vec<u8>,
}

pub fn record(ws: &Workspace, game: &str, lib: &str, frames: u32) -> Result<Recorded> {
    let scenario = scenario_for(lib)?;
    let inputs = psxw_lift::run::load(ws, game)?;
    let (ram, sp, gp) = start(&inputs)?;
    let wasm = psxw_build::game::build(ws, &psxw_build::game::Options::new(game))?.0.wasm;
    let render = crate::render::server(ws);
    let replay = replay(frames);
    let o = Options { wasm: wasm.into(), replay: Some(replay.clone()), deadline_secs: 20, render: Some(render.clone()), ..Default::default() };
    let mut side = Side { c: Traced { c: Conform::new(&o, &ram, sp, gp)?, trace: Default::default() }, log: vec![] };
    scenario(&Ctx { inputs: &inputs }, &mut side, frames)?;
    let vram = {
        let mut r = render.lock().map_err(|_| anyhow::anyhow!("renderer poisoned"))?;
        let px = r.vram()?.context("the server renderer has no VRAM")?;
        let bytes: Vec<u8> = px.iter().flat_map(|p| p.to_le_bytes()).collect();
        format!("{:016x}", psxw_host_native::fnv(&bytes))
    };
    let steps = side.c.trace.take();
    let report = side.c.c.finish();
    Ok(Recorded {
        library: lib.into(),
        sp,
        gp,
        ram_addr: ram[0].0,
        steps,
        vram,
        audio_frames: report.audio.frames,
        audio_hash: report.audio.hash,
        ram: ram.into_iter().next().map(|r| r.1).unwrap_or_default(),
        replay,
    })
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
    let mut chip: Option<String> = None;
    let mut seeds = 4u64;
    let mut modes = false;
    let mut replay_name: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--chip" => chip = it.next().map(|s| s.to_string()),
            "--seeds" => seeds = it.next().context("--seeds needs a value")?.parse()?,
            "--modes" => modes = true,
            "--replay" => replay_name = it.next().map(|s| s.to_string()),
            "--lib" => lib = it.next().map(|s| s.to_string()),
            "--frames" => frames = it.next().context("--frames needs a value")?.parse()?,
            "--render" => server = it.next().is_some_and(|r| *r == "server"),
            "--dump-frames" => dump = it.next().map(|d| std::path::PathBuf::from(*d)),
            other => bail!("unknown conform option {other}"),
        }
    }
    if modes {
        return crate::modes::run(ws, game, replay_name.as_deref());
    }
    if let Some(chip) = chip {
        return run_chip(ws, game, &chip, seeds);
    }
    let lib = lib.context("--lib or --chip is required")?;
    let scenario = scenario_for(&lib)?;

    let inputs = psxw_lift::run::load(ws, game)?;
    let (ram, sp, gp) = start(&inputs)?;

    let probed = probes(&lib).iter().map(|p| p.to_string()).collect::<Vec<_>>();
    let base = psxw_build::game::Options { probes: probed.clone(), ..psxw_build::game::Options::new(game) };
    let adapters = psxw_build::game::build(ws, &base)?.0.wasm;
    // A library with adapters is compared with its original code over the
    // devices. Guest code is already original on both sides, and running
    // it as a device library would route its I/O to devices it may not
    // have (libsnd writes SPU registers directly).
    let boundary = match &inputs.manifest.game.boundary {
        Some(p) => psxw_inputs::boundary::Boundary::load(&ws.root.join(p))?,
        None => Default::default(),
    };
    let guest = boundary.policy(&format!("{lib}/")) == Some(psxw_inputs::boundary::Policy::Guest);
    let original = if guest {
        adapters.clone()
    } else {
        let opts = psxw_build::game::Options { original: vec![lib.clone()], ..base };
        psxw_build::game::build(ws, &opts)?.0.wasm
    };

    let ctx = Ctx { inputs: &inputs };
    let dir = ws.out_dir().join("conform").join(game);
    std::fs::create_dir_all(&dir)?;
    // Each side gets its own storage, so the memory cards the runtime
    // writes back can be checked after the run (M7 exit criterion 1).
    let side = |wasm: &str, which: &str| -> Result<Side> {
        let render = server.then(|| crate::render::server(ws));
        let dump_frames = dump.clone().map(|d| (d.join(which), 1));
        let storage = dir.join(format!("storage-{which}"));
        let _ = std::fs::remove_dir_all(&storage);
        let o = Options {
            wasm: wasm.into(),
            replay: Some(replay(frames)),
            deadline_secs: 20,
            render,
            dump_frames,
            storage_dir: Some(storage),
            ..Default::default()
        };
        Ok(Side { c: Traced { c: Conform::new(&o, &ram, sp, gp)?, trace: Default::default() }, log: vec![] })
    };
    let mut a = side(&adapters, "adapter")?;
    let mut o = side(&original, "original")?;
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
    // libspu's scenario plays a voice: silence means the SPU path is broken.
    let sound = lib != "libspu" || (a.audible > 0 && o.audible > 0);
    let pass = ra.is_ok() && ro.is_ok() && diffs.is_empty() && a.log.len() == o.log.len() && a.cards_valid && o.cards_valid && sound;
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

/// One side of a chip's cases (D59): one instance of one build, with SDK
/// functions found by name in the game's map.
struct ChipSide<'a> {
    c: Conform,
    ctx: &'a Ctx<'a>,
    log: Vec<Observation>,
    callback: Option<(u32, u32)>,
    movie: Option<u32>,
}

impl sdk_psyq::cases::Harness for ChipSide<'_> {
    fn linked(&self, name: &str) -> bool {
        self.ctx.addr(name).is_ok()
    }

    fn call(&mut self, name: &str, args: &[u32]) -> Result<u32, String> {
        let a = self.ctx.addr(name).map_err(|e| format!("{e:#}"))?;
        self.c.call_args(a, args).map(|v| v as u32).map_err(|e| format!("{e:#}"))
    }

    fn read(&mut self, addr: u32, len: usize) -> Vec<u8> {
        self.c.read(addr, len)
    }

    fn write(&mut self, addr: u32, bytes: &[u8]) {
        self.c.write(addr, bytes)
    }

    fn vblanks(&mut self, n: u32) -> Result<(), String> {
        self.c.run_vblanks(n).map_err(|e| format!("{e:#}"))
    }

    fn observe(&mut self, what: &str, value: String) {
        self.log.push(Observation { step: what.into(), values: vec![value] });
    }

    fn device(&mut self) -> String {
        let (frames, audible, hash) = self.c.audio();
        format!("render {:016x} display {:016x} audio {frames} frames, {audible} audible, {hash:016x}", self.c.render_hash(), self.c.present_hash())
    }

    fn callback(&self) -> Option<(u32, u32)> {
        self.callback
    }

    fn movie(&self) -> Option<u32> {
        self.movie
    }

    fn addr(&self, name: &str) -> Option<u32> {
        self.ctx.addr(name).ok()
    }

    fn plug_port2(&mut self, plugged: bool) -> Result<(), String> {
        self.c.set_replay(replay_pads(CHIP_FRAMES, if plugged { 2 } else { 1 }));
        Ok(())
    }
}

/// The first sector of the disc's first movie: the first file, by
/// position, one of whose first eight sectors starts an STR frame (a data
/// sector whose user data begins 0x0160, 0x8001). A fact of the disc,
/// found the same way for every game.
fn first_movie(cue: &std::path::Path) -> Option<u32> {
    use psx_devices::cd;
    let mut disc = psxw_host_native::disc::Disc::open(cue).ok()?;
    let mut dirs = vec![cd::root(&mut disc)?];
    let mut files = Vec::new();
    while let Some(d) = dirs.pop() {
        for e in cd::list(&mut disc, &d) {
            if e.dir { dirs.push(e) } else { files.push(e) }
        }
    }
    files.sort_by_key(|e| e.lba);
    files.iter().find_map(|f| {
        (0..8.min(f.size.div_ceil(2048))).map(|i| f.lba + i).find(|&lba| {
            let mut raw = [0u8; cd::RAW];
            disc.read_raw(lba, &mut raw) && raw[15] == 2 && raw[0x12] & 0x20 == 0 && raw[0x18..0x1C] == [0x60, 0x01, 0x01, 0x80]
        })
    })
}

/// Vblanks of pad input a chip case gets.
const CHIP_FRAMES: u32 = 600;

/// One case's outcome in one game.
#[derive(Serialize)]
struct CaseResult {
    case: String,
    level: u8,
    /// `pass`, `FAIL`, or `skipped: …`.
    verdict: String,
    first_difference: Option<(Observation, Observation)>,
    native_log: Vec<Observation>,
    device_log: Vec<Observation>,
}

#[derive(Serialize)]
struct ChipReport {
    schema_version: u32,
    game: String,
    chip: String,
    /// The chip's mode in the game's build: the adapter side is the device
    /// itself unless it is native.
    native: bool,
    why: String,
    pass: bool,
    cases: Vec<CaseResult>,
}

/// `psxw conform <game> --chip C [--seeds N]`: every case of chip C, on the
/// game's build and on its build with C on the device; level-1 cases once
/// per seed. A case runs only where the game links every function it
/// names (level 3: the same cases in every game).
pub fn run_chip(ws: &Workspace, game: &str, chip: &str, seeds: u64) -> Result<bool> {
    let inputs = psxw_lift::run::load(ws, game)?;
    let (ram, sp, gp) = start(&inputs)?;
    let (native, _) = psxw_build::game::build(ws, &psxw_build::game::Options::new(game))?;
    let mode = native.chips.get(chip).cloned().with_context(|| format!("{game} places no member of chip {chip}"))?;
    if !mode.native {
        eprintln!("conform: {chip} is emulated in {game} ({}): both sides run the device", mode.why);
    }
    let device = psxw_build::game::build(ws, &psxw_build::game::Options { device: vec![chip.into()], ..psxw_build::game::Options::new(game) })?.0;
    let ctx = Ctx { inputs: &inputs };
    let callback = ctx.callback().ok();
    // The game's disc, for the CD chip's cases, and its first movie.
    let disc = ws.disc(game).ok();
    let movie = disc.as_deref().and_then(first_movie);
    if chip == "cd" {
        eprintln!("conform: disc {:?}, first movie at {movie:?}", disc.as_deref().map(|d| d.display().to_string()));
    }
    let side = |wasm: &str, ram: &[(u32, Vec<u8>)]| -> Result<ChipSide<'_>> {
        // A wall-clock bound per call that catches a hang, not a slow one: a
        // case may spin libgpu's timeout on the device side (a million
        // VSync(-1) polls), which took over 20 s on a loaded machine.
        let o = Options { wasm: wasm.into(), disc: disc.clone().unwrap_or_default(), replay: Some(replay(CHIP_FRAMES)), deadline_secs: 120, ..Default::default() };
        Ok(ChipSide { c: Conform::new(&o, ram, sp, gp)?, ctx: &ctx, log: vec![], callback, movie })
    };
    let mut results = Vec::new();
    for case in sdk_psyq::cases::all().into_iter().filter(|c| c.chip == chip) {
        let missing: Vec<&str> = case.needs.iter().copied().filter(|n| ctx.addr(n).is_err()).collect();
        if !missing.is_empty() {
            let verdict = format!("skipped: {game} doesn't link {}", missing.join(", "));
            println!("conform {game} {chip} {}: {verdict}", case.name);
            results.push(CaseResult { case: case.name.into(), level: case.level, verdict, first_difference: None, native_log: vec![], device_log: vec![] });
            continue;
        }
        let ram = [ram.clone(), resident(&inputs, case.needs)?].concat();
        for seed in 0..if case.level == 1 { seeds } else { 1 } {
            let mut logs = Vec::new();
            for wasm in [&native.wasm, &device.wasm] {
                let mut s = side(wasm, &ram)?;
                let r = (case.run)(&mut s, &mut sdk_psyq::cases::Rng::new(seed));
                if let Err(e) = r {
                    s.log.push(Observation { step: "error".into(), values: vec![e] });
                }
                let d = sdk_psyq::cases::Harness::device(&mut s);
                s.log.push(Observation { step: "device".into(), values: vec![d] });
                logs.push(s.log);
            }
            let (d, n) = (logs.pop().unwrap_or_default(), logs.pop().unwrap_or_default());
            let first = n.iter().zip(&d).find(|(x, y)| x != y).map(|(x, y)| (x.clone(), y.clone()));
            let errored = n.iter().chain(&d).any(|o| o.step == "error");
            let ok = first.is_none() && n.len() == d.len() && !errored;
            let name = if case.level == 1 { format!("{} seed {seed}", case.name) } else { case.name.to_string() };
            println!("conform {game} {chip} {name}: {}", if ok { "pass" } else { "FAIL" });
            if let Some((x, y)) = &first {
                println!("  differs at {}:\n    native {:?}\n    device {:?}", x.step, x.values, y.values);
            }
            results.push(CaseResult { case: name, level: case.level, verdict: if ok { "pass" } else { "FAIL" }.into(), first_difference: first, native_log: n, device_log: d });
        }
    }
    let pass = results.iter().all(|r| r.verdict != "FAIL");
    let report = ChipReport { schema_version: 1, game: game.into(), chip: chip.into(), native: mode.native, why: mode.why, pass, cases: results };
    let dir = ws.out_dir().join("conform").join(game);
    std::fs::create_dir_all(&dir)?;
    let json = dir.join(format!("chip-{chip}.json"));
    std::fs::write(&json, serde_json::to_string_pretty(&report)?)?;
    println!("conform {game} chip {chip}: {} ({} case runs; {})", if pass { "PASS" } else { "FAIL" }, report.cases.len(), json.display());
    Ok(pass)
}
