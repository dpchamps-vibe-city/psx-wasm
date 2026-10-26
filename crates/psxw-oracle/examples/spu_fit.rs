//! Calibration tool (not part of the harness): run psx-spu through the
//! runner model on probe vectors (`spu_probe` output directories) and
//! report where it differs from the emulator's results.
//!
//! `spu_fit [--quiet] [--timing k=v,...] DIR...`

use anyhow::Result;
use psxw_oracle::Store;
use psxw_oracle::spu::{Timing, run};
use psxw_oracle::vector::{SpuRam, SpuRun, SpuWrite};
use serde_json::Value;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut timing = Timing::default();
    let mut quiet = false;
    let mut detail = false;
    let mut dump: Option<std::path::PathBuf> = None;
    while let Some(a) = args.first().cloned() {
        match a.as_str() {
            "--timing" => {
                timing = timing.with(&args[1])?;
                args.drain(..2);
            }
            "--quiet" => {
                quiet = true;
                args.remove(0);
            }
            "--detail" => {
                detail = true;
                args.remove(0);
            }
            "--dump" => {
                dump = Some(args[1].clone().into());
                args.drain(..2);
            }
            _ => break,
        }
    }
    let ws = psxw_inputs::Workspace::discover()?;
    let store = Store::new(psxw_oracle::images::oracle_dir(&ws));
    let (mut total, mut bad) = (0, 0);
    for dir in &args {
        let dir = std::path::PathBuf::from(dir);
        let spec: Value = serde_json::from_slice(&std::fs::read(dir.join("spec.json"))?)?;
        for (i, item) in spec.as_array().unwrap().iter().enumerate() {
            let Some(s) = item.get("spu") else { continue };
            let Ok(want) = std::fs::read(dir.join(format!("{i}.res"))) else { continue };
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
            let captures: Vec<(u32, u32)> =
                s["captures"].as_array().into_iter().flatten().map(|c| (c[0].as_u64().unwrap() as u32, c[1].as_u64().unwrap() as u32)).collect();
            let v = SpuRun { ram, writes, samples: s["samples"].as_u64().unwrap() as u32, captures: captures.clone() };
            let got = run(&v, &store, &timing)?;
            if let Some(d) = &dump {
                std::fs::create_dir_all(d)?;
                std::fs::write(d.join(format!("{i}.res")), &got)?;
            }
            total += 1;
            if got == want {
                continue;
            }
            bad += 1;
            if quiet {
                continue;
            }
            let mut regs = Vec::new();
            for off in (0..0x260).step_by(2) {
                let (g, w) = (u16::from_le_bytes([got[off], got[off + 1]]), u16::from_le_bytes([want[off], want[off + 1]]));
                if g != w {
                    regs.push(format!("{off:03x}:{g:04x}/{w:04x}"));
                }
            }
            let mut pos = 0x260;
            let mut caps = Vec::new();
            for (a, n) in &captures {
                let (g, w) = (&got[pos..pos + *n as usize], &want[pos..pos + *n as usize]);
                let diffs: Vec<usize> = (0..*n as usize / 2).filter(|k| g[2 * k..2 * k + 2] != w[2 * k..2 * k + 2]).collect();
                if !diffs.is_empty() {
                    let k = diffs[0];
                    let h = |b: &[u8], k: usize| i16::from_le_bytes([b[2 * k], b[2 * k + 1]]);
                    caps.push(format!("cap {a:x}: {} differ, first at +{:x} got {} want {}", diffs.len(), 2 * k, h(g, k), h(w, k)));
                    if detail {
                        let lo = k.saturating_sub(4);
                        let hi = (k + 12).min(*n as usize / 2);
                        caps.push(format!("   got  {:?}", (lo..hi).map(|k| h(g, k)).collect::<Vec<_>>()));
                        caps.push(format!("   want {:?}", (lo..hi).map(|k| h(w, k)).collect::<Vec<_>>()));
                    }
                }
                pos += *n as usize;
            }
            println!("{}/{i}: regs [{}] {}", dir.file_name().unwrap().to_string_lossy(), regs.join(" "), caps.join("; "));
        }
    }
    println!("{bad} of {total} differ");
    Ok(())
}
