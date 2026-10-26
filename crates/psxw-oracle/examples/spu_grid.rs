//! Calibration tool (not part of the harness): grid-search the readback
//! constants of `psxw_oracle::spu::Timing` against probe results.
//!
//! `spu_grid [--timing k=v,...] [--regs-only|--caps-only] name=lo:hi:step ... -- DIR...`
//!
//! Per vector it records the registers and the captured RAM after every
//! tick around the end of the run, then scores each combination of the
//! readback constants (end, end_after_write, read_gap, data_skip,
//! capture_start, word, capture_gap, phase) by lookups.

use anyhow::{Result, bail};
use psxw_oracle::Store;
use psxw_oracle::spu::{Timing, at_edge, written};
use psxw_oracle::vector::{SpuRam, SpuRun, SpuWrite};
use serde_json::Value;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

struct Case {
    run: SpuRun,
    want: Vec<u8>,
    last: Option<f64>,
    /// First tick in the tables.
    m0: u64,
    regs: Vec<Vec<u16>>,
    ram: Vec<Vec<u16>>,
    /// Captured RAM halfword index -> column in `ram`.
    cols: std::collections::HashMap<usize, usize>,
}

const SPAN: u64 = 160;

fn score(c: &Case, t: &Timing, regs_on: bool, caps_on: bool) -> (usize, usize) {
    let at = |time: f64| -> usize {
        let m = ((time + t.phase) / 768.0).floor() as i64;
        (m - c.m0 as i64).clamp(0, c.regs.len() as i64 - 1) as usize
    };
    let end = 768.0 * c.run.samples as f64 + t.end;
    let _ = c.last;
    let mut last: Option<f64> = None;
    for w in &c.run.writes {
        let at = 768.0 * w.sample as f64 + t.write;
        last = Some(match last {
            Some(p) if p + t.write_gap > at => p + t.write_gap,
            _ => at,
        });
    }
    let start = match last {
        Some(p) if p + t.end_after_write > end => p + t.end_after_write,
        _ => end,
    };
    let mut bad_regs = 0;
    let mut now = start;
    for j in 0..304usize {
        let off = 2 * j;
        if off == 0x1A8 {
            now -= t.data_skip;
        } else if regs_on {
            let v = c.regs[at(now)][j];
            if v != u16::from_le_bytes([c.want[off], c.want[off + 1]]) {
                bad_regs += 1;
            }
        }
        now += t.read_gap;
    }
    let mut bad_caps = 0;
    if caps_on {
        let mut pos = 0x260;
        for (a, m) in capture_ticks(c, t, now) {
            let k = (m as i64 - c.m0 as i64).clamp(0, c.ram.len() as i64 - 1) as usize;
            if c.ram[k][c.cols[&a]] != u16::from_le_bytes([c.want[pos], c.want[pos + 1]]) {
                bad_caps += 1;
            }
            pos += 2;
        }
    }
    (bad_regs, bad_caps)
}

/// For each captured halfword: its RAM index and the tick after which the
/// DMA reads it (as `psxw_oracle::spu::readback` does).
fn capture_ticks(c: &Case, t: &Timing, now: f64) -> Vec<(usize, u64)> {
    let mut out = Vec::new();
    let mut m = 1 + ((now - t.read_gap + t.capture_start + t.phase) / 768.0).floor() as u64;
    for &(addr, len) in &c.run.captures {
        if len == 0 {
            continue;
        }
        let halfwords = len / 2;
        let loads = halfwords.div_ceil(32);
        for h in 0..halfwords {
            out.push((((addr / 2 + h) & 0x3FFFF) as usize, m + (h / 32) as u64));
        }
        m += loads as u64 + t.capture_gap as u64;
    }
    out
}

/// Per element (snapshot registers, then captured halfwords): whether the
/// model matches the golden under `t`.
fn elements(c: &Case, t: &Timing) -> Vec<bool> {
    let at = |time: f64| -> usize {
        let m = ((time + t.phase) / 768.0).floor() as i64;
        (m - c.m0 as i64).clamp(0, c.regs.len() as i64 - 1) as usize
    };
    let end = 768.0 * c.run.samples as f64 + t.end;
    let mut last: Option<f64> = None;
    for w in &c.run.writes {
        let at = 768.0 * w.sample as f64 + t.write;
        last = Some(match last {
            Some(p) if p + t.write_gap > at => p + t.write_gap,
            _ => at,
        });
    }
    let start = match last {
        Some(p) if p + t.end_after_write > end => p + t.end_after_write,
        _ => end,
    };
    let mut out = Vec::new();
    let mut now = start;
    for j in 0..304usize {
        let off = 2 * j;
        if off == 0x1A8 {
            now -= t.data_skip;
            out.push(true);
        } else {
            out.push(c.regs[at(now)][j] == u16::from_le_bytes([c.want[off], c.want[off + 1]]));
        }
        now += t.read_gap;
    }
    let mut pos = 0x260;
    for (a, m) in capture_ticks(c, t, now) {
        let k = (m as i64 - c.m0 as i64).clamp(0, c.ram.len() as i64 - 1) as usize;
        out.push(c.ram[k][c.cols[&a]] == u16::from_le_bytes([c.want[pos], c.want[pos + 1]]));
        pos += 2;
    }
    out
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut base = Timing::default();
    let (mut regs_on, mut caps_on) = (true, true);
    let mut grid: Vec<(String, Vec<f64>)> = Vec::new();
    while let Some(a) = args.first().cloned() {
        args.remove(0);
        match a.as_str() {
            "--timing" => base = base.with(&args.remove(0))?,
            "--regs-only" => caps_on = false,
            "--caps-only" => regs_on = false,
            "--" => break,
            spec => {
                let Some((name, range)) = spec.split_once('=') else { bail!("bad grid spec {spec}") };
                let p: Vec<f64> = range.split(':').map(|x| x.parse().unwrap()).collect();
                let mut vals = Vec::new();
                let mut v = p[0];
                while v <= p[1] + 1e-9 {
                    vals.push(v);
                    v += p[2];
                }
                grid.push((name.to_string(), vals));
            }
        }
    }
    let ws = psxw_inputs::Workspace::discover()?;
    let store = Store::new(psxw_oracle::images::oracle_dir(&ws));
    let mut cases = Vec::new();
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
            let captures = s["captures"].as_array().into_iter().flatten().map(|c| (c[0].as_u64().unwrap() as u32, c[1].as_u64().unwrap() as u32)).collect();
            let run = SpuRun { ram, writes, samples: s["samples"].as_u64().unwrap() as u32, captures };
            let mut w = written(at_edge(&run, &store)?, &run, &base);
            let m0 = run.samples as u64 - 4;
            w.step_to(m0);
            let (mut regs, mut ramt) = (Vec::new(), Vec::new());
            let mut cols = std::collections::HashMap::new();
            let mut addrs = Vec::new();
            for &(a, len) in &run.captures {
                for h in 0..len / 2 {
                    let i = ((a / 2 + h) & 0x3FFFF) as usize;
                    if !cols.contains_key(&i) {
                        cols.insert(i, addrs.len());
                        addrs.push(i);
                    }
                }
            }
            for m in m0..m0 + SPAN {
                w.step_to(m);
                regs.push((0..304u32).map(|j| if 2 * j == 0x1A8 { 0 } else { w.spu().read(2 * j) }).collect());
                ramt.push(addrs.iter().map(|&i| w.spu().ram()[i]).collect());
            }
            cases.push(Case { last: w.last_write(), run, want, m0, regs, ram: ramt, cols });
        }
    }
    eprintln!("{} cases", cases.len());
    if std::env::var("SPU_ELEMENTS").is_ok() {
        // Per mismatching element at phase 0: the nearest phase (by
        // absolute value) at which it matches. The largest is the model's
        // worst error, jitter included.
        let mut worst: Vec<(i32, usize, usize)> = Vec::new();
        for (i, c) in cases.iter().enumerate() {
            let per: Vec<Vec<bool>> = (-400..=400).map(|ph| elements(c, &base.with(&format!("phase={ph}")).unwrap())).collect();
            let n = per[0].len();
            for e in 0..n {
                if per[400][e] {
                    continue;
                }
                let d = (1..=400).find(|d| per[400 + d][e] || per[400 - d][e]).map(|d| d as i32).unwrap_or(9999);
                worst.push((d, i, e));
            }
        }
        worst.sort();
        let mut hopeless = std::collections::BTreeMap::new();
        for w in worst.iter().filter(|w| w.0 == 9999) {
            *hopeless.entry(w.1).or_insert(0) += 1;
        }
        println!("cases with elements no phase fixes (case: count): {hopeless:?}");
        worst.retain(|w| w.0 < 9999);
        for w in worst.iter().rev().take(25) {
            println!("distance {:5} case {:3} element {}", w.0, w.1, w.2);
        }
        println!("{} mismatching elements", worst.len());
        return Ok(());
    }
    if std::env::var("SPU_PHASES").is_ok() {
        // Per case: the phases (cycles, step 1) at which it matches exactly.
        let mut all = Vec::new();
        for (i, c) in cases.iter().enumerate() {
            let ok: Vec<i32> = (-300..=300).filter(|ph| {
                let t = base.with(&format!("phase={ph}")).unwrap();
                score(c, &t, regs_on, caps_on) == (0, 0)
            }).collect();
            match (ok.first(), ok.last()) {
                (Some(a), Some(b)) => {
                    println!("case {i}: phase {a}..{b} ({} values)", ok.len());
                    all.push((*a, *b));
                }
                _ => println!("case {i}: no phase matches"),
            }
        }
        let lo = all.iter().map(|x| x.0).max();
        let hi = all.iter().map(|x| x.1).min();
        println!("intersection {lo:?}..{hi:?}");
        return Ok(());
    }
    let mut results: Vec<((usize, usize), Vec<f64>)> = Vec::new();
    let mut idx = vec![0usize; grid.len()];
    loop {
        let mut t = base;
        let mut combo = Vec::new();
        for (k, (name, vals)) in grid.iter().enumerate() {
            let v = vals[idx[k]];
            combo.push(v);
            t = t.with(&format!("{name}={v}"))?;
        }
        let mut tot = (0, 0);
        for c in &cases {
            let (a, b) = score(c, &t, regs_on, caps_on);
            tot.0 += a;
            tot.1 += b;
        }
        results.push((tot, combo));
        let mut k = 0;
        loop {
            if k == grid.len() {
                break;
            }
            idx[k] += 1;
            if idx[k] < grid[k].1.len() {
                break;
            }
            idx[k] = 0;
            k += 1;
        }
        if k == grid.len() {
            break;
        }
    }
    results.sort_by_key(|r| r.0.0 + r.0.1);
    let names: Vec<&str> = grid.iter().map(|g| g.0.as_str()).collect();
    for (s, combo) in results.iter().take(15) {
        let kv: Vec<String> = names.iter().zip(combo).map(|(n, v)| format!("{n}={v}")).collect();
        println!("regs {:5} caps {:6}  {}", s.0, s.1, kv.join(","));
    }
    Ok(())
}
