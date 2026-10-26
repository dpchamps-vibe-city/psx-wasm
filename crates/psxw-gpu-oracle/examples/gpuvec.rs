//! Debugging aid: dump a stored GPU vector and, with `--diff`, run it
//! through psx-wasm's GPU and list the pixels that differ from its
//! golden.
//!
//! ```text
//! cargo run -p psxw-gpu-oracle --example gpuvec -- <vector id prefix> [--diff] [--max N]
//! ```

use anyhow::{Context, Result, bail};
use psxw_inputs::Workspace;
use psxw_oracle::commands::DeviceUnderTest;
use psxw_oracle::golden::EXACT_TARGET;
use psxw_oracle::target::emulator_target_id;
use psxw_oracle::vector::{Rect, TransferStep, Vector, bytes_to_pixels};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(prefix) = args.first() else { bail!("usage: gpuvec <id prefix> [--diff] [--max N]") };
    let diff = args.iter().any(|a| a == "--diff");
    let max: usize = args.iter().position(|a| a == "--max").and_then(|i| args.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(40);
    let ws = Workspace::discover()?;
    let store = psxw_oracle::commands::store(&ws);
    let id = store.vector_ids()?.into_iter().find(|id| id.hex().starts_with(prefix.as_str())).context("no such vector")?;
    let v = store.vector(&id)?;
    println!("vector {}", id.hex());
    let readback: Vec<Rect> = match &v {
        Vector::GpuDraw(d) => {
            for r in &d.init {
                println!("init  {:?}", r.rect);
            }
            println!("state {}", d.state.iter().map(|w| format!("{w:08x}")).collect::<Vec<_>>().join(" "));
            println!("mode  {:?}", d.mode);
            for p in &d.packets {
                println!("packet {}", p.iter().map(|w| format!("{w:08x}")).collect::<Vec<_>>().join(" "));
            }
            for r in &d.readback {
                println!("read  {r:?}");
            }
            d.readback.clone()
        }
        Vector::GpuTransfer(t) => {
            for r in &t.init {
                println!("init  {:?}", r.rect);
            }
            println!("state {:08x?}", t.state);
            let mut rb = Vec::new();
            for s in &t.steps {
                println!("step  {s:?}");
                if let TransferStep::VramToCpu(r) = s {
                    rb.push(*r);
                }
            }
            rb.extend(t.readback.iter().copied());
            rb
        }
        _ => bail!("not a GPU vector"),
    };
    if !diff {
        return Ok(());
    }
    let pins = ws.lock.oracle.as_ref().context("no [oracle]")?;
    let emu = emulator_target_id(pins.beetle.short(), pins.openbios.short());
    let (want, which) = match store.golden(EXACT_TARGET, &id)? {
        Some((g, _)) => (g, "exact"),
        None => (store.golden(&emu, &id)?.context("no golden")?.0, "emulator"),
    };
    let mut dev = psxw_gpu_oracle::GpuDevice::new(&ws);
    eprintln!("{}", dev.name());
    let got = dev.run(&v, &store)?;
    let (g, w) = (bytes_to_pixels(&got), bytes_to_pixels(&want));
    let mut i = 0;
    let mut shown = 0;
    let mut total = 0;
    for r in &readback {
        for y in r.y..r.y + r.h {
            for x in r.x..r.x + r.w {
                if g[i] != w[i] {
                    total += 1;
                    if shown < max {
                        println!("({x:4},{y:3}) got {:04x} want {:04x} ({which})", g[i], w[i]);
                        shown += 1;
                    }
                }
                i += 1;
            }
        }
    }
    println!("{total} pixels differ");
    Ok(())
}
