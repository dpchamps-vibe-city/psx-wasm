//! Data placement (docs/design.md §1, §8). Every data symbol a guest object
//! defines or references gets a guest address:
//!
//! - named symbols sit where the address map puts them;
//! - anonymous read-only data without relocations (string literals,
//!   constant tables) is placed on identical bytes in the container's
//!   original read-only data, so pointers to it equal the original's;
//! - everything else anonymous goes to the AUX region, 8-byte aligned;
//! - mods' data (plans/M11) goes to the mod area, below its stack: a mod's
//!   names never resolve to game addresses, and its references resolve
//!   inside the mod first.

use crate::object::{RelocKind, SymbolKind, WasmObject};
use anyhow::{Result, bail};
use psxw_inputs::{AddressMap, originals::Originals};
use std::collections::{BTreeMap, HashMap};

/// Start of the AUX region (window offset `phys::AUX_DATA.start`).
pub const AUX_BASE: u32 = 0x8000_0000 | psxw_abi::phys::AUX_DATA.start;
pub const AUX_END: u32 = 0x8000_0000 | psxw_abi::phys::AUX_DATA.end;
/// Static mods' data: from the mod area's start up to its stack.
pub const MOD_BASE: u32 = 0x8000_0000 | psxw_abi::phys::MOD_DATA.start;
pub const MOD_END: u32 = 0x8000_0000 | psxw_abi::phys::MOD_STACK.start;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// At its address-map address.
    Named,
    /// On identical original read-only bytes.
    Matched,
    /// Allocated in the AUX region.
    Aux,
    /// A mod's, in the mod area.
    Mod,
}

#[derive(Debug, Clone, Copy)]
pub struct Placed {
    pub addr: u32,
    pub how: How,
}

/// Original read-only data per container, for content matching.
#[derive(Debug, Clone, Default)]
pub struct Rodata {
    ranges: BTreeMap<String, Vec<(u32, Vec<u8>)>>,
}

impl Rodata {
    pub fn new(map: &AddressMap, originals: &Originals) -> Self {
        let mut ranges = BTreeMap::new();
        for c in &map.containers {
            let mut v = Vec::new();
            for r in &c.rodata {
                if let Some(bytes) = originals.read(&c.id, r.start, (r.end - r.start) as usize) {
                    v.push((r.start, bytes.to_vec()));
                }
            }
            ranges.insert(c.id.clone(), v);
        }
        Self { ranges }
    }

    /// First suitably aligned address in `container`'s (then the main
    /// container's) read-only data holding exactly `needle`.
    fn find(&self, containers: &[&str], needle: &[u8], align: u32) -> Option<u32> {
        if needle.is_empty() {
            return None;
        }
        for c in containers {
            for (base, bytes) in self.ranges.get(*c).into_iter().flatten() {
                let mut i = 0;
                while i + needle.len() <= bytes.len() {
                    let a = base + i as u32;
                    if a % align == 0 && &bytes[i..i + needle.len()] == needle {
                        return Some(a);
                    }
                    i += 1;
                }
            }
        }
        None
    }
}

#[derive(Debug, Default)]
pub struct Layout {
    /// (input, symbol index) → placement, for defined data symbols.
    pub defined: HashMap<(usize, u32), Placed>,
    /// AUX and mod-area allocations: (input, symbol index, address).
    pub aux: Vec<(usize, u32, u32)>,
    next: u32,
    next_mod: u32,
    /// Per input, the mod it belongs to (`None` for the game's).
    mod_of: Vec<Option<usize>>,
}

impl Layout {
    pub fn place(
        inputs: &[(&str, &WasmObject)],
        map: &AddressMap,
        rodata: Option<&Rodata>,
    ) -> Result<Self> {
        Self::place_mods(inputs, map, rodata, &[])
    }

    /// [`Layout::place`], with `mod_of[i]` naming the mod input `i` belongs
    /// to (missing or `None`: the game's).
    pub fn place_mods(
        inputs: &[(&str, &WasmObject)],
        map: &AddressMap,
        rodata: Option<&Rodata>,
        mod_of: &[Option<usize>],
    ) -> Result<Self> {
        let mut l = Layout { next: AUX_BASE, next_mod: MOD_BASE, mod_of: mod_of.to_vec(), ..Default::default() };
        for (i, (container, obj)) in inputs.iter().enumerate() {
            for (si, s) in obj.symbols.iter().enumerate() {
                if s.kind != SymbolKind::Data || !s.defined {
                    continue;
                }
                let Some(def) = s.data else { continue };
                if l.module(i).is_some() {
                    let align = (1u32 << obj.segment_of(def).align_log2).max(8);
                    let addr = l.next_mod.div_ceil(align) * align;
                    l.next_mod = addr + def.size.max(1);
                    if l.next_mod > MOD_END {
                        bail!("the mod area is full placing {}", s.name);
                    }
                    l.defined.insert((i, si as u32), Placed { addr, how: How::Mod });
                    l.aux.push((i, si as u32, addr));
                    continue;
                }
                if let Some(r) = map.resolve(container, &s.name) {
                    l.defined.insert((i, si as u32), Placed { addr: r.addr, how: How::Named });
                    continue;
                }
                let seg = obj.segment_of(def);
                let align = 1u32 << seg.align_log2;
                let has_relocs = obj.relocs_in(def).next().is_some();
                if seg.is_rodata() && !has_relocs {
                    let main = map.main.as_str();
                    if let Some(a) = rodata.and_then(|r| r.find(&[container, main], obj.data_bytes(def), align)) {
                        l.defined.insert((i, si as u32), Placed { addr: a, how: How::Matched });
                        continue;
                    }
                }
                let addr = l.next.div_ceil(align.max(8)) * align.max(8);
                l.next = addr + def.size.max(1);
                if l.next > AUX_END {
                    bail!("AUX region exhausted placing {}", s.name);
                }
                l.defined.insert((i, si as u32), Placed { addr, how: How::Aux });
                l.aux.push((i, si as u32, addr));
            }
        }
        Ok(l)
    }

    /// Address of data symbol `si` of input `i`: its placement, or for an
    /// undefined symbol, the address map's (then any input defining it).
    pub fn data_addr(
        &self,
        inputs: &[(&str, &WasmObject)],
        map: &AddressMap,
        i: usize,
        si: u32,
    ) -> Result<u32> {
        if let Some(p) = self.defined.get(&(i, si)) {
            return Ok(p.addr);
        }
        let (container, obj) = inputs[i];
        let s = &obj.symbols[si as usize];
        // A mod's own globals first, then the game's.
        if let Some(m) = self.module(i) {
            for (j, (_, o)) in inputs.iter().enumerate().filter(|(j, _)| self.module(*j) == Some(m)) {
                for (sj, t) in o.symbols.iter().enumerate() {
                    if t.kind == SymbolKind::Data && t.defined && !t.local && t.name == s.name {
                        if let Some(p) = self.defined.get(&(j, sj as u32)) {
                            return Ok(p.addr);
                        }
                    }
                }
            }
        }
        if let Some(r) = map.resolve(container, &s.name) {
            return Ok(r.addr);
        }
        for (j, (_, o)) in inputs.iter().enumerate().filter(|(j, _)| self.module(*j).is_none()) {
            for (sj, t) in o.symbols.iter().enumerate() {
                if t.kind == SymbolKind::Data && t.defined && !t.local && t.name == s.name {
                    if let Some(p) = self.defined.get(&(j, sj as u32)) {
                        return Ok(p.addr);
                    }
                }
            }
        }
        bail!("data symbol {} has no address", s.name)
    }

    /// The mod input `i` belongs to.
    pub fn module(&self, i: usize) -> Option<usize> {
        self.mod_of.get(i).copied().flatten()
    }

    /// Bytes of a placed symbol with its data relocations applied.
    pub fn bytes_of(
        &self,
        inputs: &[(&str, &WasmObject)],
        map: &AddressMap,
        i: usize,
        si: u32,
        func_addr: &dyn Fn(usize, u32) -> Result<u32>,
    ) -> Result<Vec<u8>> {
        let obj = inputs[i].1;
        let def = obj.symbols[si as usize].data.expect("defined data");
        let mut bytes = if obj.segment_of(def).is_bss() { vec![0; def.size as usize] } else { obj.data_bytes(def).to_vec() };
        for (off, r) in obj.relocs_in(def) {
            let v = match r.kind {
                RelocKind::MemoryAddrI32 => self.data_addr(inputs, map, i, r.symbol)?.wrapping_add(r.addend as u32),
                RelocKind::TableIndexI32 => func_addr(i, r.symbol)?,
                RelocKind::Other => bail!("unsupported data relocation in {}", obj.symbols[si as usize].name),
            };
            bytes[off as usize..off as usize + 4].copy_from_slice(&v.to_le_bytes());
        }
        Ok(bytes)
    }
}
