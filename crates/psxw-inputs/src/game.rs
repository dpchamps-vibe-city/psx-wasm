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
    /// Per-function tier pins, by function name.
    #[serde(default)]
    pub tier: BTreeMap<String, TierPin>,
    #[serde(default)]
    pub difftest: DifftestSection,
    #[serde(default)]
    pub run: RunSection,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RunSection {
    /// A function the game enters once it reaches its main loop (the boot
    /// check requires it).
    pub main_loop_symbol: Option<String>,
    /// `realistic` or `instant` CD timing by default.
    pub cd_timing: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DifftestSection {
    /// Functions excluded from the differential comparison, with the
    /// recorded reason (reported as `excluded`, never as a pass).
    #[serde(default)]
    pub exclude: BTreeMap<String, String>,
}

/// A function forced to a tier, with the evidence for it.
#[derive(Debug, Clone, Deserialize)]
pub struct TierPin {
    /// `lift` (the C can't reproduce the original) or `c`.
    pub tier: String,
    pub reason: String,
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
