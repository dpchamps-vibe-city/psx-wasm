//! The subset of splat configuration psx-wasm needs: code segments and their
//! subsegments, which give each container's address range, its C translation
//! units, and where SDK archive members were placed.

use anyhow::{Context, Result, anyhow};
use serde_yaml::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SplatConfig {
    /// File stem of the config; the container id.
    pub id: String,
    /// `options.src_path`, relative to the repo root.
    pub src_path: PathBuf,
    /// `options.target_path`: the original file this config splits.
    pub target_path: Option<PathBuf>,
    /// `sha1` of the original file.
    pub sha1: Option<String>,
    pub code: Vec<CodeSegment>,
}

#[derive(Debug, Clone)]
pub struct CodeSegment {
    pub name: String,
    pub rom_start: u32,
    pub rom_end: u32,
    pub vram: u32,
    pub subsegments: Vec<Subsegment>,
}

#[derive(Debug, Clone)]
pub struct Subsegment {
    pub rom_start: u32,
    pub rom_end: u32,
    /// `c`, `o`, `data`, `rodata`, `.rodata`, ...
    pub kind: String,
    pub name: Option<String>,
    pub args: Vec<String>,
}

impl CodeSegment {
    pub fn vram_of(&self, rom: u32) -> u32 {
        self.vram + (rom - self.rom_start)
    }
    pub fn vram_end(&self) -> u32 {
        self.vram_of(self.rom_end)
    }
}

pub fn load(path: &Path) -> Result<SplatConfig> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let doc: Value = serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("bad splat path {}", path.display()))?
        .to_string();
    let src_path = doc["options"]["src_path"]
        .as_str()
        .ok_or_else(|| anyhow!("{}: options.src_path missing", path.display()))?
        .into();

    let segments = doc["segments"]
        .as_sequence()
        .ok_or_else(|| anyhow!("{}: segments missing", path.display()))?;
    // Each segment ends where the next begins; a bare `[0x76000]` marks the end.
    let starts: Vec<Option<u32>> = segments.iter().map(segment_start).collect();
    let mut code = Vec::new();
    for (i, seg) in segments.iter().enumerate() {
        if seg["type"].as_str() != Some("code") {
            continue;
        }
        let rom_start = num(&seg["start"]).ok_or_else(|| anyhow!("{}: code segment without start", path.display()))?;
        let rom_end = starts[i + 1..]
            .iter()
            .flatten()
            .next()
            .copied()
            .ok_or_else(|| anyhow!("{}: code segment without end", path.display()))?;
        let vram = num(&seg["vram"]).ok_or_else(|| anyhow!("{}: code segment without vram", path.display()))?;
        let mut subs: Vec<Subsegment> = seg["subsegments"]
            .as_sequence()
            .map(|s| s.iter().filter_map(subsegment).collect())
            .unwrap_or_default();
        for j in 0..subs.len() {
            subs[j].rom_end = subs.get(j + 1).map_or(rom_end, |n| n.rom_start);
        }
        code.push(CodeSegment {
            name: seg["name"].as_str().unwrap_or(&id).to_string(),
            rom_start,
            rom_end,
            vram,
            subsegments: subs,
        });
    }
    Ok(SplatConfig {
        id,
        src_path,
        target_path: doc["options"]["target_path"].as_str().map(PathBuf::from),
        sha1: doc["sha1"].as_str().map(str::to_lowercase),
        code,
    })
}

fn segment_start(seg: &Value) -> Option<u32> {
    match seg {
        Value::Sequence(items) => items.first().and_then(num),
        Value::Mapping(_) => num(&seg["start"]),
        _ => None,
    }
}

fn subsegment(v: &Value) -> Option<Subsegment> {
    match v {
        Value::Sequence(items) => {
            let rom_start = num(items.first()?)?;
            let kind = items.get(1)?.as_str()?.to_string();
            let name = items.get(2).and_then(Value::as_str).map(str::to_string);
            let args = items.iter().skip(3).filter_map(Value::as_str).map(str::to_string).collect();
            Some(Subsegment { rom_start, rom_end: rom_start, kind, name, args })
        }
        Value::Mapping(_) => Some(Subsegment {
            rom_start: num(&v["start"])?,
            rom_end: 0,
            kind: v["type"].as_str()?.to_string(),
            name: v["name"].as_str().map(str::to_string),
            args: Vec::new(),
        }),
        _ => None,
    }
}

fn num(v: &Value) -> Option<u32> {
    match v {
        Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        Value::String(s) => crate::parse_u32(s),
        _ => None,
    }
}
