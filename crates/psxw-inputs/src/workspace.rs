use crate::game::GameManifest;
use crate::lock::{Lockfile, RepoPin};
use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};

/// The psx-wasm checkout: `inputs.lock`, manifests, and the directories the
/// tools own (`.inputs/`, `.toolchain/`, `out/`).
#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
    pub lock: Lockfile,
}

impl Workspace {
    /// Find the workspace by walking up from the current directory to the
    /// first directory holding `inputs.lock`.
    pub fn discover() -> Result<Self> {
        let cwd = std::env::current_dir()?;
        let root = cwd
            .ancestors()
            .find(|d| d.join("inputs.lock").is_file())
            .ok_or_else(|| anyhow!("no inputs.lock above {}", cwd.display()))?
            .to_path_buf();
        Self::open(&root)
    }

    pub fn open(root: &Path) -> Result<Self> {
        let path = root.join("inputs.lock");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let lock = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(Self { root: root.to_path_buf(), lock })
    }

    pub fn game(&self, id: &str) -> Result<GameManifest> {
        let path = self.root.join("games").join(id).join("game.toml");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn game_pin(&self, manifest: &GameManifest) -> Result<&RepoPin> {
        self.lock
            .game
            .get(&manifest.game.input)
            .ok_or_else(|| anyhow!("inputs.lock has no [game.{}]", manifest.game.input))
    }

    /// The local checkout a pin points at.
    pub fn checkout(&self, pin: &RepoPin) -> PathBuf {
        self.root.join(&pin.repo)
    }

    pub fn inputs_dir(&self) -> PathBuf {
        self.root.join(".inputs")
    }

    pub fn toolchain_dir(&self) -> PathBuf {
        self.root.join(".toolchain")
    }

    pub fn out_dir(&self) -> PathBuf {
        self.root.join("out")
    }

    /// The pinned clang; errors if `psxw toolchain fetch` hasn't run.
    pub fn clang(&self) -> Result<PathBuf> {
        let pin = self
            .lock
            .toolchain
            .get("clang")
            .ok_or_else(|| anyhow!("inputs.lock has no [toolchain.clang]"))?;
        let clang = self.toolchain_dir().join(&pin.dist).join("bin").join("clang");
        if !clang.is_file() {
            return Err(anyhow!("{} missing; run `psxw toolchain fetch`", clang.display()));
        }
        Ok(clang)
    }
}
