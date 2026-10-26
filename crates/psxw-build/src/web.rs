//! `psxw build-web <game>` (M5): the browser bundle in `out/web/<game>/`:
//! `web/`'s page, Worker and worklet, the game's `game.wasm`, and the
//! renderer (`psxw-host-web` through the pinned wasm-bindgen CLI) in
//! `pkg/`. Local only: `game.wasm` is game-derived.

use anyhow::{Context as _, Result, bail};
use psxw_inputs::Workspace;
use std::path::PathBuf;
use std::process::Command;

pub fn out_dir(ws: &Workspace, game: &str) -> PathBuf {
    ws.out_dir().join("web").join(game)
}

pub fn build(ws: &Workspace, game: &str) -> Result<PathBuf> {
    let (report, _) = crate::game::build(ws, &crate::game::Options::new(game))?;
    let dir = out_dir(ws, game);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("pkg"))?;

    eprintln!("build-web: psxw-host-web for wasm32");
    let ok = Command::new("cargo")
        .current_dir(&ws.root)
        .args(["build", "-q", "--release", "--target", "wasm32-unknown-unknown", "-p", "psxw-host-web"])
        .status()
        .context("running cargo")?
        .success();
    if !ok {
        bail!("building psxw-host-web failed");
    }
    let wasm = ws.root.join("target/wasm32-unknown-unknown/release/psxw_host_web.wasm");
    let ok = Command::new(ws.tool("wasm-bindgen", "wasm-bindgen")?)
        .args(["--target", "web", "--no-typescript", "--out-dir"])
        .arg(dir.join("pkg"))
        .arg(&wasm)
        .status()
        .context("running wasm-bindgen")?
        .success();
    if !ok {
        bail!("wasm-bindgen failed");
    }
    for e in std::fs::read_dir(ws.root.join("web"))? {
        let p = e?.path();
        if p.is_file() {
            std::fs::copy(&p, dir.join(p.file_name().unwrap_or_default()))?;
        }
    }
    std::fs::copy(&report.wasm, dir.join("game.wasm"))?;
    Ok(dir)
}
