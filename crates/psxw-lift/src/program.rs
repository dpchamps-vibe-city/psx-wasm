//! Every container's code as the translator sees it: text ranges and the
//! set of function starts, from which function extents follow.
//!
//! Starts come from the union of what the inputs say (symbol files, the
//! first function of each object's `.text`, C definitions, the EXE entry)
//! and what translation discovers (call targets, code addresses built in
//! registers). An extent runs to the next start, or covers `size` bytes when
//! the symbol file gives one, in which case the names inside it are
//! interior entry points rather than boundaries.

use psxw_inputs::AddressMap;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    /// `type:func` in the container's symbol file.
    Symbol,
    /// First address of an object's or TU's `.text`.
    Subsegment,
    /// A C definition resolved through the address map.
    Defined,
    /// A function the C hands to the lift tier (`__psxw_lift_*` marker).
    Marker,
    /// The PS-X EXE entry point.
    Entry,
    /// Target of a direct call or jump found while translating.
    CallTarget,
    /// A code address materialized in a register (function pointer).
    CodePointer,
    /// A word of initialized data pointing at the first instruction of
    /// code no known function reaches (a function table entry).
    DataPointer,
    /// The return address of a `setjmp` call (BIOS A0h:13h): where a
    /// longjmp to its buffer resumes, with the registers the buffer holds.
    Continuation,
    /// Code only a code pointer reaches that reads registers on entry the
    /// o32 ABI doesn't pass: a block hand-written code jumps into at a
    /// computed offset (an unrolled loop entered part way), sharing the
    /// jumper's registers. A computed jump expands it in place.
    ComputedEntry,
}

#[derive(Debug, Clone, Default)]
pub struct Start {
    pub name: Option<String>,
    pub size: Option<u32>,
    pub sources: BTreeSet<Source>,
    /// Ends the preceding function's extent. Discovered entry points that
    /// are reachable inside an existing function are not boundaries.
    pub boundary: bool,
}

#[derive(Debug, Clone)]
pub struct Code {
    pub id: String,
    pub text: Vec<Range<u32>>,
    /// The container's loaded bytes outside its text (initialized and
    /// read-only data), in guest addresses.
    pub data: Vec<Range<u32>>,
    pub starts: BTreeMap<u32, Start>,
}

/// One function to translate.
#[derive(Debug, Clone)]
pub struct Function {
    pub container: String,
    pub addr: u32,
    pub end: u32,
    pub name: String,
    pub sources: BTreeSet<Source>,
    /// Set for an entry point inside another function's sized extent.
    pub interior_of: Option<u32>,
}

/// Where a code address lives, seen from a given container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Located {
    /// Statically known container.
    Code(String),
    /// In an overlay slot: which overlay is resident decides at run time.
    Dynamic,
    /// A BIOS vector (`0xA0`, `0xB0`, `0xC0`).
    Bios(u8),
    /// Elsewhere in the BIOS kernel's RAM (below `0x10000`).
    Kernel,
}

#[derive(Debug, Clone)]
pub struct Program {
    pub main: String,
    pub containers: BTreeMap<String, Code>,
}

impl Program {
    /// Starts from the symbol files and object boundaries.
    pub fn from_map(map: &AddressMap) -> Self {
        let mut containers = BTreeMap::new();
        for c in &map.containers {
            let mut data = Vec::new();
            for (file, vram) in &c.segments {
                let seg = *vram..vram + (file.end - file.start);
                let mut cuts: Vec<&Range<u32>> = c.text.iter().filter(|t| t.start < seg.end && seg.start < t.end).collect();
                cuts.sort_by_key(|t| t.start);
                let mut at = seg.start;
                for t in cuts {
                    if t.start > at {
                        data.push(at..t.start);
                    }
                    at = at.max(t.end);
                }
                if at < seg.end {
                    data.push(at..seg.end);
                }
            }
            let mut code = Code { id: c.id.clone(), text: c.text.clone(), data, starts: BTreeMap::new() };
            for &a in &c.text_starts {
                code.add(a, None, None, Source::Subsegment, true);
            }
            for e in map.symbols(&c.id) {
                if e.is_func() && code.in_text(e.addr) {
                    code.add(e.addr, Some(&e.name), e.size, Source::Symbol, true);
                }
            }
            containers.insert(c.id.clone(), code);
        }
        Self { main: map.main.clone(), containers }
    }

    pub fn code(&self, container: &str) -> Option<&Code> {
        self.containers.get(container)
    }

    /// Add a start; returns whether it was new. Addresses outside the
    /// container's text are ignored (returns false).
    pub fn add(&mut self, container: &str, addr: u32, name: Option<&str>, source: Source, boundary: bool) -> bool {
        match self.containers.get_mut(container) {
            Some(code) if code.in_text(addr) => code.add(addr, name, None, source, boundary),
            _ => false,
        }
    }

    /// Move the start of the text range that begins at most `within` bytes
    /// after `addr` back to `addr`: the inputs filed the code before it as
    /// data. Returns whether a range moved.
    pub fn extend_text(&mut self, container: &str, addr: u32, within: u32) -> bool {
        let Some(code) = self.containers.get_mut(container) else { return false };
        let Some(t) = code.text.iter_mut().find(|t| t.start > addr && t.start - addr <= within) else { return false };
        let end = std::mem::replace(&mut t.start, addr);
        code.data.retain_mut(|d| {
            if d.start <= addr && d.end == end {
                d.end = addr;
            }
            d.start < d.end
        });
        true
    }

    /// Mark an existing start with another source; returns whether it is
    /// new to that start.
    pub fn add_source(&mut self, container: &str, addr: u32, source: Source) -> bool {
        self.containers.get_mut(container).and_then(|c| c.starts.get_mut(&addr)).is_some_and(|s| s.sources.insert(source))
    }

    /// Where `addr` lives as seen from code in `from`.
    pub fn locate(&self, from: &str, addr: u32) -> Option<Located> {
        if self.containers.get(from).is_some_and(|c| c.in_text(addr)) {
            return Some(Located::Code(from.to_string()));
        }
        if self.containers.get(&self.main).is_some_and(|c| c.in_text(addr)) {
            return Some(Located::Code(self.main.clone()));
        }
        if self.containers.values().any(|c| c.in_text(addr)) {
            return Some(Located::Dynamic);
        }
        match addr & 0x1FFF_FFFF {
            a @ (0xA0 | 0xB0 | 0xC0) => Some(Located::Bios(a as u8)),
            a if a < 0x1_0000 && a != 0 => Some(Located::Kernel),
            _ => None,
        }
    }

    /// Every function of a container with its extent.
    pub fn functions(&self, container: &str) -> Vec<Function> {
        let Some(code) = self.containers.get(container) else { return Vec::new() };
        let sized: Vec<(u32, u32)> = code
            .starts
            .iter()
            .filter_map(|(&a, s)| s.size.filter(|&n| n > 0).map(|n| (a, a + n)))
            .collect();
        let in_sized = |a: u32| sized.iter().find(|&&(s, e)| s < a && a < e).copied();
        let run_end = |a: u32| code.text.iter().find(|r| r.contains(&a)).map_or(a, |r| r.end);
        let boundaries: Vec<u32> =
            code.starts.iter().filter(|(a, s)| s.boundary && in_sized(**a).is_none()).map(|(a, _)| *a).collect();
        // Extents of boundary functions.
        let mut ends: BTreeMap<u32, u32> = BTreeMap::new();
        for (i, &a) in boundaries.iter().enumerate() {
            let end = match code.starts[&a].size.filter(|&n| n > 0) {
                Some(n) => a + n,
                None => boundaries.get(i + 1).copied().unwrap_or(u32::MAX),
            };
            ends.insert(a, end.min(run_end(a)));
        }

        let mut out = Vec::new();
        for (&addr, start) in &code.starts {
            let (end, interior_of) = if let Some(&e) = ends.get(&addr) {
                (e, None)
            } else if let Some((s, e)) = in_sized(addr) {
                (e, Some(s))
            } else {
                match ends.range(..addr).next_back().filter(|&(_, &e)| e > addr) {
                    Some((&s, &e)) => (e, Some(s)),
                    None => (run_end(addr), None),
                }
            };
            out.push(Function {
                container: container.to_string(),
                addr,
                end,
                name: start.name.clone().unwrap_or_else(|| format!("func_{addr:08X}")),
                sources: start.sources.clone(),
                interior_of,
            });
        }
        out
    }
}

impl Code {
    pub fn in_text(&self, addr: u32) -> bool {
        addr & 3 == 0 && self.text.iter().any(|r| r.contains(&addr))
    }

    fn add(&mut self, addr: u32, name: Option<&str>, size: Option<u32>, source: Source, boundary: bool) -> bool {
        let new = !self.starts.contains_key(&addr);
        let s = self.starts.entry(addr).or_default();
        s.boundary |= boundary;
        if s.name.is_none() {
            s.name = name.map(str::to_string);
        }
        if s.size.is_none() {
            s.size = size;
        }
        s.sources.insert(source);
        new
    }
}

/// C symbol of the translation of a function.
pub fn translated_symbol(main: &str, container: &str, addr: u32) -> String {
    if container == main { format!("psxw_t_{addr:08X}") } else { format!("psxw_t_{container}_{addr:08X}") }
}

/// Symbol translated code calls for a statically located function. The
/// linker binds it to the translation or to the C function's o32 entry.
pub fn call_symbol(main: &str, container: &str, addr: u32) -> String {
    if container == main { format!("psxw_f_{addr:08X}") } else { format!("psxw_f_{container}_{addr:08X}") }
}

/// Symbol for a call into an overlay slot from outside it; the linker binds
/// it to a stub that dispatches on the resident code.
pub fn dynamic_symbol(addr: u32) -> String {
    format!("psxw_d_{addr:08X}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program(starts: &[(u32, Option<u32>)]) -> Program {
        let mut code = Code { id: "exe".into(), text: vec![0x8001_0000..0x8001_0100], data: vec![], starts: BTreeMap::new() };
        for &(a, size) in starts {
            code.add(a, None, size, Source::Symbol, true);
        }
        Program { main: "exe".into(), containers: [("exe".to_string(), code)].into() }
    }

    #[test]
    fn extents_and_interiors() {
        let p = program(&[(0x8001_0000, None), (0x8001_0010, Some(0x20)), (0x8001_0018, None), (0x8001_0040, None)]);
        let f: Vec<_> = p.functions("exe").iter().map(|f| (f.addr, f.end, f.interior_of)).collect();
        assert_eq!(
            f,
            vec![
                (0x8001_0000, 0x8001_0010, None),
                (0x8001_0010, 0x8001_0030, None),
                (0x8001_0018, 0x8001_0030, Some(0x8001_0010)),
                (0x8001_0040, 0x8001_0100, None),
            ]
        );
    }

    #[test]
    fn locate() {
        let p = program(&[(0x8001_0000, None)]);
        assert_eq!(p.locate("exe", 0x8001_0004), Some(Located::Code("exe".into())));
        assert_eq!(p.locate("exe", 0x0000_00B0), Some(Located::Bios(0xB0)));
        assert_eq!(p.locate("exe", 0x8000_00A0), Some(Located::Bios(0xA0)));
        assert_eq!(p.locate("exe", 0x8002_0000), None);
    }
}
