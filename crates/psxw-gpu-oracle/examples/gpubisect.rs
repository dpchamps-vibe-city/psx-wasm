//! Debugging aid: find the first packet of a GPU_DRAW vector after which
//! psx-wasm's GPU and the device target disagree. Every prefix of the
//! packet list becomes a vector, captured on the target and run through
//! the device (none of them is stored).
//!
//! ```text
//! cargo run --release -p psxw-gpu-oracle --example gpubisect -- <vector id prefix>
//! ```

use anyhow::{Context, Result, bail};
use psxw_inputs::Workspace;
use psxw_oracle::commands::DeviceUnderTest;
use psxw_oracle::target::{DockerTarget, run_vectors};
use psxw_oracle::vector::{Vector, bytes_to_pixels};

fn main() -> Result<()> {
    let prefix = std::env::args().nth(1).context("usage: gpubisect <id prefix>")?;
    let ws = Workspace::discover()?;
    let store = psxw_oracle::commands::store(&ws);
    let id = store.vector_ids()?.into_iter().find(|id| id.hex().starts_with(prefix.as_str())).context("no such vector")?;
    let Vector::GpuDraw(d) = store.vector(&id)? else { bail!("not a GPU_DRAW vector") };
    let prefixes: Vec<_> = (1..=d.packets.len())
        .map(|k| {
            let mut p = d.clone();
            p.packets.truncate(k);
            let v = Vector::GpuDraw(p);
            (v.id(), v)
        })
        .collect();
    let mut target = DockerTarget::open(&ws)?;
    let goldens = run_vectors(&mut target, &store, &prefixes)?;
    let mut dev = psxw_gpu_oracle::GpuDevice::new(&ws);
    for (k, ((_, v), golden)) in prefixes.iter().zip(goldens).enumerate() {
        let golden = golden.map_err(|e| anyhow::anyhow!(e))?;
        let got = dev.run(v, &store)?;
        if got != golden {
            let (g, w) = (bytes_to_pixels(&got), bytes_to_pixels(&golden));
            let r = d.readback[0];
            println!("first difference after packet {} of {}:", k + 1, d.packets.len());
            println!("  {}", d.packets[k].iter().map(|w| format!("{w:08x}")).collect::<Vec<_>>().join(" "));
            let mut shown = 0;
            for (i, (a, b)) in g.iter().zip(&w).enumerate() {
                if a != b && shown < 20 {
                    let (x, y) = (r.x as usize + i % r.w as usize, r.y as usize + i / r.w as usize);
                    println!("  ({x},{y}) got {a:04x} want {b:04x}");
                    shown += 1;
                }
            }
            println!("  {} pixels differ", g.iter().zip(&w).filter(|(a, b)| a != b).count());
            return Ok(());
        }
    }
    println!("every prefix agrees");
    Ok(())
}
