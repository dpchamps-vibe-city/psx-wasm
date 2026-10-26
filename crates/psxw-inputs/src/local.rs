//! `psxw.local.toml` (gitignored): paths on this machine the user supplies,
//! such as disc images. Inputs are never searched for: a missing key is an
//! error naming the key to set.

use crate::workspace::Workspace;
use anyhow::{Context as _, Result, anyhow};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const FILE: &str = "psxw.local.toml";

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Local {
    #[serde(default)]
    pub game: BTreeMap<String, LocalGame>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LocalGame {
    /// The game's disc image: a `.cue` (with raw MODE2/2352 `.bin` tracks).
    pub disc: Option<PathBuf>,
}

impl Workspace {
    pub fn local(&self) -> Result<Local> {
        let path = self.root.join(FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
            Err(_) => Ok(Local::default()),
        }
    }

    /// The disc image the user configured for a game.
    pub fn disc(&self, game: &str) -> Result<PathBuf> {
        let local = self.local()?;
        let disc = local.game.get(game).and_then(|g| g.disc.clone()).ok_or_else(|| {
            anyhow!("no disc image configured: set `[game.{game}] disc = \"/path/to/disc.cue\"` in {FILE}")
        })?;
        let disc = if disc.is_relative() { self.root.join(disc) } else { disc };
        if !disc.is_file() {
            return Err(anyhow!("{} ([game.{game}] disc in {FILE}) does not exist", disc.display()));
        }
        Ok(disc)
    }
}
