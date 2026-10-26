//! Debugging aid: a soak run of the synthetic GPU generators with another
//! seed and more vectors than the archived set, captured on the device
//! target and compared with psx-wasm's GPU on the spot. The vectors and
//! their goldens are stored in the set `soak` (which `check` does not
//! read), so `gpuvec --diff` can examine a mismatch.
//!
//! ```text
//! cargo run --release -p psxw-gpu-oracle --example gpusoak -- --seed N --per N [--only GEN]
//! ```

use anyhow::Result;
use psxw_inputs::Workspace;
use psxw_oracle::commands::DeviceUnderTest;
use psxw_oracle::gpu_synth;
use psxw_oracle::target::{DockerTarget, Target, run_vectors};
use std::collections::BTreeMap;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let seed = arg("--seed").and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok()).unwrap_or(0x50AC);
    let per: usize = arg("--per").and_then(|s| s.parse().ok()).unwrap_or(100);
    let only = arg("--only");
    let ws = Workspace::discover()?;
    let store = psxw_oracle::commands::store(&ws);
    let all: Vec<_> = gpu_synth::generate(&store, seed, per)?
        .into_iter()
        .filter(|s| only.as_deref().is_none_or(|o| o == s.generator))
        .collect();
    let mut vectors = Vec::new();
    for s in &all {
        vectors.push((store.put_vector(&s.vector)?, s.vector.clone()));
    }
    store.add_to_set("soak", vectors.iter().map(|(id, _)| *id))?;
    eprintln!("soak: {} vectors (seed {seed:#x}, {per} per generator)", vectors.len());
    let mut target = DockerTarget::open(&ws)?;
    let results = run_vectors(&mut target, &store, &vectors)?;
    let mut dev = psxw_gpu_oracle::GpuDevice::new(&ws);
    eprintln!("{}", dev.name());
    let mut table: BTreeMap<&str, (usize, usize, Vec<String>)> = BTreeMap::new();
    for ((id, v), (s, res)) in vectors.iter().zip(all.iter().zip(results)) {
        let e = table.entry(s.generator).or_default();
        e.0 += 1;
        let golden = match res {
            Ok(g) => g,
            Err(err) => {
                e.2.push(format!("{} target error: {err}", &id.hex()[..12]));
                continue;
            }
        };
        if store.golden(target.id(), id)?.is_none() {
            store.put_golden(id, &golden, &target.template().for_result(id, v.kind().name(), &golden))?;
        }
        let got = dev.run(v, &store)?;
        if got != golden {
            e.1 += 1;
            let n = got.iter().zip(&golden).filter(|(a, b)| a != b).count();
            e.2.push(format!("{} {n} bytes differ", &id.hex()[..12]));
        }
    }
    for (g, (n, bad, notes)) in &table {
        println!("{g:<16} {n:>5} vectors, {bad} differ");
        for note in notes.iter().take(5) {
            println!("    {note}");
        }
    }
    // Keep only the vectors that differ (and their goldens and blobs).
    let keep: std::collections::BTreeSet<String> =
        table.values().flat_map(|(_, _, notes)| notes.iter().map(|n| n[..12].to_string())).collect();
    let mut kept = Vec::new();
    for (id, _) in &vectors {
        let hex = id.hex();
        if keep.contains(&hex[..12]) || store.set("selftest")?.contains(id) {
            kept.push(*id);
            continue;
        }
        let _ = std::fs::remove_file(store.root().join("vectors").join(format!("{hex}.vec")));
        for ext in ["res", "json"] {
            let _ = std::fs::remove_file(store.golden_dir(target.id()).join(format!("{hex}.{ext}")));
        }
    }
    let _ = std::fs::remove_file(store.root().join("sets").join("soak.ids"));
    store.add_to_set("soak", kept)?;
    gc_blobs(&store)?;
    Ok(())
}

/// Delete blobs no stored vector references.
fn gc_blobs(store: &psxw_oracle::Store) -> Result<()> {
    let mut used = std::collections::BTreeSet::new();
    for id in store.vector_ids()? {
        for b in store.vector(&id)?.blobs() {
            used.insert(b.0.hex());
        }
    }
    let mut removed = 0;
    for e in std::fs::read_dir(store.root().join("blobs"))? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if !used.contains(&name) {
            std::fs::remove_file(e.path())?;
            removed += 1;
        }
    }
    eprintln!("soak: removed {removed} unreferenced blobs");
    Ok(())
}
