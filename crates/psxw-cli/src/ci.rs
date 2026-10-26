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

fn checks_detail(checks: &[crate::run::Check]) -> (bool, String) {
    let failed: Vec<String> = checks.iter().filter(|c| !c.pass).map(|c| format!("{}: {}", c.name, c.detail)).collect();
    if failed.is_empty() {
        (true, format!("{} checks pass", checks.len()))
    } else {
        (false, failed.join("; "))
    }
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
    let built = psxw_build::game::build(ws, &psxw_build::game::Options::new(game));
    gate(&mut gates, "build", "M2", built.as_ref().map(|(r, _)| (true, r.wasm.clone())).map_err(|e| anyhow::anyhow!("{e:#}")));
    gate(&mut gates, "boot", "M2", (|| {
        built.as_ref().map_err(|_| anyhow::anyhow!("no build"))?;
        let (_, checks) = crate::run::evaluate(ws, game, &["--frames", "1200", "--check", "boot", "--check", "determinism"])?;
        Ok(checks_detail(&checks))
    })());
    gate(&mut gates, "ubsan", "M2", (|| {
        let opts = psxw_build::game::Options { ubsan: true, ..psxw_build::game::Options::new(game) };
        let (report, _) = psxw_build::game::build(ws, &opts)?;
        let wasm = report.wasm;
        let (_, checks) = crate::run::evaluate(ws, game, &["--frames", "1200", "--wasm", &wasm, "--check", "no-trap"])?;
        Ok(checks_detail(&checks))
    })());
    gate(&mut gates, "conform", "M4", (|| {
        let mut detail = Vec::new();
        let mut pass = true;
        for lib in crate::conform::LIBRARIES {
            let ok = crate::conform::run(ws, game, &["--lib", lib])?;
            pass &= ok;
            detail.push(format!("{lib} {}", if ok { "pass" } else { "FAIL" }));
        }
        Ok((pass, detail.join(", ")))
    })());
    gate(&mut gates, "web-gpu", "M5", (|| {
        let ok = crate::webtest::run(ws, &["gpu"])?;
        Ok((ok, "every GPU vector through the browser renderer in Chromium and Firefox".into()))
    })());
    gate(&mut gates, "web-conform", "M5", (|| {
        let ok = crate::webtest::run(ws, &["conform", game])?;
        Ok((ok, "every conformance scenario replayed on game.wasm in Chromium and Firefox, native results".into()))
    })());
    gate(&mut gates, "web-storage", "M7", (|| {
        let ok = crate::webtest::run(ws, &["storage"])?;
        Ok((ok, "memory cards through OPFS in Chromium and Firefox".into()))
    })());
    gate(&mut gates, "web-game", "M5", (|| {
        let ok = crate::webtest::run(ws, &["game", game])?;
        Ok((ok, "RAM and VRAM hashes, native vs browsers".into()))
    })());
    gate(&mut gates, "oracle", "M3", (|| {
        let reports = psxw_oracle::commands::ci(ws, &mut crate::oracle::devices(ws))?;
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
