//! Data placement (docs/design.md §1, §8). Every data symbol a guest object
//! defines or references gets a guest address:
//!
//! - named symbols sit where the address map puts them;
//! - anonymous read-only data without relocations (string literals,
//!   constant tables) is placed on identical bytes in the container's
//!   original read-only data, so pointers to it equal the original's;
//! - everything else anonymous goes to the AUX region, 8-byte aligned.

use crate::object::{RelocKind, SymbolKind, WasmObject};
use anyhow::{Result, bail};
use psxw_inputs::{AddressMap, originals::Originals};
use std::collections::{BTreeMap, HashMap};

/// Start of the AUX region (window offset `phys::AUX_DATA.start`).
pub const AUX_BASE: u32 = 0x8000_0000 | psxw_abi::phys::AUX_DATA.start;
pub const AUX_END: u32 = 0x8000_0000 | psxw_abi::phys::AUX_DATA.end;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// At its address-map address.
    Named,
    /// On identical original read-only bytes.
    Matched,
    /// Allocated in the AUX region.
    Aux,
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
    /// AUX allocations: (input, symbol index, address).
    pub aux: Vec<(usize, u32, u32)>,
    next: u32,
}

impl Layout {
    pub fn place(
        inputs: &[(&str, &WasmObject)],
        map: &AddressMap,
        rodata: Option<&Rodata>,
    ) -> Result<Self> {
        let mut l = Layout { next: AUX_BASE, ..Default::default() };
        for (i, (container, obj)) in inputs.iter().enumerate() {
            for (si, s) in obj.symbols.iter().enumerate() {
                if s.kind != SymbolKind::Data || !s.defined {
                    continue;
                }
                let Some(def) = s.data else { continue };
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
        if let Some(r) = map.resolve(container, &s.name) {
            return Ok(r.addr);
        }
        for (j, (_, o)) in inputs.iter().enumerate() {
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
