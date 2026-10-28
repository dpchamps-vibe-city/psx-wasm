//! `psxw oracle …`: the device oracle harness (M3).
//!
//! ```text
//! psxw oracle gen-header                regenerate oracle/include/psxw_oracle_wire.h
//! psxw oracle images build [--no-cache] build both images and the runner; record .oracle/images.json
//! psxw oracle images verify             check the recorded images against inputs.lock and docker
//! psxw oracle selftest [--seed N] [--count N]
//!                                       exact-by-construction GPU vectors through the emulator
//! psxw oracle capture [--kind K] [--missing]
//!                                       goldens for stored vectors (recapture checks determinism)
//! psxw oracle check <game> --device D   psx-wasm's device vs the goldens
//! psxw oracle ci                        images verify + selftest + capture (the oracle's CI gate)
//! psxw oracle play <game> …             the game under the emulator from a replay (wholegame.rs)
//! psxw oracle ramdiff <game> …          psx-wasm's RAM vs the emulator's, by symbol (wholegame.rs)
//! psxw oracle record <game> …           GPU vectors from a native run (record.rs)
//! psxw oracle export --out DIR [--set S]… [--kind K] [--missing-hw]
//!                                       batches for capture on a contributor's console (M10)
//! psxw oracle import DIR                their results, as hw-<rig> goldens
//! psxw oracle disagree                  emulator vs hardware goldens, vector by vector
//! ```

use anyhow::{Result, bail};
use psxw_inputs::Workspace;
use psxw_oracle::commands::{self, DeviceUnderTest};
use psxw_oracle::report::Report;
use psxw_oracle::{Kind, images};
use std::path::Path;

const USAGE: &str = "oracle <command> [arguments] [options]\n\nRun `psxw oracle --help` for commands and options.";

/// psx-wasm's device implementations the oracle can check. They start
/// lazily: only the checked device runs.
pub(crate) fn devices(ws: &Workspace) -> Vec<Box<dyn DeviceUnderTest>> {
    vec![
        Box::new(psxw_oracle::gte::Device),
        Box::new(psxw_gpu_oracle::GpuDevice::new(ws)),
        Box::new(psxw_oracle::spu::Device::default()),
        Box::new(psxw_oracle::xa_disc::Device),
    ]
}

fn finish(report: &Report, path: &Path) {
    print!("{}", report.summary());
    eprintln!("\nfull report: {}", path.display());
}

fn number(s: Option<&&str>, what: &str) -> Result<u64> {
    let Some(s) = s else { bail!("{what} needs a value") };
    match psxw_inputs::parse_u32(s) {
        Some(v) => Ok(v as u64),
        None => match s.strip_prefix("0x").map(|h| u64::from_str_radix(h, 16)) {
            Some(Ok(v)) => Ok(v),
            _ => bail!("{what}: bad number {s}"),
        },
    }
}

pub fn run(ws: &Workspace, args: &[&str]) -> Result<()> {
    match args {
        ["gen-header"] => {
            let path = ws.root.join(psxw_oracle::wire::C_HEADER_PATH);
            std::fs::write(&path, psxw_oracle::c_header())?;
            eprintln!("wrote {}", path.display());
        }
        ["images", "build", rest @ ..] => {
            let no_cache = match rest {
                [] => false,
                ["--no-cache"] => true,
                _ => bail!("usage: psxw oracle images build [--no-cache]"),
            };
            let im = images::build(ws, no_cache)?;
            println!("psxw-oracle-emu  {}", im.emu.id);
            println!("psxw-psx-sdk     {}", im.sdk.id);
            println!("bios sha1        {}", im.emu_facts.bios_sha1);
            println!("core             {}", im.emu_facts.core);
            for (name, sha) in &im.runner.files {
                println!("runner           {name} {sha}");
            }
            eprintln!("recorded {}", images::images_path(ws).display());
        }
        ["images", "verify"] => {
            let im = images::load(ws)?;
            images::verify(ws, &im)?;
            println!("images match inputs.lock: {} {}", im.emu.id, im.sdk.id);
        }
        ["selftest", rest @ ..] => {
            let (mut seed, mut count) = (commands::DEFAULT_SEED, commands::DEFAULT_SELFTEST);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--seed" => seed = number(it.next(), "--seed")?,
                    "--count" => count = number(it.next(), "--count")? as usize,
                    other => bail!("unknown selftest option {other}"),
                }
            }
            let (report, path) = commands::selftest(ws, seed, count)?;
            finish(&report, &path);
            if !report.pass {
                std::process::exit(2);
            }
        }
        ["capture", rest @ ..] => {
            let (mut kind, mut missing) = (None, false);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--kind" => match it.next() {
                        Some(k) => kind = Some(Kind::parse(k)?),
                        None => bail!("--kind needs a value"),
                    },
                    "--missing" => missing = true,
                    other => bail!("unknown capture option {other}"),
                }
            }
            let (report, path) = commands::capture(ws, kind, missing)?;
            finish(&report, &path);
            if !report.pass {
                std::process::exit(2);
            }
        }
        ["check", game, "--device", device] => {
            let mut impls = devices(ws);
            let checked = commands::check(ws, game, device, &mut impls)?;
            finish(&checked.report, &checked.path);
            if !checked.implemented {
                bail!("psx-wasm has no {device} implementation yet; nothing was compared");
            }
            if !checked.report.pass {
                std::process::exit(2);
            }
        }
        ["ci"] => {
            let mut pass = true;
            for (report, path) in commands::ci(ws, &mut devices(ws))? {
                finish(&report, &path);
                pass &= report.pass;
            }
            if !pass {
                std::process::exit(2);
            }
        }
        ["export", rest @ ..] => {
            let (mut out, mut sets, mut kind, mut missing) = (None, Vec::new(), None, false);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--out" => out = it.next().map(|s| std::path::PathBuf::from(*s)),
                    "--set" => sets.extend(it.next().map(|s| s.to_string())),
                    "--kind" => match it.next() {
                        Some(k) => kind = Some(Kind::parse(k)?),
                        None => bail!("--kind needs a value"),
                    },
                    "--missing-hw" => missing = true,
                    other => bail!("unknown export option {other}"),
                }
            }
            let Some(out) = out else { bail!("export needs --out DIR") };
            let store = psxw_oracle::Store::new(images::oracle_dir(ws));
            let mut ids: std::collections::BTreeSet<psxw_oracle::VectorId> = std::collections::BTreeSet::new();
            if sets.is_empty() {
                ids.extend(store.vector_ids()?);
            }
            for s in &sets {
                ids.extend(store.set(s)?);
            }
            let hw = psxw_oracle::batch::hardware_targets(&store);
            let mut chosen = Vec::new();
            for id in ids {
                if kind.is_some_and(|k| store.vector(&id).map(|v| v.kind() != k).unwrap_or(true)) {
                    continue;
                }
                if missing && hw.iter().any(|t| store.has_golden(t, &id)) {
                    continue;
                }
                chosen.push(id);
            }
            let im = images::load(ws)?;
            let m = psxw_oracle::batch::export(&store, &chosen, &ws.root.join(&im.runner.dir), &im.runner.files, &out)?;
            println!("exported {} vectors in {} batches to {}", m.vectors.len(), m.batches.len(), out.display());
        }
        ["import", dir] => {
            let store = psxw_oracle::Store::new(images::oracle_dir(ws));
            let r = psxw_oracle::batch::import(&store, Path::new(dir))?;
            println!("{}: {} goldens stored, {} vectors without one", r.target, r.stored, r.failed.len());
            for (id, why) in r.failed.iter().take(16) {
                println!("  {}: {why}", &id[..12]);
            }
        }
        ["disagree"] => {
            let store = psxw_oracle::Store::new(images::oracle_dir(ws));
            let pins = ws.lock.oracle.as_ref().ok_or_else(|| anyhow::anyhow!("inputs.lock has no [oracle] section"))?;
            let emu = psxw_oracle::target::emulator_target_id(pins.beetle.short(), pins.openbios.short());
            let d = psxw_oracle::batch::disagreements(&store, &emu, &psxw_oracle::batch::hardware_targets(&store))?;
            let path = ws.out_dir().join("oracle").join("disagreements.json");
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(&path, serde_json::to_string_pretty(&d)?)?;
            println!(
                "{} vs {:?}: {} compared, {} agree, {} differ, {} hardware-only",
                d.emulator,
                d.hardware,
                d.compared,
                d.agree,
                d.differ.len(),
                d.hardware_only
            );
            for x in d.differ.iter().take(16) {
                println!("  {} {} ({}): {}", &x.vector[..12], x.kind, x.hardware, x.detail);
            }
            eprintln!("full report: {}", path.display());
        }
        ["play", game, rest @ ..] => {
            if !crate::wholegame::play_cmd(ws, game, rest)? {
                std::process::exit(2);
            }
        }
        ["ramdiff", game, rest @ ..] => {
            if !crate::wholegame::ramdiff_cmd(ws, game, rest)? {
                std::process::exit(2);
            }
        }
        ["play" | "ramdiff", ..] => bail!("usage: psxw oracle {}", crate::wholegame::USAGE),
        ["record", game, rest @ ..] => crate::record::record_cmd(ws, game, rest)?,
        ["record", ..] => bail!("usage: psxw oracle {}", crate::record::USAGE),
        _ => bail!("usage: psxw {USAGE}"),
    }
    Ok(())
}
