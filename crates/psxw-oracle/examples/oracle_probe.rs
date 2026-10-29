//! Fitting aid for the MDEC, CD controller and BIOS thread vectors: one
//! vector's target result next to psx-wasm's, word by word (CD results
//! interrupt by interrupt).
//!
//! `cargo run --release -p psxw-oracle --example oracle_probe -- set <set> <index>`
//! runs vector `index` of a stored set (in id order) through the emulator
//! target; `… -- threads "op,a,b,c" …` makes a BIOS_THREADS vector from
//! the steps given. Environment:
//! - `PROBE_XCHG=<file>`: only write the vector's exchange file, for an
//!   emulator run by hand (e.g. with a runner built outside `images build`);
//! - `PROBE_XRES=<file>`: compare that run's result file instead of running;
//! - `PROBE_DUMP=<dir>`: also write both results there (`got.bin`, `ours.bin`);
//! - `PROBE_ALL=1`: print every word, not only the first differences.

use psxw_inputs::Workspace;
use psxw_oracle::commands::DeviceUnderTest;
use psxw_oracle::store::Store;
use psxw_oracle::target::{DockerTarget, run_vectors};
use psxw_oracle::vector::{BiosThreads, CdCmds, Vector};
use psxw_oracle::{script, wire};

fn num(s: &str) -> anyhow::Result<u32> {
    Ok(match s.strip_prefix("0x") {
        Some(h) => u32::from_str_radix(h, 16)?,
        None => s.parse()?,
    })
}

fn words(b: &[u8]) -> Vec<u32> {
    b.chunks(4).map(|c| c.iter().rev().fold(0u32, |a, &x| a << 8 | x as u32)).collect()
}

fn ours(v: &Vector, store: &Store) -> anyhow::Result<Vec<u8>> {
    match v {
        Vector::BiosThreads(_) => psxw_oracle::threads::Device.run(v, store),
        Vector::MdecRun(_) => psxw_oracle::mdec::Device.run(v, store),
        Vector::CdCmds(_) => psxw_oracle::cdc::Device.run(v, store),
        _ => anyhow::bail!("no device for a {} vector", v.kind()),
    }
}

fn show_words(got: &[u8], ours: &[u8]) {
    let (got, ours) = (words(got), words(ours));
    let n = got.len().max(ours.len());
    let diff: Vec<usize> = (0..n).filter(|&i| got.get(i) != ours.get(i)).collect();
    println!("{} words, {} differ (target, then psx-wasm)", got.len(), diff.len());
    let all = std::env::var("PROBE_ALL").is_ok();
    let f = |w: Option<&u32>| w.map_or("--------".to_string(), |w| format!("{w:08x}"));
    for i in 0..n {
        if all || diff.iter().take(40).any(|&d| d == i) {
            println!("{i:5}: {}  {}{}", f(got.get(i)), f(ours.get(i)), if diff.contains(&i) { "  <>" } else { "" });
        }
    }
}

fn show_cd(c: &CdCmds, got: &[u8], ours: &[u8]) {
    let rd = |b: &[u8], p: usize| u32::from_le_bytes([b[p], b[p + 1], b[p + 2], b[p + 3]]);
    let records: usize = c.cmds.iter().map(|k| k.ints as usize).sum();
    println!("interrupt enable as found: {:02x} (psx-wasm {:02x})", rd(got, 0), rd(ours, 0));
    let (mut pos, mut rec) = (8usize, 0usize);
    let times = got.len() - 4 * records;
    for k in &c.cmds {
        for _ in 0..k.ints {
            let size = (wire::CD_RECORD_SIZE + k.data) as usize;
            let fmt = |b: &[u8]| {
                let t = rd(b, pos);
                let resp: Vec<String> = b[pos + 4..pos + 4 + ((t >> 8) as usize).min(16)].iter().map(|x| format!("{x:02x}")).collect();
                format!("INT{} [{}] @{}", t & 0xFF, resp.join(" "), rd(b, times + 4 * rec) * wire::CD_TICK_CYCLES)
            };
            let same = got[pos..pos + size] == ours[pos..pos + size];
            println!("cmd {:02x} {:02x?}: {}  |  {}{}", k.cmd, k.params, fmt(got), fmt(ours), if same { "" } else { "   <>" });
            pos += size;
            rec += 1;
        }
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ws = Workspace::open(std::path::Path::new("."))?;
    let store = Store::new(ws.root.join(".oracle"));
    let v = match args.first().map(String::as_str) {
        Some("threads") => {
            let mut steps = Vec::new();
            for s in &args[1..] {
                let v = s.split(',').map(num).collect::<anyhow::Result<Vec<u32>>>()?;
                anyhow::ensure!(v.len() == 4, "a step is op,a,b,c");
                steps.push([v[0], v[1], v[2], v[3]]);
            }
            Vector::BiosThreads(BiosThreads { pc: psxw_oracle::threads::PC, tcbs: psxw_oracle::threads::TCBS, steps })
        }
        Some("set") if args.len() == 3 => {
            let ids: Vec<_> = store.set(&args[1])?.into_iter().collect();
            store.vector(ids.get(args[2].parse::<usize>()?).ok_or_else(|| anyhow::anyhow!("no such index"))?)?
        }
        _ => anyhow::bail!("usage: oracle_probe set NAME INDEX | threads STEP…"),
    };
    let ops = script::compile(&v, &store)?;
    if let Ok(path) = std::env::var("PROBE_XCHG") {
        std::fs::write(path, script::encode_exchanges(&script::pack(std::slice::from_ref(&ops)).batches))?;
        return Ok(());
    }
    let got = if let Ok(path) = std::env::var("PROBE_XRES") {
        let plan = script::pack(std::slice::from_ref(&ops));
        let mut res = Vec::new();
        for (x, slots) in script::decode_results(&std::fs::read(path)?)?.iter().zip(&plan.slots) {
            res.extend(script::split_batch(x, slots.len())?);
        }
        script::assemble(&ops, &res).map_err(|e| format!("{e:#}"))
    } else {
        run_vectors(&mut DockerTarget::open(&ws)?, &store, &[(v.id(), v.clone())])?.remove(0)
    };
    let got = match got {
        Ok(g) => g,
        Err(e) => anyhow::bail!("target: {e}"),
    };
    let ours = ours(&v, &store)?;
    if let Ok(dir) = std::env::var("PROBE_DUMP") {
        std::fs::write(format!("{dir}/got.bin"), &got)?;
        std::fs::write(format!("{dir}/ours.bin"), &ours)?;
    }
    match &v {
        Vector::CdCmds(c) => show_cd(c, &got, &ours),
        _ => show_words(&got, &ours),
    }
    Ok(())
}
