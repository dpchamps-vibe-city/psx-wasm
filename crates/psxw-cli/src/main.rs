//! `psxw`: the driver.
//!
//! ```text
//! psxw toolchain fetch                  download + verify pinned clang
//! psxw inputs sync                      snapshot every pinned repo
//! psxw census <game> [--only S] [--only-header S] [--no-probe]
//!                                       M0: compile every TU, check vs original
//! psxw lift <game> [--function F | --container C | --all]
//!                                       M1: translate every function to C
//! psxw difftest <game> [--only S] [--seeds N] [--keep]
//!                                       M1: translated == decompiled C
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
        ["difftest", game, rest @ ..] => {
            let mut opts =
                psxw_difftest::Options { game: game.to_string(), only: None, seeds: 64, keep: false, verbose: false };
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--only" => opts.only = it.next().map(|s| s.to_string()),
                    "--seeds" => opts.seeds = it.next().and_then(|s| s.parse().ok()).unwrap_or(64),
                    "--keep" => opts.keep = true,
                    "--verbose" => opts.verbose = true,
                    other => bail!("unknown difftest option {other}"),
                }
            }
            let (report, json) = psxw_difftest::run(&ws, &opts)?;
            print!("{}", report.summary());
            eprintln!("\nfull report: {}", json.display());
            if !report.pass() {
                std::process::exit(2);
            }
        }
        ["protos", game] => {
            let manifest = ws.game(game)?;
            let pin = ws.game_pin(&manifest)?;
            let obj = ws.out_dir().join("census").join(game).join(pin.short()).join("obj");
            let clang = psxw_cc::Clang::new(ws.clang()?, obj.clone(), Vec::new(), Vec::new(), &obj.join("..").join("protos"))?;
            let mut files = Vec::new();
            for c in std::fs::read_dir(&obj)? {
                let c = c?.path();
                if !c.is_dir() {
                    continue;
                }
                for f in std::fs::read_dir(c)? {
                    let p = f?.path();
                    if p.extension().is_some_and(|e| e == "i") {
                        files.push(p);
                    }
                }
            }
            use rayon::prelude::*;
            let results: Vec<_> = files.par_iter().map(|p| (p.clone(), psxw_cc::protos::cached(&clang, p))).collect();
            let (mut n, mut byval, mut sret, mut variadic, mut failed) = (0, 0, 0, 0, 0);
            for (p, r) in &results {
                match r {
                    Ok(protos) => {
                        for pr in protos {
                            n += 1;
                            byval += pr.params.iter().filter(|k| matches!(k, psxw_cc::protos::Kind::ByVal { .. })).count();
                            sret += usize::from(pr.params.first().is_some_and(|k| matches!(k, psxw_cc::protos::Kind::Sret { .. })));
                            variadic += usize::from(pr.variadic);
                        }
                    }
                    Err(e) => {
                        failed += 1;
                        eprintln!("{}: {e:#}", p.display());
                    }
                }
            }
            println!("{} TUs, {n} prototypes, {byval} by-value params, {sret} struct returns, {variadic} variadic, {failed} failed", files.len());
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
        ["oracle", rest @ ..] => oracle::run(&ws, rest)?,
        _ => bail!(
            "usage: psxw toolchain fetch | inputs sync | census <game> [--only S] [--only-header S] [--no-probe] \
             | lift <game> [--function F | --container C | --all] | difftest <game> [--only S] [--seeds N] \
             | {}",
            oracle::USAGE
        ),
    }
    Ok(())
}
