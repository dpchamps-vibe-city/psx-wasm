//! `psxw oracle record <game> [--replay R] [--frames N] [--device gpu|spu]
//! [--every F] [--budget N] [--samples S]`: the recording hooks (M4 task 10,
//! M6 task 6). Runs the game natively from a replay and records into set
//! `game-<id>`:
//! - **gpu:** the first GPU linked-list transfer at least `F` frames
//!   (default 60) after the last, up to `N` (default 64), as GPU_DRAW
//!   vectors (`psxw_gpu_oracle::record`); the run renders through the
//!   render server, whose VRAM they start from;
//! - **spu:** a window of `S` samples (default 8820, 0.2 s) every `F`
//!   frames, up to `N`, as SPU_RUN vectors (`psxw_oracle::spu::
//!   window_vector`).
//!
//! Both by default. Then `psxw oracle capture --missing` takes their
//! goldens and `psxw oracle check <game> --device gpu|spu` judges them.

use anyhow::{Context, Result, bail, ensure};
use psxw_host_native::{GpuRecording, SpuRecording, Stop};
use psxw_inputs::Workspace;
use psxw_oracle::images;
use psxw_oracle::record::{Outcome, StoreRecorder, game_set};
use std::collections::BTreeMap;

pub const USAGE: &str = "record <game> [--replay R] [--frames N] [--device gpu|spu] [--every F] [--budget N] [--samples S]";

#[derive(Default)]
struct Tally {
    offered: usize,
    recorded: usize,
    declined: usize,
    skipped: BTreeMap<String, usize>,
}

impl Tally {
    fn add(&mut self, o: Outcome) {
        self.offered += 1;
        match o {
            Outcome::Recorded(_) => self.recorded += 1,
            Outcome::Declined => self.declined += 1,
            Outcome::Skipped(why) => *self.skipped.entry(why).or_default() += 1,
        }
    }

    fn print(&self, what: &str) {
        println!("{what}: {} offered, {} recorded, {} already recorded", self.offered, self.recorded, self.declined);
        for (why, n) in &self.skipped {
            println!("  skipped {n}: {why}");
        }
    }
}

pub fn record_cmd(ws: &Workspace, game: &str, args: &[&str]) -> Result<()> {
    let (mut replay, mut frames, mut device) = (None, None, None::<String>);
    let (mut every, mut budget, mut samples) = (None::<u64>, None::<usize>, 8820u32);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().with_context(|| format!("{a} needs a value"));
        match *a {
            "--replay" => replay = Some(val()?.to_string()),
            "--frames" => frames = Some(val()?.parse::<u64>()?),
            "--device" => device = Some(val()?.to_string()),
            "--every" => every = Some(val()?.parse()?),
            "--budget" => budget = Some(val()?.parse()?),
            "--samples" => samples = val()?.parse()?,
            other => bail!("unknown option {other}; usage: psxw oracle {USAGE}"),
        }
    }
    let (gpu, spu) = match device.as_deref() {
        None => (true, true),
        Some("gpu") => (true, false),
        Some("spu") => (false, true),
        Some(d) => bail!("--device {d}: gpu or spu"),
    };
    let (bytes, label) = crate::wholegame::replay(ws, game, replay.as_deref())?;
    let (mut o, _) = crate::run::options(ws, game, &[])?;
    o.frames = frames.unwrap_or_else(|| psxw_abi::replay::Header::parse(&bytes).map_or(0, |h| h.frames as u64));
    o.replay = Some(bytes);
    if gpu {
        o.render = Some(crate::render::server(ws));
        o.record_gpu = Some(GpuRecording { every: every.unwrap_or(60).max(1), budget: budget.unwrap_or(64) });
    }
    if spu {
        o.record_spu = Some(SpuRecording { every: every.unwrap_or(300).max(1), samples, budget: budget.unwrap_or(24) });
    }
    eprintln!("record: {game}, replay {label}, {} frames", o.frames);
    let r = psxw_host_native::run(o)?;
    ensure!(matches!(r.stop, Stop::FrameLimit), "the run stopped early: {:?} (frame {})", r.stop, r.frames);

    let store = psxw_oracle::Store::new(images::oracle_dir(ws));
    let mut rec = StoreRecorder::new(store.clone(), &game_set(game))?;
    if gpu {
        let mut t = Tally::default();
        for b in &r.gpu_batches {
            t.add(psxw_gpu_oracle::record::draw_vector(&mut rec, &store, &b.vram, &b.state, &b.nodes)?);
        }
        t.print("gpu batches");
    }
    if spu {
        let mut t = Tally::default();
        for w in &r.spu_windows {
            t.add(psxw_oracle::spu::window_vector(&mut rec, &store, w.samples, &w.regs, &w.writes, &w.ram)?);
        }
        t.print("spu windows");
    }
    rec.flush()?;
    println!("set {}; next: psxw oracle capture --missing, then psxw oracle check {game} --device gpu|spu", game_set(game));
    Ok(())
}
