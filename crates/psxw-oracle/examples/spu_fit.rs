//! Calibration tool (not part of the harness): run psx-spu through the
//! runner model on SPU_RUN vectors and report where it differs from the
//! emulator's results.
//!
//! `spu_fit [--quiet] [--detail] [--timing k=v,...] [--dump DIR]
//!          [--export DIR] [--limit N] (DIR | --set NAME)...`
//!
//! A DIR is a `spu_probe` output directory (spec.json plus N.res); `--set`
//! takes a store set and this pin's emulator goldens. `--export` writes the
//! failing set vectors as a probe directory (for scripts).

use anyhow::Result;
use psxw_oracle::spu::{Timing, run};
use psxw_oracle::target::emulator_target_id;
use psxw_oracle::vector::{SpuRam, SpuRun, SpuWrite, Vector};
use psxw_oracle::Store;
use serde_json::{Value, json};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

fn from_probe(store: &Store, s: &Value) -> Result<SpuRun> {
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
    Ok(SpuRun { ram, writes, samples: s["samples"].as_u64().unwrap() as u32, captures })
}

fn to_probe(store: &Store, r: &SpuRun) -> Result<Value> {
    let mut ram = Vec::new();
    for x in &r.ram {
        let b = store.blob(&x.bytes)?;
        ram.push(json!({"addr": x.addr, "hex": b.iter().map(|v| format!("{v:02x}")).collect::<String>()}));
    }
    let writes: Vec<Value> = r.writes.iter().map(|w| json!([w.sample, w.reg, w.value])).collect();
    let caps: Vec<Value> = r.captures.iter().map(|c| json!([c.0, c.1])).collect();
    Ok(json!({"spu": {"samples": r.samples, "writes": writes, "ram": ram, "captures": caps}}))
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut timing = Timing::default();
    let (mut quiet, mut detail) = (false, false);
    let mut dump: Option<std::path::PathBuf> = None;
    let mut export: Option<std::path::PathBuf> = None;
    let mut limit = usize::MAX;
    let mut sources = Vec::new();
    while !args.is_empty() {
        let a = args.remove(0);
        match a.as_str() {
            "--timing" => timing = timing.with(&args.remove(0))?,
            "--quiet" => quiet = true,
            "--detail" => detail = true,
            "--dump" => dump = Some(args.remove(0).into()),
            "--export" => export = Some(args.remove(0).into()),
            "--limit" => limit = args.remove(0).parse()?,
            "--set" => sources.push((true, args.remove(0))),
            _ => sources.push((false, a)),
        }
    }
    let ws = psxw_inputs::Workspace::discover()?;
    let store = Store::new(psxw_oracle::images::oracle_dir(&ws));
    let pins = ws.lock.oracle.as_ref().unwrap();
    let emu = emulator_target_id(pins.beetle.short(), pins.openbios.short());
    let mut cases: Vec<(String, SpuRun, Vec<u8>)> = Vec::new();
    for (is_set, src) in &sources {
        if *is_set {
            for id in store.set(src)? {
                if let (Vector::SpuRun(r), Some((want, _))) = (store.vector(&id)?, store.golden(&emu, &id)?) {
                    cases.push((format!("{src}/{}", &id.hex()[..12]), r, want));
                }
            }
        } else {
            let dir = std::path::PathBuf::from(src);
            let spec: Value = serde_json::from_slice(&std::fs::read(dir.join("spec.json"))?)?;
            for (i, item) in spec.as_array().unwrap().iter().enumerate() {
                let Some(s) = item.get("spu") else { continue };
                let Ok(want) = std::fs::read(dir.join(format!("{i}.res"))) else { continue };
                cases.push((format!("{}/{i}", dir.file_name().unwrap().to_string_lossy()), from_probe(&store, s)?, want));
            }
        }
    }
    let (mut total, mut bad) = (0, 0);
    let mut exported = Vec::new();
    for (i, (label, v, want)) in cases.iter().enumerate() {
        let got = run(v, &store, &timing)?;
        if let Some(d) = &dump {
            std::fs::create_dir_all(d)?;
            std::fs::write(d.join(format!("{i}.res")), &got)?;
        }
        total += 1;
        if got == *want {
            continue;
        }
        bad += 1;
        if let Some(d) = &export {
            std::fs::create_dir_all(d)?;
            std::fs::write(d.join(format!("{}.res", exported.len())), want)?;
            std::fs::create_dir_all(d.join("model"))?;
            std::fs::write(d.join("model").join(format!("{}.res", exported.len())), &got)?;
            exported.push(to_probe(&store, v)?);
        }
        if quiet || bad > limit {
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
        for (a, n) in &v.captures {
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
        println!("{label}: regs [{}] {}", regs.join(" "), caps.join("; "));
    }
    if let Some(d) = &export {
        std::fs::write(d.join("spec.json"), serde_json::to_vec(&exported)?)?;
    }
    println!("{bad} of {total} differ");
    Ok(())
}
