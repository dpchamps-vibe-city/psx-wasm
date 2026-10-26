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
}

impl RepoPin {
    pub fn short(&self) -> &str {
        &self.commit[..self.commit.len().min(7)]
    }
}
