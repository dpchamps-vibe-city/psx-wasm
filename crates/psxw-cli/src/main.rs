//! `psxw`: the driver.
//!
//! ```text
//! psxw toolchain fetch                  download + verify pinned clang
//! psxw inputs sync                      snapshot every pinned repo
//! psxw census <game> [--only S] [--only-header S] [--no-probe]
//!                                       M0: compile every TU, check vs original
//! psxw oracle …                         M3: device oracle harness (oracle.rs)
//! ```

use anyhow::{Result, bail};
use psxw_inputs::{Workspace, snapshot, toolchain};

mod oracle;

fn main() {
    if let Err(e) = run() {
        eprintln!("psxw: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ws = Workspace::discover()?;
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["toolchain", "fetch"] => {
            for name in ws.lock.toolchain.keys() {
                toolchain::fetch(&ws, name)?;
            }
        }
        ["inputs", "sync"] => {
            for (name, pin) in ws.lock.game.iter().chain(&ws.lock.sdk) {
                let dir = snapshot::ensure(&ws, name, pin)?;
                eprintln!("{name}: {} → {}", pin.short(), dir.display());
            }
        }
        ["census", game, rest @ ..] => {
            let mut opts = psxw_census::Options { game: game.to_string(), only: None, probe: true, only_header: None };
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--only" => opts.only = it.next().map(|s| s.to_string()),
                    "--no-probe" => opts.probe = false,
                    "--only-header" => opts.only_header = it.next().map(|s| s.to_string()),
                    other => bail!("unknown census option {other}"),
                }
            }
            let (report, json) = psxw_census::run(&ws, &opts)?;
            print!("{}", report.summary());
            eprintln!("\nfull report: {}", json.display());
            if !report.verdict.pass {
                std::process::exit(2);
            }
        }
        ["oracle", rest @ ..] => oracle::run(&ws, rest)?,
        _ => bail!("usage: psxw toolchain fetch | inputs sync | census <game> [--only S] [--only-header S] [--no-probe] | {}", oracle::USAGE),
    }
    Ok(())
}
