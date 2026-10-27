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
    /// Old names (`--bump`, other sources), each with what replaced it.
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
    }
}
