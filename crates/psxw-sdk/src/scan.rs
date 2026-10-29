//! The hardware-reach analysis over an SDK's relocatable library objects
//! (plans/unified-adaptor-layer, phase 1 task 2).
//!
//! Per function: the chips its own instructions touch, through constant
//! addresses, register pointers held in data (initialised, or stored there
//! at run time by any function) and kernel-RAM pointers read at run time;
//! its calls, by relocation across libraries and through function-pointer
//! tables; the library data it reads and writes. A linear pass per
//! function, repeated until no new register pointers are learned.

use crate::elf::{self, Object, Reloc};
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

/// What an address is, on the PS1's bus.
pub fn chip(a: u32) -> Option<&'static str> {
    if a == 0xFFFE_0130 {
        return Some("cachectl");
    }
    if !matches!(a >> 29, 0 | 4 | 5) {
        return None;
    }
    let p = a & 0x1FFF_FFFF;
    Some(match p {
        0x1F80_1000..0x1F80_1024 | 0x1F80_1060..0x1F80_1064 => "memctrl",
        0x1F80_1040..0x1F80_1050 => "SIO0",
        0x1F80_1050..0x1F80_1060 => "SIO1",
        0x1F80_1070..0x1F80_1078 => "IRQ",
        0x1F80_1080..0x1F80_1100 => "DMA",
        0x1F80_1100..0x1F80_1130 => "timers",
        0x1F80_1800..0x1F80_1804 => "CD",
        0x1F80_1810..0x1F80_1818 => "GPU",
        0x1F80_1820..0x1F80_1828 => "MDEC",
        0x1F80_1C00..0x1F80_2000 => "SPU",
        0x1F80_2000..0x1F80_3000 => "exp2",
        0x1F00_0000..0x1F80_0000 => "exp1",
        0x1FC0_0000..0x1FC8_0000 => "rom",
        1..0x1_0000 => "kernel",
        _ => return None,
    })
}

/// The chips that are hardware (not BIOS RAM or ROM).
pub fn is_hardware(chip: &str) -> bool {
    !matches!(chip, "kernel" | "rom")
}

/// One library object.
pub struct Member {
    pub lib: String,
    /// The member's name without its archive index: `sys` for `3-sys.o`.
    pub name: String,
    pub obj: Object,
}

/// A function's facts.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Function {
    pub lib: String,
    pub member: String,
    /// Offset in its text section.
    pub off: u32,
    pub section: usize,
    pub exported: bool,
    /// Chips its own instructions touch, with the access count.
    pub direct: BTreeMap<String, u32>,
    /// Direct calls and resolved calls through pointers.
    pub calls: BTreeSet<String>,
    /// It calls through a pointer the pass couldn't resolve.
    pub indirect: bool,
    /// Library data (by symbol) it loads from, and stores to.
    pub reads: BTreeSet<String>,
    pub writes: BTreeSet<String>,
    /// BIOS functions it calls through the A0h/B0h/C0h vectors (`B0:57`;
    /// `B0:?` when the number isn't a constant).
    pub bios_calls: BTreeSet<String>,
}

/// The whole SDK's analysis.
#[derive(Debug, Default, Serialize)]
pub struct Scan {
    pub objects: usize,
    pub functions: BTreeMap<String, Function>,
    /// Functions whose address each library takes (the targets of its
    /// unresolved indirect calls).
    pub taken: BTreeMap<String, BTreeSet<String>>,
    /// Register pointers in data: location → address.
    pub pointers: BTreeMap<String, String>,
}

/// A memory location: an object's section and offset, or a symbol defined
/// elsewhere (`COMMON` or undefined).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Loc {
    Extern(String),
    Common(String),
    At(usize, usize, Option<i64>),
}

/// A function: (object, text section, offset).
type FKey = (usize, usize, u32);

#[derive(Debug, Clone, PartialEq)]
enum Val {
    /// A constant.
    C(u32),
    /// A relocation symbol of the current object, plus a constant (when
    /// known).
    A(u32, Option<i64>),
    /// A register pointer: its address when known, its chip.
    Io(Option<u32>, Option<&'static str>),
    /// A pointer read from kernel RAM.
    Kernel,
    /// A function pointer.
    Fn(FKey),
}

const LOADS: [u32; 8] = [0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x32];
const STORES: [u32; 6] = [0x28, 0x29, 0x2A, 0x2B, 0x2E, 0x3A];
/// Registers a call may change.
const CLOBBER: [usize; 18] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 24, 25, 31];

/// Load every `<lib>/<NN>-<member>.o` under `dir`.
pub fn load(dir: &Path) -> Result<Vec<Member>> {
    let mut out = Vec::new();
    let mut libs: Vec<_> = std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))?.filter_map(|e| e.ok()).collect();
    libs.sort_by_key(|e| e.file_name());
    for l in libs.into_iter().filter(|e| e.path().is_dir()) {
        let lib = l.file_name().to_string_lossy().into_owned();
        if lib == "standalone" {
            continue;
        }
        let mut files: Vec<_> = std::fs::read_dir(l.path())?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "o")).collect();
        files.sort();
        for p in files {
            let stem = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            let name = stem.split_once('-').map_or(stem.as_str(), |(_, n)| n).to_string();
            out.push(Member { lib: lib.clone(), name, obj: Object::read(&p)? });
        }
    }
    Ok(out)
}

struct Ctx<'a> {
    objs: &'a [Member],
    /// Global definitions: name → (object, section, value), or COMMON.
    gdef: HashMap<String, Option<(usize, usize, u32)>>,
    /// Function starts per (object, text section).
    starts: BTreeMap<(usize, usize), BTreeSet<u32>>,
    names: HashMap<FKey, (String, bool)>,
    /// Register pointers kept in data.
    ptr: BTreeMap<Loc, u32>,
    /// Function pointers kept in data.
    fnptr: HashMap<Loc, FKey>,
    /// Data symbols per (object, section), sorted.
    datasyms: HashMap<(usize, usize), Vec<(u32, String)>>,
}

impl Ctx<'_> {
    fn obj(&self, oi: usize) -> &Object {
        &self.objs[oi].obj
    }

    fn loc(&self, oi: usize, sym: u32, off: Option<i64>) -> Loc {
        let s = &self.obj(oi).symbols[sym as usize];
        if s.shndx == 0 || s.shndx == elf::SHN_COMMON {
            return match self.gdef.get(&s.name) {
                None => Loc::Extern(s.name.clone()),
                Some(None) => Loc::Common(s.name.clone()),
                Some(Some((o, sh, v))) => Loc::At(*o, *sh, off.map(|x| *v as i64 + x)),
            };
        }
        let base = if s.ty == elf::STT_SECTION { 0 } else { s.value as i64 };
        Loc::At(oi, s.shndx as usize, off.map(|x| base + x))
    }

    fn text_target(&self, oi: usize, sym: u32, addend: i64) -> Option<FKey> {
        let o = self.obj(oi);
        let s = &o.symbols[sym as usize];
        if s.shndx == 0 {
            let (d, sh, v) = (*self.gdef.get(&s.name)?)?;
            return self.obj(d).is_text(sh).then_some((d, sh, (v as i64 + addend) as u32));
        }
        let sh = s.shndx as usize;
        if !o.is_text(sh) {
            return None;
        }
        let base = if s.ty == elf::STT_SECTION { 0 } else { s.value as i64 };
        Some((oi, sh, (base + addend) as u32))
    }

    fn fkey(&self, k: FKey) -> String {
        match self.names.get(&k) {
            Some((n, true)) => n.clone(),
            Some((n, false)) => format!("{}/{}:{n}", self.objs[k.0].lib, self.objs[k.0].name),
            None => format!("{}/{}:{:#x}", self.objs[k.0].lib, self.objs[k.0].name, k.2),
        }
    }

    fn is_start(&self, k: FKey) -> bool {
        self.starts.get(&(k.0, k.1)).is_some_and(|s| s.contains(&k.2))
    }

    /// The data object a location belongs to: its symbol, else the exact
    /// location.
    fn state_key(&self, l: &Loc) -> Option<String> {
        match l {
            Loc::Extern(_) => None,
            Loc::Common(n) => Some(n.clone()),
            Loc::At(oi, sh, off) => {
                let off = (*off)?;
                if self.obj(*oi).is_text(*sh) {
                    return None;
                }
                let m = &self.objs[*oi];
                let best = self.datasyms.get(&(*oi, *sh)).and_then(|v| v.iter().rev().find(|(o, _)| *o as i64 <= off));
                Some(match best {
                    Some((_, n)) if !n.is_empty() => format!("{}/{}:{n}", m.lib, m.name),
                    _ => format!("{}/{}:{}+{off:#x}", m.lib, m.name, m.obj.sections[*sh].name),
                })
            }
        }
    }
}

pub fn scan(objs: &[Member]) -> Scan {
    let mut cx = Ctx {
        objs,
        gdef: HashMap::new(),
        starts: BTreeMap::new(),
        names: HashMap::new(),
        ptr: BTreeMap::new(),
        fnptr: HashMap::new(),
        datasyms: HashMap::new(),
    };
    for (oi, m) in objs.iter().enumerate() {
        for s in m.obj.symbols.iter().filter(|s| s.bind == elf::STB_GLOBAL && !s.name.is_empty()) {
            if s.shndx == elf::SHN_COMMON {
                cx.gdef.entry(s.name.clone()).or_insert(None);
            } else if s.shndx > 0 && s.shndx < 0xFF00 && !matches!(cx.gdef.get(&s.name), Some(Some(_))) {
                cx.gdef.insert(s.name.clone(), Some((oi, s.shndx as usize, s.value)));
            }
        }
    }
    // Register pointers: initialised data words in the I/O window.
    for (oi, m) in objs.iter().enumerate() {
        for i in (0..m.obj.sections.len()).filter(|&i| m.obj.is_data(i)) {
            let d = m.obj.data(i);
            let relocated: BTreeSet<u32> = m.obj.relocs.get(&i).into_iter().flatten().map(|r| r.offset).collect();
            for off in (0..d.len().saturating_sub(3)).step_by(4) {
                let w = m.obj.word(i, off as u32);
                if !relocated.contains(&(off as u32)) && matches!(w >> 29, 0 | 4 | 5) && (0x1F80_1000..0x1F80_3000).contains(&(w & 0x1FFF_FFFF)) {
                    cx.ptr.insert(Loc::At(oi, i, Some(off as i64)), w);
                }
            }
        }
    }
    // Function starts: named text symbols, then local call targets.
    for (oi, m) in objs.iter().enumerate() {
        let o = &m.obj;
        for s in &o.symbols {
            if o.is_text(s.shndx as usize) && matches!(s.ty, 0 | 2) && !s.name.is_empty() && !s.name.starts_with('.') {
                cx.starts.entry((oi, s.shndx as usize)).or_default().insert(s.value);
                cx.names.entry((oi, s.shndx as usize, s.value)).or_insert((s.name.clone(), s.bind == elf::STB_GLOBAL));
            }
        }
        for (&ti, rels) in o.relocs.iter().filter(|(t, _)| o.is_text(**t)) {
            for r in rels.iter().filter(|r| r.ty == elf::R_MIPS_26) {
                let w = o.word(ti, r.offset);
                let s = &o.symbols[r.sym as usize];
                if w >> 26 == 3 && s.shndx as usize == ti && s.ty == elf::STT_SECTION {
                    cx.starts.entry((oi, ti)).or_default().insert((w & 0x3FF_FFFF) << 2);
                }
            }
        }
    }
    // Function pointers: data words relocated to text (.rdata holds mostly
    // switch tables), and `addiu` of a text address.
    let mut taken: BTreeMap<String, BTreeSet<FKey>> = BTreeMap::new();
    for (oi, m) in objs.iter().enumerate() {
        let o = &m.obj;
        for (&sec, rels) in &o.relocs {
            if o.is_data(sec) {
                for r in rels.iter().filter(|r| r.ty == elf::R_MIPS_32) {
                    let addend = o.word(sec, r.offset) as i32 as i64;
                    if let Some(t) = cx.text_target(oi, r.sym, addend) {
                        cx.fnptr.insert(Loc::At(oi, sec, Some(r.offset as i64)), t);
                        if o.sections[sec].name != ".rdata" {
                            cx.starts.entry((t.0, t.1)).or_default().insert(t.2);
                            taken.entry(m.lib.clone()).or_default().insert(t);
                        }
                    }
                }
            } else if o.is_text(sec) {
                for r in rels.iter().filter(|r| r.ty == elf::R_MIPS_LO16) {
                    let w = o.word(sec, r.offset);
                    if w >> 26 == 0x09 {
                        if let Some(t) = cx.text_target(oi, r.sym, w as u16 as i16 as i64) {
                            cx.starts.entry((t.0, t.1)).or_default().insert(t.2);
                            taken.entry(m.lib.clone()).or_default().insert(t);
                        }
                    }
                }
            }
        }
    }
    for (oi, m) in objs.iter().enumerate() {
        for s in &m.obj.symbols {
            let sh = s.shndx as usize;
            if s.shndx > 0 && s.shndx < 0xFF00 && !m.obj.is_text(sh) && !s.name.is_empty() && s.ty != elf::STT_SECTION {
                cx.datasyms.entry((oi, sh)).or_default().push((s.value, s.name.clone()));
            }
        }
    }
    for v in cx.datasyms.values_mut() {
        v.sort();
    }

    let mut functions = BTreeMap::new();
    for _ in 0..5 {
        let (f, learned) = scan_all(&mut cx);
        functions = f;
        if !learned {
            break;
        }
    }
    let taken = taken.into_iter().map(|(l, ts)| (l, ts.into_iter().map(|t| cx.fkey(t)).collect())).collect();
    let pointers = cx
        .ptr
        .iter()
        .map(|(l, v)| {
            let k = match l {
                Loc::At(oi, sh, off) => format!("{}/{}:{}+{:#x}", objs[*oi].lib, objs[*oi].name, objs[*oi].obj.sections[*sh].name, off.unwrap_or(0)),
                Loc::Common(n) | Loc::Extern(n) => n.clone(),
            };
            (k, format!("{v:#010x}"))
        })
        .collect();
    Scan { objects: objs.len(), functions, taken, pointers }
}

/// One pass over every function; whether it learned new register pointers.
fn scan_all(cx: &mut Ctx) -> (BTreeMap<String, Function>, bool) {
    let mut learned = false;
    let mut out: BTreeMap<String, Function> = BTreeMap::new();
    let starts: Vec<((usize, usize), Vec<u32>)> = cx.starts.iter().map(|(k, v)| (*k, v.iter().copied().collect())).collect();
    for ((oi, sh), offs) in starts {
        let m = &cx.objs[oi];
        let o = &m.obj;
        let d = o.data(sh);
        let rel: HashMap<u32, Reloc> = o.relocs.get(&sh).into_iter().flatten().map(|r| (r.offset, *r)).collect();
        for (k, &start) in offs.iter().enumerate() {
            let end = offs.get(k + 1).copied().unwrap_or(d.len() as u32);
            let key = cx.fkey((oi, sh, start));
            let mut f = Function {
                lib: m.lib.clone(),
                member: m.name.clone(),
                off: start,
                section: sh,
                exported: cx.names.get(&(oi, sh, start)).is_some_and(|n| n.1),
                ..Default::default()
            };
            let mut reg: [Option<Val>; 32] = Default::default();
            reg[0] = Some(Val::C(0));
            let mut pending = false;
            // The last constant put in $t1, the BIOS function number: set
            // before calls the analysis treats as clobbering it.
            let mut t1: Option<u32> = None;
            // A value the call now in its delay slot returns in $v0.
            let mut returns: Option<Val> = None;
            let mut pc = start;
            while pc + 4 <= end {
                let w = o.word(sh, pc);
                let (op, rs, rt, rd) = (w >> 26, ((w >> 21) & 31) as usize, ((w >> 16) & 31) as usize, ((w >> 11) & 31) as usize);
                let imm = w & 0xFFFF;
                let simm = imm as u16 as i16 as i64;
                let r = rel.get(&pc).copied();
                let clobber = std::mem::take(&mut pending);
                let set = |reg: &mut [Option<Val>; 32], i: usize, v: Option<Val>| {
                    if i != 0 {
                        reg[i] = v;
                    }
                };
                let funct = w & 63;
                if op == 0x0F {
                    let v = match r {
                        Some(r) if r.ty == elf::R_MIPS_HI16 => Val::A(r.sym, Some((imm as i64) << 16)),
                        _ => Val::C(imm << 16),
                    };
                    set(&mut reg, rt, Some(v));
                } else if matches!(op, 0x08 | 0x09 | 0x0D) {
                    let b = reg[rs].clone();
                    let v = match (r, b) {
                        (Some(r), b) if r.ty == elf::R_MIPS_LO16 => {
                            let hi = match b {
                                Some(Val::A(s, Some(h))) if s == r.sym => h,
                                _ => 0,
                            };
                            Some(Val::A(r.sym, Some(hi + simm)))
                        }
                        (_, Some(Val::C(c))) => Some(Val::C(if op == 0x0D { c | imm } else { (c as i64 + simm) as u32 })),
                        (_, Some(Val::Io(a, ch))) => Some(Val::Io(a.map(|a| (a as i64 + simm) as u32), ch)),
                        (_, Some(Val::A(s, x))) => Some(Val::A(s, x.map(|x| x + simm))),
                        (_, Some(Val::Kernel)) => Some(Val::Kernel),
                        _ => None,
                    };
                    set(&mut reg, rt, v);
                } else if op == 0 && matches!(funct, 0x20 | 0x21 | 0x25) {
                    let (a, b) = (reg[rs].clone(), reg[rt].clone());
                    let v = match (a, b) {
                        (Some(Val::C(0)), b) => b,
                        (a, Some(Val::C(0))) => a,
                        (Some(Val::Io(_, ch)), _) | (_, Some(Val::Io(_, ch))) => Some(Val::Io(None, ch)),
                        (Some(Val::Kernel), _) | (_, Some(Val::Kernel)) => Some(Val::Kernel),
                        (Some(Val::C(x)), Some(Val::C(y))) => Some(Val::C(x.wrapping_add(y))),
                        _ => None,
                    };
                    set(&mut reg, rd, v);
                } else if LOADS.contains(&op) || STORES.contains(&op) {
                    let b = reg[rs].clone();
                    let mut target: Option<Loc> = None;
                    let mut addr: Option<u32> = None;
                    match (r, &b) {
                        (Some(r), _) if r.ty == elf::R_MIPS_LO16 => {
                            let hi = match &b {
                                Some(Val::A(s, h)) if *s == r.sym => *h,
                                _ if o.symbols[r.sym as usize].ty != elf::STT_SECTION => Some(0),
                                _ => None,
                            };
                            target = Some(cx.loc(oi, r.sym, hi.map(|h| h + simm)));
                        }
                        (_, Some(Val::A(s, x))) => target = Some(cx.loc(oi, *s, x.map(|x| x + simm))),
                        (_, Some(Val::C(c))) => addr = Some((*c as i64 + simm) as u32),
                        (_, Some(Val::Io(a, ch))) => {
                            let c = a.and_then(|a| chip((a as i64 + simm) as u32)).or(*ch).unwrap_or("io");
                            *f.direct.entry(c.into()).or_default() += 1;
                        }
                        (_, Some(Val::Kernel)) => *f.direct.entry("kernel".into()).or_default() += 1,
                        _ => {}
                    }
                    let load = LOADS.contains(&op);
                    if let Some(k) = target.as_ref().and_then(|t| cx.state_key(t)) {
                        if load { f.reads.insert(k) } else { f.writes.insert(k) };
                    }
                    let mut kernel_load = false;
                    if let Some(a) = addr {
                        if let Some(c) = chip(a) {
                            // Kernel RAM only through an absolute address:
                            // $zero, or a lui of a KSEG base.
                            let absolute = rs == 0 || matches!(b, Some(Val::C(c)) if matches!(c >> 16, 0x8000 | 0xA000));
                            if c != "kernel" || absolute {
                                *f.direct.entry(c.into()).or_default() += 1;
                                kernel_load = c == "kernel";
                            }
                        }
                    }
                    if load {
                        let mut v = None;
                        if let Some(t) = target.as_ref().filter(|t| !matches!(t, Loc::Extern(_))) {
                            if let Some(&p) = cx.ptr.get(t) {
                                v = Some(Val::Io(Some(p), chip(p)));
                            } else if let Some(&fk) = cx.fnptr.get(t) {
                                if cx.is_start(fk) {
                                    v = Some(Val::Fn(fk));
                                }
                            }
                        }
                        if kernel_load || matches!(b, Some(Val::Kernel)) {
                            v = Some(Val::Kernel);
                        }
                        if op != 0x32 {
                            set(&mut reg, rt, v);
                        }
                    } else if let Some(t) = target.filter(|t| matches!(t, Loc::At(_, _, Some(_)) | Loc::Common(_))) {
                        // A register pointer stored to memory.
                        let val = match &reg[rt] {
                            Some(Val::Io(Some(a), _)) => Some(*a),
                            Some(Val::C(c)) if chip(*c).is_some_and(|c| c != "kernel") => Some(*c),
                            _ => None,
                        };
                        if let Some(val) = val {
                            if !cx.ptr.contains_key(&t) {
                                cx.ptr.insert(t, val);
                                learned = true;
                            }
                        }
                    }
                } else if matches!(op, 2 | 3) && r.is_some_and(|r| r.ty == elf::R_MIPS_26) {
                    let r = r.unwrap_or(Reloc { offset: 0, ty: 0, sym: 0 });
                    let s = &o.symbols[r.sym as usize];
                    let t = if s.shndx == 0 {
                        cx.gdef.get(&s.name).copied().flatten().filter(|(d, sh, _)| cx.obj(*d).is_text(*sh)).map(|(d, sh, v)| (d, sh, v))
                    } else if o.is_text(s.shndx as usize) {
                        let off = if s.ty == elf::STT_SECTION { (w & 0x3FF_FFFF) << 2 } else { s.value };
                        Some((oi, s.shndx as usize, off))
                    } else {
                        None
                    };
                    if let Some(t) = t {
                        let name = cx.fkey(t);
                        if op == 3 || name != key {
                            f.calls.insert(name);
                        }
                    }
                    if op == 3 {
                        pending = true;
                    }
                } else if op == 0 && matches!(funct, 8 | 9) && rs != 31 && matches!(reg[rs], Some(Val::C(0xA0 | 0xB0 | 0xC0))) {
                    // A BIOS call through its vector. GetC0Table and
                    // GetB0Table (B0h:56h, 57h) return kernel tables.
                    let Some(Val::C(table)) = reg[rs] else { unreachable!() };
                    f.bios_calls.insert(match t1 {
                        Some(n) => format!("{table:02X}:{n:02X}"),
                        None => format!("{table:02X}:?"),
                    });
                    if table == 0xB0 && matches!(t1, Some(0x56 | 0x57)) {
                        returns = Some(Val::Kernel);
                    }
                    if funct == 9 {
                        pending = true;
                    }
                } else if op == 0 && matches!(funct, 8 | 9) && rs != 31 {
                    let t = match &reg[rs] {
                        Some(Val::Fn(k)) => Some(cx.fkey(*k)),
                        Some(Val::A(s, Some(x))) => cx.text_target(oi, *s, *x).filter(|t| cx.is_start(*t)).map(|t| cx.fkey(t)),
                        Some(Val::Kernel) => {
                            *f.direct.entry("kernel".into()).or_default() += 1;
                            None
                        }
                        // Into BIOS code in kernel RAM (code a patch put there).
                        Some(Val::C(a)) if chip(*a) == Some("kernel") => {
                            *f.direct.entry("kernel".into()).or_default() += 1;
                            None
                        }
                        _ => None,
                    };
                    match t {
                        Some(t) => {
                            f.calls.insert(t);
                        }
                        None if funct == 9 => f.indirect = true,
                        None => {}
                    }
                    if funct == 9 {
                        pending = true;
                    }
                }
                if rt == 9 && matches!(op, 0x09 | 0x0D) && rs == 0 {
                    t1 = Some(imm);
                }
                if clobber {
                    for i in CLOBBER {
                        reg[i] = None;
                    }
                    reg[2] = returns.take();
                }
                pc += 4;
            }
            out.insert(key, f);
        }
    }
    (out, learned)
}

impl Scan {
    /// What each function reaches through its own library's code
    /// (hardware chips, and `kernel`/`rom` for BIOS internals): directly,
    /// by calls, or by its unresolved indirect calls to the functions its
    /// library takes the address of.
    pub fn reach(&self) -> BTreeMap<&str, BTreeSet<String>> {
        let mut reach: BTreeMap<&str, BTreeSet<String>> =
            self.functions.iter().map(|(k, f)| (k.as_str(), f.direct.keys().cloned().collect())).collect();
        let edges: BTreeMap<&str, Vec<&str>> = self
            .functions
            .iter()
            .map(|(k, f)| {
                let may = f.indirect.then(|| self.taken.get(&f.lib)).flatten().into_iter().flatten();
                let callees = f
                    .calls
                    .iter()
                    .chain(may)
                    .filter(|c| *c != k && self.functions.get(*c).is_some_and(|g| g.lib == f.lib))
                    .map(String::as_str)
                    .collect();
                (k.as_str(), callees)
            })
            .collect();
        loop {
            let mut changed = false;
            for (k, cs) in &edges {
                let add: BTreeSet<String> = cs.iter().flat_map(|c| reach[c].iter()).filter(|c| !reach[k].contains(*c)).cloned().collect();
                if !add.is_empty() {
                    reach.get_mut(k).map(|r| r.extend(add));
                    changed = true;
                }
            }
            if !changed {
                break reach;
            }
        }
    }

    /// State accessors: functions that reach no hardware but share
    /// mutable library state with ones that do. State is data a
    /// hardware-reaching function of the library accesses and some
    /// function of the library writes; an accessor reads it, or writes
    /// what a hardware-reaching function reads.
    pub fn state_accessors(&self, reach: &BTreeMap<&str, BTreeSet<String>>, libs: &BTreeSet<&str>) -> BTreeSet<&str> {
        let mut written: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        let mut hw_reads: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        let mut hw_any: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for (k, f) in &self.functions {
            written.entry(&f.lib).or_default().extend(f.writes.iter().map(String::as_str));
            if !reach[k.as_str()].is_empty() {
                hw_reads.entry(&f.lib).or_default().extend(f.reads.iter().map(String::as_str));
                hw_any.entry(&f.lib).or_default().extend(f.reads.iter().chain(&f.writes).map(String::as_str));
            }
        }
        let empty = BTreeSet::new();
        self.functions
            .iter()
            .filter(|(k, f)| reach[k.as_str()].is_empty() && libs.contains(f.lib.as_str()))
            .filter(|(_, f)| {
                let w = written.get(f.lib.as_str()).unwrap_or(&empty);
                let any = hw_any.get(f.lib.as_str()).unwrap_or(&empty);
                let reads = hw_reads.get(f.lib.as_str()).unwrap_or(&empty);
                f.reads.iter().any(|d| any.contains(d.as_str()) && w.contains(d.as_str())) || f.writes.iter().any(|d| reads.contains(d.as_str()))
            })
            .map(|(k, _)| k.as_str())
            .collect()
    }
}
