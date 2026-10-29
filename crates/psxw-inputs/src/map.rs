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
    /// Code (`.text`) ranges in guest addresses, adjacent subsegments merged.
    pub text: Vec<Range<u32>>,
    /// Start of each object's or TU's `.text` (each begins with a function).
    pub text_starts: Vec<u32>,
    /// Read-only data ranges (`rodata`, `.rodata`, objects' `.rdata`).
    pub rodata: Vec<Range<u32>>,
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

impl SdkPlacement {
    /// The member's code: `[…, o, lib/x/y]` and `[…, o, lib/x/y, .text]`
    /// both place `.text`.
    pub fn is_text(&self) -> bool {
        is_text_section(self.section.as_deref())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// Named in the container's own symbol file.
    Own,
    /// Named in another container's symbol file that the container's splat
    /// config lists (`symbol_addrs_path`): an overlay using another's.
    Listed,
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
    /// Per container, the other containers whose symbol files its splat
    /// config lists, in the config's order.
    listed: HashMap<String, Vec<String>>,
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
        // Listed files that aren't a container's (the decomp's own build
        // outputs, such as a copy of the executable's symbols) aren't in the
        // snapshot; the main container's symbols resolve from it anyway.
        let dir = &manifest.containers.symbols_dir;
        let listed = configs
            .iter()
            .map(|cfg| {
                let others = cfg
                    .symbol_addrs
                    .iter()
                    .filter_map(|p| {
                        configs.iter().find(|o| {
                            o.id != cfg.id && ["txt", "names.txt"].iter().any(|s| *p == dir.join(format!("{}.{s}", o.id)))
                        })
                    })
                    .map(|o| o.id.clone())
                    .collect();
                (cfg.id.clone(), others)
            })
            .collect();
        Ok(Self { main, containers, symbols, by_name, listed })
    }

    /// A map with containers but no symbols (tests and synthetic inputs).
    pub fn synthetic(main: &str, containers: Vec<Container>) -> Self {
        Self {
            main: main.to_string(),
            containers,
            symbols: HashMap::new(),
            by_name: HashMap::new(),
            listed: HashMap::new(),
        }
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
        for other in self.listed.get(container).into_iter().flatten() {
            if let Some(r) = self.lookup(other, name, Via::Listed) {
                return Some(r);
            }
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
    let mut text: Vec<Range<u32>> = Vec::new();
    let mut text_starts = Vec::new();
    let mut rodata: Vec<Range<u32>> = Vec::new();
    let lo = cfg.code.iter().map(|s| s.vram).min().unwrap_or(0);
    let hi = cfg.code.iter().map(|s| s.vram_end()).max().unwrap_or(0);
    for seg in &cfg.code {
        for sub in &seg.subsegments {
            if is_text(sub) && sub.rom_end > sub.rom_start {
                let r = seg.vram_of(sub.rom_start)..seg.vram_of(sub.rom_end);
                text_starts.push(r.start);
                match text.last_mut() {
                    Some(last) if last.end == r.start => last.end = r.end,
                    _ => text.push(r),
                }
            }
            if is_rodata(sub) && sub.rom_end > sub.rom_start {
                let r = seg.vram_of(sub.rom_start)..seg.vram_of(sub.rom_end);
                match rodata.last_mut() {
                    Some(last) if last.end == r.start => last.end = r.end,
                    _ => rodata.push(r),
                }
            }
            let Some(name) = &sub.name else { continue };
            match sub.kind.as_str() {
                "c" => tus.push(TranslationUnit {
                    container: cfg.id.clone(),
                    name: name.clone(),
                    path: cfg.src_path.join(format!("{name}.c")),
                }),
                "o" => {
                    // `<library>/<member>`: the last two path components,
                    // whatever directory the config keeps them in.
                    let parts: Vec<&str> = name.rsplitn(3, '/').collect();
                    let member = match parts.as_slice() {
                        [m, l, ..] => format!("{l}/{m}"),
                        _ => name.clone(),
                    };
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
        text,
        text_starts,
        rodata,
    }
}

fn is_rodata(sub: &splat::Subsegment) -> bool {
    match sub.kind.as_str() {
        "rodata" | ".rodata" => true,
        "o" => sub.args.first().is_some_and(|s| s == ".rdata" || s == ".rodata"),
        _ => false,
    }
}

/// splat subsegment kinds that hold instructions. An `o` (object) entry with
/// no section argument is that object's `.text`.
fn is_text(sub: &splat::Subsegment) -> bool {
    match sub.kind.as_str() {
        "c" | "asm" | "hasm" => true,
        "o" => is_text_section(sub.args.first().map(String::as_str)),
        _ => false,
    }
}

/// An `o` subsegment's section argument names its `.text` when absent or
/// spelled out.
fn is_text_section(section: Option<&str>) -> bool {
    section.is_none_or(|s| s == ".text")
}

/// splat names unnamed symbols `<prefix>_<8 hex digits>`.
fn auto_name(name: &str) -> Option<Resolved> {
    let (prefix, hex) = name.rsplit_once('_')?;
    if prefix.is_empty() || hex.len() != 8 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    // Only C identifiers: compiler-local labels such as clang's
    // `.Lswitch.table.func_80012345` merely mention a function's name.
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    if !prefix.bytes().all(ident) || prefix.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    let addr = u32::from_str_radix(hex, 16).ok()?;
    if !(0x8000_0000..0x8020_0000).contains(&addr) {
        return None;
    }
    let is_func = prefix == "func" || prefix.ends_with("_func");
    Some(Resolved { addr, size: None, is_func, container: None, via: Via::AutoName })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_names_are_identifiers() {
        assert_eq!(auto_name("func_80012345").map(|r| r.addr), Some(0x8001_2345));
        assert!(auto_name("func_80012345").is_some_and(|r| r.is_func));
        assert_eq!(auto_name("D_8005E274").map(|r| r.addr), Some(0x8005_E274));
        assert!(auto_name(".Lswitch.table.ovl_11_func_800F1BD0").is_none());
        assert!(auto_name("func_90012345").is_none());
    }

    #[test]
    fn object_text_with_or_without_section() {
        let at = |section: Option<&str>| SdkPlacement {
            member: "libapi/first".into(),
            section: section.map(str::to_string),
            vram: 0x8001_0000..0x8001_0100,
        };
        assert!(at(None).is_text());
        assert!(at(Some(".text")).is_text());
        assert!(!at(Some(".rodata")).is_text());
        assert!(!at(Some(".data")).is_text());
    }
}
