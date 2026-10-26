//! Whole-game RAM comparison by symbol (plans/M03-oracle-harness.md,
//! "Whole-game RAM oracle"). Two 2 MiB dumps taken at the same value of
//! the game's frame counter are compared byte for byte; each differing byte
//! is attributed to the symbol covering it (the main container's, or the
//! resident overlay's), or to a 4 KiB page when no symbol covers it.
//! Excluded ranges (kernel RAM, the stack, manifest exclusions with their
//! reasons) are counted but never reported as differences.

use serde::Serialize;
use std::ops::Range;

pub const RAM_SIZE: usize = 0x20_0000;
/// The kernel's RAM: the runtime's HLE BIOS and OpenBIOS keep different
/// state there by construction.
pub const KERNEL: Range<u32> = 0..0x1_0000;
/// An unsized symbol covers at most this much (up to the next symbol).
pub const UNSIZED_MAX: u32 = 0x1_0000;
/// Differing symbols kept per point in the report.
pub const MAX_SYMBOLS: usize = 64;

/// A named range of physical RAM offsets (`addr & 0x1FFFFF`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sym {
    pub name: String,
    pub range: Range<u32>,
}

#[derive(Debug, Clone)]
pub struct OverlayLayout {
    pub id: String,
    /// Physical offsets the overlay's segments occupy.
    pub range: Range<u32>,
    /// The overlay is resident when these bytes are at this offset.
    pub probe_at: u32,
    pub probe: Vec<u8>,
    pub syms: Vec<Sym>,
}

#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub main: Vec<Sym>,
    pub overlays: Vec<OverlayLayout>,
    /// Ranges left out, with the reason.
    pub excluded: Vec<(Range<u32>, String)>,
}

/// Symbols from `(name, addr, size)`, sorted; an unsized symbol extends to
/// the next one, at most [`UNSIZED_MAX`] bytes.
pub fn syms(mut entries: Vec<(String, u32, Option<u32>)>) -> Vec<Sym> {
    entries.sort_by_key(|e| (e.1 & 0x1F_FFFF, std::cmp::Reverse(e.2)));
    entries.dedup_by_key(|e| e.1 & 0x1F_FFFF);
    let mut out = Vec::with_capacity(entries.len());
    for (i, (name, addr, size)) in entries.iter().enumerate() {
        let start = addr & 0x1F_FFFF;
        let next = entries.get(i + 1).map_or(RAM_SIZE as u32, |e| e.1 & 0x1F_FFFF);
        let end = match size {
            Some(s) if *s > 0 => start.saturating_add(*s),
            _ => next.min(start.saturating_add(UNSIZED_MAX)),
        };
        if end > start {
            out.push(Sym { name: name.clone(), range: start..end.min(RAM_SIZE as u32) });
        }
    }
    out
}

fn find(syms: &[Sym], off: u32) -> Option<&Sym> {
    let i = syms.partition_point(|s| s.range.start <= off);
    syms[..i].iter().rev().take(8).find(|s| s.range.contains(&off))
}

impl Layout {
    /// Overlays whose probe bytes are in `ram`.
    pub fn resident(&self, ram: &[u8]) -> Vec<usize> {
        (0..self.overlays.len())
            .filter(|&i| {
                let o = &self.overlays[i];
                let at = o.probe_at as usize;
                !o.probe.is_empty() && ram.get(at..at + o.probe.len()) == Some(o.probe.as_slice())
            })
            .collect()
    }

    fn excluded(&self, off: u32) -> bool {
        self.excluded.iter().any(|(r, _)| r.contains(&off))
    }

    fn owner(&self, off: u32, resident: &[usize]) -> (String, String, Range<u32>) {
        for &i in resident {
            let o = &self.overlays[i];
            if o.range.contains(&off) {
                return match find(&o.syms, off) {
                    Some(s) => (o.id.clone(), s.name.clone(), s.range.clone()),
                    None => (o.id.clone(), page_name(off), gap(&o.syms, off)),
                };
            }
        }
        match find(&self.main, off) {
            Some(s) => ("main".into(), s.name.clone(), s.range.clone()),
            None => ("-".into(), page_name(off), gap(&self.main, off)),
        }
    }
}

/// From `off` to the end of its page or the next symbol, whichever is first.
fn gap(syms: &[Sym], off: u32) -> Range<u32> {
    let next = syms.get(syms.partition_point(|s| s.range.start <= off)).map_or(u32::MAX, |s| s.range.start);
    off..((off & !0xFFF) + 0x1000).min(next)
}

fn page_name(off: u32) -> String {
    format!("<page 0x{:08X}>", 0x8000_0000 | (off & !0xFFF))
}

#[derive(Debug, Clone, Serialize)]
pub struct SymDiff {
    pub container: String,
    pub name: String,
    pub addr: String,
    pub size: u32,
    /// Bytes that differ inside the symbol.
    pub bytes: u32,
    /// Address of the first differing byte and up to 16 bytes from there.
    pub first: String,
    pub a: String,
    pub b: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PointDiff {
    pub counter: u32,
    pub frame_a: u64,
    pub frame_b: u64,
    pub differing_bytes: u64,
    pub excluded_differing_bytes: u64,
    pub differing_symbols: usize,
    pub resident_a: Vec<String>,
    pub resident_b: Vec<String>,
    /// The first differing symbols, by address.
    pub symbols: Vec<SymDiff>,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Compares two dumps at one aligned point.
pub fn diff(layout: &Layout, counter: u32, frame_a: u64, a: &[u8], frame_b: u64, b: &[u8]) -> PointDiff {
    let ra = layout.resident(a);
    let rb = layout.resident(b);
    let mut resident = ra.clone();
    resident.extend(rb.iter().filter(|i| !ra.contains(i)));
    let names = |r: &[usize]| r.iter().map(|&i| layout.overlays[i].id.clone()).collect::<Vec<_>>();
    let mut p = PointDiff {
        counter,
        frame_a,
        frame_b,
        differing_bytes: 0,
        excluded_differing_bytes: 0,
        differing_symbols: 0,
        resident_a: names(&ra),
        resident_b: names(&rb),
        symbols: Vec::new(),
    };
    let n = a.len().min(b.len()).min(RAM_SIZE);
    let mut off = 0usize;
    while off < n {
        if a[off] == b[off] {
            off += 1;
            continue;
        }
        let o = off as u32;
        if layout.excluded(o) {
            p.excluded_differing_bytes += 1;
            off += 1;
            continue;
        }
        let (container, name, range) = layout.owner(o, &resident);
        let end = (range.end as usize).min(n);
        let mut bytes = 0u32;
        for i in off..end {
            if a[i] != b[i] && !layout.excluded(i as u32) {
                bytes += 1;
            }
        }
        p.differing_bytes += bytes as u64;
        p.differing_symbols += 1;
        if p.symbols.len() < MAX_SYMBOLS {
            let shown = off..(off + 16).min(end);
            p.symbols.push(SymDiff {
                container,
                name,
                addr: format!("0x{:08X}", 0x8000_0000 | range.start),
                size: range.end - range.start,
                bytes,
                first: format!("0x{:08X}", 0x8000_0000 | o),
                a: hex(&a[shown.clone()]),
                b: hex(&b[shown]),
            });
        }
        off = end.max(off + 1);
    }
    p
}

/// Aligned points: for each wanted counter value, the first frame each
/// trace holds it (trace index i is frame i + 1).
pub fn align(trace_a: &[u32], trace_b: &[u32], values: &[u32]) -> Vec<(u32, Option<u64>, Option<u64>)> {
    let first = |t: &[u32], v: u32| t.iter().position(|&w| w == v).map(|i| i as u64 + 1);
    values.iter().map(|&v| (v, first(trace_a, v), first(trace_b, v))).collect()
}

/// Counter values to align on: the counter at every `every`-th frame of
/// `trace`, skipping zero and repeats.
pub fn points(trace: &[u32], every: u64) -> Vec<u32> {
    let mut v = Vec::new();
    let mut f = every;
    while f as usize <= trace.len() {
        let w = trace[f as usize - 1];
        if w != 0 && !v.contains(&w) {
            v.push(w);
        }
        f += every;
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout {
            main: syms(vec![("gA".into(), 0x8002_0000, Some(8)), ("gB".into(), 0x8002_0010, None), ("gC".into(), 0x8002_0020, None)]),
            overlays: vec![OverlayLayout {
                id: "ovl".into(),
                range: 0x10_0000..0x10_1000,
                probe_at: 0x10_0000,
                probe: vec![1, 2, 3, 4],
                syms: syms(vec![("ovl_f".into(), 0x8010_0000, Some(0x100))]),
            }],
            excluded: vec![(KERNEL, "kernel".into())],
        }
    }

    #[test]
    fn sizes() {
        let s = layout().main;
        assert_eq!(s[0].range, 0x2_0000..0x2_0008);
        assert_eq!(s[1].range, 0x2_0010..0x2_0020);
        assert_eq!(s[2].range, 0x2_0020..0x3_0020);
    }

    #[test]
    fn attributes_and_excludes() {
        let l = layout();
        let a = vec![0u8; RAM_SIZE];
        let mut b = a.clone();
        b[0x100] = 1; // kernel
        b[0x2_0001] = 1; // gA
        b[0x2_0012] = 1; // gB
        b[0x2_0013] = 1; // gB
        b[0x2_0009] = 1; // between gA and gB: page
        let p = diff(&l, 7, 10, &a, 12, &b);
        assert_eq!(p.excluded_differing_bytes, 1);
        assert_eq!(p.differing_bytes, 4);
        let names: Vec<&str> = p.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["gA", "<page 0x80020000>", "gB"]);
        assert_eq!(p.symbols[2].bytes, 2);
    }

    #[test]
    fn overlay_symbols_when_resident() {
        let l = layout();
        let mut a = vec![0u8; RAM_SIZE];
        a[0x10_0000..0x10_0004].copy_from_slice(&[1, 2, 3, 4]);
        let mut b = a.clone();
        b[0x10_0010] = 9;
        let p = diff(&l, 1, 1, &a, 1, &b);
        assert_eq!(p.resident_a, ["ovl"]);
        assert_eq!(p.symbols[0].name, "ovl_f");
        assert_eq!(p.symbols[0].container, "ovl");
    }

    #[test]
    fn alignment() {
        let a = [0, 0, 1, 1, 2, 2, 3];
        let b = [0, 1, 1, 2, 2, 3, 3];
        assert_eq!(points(&a, 2), [1, 2]);
        assert_eq!(align(&a, &b, &[2, 3, 9]), [(2, Some(5), Some(4)), (3, Some(7), Some(6)), (9, None, None)]);
    }
}
