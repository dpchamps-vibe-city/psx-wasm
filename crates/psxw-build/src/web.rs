//! `psxw build-web <game>` (M5): the browser bundle in `out/web/<game>/`:
//! `web/`'s page, Worker and worklet, the game's `game.wasm`, and the
//! renderer (`psxw-host-web` through the pinned wasm-bindgen CLI) in
//! `pkg/`, and the unmerged guest.wasm and runtime.wasm for dev mode.
//! Local only: the wasm modules are game-derived.

use anyhow::{Context as _, Result, bail};
use psxw_inputs::Workspace;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

pub fn out_dir(ws: &Workspace, game: &str) -> PathBuf {
    ws.out_dir().join("web").join(game)
}

pub fn build(ws: &Workspace, game: &str) -> Result<PathBuf> {
    build_into(ws, game, out_dir(ws, game))
}

/// The bundle in `dir`, which it replaces. The browser tests build their
/// own, so a run never rewrites a bundle someone is serving.
pub fn build_into(ws: &Workspace, game: &str, dir: PathBuf) -> Result<PathBuf> {
    let (report, _) = crate::game::build(ws, &crate::game::Options::new(game))?;
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    pkg(ws, &dir.join("pkg"))?;
    for e in std::fs::read_dir(ws.root.join("web"))? {
        let p = e?.path();
        if p.is_file() {
            std::fs::copy(&p, dir.join(p.file_name().unwrap_or_default()))?;
        }
    }
    std::fs::copy(&report.wasm, dir.join("game.wasm"))?;
    // What the page needs to record replays: the clock they index by.
    let inputs = psxw_lift::run::load(ws, game)?;
    let clock = crate::game::frame_counter(ws, game, &inputs).ok();
    std::fs::write(dir.join("game.json"), serde_json::to_string(&serde_json::json!({ "id": game, "frame_counter": clock }))?)?;
    // Dev mode (`?dev`): the unmerged modules, wired in the Worker.
    std::fs::copy(crate::game::out_dir(ws, game).join("guest.wasm"), dir.join("guest.wasm"))?;
    std::fs::copy(crate::runtime::path(ws), dir.join("runtime.wasm"))?;
    check_dev(&dir)?;
    Ok(dir)
}

fn interface(path: &std::path::Path) -> Result<(Vec<(String, String)>, BTreeSet<String>)> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let (mut imports, mut exports) = (Vec::new(), BTreeSet::new());
    for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
        match payload? {
            wasmparser::Payload::ImportSection(r) => {
                for i in r.into_imports() {
                    let i = i?;
                    imports.push((i.module.to_string(), i.name.to_string()));
                }
            }
            wasmparser::Payload::ExportSection(r) => {
                for e in r {
                    exports.insert(e?.name.to_string());
                }
            }
            _ => {}
        }
    }
    Ok((imports, exports))
}

/// Dev mode wires guest.wasm and runtime.wasm the way wasm-merge does
/// (worker.js `instantiateDev`): the guest's `psx.*` imports are runtime
/// exports, the runtime's `guest.*` imports are guest exports, everything
/// else is `host.*`, the pair exports all that game.wasm does, and both
/// carry the same adapter table.
fn check_dev(dir: &std::path::Path) -> Result<()> {
    let guest_table = crate::runtime::adapters(&std::fs::read(dir.join("guest.wasm"))?)?;
    if guest_table != crate::runtime::adapters(&std::fs::read(dir.join("runtime.wasm"))?)? {
        bail!("guest.wasm was linked against another runtime's adapter table");
    }
    let (gi, ge) = interface(&dir.join("guest.wasm"))?;
    let (ri, re) = interface(&dir.join("runtime.wasm"))?;
    let (_, game) = interface(&dir.join("game.wasm"))?;
    let mut bad: Vec<String> = Vec::new();
    for (m, n) in &gi {
        if m != "psx" || !re.contains(n) {
            bad.push(format!("guest.wasm imports {m}.{n}"));
        }
    }
    for (m, n) in &ri {
        match m.as_str() {
            "guest" if ge.contains(n) => {}
            "host" => {}
            _ => bad.push(format!("runtime.wasm imports {m}.{n}")),
        }
    }
    bad.extend(game.iter().filter(|n| !ge.contains(*n) && !re.contains(*n)).map(|n| format!("game.wasm exports {n}, the pair doesn't")));
    if !bad.is_empty() {
        bail!("dev mode can't wire guest.wasm and runtime.wasm: {}", bad.join("; "));
    }
    Ok(())
}

/// The renderer package (`psxw-host-web` and its wasm-bindgen glue) in
/// `out`.
pub fn pkg(ws: &Workspace, out: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(out)?;
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
        .arg(out)
        .arg(&wasm)
        .status()
        .context("running wasm-bindgen")?
        .success();
    if !ok {
        bail!("wasm-bindgen failed");
    }
    Ok(())
}
