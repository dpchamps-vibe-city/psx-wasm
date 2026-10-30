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
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

const USAGE: &str = "web-test <command> [arguments] [options]\n\nRun `psxw web-test --help` for commands and options.";
const IMAGE: &str = "psxw-browsers:local";

#[derive(Serialize, Deserialize)]
struct Record {
    id: String,
    base_image: String,
    debian_snapshot: String,
    /// sha256 of the Dockerfile and the files it copies: a change rebuilds.
    #[serde(default)]
    context: String,
    sources: serde_json::Value,
}

/// The image's build inputs in `oracle/docker/`, hashed.
fn context_hash(ws: &Workspace) -> Result<String> {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    for f in ["browsers.Dockerfile", "apt-snapshot.sh", "browser-serve.py", "run-browsers.sh"] {
        h.update(std::fs::read(ws.root.join("oracle/docker").join(f))?);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn record_path(ws: &Workspace) -> PathBuf {
    ws.root.join(".oracle").join("browsers.json")
}

/// The image, built from the pins on first use (or when it is gone, or
/// its Dockerfile changed).
pub fn image(ws: &Workspace) -> Result<String> {
    let pins = ws.lock.oracle.as_ref().context("inputs.lock has no [oracle] section")?;
    let context = context_hash(ws)?;
    if let Ok(text) = std::fs::read_to_string(record_path(ws)) {
        let rec: Record = serde_json::from_str(&text)?;
        if rec.base_image == pins.base_image && rec.debian_snapshot == pins.debian_snapshot && rec.context == context && image_id(IMAGE).ok().as_ref() == Some(&rec.id) {
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
    let rec = Record { id: image_id(IMAGE)?, base_image: pins.base_image.clone(), debian_snapshot: pins.debian_snapshot.clone(), context, sources };
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
    // --debug: the debug build (D53), its 2 GiB memory in each browser.
    if args.contains(&"--debug") {
        let rest: Vec<&str> = args.iter().copied().filter(|a| *a != "--debug").collect();
        let (r, _) = psxw_build::game::build(ws, &psxw_build::game::Options { debug: true, ..psxw_build::game::Options::new(game) })?;
        return game_with(ws, game, &rest, Some(&r), &[]);
    }
    game_with(ws, game, args, None, &[])
}

/// `web-test game` on a given build (`built`: with mods linked in, or a
/// moddable base) instead of the plain one, with `modules` loaded into it
/// at start (natively and in the browsers).
pub fn game_with(ws: &Workspace, game: &str, args: &[&str], built: Option<&psxw_build::game::Report>, modules: &[PathBuf]) -> Result<bool> {
    let mut browsers = vec!["chromium".to_string(), "firefox".to_string()];
    let (mut timeout, mut frames) = (1800u32, None::<u64>);
    let mut replay_path: Option<String> = None;
    let mut hash: Vec<u64> = vec![60, 300, 600];
    let mut record = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--browsers" => browsers = it.next().context("--browsers needs a value")?.split(',').map(str::to_string).collect(),
            "--timeout" => timeout = it.next().context("--timeout needs a value")?.parse()?,
            "--frames" => frames = Some(it.next().context("--frames needs a value")?.parse()?),
            "--replay" => replay_path = it.next().map(|s| s.to_string()),
            "--hash" => hash = it.next().context("--hash needs a value")?.split(',').map(str::parse).collect::<Result<_, _>>()?,
            "--record" => record = true,
            other => bail!("unknown option {other}"),
        }
    }
    let disc = ws.disc(game)?;
    let dir = ws.out_dir().join("web").join(format!("test-{game}"));
    let bundle = match built {
        Some(r) => psxw_build::web::bundle(ws, game, r, dir)?,
        None => psxw_build::web::build_into(ws, game, dir)?,
    };
    if !modules.is_empty() {
        crate::mods::write_snapshot(ws, game, &bundle)?;
    }
    // A named replay or file runs its length unless --frames says less.
    let replay = match &replay_path {
        Some(p) => crate::wholegame::replay(ws, game, Some(p))?.0,
        None => psxw_abi_replay_none(frames.unwrap_or(600)),
    };
    let frames = frames.unwrap_or_else(|| psxw_abi::replay::Header::parse(&replay).map_or(600, |h| h.frames as u64));
    std::fs::write(bundle.join("replay.rpl"), &replay)?;
    let mut module_urls = Vec::new();
    if !modules.is_empty() {
        std::fs::create_dir_all(bundle.join("mods"))?;
        for m in modules {
            let name = m.file_name().context("module name")?.to_string_lossy().into_owned();
            std::fs::copy(m, bundle.join("mods").join(&name))?;
            module_urls.push(format!("mods/{name}"));
        }
    }
    // The disc's mount point inside the read-only bundle mount.
    std::fs::create_dir_all(bundle.join("disc"))?;

    // Native.
    let (mut o, _) = crate::run::options(ws, game, &[])?;
    if let Some(r) = built {
        o.wasm = r.wasm.clone().into();
    }
    o.mods = modules.to_vec();
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
    let mut page = format!("index.html?discurl=disc/{cue}&autostart&verify&frames={frames}&hash={}&replay=replay.rpl&post", list(&hash));
    if record {
        page += "&record";
    }
    if !module_urls.is_empty() {
        page += &format!("&mods={}", module_urls.join(","));
    }
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
        // What the page put on screen: at least one sampled frame lit.
        let d = &r["display"];
        if d["lit"].as_u64().unwrap_or(0) == 0 {
            diffs.push(format!("the page showed no picture ({} frames shown, {} sampled, none lit)", d["shown"], d["sampled"]));
        }
        let (na, ba) = (&native.audio, &r["audio"]);
        if ba["hash"].as_str() != Some(na.hash.as_str()) || ba["frames"].as_u64() != Some(na.frames) {
            diffs.push(format!("audio: native {} frames {}, browser {} frames {}", na.frames, na.hash, ba["frames"], ba["hash"]));
        }
        // --record: the page recorded the session; played natively, the
        // recording must reach the same RAM.
        if record {
            match std::fs::read(results.join(format!("result-{b}-replay.json"))) {
                Ok(rec) if psxw_abi::replay::Header::parse(&rec).is_some() => {
                    let (mut o, _) = crate::run::options(ws, game, &[])?;
                    if let Some(r) = built {
                        o.wasm = r.wasm.clone().into();
                    }
                    o.mods = modules.to_vec();
                    o.frames = frames;
                    o.replay = Some(rec);
                    o.hash_at = hash.clone();
                    let again = psxw_host_native::run(o)?;
                    for f in &hash {
                        if again.ram_hashes.get(f) != native.ram_hashes.get(f) {
                            diffs.push(format!("recording, frame {f}: RAM {:?}, the replay gave {:?}", again.ram_hashes.get(f), native.ram_hashes.get(f)));
                        }
                    }
                }
                _ => diffs.push("no recorded replay".into()),
            }
        }
        pass &= diffs.is_empty();
        println!(
            "{b}: {} ({} frames compared, {} s in the browser; {} frames shown, {} of {} sampled lit)",
            if diffs.is_empty() { "PASS" } else { "FAIL" },
            hash.len(),
            r["seconds"],
            d["shown"],
            d["lit"],
            d["sampled"]
        );
        for d in diffs {
            println!("  {d}");
        }
    }
    Ok(pass)
}

/// A mod as the page loads it: a module (compiled, M11 phase 2), or a
/// script mod's directory, with its `mod.js` (plans/M12), and the snapshot
/// its `game` comes from (the bundle's when none).
#[derive(Debug, Clone)]
pub enum PageMod {
    Module(PathBuf),
    Script(PathBuf, Option<PathBuf>),
}

impl PageMod {
    pub fn script(dir: PathBuf) -> Self {
        PageMod::Script(dir, None)
    }
}

/// A script mod copied for the page, with its `psxmod.json` naming its
/// snapshot (copied into the bundle's `snapshots/`).
pub fn place_script(dir: &std::path::Path, snapshot: Option<&std::path::Path>, bundle: &std::path::Path, to: &std::path::Path) -> Result<()> {
    copy_js(dir, to)?;
    if let Some(snap) = snapshot {
        let name = snap.file_name().context("snapshot file")?.to_string_lossy().into_owned();
        std::fs::create_dir_all(bundle.join("snapshots"))?;
        let dst = bundle.join("snapshots").join(&name);
        if !dst.is_file() {
            std::fs::copy(snap, &dst)?;
        }
        let rel = to.strip_prefix(bundle).map(|r| r.components().count()).unwrap_or(2);
        let up = "../".repeat(rel);
        std::fs::write(to.join("psxmod.json"), serde_json::json!({ "snapshot": format!("{up}snapshots/{name}") }).to_string())?;
    }
    Ok(())
}

impl PageMod {
    /// Into the bundle's `mods/`; the URL the page loads it from.
    pub fn place(&self, bundle: &std::path::Path) -> Result<String> {
        self.place_as(bundle, "")
    }

    /// The same, a script mod at its own URL (`<dir><suffix>/mod.js`): a
    /// module the page imported once is never imported again from the same
    /// URL.
    pub fn place_as(&self, bundle: &std::path::Path, suffix: &str) -> Result<String> {
        let mods = bundle.join("mods");
        std::fs::create_dir_all(&mods)?;
        match self {
            PageMod::Module(p) => {
                let name = p.file_name().context("module name")?.to_string_lossy().into_owned();
                std::fs::copy(p, mods.join(&name))?;
                Ok(format!("mods/{name}"))
            }
            PageMod::Script(dir, snapshot) => {
                let name = format!("{}{suffix}", std::path::absolute(dir)?.file_name().context("script mod directory")?.to_string_lossy());
                place_script(dir, snapshot.as_deref(), bundle, &mods.join(&name))?;
                Ok(format!("mods/{name}/mod.js"))
            }
        }
    }
}

/// A script mod's JavaScript, its tests left out.
pub fn copy_js(from: &std::path::Path, to: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let p = e?.path();
        let name = p.file_name().unwrap_or_default();
        if p.is_dir() && name != "tests" && name != "node_modules" {
            copy_js(&p, &to.join(name))?;
        } else if p.extension().is_some_and(|x| x == "js" || x == "mjs" || x == "json") {
            std::fs::copy(&p, to.join(name))?;
        }
    }
    Ok(())
}

/// A page run for a mod test (plans/M12 task 11): the moddable base with
/// mods loaded at start or swapped in at vblanks, values read every
/// vblank, commands at vblanks, and the run's log lines.
#[derive(Debug, Clone, Default)]
pub struct PageRun {
    /// Names the bundle and results directories.
    pub tag: String,
    pub mods: Vec<PageMod>,
    /// (vblank, index, mod).
    pub swaps: Vec<(u64, usize, PageMod)>,
    pub replay: Vec<u8>,
    pub frames: u64,
    pub hash_at: Vec<u64>,
    /// (address, bytes) read at every vblank.
    pub watch: Vec<(u32, u32)>,
    /// (vblank, command, arguments).
    pub commands: Vec<(u64, String, Vec<serde_json::Value>)>,
    pub browsers: Vec<String>,
    pub timeout: u32,
}

/// One browser's run, in the native host's shapes.
#[derive(Debug, Clone, Default)]
pub struct BrowserRun {
    pub browser: String,
    /// The page failed before the run's end (no result, a load error).
    pub error: Option<String>,
    /// `frame limit`, or why it stopped.
    pub stop: String,
    pub frames: u64,
    pub ram_hashes: BTreeMap<u64, String>,
    pub vram_hashes: BTreeMap<u64, String>,
    pub audio_frames: u64,
    pub audio_hash: String,
    pub watched: Vec<Vec<u32>>,
    pub logs: Vec<String>,
    pub events: Vec<serde_json::Value>,
    pub seconds: f64,
}

impl BrowserRun {
    pub fn reached_limit(&self) -> bool {
        self.error.is_none() && self.stop == "frame limit"
    }

    /// RAM, VRAM and audio, for comparing two runs.
    pub fn hashes(&self) -> String {
        format!("ram {:?} vram {:?} audio {} {}", self.ram_hashes, self.vram_hashes, self.audio_frames, self.audio_hash)
    }

    /// The runtime's report (counters, held waits).
    pub fn report(&self) -> Option<&serde_json::Value> {
        self.events.iter().rev().find(|e| e["event"] == "report")
    }
}

/// Run a moddable `base` with mods in each browser (the `psxw-browsers`
/// image, D44).
pub fn page_runs(ws: &Workspace, game: &str, base: &psxw_build::game::Report, run: &PageRun) -> Result<Vec<BrowserRun>> {
    let disc = ws.disc(game)?;
    let dir = ws.out_dir().join("web").join(format!("test-{game}-{}", run.tag));
    let bundle = crate::mods::moddable_bundle(ws, game, base, dir)?;
    std::fs::write(bundle.join("replay.rpl"), &run.replay)?;
    std::fs::create_dir_all(bundle.join("disc"))?;
    let mut urls = Vec::new();
    for m in &run.mods {
        urls.push(m.place(&bundle)?);
    }
    let mut swaps = Vec::new();
    for (k, (f, i, m)) in run.swaps.iter().enumerate() {
        swaps.push(format!("&swap={f}:{i}:{}", m.place_as(&bundle, &format!("@swap{k}"))?));
    }
    let test = serde_json::json!({
        "watch": run.watch.iter().map(|&(a, n)| serde_json::json!([a, n])).collect::<Vec<_>>(),
        "commands": run.commands.iter().map(|(at, name, args)| serde_json::json!({ "at": at, "name": name, "args": args })).collect::<Vec<_>>(),
        "logs": true,
    });
    std::fs::write(bundle.join("test.json"), test.to_string())?;
    let id = image(ws)?;
    let results = ws.out_dir().join("web").join(format!("test-{game}-{}-results", run.tag));
    let _ = std::fs::remove_dir_all(&results);
    std::fs::create_dir_all(&results)?;
    let cue = disc.file_name().context("disc name")?.to_string_lossy().into_owned();
    let list = |v: &[u64]| v.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let mut page = format!(
        "index.html?discurl=disc/{cue}&autostart&verify&frames={}&hash={}&replay=replay.rpl&post&test=test.json",
        run.frames,
        list(&run.hash_at)
    );
    if !urls.is_empty() {
        page += &format!("&mods={}", urls.join(","));
    }
    page += &swaps.concat();
    let mut args: Vec<String> = vec![page, run.timeout.to_string()];
    args.extend(run.browsers.iter().cloned());
    let disc_dir = disc.parent().context("disc directory")?.to_path_buf();
    let argv = docker_run(
        &id,
        &owner_of(&results)?,
        &[(bundle.clone(), "/www", true), (disc_dir, "/www/disc", true), (results.clone(), "/out", false)],
        None,
        &args,
    );
    ensure!(Command::new("docker").args(&argv).status()?.success(), "the browser container failed");
    let mut out = Vec::new();
    for b in &run.browsers {
        let mut r = BrowserRun { browser: b.clone(), ..Default::default() };
        let Ok(text) = std::fs::read_to_string(results.join(format!("result-{b}.json"))) else {
            r.error = Some(format!("no result within {} s (log: {})", run.timeout, results.join(format!("{b}.log")).display()));
            out.push(r);
            continue;
        };
        let v: serde_json::Value = serde_json::from_str(&text)?;
        if let Some(e) = v["error"].as_str() {
            r.error = Some(e.to_string());
            out.push(r);
            continue;
        }
        let map = |o: &serde_json::Value| -> BTreeMap<u64, String> {
            o.as_object().into_iter().flatten().filter_map(|(k, v)| Some((k.parse().ok()?, v.as_str()?.to_string()))).collect()
        };
        r.stop = v["stop"].as_str().unwrap_or("").to_string();
        r.frames = v["frames"].as_u64().unwrap_or(0);
        r.ram_hashes = map(&v["ram"]);
        r.vram_hashes = map(&v["vram"]);
        r.audio_frames = v["audio"]["frames"].as_u64().unwrap_or(0);
        r.audio_hash = v["audio"]["hash"].as_str().unwrap_or("").to_string();
        r.watched = serde_json::from_value(v["watched"].clone()).unwrap_or_default();
        r.logs = serde_json::from_value(v["logs"].clone()).unwrap_or_default();
        r.events = serde_json::from_value(v["events"].clone()).unwrap_or_default();
        r.seconds = v["seconds"].as_f64().unwrap_or(0.0);
        out.push(r);
    }
    Ok(out)
}

/// `web-test pacing`: M5 exit criterion 3. The game runs idle at real
/// time for `--seconds` (default 60) in each browser; the frames it reached
/// must be within one of wall time × the frame rate. The report always
/// prints; it gates only when `psxw.local.toml` states the machine
/// (`machine = "…"`), since pacing says nothing about an unknown one.
fn pacing(ws: &Workspace, game: &str, args: &[&str]) -> Result<bool> {
    let mut browsers = vec!["chromium".to_string(), "firefox".to_string()];
    let mut seconds = 60u64;
    // Cost (plans/M12 task 13): a replay instead of idling, on the
    // moddable base, with mods loaded at start.
    let mut replay_name: Option<String> = None;
    let mut moddable = false;
    let mut mods: Vec<PageMod> = Vec::new();
    let mut tag = String::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--browsers" => browsers = it.next().context("--browsers needs a value")?.split(',').map(str::to_string).collect(),
            "--seconds" => seconds = it.next().context("--seconds needs a value")?.parse()?,
            "--replay" => replay_name = Some(it.next().context("--replay needs a value")?.to_string()),
            "--moddable" => moddable = true,
            "--mod" => {
                let d = std::path::PathBuf::from(it.next().context("--mod needs a mod directory or module")?);
                mods.push(if d.join("mod.js").is_file() { PageMod::script(d) } else { PageMod::Module(d) });
                moddable = true;
            }
            "--tag" => tag = format!("-{}", it.next().context("--tag needs a value")?),
            other => bail!("unknown option {other}"),
        }
    }
    let disc = ws.disc(game)?;
    let dir = ws.out_dir().join("web").join(format!("test-{game}{tag}"));
    let bundle = if moddable {
        let (base, _) = psxw_build::game::build(ws, &psxw_build::game::Options { moddable: true, ..psxw_build::game::Options::new(game) })?;
        crate::mods::moddable_bundle(ws, game, &base, dir)?
    } else {
        psxw_build::web::build_into(ws, game, dir)?
    };
    std::fs::create_dir_all(bundle.join("disc"))?;
    // NTSC, as the worker paces it (`web/worker.js`).
    let hz = 59.826;
    let mut frames = (seconds as f64 * hz).round() as u64;
    let mut extra = String::new();
    if let Some(r) = &replay_name {
        let replay = crate::wholegame::replay(ws, game, Some(r))?.0;
        frames = psxw_abi::replay::Header::parse(&replay).map_or(frames, |h| h.frames as u64);
        seconds = (frames as f64 / hz).ceil() as u64;
        std::fs::write(bundle.join("replay.rpl"), &replay)?;
        extra += "&replay=replay.rpl";
    }
    if !mods.is_empty() {
        let urls = mods.iter().map(|m| m.place(&bundle)).collect::<Result<Vec<_>>>()?;
        extra += &format!("&mods={}", urls.join(","));
    }
    let id = image(ws)?;
    let results = ws.out_dir().join("web").join(format!("test-{game}-pacing{tag}"));
    let _ = std::fs::remove_dir_all(&results);
    std::fs::create_dir_all(&results)?;
    let cue = disc.file_name().context("disc name")?.to_string_lossy().into_owned();
    let page = format!("index.html?discurl=disc/{cue}&autostart&frames={frames}&post{extra}");
    let mut argv_args: Vec<String> = vec![page, (seconds * 3 + 120).to_string()];
    argv_args.extend(browsers.iter().cloned());
    let disc_dir = disc.parent().context("disc directory")?.to_path_buf();
    let argv = docker_run(
        &id,
        &owner_of(&results)?,
        &[(bundle.clone(), "/www", true), (disc_dir, "/www/disc", true), (results.clone(), "/out", false)],
        None,
        &argv_args,
    );
    ensure!(Command::new("docker").args(&argv).status()?.success(), "the browser container failed");
    let machine = ws.local()?.machine;
    let mut pass = true;
    let mut report = Vec::new();
    for b in &browsers {
        let Ok(text) = std::fs::read_to_string(results.join(format!("result-{b}.json"))) else {
            println!("{b}: no result");
            pass = false;
            continue;
        };
        let r: serde_json::Value = serde_json::from_str(&text)?;
        let (reached, wall) = (r["frames"].as_u64().unwrap_or(0), r["seconds"].as_f64().unwrap_or(0.0));
        let drift = reached as f64 - wall * hz;
        let ok = r["error"].is_null() && reached == frames && drift.abs() <= 1.0;
        pass &= ok;
        println!(
            "{b}: {} frames in {wall:.3} s, drift {drift:+.2} frames, {} late; frame ms {}",
            reached, r["lagFrames"], r["frameMs"]
        );
        report.push(serde_json::json!({ "browser": b, "frames": reached, "seconds": wall, "drift": drift, "late": r["lagFrames"], "frame_ms": r["frameMs"], "slowest": r["slowest"], "within_one_frame": ok }));
    }
    let path = ws.out_dir().join("web").join(format!("pacing-{game}{tag}.json"));
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({ "game": game, "seconds": seconds, "hz": hz, "machine": machine, "replay": replay_name, "moddable": moddable, "browsers": report }))?,
    )?;
    match &machine {
        Some(m) => println!("pacing on {m}: {}", if pass { "PASS" } else { "FAIL" }),
        None => println!("pacing: {} (reported only: psxw.local.toml states no machine)", if pass { "within one frame" } else { "outside one frame" }),
    }
    eprintln!("report: {}", path.display());
    Ok(pass || machine.is_none())
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
    psxw_oracle::play::replay_from_presses("none", frames as u32, &[], 0, 2).expect("empty replay")
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
        Some(&"pacing") => {
            let g = it.next().context("web-test pacing needs a game")?;
            return pacing(ws, g, &args[2..]);
        }
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
