use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct Lockfile {
    #[serde(default)]
    pub toolchain: BTreeMap<String, ToolPin>,
    #[serde(default)]
    pub game: BTreeMap<String, RepoPin>,
    #[serde(default)]
    pub sdk: BTreeMap<String, RepoPin>,
    /// Each game's mods repository (M11), which the CI gate `mods` checks,
    /// builds and tests.
    #[serde(default)]
    pub mods: BTreeMap<String, RepoPin>,
    /// The device oracle's images and their sources (M3). Optional so a
    /// lockfile without them still parses.
    #[serde(default)]
    pub oracle: Option<OraclePins>,
}

/// `[oracle]`: everything the oracle's Docker images are built from.
#[derive(Debug, Clone, Deserialize)]
pub struct OraclePins {
    /// Base image of both images, pinned by digest (`name@sha256:…`).
    pub base_image: String,
    /// snapshot.debian.org timestamp the images install packages from.
    pub debian_snapshot: String,
    pub beetle: SourcePin,
    pub openbios: OpenBiosPin,
    pub psn00bsdk: ReleasePin,
}

/// A source repository fetched at one commit.
#[derive(Debug, Clone, Deserialize)]
pub struct SourcePin {
    pub repo: String,
    pub commit: String,
}

impl SourcePin {
    pub fn short(&self) -> &str {
        &self.commit[..self.commit.len().min(7)]
    }
}

/// PCSX-Redux at a commit, plus the nugget submodule commit it records.
#[derive(Debug, Clone, Deserialize)]
pub struct OpenBiosPin {
    pub repo: String,
    pub commit: String,
    pub nugget_repo: String,
    pub nugget_commit: String,
}

impl OpenBiosPin {
    pub fn short(&self) -> &str {
        &self.commit[..self.commit.len().min(7)]
    }
}

/// A prebuilt release archive, verified by sha256.
#[derive(Debug, Clone, Deserialize)]
pub struct ReleasePin {
    pub release: String,
    pub url: String,
    pub sha256: String,
    /// The toolchain the archive carries, e.g. `gcc-mipsel-none-elf-12.3.0`.
    #[serde(default)]
    pub toolchain: Option<String>,
}

/// A downloadable toolchain distribution, verified by sha256.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolPin {
    pub dist: String,
    pub url: String,
    pub sha256: String,
}

/// One pinned input repository.
#[derive(Debug, Clone, Deserialize)]
pub struct RepoPin {
    /// Local checkout, relative to the workspace root. Used as the object
    /// store for snapshots and as the root for user-supplied originals.
    pub repo: PathBuf,
    pub remote: Option<String>,
    pub commit: String,
    /// Produces the repo's matching build (not used by the census).
    #[serde(default)]
    pub build: Vec<String>,
    pub disc: Option<DiscPin>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiscPin {
    pub exe: String,
    pub exe_sha256: String,
    /// sha1 of each image file (`.bin`), in cue order.
    #[serde(default)]
    pub files: Vec<DiscFile>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct DiscFile {
    pub name: String,
    pub sha1: String,
}

impl RepoPin {
    pub fn short(&self) -> &str {
        &self.commit[..self.commit.len().min(7)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_without_oracle() {
        let lock: Lockfile = toml::from_str("[toolchain.clang]\ndist = \"d\"\nurl = \"u\"\nsha256 = \"s\"\n").unwrap();
        assert!(lock.oracle.is_none());
    }

    #[test]
    fn parses_workspace_lockfile() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../inputs.lock");
        let lock: Lockfile = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let oracle = lock.oracle.expect("[oracle] pinned");
        assert!(oracle.base_image.contains("@sha256:"));
        assert_eq!(oracle.beetle.commit.len(), 40);
        assert_eq!(oracle.openbios.commit.len(), 40);
        assert_eq!(oracle.openbios.nugget_commit.len(), 40);
        assert_eq!(oracle.psn00bsdk.sha256.len(), 64);
    }
}
