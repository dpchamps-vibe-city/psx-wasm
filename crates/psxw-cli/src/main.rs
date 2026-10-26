//! `psxw`: the driver.
//!
//! ```text
//! psxw toolchain fetch                  download + verify pinned clang
//! psxw inputs sync                      snapshot every pinned repo
//! psxw census <game> [--only S] [--only-header S] [--no-probe]
//!                                       M0: compile every TU, check vs original
//! psxw lift <game> [--function F | --container C | --all]
//!                                       M1: translate every function to C
//! psxw difftest <game> [--only S] [--seeds N] [--keep] [--verbose]
//!                                       M1: translated == decompiled C
//! psxw link-test <game> --function F    M1: link one pair, validate, run once
//! psxw ci <game>                        every gate of every milestone in place
//! psxw runtime build                    runtime.wasm (game-agnostic)
//! psxw build <game> [--debug-io] [--ubsan] [--original libpad,…]
//!                                       M2: out/<game>/game.wasm
//! psxw report tiers <game>              each function's tier, by library
//! psxw run <game> [--frames N] [--replay R] [--check boot] [--cd instant] [--verbose]
//!                                       M2: run game.wasm headless
//! psxw identity <game>                 M8: SDK object identity report
//! psxw build-web <game>; psxw serve [dir] [--port N]   M5: the browser bundle
//! psxw oracle …                         M3: device oracle harness (oracle.rs)
//! ```

use anyhow::{Context as _, Result, bail};
use psxw_inputs::{Workspace, snapshot, toolchain};

mod ci;
mod run;
mod conform;
mod render;
mod serve;
mod webtest;
mod oracle;
mod wholegame;

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
        ["layout", game] => {
            let manifest = ws.game(game)?;
            let path = manifest.game.boundary.clone().context("the game has no boundary")?;
            let boundary = psxw_inputs::boundary::Boundary::load(&ws.root.join(&path))?;
            let l = boundary.layout.context("the boundary has no [layout]")?;
            let layouts = psxw_census::layout::sdk_layout(&ws, game, &l.headers, &l.structs)?;
            let pin = ws.game_pin(&manifest)?;
            let provenance = format!("From {} @ {}, headers {} ({}).", manifest.game.id, pin.short(), l.headers.join(", "), path.display());
            let out = ws.root.join(&l.output);
            std::fs::write(&out, psxw_census::layout::rust_module(&layouts, &provenance))?;
            for s in &layouts {
                println!("{:<14} size {:>3}, {} fields ({})", s.name, s.size, s.fields.len(), s.header);
            }
            eprintln!("wrote {}", out.display());
        }
        ["build-web", game] => {
            let dir = psxw_build::web::build(&ws, game)?;
            eprintln!("web bundle: {} (serve it with `psxw serve {}`)", dir.display(), dir.display());
        }
        ["web-test", rest @ ..] => {
            if !webtest::run(&ws, rest)? {
                std::process::exit(2);
            }
        }
        ["serve", rest @ ..] => {
            let mut root = ws.out_dir().join("web");
            let mut port = 8080u16;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--port" => port = it.next().context("--port needs a value")?.parse()?,
                    dir => root = dir.into(),
                }
            }
            serve::serve(root, port)?;
        }
        ["identity", game] => {
            let inputs = psxw_lift::run::load(&ws, game)?;
            let db = psxw_inputs::identity::SigDb::pinned(&ws)?;
            let r = psxw_inputs::identity::identify(game, &inputs.map, &inputs.originals, &db);
            let dir = ws.out_dir().join("identity");
            std::fs::create_dir_all(&dir)?;
            let json = dir.join(format!("{game}.json"));
            std::fs::write(&json, serde_json::to_string_pretty(&r)?)?;
            println!(
                "{game}: {} SDK members; {} identified by signature, {} by subsegment only ({} with no matching version)",
                r.members,
                r.by_signature,
                r.by_subsegment_only,
                r.unmatched.len()
            );
            for (v, n) in &r.versions {
                println!("  PsyQ {v}: {n} members match");
            }
            let names: usize = r.identities.iter().map(|i| i.names_differ.len()).sum();
            println!("  functions the signatures name differently: {names}");
            eprintln!("report: {}", json.display());
        }
        ["oracle", rest @ ..] => oracle::run(&ws, rest)?,
        ["conform", game, rest @ ..] => {
            if !conform::run(&ws, game, rest)? {
                std::process::exit(2);
            }
        }
        ["run", game, rest @ ..] => {
            if !run::run(&ws, game, rest)? {
                std::process::exit(2);
            }
        }
        ["report", "tiers", game] => {
            let path = psxw_build::game::out_dir(&ws, game).join("tiers.json");
            let text = std::fs::read_to_string(&path)
                .map_err(|_| anyhow::anyhow!("{} missing; run `psxw build {game}`", path.display()))?;
            let rows: Vec<serde_json::Value> = serde_json::from_str(&text)?;
            let mut by: std::collections::BTreeMap<(String, String), usize> = Default::default();
            for r in &rows {
                let lib = r["sdk"].as_str().map_or("game".to_string(), |m| m.split('/').next().unwrap_or(m).to_string());
                *by.entry((lib, r["tier"].as_str().unwrap_or("?").to_string())).or_default() += 1;
            }
            for ((lib, tier), n) in by {
                println!("{lib:<10} {tier:<8} {n}");
            }
            eprintln!("per-function tiers: {}", path.display());
        }
        ["runtime", "build"] => {
            let p = psxw_build::runtime::build(&ws)?;
            eprintln!("runtime: {}", p.display());
        }
        ["build", game, rest @ ..] => {
            let mut opts = psxw_build::game::Options::new(game);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--debug-io" => opts.debug_io = true,
                    "--ubsan" => opts.ubsan = true,
                    "--original" => match it.next() {
                        Some(l) => opts.original.extend(l.split(',').map(str::to_string)),
                        None => bail!("--original needs a library list"),
                    },
                    other => bail!("unknown build option {other}"),
                }
            }
            let (report, json) = psxw_build::game::build(&ws, &opts)?;
            println!("{}", serde_json::to_string_pretty(&report.tiers)?);
            eprintln!("game.wasm: {} ({} bytes); report {}", report.wasm, report.game_bytes, json.display());
        }
        ["ci", game] => {
            if !ci::run(&ws, game)? {
                std::process::exit(2);
            }
        }
        ["link-test", game, "--function", f] => {
            let opts = psxw_difftest::Options {
                game: game.to_string(),
                only: Some(f.to_string()),
                seeds: 1,
                keep: true,
                verbose: true,
            };
            let (report, json) = psxw_difftest::run(&ws, &opts)?;
            print!("{}", report.summary());
            eprintln!("\nfull report: {json}; modules kept in {}", json.parent().map(|p| p.join("shards")).unwrap_or_default().display(), json = json.display());
            if !report.pass() {
                std::process::exit(2);
            }
        }
        _ => bail!(
            "usage: psxw toolchain fetch | inputs sync | census <game> [--only S] [--only-header S] [--no-probe] \
             | lift <game> [--function F | --container C | --all] | difftest <game> [--only S] [--seeds N] \
             | link-test <game> --function F | ci <game> | {} | {}",
            conform::USAGE,
            oracle::USAGE
        ),
    }
    Ok(())
}
