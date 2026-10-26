//! The address map: every container, its translation units and SDK member
//! placements (from splat), and every named symbol (from the symbol files).
//! Names resolve to original addresses; this is what lets psx-wasm keep the
//! PSX memory map (docs/design.md §1).

use crate::game::GameManifest;
use crate::splat::{self, SplatConfig};
use crate::symbols::{self, SymEntry};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Container {
    pub id: String,
    pub is_main: bool,
    /// Union of the container's code segments, in guest addresses.
    pub vram: Range<u32>,
    pub tus: Vec<TranslationUnit>,
    /// Where SDK archive members' sections were placed.
    pub sdk: Vec<SdkPlacement>,
    /// Original file (relative to the user's checkout) and its sha1.
    pub target_path: Option<PathBuf>,
    pub sha1: Option<String>,
    /// File-offset ↔ guest-address mapping: (file range, vram start).
    pub segments: Vec<(Range<u32>, u32)>,
}

#[derive(Debug, Clone)]
pub struct TranslationUnit {
    pub container: String,
    /// splat subsegment name, e.g. `modules/game_loop`.
    pub name: String,
    /// Source path relative to the repo root.
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct SdkPlacement {
    /// `libgpu/sys` (archive/member without extension).
    pub member: String,
    pub section: Option<String>,
    pub vram: Range<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// Named in the container's own symbol file.
    Own,
    /// Named in the main executable's symbol file (an overlay importing it).
    Main,
    /// Named in an overlay's symbol file (the executable importing it).
    Overlay,
    /// splat's automatic `<prefix>_<ADDR>` naming.
    AutoName,
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub addr: u32,
    pub size: Option<u32>,
    pub is_func: bool,
    /// Container whose symbol file named it (`None` for auto-names).
    pub container: Option<String>,
    pub via: Via,
}

#[derive(Debug, Clone)]
pub struct AddressMap {
    pub main: String,
    pub containers: Vec<Container>,
    symbols: HashMap<String, Vec<SymEntry>>,
    by_name: HashMap<String, HashMap<String, usize>>,
}

impl AddressMap {
    pub fn load(snapshot: &Path, manifest: &GameManifest) -> Result<Self> {
        let splat_dir = snapshot.join(&manifest.containers.splat_dir);
        let mut configs: Vec<SplatConfig> = Vec::new();
        for entry in std::fs::read_dir(&splat_dir).with_context(|| format!("reading {}", splat_dir.display()))? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "yaml") {
                configs.push(splat::load(&path)?);
            }
        }
        configs.sort_by(|a, b| a.id.cmp(&b.id));
        let main = manifest.containers.main.clone();
        if !configs.iter().any(|c| c.id == main) {
            bail!("no splat config for main container {main}");
        }

        let mut containers = Vec::new();
        let mut symbols = HashMap::new();
        for cfg in &configs {
            containers.push(container_from(cfg, cfg.id == main));
            let mut entries = Vec::new();
            for suffix in ["txt", "names.txt"] {
                let path = snapshot.join(&manifest.containers.symbols_dir).join(format!("{}.{suffix}", cfg.id));
                if let Ok(text) = std::fs::read_to_string(&path) {
                    entries.extend(symbols::parse(&text));
                }
            }
            symbols.insert(cfg.id.clone(), entries);
        }
        let by_name = symbols
            .iter()
            .map(|(id, entries): (&String, &Vec<SymEntry>)| {
                let names = entries.iter().enumerate().map(|(i, e)| (e.name.clone(), i)).collect();
                (id.clone(), names)
            })
            .collect();
        Ok(Self { main, containers, symbols, by_name })
    }

    pub fn container(&self, id: &str) -> Option<&Container> {
        self.containers.iter().find(|c| c.id == id)
    }

    pub fn symbols(&self, container: &str) -> &[SymEntry] {
        self.symbols.get(container).map_or(&[], Vec::as_slice)
    }

    /// Resolve a name as seen from code in `container`. Asm labels may be
    /// `sym+offset` / `sym-offset`, as GNU as resolves them.
    pub fn resolve(&self, container: &str, name: &str) -> Option<Resolved> {
        if let Some(i) = name.rfind(['+', '-']).filter(|&i| i > 0) {
            let delta = crate::parse_u32(&name[i + 1..])? as i64;
            let delta = if &name[i..=i] == "-" { -delta } else { delta };
            let mut r = self.resolve(container, &name[..i])?;
            r.addr = (r.addr as i64 + delta) as u32;
            r.size = None;
            return Some(r);
        }
        if let Some(r) = self.lookup(container, name, Via::Own) {
            return Some(r);
        }
        if container != self.main {
            if let Some(r) = self.lookup(&self.main, name, Via::Main) {
                return Some(r);
            }
        } else {
            for c in &self.containers {
                if c.id != self.main {
                    if let Some(r) = self.lookup(&c.id, name, Via::Overlay) {
                        return Some(r);
                    }
                }
            }
        }
        auto_name(name)
    }

    fn lookup(&self, container: &str, name: &str, via: Via) -> Option<Resolved> {
        let i = *self.by_name.get(container)?.get(name)?;
        let e = &self.symbols[container][i];
        Some(Resolved {
            addr: e.addr,
            size: e.size,
            is_func: e.is_func(),
            container: Some(container.to_string()),
            via,
        })
    }

    /// SDK placement covering `addr` in `container`, if any.
    pub fn sdk_at(&self, container: &str, addr: u32) -> Option<&SdkPlacement> {
        self.container(container)?.sdk.iter().find(|p| p.vram.contains(&addr))
    }
}

fn container_from(cfg: &SplatConfig, is_main: bool) -> Container {
    let mut tus = Vec::new();
    let mut sdk = Vec::new();
    let lo = cfg.code.iter().map(|s| s.vram).min().unwrap_or(0);
    let hi = cfg.code.iter().map(|s| s.vram_end()).max().unwrap_or(0);
    for seg in &cfg.code {
        for sub in &seg.subsegments {
            let Some(name) = &sub.name else { continue };
            match sub.kind.as_str() {
                "c" => tus.push(TranslationUnit {
                    container: cfg.id.clone(),
                    name: name.clone(),
                    path: cfg.src_path.join(format!("{name}.c")),
                }),
                "o" => {
                    let member = name.trim_start_matches("../").trim_start_matches("lib/").to_string();
                    sdk.push(SdkPlacement {
                        member,
                        section: sub.args.first().cloned(),
                        vram: seg.vram_of(sub.rom_start)..seg.vram_of(sub.rom_end),
                    });
                }
                _ => {}
            }
        }
    }
    Container {
        id: cfg.id.clone(),
        is_main,
        vram: lo..hi,
        tus,
        sdk,
        target_path: cfg.target_path.clone(),
        sha1: cfg.sha1.clone(),
        segments: cfg.code.iter().map(|s| (s.rom_start..s.rom_end, s.vram)).collect(),
    }
}

/// splat names unnamed symbols `<prefix>_<8 hex digits>`.
fn auto_name(name: &str) -> Option<Resolved> {
    let (prefix, hex) = name.rsplit_once('_')?;
    if prefix.is_empty() || hex.len() != 8 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let addr = u32::from_str_radix(hex, 16).ok()?;
    if !(0x8000_0000..0x8020_0000).contains(&addr) {
        return None;
    }
    let is_func = prefix == "func" || prefix.ends_with("_func");
    Some(Resolved { addr, size: None, is_func, container: None, via: Via::AutoName })
}
