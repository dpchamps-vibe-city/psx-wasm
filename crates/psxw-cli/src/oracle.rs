//! `psxw oracle …`: the device oracle harness (M3).

use anyhow::{Result, bail};
use psxw_inputs::Workspace;

pub const USAGE: &str = "oracle gen-header | images build";

pub fn run(ws: &Workspace, args: &[&str]) -> Result<()> {
    match args {
        ["gen-header"] => {
            let path = ws.root.join(psxw_oracle::wire::C_HEADER_PATH);
            std::fs::write(&path, psxw_oracle::c_header())?;
            eprintln!("wrote {}", path.display());
        }
        _ => bail!("usage: psxw {USAGE}"),
    }
    Ok(())
}
