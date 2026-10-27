//! Snapshots (D55, plans/M12): one source's view of one game binary, as
//! neutral JSON (`schema/snapshot.schema.json`). A snapshot names the
//! binary by its identity hashes, and records functions and globals by
//! container and address, with names, signatures and types, record
//! layouts, and type expressions as data. Headers and a decomp's
//! conventions appear only as provenance.
//!
//! Frontends produce snapshots; only a frontend knows its source:
//! - [`from_symbols`]: names, addresses and sizes from the address map,
//!   which every game has;
//! - [`crate::cfront`]: declarations and layouts from the C headers;
//! - [`crate::dwarf`]: types and layouts as compiled.
//!
//! Script mods' `game` and `game.d.ts` are generated from snapshots, never
//! from a source directly.

use crate::symbols::SymbolTable;
use crate::workspace::Identity;
use anyhow::{Context as _, Result, bail};
use psxw_inputs::game::PointSpec;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use std::collections::BTreeMap;
use std::path::Path;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub schema_version: u32,
    /// The binary: the identity hashes (`identity.json`).
    pub game: Identity,
    pub source: Source,
    /// The executable's container.
    pub main: String,
    pub containers: Vec<Container>,
    pub functions: Vec<Function>,
    pub globals: Vec<Global>,
    /// Records, enums and typedefs, by id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub types: BTreeMap<String, TypeDef>,
    /// The manifest's named hook points.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub points: BTreeMap<String, PointSpec>,
    /// Fields and globals a frontend dropped, each with why (layouts the
    /// compilers disagree on, say).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped: Vec<String>,
    /// What a frontend kept with less evidence than usual, each with why
    /// (layouts only one compiler gave).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unchecked: Vec<String>,
}

/// Where a snapshot's view comes from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// `symbols`, `c`, `dwarf`, `local`, or `merged`.
    pub kind: String,
    /// The source's version: a decomp commit, a file's hash.
    pub version: String,
    /// Which source, when a workspace names several (`psxmods.json`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A merged snapshot's sources, in order of precedence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged: Vec<Source>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Container {
    pub id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub main: bool,
    /// What the disc loads: `[start, end)`.
    pub loaded: [u32; 2],
    pub text: Vec<[u32; 2]>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Function {
    pub container: String,
    pub address: u32,
    /// The first is the name shown; the rest are aliases.
    pub names: Vec<String>,
    /// Old names (`--bump`) and other sources' names: deprecated aliases
    /// of the first of `names`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deprecated: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<Signature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Global {
    pub container: String,
    pub address: u32,
    pub names: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deprecated: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub ty: Option<TypeExpr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

/// Where a declaration came from: a header and its text, a compilation
/// unit. Never read for meaning.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// A type, as data: a width (`u8` … `s64`, `f32`, `f64`, `void`), or one of
/// the forms below.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum TypeExpr {
    Width(String),
    Ptr {
        ptr: Box<TypeExpr>,
    },
    Array {
        array: Box<TypeExpr>,
        count: u32,
    },
    Record {
        record: String,
    },
    /// An enum or typedef by id.
    Named {
        named: String,
    },
    Fn {
        #[serde(rename = "fn")]
        func: Box<Signature>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Signature {
    pub params: Vec<Param>,
    pub result: TypeExpr,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub variadic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Param {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub ty: TypeExpr,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TypeDef {
    Struct { size: u32, align: u32, fields: Vec<Field>, #[serde(default, skip_serializing_if = "Option::is_none")] provenance: Option<Provenance> },
    Union { size: u32, align: u32, fields: Vec<Field>, #[serde(default, skip_serializing_if = "Option::is_none")] provenance: Option<Provenance> },
    Enum { size: u32, values: BTreeMap<String, i64> },
    Typedef { #[serde(rename = "type")] ty: TypeExpr },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub name: String,
    pub offset: u32,
    #[serde(rename = "type")]
    pub ty: TypeExpr,
    /// A bitfield: its bit offset in the word at `offset`, and width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bits: Option<[u32; 2]>,
}

impl Snapshot {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let s: Self = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        if s.schema_version != SCHEMA_VERSION {
            bail!("{} is snapshot schema {}, this psxw reads {SCHEMA_VERSION}", path.display(), s.schema_version);
        }
        Ok(s)
    }

    /// Canonical JSON: what the hash names.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("snapshot serializes")
    }

    /// The snapshot's name: the sha256 of its canonical JSON, 16 hex digits.
    pub fn hash(&self) -> String {
        let d = sha2::Sha256::digest(self.to_json().as_bytes());
        d.iter().take(8).map(|b| format!("{b:02x}")).collect()
    }

    pub fn function(&self, container: &str, address: u32) -> Option<&Function> {
        self.functions.iter().find(|f| f.container == container && f.address == address)
    }

    pub fn global(&self, container: &str, address: u32) -> Option<&Global> {
        self.globals.iter().find(|g| g.container == container && g.address == address)
    }
}

/// The symbol-list frontend: names, addresses and sizes from the address
/// map (through the build's symbol table), and the manifest's points.
pub fn from_symbols(table: &SymbolTable, game: Identity) -> Snapshot {
    let containers = table
        .containers
        .iter()
        .map(|c| Container { id: c.id.clone(), main: c.main, loaded: c.loaded, text: c.text.clone() })
        .collect();
    let functions = table
        .functions
        .iter()
        .map(|f| Function {
            container: f.container.clone(),
            address: f.addr,
            names: f.names.clone(),
            deprecated: vec![],
            signature: None,
            provenance: None,
        })
        .collect();
    let mut globals: Vec<Global> = Vec::new();
    for d in &table.data {
        match globals.iter_mut().find(|g| g.container == d.container && g.address == d.addr) {
            Some(g) => {
                if !g.names.contains(&d.name) {
                    g.names.push(d.name.clone());
                }
                g.size = g.size.or(d.size);
            }
            None => globals.push(Global {
                container: d.container.clone(),
                address: d.addr,
                names: vec![d.name.clone()],
                deprecated: vec![],
                size: d.size,
                ty: None,
                provenance: None,
            }),
        }
    }
    Snapshot {
        schema: None,
        schema_version: SCHEMA_VERSION,
        game,
        source: Source { kind: "symbols".into(), version: table.decomp_commit.clone(), name: None, merged: vec![] },
        main: table.main.clone(),
        containers,
        functions,
        globals,
        types: BTreeMap::new(),
        points: table.points.clone(),
        dropped: vec![],
        unchecked: vec![],
    }
}

/// What `psxw mod sdk --bump` found comparing two snapshots by address.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Bump {
    /// (old name, new name, container, address): the old name stays an
    /// alias, deprecated.
    pub renames: Vec<(String, String, String, u32)>,
    /// A record whose fields moved or changed type, at each anchor that
    /// reaches it.
    pub layouts: Vec<String>,
    /// A name whose address no longer holds a function or global, naming
    /// what is there now. A mod that uses one fails its check (M11's rule).
    pub removals: Vec<String>,
}

/// Compare `old` with `new` by address; `new` keeps every renamed name of
/// `old` (and its aliases) as a deprecated alias.
pub fn bump(old: &Snapshot, new: &mut Snapshot) -> Bump {
    let mut b = Bump::default();
    let describe = |s: &Snapshot, container: &str, address: u32| -> String {
        if let Some(f) = s.function(container, address) {
            return format!("function {}", f.names[0]);
        }
        if let Some(g) = s.global(container, address) {
            return format!("global {}", g.names[0]);
        }
        "nothing".into()
    };
    let new_view = new.clone();
    for f in &old.functions {
        let names: Vec<&String> = f.names.iter().chain(&f.deprecated).collect();
        match new.functions.iter_mut().find(|g| g.container == f.container && g.address == f.address) {
            Some(g) => {
                for n in names {
                    if !g.names.contains(n) && !g.deprecated.contains(n) {
                        if f.names.contains(n) {
                            b.renames.push((n.clone(), g.names[0].clone(), f.container.clone(), f.address));
                        }
                        g.deprecated.push(n.clone());
                    }
                }
            }
            None => b.removals.push(format!(
                "function {}: no function starts at {} 0x{:08X} any more; there is {} there now",
                f.names[0],
                f.container,
                f.address,
                describe(&new_view, &f.container, f.address)
            )),
        }
    }
    for g in &old.globals {
        let names: Vec<&String> = g.names.iter().chain(&g.deprecated).collect();
        match new.globals.iter_mut().find(|h| h.container == g.container && h.address == g.address) {
            Some(h) => {
                for n in names {
                    if !h.names.contains(n) && !h.deprecated.contains(n) {
                        if g.names.contains(n) {
                            b.renames.push((n.clone(), h.names[0].clone(), g.container.clone(), g.address));
                        }
                        h.deprecated.push(n.clone());
                    }
                }
            }
            None => b.removals.push(format!(
                "global {}: nothing is named at {} 0x{:08X} any more; there is {} there now",
                g.names[0],
                g.container,
                g.address,
                describe(&new_view, &g.container, g.address)
            )),
        }
    }
    // Records whose fields moved or changed type.
    let fields = |d: &TypeDef| -> Option<Vec<(String, u32, TypeExpr)>> {
        match d {
            TypeDef::Struct { fields, .. } | TypeDef::Union { fields, .. } => Some(fields.iter().map(|f| (f.name.clone(), f.offset, f.ty.clone())).collect()),
            _ => None,
        }
    };
    let mut changed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (id, d) in &old.types {
        let (Some(a), Some(n)) = (fields(d), new.types.get(id).and_then(fields)) else { continue };
        let mut what = Vec::new();
        for (name, off, ty) in &a {
            match n.iter().find(|x| x.0 == *name) {
                Some((_, o, _)) if o != off => what.push(format!("{name} moved +0x{off:X} -> +0x{o:X}")),
                Some((_, _, t2)) if t2 != ty => what.push(format!("{name} changed type")),
                Some(_) => {}
                None => what.push(format!("{name} is gone")),
            }
        }
        if !what.is_empty() {
            changed.insert(id.clone(), what);
        }
    }
    if !changed.is_empty() {
        let reaches = |t: &TypeExpr| -> Vec<String> {
            let mut out = Vec::new();
            let mut stack = vec![t.clone()];
            while let Some(t) = stack.pop() {
                match t {
                    TypeExpr::Record { record } if changed.contains_key(&record) => out.push(record),
                    TypeExpr::Ptr { ptr } => stack.push(*ptr),
                    TypeExpr::Array { array, .. } => stack.push(*array),
                    _ => {}
                }
            }
            out
        };
        for g in &new.globals {
            for r in g.ty.as_ref().map(&reaches).unwrap_or_default() {
                b.layouts.push(format!("global {} ({r}): {}", g.names[0], changed[&r].join(", ")));
            }
        }
        for f in &new.functions {
            for p in f.signature.iter().flat_map(|s| &s.params) {
                for r in reaches(&p.ty) {
                    b.layouts.push(format!("{}'s parameter {} ({r}): {}", f.names[0], p.name.clone().unwrap_or_default(), changed[&r].join(", ")));
                }
            }
        }
    }
    b
}

/// A name the snapshot keeps only as a deprecated alias: what it is now.
pub fn renamed<'a>(s: &'a Snapshot, name: &str) -> Option<&'a str> {
    s.functions.iter().find(|f| f.deprecated.iter().any(|d| d == name)).map(|f| f.names[0].as_str()).or_else(|| s.globals.iter().find(|g| g.deprecated.iter().any(|d| d == name)).map(|g| g.names[0].as_str()))
}

/// Several sources' snapshots of one binary, merged by address (D55):
/// `sources` in order of precedence, each named. The first's names lead;
/// every other source's names become aliases; a type or signature two
/// sources give differently warns, naming both and the winner. Each
/// symbol's provenance says which source typed it.
pub fn merge(sources: &[(String, Snapshot)]) -> Result<(Snapshot, Vec<String>)> {
    let Some((first_name, first)) = sources.first() else { bail!("no sources to merge") };
    let mut s = first.clone();
    let mut warnings = Vec::new();
    let named = |p: &Option<Provenance>, n: &str| -> Option<Provenance> {
        Some(Provenance { source: n.to_string(), ..p.clone().unwrap_or(Provenance { source: String::new(), file: None, text: None }) })
    };
    for f in &mut s.functions {
        if f.signature.is_some() {
            f.provenance = named(&f.provenance, first_name);
        }
    }
    for g in &mut s.globals {
        if g.ty.is_some() {
            g.provenance = named(&g.provenance, first_name);
        }
    }
    let mut merged = vec![Source { name: Some(first_name.clone()), ..first.source.clone() }];
    for (name, o) in &sources[1..] {
        if o.game != s.game {
            bail!("source {name} describes another binary than {first_name} (its identity hashes differ)");
        }
        merged.push(Source { name: Some(name.clone()), ..o.source.clone() });
        let winner = |what: &str, p: &Option<Provenance>| p.as_ref().map(|p| p.source.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| what.to_string());
        for f in &o.functions {
            match s.functions.iter_mut().find(|x| x.container == f.container && x.address == f.address) {
                Some(x) => {
                    for n in f.names.iter().chain(&f.deprecated) {
                        if !x.names.contains(n) && !x.deprecated.contains(n) {
                            x.deprecated.push(n.clone());
                        }
                    }
                    match (&x.signature, &f.signature) {
                        (None, Some(sig)) => {
                            x.signature = Some(sig.clone());
                            x.provenance = named(&f.provenance, name);
                        }
                        (Some(a), Some(b)) if a != b => {
                            let w = winner(first_name, &x.provenance);
                            warnings.push(format!("{}: {w} and {name} declare it differently; {w}'s wins", x.names[0]));
                        }
                        _ => {}
                    }
                }
                None => {
                    let mut f = f.clone();
                    f.provenance = named(&f.provenance, name);
                    s.functions.push(f);
                }
            }
        }
        for g in &o.globals {
            match s.globals.iter_mut().find(|x| x.container == g.container && x.address == g.address) {
                Some(x) => {
                    for n in g.names.iter().chain(&g.deprecated) {
                        if !x.names.contains(n) && !x.deprecated.contains(n) {
                            x.deprecated.push(n.clone());
                        }
                    }
                    match (&x.ty, &g.ty) {
                        (None, Some(t)) => {
                            x.ty = Some(t.clone());
                            x.size = x.size.or(g.size);
                            x.provenance = named(&g.provenance, name);
                        }
                        (Some(a), Some(b)) if a != b => {
                            let w = winner(first_name, &x.provenance);
                            warnings.push(format!("{}: {w} and {name} type it differently; {w}'s wins", x.names[0]));
                        }
                        _ => {}
                    }
                }
                None => {
                    let mut g = g.clone();
                    g.provenance = named(&g.provenance, name);
                    s.globals.push(g);
                }
            }
        }
        for (id, t) in &o.types {
            match s.types.get(id) {
                Some(x) if x != t => {
                    if matches!(x, TypeDef::Struct { .. } | TypeDef::Union { .. }) {
                        warnings.push(format!("{id}: the sources lay it out differently; {first_name}'s wins (or the most local that defines it)"));
                    }
                }
                Some(_) => {}
                None => {
                    s.types.insert(id.clone(), t.clone());
                }
            }
        }
        for p in &o.points {
            s.points.entry(p.0.clone()).or_insert(p.1.clone());
        }
        s.dropped.extend(o.dropped.iter().map(|d| format!("{name}: {d}")));
    }
    s.source = Source { kind: "merged".into(), version: first.source.version.clone(), name: None, merged };
    Ok((s, warnings))
}

/// A workspace's own declarations (`types/*.json`, plans/M12 phase 3):
/// snapshot entries for memory no decomp has typed. Addresses are numbers
/// or `0x…` strings; a container defaults to the executable's.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fragment {
    #[serde(rename = "$schema", default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub functions: Vec<FragmentSymbol>,
    #[serde(default)]
    pub globals: Vec<FragmentSymbol>,
    #[serde(default)]
    pub types: BTreeMap<String, TypeDef>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FragmentSymbol {
    #[serde(default)]
    pub container: Option<String>,
    pub address: serde_json::Value,
    pub names: Vec<String>,
    #[serde(default)]
    pub signature: Option<Signature>,
    #[serde(rename = "type", default)]
    pub ty: Option<TypeExpr>,
    #[serde(default)]
    pub size: Option<u32>,
}

fn address(v: &serde_json::Value) -> Result<u32> {
    match v {
        serde_json::Value::Number(n) => n.as_u64().map(|a| a as u32).context("address: a number"),
        serde_json::Value::String(s) => u32::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16).with_context(|| format!("address {s:?}: hex")),
        _ => bail!("address: a number or a 0x string"),
    }
}

/// The fragments in `dir` (`types/*.json`) as one snapshot of `like`'s
/// binary (its identity, containers and main).
pub fn from_fragments(dir: &Path, like: &Snapshot) -> Result<Option<Snapshot>> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Ok(None) };
    let mut files: Vec<std::path::PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
    files.sort();
    if files.is_empty() {
        return Ok(None);
    }
    let mut s = Snapshot {
        schema: None,
        schema_version: SCHEMA_VERSION,
        game: like.game.clone(),
        source: Source { kind: "local".into(), version: String::new(), name: None, merged: vec![] },
        main: like.main.clone(),
        containers: like.containers.clone(),
        functions: vec![],
        globals: vec![],
        types: BTreeMap::new(),
        points: BTreeMap::new(),
        dropped: vec![],
        unchecked: vec![],
    };
    let mut hash = sha2::Sha256::new();
    for p in files {
        let text = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        hash.update(text.as_bytes());
        let f: Fragment = serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))?;
        let file = p.file_name().map(|n| n.to_string_lossy().into_owned());
        let prov = Some(Provenance { source: "local".into(), file: file.clone(), text: None });
        for x in f.functions {
            s.functions.push(Function {
                container: x.container.unwrap_or_else(|| like.main.clone()),
                address: address(&x.address).with_context(|| format!("{}", p.display()))?,
                names: x.names,
                deprecated: vec![],
                signature: x.signature,
                provenance: prov.clone(),
            });
        }
        for x in f.globals {
            s.globals.push(Global {
                container: x.container.unwrap_or_else(|| like.main.clone()),
                address: address(&x.address).with_context(|| format!("{}", p.display()))?,
                names: x.names,
                deprecated: vec![],
                size: x.size,
                ty: x.ty,
                provenance: prov.clone(),
            });
        }
        s.types.extend(f.types);
    }
    s.source.version = hash.finalize().iter().take(8).map(|b| format!("{b:02x}")).collect();
    Ok(Some(s))
}
