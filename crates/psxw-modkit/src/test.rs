//! A mod's tests (plans/M11 task 10): `tests/*.json`, each a replay run for
//! some vblanks with symbols traced every vblank, expectations, and an
//! optional check command reading the trace as JSON on stdin.

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestSpec {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub replay: ReplayRef,
    pub frames: u64,
    #[serde(default)]
    pub trace: Vec<TraceSym>,
    #[serde(default)]
    pub expect: Vec<Expect>,
    #[serde(default)]
    pub check: Option<Check>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReplayRef {
    /// `games/<id>/replays/<name>` in psx-wasm.
    Name { name: String },
    /// Relative to the test.
    File { file: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    U8,
    S8,
    U16,
    S16,
    U32,
    S32,
}

impl ValueType {
    pub fn size(self) -> u32 {
        match self {
            ValueType::U8 | ValueType::S8 => 1,
            ValueType::U16 | ValueType::S16 => 2,
            ValueType::U32 | ValueType::S32 => 4,
        }
    }

    /// A zero-extended raw value as this type.
    pub fn value(self, raw: u32) -> i64 {
        match self {
            ValueType::U8 => (raw & 0xFF) as i64,
            ValueType::S8 => raw as u8 as i8 as i64,
            ValueType::U16 => (raw & 0xFFFF) as i64,
            ValueType::S16 => raw as u16 as i16 as i64,
            ValueType::U32 => raw as i64,
            ValueType::S32 => raw as i32 as i64,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceSym {
    pub symbol: String,
    #[serde(rename = "type")]
    pub ty: ValueType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Expect {
    NoTrap,
    /// Two runs give identical RAM, VRAM and audio hashes.
    Deterministic,
    /// `symbol`'s value at vblank `at` compares to `value` by `op`.
    Symbol {
        at: u64,
        symbol: String,
        #[serde(rename = "type", default)]
        ty: Option<ValueType>,
        op: String,
        value: i64,
    },
    /// A line the mods logged (`psxmod_log`) contains this.
    Log { contains: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub command: Vec<String>,
    /// Also trace the game without mods over the same replay.
    #[serde(default)]
    pub baseline: bool,
}

/// One test file.
#[derive(Debug, Clone)]
pub struct Test {
    pub path: PathBuf,
    pub name: String,
    pub spec: TestSpec,
}

/// A mod's tests, by file name.
pub fn tests(mod_dir: &Path) -> Result<Vec<Test>> {
    let dir = mod_dir.join("tests");
    let Ok(rd) = std::fs::read_dir(&dir) else { return Ok(vec![]) };
    let mut paths: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let text = std::fs::read_to_string(&p)?;
            let spec: TestSpec = serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))?;
            Ok(Test { name: p.file_stem().unwrap_or_default().to_string_lossy().into_owned(), path: p, spec })
        })
        .collect()
}

/// Whether `a op b` holds.
pub fn compare(a: i64, op: &str, b: i64) -> Option<bool> {
    Some(match op {
        "==" => a == b,
        "!=" => a != b,
        "<" => a < b,
        "<=" => a <= b,
        ">" => a > b,
        ">=" => a >= b,
        _ => return None,
    })
}
