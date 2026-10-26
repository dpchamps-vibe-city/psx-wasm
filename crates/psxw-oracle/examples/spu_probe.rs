//! Calibration probe (not part of the harness): run a JSON list of SPU_RUN
//! vectors, interleaved with ECHO ops that shift the runner's timing, on
//! the emulator target, and write each result to `<out>/<index>.res`.
//!
//! Spec: `[{"echo": nbytes} | {"spu": {"samples": n, "ram": [{"addr": a,
//! "hex": "..."}], "writes": [[sample, reg, value], ...], "captures":
//! [[addr, len], ...]}}, ...]`.

use anyhow::{Context, Result};
use psxw_oracle::Store;
use psxw_oracle::target::{DockerTarget, run_vectors};
use psxw_oracle::vector::{SpuRam, SpuRun, SpuWrite, Vector};
use serde_json::Value;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let spec: Value = serde_json::from_slice(&std::fs::read(&args[1])?)?;
    let out = std::path::PathBuf::from(&args[2]);
    std::fs::create_dir_all(&out)?;
    let ws = psxw_inputs::Workspace::discover()?;
    let store = Store::new(psxw_oracle::images::oracle_dir(&ws));
    let mut vectors = Vec::new();
    let mut names = Vec::new();
    for (i, item) in spec.as_array().context("spec is a list")?.iter().enumerate() {
        if item.get("gpu").is_some() {
            // A small fill: evicts the runner's SPU_RUN code from the I-cache.
            vectors.push(Vector::GpuDraw(psxw_oracle::vector::GpuDraw {
                init: vec![],
                state: vec![],
                mode: psxw_oracle::vector::GpuMode::Port,
                packets: vec![vec![0x0200_0000, 0, 0x0010_0010]],
                readback: vec![],
            }));
            names.push(None);
            continue;
        }
        if item.get("gte").is_some() {
            vectors.push(Vector::GteOp(psxw_oracle::vector::GteOp { data: [0; 32], ctrl: [0; 32], cmd: 0x0018_0001 }));
            names.push(None);
            continue;
        }
        if let Some(n) = item.get("echo") {
            vectors.push(Vector::Echo(vec![0x55; n.as_u64().unwrap() as usize]));
            names.push(None);
            continue;
        }
        let s = &item["spu"];
        let mut ram = Vec::new();
        for r in s["ram"].as_array().into_iter().flatten() {
            let bytes = store.put_blob(&hex(r["hex"].as_str().unwrap()))?;
            ram.push(SpuRam { addr: r["addr"].as_u64().unwrap() as u32, bytes });
        }
        let writes = s["writes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|w| SpuWrite { sample: w[0].as_u64().unwrap() as u32, reg: w[1].as_u64().unwrap() as u32, value: w[2].as_u64().unwrap() as u16 })
            .collect();
        let captures = s["captures"].as_array().into_iter().flatten().map(|c| (c[0].as_u64().unwrap() as u32, c[1].as_u64().unwrap() as u32)).collect();
        vectors.push(Vector::SpuRun(SpuRun { ram, writes, samples: s["samples"].as_u64().unwrap() as u32, captures }));
        names.push(Some(i));
    }
    let mut target = DockerTarget::open(&ws)?;
    let list: Vec<_> = vectors.iter().map(|v| (v.id(), v.clone())).collect();
    let results = run_vectors(&mut target, &store, &list)?;
    let mut errors = String::new();
    for (name, res) in names.iter().zip(results) {
        let Some(i) = name else { continue };
        match res {
            Ok(bytes) => std::fs::write(out.join(format!("{i}.res")), bytes)?,
            Err(e) => errors.push_str(&format!("{i}: {e}\n")),
        }
    }
    std::fs::write(out.join("errors.txt"), &errors)?;
    eprint!("{errors}");
    Ok(())
}
