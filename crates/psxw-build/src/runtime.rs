//! `psxw runtime build`: `runtime.wasm`, game-agnostic. The Rust runtime is
//! linked by rustc's wasm-ld with the guest's memory imported and its own
//! data at `RUNTIME_BASE`. The adapter table (name per id) is stamped into a
//! custom section, so the guest link binds the same ids.

use anyhow::{Context as _, Result, bail};
use psxw_inputs::Workspace;
use std::path::PathBuf;
use std::process::Command;

pub const ADAPTERS_SECTION: &str = "psxw.adapters";

pub fn path(ws: &Workspace) -> PathBuf {
    ws.out_dir().join("runtime").join("runtime.wasm")
}

pub fn build(ws: &Workspace) -> Result<PathBuf> {
    let status = Command::new("cargo")
        .current_dir(&ws.root)
        .args(["rustc", "-q", "-p", "psx-runtime", "--target", "wasm32-unknown-unknown", "--profile", "runtime"])
        .args(["--crate-type", "cdylib", "--"])
        .arg(format!("-Clink-arg=--import-memory={},{}", psxw_abi::guest::MODULE, psxw_abi::guest::MEMORY))
        .arg(format!("-Clink-arg=--global-base={}", psxw_abi::RUNTIME_BASE))
        .status()
        .context("running cargo")?;
    if !status.success() {
        bail!("building the runtime failed");
    }
    let built = ws.root.join("target/wasm32-unknown-unknown/runtime/psx_runtime.wasm");
    let mut bytes = std::fs::read(&built).with_context(|| format!("reading {}", built.display()))?;
    let names: Vec<&str> = sdk_psyq::adapters().into_iter().map(|(n, _)| n).collect();
    let section = wasm_encoder::CustomSection { name: ADAPTERS_SECTION.into(), data: names.join("\n").into_bytes().into() };
    use wasm_encoder::Section;
    section.append_to(&mut bytes);
    let out = path(ws);
    std::fs::create_dir_all(out.parent().unwrap())?;
    std::fs::write(&out, &bytes)?;
    Ok(out)
}

/// The adapter names stamped into a runtime module, in id order.
pub fn adapters(wasm: &[u8]) -> Result<Vec<String>> {
    for p in wasmparser::Parser::new(0).parse_all(wasm) {
        if let wasmparser::Payload::CustomSection(r) = p? {
            if r.name() == ADAPTERS_SECTION {
                return Ok(String::from_utf8(r.data().to_vec())?.lines().map(str::to_string).collect());
            }
        }
    }
    bail!("runtime.wasm has no {ADAPTERS_SECTION} section; rebuild it with `psxw runtime build`")
}
