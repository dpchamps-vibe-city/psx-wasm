//! Throughput of psx-wasm's GPU on the archived GPU vectors: front end
//! (decode, batching, encoding) on the host, backend (render server on
//! Mesa llvmpipe in the container) per vector.
//!
//! ```text
//! cargo run --release -p psxw-gpu-oracle --example gpubench [-- --repeat N]
//! ```

use anyhow::Result;
use psxw_inputs::Workspace;
use psxw_oracle::commands::DeviceUnderTest;
use psxw_oracle::vector::Kind;
use std::collections::BTreeMap;
use std::time::Instant;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let repeat: usize = args.iter().position(|a| a == "--repeat").and_then(|i| args.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(1);
    let ws = Workspace::discover()?;
    let store = psxw_oracle::commands::store(&ws);
    let mut groups: BTreeMap<String, Vec<psxw_oracle::Vector>> = BTreeMap::new();
    for set in store.set_names()? {
        for id in store.set(&set)? {
            let v = store.vector(&id)?;
            if matches!(v.kind(), Kind::GpuDraw | Kind::GpuTransfer) {
                let g = if set.starts_with("synth-gpu-") { "synthetic".to_string() } else { set.clone() };
                groups.entry(g).or_default().push(v);
            }
        }
    }
    let mut dev = psxw_gpu_oracle::GpuDevice::new(&ws);
    let t = Instant::now();
    let name = dev.name();
    println!("startup (image check, server build check, container start): {:.2?}\n{name}", t.elapsed());
    println!("{:<12} {:>8} {:>12} {:>12} {:>12}", "set", "vectors", "front end", "total", "per vector");
    for (g, vs) in &groups {
        let mut fe = std::time::Duration::ZERO;
        let t = Instant::now();
        for _ in 0..repeat {
            for v in vs {
                let f = Instant::now();
                psxw_gpu_oracle::replay(v, &store)?;
                fe += f.elapsed();
                dev.run(v, &store)?;
            }
        }
        let total = t.elapsed();
        let n = (vs.len() * repeat) as u32;
        // run() replays again; count the front end once.
        println!("{g:<12} {:>8} {:>12.2?} {:>12.2?} {:>12.2?}", n, fe, total - fe, (total - fe) / n);
    }
    Ok(())
}
