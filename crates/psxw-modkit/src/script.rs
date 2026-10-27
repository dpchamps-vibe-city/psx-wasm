//! Script mods (plans/M12, D54): a directory holding `mod.js`, an ES module
//! whose default export is `mod(factory)`, and its `tests/`. The browser
//! loader runs them (`web/script.js`); psxw runs no JS itself. `psxw mod
//! check` reads their definitions by running the factories in Node in the
//! browsers image, and their tests run in the browsers.

use crate::decl::Mod;
use anyhow::{Context as _, Result, bail};
use std::path::{Path, PathBuf};

pub const FILE: &str = "mod.js";

#[derive(Debug, Clone)]
pub struct ScriptMod {
    pub dir: PathBuf,
    /// Its directory's name, which its definition's `id` must equal.
    pub id: String,
    /// The snapshot its `game` comes from, when not the workspace's (or the
    /// bundle's): mods pinned to different snapshots load together (D55).
    pub snapshot: Option<PathBuf>,
}

impl ScriptMod {
    pub fn load(dir: &Path) -> Result<Self> {
        if !dir.join(FILE).is_file() {
            bail!("{} has no {FILE}", dir.display());
        }
        let id = dir.canonicalize()?.file_name().context("mod directory")?.to_string_lossy().into_owned();
        Ok(Self { dir: dir.to_path_buf(), id, snapshot: None })
    }
}

/// A mod of either kind.
#[derive(Debug, Clone)]
pub enum AnyMod {
    Compiled(Mod),
    Script(ScriptMod),
}

impl AnyMod {
    /// `mod.json` makes a compiled mod, `mod.js` a script mod.
    pub fn load(dir: &Path) -> Result<Self> {
        if dir.join(crate::decl::FILE).is_file() {
            return Ok(AnyMod::Compiled(Mod::load(dir)?));
        }
        if dir.join(FILE).is_file() {
            return Ok(AnyMod::Script(ScriptMod::load(dir)?));
        }
        bail!("{} is no mod: it has neither {} nor {FILE}", dir.display(), crate::decl::FILE)
    }

    pub fn id(&self) -> &str {
        match self {
            AnyMod::Compiled(m) => m.id(),
            AnyMod::Script(s) => &s.id,
        }
    }

    pub fn dir(&self) -> &Path {
        match self {
            AnyMod::Compiled(m) => &m.dir,
            AnyMod::Script(s) => &s.dir,
        }
    }

    pub fn is_script(&self) -> bool {
        matches!(self, AnyMod::Script(_))
    }
}

/// The compiled mods among `mods`, in order.
pub fn compiled(mods: &[AnyMod]) -> Vec<Mod> {
    mods.iter()
        .filter_map(|m| match m {
            AnyMod::Compiled(c) => Some(c.clone()),
            AnyMod::Script(_) => None,
        })
        .collect()
}

/// Whether `dir` holds a mod of either kind.
pub fn is_mod_dir(dir: &Path) -> bool {
    dir.join(crate::decl::FILE).is_file() || dir.join(FILE).is_file()
}

/// A script mod's definition as `web/test/check-mods.mjs` prints it: names
/// unresolved, each hook's handler kinds.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Definition {
    pub id: Option<String>,
    pub version: Option<String>,
    #[serde(default)]
    pub load_after: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<DefHook>,
    #[serde(default)]
    pub data: Vec<DefData>,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub init: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct DefHook {
    pub target: crate::decl::Target,
    pub kinds: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct DefData {
    pub target: crate::decl::Target,
    #[serde(default)]
    pub offset: u32,
    #[serde(rename = "type")]
    pub ty: crate::decl::DataType,
    pub value: serde_json::Value,
}

impl ScriptMod {
    /// Its definition as a mod the planner reads (`Mod::script`): `replace`
    /// is a patch, `around` a patch with `original`.
    pub fn as_mod(&self, def: &Definition, game: &str, main: &str) -> Result<Mod> {
        use crate::decl::{DataPatch, Hook, Impl, Kind, ModDecl, Target};
        let id = def.id.clone().unwrap_or_default();
        if id != self.id {
            bail!("{}: its definition's id is {id:?}; a script mod's id is its directory's name", self.dir.display());
        }
        let imp = |what: &str| Impl { file: FILE.into(), function: what.into() };
        let place = |t: &Target| -> Target {
            match (&t.container, &t.address) {
                (None, Some(_)) => Target { container: Some(main.to_string()), ..t.clone() },
                _ => t.clone(),
            }
        };
        let mut hooks = Vec::new();
        for (i, h) in def.hooks.iter().enumerate() {
            for k in &h.kinds {
                let (kind, original) = match k.as_str() {
                    "before" => (Kind::Before, None),
                    "after" => (Kind::After, None),
                    "replace" => (Kind::Patch, None),
                    "around" => (Kind::Patch, Some("original".to_string())),
                    other => bail!("{id}: hooks[{i}]: unknown handler {other}"),
                };
                hooks.push(Hook { kind, target: place(&h.target), from: None, imp: imp(&format!("hook{i}_{k}")), original, signature: None });
            }
        }
        let data = def
            .data
            .iter()
            .map(|d| DataPatch { target: place(&d.target), offset: d.offset, ty: d.ty, value: d.value.clone() })
            .collect();
        let decl = ModDecl {
            schema: None,
            id,
            version: def.version.clone().unwrap_or_else(|| "0.0.0".into()),
            game: game.to_string(),
            load_after: def.load_after.clone(),
            hooks,
            data,
            wasm: None,
        };
        let m = Mod { dir: self.dir.clone(), decl, script: true };
        m.validate()?;
        Ok(m)
    }
}

/// A packed script mod (`psxw mod pack`, plans/M12 phase 3): one file with
/// its `mod.js` and the part of its snapshot it uses, which any bundle of
/// the same binary loads with no SDK. A pack holds a single-module mod.
pub fn pack(m: &ScriptMod, snap: &crate::snapshot::Snapshot) -> Result<serde_json::Value> {
    let text = std::fs::read_to_string(m.dir.join(FILE))?;
    let others: Vec<String> = std::fs::read_dir(&m.dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "js" || x == "mjs") && p.file_name().is_some_and(|n| n != FILE))
        .map(|p| p.display().to_string())
        .collect();
    if !others.is_empty() || text.contains("from \"./") || text.contains("from './") || text.contains("import(\"./") {
        bail!("{}: a pack holds one module; this mod has more ({})", m.id, if others.is_empty() { "relative imports".into() } else { others.join(", ") });
    }
    Ok(serde_json::json!({ "psxpack": 1, "id": m.id, "mod": text, "snapshot": subset(snap, &text) }))
}

/// The part of a snapshot a mod's source names: `game.Name`,
/// `game.addr.Name`, `game.types.Name`, `game.at(…, "Name")`, and the
/// types they reach.
pub fn subset(snap: &crate::snapshot::Snapshot, source: &str) -> crate::snapshot::Snapshot {
    use crate::snapshot::{TypeDef, TypeExpr};
    use std::collections::BTreeSet;
    let ident = |s: &str| -> String { s.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$').collect() };
    let mut names: BTreeSet<String> = BTreeSet::new();
    let mut records: BTreeSet<String> = BTreeSet::new();
    for (i, _) in source.match_indices("game.") {
        let rest = &source[i + 5..];
        for (prefix, types) in [("addr.", false), ("types.", true), ("points.", false), ("", false)] {
            if let Some(r) = rest.strip_prefix(prefix) {
                let n = ident(r);
                if !n.is_empty() {
                    if types {
                        records.insert(n);
                    } else {
                        names.insert(n);
                    }
                    break;
                }
            }
        }
    }
    // String names in the source (`game.at("ovl_11", "Name")`).
    for part in source.split(['"', '\'']) {
        if !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            names.insert(part.to_string());
        }
    }
    let used = |n: &[String], d: &[String]| n.iter().chain(d).any(|x| names.contains(x));
    let mut s = snap.clone();
    s.functions.retain(|f| used(&f.names, &f.deprecated));
    s.globals.retain(|g| used(&g.names, &g.deprecated));
    for p in s.points.values() {
        for f in p.before.iter().chain(&p.after) {
            if let Some(x) = snap.functions.iter().find(|x| x.names.contains(f)) {
                if !s.functions.iter().any(|y| y.address == x.address && y.container == x.container) {
                    s.functions.push(x.clone());
                }
            }
        }
    }
    // The types they reach, and those named through `game.types`.
    let mut want: Vec<String> = snap
        .types
        .iter()
        .filter(|(id, _)| records.contains(id.trim_start_matches("struct ").trim_start_matches("union ")) || records.contains(*id))
        .map(|(id, _)| id.clone())
        .collect();
    let mut stack: Vec<TypeExpr> = s.globals.iter().filter_map(|g| g.ty.clone()).collect();
    for f in &s.functions {
        if let Some(sig) = &f.signature {
            stack.extend(sig.params.iter().map(|p| p.ty.clone()));
            stack.push(sig.result.clone());
        }
    }
    let mut keep: BTreeSet<String> = BTreeSet::new();
    loop {
        while let Some(t) = stack.pop() {
            match t {
                TypeExpr::Record { record: id } | TypeExpr::Named { named: id } => want.push(id),
                TypeExpr::Ptr { ptr } => stack.push(*ptr),
                TypeExpr::Array { array, .. } => stack.push(*array),
                TypeExpr::Fn { func } => {
                    stack.extend(func.params.iter().map(|p| p.ty.clone()));
                    stack.push(func.result.clone());
                }
                TypeExpr::Width(_) => {}
            }
        }
        let Some(id) = want.pop() else { break };
        if !keep.insert(id.clone()) {
            continue;
        }
        match snap.types.get(&id) {
            Some(TypeDef::Struct { fields, .. } | TypeDef::Union { fields, .. }) => stack.extend(fields.iter().map(|f| f.ty.clone())),
            Some(TypeDef::Typedef { ty }) => stack.push(ty.clone()),
            _ => {}
        }
    }
    s.types.retain(|id, _| keep.contains(id));
    s.dropped.clear();
    s.unchecked.clear();
    s
}
