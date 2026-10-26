//! `psxw oracle record <game> [--replay R] [--frames N] [--every F] [--budget N]`:
//! the GPU recording hook (M4 task 10). Runs the game natively from a
//! replay, with the render server as its renderer, and records the first
//! GPU linked-list transfer at least `F` frames (default 60) after the
//! last, up to `N` (default 64), as GPU_DRAW vectors in set `game-<id>`
//! (`psxw_gpu_oracle::record`). Then `psxw oracle capture --missing`
//! takes their goldens and `psxw oracle check <game> --device gpu` judges
//! them.

use anyhow::{Context, Result, bail, ensure};
use psxw_gpu_oracle::record::{Outcome, draw_vector};
use psxw_host_native::{GpuRecording, Stop};
use psxw_inputs::Workspace;
use psxw_oracle::images;
use psxw_oracle::record::{StoreRecorder, game_set};
use std::collections::BTreeMap;

pub const USAGE: &str = "record <game> [--replay R] [--frames N] [--every F] [--budget N]";

pub fn record_cmd(ws: &Workspace, game: &str, args: &[&str]) -> Result<()> {
    let (mut replay, mut frames, mut every, mut budget) = (None, None, 60u64, 64usize);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().with_context(|| format!("{a} needs a value"));
        match *a {
            "--replay" => replay = Some(val()?.to_string()),
            "--frames" => frames = Some(val()?.parse::<u64>()?),
            "--every" => every = val()?.parse()?,
            "--budget" => budget = val()?.parse()?,
            other => bail!("unknown option {other}; usage: psxw oracle {USAGE}"),
        }
    }
    let (bytes, label) = crate::wholegame::replay(ws, game, replay.as_deref())?;
    let (mut o, _) = crate::run::options(ws, game, &[])?;
    o.frames = frames.unwrap_or_else(|| psxw_abi::replay::Header::parse(&bytes).map_or(0, |h| h.frames as u64));
    o.replay = Some(bytes);
    o.render = Some(crate::render::server(ws));
    o.record_gpu = Some(GpuRecording { every: every.max(1), budget });
    eprintln!("record: {game}, replay {label}, {} frames", o.frames);
    let r = psxw_host_native::run(o)?;
    ensure!(matches!(r.stop, Stop::FrameLimit), "the run stopped early: {:?} (frame {})", r.stop, r.frames);

    let store = psxw_oracle::Store::new(images::oracle_dir(ws));
    let mut rec = StoreRecorder::new(store.clone(), &game_set(game))?;
    let (mut recorded, mut declined) = (0, 0);
    let mut skipped: BTreeMap<String, usize> = BTreeMap::new();
    for b in &r.gpu_batches {
        match draw_vector(&mut rec, &store, &b.vram, &b.state, &b.nodes)? {
            Outcome::Recorded(_) => recorded += 1,
            Outcome::Declined => declined += 1,
            Outcome::Skipped(why) => *skipped.entry(why).or_default() += 1,
        }
    }
    rec.flush()?;
    println!("{} batches offered: {recorded} recorded, {declined} already recorded", r.gpu_batches.len());
    for (why, n) in &skipped {
        println!("  skipped {n}: {why}");
    }
    println!("set {}; next: psxw oracle capture --missing, then psxw oracle check {game} --device gpu", game_set(game));
    Ok(())
}
