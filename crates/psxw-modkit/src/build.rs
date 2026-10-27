//! `psxw mod build`: plan the mods against the game, generate each mod's
//! `mod.h`, compile its C, and link everything into `game.wasm` through
//! psxw-build's mod plan (plans/M11 tasks 4–8).

use crate::compile::{self, Object, Surface};
use crate::decl::{DataValue, Mod};
use crate::genfiles::{self, Flags};
use crate::headers::{self, HeaderIndex};
use crate::plan::{self, LayerPlan, Plan};
use crate::symbols::{SymbolTable, TierName};
use anyhow::{Context as _, Result, bail};
use psxw_build::game::{ModPlan, Options, Prepared, Report};
use psxw_inputs::Workspace;
use psxw_link::Input;
use psxw_link::game::Tier;
use psxw_link::mods::{self as lm, ModSpec};
use psxw_link::object::{NameClass, classify_name};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const MODS_JSON_VERSION: u32 = 1;

/// The game's snapshot directory.
pub fn snapshot(ws: &Workspace, game: &str) -> Result<PathBuf> {
    let manifest = ws.game(game)?;
    let pin = ws.game_pin(&manifest)?.clone();
    psxw_inputs::snapshot::ensure(ws, &manifest.game.input, &pin)
}

/// How the game's C compiles, for mod sources.
pub fn flags(ws: &Workspace, game: &str, work: &Path) -> Result<Flags> {
    let manifest = ws.game(game)?;
    let snap = snapshot(ws, game)?;
    std::fs::create_dir_all(work)?;
    let prelude = work.join("psxw_prelude.h");
    if std::fs::read_to_string(&prelude).ok().as_deref() != Some(psxw_cc::PRELUDE) {
        std::fs::write(&prelude, psxw_cc::PRELUDE)?;
    }
    Ok(Flags {
        clang: ws.clang()?,
        prelude,
        includes: manifest.c.include.iter().map(|i| snap.join(i)).collect(),
        defines: manifest.c.defines.clone(),
    })
}

/// The header index at the pinned commit, cached under `out/modkit/`.
pub fn header_index(ws: &Workspace, prep: &Prepared) -> Result<HeaderIndex> {
    let game = &prep.inputs.manifest.game.id;
    let dir = ws.out_dir().join("modkit").join(game);
    let path = dir.join(format!("headers-{}.json", &prep.commit[..7]));
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(idx) = serde_json::from_str(&text) {
            return Ok(idx);
        }
    }
    let snap = snapshot(ws, game)?;
    let manifest = &prep.inputs.manifest;
    let work = dir.join("headers");
    let clang = psxw_cc::Clang::new(ws.clang()?, snap.clone(), manifest.c.include.clone(), manifest.c.defines.clone(), &work)?;
    let idx = headers::index(&clang, &snap, manifest, &prep.inputs.map, &work)?;
    std::fs::create_dir_all(&dir)?;
    std::fs::write(&path, serde_json::to_string(&idx)?)?;
    Ok(idx)
}

/// The game as mods see it, from a fresh build preparation.
pub fn table(ws: &Workspace, game: &str) -> Result<(Prepared, SymbolTable)> {
    let prep = psxw_build::game::prepare(ws, &Options::new(game))?;
    let headers = header_index(ws, &prep)?;
    let t = SymbolTable::from_build(&prep, headers)?;
    Ok((prep, t))
}

/// Where a build puts things.
pub struct Layout {
    /// game.wasm, guest.wasm, mods.json, build.json.
    pub out: PathBuf,
    /// Each mod's `mod.h` directory.
    pub include: Box<dyn Fn(&str) -> PathBuf>,
    /// How mod sources compile (default: from the game's snapshot).
    pub flags: Option<Flags>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModEntry {
    pub id: String,
    pub version: String,
    pub dir: String,
    pub hooks: usize,
    pub data: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChainLink {
    #[serde(rename = "mod")]
    pub module: String,
    pub kinds: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChainEntry {
    pub target: String,
    pub container: String,
    pub address: String,
    pub tier: TierName,
    /// Outermost first.
    pub default: Vec<ChainLink>,
    /// By caller name, outermost first; they wrap the default chain.
    pub from: BTreeMap<String, Vec<ChainLink>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DataEntry {
    #[serde(rename = "mod")]
    pub module: String,
    pub target: String,
    pub container: String,
    pub address: String,
    pub size: u32,
}

/// `mods.json`: what the mods do to the game.
#[derive(Debug, Clone, Serialize)]
pub struct ModsReport {
    pub schema_version: u32,
    pub game: String,
    /// In load order.
    pub mods: Vec<ModEntry>,
    pub chains: Vec<ChainEntry>,
    pub data: Vec<DataEntry>,
    pub warnings: Vec<String>,
    pub surfaces: BTreeMap<String, Surface>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wasm: Option<String>,
}

pub fn report(table: &SymbolTable, plan: &Plan, mods: &[Mod]) -> ModsReport {
    let id = |m: usize| mods[m].id().to_string();
    let links = |ls: &[LayerPlan]| ls.iter().rev().map(|l| ChainLink { module: id(l.module), kinds: l.kinds() }).collect();
    ModsReport {
        schema_version: MODS_JSON_VERSION,
        game: table.game.clone(),
        mods: plan
            .order
            .iter()
            .map(|&i| ModEntry {
                id: id(i),
                version: mods[i].decl.version.clone(),
                dir: mods[i].dir.display().to_string(),
                hooks: mods[i].decl.hooks.len(),
                data: mods[i].decl.data.len(),
            })
            .collect(),
        chains: plan
            .targets
            .values()
            .map(|t| ChainEntry {
                target: t.name.clone(),
                container: t.container.clone(),
                address: format!("0x{:08X}", t.addr),
                tier: t.tier,
                default: links(&t.default),
                from: t.from.values().map(|(n, ls)| (n.clone(), links(ls))).collect(),
            })
            .collect(),
        data: plan
            .data
            .iter()
            .map(|d| DataEntry { module: id(d.module), target: d.what.clone(), container: d.container.clone(), address: format!("0x{:08X}", d.addr), size: d.size })
            .collect(),
        warnings: plan.warnings.clone(),
        surfaces: BTreeMap::new(),
        wasm: None,
    }
}

/// A built mod set.
pub struct Built {
    pub report: Report,
    pub plan: Plan,
    pub mods: ModsReport,
    pub mods_json: PathBuf,
}

/// The C name a census object defines a function under.
fn c_name(prep: &Prepared, c: &str, a: u32) -> Option<String> {
    let Some(Tier::C { input, func }) = prep.tiers.get(&(c.to_string(), a)) else { return None };
    let o = &prep.link_inputs[*input].obj;
    let s = o.func_symbol(func + o.num_imported_funcs())?;
    match classify_name(&s.name) {
        NameClass::Guest(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Plan, compile and link `mods` (in the order the user gave) into
/// `game.wasm`. `base` carries the build's other options.
pub fn build(ws: &Workspace, base: &Options, mods: &[Mod], layout: &Layout) -> Result<Built> {
    let game = base.game.clone();
    let (prep, table) = table(ws, &game)?;
    let plan = plan::plan(&table, mods);
    for w in &plan.warnings {
        eprintln!("mods: warning: {w}");
    }
    if !plan.ok() {
        bail!("mods have errors:\n  {}", plan.errors.join("\n  "));
    }
    let flags = match &layout.flags {
        Some(f) => f.clone(),
        None => flags(ws, &game, &layout.out)?,
    };
    let prelude = prep.inputs.manifest.probe.prelude.clone();

    // Each mod's mod.h and objects, in load order.
    let mut objects: BTreeMap<usize, Vec<Object>> = BTreeMap::new();
    for &mi in &plan.order {
        let m = &mods[mi];
        let include = (layout.include)(m.id());
        std::fs::create_dir_all(&include)?;
        write_if_changed(&include.join("mod.h"), &genfiles::mod_h(&plan, mods, mi, &prelude))?;
        write_if_changed(&include.join("psxmod.h"), genfiles::PSXMOD_H)?;
        let (_, obj_dir) = compile::mod_dirs(&layout.out, m.id());
        let objs = compile::compile(&flags, m, &include, &obj_dir)?;
        objects.insert(mi, objs);
    }
    check_c_kinds(&prep, &plan, mods, &objects)?;

    // The link's view: inputs, chains, data patches.
    let main = prep.inputs.map.main.clone();
    let mut inputs: Vec<Input> = Vec::new();
    let mut input_of: BTreeMap<(usize, String), usize> = BTreeMap::new();
    let mut spec = ModSpec::default();
    let mut position: BTreeMap<usize, usize> = BTreeMap::new();
    for &mi in &plan.order {
        let m = &mods[mi];
        let mut ids = Vec::new();
        for o in &objects[&mi] {
            input_of.insert((mi, o.file.clone()), inputs.len());
            ids.push(inputs.len());
            inputs.push(Input {
                label: format!("mod {}/{}", m.id(), o.file),
                container: main.clone(),
                obj: o.obj.clone(),
                protos: o.protos.clone(),
                generated: false,
                gcc: Some(prep.gcc),
            });
        }
        position.insert(mi, spec.mods.len());
        spec.mods.push(lm::ModInputs { id: m.id().to_string(), inputs: ids });
    }
    let imp = |mi: usize, i: &crate::decl::Impl| -> Result<lm::Impl> {
        let input = *input_of.get(&(mi, i.file.clone())).with_context(|| format!("{}: no object for {}", mods[mi].id(), i.file))?;
        Ok(lm::Impl { input, function: i.function.clone() })
    };
    let layer = |l: &LayerPlan| -> Result<lm::Layer> {
        let call = |h: &crate::plan::HookUse| -> Result<lm::Call> { Ok(lm::Call { imp: imp(l.module, &h.imp)?, bare: h.bare }) };
        Ok(lm::Layer {
            module: position[&l.module],
            befores: l.befores.iter().map(call).collect::<Result<_>>()?,
            patch: l.patch.as_ref().map(|p| -> Result<lm::Patch> { Ok(lm::Patch { imp: imp(l.module, &p.imp)?, original: p.original.clone() }) }).transpose()?,
            afters: l.afters.iter().map(call).collect::<Result<_>>()?,
        })
    };
    // Every C function is already weak and noinline in every build (D50),
    // so hooking one needs no recompile.
    for t in plan.targets.values() {
        let mut chains = lm::Chains { returns: t.returns, default: t.default.iter().map(&layer).collect::<Result<_>>()?, from: BTreeMap::new() };
        for ((cc, ca), (_, ls)) in &t.from {
            chains.from.insert((cc.clone(), *ca), ls.iter().map(&layer).collect::<Result<_>>()?);
        }
        spec.targets.insert((t.container.clone(), t.addr), chains);
    }
    for d in &plan.data {
        let value = match &d.value {
            DataValue::Bytes(b) => lm::PatchValue::Bytes(b.clone()),
            DataValue::FnPtr(i) => lm::PatchValue::FnPtr(imp(d.module, i)?),
        };
        let resident = if d.container == main {
            None
        } else {
            let c = prep.inputs.map.container(&d.container).context("container")?;
            let at = c.text.first().map(|r| r.start).with_context(|| format!("{} has no code to recognize it by", d.container))?;
            let w = |a: u32| prep.inputs.originals.word(&d.container, a).unwrap_or(0);
            Some(lm::Resident { at, words: [w(at), w(at + 4)] })
        };
        spec.data.push(lm::DataPatch { container: d.container.clone(), addr: d.addr, value, resident });
    }

    // The game, linked with the mods.
    let opts = Options { mods: Some(ModPlan { out: layout.out.clone(), inputs, spec }), ..base.clone() };
    let (report_, _) = psxw_build::game::finish(ws, &opts, prep)?;

    let mut mr = report(&table, &plan, mods);
    let own: BTreeSet<String> = mods.iter().flat_map(|m| m.decl.hooks.iter().filter_map(|h| h.original.clone())).collect();
    for &mi in &plan.order {
        mr.surfaces.insert(mods[mi].id().to_string(), compile::surface(&objects[&mi], &own)?);
    }
    // Two mods writing the same global.
    let mut writers: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (id, s) in &mr.surfaces {
        for g in &s.writes {
            writers.entry(g.as_str()).or_default().push(id.as_str());
        }
    }
    let shared: Vec<String> = writers.iter().filter(|(_, w)| w.len() > 1).map(|(g, w)| format!("{} all write {g}", w.join(", "))).collect();
    for w in &shared {
        eprintln!("mods: warning: {w}");
    }
    mr.warnings.extend(shared);
    mr.wasm = Some(report_.wasm.clone());
    let mods_json = layout.out.join("mods.json");
    std::fs::write(&mods_json, serde_json::to_string_pretty(&mr)?)?;
    Ok(Built { report: report_, plan, mods: mr, mods_json })
}

/// For a decompiled-C target, each patch's C-level parameter kinds must
/// pass the way the function's own do (structs by value, variadics): the
/// linker's existing comparison.
fn check_c_kinds(prep: &Prepared, plan: &Plan, mods: &[Mod], objects: &BTreeMap<usize, Vec<Object>>) -> Result<()> {
    let mut errors = Vec::new();
    for t in plan.targets.values().filter(|t| t.tier == TierName::C) {
        let Some(Tier::C { input, .. }) = prep.tiers.get(&(t.container.clone(), t.addr)) else { continue };
        let Some(name) = c_name(prep, &t.container, t.addr) else { continue };
        let Some(defined) = prep.link_inputs[*input].protos.get(&name) else { continue };
        for l in t.default.iter().chain(t.from.values().flat_map(|(_, ls)| ls)) {
            let Some(p) = &l.patch else { continue };
            let Some(o) = objects[&l.module].iter().find(|o| o.file == p.imp.file) else { continue };
            let Some(mine) = o.protos.get(&p.imp.function) else { continue };
            if psxw_link::game::abi_differs(mine, defined) || mine.ret != defined.ret {
                errors.push(format!(
                    "{}: {} passes its arguments as {:?} -> {:?}, but {} takes {:?} -> {:?}",
                    mods[l.module].id(),
                    p.imp.function,
                    mine.params,
                    mine.ret,
                    t.name,
                    defined.params,
                    defined.ret
                ));
            }
        }
    }
    if !errors.is_empty() {
        bail!("{}", errors.join("\n"));
    }
    Ok(())
}

pub fn write_if_changed(path: &Path, text: &str) -> Result<()> {
    if std::fs::read_to_string(path).ok().as_deref() != Some(text) {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(path, text)?;
    }
    Ok(())
}
