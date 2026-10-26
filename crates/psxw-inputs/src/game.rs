use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// `games/<id>/game.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct GameManifest {
    pub game: GameSection,
    pub containers: ContainersSection,
    pub c: CSection,
    pub probe: ProbeSection,
    pub reference: ReferenceSection,
    #[serde(default)]
    pub tier: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GameSection {
    pub id: String,
    pub title: String,
    pub serial: Option<String>,
    /// Key of `[game.<input>]` in `inputs.lock`.
    pub input: String,
    pub sdk: Option<String>,
    pub boundary: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContainersSection {
    pub splat_dir: PathBuf,
    pub symbols_dir: PathBuf,
    pub main: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CSection {
    pub include: Vec<PathBuf>,
    #[serde(default)]
    pub defines: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProbeSection {
    pub include_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub prelude: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReferenceSection {
    pub cpp: PathBuf,
    pub cppflags: Vec<String>,
    pub cc1: PathBuf,
    pub cc1flags: Vec<String>,
}
