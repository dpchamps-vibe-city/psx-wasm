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
}

impl ScriptMod {
    pub fn load(dir: &Path) -> Result<Self> {
        if !dir.join(FILE).is_file() {
            bail!("{} has no {FILE}", dir.display());
        }
        let id = dir.canonicalize()?.file_name().context("mod directory")?.to_string_lossy().into_owned();
        Ok(Self { dir: dir.to_path_buf(), id })
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
