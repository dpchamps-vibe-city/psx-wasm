//! The `psxw-render-gl` Docker image: Mesa llvmpipe (EGL surfaceless) and
//! the pinned Rust toolchain, built from `oracle/docker/render.Dockerfile`
//! with the pins of `inputs.lock` (`[oracle]` base image and Debian
//! snapshot, `[oracle.render_gl]` Rust), and recorded in
//! `.oracle/render-gl.json` the way `psxw oracle images build` records the
//! emulator images. The render server is built inside it from the
//! workspace, mounted read-only, into `.oracle/render-gl/target`.

use anyhow::{Context, Result, bail, ensure};
use psxw_inputs::Workspace;
use psxw_oracle::images::{docker_available, docker_output, image_id, oracle_dir, owner_of};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const IMAGE: &str = "psxw-render-gl:local";
pub const RECORD_SCHEMA_VERSION: u32 = 1;

/// `[oracle.render_gl]` plus the `[oracle]` pins the image shares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pins {
    pub base_image: String,
    pub debian_snapshot: String,
    pub rust_version: String,
    pub rust_url: String,
    pub rust_sha256: String,
}

impl Pins {
    pub fn load(ws: &Workspace) -> Result<Self> {
        let path = ws.root.join("inputs.lock");
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let lock: toml::Value = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let oracle = lock.get("oracle").context("inputs.lock has no [oracle] section")?;
        let rg = oracle.get("render_gl").context("inputs.lock has no [oracle.render_gl] section")?;
        let s = |v: &toml::Value, k: &str| -> Result<String> {
            v.get(k).and_then(|x| x.as_str()).map(str::to_string).with_context(|| format!("inputs.lock: {k} missing"))
        };
        Ok(Pins {
            base_image: s(oracle, "base_image")?,
            debian_snapshot: s(oracle, "debian_snapshot")?,
            rust_version: s(rg, "rust_version")?,
            rust_url: s(rg, "rust_url")?,
            rust_sha256: s(rg, "rust_sha256")?,
        })
    }
}

/// `.oracle/render-gl.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub schema_version: u32,
    pub built: String,
    pub pins: Pins,
    pub tag: String,
    /// Local image id (`sha256:…`).
    pub id: String,
    /// What the image says it holds (Mesa, glvnd, rustc versions).
    pub sources: Value,
}

pub fn record_path(ws: &Workspace) -> PathBuf {
    oracle_dir(ws).join("render-gl.json")
}

pub fn work_dir(ws: &Workspace) -> PathBuf {
    oracle_dir(ws).join("render-gl")
}

/// Build the image from the pins and record it.
pub fn build(ws: &Workspace, no_cache: bool) -> Result<Record> {
    ensure!(docker_available(), "docker is not available (docker version failed)");
    let pins = Pins::load(ws)?;
    let logs = ws.out_dir().join("oracle").join("images");
    std::fs::create_dir_all(&logs)?;
    let log = logs.join("render-build.log");
    let mut cmd = Command::new("docker");
    cmd.arg("build").arg("-f").arg(ws.root.join("oracle/docker/render.Dockerfile")).arg("-t").arg(IMAGE).arg("--progress=plain");
    if no_cache {
        cmd.arg("--no-cache");
    }
    for (k, v) in [
        ("BASE_IMAGE", &pins.base_image),
        ("DEBIAN_SNAPSHOT", &pins.debian_snapshot),
        ("RUST_VERSION", &pins.rust_version),
        ("RUST_URL", &pins.rust_url),
        ("RUST_SHA256", &pins.rust_sha256),
    ] {
        cmd.arg("--build-arg").arg(format!("{k}={v}"));
    }
    cmd.arg(ws.root.join("oracle"));
    let file = std::fs::File::create(&log)?;
    cmd.stdout(file.try_clone()?).stderr(file);
    eprintln!("gpu oracle: building {IMAGE} (log: {})", log.display());
    if !cmd.status().context("running docker build")?.success() {
        bail!("docker build of {IMAGE} failed; see {}", log.display());
    }
    let sources: Value = serde_json::from_str(&docker_output(&[
        "run".into(),
        "--rm".into(),
        "--network=none".into(),
        IMAGE.into(),
        "cat".into(),
        "/opt/psxw/sources.json".into(),
    ])?)?;
    ensure!(
        sources.get("debian_snapshot").and_then(Value::as_str) == Some(&pins.debian_snapshot),
        "{IMAGE} reports snapshot {:?}, inputs.lock pins {}",
        sources.get("debian_snapshot"),
        pins.debian_snapshot
    );
    let rec = Record {
        schema_version: RECORD_SCHEMA_VERSION,
        built: psxw_oracle::golden::now_rfc3339(),
        pins,
        tag: IMAGE.into(),
        id: image_id(IMAGE)?,
        sources,
    };
    std::fs::create_dir_all(oracle_dir(ws))?;
    std::fs::write(record_path(ws), serde_json::to_string_pretty(&rec)?)?;
    Ok(rec)
}

/// The recorded image, rebuilt when missing, built from other pins, or
/// gone from the daemon.
pub fn ensure(ws: &Workspace) -> Result<Record> {
    let pins = Pins::load(ws)?;
    if let Ok(text) = std::fs::read_to_string(record_path(ws))
        && let Ok(rec) = serde_json::from_str::<Record>(&text)
        && rec.schema_version == RECORD_SCHEMA_VERSION
        && rec.pins == pins
        && image_id(&rec.tag).ok().as_deref() == Some(rec.id.as_str())
    {
        return Ok(rec);
    }
    build(ws, false)
}

/// Build (incrementally) the render server inside the image, from the
/// workspace mounted read-only. Returns the directory holding the binary.
pub fn build_server(ws: &Workspace, rec: &Record) -> Result<PathBuf> {
    let work = work_dir(ws);
    let (cargo, target) = (work.join("cargo"), work.join("target"));
    std::fs::create_dir_all(&cargo)?;
    std::fs::create_dir_all(&target)?;
    let user = owner_of(&work)?;
    let mount = |host: &Path, guest: &str, ro: bool| format!("{}:{guest}{}", host.display(), if ro { ":ro" } else { "" });
    let args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--user".into(),
        user,
        "-v".into(),
        mount(&ws.root, "/src", true),
        "-v".into(),
        mount(&cargo, "/cargo", false),
        "-v".into(),
        mount(&target, "/target", false),
        "-e".into(),
        "CARGO_HOME=/cargo".into(),
        "-e".into(),
        "CARGO_TARGET_DIR=/target".into(),
        rec.id.clone(),
        "cargo".into(),
        "build".into(),
        "--release".into(),
        "--locked".into(),
        "--manifest-path".into(),
        "/src/Cargo.toml".into(),
        "-p".into(),
        "psx-render-gl".into(),
        "--features".into(),
        "headless".into(),
        "--bin".into(),
        "psxw-render-server".into(),
    ];
    let out = Command::new("docker").args(&args).output().context("running docker")?;
    let log = ws.out_dir().join("oracle").join("runs");
    std::fs::create_dir_all(&log)?;
    std::fs::write(log.join("render-server-build.log"), [&out.stdout[..], &out.stderr[..]].concat())?;
    if !out.status.success() {
        bail!("building psxw-render-server in {IMAGE} failed:\n{}", String::from_utf8_lossy(&out.stderr));
    }
    let dir = target.join("release");
    ensure!(dir.join("psxw-render-server").is_file(), "psxw-render-server missing after the build");
    Ok(dir)
}
