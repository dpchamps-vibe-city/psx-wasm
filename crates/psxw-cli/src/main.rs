//! `psxw`: the driver.
//!
//! ```text
//! psxw toolchain fetch                  download + verify pinned clang
//! psxw inputs sync                      snapshot every pinned repo
//! psxw census <game> [--only S] [--only-header S] [--no-probe]
//!                                       M0: compile every TU, check vs original
//! psxw lift <game> [--function F | --container C | --all]
//!                                       M1: translate every function to C
//! ```

use anyhow::{Result, bail};
use psxw_inputs::{Workspace, snapshot, toolchain};

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
        ["lift", game, rest @ ..] => {
            let mut opts = psxw_lift::run::Options { game: game.to_string(), function: None, container: None };
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--function" => opts.function = it.next().map(|s| s.to_string()),
                    "--container" => opts.container = it.next().map(|s| s.to_string()),
                    "--all" => {}
                    other => bail!("unknown lift option {other}"),
                }
            }
            let (report, json) = psxw_lift::run::run(&ws, &opts)?;
            print!("{}", report.summary());
            eprintln!("\nfull report: {}", json.display());
            if !report.verdict.pass {
                std::process::exit(2);
            }
        }
        ["disasm", game, container, addr, rest @ ..] => {
            let inputs = psxw_lift::run::load(&ws, game)?;
            let start = psxw_inputs::parse_u32(addr).ok_or_else(|| anyhow::anyhow!("bad address {addr}"))?;
            let count = rest.first().and_then(|n| psxw_inputs::parse_u32(n)).unwrap_or(32);
            for i in 0..count {
                let pc = start + 4 * i;
                let Some(w) = inputs.originals.word(container, pc) else { break };
                let insn = psxw_lift::decode::decode(w);
                let mark = match inputs.program.code(container).and_then(|c| c.starts.get(&pc)) {
                    Some(s) => format!("  <{}>", s.name.as_deref().unwrap_or("start")),
                    None => String::new(),
                };
                println!("{pc:08X}: {w:08X}  {}{mark}", psxw_lift::decode::disasm(&insn, pc));
            }
        }
        _ => bail!(
            "usage: psxw toolchain fetch | inputs sync | census <game> [--only S] [--only-header S] [--no-probe] | lift <game> [--function F | --container C | --all]"
        ),
    }
    Ok(())
}
