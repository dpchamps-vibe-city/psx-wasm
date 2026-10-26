//! `psxw runtime build`: `runtime.wasm`, game-agnostic. The Rust runtime is
//! linked by rustc's wasm-ld with the guest's memory imported and its own
//! data at `RUNTIME_BASE`. The adapter table (name per id) is stamped into a
//! custom section, so the guest link binds the same ids.

use anyhow::{Context as _, Result, bail};
use psxw_inputs::Workspace;
use std::path::PathBuf;
use std::process::Command;

pub const ADAPTERS_SECTION: &str = "psxw.adapters";
/// SDK libraries that run over the runtime's devices (`sdk_psyq::DEVICE_LIBRARIES`).
pub const DEVICES_SECTION: &str = "psxw.devices";

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
    use wasm_encoder::Section;
    let section = wasm_encoder::CustomSection { name: ADAPTERS_SECTION.into(), data: names.join("\n").into_bytes().into() };
    section.append_to(&mut bytes);
    let devices = sdk_psyq::DEVICE_LIBRARIES.join("\n");
    wasm_encoder::CustomSection { name: DEVICES_SECTION.into(), data: devices.into_bytes().into() }.append_to(&mut bytes);
    let out = path(ws);
    std::fs::create_dir_all(out.parent().unwrap())?;
    std::fs::write(&out, &bytes)?;
    Ok(out)
}

fn section(wasm: &[u8], name: &str) -> Result<Vec<String>> {
    for p in wasmparser::Parser::new(0).parse_all(wasm) {
        if let wasmparser::Payload::CustomSection(r) = p? {
            if r.name() == name {
                return Ok(String::from_utf8(r.data().to_vec())?.lines().map(str::to_string).collect());
            }
        }
    }
    bail!("runtime.wasm has no {name} section; rebuild it with `psxw runtime build`")
}

/// The adapter names stamped into a runtime module, in id order.
pub fn adapters(wasm: &[u8]) -> Result<Vec<String>> {
    section(wasm, ADAPTERS_SECTION)
}

/// The SDK libraries a runtime module runs over its devices.
pub fn devices(wasm: &[u8]) -> Result<Vec<String>> {
    section(wasm, DEVICES_SECTION)
}
