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
    /// Not run: the manifest's `[ci] skip` reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped: Option<String>,
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
    gates.push(Gate { name, milestone, pass, detail, skipped: None });
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
    let ci = ws.game(game)?.ci;
    let mut gates = Vec::new();
    // A gate the manifest skips, with its reason, instead of running it.
    let skip = |gates: &mut Vec<Gate>, name: &'static str, milestone: &'static str| -> bool {
        let Some(why) = ci.skip.get(name) else { return false };
        eprintln!("ci: {name:<12} skip {why}");
        gates.push(Gate { name, milestone, pass: true, detail: String::new(), skipped: Some(why.clone()) });
        true
    };
    macro_rules! gate {
        ($name:literal, $m:literal, $body:expr) => {
            if !skip(&mut gates, $name, $m) {
                gate(&mut gates, $name, $m, $body);
            }
        };
    }
    gate!("inputs", "M0", (|| {
        for name in ws.lock.toolchain.keys() {
            toolchain::fetch(ws, name)?;
        }
        for (name, pin) in ws.lock.game.iter().chain(&ws.lock.sdk) {
            snapshot::ensure(ws, name, pin)?;
        }
        for (game, pin) in &ws.lock.mods {
            snapshot::ensure(ws, &format!("{game}-mods"), pin)?;
        }
        Ok((true, "toolchains and snapshots match inputs.lock".into()))
    })());
    gate!("census", "M0", (|| {
        let opts = psxw_census::Options { game: game.into(), only: None, probe: !ci.census_no_probe, only_header: None };
        let (r, json) = psxw_census::run(ws, &opts)?;
        Ok((r.verdict.pass, json.display().to_string()))
    })());
    gate!("lift", "M1", (|| {
        let opts = psxw_lift::run::Options { game: game.into(), function: None, container: None };
        let (r, json) = psxw_lift::run::run(ws, &opts)?;
        let n: usize = r.containers.values().map(|c| c.translated).sum();
        Ok((r.verdict.pass, format!("{n} functions translated; {}", json.display())))
    })());
    gate!("difftest", "M1", (|| {
        let opts = psxw_difftest::Options { game: game.into(), only: None, seeds: 64, keep: false, verbose: false };
        let (r, json) = psxw_difftest::run(ws, &opts)?;
        let counts: Vec<String> = r.counts.iter().map(|(k, n)| format!("{k} {n}")).collect();
        Ok((r.pass(), format!("{}; {}", counts.join(", "), json.display())))
    })());
    gate!("unit-tests", "M1", (|| {
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
    gate!("boot", "M2", (|| {
        built.as_ref().map_err(|_| anyhow::anyhow!("no build"))?;
        let (_, checks) = crate::run::evaluate(
            ws,
            game,
            &["--frames", "1200", "--check", "boot", "--check", "determinism", "--check", "no-trap", "--check", "no-shadow-io"],
        )?;
        Ok(checks_detail(&checks))
    })());
    gate!("ubsan", "M2", (|| {
        let opts = psxw_build::game::Options { ubsan: true, ..psxw_build::game::Options::new(game) };
        let (report, _) = psxw_build::game::build(ws, &opts)?;
        let wasm = report.wasm;
        let (_, checks) = crate::run::evaluate(ws, game, &["--frames", "1200", "--wasm", &wasm, "--check", "no-trap"])?;
        Ok(checks_detail(&checks))
    })());
    gate!("conform", "M4", (|| {
        let mut detail = Vec::new();
        let mut pass = true;
        for lib in crate::conform::LIBRARIES {
            let ok = crate::conform::run(ws, game, &["--lib", lib])?;
            pass &= ok;
            detail.push(format!("{lib} {}", if ok { "pass" } else { "FAIL" }));
        }
        // Every native chip's adapter cases (D59; levels 1-3).
        let native: Vec<String> = built.as_ref().map(|(r, _)| r.chips.iter().filter(|(_, m)| m.native).map(|(c, _)| c.clone()).collect()).unwrap_or_default();
        for chip in native {
            let ok = crate::conform::run_chip(ws, game, &chip, 4)?;
            pass &= ok;
            detail.push(format!("chip {chip} {}", if ok { "pass" } else { "FAIL" }));
        }
        Ok((pass, detail.join(", ")))
    })());
    gate!("mods", "M11", (|| {
        built.as_ref().map_err(|_| anyhow::anyhow!("no build"))?;
        crate::mods::ci(ws, game)
    })());
    gate!("web-gpu", "M5", (|| {
        let ok = crate::webtest::run(ws, &["gpu"])?;
        Ok((ok, "every GPU vector through the browser renderer in Chromium and Firefox".into()))
    })());
    gate!("web-conform", "M5", (|| {
        let ok = crate::webtest::run(ws, &["conform", game])?;
        Ok((ok, "every conformance scenario replayed on game.wasm in Chromium and Firefox, native results".into()))
    })());
    gate!("web-storage", "M7", (|| {
        let ok = crate::webtest::run(ws, &["storage"])?;
        Ok((ok, "memory cards through OPFS in Chromium and Firefox".into()))
    })());
    gate!("web-game", "M5", (|| {
        // The milestones' title-to-gameplay replay when the game has one
        // (recorded on the way, checking the recorder too); else idle.
        let named = ["toml", "rpl"].iter().any(|x| ws.root.join("games").join(game).join("replays").join(format!("title-to-gameplay.{x}")).is_file());
        let args: Vec<&str> = if named {
            vec!["game", game, "--replay", "title-to-gameplay", "--hash", "300,1200,2400,3600,4800,6000", "--record", "--timeout", "3600"]
        } else {
            vec!["game", game]
        };
        let ok = crate::webtest::run(ws, &args)?;
        let what = if named { "title-to-gameplay" } else { "600 idle frames" };
        Ok((ok, format!("{what}: RAM, VRAM and audio, native vs browsers")))
    })());
    gate!("web-pacing", "M5", (|| {
        let ok = crate::webtest::run(ws, &["pacing", game])?;
        Ok((ok, "60 s idle at real time; gates once psxw.local.toml states the machine".into()))
    })());
    gate!("oracle", "M3", (|| {
        let reports = psxw_oracle::commands::ci(ws, &mut crate::oracle::devices(ws))?;
        let pass = reports.iter().all(|(r, _)| r.pass);
        let detail: Vec<String> = reports.iter().map(|(r, _)| format!("{}: {}", r.command, if r.pass { "pass" } else { "FAIL" })).collect();
        Ok((pass, detail.join(", ")))
    })());

    // D59: the SDK's reach table in the repo is the scan's (when the SDK's
    // objects are here), and the scan misses no access the translator sees.
    gate!("sdk-reach", "D59", (|| {
        let sdk = ws.game(game)?.game.sdk.unwrap_or_default();
        if !psxw_sdk::objects_dir(ws, &sdk).is_ok_and(|d| d.is_dir()) {
            return Ok((true, format!("{sdk}: no library objects in this checkout; the committed table stands")));
        }
        let ok = crate::sdk::run(ws, &["--sdk", &sdk, "--game", game, "--check"])?;
        Ok((ok, format!("{} matches the scan of {sdk}'s objects", psxw_inputs::boundary::Reach::path(&ws.root, &sdk).display())))
    })());
    // D59 level 4: the replay native and with every chip on its device.
    gate!("fallback", "D59", (|| {
        built.as_ref().map_err(|_| anyhow::anyhow!("no build"))?;
        let ok = crate::modes::run(ws, game, None)?;
        Ok((ok, "the replay native and on every device: checks pass, checkpoints reached (out/conform/<game>/modes)".into()))
    })());

    if let Some(name) = &ci.replay {
        gate!("replay", "M13", (|| {
            built.as_ref().map_err(|_| anyhow::anyhow!("no build"))?;
            let (_, checks) = crate::run::evaluate(
                ws,
                game,
                &["--replay", name, "--check", "determinism", "--check", "no-trap", "--check", "no-shadow-io"],
            )?;
            Ok(checks_detail(&checks))
        })());
    }

    let pass = gates.iter().all(|g| g.pass);
    let report = Report { schema_version: 1, game: game.into(), pass, gates };
    let dir = ws.out_dir().join("ci").join(game);
    std::fs::create_dir_all(&dir)?;
    let json = dir.join("ci.json");
    std::fs::write(&json, serde_json::to_string_pretty(&report)?)?;
    eprintln!("ci: {} ({})", if pass { "PASS" } else { "FAIL" }, json.display());
    Ok(pass)
}
