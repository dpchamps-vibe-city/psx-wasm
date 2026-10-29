//! ELF32 little-endian relocatable objects (MIPS): sections, symbols and
//! REL relocations, which is all PsyQ's converted library objects carry.

use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;
use std::path::Path;

pub const SHT_SYMTAB: u32 = 2;
pub const SHT_NOBITS: u32 = 8;
pub const SHT_REL: u32 = 9;
pub const SHF_EXECINSTR: u32 = 4;
pub const SHN_COMMON: u16 = 0xFFF2;
pub const STB_GLOBAL: u8 = 1;
pub const STT_SECTION: u8 = 3;

pub const R_MIPS_32: u8 = 2;
pub const R_MIPS_26: u8 = 4;
pub const R_MIPS_HI16: u8 = 5;
pub const R_MIPS_LO16: u8 = 6;

#[derive(Debug, Clone)]
pub struct Section {
    pub name: String,
    pub ty: u32,
    pub flags: u32,
    pub offset: u32,
    pub size: u32,
    pub link: u32,
    pub info: u32,
}

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub value: u32,
    pub bind: u8,
    pub ty: u8,
    pub shndx: u16,
}

/// One REL entry: the patched offset, its type, its symbol.
#[derive(Debug, Clone, Copy)]
pub struct Reloc {
    pub offset: u32,
    pub ty: u8,
    pub sym: u32,
}

#[derive(Debug, Clone)]
pub struct Object {
    pub bytes: Vec<u8>,
    pub sections: Vec<Section>,
    pub symbols: Vec<Symbol>,
    /// Relocations by the section they patch.
    pub relocs: BTreeMap<usize, Vec<Reloc>>,
}

fn u16_at(b: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(b.get(at..at + 2).context("truncated ELF")?.try_into()?))
}

pub fn u32_at(b: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(b.get(at..at + 4).context("truncated ELF")?.try_into()?))
}

fn cstr(b: &[u8], at: usize) -> String {
    let s = b.get(at..).unwrap_or_default();
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    s[..end].iter().map(|&c| c as char).collect()
}

impl Object {
    pub fn read(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(bytes).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parse(bytes: Vec<u8>) -> Result<Self> {
        let b = &bytes;
        ensure!(b.get(..4) == Some(b"\x7fELF"), "not an ELF file");
        ensure!(b[4] == 1 && b[5] == 1, "not ELF32 little-endian");
        let shoff = u32_at(b, 0x20)? as usize;
        let shentsize = u16_at(b, 0x2E)? as usize;
        let shnum = u16_at(b, 0x30)? as usize;
        let shstrndx = u16_at(b, 0x32)? as usize;
        let mut raw = Vec::with_capacity(shnum);
        for i in 0..shnum {
            let at = shoff + i * shentsize;
            let w = |k: usize| u32_at(b, at + 4 * k);
            raw.push([w(0)?, w(1)?, w(2)?, w(4)?, w(5)?, w(6)?, w(7)?]);
        }
        let strtab = raw.get(shstrndx).context("no section name table")?[3] as usize;
        let sections: Vec<Section> = raw
            .iter()
            .map(|r| Section { name: cstr(b, strtab + r[0] as usize), ty: r[1], flags: r[2], offset: r[3], size: r[4], link: r[5], info: r[6] })
            .collect();
        let mut symbols = Vec::new();
        let mut relocs: BTreeMap<usize, Vec<Reloc>> = BTreeMap::new();
        for s in sections.iter().filter(|s| s.ty == SHT_SYMTAB) {
            let names = sections.get(s.link as usize).context("symbol table without strings")?.offset as usize;
            for k in 0..(s.size / 16) as usize {
                let at = s.offset as usize + 16 * k;
                let name = u32_at(b, at)? as usize;
                let info = b[at + 12];
                symbols.push(Symbol {
                    name: if name == 0 { String::new() } else { cstr(b, names + name) },
                    value: u32_at(b, at + 4)?,
                    bind: info >> 4,
                    ty: info & 15,
                    shndx: u16_at(b, at + 14)?,
                });
            }
        }
        for s in sections.iter().filter(|s| s.ty == SHT_REL) {
            let v = relocs.entry(s.info as usize).or_default();
            for k in 0..(s.size / 8) as usize {
                let at = s.offset as usize + 8 * k;
                let info = u32_at(b, at + 4)?;
                v.push(Reloc { offset: u32_at(b, at)?, ty: info as u8, sym: info >> 8 });
            }
        }
        Ok(Self { bytes, sections, symbols, relocs })
    }

    /// A section's contents (empty for NOBITS).
    pub fn data(&self, i: usize) -> &[u8] {
        match self.sections.get(i) {
            Some(s) if s.ty != SHT_NOBITS => self.bytes.get(s.offset as usize..(s.offset + s.size) as usize).unwrap_or_default(),
            _ => &[],
        }
    }

    pub fn is_text(&self, i: usize) -> bool {
        i > 0 && self.sections.get(i).is_some_and(|s| s.flags & SHF_EXECINSTR != 0)
    }

    /// Initialised data sections, where pointers are kept.
    pub fn is_data(&self, i: usize) -> bool {
        self.sections.get(i).is_some_and(|s| matches!(s.name.as_str(), ".data" | ".rdata" | ".sdata"))
    }

    pub fn word(&self, section: usize, off: u32) -> u32 {
        u32_at(self.data(section), off as usize).unwrap_or(0)
    }
}
