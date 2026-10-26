//! XA calibration aid: dump one captured XA vector's rings and psx-wasm's
//! decoded stream (raw little-endian i16) for analysis.
//! `cargo run --release -p psxw-oracle --example xa_probe -- <vector index> <phase> <out dir>`

use psxw_oracle::Store;
use psxw_oracle::vector::Vector;
use psxw_oracle::xa_disc;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let idx: usize = args.get(1).map_or(Ok(0), |s| s.parse())?;
    let phase: u32 = args.get(2).map_or(Ok(6), |s| s.parse())?;
    let out = std::path::PathBuf::from(args.get(3).cloned().unwrap_or_else(|| ".".into()));
    let store = Store::new(".oracle");
    let text = std::fs::read_to_string(".oracle/sets/synthetic-xa.ids")?;
    let ids: Vec<_> = text.lines().map(|l| psxw_oracle::VectorId::parse(l.trim())).collect::<anyhow::Result<_>>()?;
    let id = ids[idx];
    let Vector::CdXa(x) = store.vector(&id)? else { anyhow::bail!("not XA") };
    let target = std::fs::read_dir(".oracle/goldens")?
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .find(|n| n.starts_with("emu-"))
        .unwrap();
    let (golden, _) = store.golden(&target, &id)?.unwrap();
    let file = store.blob(&x.xa)?;
    let (l, r) = xa_disc::stream(&file, x.start, (x.file as u8, x.channel as u8), phase);
    let bytes = |v: &[i16]| v.iter().flat_map(|s| s.to_le_bytes()).collect::<Vec<u8>>();
    std::fs::create_dir_all(&out)?;
    std::fs::write(out.join("ring.bin"), &golden)?;
    std::fs::write(out.join("left.bin"), bytes(&l))?;
    std::fs::write(out.join("right.bin"), bytes(&r))?;
    println!("vector {idx}: start {} channel {} samples {}; stream {} samples", x.start, x.channel, x.samples, l.len());
    Ok(())
}
