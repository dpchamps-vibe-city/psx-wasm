//! `psxw ci <game>`: every oracle of every milestone whose gates are in
//! place, from the pinned inputs. Runs them all, prints one line per gate,
//! writes `out/ci/<game>/ci.json`, and fails if any gate failed.

use anyhow::Result;
use psxw_inputs::{Workspace, snapshot, toolchain};
use serde::Serialize;

#[derive(Serialize)]
struct Gate {
    name: &'static str,
    milestone: &'static str,
    pass: bool,
    detail: String,
}

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    game: String,
    pass: bool,
    gates: Vec<Gate>,
}

fn gate(gates: &mut Vec<Gate>, name: &'static str, milestone: &'static str, r: Result<(bool, String)>) {
    let (pass, detail) = r.unwrap_or_else(|e| (false, format!("error: {e:#}")));
    eprintln!("ci: {name:<12} {:<4} {detail}", if pass { "ok" } else { "FAIL" });
    gates.push(Gate { name, milestone, pass, detail });
}

pub fn run(ws: &Workspace, game: &str) -> Result<bool> {
    let mut gates = Vec::new();
    gate(&mut gates, "inputs", "M0", (|| {
        for name in ws.lock.toolchain.keys() {
            toolchain::fetch(ws, name)?;
        }
        for (name, pin) in ws.lock.game.iter().chain(&ws.lock.sdk) {
            snapshot::ensure(ws, name, pin)?;
        }
        Ok((true, "toolchains and snapshots match inputs.lock".into()))
    })());
    gate(&mut gates, "census", "M0", (|| {
        let opts = psxw_census::Options { game: game.into(), only: None, probe: true, only_header: None };
        let (r, json) = psxw_census::run(ws, &opts)?;
        Ok((r.verdict.pass, json.display().to_string()))
    })());
    gate(&mut gates, "lift", "M1", (|| {
        let opts = psxw_lift::run::Options { game: game.into(), function: None, container: None };
        let (r, json) = psxw_lift::run::run(ws, &opts)?;
        let n: usize = r.containers.values().map(|c| c.translated).sum();
        Ok((r.verdict.pass, format!("{n} functions translated; {}", json.display())))
    })());
    gate(&mut gates, "difftest", "M1", (|| {
        let opts = psxw_difftest::Options { game: game.into(), only: None, seeds: 64, keep: false, verbose: false };
        let (r, json) = psxw_difftest::run(ws, &opts)?;
        let counts: Vec<String> = r.counts.iter().map(|(k, n)| format!("{k} {n}")).collect();
        Ok((r.pass(), format!("{}; {}", counts.join(", "), json.display())))
    })());
    gate(&mut gates, "unit-tests", "M1", (|| {
        let out = std::process::Command::new("cargo")
            .current_dir(&ws.root)
            .args(["test", "--workspace", "--release", "-q"])
            .output()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let passed: usize = text
            .lines()
            .filter_map(|l| l.strip_prefix("test result: ok. "))
            .filter_map(|l| l.split(' ').next()?.parse::<usize>().ok())
            .sum();
        Ok((out.status.success(), format!("cargo test --workspace: {passed} passed")))
    })());
    gate(&mut gates, "oracle", "M3", (|| {
        let reports = psxw_oracle::commands::ci(ws)?;
        let pass = reports.iter().all(|(r, _)| r.pass);
        let detail: Vec<String> = reports.iter().map(|(r, _)| format!("{}: {}", r.command, if r.pass { "pass" } else { "FAIL" })).collect();
        Ok((pass, detail.join(", ")))
    })());

    let pass = gates.iter().all(|g| g.pass);
    let report = Report { schema_version: 1, game: game.into(), pass, gates };
    let dir = ws.out_dir().join("ci").join(game);
    std::fs::create_dir_all(&dir)?;
    let json = dir.join("ci.json");
    std::fs::write(&json, serde_json::to_string_pretty(&report)?)?;
    eprintln!("ci: {} ({})", if pass { "PASS" } else { "FAIL" }, json.display());
    Ok(pass)
}
