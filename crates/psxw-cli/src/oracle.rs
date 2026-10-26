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
//! ```

use anyhow::{Result, bail};
use psxw_inputs::Workspace;
use psxw_oracle::commands::{self, DeviceUnderTest};
use psxw_oracle::report::Report;
use psxw_oracle::{Kind, images};
use std::path::Path;

pub const USAGE: &str = "oracle gen-header | images build [--no-cache] | images verify | selftest [--seed N] [--count N] \
                         | capture [--kind K] [--missing] | check <game> --device D | ci \
                         | play <game> … | ramdiff <game> …";

/// psx-wasm's device implementations the oracle can check (spu joins in
/// M6). They start lazily: only the checked device runs.
fn devices(ws: &Workspace) -> Vec<Box<dyn DeviceUnderTest>> {
    vec![Box::new(psxw_oracle::gte::Device), Box::new(psxw_gpu_oracle::GpuDevice::new(ws))]
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
            for (report, path) in commands::ci(ws)? {
                finish(&report, &path);
                pass &= report.pass;
            }
            if !pass {
                std::process::exit(2);
            }
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
        _ => bail!("usage: psxw {USAGE}"),
    }
    Ok(())
}
