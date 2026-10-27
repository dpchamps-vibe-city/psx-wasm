//! A mods repository (plans/M11 task 9): `psxmods.json` names the game and
//! the psx-wasm checkout; `mods/<id>/` holds each mod, `sets/<name>.json`
//! mods applied together; `.psxw/` everything generated (the mod SDK, each
//! mod's `mod.h`, `compile_commands.json`, builds). `psxmods.lock.json`
//! records the SDK the repo builds against.

use crate::decl::Mod;
use crate::genfiles::{self, Flags};
use crate::plan::{self, Plan};
use crate::symbols::SymbolTable;
use anyhow::{Context as _, Result, bail, ensure};
use psxw_inputs::Workspace;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const FILE: &str = "psxmods.json";
pub const LOCK: &str = "psxmods.lock.json";
pub const STATE: &str = ".psxw";

/// `psxmods.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub game: String,
    /// The psx-wasm checkout, relative to the workspace.
    pub psx_wasm: String,
}

/// `sets/<name>.json`: mods applied together, in this order.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Set {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub mods: Vec<String>,
}

/// `psxmods.lock.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Lock {
    pub game: String,
    /// The psx-wasm commit the SDK was exported from.
    pub psx_wasm: String,
    /// The decomp commit psx-wasm pins.
    pub decomp: String,
    pub identity: Identity,
    /// Each mod's resolved function targets, so a bump can say what a
    /// renamed one became.
    pub targets: BTreeMap<String, Vec<LockedTarget>>,
}

/// The game's files, by hash (no game bytes).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub exe_sha256: String,
    /// Each container's original file, by sha1 (its splat config's).
    pub containers: BTreeMap<String, String>,
    /// The disc's image files.
    pub disc: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LockedTarget {
    pub hook: usize,
    pub target: String,
    pub container: String,
    pub address: String,
}

pub struct ModsWorkspace {
    pub root: PathBuf,
    pub manifest: Manifest,
    /// Generated files (default `<root>/.psxw`).
    pub state: PathBuf,
    /// The psx-wasm checkout.
    pub psx_wasm: PathBuf,
}

impl ModsWorkspace {
    /// The workspace above the current directory, if any.
    pub fn discover() -> Result<Option<Self>> {
        let cwd = std::env::current_dir()?;
        match cwd.ancestors().find(|d| d.join(FILE).is_file()) {
            Some(root) => Ok(Some(Self::open(root, None, None)?)),
            None => Ok(None),
        }
    }

    /// `state` and `psx_wasm` override the defaults (CI runs a pinned
    /// snapshot of the repo against the psx-wasm it is part of).
    pub fn open(root: &Path, state: Option<PathBuf>, psx_wasm: Option<PathBuf>) -> Result<Self> {
        let path = root.join(FILE);
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let manifest: Manifest = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let psx_wasm = psx_wasm.unwrap_or_else(|| root.join(&manifest.psx_wasm));
        Ok(Self { root: root.to_path_buf(), state: state.unwrap_or_else(|| root.join(STATE)), manifest, psx_wasm })
    }

    pub fn game(&self) -> &str {
        &self.manifest.game
    }

    pub fn psx(&self) -> Result<Workspace> {
        Workspace::open(&self.psx_wasm).with_context(|| format!("the psx-wasm checkout ({}, psxmods.json psx_wasm)", self.psx_wasm.display()))
    }

    pub fn sdk_dir(&self) -> PathBuf {
        self.state.join("sdk")
    }

    pub fn include_dir(&self, id: &str) -> PathBuf {
        self.state.join("mods").join(id).join("include")
    }

    pub fn build_dir(&self, label: &str) -> PathBuf {
        self.state.join("build").join(label)
    }

    pub fn mods_dir(&self) -> PathBuf {
        self.root.join("mods")
    }

    /// Every mod, by id.
    pub fn all(&self) -> Result<Vec<Mod>> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(self.mods_dir()) else { return Ok(out) };
        let mut dirs: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.join(crate::decl::FILE).is_file()).collect();
        dirs.sort();
        for d in dirs {
            out.push(Mod::load(&d)?);
        }
        Ok(out)
    }

    pub fn set(&self, name: &str) -> Result<Option<Set>> {
        let p = self.root.join("sets").join(format!("{name}.json"));
        let Ok(text) = std::fs::read_to_string(&p) else { return Ok(None) };
        Ok(Some(serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))?))
    }

    /// A mod, a set, or (none given) every mod: a label and the mods in
    /// the order to apply them.
    pub fn select(&self, name: Option<&str>) -> Result<(String, Vec<Mod>)> {
        let all = self.all()?;
        let pick = |ids: &[String]| -> Result<Vec<Mod>> {
            ids.iter()
                .map(|id| all.iter().find(|m| m.id() == id).cloned().with_context(|| format!("no mod {id} in {}", self.mods_dir().display())))
                .collect()
        };
        match name {
            None => Ok(("all".into(), all.clone())),
            Some(n) => {
                if let Some(m) = all.iter().find(|m| m.id() == n) {
                    return Ok((n.to_string(), vec![m.clone()]));
                }
                match self.set(n)? {
                    Some(s) => Ok((format!("set-{n}"), pick(&s.mods)?)),
                    None => bail!("{n} is neither a mod (mods/{n}/mod.json) nor a set (sets/{n}.json)"),
                }
            }
        }
    }

    pub fn table(&self) -> Result<SymbolTable> {
        let p = self.sdk_dir().join("symbols.json");
        ensure!(p.is_file(), "no mod SDK in {}: run psxw mod sdk", self.sdk_dir().display());
        SymbolTable::load(&p)
    }

    pub fn lock(&self) -> Result<Option<Lock>> {
        let p = self.root.join(LOCK);
        let Ok(text) = std::fs::read_to_string(&p) else { return Ok(None) };
        Ok(Some(serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))?))
    }

    /// The SDK's compile flags.
    pub fn flags(&self) -> Result<Flags> {
        let p = self.sdk_dir().join("flags.json");
        let text = std::fs::read_to_string(&p).with_context(|| format!("reading {}: run psxw mod sdk", p.display()))?;
        let f: SdkFlags = serde_json::from_str(&text)?;
        let sdk = self.sdk_dir();
        Ok(Flags { clang: f.clang.into(), prelude: sdk.join(&f.prelude), includes: f.includes.iter().map(|i| sdk.join(i)).collect(), defines: f.defines })
    }

    /// Write each mod's `mod.h` and the workspace's
    /// `compile_commands.json` (and a `.clangd` pointing at it).
    pub fn generate(&self, table: &SymbolTable, mods: &[Mod], plan: &Plan) -> Result<()> {
        let flags = self.flags()?;
        let prelude: Vec<String> = SdkFlags::load(&self.sdk_dir())?.prelude_headers;
        let mut cmds = Vec::new();
        for (mi, m) in mods.iter().enumerate() {
            let inc = self.include_dir(m.id());
            crate::build::write_if_changed(&inc.join("mod.h"), &genfiles::mod_h(plan, mods, mi, &prelude))?;
            crate::build::write_if_changed(&inc.join("psxmod.h"), genfiles::PSXMOD_H)?;
            let clang = psxw_cc::Clang::new(flags.clang.clone(), m.dir.clone(), [flags.includes.clone(), vec![inc.clone()]].concat(), flags.defines.clone(), &self.state.join("scan"))?;
            let defines = |src: &Path| -> Vec<String> {
                let rel = src.strip_prefix(&m.dir).map(Path::to_path_buf).unwrap_or_else(|_| src.to_path_buf());
                crate::compile::static_defines(&clang, m, &rel).unwrap_or_default()
            };
            cmds.extend(genfiles::compile_commands(&flags, m, &inc, &defines)?);
        }
        let _ = table;
        crate::build::write_if_changed(&self.state.join("compile_commands.json"), &serde_json::to_string_pretty(&cmds)?)?;
        let clangd = self.root.join(".clangd");
        if !clangd.exists() {
            std::fs::write(&clangd, "CompileFlags:\n  CompilationDatabase: .psxw\n")?;
        }
        Ok(())
    }

    /// The SDK must be the one psx-wasm's pins describe.
    pub fn check_pins(&self, psx: &Workspace) -> Result<()> {
        let lock = self.lock()?.with_context(|| format!("no {LOCK}: run psxw mod sdk"))?;
        let (decomp, identity) = pins(psx, self.game())?;
        if lock.decomp != decomp || lock.identity != identity {
            bail!(
                "the mod SDK is for {} @ {}, but psx-wasm pins {}: run psxw mod sdk --bump",
                lock.game,
                &lock.decomp[..7.min(lock.decomp.len())],
                &decomp[..7.min(decomp.len())]
            );
        }
        Ok(())
    }
}

/// `sdk/flags.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdkFlags {
    pub clang: String,
    pub std: String,
    /// Relative to the SDK directory.
    pub prelude: String,
    pub includes: Vec<String>,
    pub defines: Vec<String>,
    /// The game's prelude headers, which `mod.h` includes first.
    pub prelude_headers: Vec<String>,
}

impl SdkFlags {
    pub fn load(sdk: &Path) -> Result<Self> {
        let p = sdk.join("flags.json");
        let text = std::fs::read_to_string(&p).with_context(|| format!("reading {}: run psxw mod sdk", p.display()))?;
        Ok(serde_json::from_str(&text)?)
    }
}

/// psx-wasm's current pins for a game: the decomp commit and the game's
/// file hashes.
pub fn pins(psx: &Workspace, game: &str) -> Result<(String, Identity)> {
    let manifest = psx.game(game)?;
    let pin = psx.game_pin(&manifest)?;
    let snap = psxw_inputs::snapshot::ensure(psx, &manifest.game.input, pin)?;
    let map = psxw_inputs::AddressMap::load(&snap, &manifest)?;
    let mut id = Identity::default();
    if let Some(d) = &pin.disc {
        id.exe_sha256 = d.exe_sha256.clone();
        id.disc = d.files.iter().map(|f| (f.name.clone(), f.sha1.clone())).collect();
    }
    for c in &map.containers {
        if let Some(s) = &c.sha1 {
            id.containers.insert(c.id.clone(), s.clone());
        }
    }
    Ok((pin.commit.clone(), id))
}

fn psx_commit(psx: &Workspace) -> String {
    std::process::Command::new("git")
        .arg("-C")
        .arg(&psx.root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn copy_tree(from: &Path, to: &Path, ext: &[&str]) -> Result<usize> {
    let mut n = 0;
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let p = e?.path();
        let dst = to.join(p.file_name().unwrap_or_default());
        if p.is_dir() {
            n += copy_tree(&p, &dst, ext)?;
        } else if p.extension().is_some_and(|x| ext.iter().any(|e| x == *e)) {
            std::fs::copy(&p, &dst)?;
            n += 1;
        }
    }
    Ok(n)
}

/// What `psxw mod sdk` did.
pub struct SdkOutcome {
    pub plan: Plan,
    pub mods: Vec<Mod>,
    /// Targets a bump lost, each with what is at its old address now.
    pub moved: Vec<String>,
    pub lock: Lock,
}

/// `psxw mod sdk [--bump]`: export the mod SDK at psx-wasm's pins, check
/// every mod against it, and record it in `psxmods.lock.json`.
pub fn sdk(ws: &ModsWorkspace, bump: bool) -> Result<SdkOutcome> {
    let psx = ws.psx()?;
    let game = ws.game().to_string();
    let (decomp, identity) = pins(&psx, &game)?;
    let old = ws.lock()?;
    if let Some(l) = &old {
        if !bump && (l.decomp != decomp || l.identity != identity) {
            bail!(
                "{LOCK} is for {game} @ {}, psx-wasm now pins {}: psxw mod sdk --bump moves to it and checks every mod again",
                &l.decomp[..7.min(l.decomp.len())],
                &decomp[..7]
            );
        }
    }
    let (_, table) = crate::build::table(&psx, &game)?;
    let sdk = ws.sdk_dir();
    let _ = std::fs::remove_dir_all(&sdk);
    std::fs::create_dir_all(&sdk)?;
    // The decomp's headers at the pin.
    let manifest = psx.game(&game)?;
    let snap = crate::build::snapshot(&psx, &game)?;
    let mut includes = Vec::new();
    for inc in &manifest.c.include {
        let rel = Path::new("game").join(inc);
        let src = snap.join(inc);
        if src.is_dir() {
            copy_tree(&src, &sdk.join(&rel), &["h", "inc"])?;
        }
        includes.push(rel.to_string_lossy().into_owned());
    }
    std::fs::write(sdk.join("psxw_prelude.h"), psxw_cc::PRELUDE)?;
    std::fs::create_dir_all(sdk.join("include"))?;
    std::fs::write(sdk.join("include").join("psxmod.h"), genfiles::PSXMOD_H)?;
    let flags = SdkFlags {
        clang: psx.clang()?.display().to_string(),
        std: genfiles::STD.trim_start_matches("-std=").into(),
        prelude: "psxw_prelude.h".into(),
        includes,
        defines: manifest.c.defines.clone(),
        prelude_headers: manifest.probe.prelude.clone(),
    };
    std::fs::write(sdk.join("flags.json"), serde_json::to_string_pretty(&flags)?)?;
    std::fs::write(sdk.join("symbols.json"), serde_json::to_string(&table)?)?;
    std::fs::write(sdk.join("identity.json"), serde_json::to_string_pretty(&identity)?)?;
    let schemas = sdk.join("schema");
    std::fs::create_dir_all(&schemas)?;
    for (name, text) in crate::SCHEMAS {
        std::fs::write(schemas.join(name), text)?;
    }

    // Every mod against it.
    let mods = ws.all()?;
    let plan = plan::plan(&table, &mods);
    let mut moved = Vec::new();
    if let Some(l) = &old {
        for (mi, m) in mods.iter().enumerate() {
            for t in l.targets.get(m.id()).into_iter().flatten() {
                if plan.resolved.contains_key(&(mi, t.hook)) {
                    continue;
                }
                let addr = u32::from_str_radix(t.address.trim_start_matches("0x"), 16).unwrap_or(0);
                let now = table.function(&t.container, addr).map(|f| f.name().to_string());
                moved.push(match now {
                    Some(n) => format!("{}: {} no longer exists; {} 0x{addr:08X} is now {n}", m.id(), t.target, t.container),
                    None => format!("{}: {} no longer exists, and no function starts at its old address ({} 0x{addr:08X})", m.id(), t.target, t.container),
                });
            }
        }
    }
    let mut targets: BTreeMap<String, Vec<LockedTarget>> = BTreeMap::new();
    for ((mi, hi), (f, _)) in &plan.resolved {
        let h = &mods[*mi].decl.hooks[*hi];
        targets.entry(mods[*mi].id().to_string()).or_default().push(LockedTarget {
            hook: *hi,
            target: h.target.to_string(),
            container: f.container.clone(),
            address: format!("0x{:08X}", f.addr),
        });
    }
    // Keep what a mod with errors had locked, so the next bump still knows.
    if let Some(l) = &old {
        for (id, ts) in &l.targets {
            if !targets.contains_key(id) && mods.iter().any(|m| m.id() == id) {
                targets.insert(id.clone(), ts.clone());
            }
        }
    }
    let lock = Lock { game: game.clone(), psx_wasm: psx_commit(&psx), decomp, identity, targets };
    std::fs::write(ws.root.join(LOCK), serde_json::to_string_pretty(&lock)? + "\n")?;
    if plan.ok() {
        ws.generate(&table, &mods, &plan)?;
    }
    Ok(SdkOutcome { plan, mods, moved, lock })
}
