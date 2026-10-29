//! Inputs: `inputs.lock`, read-only snapshots of the pinned decomp repos, the
//! per-game manifest, and the address map built from the repos' tracked
//! configuration. This is the only crate that knows how a decomp repo is laid
//! out; everything downstream sees the model. See `docs/design.md`
//! "Pinning the inputs".

pub mod boundary;
pub mod game;
pub mod headers;
pub mod identity;
pub mod local;
pub mod lock;
pub mod map;
pub mod originals;
pub mod psxexe;
pub mod snapshot;
pub mod splat;
pub mod symbols;
pub mod toolchain;
pub mod workspace;

pub use game::GameManifest;
pub use lock::Lockfile;
pub use map::{AddressMap, Resolved, Via};
pub use workspace::Workspace;

/// How a guest function is implemented in this build. Exactly one per
/// (container, address).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Decompiled C compiled by clang.
    C,
    /// Rust runtime import (boundary manifest).
    Replace,
    /// Original MIPS translated ahead of time from a relocatable object.
    Lift,
}

/// Boundary manifest policy for an SDK library or function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// C when the SDK decomp has it, otherwise lift.
    Compile,
    /// Always the runtime's implementation.
    Replace,
    /// Never use C even if present (pinned while a C version misbehaves).
    Lift,
}

/// Lowercase hex sha256 of a byte slice.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex(&Sha256::digest(bytes))
}

/// Lowercase hex sha256 of a file, streamed.
pub fn sha256_file(path: &std::path::Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Parse `0x...` or decimal.
pub fn parse_u32(s: &str) -> Option<u32> {
    let s = s.trim();
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u32::from_str_radix(h, 16).ok(),
        None => s.parse().ok(),
    }
}
