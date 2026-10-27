//! The DWARF frontend (D55, plans/M12 phase 3): types and layouts as clang
//! compiled them, from a debug build's DWARF (D53, `psxw build --debug`),
//! one compilation unit at a time. Records with every field's offset,
//! bitfields with their bits, typedefs, enums; globals' types by address
//! (a debug build's window puts each global at its own KSEG0 address);
//! defined functions' signatures by name.
//!
//! [`merge`] puts it over the C-header frontend's snapshot: layouts from
//! DWARF, declarations of functions that have no C from the headers.
//! [`agree`] is the frontends' cross-check.

// gimli names its constants as DWARF does.
#![allow(non_upper_case_globals)]

use crate::snapshot::{Field, Param, Provenance, Signature, Snapshot, Source, TypeDef, TypeExpr};
use anyhow::Result;
use gimli::constants::*;
use gimli::read::{self, AttributeValue, EndianSlice};
use gimli::LittleEndian;
use std::collections::{BTreeMap, HashMap};

type R<'a> = EndianSlice<'a, LittleEndian>;

/// One DIE, as the frontend needs it.
#[derive(Debug, Clone, Default)]
struct Die {
    tag: u16,
    name: Option<String>,
    linkage: Option<String>,
    ty: Option<usize>,
    size: Option<u64>,
    member_offset: Option<u64>,
    bit_size: Option<u64>,
    data_bit_offset: Option<u64>,
    count: Option<u64>,
    upper: Option<i64>,
    encoding: Option<u8>,
    declaration: bool,
    address: Option<u64>,
    value: Option<i64>,
    variadic: bool,
    children: Vec<usize>,
}

/// What a debug build's DWARF says.
#[derive(Debug, Default)]
pub struct DwarfView {
    pub types: BTreeMap<String, TypeDef>,
    /// Globals by address: (C name, type).
    pub globals: BTreeMap<u32, (String, TypeExpr)>,
    /// Defined functions by name.
    pub functions: BTreeMap<String, Signature>,
    pub units: usize,
}

struct Unit {
    dies: HashMap<usize, Die>,
    /// Record ids given by context (a field of an anonymous record's type,
    /// or an array of them).
    names: std::cell::RefCell<HashMap<usize, String>>,
}

impl Unit {
    fn width(&self, d: &Die) -> TypeExpr {
        let n = d.size.unwrap_or(4);
        TypeExpr::Width(
            match (d.encoding, n) {
                (Some(e), 4) if e == DW_ATE_float.0 => "f32",
                (Some(e), 8) if e == DW_ATE_float.0 => "f64",
                (Some(e), 1) if e == DW_ATE_signed.0 || e == DW_ATE_signed_char.0 => "s8",
                (Some(e), 2) if e == DW_ATE_signed.0 => "s16",
                (Some(e), 4) if e == DW_ATE_signed.0 => "s32",
                (Some(e), 8) if e == DW_ATE_signed.0 => "s64",
                (_, 1) => "u8",
                (_, 2) => "u16",
                (_, 8) => "u64",
                _ => "u32",
            }
            .into(),
        )
    }

    /// The type a DIE names; records and enums it defines go into `v`.
    fn ty(&self, at: Option<usize>, v: &mut DwarfView, depth: u32) -> TypeExpr {
        let Some(off) = at else { return TypeExpr::Width("void".into()) };
        let Some(d) = self.dies.get(&off) else { return TypeExpr::Width("u32".into()) };
        if depth > 64 {
            return TypeExpr::Width("u32".into());
        }
        match DwTag(d.tag) {
            DW_TAG_base_type => self.width(d),
            DW_TAG_pointer_type => TypeExpr::Ptr { ptr: Box::new(self.ty(d.ty, v, depth + 1)) },
            DW_TAG_const_type | DW_TAG_volatile_type | DW_TAG_restrict_type | DW_TAG_atomic_type => self.ty(d.ty, v, depth + 1),
            DW_TAG_typedef => {
                let name = d.name.clone().unwrap_or_default();
                // `typedef struct { … } Name;`: the record is `Name`.
                if let Some(t) = d.ty.and_then(|t| self.dies.get(&t)) {
                    if matches!(DwTag(t.tag), DW_TAG_structure_type | DW_TAG_union_type) && t.name.is_none() {
                        return self.record(d.ty.unwrap_or(0), Some(name), v, depth);
                    }
                }
                let t = self.ty(d.ty, v, depth + 1);
                v.types.entry(name).or_insert(TypeDef::Typedef { ty: t.clone() });
                t
            }
            DW_TAG_structure_type | DW_TAG_union_type => self.record(off, None, v, depth),
            DW_TAG_enumeration_type => {
                let id = format!("enum {}", d.name.clone().unwrap_or_default());
                if d.name.is_none() {
                    return TypeExpr::Width("s32".into());
                }
                let values = d.children.iter().filter_map(|c| self.dies.get(c)).filter_map(|e| Some((e.name.clone()?, e.value?))).collect();
                v.types.entry(id.clone()).or_insert(TypeDef::Enum { size: d.size.unwrap_or(4) as u32, values });
                TypeExpr::Named { named: id }
            }
            DW_TAG_array_type => {
                let mut t = self.ty(d.ty, v, depth + 1);
                let dims: Vec<u32> = d
                    .children
                    .iter()
                    .filter_map(|c| self.dies.get(c))
                    .filter(|c| DwTag(c.tag) == DW_TAG_subrange_type)
                    .map(|c| c.count.map(|n| n as u32).or(c.upper.map(|u| (u + 1) as u32)).unwrap_or(0))
                    .collect();
                for n in dims.into_iter().rev() {
                    t = TypeExpr::Array { array: Box::new(t), count: n };
                }
                t
            }
            DW_TAG_subroutine_type => TypeExpr::Fn { func: Box::new(self.sig(d, v, depth)) },
            _ => TypeExpr::Width("u32".into()),
        }
    }

    fn sig(&self, d: &Die, v: &mut DwarfView, depth: u32) -> Signature {
        let params = d
            .children
            .iter()
            .filter_map(|c| self.dies.get(c))
            .filter(|c| DwTag(c.tag) == DW_TAG_formal_parameter)
            .map(|p| Param { name: p.name.clone(), ty: self.ty(p.ty, v, depth + 1) })
            .collect();
        Signature { params, result: self.ty(d.ty, v, depth + 1), variadic: d.variadic }
    }

    /// A record: its id (`struct Tag`, a typedef's name, `Parent.field`),
    /// defined in `v` with its layout unless only declared here.
    fn record(&self, off: usize, typedef: Option<String>, v: &mut DwarfView, depth: u32) -> TypeExpr {
        let d = &self.dies[&off];
        let keyword = if DwTag(d.tag) == DW_TAG_union_type { "union" } else { "struct" };
        let id = match (&d.name, typedef, self.names.borrow().get(&off).cloned()) {
            (Some(n), _, _) => format!("{keyword} {n}"),
            (None, Some(t), _) => t,
            (None, None, Some(n)) => n,
            (None, None, None) => format!("{keyword} @{off:x}"),
        };
        if d.declaration || v.types.contains_key(&id) {
            return TypeExpr::Record { record: id };
        }
        // Defined before its fields, which can point back at it.
        let size = d.size.unwrap_or(0) as u32;
        v.types.insert(id.clone(), TypeDef::Struct { size, align: 1, fields: vec![], provenance: None });
        let mut fields = Vec::new();
        self.members(off, &id, 0, v, depth, &mut fields);
        let align = fields.iter().map(|f: &Field| align_of(&f.ty, &v.types)).max().unwrap_or(1);
        let prov = Some(Provenance { source: "dwarf".into(), file: None, text: None });
        let def = if keyword == "union" {
            TypeDef::Union { size, align, fields, provenance: prov }
        } else {
            TypeDef::Struct { size, align, fields, provenance: prov }
        };
        v.types.insert(id.clone(), def);
        TypeExpr::Record { record: id }
    }

    /// A record's members, anonymous members' flattened at `base`.
    fn members(&self, off: usize, id: &str, base: u32, v: &mut DwarfView, depth: u32, out: &mut Vec<Field>) {
        let d = &self.dies[&off];
        for c in d.children.clone() {
            let Some(m) = self.dies.get(&c) else { continue };
            if DwTag(m.tag) != DW_TAG_member {
                continue;
            }
            let offset = base + m.member_offset.unwrap_or(0) as u32;
            match &m.name {
                None => {
                    // An anonymous member: its fields are the record's own.
                    if let Some(t) = m.ty {
                        if self.dies.get(&t).is_some_and(|x| matches!(DwTag(x.tag), DW_TAG_structure_type | DW_TAG_union_type)) {
                            self.members(t, id, offset, v, depth, out);
                        }
                    }
                }
                Some(name) => {
                    // An anonymous record inline in a named field (or an
                    // array of them) is `Parent.field`, as the headers have it.
                    let mut el = m.ty;
                    while let Some(x) = el.and_then(|t| self.dies.get(&t)).filter(|x| DwTag(x.tag) == DW_TAG_array_type) {
                        el = x.ty;
                    }
                    if let Some(e) = el.filter(|e| self.dies.get(e).is_some_and(|x| matches!(DwTag(x.tag), DW_TAG_structure_type | DW_TAG_union_type) && x.name.is_none())) {
                        self.names.borrow_mut().entry(e).or_insert_with(|| format!("{id}.{name}"));
                    }
                    let ty = self.ty(m.ty, v, depth + 1);
                    match (m.bit_size, m.data_bit_offset) {
                        (Some(width), Some(bit)) => {
                            let unit = (size_of(&ty, &v.types).unwrap_or(4) * 8).max(8) as u64;
                            let word = (bit / unit) * unit;
                            out.push(Field { name: name.clone(), offset: base + (word / 8) as u32, ty, bits: Some([(bit - word) as u32, width as u32]) });
                        }
                        _ => out.push(Field { name: name.clone(), offset, ty, bits: None }),
                    }
                }
            }
        }
    }
}

fn size_of(t: &TypeExpr, types: &BTreeMap<String, TypeDef>) -> Option<u32> {
    crate::cfront::size_of(t, types)
}

fn align_of(t: &TypeExpr, types: &BTreeMap<String, TypeDef>) -> u32 {
    match t {
        TypeExpr::Array { array, .. } => align_of(array, types),
        TypeExpr::Record { record } => match types.get(record) {
            Some(TypeDef::Struct { align, .. } | TypeDef::Union { align, .. }) => *align,
            _ => 1,
        },
        t => size_of(t, types).unwrap_or(4).clamp(1, 8),
    }
}

/// Read a module's DWARF (its `.debug_*` sections).
pub fn read(sections: &[(String, Vec<u8>)]) -> Result<DwarfView> {
    let dwarf = read::Dwarf::load(|id| -> Result<_, gimli::Error> {
        let data = sections.iter().find(|(n, _)| n == id.name()).map_or(&[][..], |(_, b)| b.as_slice());
        Ok(EndianSlice::new(data, LittleEndian))
    })?;
    let mut v = DwarfView::default();
    let mut headers = dwarf.units();
    while let Some(header) = headers.next()? {
        let unit = dwarf.unit(header)?;
        v.units += 1;
        let u = collect(&dwarf, &unit)?;
        let root = u.dies.keys().copied().min().unwrap_or(0);
        let top: Vec<usize> = u.dies.get(&root).map(|r| r.children.clone()).unwrap_or_default();
        for off in top {
            let d = u.dies[&off].clone();
            match DwTag(d.tag) {
                DW_TAG_variable if !d.declaration => {
                    if let (Some(a), Some(n)) = (d.address, d.linkage.clone().or(d.name.clone())) {
                        let t = u.ty(d.ty, &mut v, 0);
                        v.globals.entry(a as u32).or_insert((n, t));
                    }
                }
                DW_TAG_subprogram if !d.declaration => {
                    if let Some(n) = d.linkage.clone().or(d.name.clone()) {
                        let s = u.sig(&d, &mut v, 0);
                        v.functions.entry(n).or_insert(s);
                    }
                }
                DW_TAG_typedef | DW_TAG_structure_type | DW_TAG_union_type | DW_TAG_enumeration_type => {
                    u.ty(Some(off), &mut v, 0);
                }
                _ => {}
            }
        }
    }
    Ok(v)
}

fn collect(dwarf: &read::Dwarf<R<'_>>, unit: &read::Unit<R<'_>>) -> Result<Unit> {
    let mut dies: HashMap<usize, Die> = HashMap::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut cursor = unit.entries();
    while let Some(e) = cursor.next_dfs()? {
        let off = e.offset().0;
        let depth = e.depth().max(0) as usize;
        stack.truncate(depth);
        if let Some(&parent) = stack.last() {
            if let Some(p) = dies.get_mut(&parent) {
                p.children.push(off);
            }
        }
        stack.push(off);
        let mut d = Die { tag: e.tag().0, ..Default::default() };
        let string = |v: AttributeValue<R<'_>>| dwarf.attr_string(unit, v).ok().map(|s| s.to_string_lossy().into_owned());
        for a in e.attrs() {
            let val = a.value();
            match a.name() {
                DW_AT_name => d.name = string(val),
                DW_AT_linkage_name => d.linkage = string(val),
                DW_AT_type => {
                    if let AttributeValue::UnitRef(r) = val {
                        d.ty = Some(r.0);
                    }
                }
                DW_AT_byte_size => d.size = val.udata_value(),
                DW_AT_data_member_location => d.member_offset = val.udata_value(),
                DW_AT_bit_size => d.bit_size = val.udata_value(),
                DW_AT_data_bit_offset => d.data_bit_offset = val.udata_value(),
                DW_AT_count => d.count = val.udata_value(),
                DW_AT_upper_bound => d.upper = val.sdata_value().or(val.udata_value().map(|x| x as i64)),
                DW_AT_encoding => {
                    if let AttributeValue::Encoding(enc) = val {
                        d.encoding = Some(enc.0);
                    }
                }
                DW_AT_declaration => d.declaration = matches!(val, AttributeValue::Flag(true)),
                DW_AT_const_value => d.value = val.sdata_value().or(val.udata_value().map(|x| x as i64)),
                DW_AT_location => {
                    if let Some(expr) = val.exprloc_value() {
                        let mut ops = expr.operations(unit.encoding());
                        if let Ok(Some(gimli::read::Operation::Address { address })) = ops.next() {
                            d.address = Some(address);
                        }
                    }
                }
                _ => {}
            }
        }
        if DwTag(d.tag) == DW_TAG_unspecified_parameters {
            if let Some(&parent) = stack.iter().rev().nth(1) {
                if let Some(p) = dies.get_mut(&parent) {
                    p.variadic = true;
                }
            }
        }
        dies.insert(off, d);
    }
    Ok(Unit { dies, names: Default::default() })
}

/// The C-header frontend's snapshot with DWARF's layouts (plans/M12 phase
/// 3): records and globals' types from DWARF where it has them, functions'
/// signatures from DWARF for those with C and from the headers for the
/// rest.
pub fn merge(c: &Snapshot, d: &DwarfView) -> Snapshot {
    let mut s = c.clone();
    s.source = Source { kind: "merged".into(), version: c.source.version.clone(), name: None, merged: vec![c.source.clone(), Source { kind: "dwarf".into(), version: c.source.version.clone(), name: None, merged: vec![] }] };
    for (id, t) in &d.types {
        if matches!(t, TypeDef::Struct { .. } | TypeDef::Union { .. }) || !s.types.contains_key(id) {
            s.types.insert(id.clone(), t.clone());
        }
    }
    for g in &mut s.globals {
        if let Some((_, t)) = d.globals.get(&g.address) {
            g.ty = Some(t.clone());
        }
    }
    for f in &mut s.functions {
        if let Some(sig) = f.names.iter().find_map(|n| d.functions.get(n)) {
            f.signature = Some(sig.clone());
        }
    }
    s
}

/// Where the DWARF and C frontends disagree on a record or global type both
/// describe (plans/M12 phase 3, exit criterion 2): records' sizes and
/// fields' offsets and types; globals' types by address. Bitfields aren't
/// compared (the C frontend gives the word, DWARF the bits).
pub fn agree(c: &Snapshot, d: &DwarfView) -> (usize, Vec<String>) {
    let mut n = 0;
    let mut out = Vec::new();
    for (id, ct) in &c.types {
        let (TypeDef::Struct { size: cs, fields: cf, .. } | TypeDef::Union { size: cs, fields: cf, .. }) = ct else { continue };
        let Some(TypeDef::Struct { size: ds, fields: df, .. } | TypeDef::Union { size: ds, fields: df, .. }) = d.types.get(id) else { continue };
        n += 1;
        if cs != ds {
            out.push(format!("{id}: {cs} bytes in the headers, {ds} as compiled"));
        }
        for f in cf {
            match df.iter().find(|g| g.name == f.name) {
                Some(g) if g.bits.is_some() => {}
                Some(g) if g.offset != f.offset => out.push(format!("{id}.{}: +0x{:X} in the headers, +0x{:X} as compiled", f.name, f.offset, g.offset)),
                Some(g) if !same(&f.ty, &g.ty, c, d) => out.push(format!("{id}.{}: {:?} in the headers, {:?} as compiled", f.name, f.ty, g.ty)),
                Some(_) => {}
                // The word the C frontend gives for bitfields.
                None if df.iter().any(|g| g.bits.is_some() && g.offset == f.offset) => {}
                None => out.push(format!("{id}.{}: the headers' field isn't compiled", f.name)),
            }
        }
    }
    for g in &c.globals {
        let (Some(ct), Some((_, dt))) = (&g.ty, d.globals.get(&g.address)) else { continue };
        n += 1;
        if !same(ct, dt, c, d) {
            out.push(format!("{}: {ct:?} in the headers, {dt:?} as compiled", g.names[0]));
        }
    }
    (n, out)
}

/// Two type expressions alike once typedefs are seen through.
fn same(a: &TypeExpr, b: &TypeExpr, c: &Snapshot, d: &DwarfView) -> bool {
    let res = |t: &TypeExpr, types: &BTreeMap<String, TypeDef>| -> TypeExpr {
        let mut t = t.clone();
        for _ in 0..16 {
            match &t {
                TypeExpr::Named { named } => match types.get(named) {
                    Some(TypeDef::Typedef { ty }) => t = ty.clone(),
                    Some(TypeDef::Enum { size, .. }) => return TypeExpr::Width(format!("s{}", size * 8)),
                    _ => return t,
                },
                _ => return t,
            }
        }
        t
    };
    match (res(a, &c.types), res(b, &d.types)) {
        (TypeExpr::Width(x), TypeExpr::Width(y)) => x == y,
        (TypeExpr::Ptr { ptr: x }, TypeExpr::Ptr { ptr: y }) => same(&x, &y, c, d) || matches!(*x, TypeExpr::Width(ref w) if w == "void") || matches!(*y, TypeExpr::Width(ref w) if w == "void"),
        (TypeExpr::Array { array: x, count: m }, TypeExpr::Array { array: y, count: n }) => (m == n || m == 0 || n == 0) && same(&x, &y, c, d),
        (TypeExpr::Record { record: x }, TypeExpr::Record { record: y }) => x == y,
        (TypeExpr::Fn { .. }, TypeExpr::Fn { .. }) => true,
        (x, y) => x == y,
    }
}
