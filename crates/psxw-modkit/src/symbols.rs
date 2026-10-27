//! The game as mods see it: every function with its names, tier, callers
//! and declaration, every named global, each container's ranges, and the
//! manifest's named hook points. Built from a build's facts in psx-wasm;
//! exported into a mods workspace as the SDK's `symbols.json`. It holds
//! names and addresses only, no game bytes.

use crate::headers::{Decl, HeaderIndex};
use anyhow::{Context as _, Result, bail};
use psxw_inputs::game::PointSpec;
use psxw_link::game::Tier;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolTable {
    pub schema_version: u32,
    pub game: String,
    pub main: String,
    /// The decomp commit the names come from.
    pub decomp_commit: String,
    pub containers: Vec<ContainerInfo>,
    pub functions: Vec<FuncInfo>,
    pub data: Vec<DataInfo>,
    pub points: BTreeMap<String, PointSpec>,
    pub headers: HeaderIndex,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerInfo {
    pub id: String,
    pub main: bool,
    /// What the disc loads: `[start, end)`.
    pub loaded: [u32; 2],
    pub text: Vec<[u32; 2]>,
    /// The executable's zeroed range (its header's), `[start, end)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bss: Option<[u32; 2]>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TierName {
    C,
    Lift,
    Adapter,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuncInfo {
    pub container: String,
    pub addr: u32,
    /// Its names in the symbol files (the first is the one shown), or its
    /// splat auto-name.
    pub names: Vec<String>,
    pub tier: TierName,
    /// The translator expands it into translated callers: those calls
    /// aren't calls, so it can't be hooked.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expanded: bool,
    /// The decomp source defining it (C tier).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Functions calling it directly: (container, address).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub callers: Vec<(String, u32)>,
}

impl FuncInfo {
    pub fn name(&self) -> &str {
        &self.names[0]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataInfo {
    pub name: String,
    pub container: String,
    pub addr: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
}

/// splat's name for an unnamed function: `func_ADDR` in the executable,
/// `<container>_func_ADDR` in an overlay.
pub fn auto_name(main: &str, container: &str, addr: u32) -> String {
    if container == main { format!("func_{addr:08X}") } else { format!("{container}_func_{addr:08X}") }
}

impl SymbolTable {
    /// From a build's facts and the header index.
    pub fn from_build(prep: &psxw_build::game::Prepared, headers: HeaderIndex) -> Result<Self> {
        let map = &prep.inputs.map;
        let main = map.main.clone();
        let mut containers = Vec::new();
        for c in &map.containers {
            let bss = c.is_main.then(|| [prep.inputs.exe.b_addr, prep.inputs.exe.b_addr + prep.inputs.exe.b_size]).filter(|b| b[1] > b[0]);
            containers.push(ContainerInfo {
                id: c.id.clone(),
                main: c.is_main,
                loaded: [c.vram.start, c.vram.end],
                text: c.text.iter().map(|r| [r.start, r.end]).collect(),
                bss,
            });
        }
        // Names by address, per container.
        let mut names: BTreeMap<(String, u32), Vec<String>> = BTreeMap::new();
        for c in &map.containers {
            for e in map.symbols(&c.id) {
                names.entry((c.id.clone(), e.addr)).or_default().push(e.name.clone());
            }
        }
        let funcs: BTreeSet<(String, u32)> = prep.tiers.keys().cloned().collect();
        // Direct callers, from every function's translation.
        let mut callers: BTreeMap<(String, u32), BTreeSet<(String, u32)>> = BTreeMap::new();
        for t in &prep.translations {
            for &(_, target, _) in &t.facts.call_sites {
                let home = [t.func.container.as_str(), main.as_str()]
                    .into_iter()
                    .find(|c| funcs.contains(&(c.to_string(), target)))
                    .map(str::to_string)
                    .or_else(|| funcs.iter().find(|(_, a)| *a == target).map(|(c, _)| c.clone()));
                if let Some(h) = home {
                    callers.entry((h, target)).or_default().insert((t.func.container.clone(), t.func.addr));
                }
            }
        }
        let source = |c: &str, a: u32| -> Option<String> {
            let (_, stem) = prep.c_objects.get(&(c.to_string(), a))?;
            let k = map.container(c)?;
            k.tus.iter().find(|tu| tu.name.replace('/', "__") == *stem).map(|tu| tu.path.to_string_lossy().into_owned())
        };
        let mut functions = Vec::new();
        for ((c, a), tier) in &prep.tiers {
            let mut n = names.get(&(c.clone(), *a)).cloned().unwrap_or_default();
            if let Some(t) = prep.translations.iter().find(|t| t.func.container == *c && t.func.addr == *a) {
                if !n.contains(&t.func.name) {
                    n.push(t.func.name.clone());
                }
            }
            if n.is_empty() {
                n.push(auto_name(&main, c, *a));
            }
            functions.push(FuncInfo {
                container: c.clone(),
                addr: *a,
                names: n,
                tier: match tier {
                    Tier::C { .. } => TierName::C,
                    Tier::Lift { .. } => TierName::Lift,
                    Tier::Adapter(_) => TierName::Adapter,
                },
                expanded: prep.expanded.contains(&(c.clone(), *a)),
                source: source(c, *a),
                callers: callers.get(&(c.clone(), *a)).map(|s| s.iter().cloned().collect()).unwrap_or_default(),
            });
        }
        let mut data = Vec::new();
        for c in &map.containers {
            for e in map.symbols(&c.id) {
                if e.is_func() || funcs.contains(&(c.id.clone(), e.addr)) {
                    continue;
                }
                data.push(DataInfo { name: e.name.clone(), container: c.id.clone(), addr: e.addr, size: e.size });
            }
        }
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            game: prep.inputs.manifest.game.id.clone(),
            main,
            decomp_commit: prep.commit.clone(),
            containers,
            functions,
            data,
            points: prep.inputs.manifest.mods.points.clone(),
            headers,
        })
    }

    pub fn load(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let t: Self = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        if t.schema_version != SCHEMA_VERSION {
            bail!("{} is schema {}, this psxw reads {SCHEMA_VERSION}: run psxw mod sdk", path.display(), t.schema_version);
        }
        Ok(t)
    }

    pub fn container(&self, id: &str) -> Option<&ContainerInfo> {
        self.containers.iter().find(|c| c.id == id)
    }

    pub fn function(&self, container: &str, addr: u32) -> Option<&FuncInfo> {
        self.functions.iter().find(|f| f.container == container && f.addr == addr)
    }

    /// The header declaration of a function, by any of its names.
    pub fn decl(&self, f: &FuncInfo) -> Option<&Decl> {
        f.names.iter().find_map(|n| self.headers.functions.get(n))
    }

    /// A function's name shown with its container when that's ambiguous.
    pub fn display(&self, container: &str, addr: u32) -> String {
        match self.function(container, addr) {
            Some(f) if self.functions.iter().filter(|g| g.names.contains(&f.names[0])).count() > 1 => {
                format!("{container}:{}", f.name())
            }
            Some(f) => f.name().to_string(),
            None => format!("{container}:0x{addr:08X}"),
        }
    }

    /// Text ranges of the container holding `addr`, seen from `container`.
    pub fn in_text(&self, container: &str, addr: u32) -> bool {
        self.container(container).is_some_and(|c| c.text.iter().any(|r| (r[0]..r[1]).contains(&addr)))
    }
}
