//! Debugging aid: how much each synthetic GPU generator's vectors change
//! VRAM, from their goldens (a vector that changes nothing passes
//! trivially).
//!
//! ```text
//! cargo run -p psxw-gpu-oracle --example gpucover
//! ```

use anyhow::{Context, Result};
use psxw_inputs::Workspace;
use psxw_oracle::gpu_synth::SET_PREFIX;
use psxw_oracle::target::emulator_target_id;
use psxw_oracle::vector::{Rect, Vector, bytes_to_pixels};
use std::collections::BTreeMap;

/// The initial value of a pixel: the last init rectangle covering it.
fn initial(inits: &[(Rect, Vec<u16>)], x: u16, y: u16) -> Option<u16> {
    inits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(r, px)| px[(y - r.y) as usize * r.w as usize + (x - r.x) as usize])
}

fn main() -> Result<()> {
    let ws = Workspace::discover()?;
    let store = psxw_oracle::commands::store(&ws);
    let pins = ws.lock.oracle.as_ref().context("no [oracle]")?;
    let emu = emulator_target_id(pins.beetle.short(), pins.openbios.short());
    let mut table: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    for set in store.set_names()?.into_iter().filter(|n| n.starts_with(SET_PREFIX)) {
        for id in store.set(&set)? {
            let v = store.vector(&id)?;
            let Some((golden, _)) = store.golden(&emu, &id)? else { continue };
            let golden = bytes_to_pixels(&golden);
            let (init, readback) = match &v {
                Vector::GpuDraw(d) => (&d.init, d.readback.clone()),
                Vector::GpuTransfer(t) => (&t.init, t.readback.clone()),
                _ => continue,
            };
            let inits: Vec<(Rect, Vec<u16>)> =
                init.iter().map(|r| Ok((r.rect, bytes_to_pixels(&store.blob(&r.pixels)?)))).collect::<Result<_>>()?;
            let mut changed = 0;
            let mut i = 0;
            for r in &readback {
                for y in r.y..r.y + r.h {
                    for x in r.x..r.x + r.w {
                        if initial(&inits, x, y) != Some(golden[i]) {
                            changed += 1;
                        }
                        i += 1;
                    }
                }
            }
            let e = table.entry(set.trim_start_matches(SET_PREFIX).to_string()).or_default();
            e.0 += 1;
            e.1 += (changed > 0) as usize;
            e.2 += changed;
        }
    }
    println!("{:<16} {:>7} {:>9} {:>12}", "generator", "vectors", "changing", "pixels");
    for (g, (n, c, px)) in table {
        println!("{g:<16} {n:>7} {c:>9} {px:>12}");
    }
    Ok(())
}
