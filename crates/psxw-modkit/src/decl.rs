//! `mod.json`: a mod's identity, load order and every declaration
//! (plans/M11-modkit.md "Declarations"). Every distinct aspect is its own
//! field; `schema/mod.schema.json` describes the same file for editors.

use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FILE: &str = "mod.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModDecl {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub id: String,
    pub version: String,
    pub game: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub load_after: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<Hook>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data: Vec<DataPatch>,
    /// The mod's code as a wasm module another toolchain builds (any
    /// language), instead of C under `src/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wasm: Option<WasmSource>,
}

/// A mod built by another toolchain: a wasm module linked with
/// `wasm-ld --emit-relocs --allow-undefined` (so its code still carries
/// relocations psxw can place it by; game functions and data stay
/// undefined), which psxw links like an object.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WasmSource {
    /// The module, relative to the mod's directory.
    pub path: String,
    /// The command that builds it, run in the mod's directory first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub build: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Before,
    After,
    Patch,
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Kind::Before => "before",
            Kind::After => "after",
            Kind::Patch => "patch",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    pub kind: Kind,
    pub target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Target>,
    #[serde(rename = "impl")]
    pub imp: Impl,
    /// `patch` only: the name the implementation calls the next link by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<String>,
    /// The target's C signature, when no header declares it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<Signature>,
}

/// A function or data symbol of the game, in exactly one of four forms.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct Target {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// Hex, `0x…`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point: Option<String>,
}

/// The validated form of a [`Target`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetForm {
    Symbol(String),
    InContainer(String, String),
    Address(String, u32),
    Point(String),
}

impl Target {
    pub fn form(&self) -> Result<TargetForm> {
        match (&self.symbol, &self.container, &self.address, &self.point) {
            (Some(s), None, None, None) => Ok(TargetForm::Symbol(s.clone())),
            (Some(s), Some(c), None, None) => Ok(TargetForm::InContainer(c.clone(), s.clone())),
            (None, Some(c), Some(a), None) => {
                let hex = a.strip_prefix("0x").or_else(|| a.strip_prefix("0X")).with_context(|| format!("address {a:?}: hex, with 0x"))?;
                let v = u32::from_str_radix(hex, 16).with_context(|| format!("address {a:?} isn't hex"))?;
                Ok(TargetForm::Address(c.clone(), v))
            }
            (None, None, None, Some(p)) => Ok(TargetForm::Point(p.clone())),
            _ => bail!(
                "a target is {{symbol}}, {{container, symbol}}, {{container, address}} or {{point}}; got {}",
                serde_json::to_string(self).unwrap_or_default()
            ),
        }
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.form() {
            Ok(TargetForm::Symbol(s)) => write!(f, "{s}"),
            Ok(TargetForm::InContainer(c, s)) => write!(f, "{c}:{s}"),
            Ok(TargetForm::Address(c, a)) => write!(f, "{c}:0x{a:08X}"),
            Ok(TargetForm::Point(p)) => write!(f, "point {p}"),
            Err(_) => write!(f, "(bad target)"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct Impl {
    /// Relative to the mod's directory.
    pub file: String,
    pub function: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Signature {
    /// One C type (`void`, `s32`, `struct Foo *`, `void (*)(int)`).
    pub returns: String,
    #[serde(default)]
    pub params: Vec<Param>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub variadic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Param {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DataType {
    U8,
    S8,
    U16,
    S16,
    U32,
    S32,
    /// A mod function's address.
    Fnptr,
    /// `value` is an array of bytes.
    Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DataPatch {
    pub target: Target,
    #[serde(default)]
    pub offset: u32,
    #[serde(rename = "type")]
    pub ty: DataType,
    pub value: serde_json::Value,
}

impl DataPatch {
    /// The bytes written, or the mod function whose address is.
    pub fn value(&self) -> Result<DataValue> {
        let int = |lo: i64, hi: i64| -> Result<i64> {
            let v = self.value.as_i64().with_context(|| format!("{:?} wants an integer value", self.ty))?;
            ensure!((lo..=hi).contains(&v), "{v} doesn't fit {:?}", self.ty);
            Ok(v)
        };
        Ok(DataValue::Bytes(match self.ty {
            DataType::U8 => vec![int(0, 0xFF)? as u8],
            DataType::S8 => vec![int(-0x80, 0x7F)? as u8],
            DataType::U16 => (int(0, 0xFFFF)? as u16).to_le_bytes().to_vec(),
            DataType::S16 => (int(-0x8000, 0x7FFF)? as u16).to_le_bytes().to_vec(),
            DataType::U32 => (int(0, 0xFFFF_FFFF)? as u32).to_le_bytes().to_vec(),
            DataType::S32 => (int(-0x8000_0000, 0x7FFF_FFFF)? as u32).to_le_bytes().to_vec(),
            DataType::Bytes => {
                let a = self.value.as_array().context("bytes wants an array of bytes")?;
                a.iter()
                    .map(|v| v.as_u64().filter(|b| *b <= 0xFF).map(|b| b as u8).context("bytes wants values 0..255"))
                    .collect::<Result<_>>()?
            }
            DataType::Fnptr => {
                let imp: Impl = serde_json::from_value(self.value.clone()).context("fnptr wants {file, function}")?;
                return Ok(DataValue::FnPtr(imp));
            }
        }))
    }

    pub fn size(&self) -> u32 {
        match self.ty {
            DataType::U8 | DataType::S8 => 1,
            DataType::U16 | DataType::S16 => 2,
            DataType::U32 | DataType::S32 | DataType::Fnptr => 4,
            DataType::Bytes => self.value.as_array().map_or(0, |a| a.len() as u32),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataValue {
    Bytes(Vec<u8>),
    FnPtr(Impl),
}

/// A mod on disk: its directory and declarations.
#[derive(Debug, Clone)]
pub struct Mod {
    pub dir: PathBuf,
    pub decl: ModDecl,
    /// A script mod's definition, read by running its factory (plans/M12):
    /// planned with the others, but it needs no C types, and nothing is
    /// compiled.
    pub script: bool,
}

fn ident(s: &str) -> bool {
    let mut b = s.bytes();
    b.next().is_some_and(|c| c.is_ascii_alphabetic() || c == b'_') && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

impl Mod {
    pub fn load(dir: &Path) -> Result<Self> {
        let dir = &std::fs::canonicalize(dir).with_context(|| format!("no mod at {}", dir.display()))?;
        let path = dir.join(FILE);
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let decl: ModDecl = serde_json::from_str(&text).with_context(|| format!("{}", path.display()))?;
        let m = Mod { dir: dir.to_path_buf(), decl, script: false };
        m.validate().with_context(|| format!("{}", path.display()))?;
        Ok(m)
    }

    pub fn id(&self) -> &str {
        &self.decl.id
    }

    /// What the file says by itself, before any game is consulted.
    pub fn validate(&self) -> Result<()> {
        let d = &self.decl;
        ensure!(
            !d.id.is_empty() && d.id.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'),
            "id {:?}: lowercase letters, digits and '-'",
            d.id
        );
        ensure!(!d.version.is_empty(), "version is empty");
        let mut originals = std::collections::BTreeSet::new();
        for (i, h) in d.hooks.iter().enumerate() {
            let at = format!("hooks[{i}]");
            h.target.form().with_context(|| format!("{at}.target"))?;
            if let Some(f) = &h.from {
                match f.form().with_context(|| format!("{at}.from"))? {
                    TargetForm::Point(_) => bail!("{at}.from: a caller is a function, not a point"),
                    _ => {}
                }
            }
            ensure!(ident(&h.imp.function), "{at}.impl.function {:?} isn't a C identifier", h.imp.function);
            ensure!(self.dir.join(&h.imp.file).is_file(), "{at}.impl.file {} doesn't exist", h.imp.file);
            if let Some(o) = &h.original {
                ensure!(h.kind == Kind::Patch, "{at}: only a patch has an original");
                ensure!(ident(o), "{at}.original {o:?} isn't a C identifier");
                ensure!(originals.insert(o.clone()), "{at}.original {o}: another hook already names its next link {o}");
            }
            if let Some(s) = &h.signature {
                for p in &s.params {
                    ensure!(ident(&p.name), "{at}.signature: parameter name {:?} isn't a C identifier", p.name);
                }
            }
        }
        for (i, p) in d.data.iter().enumerate() {
            let at = format!("data[{i}]");
            match p.target.form().with_context(|| format!("{at}.target"))? {
                TargetForm::Point(_) => bail!("{at}.target: data patches name data, not a point"),
                _ => {}
            }
            match p.value().with_context(|| format!("{at}.value"))? {
                DataValue::FnPtr(imp) => {
                    ensure!(self.dir.join(&imp.file).is_file(), "{at}.value.file {} doesn't exist", imp.file);
                }
                DataValue::Bytes(b) => ensure!(!b.is_empty(), "{at}.value: no bytes"),
            }
        }
        Ok(())
    }

    /// The mod's C sources: every `.c` under `src/` (none for a mod built
    /// by another toolchain).
    pub fn sources(&self) -> Result<Vec<PathBuf>> {
        if self.decl.wasm.is_some() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let src = self.dir.join("src");
        if src.is_dir() {
            walk(&src, &mut out)?;
        }
        out.sort();
        Ok(out)
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if p.is_dir() {
            walk(&p, out)?;
        } else if p.extension().is_some_and(|e| e == "c") {
            out.push(p);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_forms() {
        let t = |j: &str| serde_json::from_str::<Target>(j).unwrap().form();
        assert_eq!(t(r#"{"symbol":"rand"}"#).unwrap(), TargetForm::Symbol("rand".into()));
        assert_eq!(t(r#"{"container":"ovl_02","address":"0x800C1C90"}"#).unwrap(), TargetForm::Address("ovl_02".into(), 0x800C_1C90));
        assert!(t(r#"{"symbol":"a","point":"frame"}"#).is_err());
        assert!(t(r#"{"container":"ovl_02","address":"800C1C90"}"#).is_err());
        assert!(serde_json::from_str::<Target>(r#"{"sym":"x"}"#).is_err());
    }

    #[test]
    fn data_values() {
        let p = |j: &str| serde_json::from_str::<DataPatch>(j).unwrap();
        assert_eq!(p(r#"{"target":{"symbol":"g"},"type":"u16","value":10}"#).value().unwrap(), DataValue::Bytes(vec![10, 0]));
        assert_eq!(p(r#"{"target":{"symbol":"g"},"type":"s8","value":-1}"#).value().unwrap(), DataValue::Bytes(vec![0xFF]));
        assert!(p(r#"{"target":{"symbol":"g"},"type":"u8","value":256}"#).value().is_err());
        assert_eq!(p(r#"{"target":{"symbol":"g"},"type":"bytes","value":[1,2,3]}"#).size(), 3);
    }
}
