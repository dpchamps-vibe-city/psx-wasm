//! `psxw mod build`: plan the mods against the game, generate each mod's
//! `mod.h`, compile its C, and link everything into `game.wasm` through
//! psxw-build's mod plan (plans/M11 tasks 4–8).

use crate::compile::{self, Object, Surface};
use crate::decl::{DataValue, Mod};
use crate::genfiles::{self, Flags};
use crate::headers::{self, HeaderIndex};
use crate::plan::{self, LayerPlan, Plan};
use crate::symbols::{SymbolTable, TierName};
use anyhow::{Context as _, Result, bail, ensure};
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
pub fn flags(ws: &Workspace, game: &str, map: &psxw_inputs::AddressMap, work: &Path) -> Result<Flags> {
    let manifest = ws.game(game)?;
    let snap = snapshot(ws, game)?;
    std::fs::create_dir_all(work)?;
    let prelude = work.join("psxw_prelude.h");
    if std::fs::read_to_string(&prelude).ok().as_deref() != Some(psxw_cc::PRELUDE) {
        std::fs::write(&prelude, psxw_cc::PRELUDE)?;
    }
    let mut includes: Vec<PathBuf> = manifest.c.include.iter().map(|i| snap.join(i)).collect();
    // A header no include dir has is named from the snapshot root.
    let headers = psxw_inputs::headers::Headers::load(&snap, &manifest)?;
    if !headers.rooted_in(&headers.contexts(map)).is_empty() {
        includes.push(snap.clone());
    }
    Ok(Flags { clang: ws.clang()?, prelude, includes, defines: manifest.c.defines.clone() })
}

/// The header index at the pinned commit, cached under `out/modkit/`.
pub fn header_index(ws: &Workspace, prep: &Prepared) -> Result<HeaderIndex> {
    let game = &prep.inputs.manifest.game.id;
    let dir = ws.out_dir().join("modkit").join(game);
    let path = dir.join(format!("headers-v{}-{}.json", headers::INDEX_VERSION, &prep.commit[..7]));
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

/// The game's snapshots (D55, plans/M12), cached per decomp commit in
/// `out/modkit/<game>/`: the symbol-list frontend's, and the C-header
/// frontend's (the symbol list with types).
pub fn snapshots(ws: &Workspace, game: &str) -> Result<(crate::snapshot::Snapshot, crate::snapshot::Snapshot)> {
    let (prep, table) = table(ws, game)?;
    let (_, identity) = crate::workspace::pins(ws, game)?;
    let symbols = crate::snapshot::from_symbols(&table, identity);
    let dir = ws.out_dir().join("modkit").join(game);
    let path = dir.join(format!("snapshot-c-v{}-{}.json", crate::cfront::FRONTEND_VERSION, &prep.commit[..7]));
    if let Ok(s) = crate::snapshot::Snapshot::load(&path) {
        if s.game == symbols.game {
            return Ok((symbols, s));
        }
    }
    let snap = snapshot(ws, game)?;
    let manifest = &prep.inputs.manifest;
    let work = dir.join("cfront");
    let clang = psxw_cc::Clang::new(ws.clang()?, snap.clone(), manifest.c.include.clone(), manifest.c.defines.clone(), &work)?;
    let pin = ws.game_pin(manifest)?;
    let checkout = ws.checkout(pin);
    let tool = |p: &Path| if p.components().count() > 1 { checkout.join(p) } else { p.to_path_buf() };
    let reference = psxw_cc::probe::Reference {
        cpp: tool(&manifest.reference.cpp),
        cppflags: manifest.reference.cppflags.clone(),
        cc1: tool(&manifest.reference.cc1),
        cc1flags: manifest.reference.cc1flags.clone(),
    };
    let reference = reference.cc1.is_file().then_some(reference);
    eprintln!("snapshot: reading {}'s headers (C frontend)", game);
    let c = crate::cfront::snapshot(symbols.clone(), &clang, reference.as_ref(), &snap, manifest, &prep.inputs.map, &work)?;
    std::fs::create_dir_all(&dir)?;
    std::fs::write(&path, c.to_json())?;
    Ok((symbols, c))
}

/// The DWARF frontend's snapshot (plans/M12 phase 3): the C-header
/// frontend's `c` with the debug build's layouts (D53, built when needed).
pub fn dwarf_snapshot(ws: &Workspace, game: &str, c: &crate::snapshot::Snapshot) -> Result<crate::snapshot::Snapshot> {
    Ok(crate::dwarf::merge(c, &dwarf_view(ws, game)?))
}

/// What the debug build's DWARF says.
pub fn dwarf_view(ws: &Workspace, game: &str) -> Result<crate::dwarf::DwarfView> {
    let (r, _) = psxw_build::game::build(ws, &Options { debug: true, ..Options::new(game) })?;
    let bytes = std::fs::read(&r.wasm)?;
    crate::dwarf::read(&psxw_link::dwarf::sections(&bytes)?)
}

/// The game's snapshots at another commit of its decomp (plans/M12 phase
/// 3): psx-wasm's pins with that commit (its census objects must exist).
pub fn snapshots_at(ws: &Workspace, game: &str, commit: &str) -> Result<(crate::snapshot::Snapshot, crate::snapshot::Snapshot)> {
    let manifest = ws.game(game)?;
    let mut other = ws.clone();
    let pin = other.lock.game.get_mut(&manifest.game.input).with_context(|| format!("inputs.lock has no [game.{}]", manifest.game.input))?;
    pin.commit = commit.to_string();
    snapshots(&other, game)
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

/// Mods planned and compiled: the build's facts, the symbol table, the
/// plan, and each mod's objects (by its index in the mods given).
pub struct Compiled {
    pub prep: Prepared,
    pub table: SymbolTable,
    pub plan: Plan,
    pub objects: BTreeMap<usize, Vec<Object>>,
}

/// Plan `mods` (in the order the user gave) and compile them.
pub fn compile_mods(ws: &Workspace, game: &str, mods: &[Mod], layout: &Layout) -> Result<Compiled> {
    let game = game.to_string();
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
        None => flags(ws, &game, &prep.inputs.map, &layout.out)?,
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
        if crate::rust::is_rust(m) {
            compile::flat_header(&flags, m, &include, &obj_dir.join("flat"))?;
            crate::rust::generate(&flags, &plan, mods, mi, &include)?;
        }
        let objs = compile::compile(&flags, m, &include, &obj_dir)?;
        objects.insert(mi, objs);
    }
    check_c_kinds(&prep, &plan, mods, &objects)?;
    Ok(Compiled { prep, table, plan, objects })
}

fn mod_input(m: &Mod, o: &Object, main: &str) -> Input {
    Input {
        label: format!("mod {}/{}", m.id(), o.file),
        container: main.to_string(),
        obj: o.obj.clone(),
        protos: o.protos.clone(),
        generated: false,
        // Mods are ordinary C, in both backends: no GCC 2.95 conventions.
        gcc: None,
    }
}

/// The resident check of an overlay data patch: the overlay's first code
/// words.
fn resident(prep: &Prepared, container: &str) -> Result<Option<lm::Resident>> {
    if container == prep.inputs.map.main {
        return Ok(None);
    }
    let c = prep.inputs.map.container(container).context("container")?;
    let at = c.text.first().map(|r| r.start).with_context(|| format!("{container} has no code to recognize it by"))?;
    let w = |a: u32| prep.inputs.originals.word(container, a).unwrap_or(0);
    Ok(Some(lm::Resident { at, words: [w(at), w(at + 4)] }))
}

/// Plan, compile and link `mods` (in the order the user gave) into
/// `game.wasm`. `base` carries the build's other options.
pub fn build(ws: &Workspace, base: &Options, mods: &[Mod], layout: &Layout) -> Result<Built> {
    let Compiled { prep, table, plan, objects } = compile_mods(ws, &base.game, mods, layout)?;

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
            inputs.push(mod_input(m, o, &main));
        }
        position.insert(mi, spec.mods.len());
        spec.mods.push(lm::ModInputs { id: m.id().to_string(), inputs: ids });
    }
    let imp = |mi: usize, i: &crate::decl::Impl| -> Result<lm::Impl> {
        // A mod built by another toolchain is one module, whatever source
        // file declared each implementation.
        let file = match &mods[mi].decl.wasm {
            Some(w) => w.path.clone(),
            None => i.file.clone(),
        };
        let input = *input_of.get(&(mi, file)).with_context(|| format!("{}: no object for {}", mods[mi].id(), i.file))?;
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
        let resident = resident(&prep, &d.container)?;
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

/// One built mod module.
#[derive(Debug, Clone, Serialize)]
pub struct BuiltModule {
    pub id: String,
    pub path: PathBuf,
    pub bytes: usize,
    pub targets: usize,
}

/// `psxmod.decl`: what the loader needs of a mod module.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ModuleDecl {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    /// sha256 of the module without this section.
    pub hash: String,
    pub data_size: u32,
    pub stack_size: u32,
    pub functions: u32,
    /// Its global functions by number: function `k`'s o32 entry is at the
    /// synthetic address `code_base + 4k` it is given at load.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub function_names: BTreeMap<String, u32>,
    pub layout_hash: String,
    /// `hook.<k>`: each hooked function, by address, and its slot's type
    /// (`c <params>-><results>` or `o32`).
    pub targets: Vec<DeclTarget>,
    /// `psxmod_calls[k]`: each game function it calls through its entry.
    pub calls: Vec<DeclTarget>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct DeclTarget {
    pub name: String,
    pub container: String,
    pub address: String,
    pub flavour: String,
    /// The mod's layer replaces the function outright (a `patch` without
    /// `original`): the layers below it don't run. The browser loader
    /// warns about it beside script mods (plans/M12).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replaces: bool,
}

/// Stack each dynamically loaded mod gets.
pub const MODULE_STACK: u32 = 0x8000;

/// Plan and compile `mods`, then link each into its own module (phase 2):
/// `<out>/<id>.mod.wasm`.
pub fn build_modules(ws: &Workspace, game: &str, mods: &[Mod], layout: &Layout) -> Result<Vec<BuiltModule>> {
    let Compiled { prep, table, plan, objects } = compile_mods(ws, game, mods, layout)?;
    let main = prep.inputs.map.main.clone();
    let identity = psxw_build::game::identity_json(ws, &prep.inputs)?;
    let flavour = |c: &str, a: u32| -> (Option<psxw_link::object::FuncType>, String) {
        match prep.tiers.get(&(c.to_string(), a)) {
            Some(Tier::C { input, func }) => {
                let o = &prep.link_inputs[*input].obj;
                let t = o.types[o.funcs[*func as usize].ty as usize].clone();
                let text = format!("c {}", lm::sig_text(&t));
                (Some(t), text)
            }
            _ => (None, "o32".into()),
        }
    };
    let entries: BTreeMap<(String, u32), Option<psxw_link::object::FuncType>> = prep
        .tiers
        .keys()
        .filter(|k| !prep.expanded.contains(*k))
        .map(|(c, a)| ((c.clone(), *a), flavour(c, *a).0))
        .collect();
    std::fs::create_dir_all(&layout.out)?;
    let mut out = Vec::new();
    for &mi in &plan.order {
        let m = &mods[mi];
        let objs = &objects[&mi];
        let inputs: Vec<Input> = objs.iter().map(|o| mod_input(m, o, &main)).collect();
        let imp = |i: &crate::decl::Impl| -> Result<lm::Impl> {
            let file = match &m.decl.wasm {
                Some(w) => w.path.clone(),
                None => i.file.clone(),
            };
            let input = objs.iter().position(|o| o.file == file).with_context(|| format!("{}: no object for {}", m.id(), i.file))?;
            Ok(lm::Impl { input, function: i.function.clone() })
        };
        let mut targets = Vec::new();
        let mut decl_targets = Vec::new();
        for t in plan.targets.values() {
            if t.from.values().any(|(_, ls)| ls.iter().any(|l| l.module == mi)) {
                bail!("{}: a from-scoped hook on {} needs the static backend; a module can't tell callers apart", m.id(), t.name);
            }
            let Some(l) = t.default.iter().find(|l| l.module == mi) else { continue };
            let call = |h: &crate::plan::HookUse| -> Result<lm::Call> { Ok(lm::Call { imp: imp(&h.imp)?, bare: h.bare }) };
            let layer = lm::Layer {
                module: 0,
                befores: l.befores.iter().map(call).collect::<Result<_>>()?,
                patch: l.patch.as_ref().map(|p| -> Result<lm::Patch> { Ok(lm::Patch { imp: imp(&p.imp)?, original: p.original.clone() }) }).transpose()?,
                afters: l.afters.iter().map(call).collect::<Result<_>>()?,
            };
            let (sig, text) = flavour(&t.container, t.addr);
            ensure!(entries.contains_key(&(t.container.clone(), t.addr)), "{}: {} has no slot", m.id(), t.name);
            let replaces = l.patch.as_ref().is_some_and(|p| p.original.is_none());
            decl_targets.push(DeclTarget { name: t.name.clone(), container: t.container.clone(), address: format!("0x{:08X}", t.addr), flavour: text, replaces });
            targets.push(psxw_link::module::ModTarget { container: t.container.clone(), addr: t.addr, sig, returns: t.returns, layer });
        }
        let mut data = Vec::new();
        for d in plan.data.iter().filter(|d| d.module == mi) {
            let value = match &d.value {
                DataValue::Bytes(b) => lm::PatchValue::Bytes(b.clone()),
                DataValue::FnPtr(i) => lm::PatchValue::FnPtr(imp(i)?),
            };
            data.push(lm::DataPatch { container: d.container.clone(), addr: d.addr, value, resident: resident(&prep, &d.container)? });
        }
        let spec = psxw_link::module::ModuleSpec { id: m.id().to_string(), targets, data, entries: entries.clone(), stack_size: MODULE_STACK };
        let linked = psxw_link::module::link(&prep.inputs.map, &spec, &inputs).with_context(|| format!("linking {}", m.id()))?;
        psxw_build::game::validate(&linked.wasm).with_context(|| format!("{}'s module fails validation", m.id()))?;
        let hash = {
            use sha2::Digest;
            sha2::Sha256::digest(&linked.wasm).iter().map(|b| format!("{b:02x}")).collect::<String>()
        };
        let decl = ModuleDecl {
            schema_version: 1,
            id: m.id().to_string(),
            version: m.decl.version.clone(),
            hash,
            data_size: linked.data_size,
            stack_size: MODULE_STACK,
            functions: linked.functions,
            function_names: linked.function_names.clone(),
            layout_hash: linked.layout_hash.clone(),
            targets: decl_targets,
            calls: linked
                .calls
                .iter()
                .map(|c| DeclTarget {
                    name: table.display(&c.container, c.addr),
                    container: c.container.clone(),
                    address: format!("0x{:08X}", c.addr),
                    flavour: format!("c {}", lm::sig_text(&c.sig)),
                    replaces: false,
                })
                .collect(),
        };
        let mut bytes = linked.wasm;
        {
            use wasm_encoder::Section;
            wasm_encoder::CustomSection { name: psxw_link::module::names::DECL_SECTION.into(), data: serde_json::to_vec(&decl)?.into() }.append_to(&mut bytes);
            wasm_encoder::CustomSection { name: lm::GAME_SECTION.into(), data: identity.clone().into_bytes().into() }.append_to(&mut bytes);
        }
        let path = layout.out.join(format!("{}.mod.wasm", m.id()));
        std::fs::write(&path, &bytes)?;
        out.push(BuiltModule { id: m.id().to_string(), path, bytes: bytes.len(), targets: decl.targets.len() });
    }
    Ok(out)
}
