//! Pinned toolchain distributions under `.toolchain/`.

use crate::workspace::Workspace;
use anyhow::{Result, anyhow, bail};
use std::process::Command;

/// Download, verify and unpack `[toolchain.<name>]`. A no-op when present.
pub fn fetch(ws: &Workspace, name: &str) -> Result<()> {
    let pin = ws
        .lock
        .toolchain
        .get(name)
        .ok_or_else(|| anyhow!("inputs.lock has no [toolchain.{name}]"))?;
    let dest = ws.toolchain_dir().join(&pin.dist);
    if dest.is_dir() {
        eprintln!("{name}: {} already present", pin.dist);
        return Ok(());
    }
    let dl = ws.toolchain_dir().join("dl");
    std::fs::create_dir_all(&dl)?;
    let archive = dl.join(format!("{}.tar.gz", pin.dist));
    eprintln!("{name}: downloading {}", pin.url);
    let ok = Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(&archive)
        .arg(&pin.url)
        .status()?
        .success();
    if !ok {
        bail!("download of {} failed", pin.url);
    }
    let got = crate::sha256_file(&archive)?;
    if got != pin.sha256 {
        bail!("{}: sha256 {got} != pinned {}", archive.display(), pin.sha256);
    }
    let ok = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(ws.toolchain_dir())
        .status()?
        .success();
    if !ok || !dest.is_dir() {
        bail!("unpacking {} did not produce {}", archive.display(), dest.display());
    }
    std::fs::remove_dir_all(&dl)?;
    eprintln!("{name}: {} ready", pin.dist);
    Ok(())
}
