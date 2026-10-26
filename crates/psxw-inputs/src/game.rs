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
    #[serde(default)]
    pub oracle: OracleSection,
    #[serde(default)]
    pub conform: ConformSection,
}

/// `[conform]`: game facts the adapter-conformance scenarios need.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ConformSection {
    /// A function the interrupt scenarios install as a callback: it takes
    /// no arguments and changes `callback_state` (a 32-bit word) the same
    /// way on every call, so the word shows how often it ran.
    pub callback: Option<String>,
    pub callback_state: Option<String>,
}

/// `[oracle]`: the whole-game RAM oracle (`psxw oracle play`/`ramdiff`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct OracleSection {
    /// The game's own frame or tick counter, `symbol` or `symbol+offset`:
    /// a 32-bit word both sides trace every frame to align on.
    pub frame_counter: Option<String>,
    /// Symbols left out of the comparison, each with its reason.
    #[serde(default)]
    pub exclude_symbols: BTreeMap<String, String>,
    /// Bytes below the initial stack pointer left out as the stack
    /// (default 64 KiB).
    pub stack_size: Option<u32>,
    /// The default replay, built from button presses.
    pub replay: Option<ReplaySpec>,
}

/// A replay described by its presses: `BUTTON[+BUTTON..]@AT[:HOLD]` (hold
/// defaults to 4). `AT` and `HOLD` count `[oracle] frame_counter` values,
/// the replay's clock (psxw-abi `replay`); `frames` is how many vblanks
/// to run.
#[derive(Debug, Clone, Deserialize)]
pub struct ReplaySpec {
    pub frames: u32,
    #[serde(default)]
    pub press: Vec<String>,
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
