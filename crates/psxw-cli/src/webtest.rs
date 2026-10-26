//! `psxw web-test gpu [--browsers chromium,firefox]` (M5): the browser
//! renderer against the GPU oracle. Every GPU vector with an emulator
//! golden is exported as its renderer command stream and golden readback
//! (`vectors.bin`). `web/test/gpu.html` runs them through psxw-host-web's
//! WebRenderer in headless Chromium and Firefox (the `psxw-browsers`
//! image, D44), and each readback must equal the golden byte for byte.
//!
//! `psxw web-test storage` (M7): memory cards through OPFS
//! (`web/cards.js`, `web/test/storage.html`).
//!
//! `psxw web-test conform <game>` (M5): every conformance scenario,
//! recorded natively step by step, replayed on the same game.wasm in each
//! browser (`web/test/conform.html`): every return value and read, and the
//! final VRAM and audio, must match. Cross-host determinism without a
//! disc.

use anyhow::{Context as _, Result, bail, ensure};
use psxw_inputs::Workspace;
use psxw_oracle::images::{docker_output, docker_run, image_id, owner_of};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

pub const USAGE: &str = "web-test gpu [--browsers chromium,firefox] [--timeout SECONDS] \
                         | web-test storage [--browsers ..] [--timeout S] \
                         | web-test conform <game> [--browsers ..] [--timeout S] \
                         | web-test game <game> [--replay R] [--frames N] [--hash F,..] [--browsers ..] [--timeout S]";
const IMAGE: &str = "psxw-browsers:local";

#[derive(Serialize, Deserialize)]
struct Record {
    id: String,
    base_image: String,
    debian_snapshot: String,
    sources: serde_json::Value,
}

fn record_path(ws: &Workspace) -> PathBuf {
    ws.root.join(".oracle").join("browsers.json")
}

/// The image, built from the pins on first use (or when it is gone).
fn image(ws: &Workspace) -> Result<String> {
    let pins = ws.lock.oracle.as_ref().context("inputs.lock has no [oracle] section")?;
    if let Ok(text) = std::fs::read_to_string(record_path(ws)) {
        let rec: Record = serde_json::from_str(&text)?;
        if rec.base_image == pins.base_image && rec.debian_snapshot == pins.debian_snapshot && image_id(IMAGE).ok().as_ref() == Some(&rec.id) {
            return Ok(rec.id);
        }
    }
    let logs = ws.out_dir().join("oracle").join("images");
    std::fs::create_dir_all(&logs)?;
    let log = logs.join("browsers-build.log");
    eprintln!("web-test: building {IMAGE} (log: {})", log.display());
    let file = std::fs::File::create(&log)?;
    let ok = Command::new("docker")
        .args(["build", "-f"])
        .arg(ws.root.join("oracle/docker/browsers.Dockerfile"))
        .args(["-t", IMAGE, "--progress=plain"])
        .args(["--build-arg", &format!("BASE_IMAGE={}", pins.base_image)])
        .args(["--build-arg", &format!("DEBIAN_SNAPSHOT={}", pins.debian_snapshot)])
        .arg(ws.root.join("oracle"))
        .stdout(file.try_clone()?)
        .stderr(file)
        .status()
        .context("running docker build")?
        .success();
    if !ok {
        bail!("docker build of {IMAGE} failed; see {}", log.display());
    }
    let sources: serde_json::Value = serde_json::from_str(&docker_output(&[
        "run".into(),
        "--rm".into(),
        "--network=none".into(),
        "--entrypoint".into(),
        "cat".into(),
        IMAGE.into(),
        "/opt/psxw/sources.json".into(),
    ])?)?;
    let rec = Record { id: image_id(IMAGE)?, base_image: pins.base_image.clone(), debian_snapshot: pins.debian_snapshot.clone(), sources };
    std::fs::create_dir_all(record_path(ws).parent().unwrap())?;
    std::fs::write(record_path(ws), serde_json::to_string_pretty(&rec)?)?;
    Ok(rec.id)
}

/// Every GPU vector with an emulator golden: (id, stream, golden).
fn vectors(ws: &Workspace) -> Result<Vec<(String, Vec<u32>, Vec<u8>)>> {
    let store = psxw_oracle::commands::store(ws);
    let pins = ws.lock.oracle.as_ref().context("inputs.lock has no [oracle] section")?;
    let target = psxw_oracle::target::emulator_target_id(pins.beetle.short(), pins.openbios.short());
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for set in store.set_names()? {
        for id in store.set(&set)? {
            if !seen.insert(id) {
                continue;
            }
            let v = store.vector(&id)?;
            if v.kind().device() != "gpu" {
                continue;
            }
            let Some((golden, _)) = store.golden(&target, &id)? else { continue };
            out.push((id.hex(), psxw_gpu_oracle::replay(&v, &store)?, golden));
        }
    }
    Ok(out)
}

fn encode(vs: &[(String, Vec<u32>, Vec<u8>)]) -> Vec<u8> {
    let mut b = b"PSXWGPUT".to_vec();
    b.extend_from_slice(&(vs.len() as u32).to_le_bytes());
    let pad = |b: &mut Vec<u8>| b.resize(b.len().div_ceil(4) * 4, 0);
    for (id, stream, golden) in vs {
        b.extend_from_slice(&(id.len() as u32).to_le_bytes());
        b.extend_from_slice(id.as_bytes());
        pad(&mut b);
        b.extend_from_slice(&(stream.len() as u32).to_le_bytes());
        for w in stream {
            b.extend_from_slice(&w.to_le_bytes());
        }
        b.extend_from_slice(&(golden.len() as u32).to_le_bytes());
        b.extend_from_slice(golden);
        pad(&mut b);
    }
    b
}

/// `web-test game`: cross-host determinism (M5 exit criterion 2). The
/// same replay runs in the native host (render server) and in each
/// browser (the `build-web` bundle, the disc mounted read-only and
/// imported into OPFS), and every requested frame's RAM and VRAM hashes
/// must agree.
fn game(ws: &Workspace, game: &str, args: &[&str]) -> Result<bool> {
    let mut browsers = vec!["chromium".to_string(), "firefox".to_string()];
    let (mut timeout, mut frames) = (1800u32, 600u64);
    let mut replay_path: Option<String> = None;
    let mut hash: Vec<u64> = vec![60, 300, 600];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--browsers" => browsers = it.next().context("--browsers needs a value")?.split(',').map(str::to_string).collect(),
            "--timeout" => timeout = it.next().context("--timeout needs a value")?.parse()?,
            "--frames" => frames = it.next().context("--frames needs a value")?.parse()?,
            "--replay" => replay_path = it.next().map(|s| s.to_string()),
            "--hash" => hash = it.next().context("--hash needs a value")?.split(',').map(str::parse).collect::<Result<_, _>>()?,
            other => bail!("unknown option {other}"),
        }
    }
    let disc = ws.disc(game)?;
    let bundle = psxw_build::web::build(ws, game)?;
    let replay = match &replay_path {
        Some(p) => std::fs::read(p)?,
        None => psxw_abi_replay_none(frames),
    };
    std::fs::write(bundle.join("replay.rpl"), &replay)?;
    // The disc's mount point inside the read-only bundle mount.
    std::fs::create_dir_all(bundle.join("disc"))?;

    // Native.
    let (mut o, _) = crate::run::options(ws, game, &[])?;
    o.frames = frames;
    o.replay = Some(replay.clone());
    o.hash_at = hash.clone();
    o.vram_hash_at = hash.clone();
    o.render = Some(crate::render::server(ws));
    let native = psxw_host_native::run(o)?;
    println!("native: {:?} after {} frames", native.stop, native.frames);

    // Browsers.
    let id = image(ws)?;
    let results = ws.out_dir().join("web").join(format!("test-{game}-results"));
    let _ = std::fs::remove_dir_all(&results);
    std::fs::create_dir_all(&results)?;
    let cue = disc.file_name().context("disc name")?.to_string_lossy().into_owned();
    let list = |v: &[u64]| v.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let page = format!("index.html?discurl=disc/{cue}&autostart&verify&frames={frames}&hash={}&replay=replay.rpl&post", list(&hash));
    let mut args: Vec<String> = vec![page, timeout.to_string()];
    args.extend(browsers.iter().cloned());
    let disc_dir = disc.parent().context("disc directory")?.to_path_buf();
    let argv = docker_run(
        &id,
        &owner_of(&results)?,
        &[(bundle.clone(), "/www", true), (disc_dir, "/www/disc", true), (results.clone(), "/out", false)],
        None,
        &args,
    );
    ensure!(Command::new("docker").args(&argv).status()?.success(), "the browser container failed");
    let mut pass = true;
    for b in &browsers {
        let Ok(text) = std::fs::read_to_string(results.join(format!("result-{b}.json"))) else {
            println!("{b}: no result within {timeout} s");
            pass = false;
            continue;
        };
        let r: serde_json::Value = serde_json::from_str(&text)?;
        if let Some(e) = r["error"].as_str() {
            println!("{b}: FAIL ({e})");
            pass = false;
            continue;
        }
        let mut diffs = Vec::new();
        for f in &hash {
            let key = f.to_string();
            let (nr, nv) = (native.ram_hashes.get(f), native.vram_hashes.get(f));
            let (br, bv) = (r["ram"][&key].as_str(), r["vram"][&key].as_str());
            if nr.map(String::as_str) != br || nv.map(String::as_str) != bv {
                diffs.push(format!("frame {f}: RAM native {nr:?} browser {br:?}; VRAM native {nv:?} browser {bv:?}"));
            }
        }
        let (na, ba) = (&native.audio, &r["audio"]);
        if ba["hash"].as_str() != Some(na.hash.as_str()) || ba["frames"].as_u64() != Some(na.frames) {
            diffs.push(format!("audio: native {} frames {}, browser {} frames {}", na.frames, na.hash, ba["frames"], ba["hash"]));
        }
        pass &= diffs.is_empty();
        println!("{b}: {} ({} frames compared, {} s in the browser)", if diffs.is_empty() { "PASS" } else { "FAIL" }, hash.len(), r["seconds"]);
        for d in diffs {
            println!("  {d}");
        }
    }
    Ok(pass)
}

/// Memory-card import and export and the Worker's save path, through OPFS
/// in each browser: every check in `web/test/storage.js` must pass.
fn storage(ws: &Workspace, args: &[&str]) -> Result<bool> {
    let mut browsers = vec!["chromium".to_string(), "firefox".to_string()];
    let mut timeout = 120u32;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--browsers" => browsers = it.next().context("--browsers needs a value")?.split(',').map(str::to_string).collect(),
            "--timeout" => timeout = it.next().context("--timeout needs a value")?.parse()?,
            other => bail!("unknown option {other}"),
        }
    }
    let id = image(ws)?;
    let bundle = ws.out_dir().join("web").join("test-storage");
    let _ = std::fs::remove_dir_all(&bundle);
    std::fs::create_dir_all(&bundle)?;
    std::fs::copy(ws.root.join("web/cards.js"), bundle.join("cards.js"))?;
    for f in ["storage.html", "storage.js", "storage-worker.js"] {
        std::fs::copy(ws.root.join("web/test").join(f), bundle.join(f))?;
    }
    let results = ws.out_dir().join("web").join("test-storage-results");
    let _ = std::fs::remove_dir_all(&results);
    std::fs::create_dir_all(&results)?;
    let mut argv_args: Vec<String> = vec!["storage.html".into(), timeout.to_string()];
    argv_args.extend(browsers.iter().cloned());
    let argv = docker_run(&id, &owner_of(&results)?, &[(bundle, "/www", true), (results.clone(), "/out", false)], None, &argv_args);
    ensure!(Command::new("docker").args(&argv).status()?.success(), "the browser container failed");
    let mut pass = true;
    for b in &browsers {
        let Ok(text) = std::fs::read_to_string(results.join(format!("result-{b}.json"))) else {
            println!("{b}: no result within {timeout} s");
            pass = false;
            continue;
        };
        let r: serde_json::Value = serde_json::from_str(&text)?;
        let ok = r["pass"].as_bool() == Some(true);
        pass &= ok;
        let checks = r["checks"].as_array().map_or(0, Vec::len);
        println!("{b}: {} — {checks} storage checks{}", if ok { "PASS" } else { "FAIL" }, r["error"].as_str().map(|e| format!(" ({e})")).unwrap_or_default());
        for c in r["checks"].as_array().into_iter().flatten().filter(|c| c["pass"] != true) {
            println!("  failed: {}", c["name"]);
        }
    }
    Ok(pass)
}

fn conform(ws: &Workspace, game: &str, args: &[&str]) -> Result<bool> {
    let mut browsers = vec!["chromium".to_string(), "firefox".to_string()];
    let mut timeout = 600u32;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--browsers" => browsers = it.next().context("--browsers needs a value")?.split(',').map(str::to_string).collect(),
            "--timeout" => timeout = it.next().context("--timeout needs a value")?.parse()?,
            other => bail!("unknown option {other}"),
        }
    }
    let mut recs = Vec::new();
    // Scenarios that count probe hits need a probed build; the replay runs
    // the plain game.wasm.
    for lib in crate::conform::LIBRARIES.iter().filter(|l| crate::conform::probes(l).is_empty()) {
        eprintln!("web-test: recording the {lib} scenario natively");
        recs.push(crate::conform::record(ws, game, lib, 120)?);
    }
    let wasm = psxw_build::game::build(ws, &psxw_build::game::Options::new(game))?.0.wasm;
    let id = image(ws)?;
    let bundle = ws.out_dir().join("web").join(format!("test-conform-{game}"));
    let _ = std::fs::remove_dir_all(&bundle);
    std::fs::create_dir_all(&bundle)?;
    psxw_build::web::pkg(ws, &bundle.join("pkg"))?;
    for f in ["conform.html", "conform.js"] {
        std::fs::copy(ws.root.join("web/test").join(f), bundle.join(f))?;
    }
    std::fs::copy(&wasm, bundle.join("game.wasm"))?;
    std::fs::write(bundle.join("conform.json"), serde_json::to_string(&recs)?)?;
    std::fs::write(bundle.join("ram.bin"), &recs[0].ram)?;
    std::fs::write(bundle.join("replay.rpl"), &recs[0].replay)?;
    let results = ws.out_dir().join("web").join(format!("test-conform-{game}-results"));
    let _ = std::fs::remove_dir_all(&results);
    std::fs::create_dir_all(&results)?;
    let mut argv_args: Vec<String> = vec!["conform.html".into(), timeout.to_string()];
    argv_args.extend(browsers.iter().cloned());
    let argv = docker_run(&id, &owner_of(&results)?, &[(bundle, "/www", true), (results.clone(), "/out", false)], None, &argv_args);
    ensure!(Command::new("docker").args(&argv).status()?.success(), "the browser container failed");
    let mut pass = true;
    for b in &browsers {
        let Ok(text) = std::fs::read_to_string(results.join(format!("result-{b}.json"))) else {
            println!("{b}: no result within {timeout} s");
            pass = false;
            continue;
        };
        let r: serde_json::Value = serde_json::from_str(&text)?;
        if let Some(e) = r["error"].as_str() {
            println!("{b}: FAIL ({e})");
            pass = false;
            continue;
        }
        for (rec, got) in recs.iter().zip(r["results"].as_array().into_iter().flatten()) {
            let mut problems = Vec::new();
            if let Some(e) = got["error"].as_str() {
                problems.push(e.to_string());
            }
            if let Some(d) = got["diffs"].as_array().filter(|d| !d.is_empty()) {
                problems.extend(d.iter().take(4).map(|x| x.to_string()));
            }
            if got["vram"].as_str() != Some(rec.vram.as_str()) {
                problems.push(format!("VRAM {} vs native {}", got["vram"], rec.vram));
            }
            if got["audio"]["hash"].as_str() != Some(rec.audio_hash.as_str()) || got["audio"]["frames"].as_u64() != Some(rec.audio_frames) {
                problems.push(format!("audio {} vs native {} frames {}", got["audio"], rec.audio_frames, rec.audio_hash));
            }
            pass &= problems.is_empty();
            println!(
                "{b} {}: {} ({} steps, VRAM {}, {} audio frames)",
                rec.library,
                if problems.is_empty() { "PASS" } else { "FAIL" },
                rec.steps.len(),
                rec.vram,
                rec.audio_frames
            );
            for p in problems {
                println!("  {p}");
            }
        }
    }
    Ok(pass)
}

/// A replay with nothing pressed for `frames` frames.
fn psxw_abi_replay_none(frames: u64) -> Vec<u8> {
    psxw_oracle::play::replay_from_presses("none", frames as u32, &[]).expect("empty replay")
}

pub fn run(ws: &Workspace, args: &[&str]) -> Result<bool> {
    let mut browsers = vec!["chromium".to_string(), "firefox".to_string()];
    let mut timeout = 600u32;
    let mut it = args.iter();
    match it.next() {
        Some(&"gpu") => {}
        Some(&"game") => {
            let g = it.next().context("web-test game needs a game")?;
            return game(ws, g, &args[2..]);
        }
        Some(&"storage") => return storage(ws, &args[1..]),
        Some(&"conform") => {
            let g = it.next().context("web-test conform needs a game")?;
            return conform(ws, g, &args[2..]);
        }
        _ => bail!("usage: psxw {USAGE}"),
    }
    while let Some(a) = it.next() {
        match *a {
            "--browsers" => browsers = it.next().context("--browsers needs a value")?.split(',').map(str::to_string).collect(),
            "--timeout" => timeout = it.next().context("--timeout needs a value")?.parse()?,
            other => bail!("unknown option {other}"),
        }
    }
    let vs = vectors(ws)?;
    ensure!(!vs.is_empty(), "no GPU vectors with goldens; run `psxw oracle capture --missing`");
    let id = image(ws)?;
    let bundle = ws.out_dir().join("web").join("test-gpu");
    let _ = std::fs::remove_dir_all(&bundle);
    std::fs::create_dir_all(&bundle)?;
    psxw_build::web::pkg(ws, &bundle.join("pkg"))?;
    for f in ["gpu.html", "gpu.js"] {
        std::fs::copy(ws.root.join("web/test").join(f), bundle.join(f))?;
    }
    std::fs::write(bundle.join("vectors.bin"), encode(&vs))?;
    let results = ws.out_dir().join("web").join("test-gpu-results");
    let _ = std::fs::remove_dir_all(&results);
    std::fs::create_dir_all(&results)?;
    eprintln!("web-test: {} GPU vectors in {}", vs.len(), browsers.join(", "));
    let mut args: Vec<String> = vec!["gpu.html".into(), timeout.to_string()];
    args.extend(browsers.iter().cloned());
    let argv = docker_run(&id, &owner_of(&results)?, &[(bundle.clone(), "/www", true), (results.clone(), "/out", false)], None, &args);
    let status = Command::new("docker").args(&argv).status().context("running docker")?;
    ensure!(status.success(), "the browser container failed ({status})");
    let mut pass = true;
    for b in &browsers {
        let path = results.join(format!("result-{b}.json"));
        let Ok(text) = std::fs::read_to_string(&path) else {
            println!("{b}: no result within {timeout} s (log: {})", results.join(format!("{b}.log")).display());
            pass = false;
            continue;
        };
        let r: serde_json::Value = serde_json::from_str(&text)?;
        // Headless Chromium on SwiftShader loses contexts but never
        // restores them (any canvas kind, any thread): the recovery path
        // can't run there, and the game keeps going without drawing.
        let unrestorable = b == "chromium" && r["contextLoss"]["unrestored"].as_bool() == Some(true);
        let restored = r["contextLoss"]["ok"].as_bool() == Some(true) || unrestorable;
        let ok = r["error"].is_null() && r["total"].as_u64() == Some(vs.len() as u64) && r["pass"] == r["total"] && restored;
        pass &= ok;
        println!(
            "{b}: {} — {} of {} vectors equal their goldens in {} ms ({})",
            if ok { "PASS" } else { "FAIL" },
            r["pass"],
            r["total"],
            r["ms"],
            r["renderer"].as_str().or(r["error"].as_str()).unwrap_or("?")
        );
        if let Some(f) = r["failures"].as_array().filter(|f| !f.is_empty()) {
            for x in f.iter().take(8) {
                println!("  {x}");
            }
        }
        if unrestorable {
            println!("  context loss: not exercised (this browser never restores a lost context)");
        } else if !restored {
            println!("  context loss: VRAM not recovered ({})", r["contextLoss"]);
        } else {
            println!("  context loss: VRAM recovered from the shadow copy");
        }
    }
    Ok(pass)
}
