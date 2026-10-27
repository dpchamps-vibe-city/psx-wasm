//! DWARF through the link (D53): clang's debug information, rewritten for
//! where the guest passes moved the code.
//!
//! An object's `.debug_*` sections are read with their relocations applied:
//! code addresses in the object's own code section, data addresses where
//! the link placed them, `__stack_pointer` as the guest's `$sp`. Every code
//! address, length, range and line row then goes through a [`CodeMap`] to
//! the output module's code section, and all units are written as one set
//! of sections. A linked module's DWARF goes through a later merge the same
//! way ([`Dwarf::add_module`]).
//!
//! Code addresses are offsets in the code section's payload, as wasm DWARF
//! has them. Code the map doesn't cover (functions the link dropped) gets
//! the tombstone address, and its ranges and rows are left out. Range and
//! location lists are written with absolute addresses: each starts by
//! setting its base address to 0, since DWARF 4 readers otherwise add the
//! unit's.

use crate::object::WasmObject;
use anyhow::{Context as _, Result};
use gimli::constants::*;
use gimli::read::{self, EndianSlice};
use gimli::write::{self, Address, AttributeValue, ConvertLineSequenceEnd};
use gimli::LittleEndian;
use wasmparser::{Parser, Payload, RelocationType};

/// The address code the map doesn't cover gets.
const TOMBSTONE: u64 = 0xFFFF_FFFF;

/// Where one function's instructions landed.
#[derive(Debug, Clone)]
pub struct FuncMap {
    /// The input body's code-section offset (where its locals start).
    pub from: u32,
    /// The output body's.
    pub to: u32,
    /// (input offset, output offset) from the bodies' starts, ascending,
    /// ending with both lengths ([`crate::rewrite::rewrite_mapped`]).
    pub ops: Vec<(u32, u32)>,
}

/// One module's code addresses to another's.
#[derive(Debug, Clone, Default)]
pub struct CodeMap(Vec<FuncMap>);

impl CodeMap {
    pub fn new(mut funcs: Vec<FuncMap>) -> Self {
        funcs.sort_by_key(|f| f.from);
        Self(funcs)
    }

    /// The output address of input code address `a`: an instruction's
    /// start goes to where its rewrite starts, a body's end to the output
    /// body's end. `None` outside every mapped function.
    pub fn map(&self, a: u64) -> Option<u64> {
        let a = u32::try_from(a).ok()?;
        let f = &self.0[self.0.partition_point(|f| f.from <= a).checked_sub(1)?];
        let off = a - f.from;
        if off > f.ops.last()?.0 {
            return None;
        }
        // Inside the locals (the function's own address): the body's start.
        let out = match f.ops.partition_point(|&(i, _)| i <= off).checked_sub(1) {
            Some(j) => f.ops[j].1,
            None => 0,
        };
        Some((f.to + out) as u64)
    }
}

/// Every defined function body's code-section offset (where its locals
/// start), in order.
pub fn body_starts(wasm: &[u8]) -> Result<Vec<u32>> {
    let mut code = 0;
    let mut out = Vec::new();
    for p in Parser::new(0).parse_all(wasm) {
        match p? {
            Payload::CodeSectionStart { unchecked_range, .. } => code = unchecked_range.start,
            Payload::CodeSectionEntry(body) => out.push((body.range().start - code) as u32),
            _ => {}
        }
    }
    Ok(out)
}

/// A module's `.debug_*` sections.
pub fn sections(wasm: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    for p in Parser::new(0).parse_all(wasm) {
        if let Payload::CustomSection(c) = p? {
            if c.name().starts_with(".debug_") {
                out.push((c.name().to_string(), c.data().to_vec()));
            }
        }
    }
    Ok(out)
}

/// Units converted so far, written out by [`Dwarf::finish`].
pub struct Dwarf {
    out: write::Dwarf,
}

impl Default for Dwarf {
    fn default() -> Self {
        Self::new()
    }
}

impl Dwarf {
    pub fn new() -> Self {
        Self { out: write::Dwarf::new() }
    }

    /// An object's units. `code` maps the object's code section;
    /// `data_addr` and `global` resolve its data and global symbols as the
    /// link did.
    pub fn add_object(
        &mut self,
        obj: &WasmObject,
        code: &CodeMap,
        data_addr: &mut dyn FnMut(u32) -> Option<u32>,
        global: &mut dyn FnMut(u32) -> Option<u32>,
    ) -> Result<()> {
        let mut sections = Vec::new();
        for d in &obj.debug {
            let mut bytes = obj.bytes[d.range.clone()].to_vec();
            for r in &d.relocs {
                let value = match r.ty {
                    RelocationType::FunctionOffsetI32 => {
                        let s = &obj.symbols[r.symbol as usize];
                        let f = s.index.context("function offset of a symbol without a function")? - obj.num_imported_funcs();
                        let body = &obj.funcs.get(f as usize).context("function offset of an imported function")?.body;
                        Some((body.start - obj.code_start) as u32 as i64 + r.addend)
                    }
                    // Into a debug section: section symbols start at 0.
                    RelocationType::SectionOffsetI32 => Some(r.addend),
                    RelocationType::MemoryAddrI32 => Some(data_addr(r.symbol).unwrap_or(0) as i64 + r.addend),
                    RelocationType::GlobalIndexI32 => global(r.symbol).map(i64::from),
                    _ => None,
                };
                if let Some(v) = value {
                    let at = r.offset as usize;
                    bytes.get_mut(at..at + 4).context("relocation outside its section")?.copy_from_slice(&(v as u32).to_le_bytes());
                }
            }
            sections.push((d.name.clone(), bytes));
        }
        self.add(&sections, code)
    }

    /// A linked module's units (its [`sections`]), its code mapped by
    /// `code`.
    pub fn add_module(&mut self, sections: &[(String, Vec<u8>)], code: &CodeMap) -> Result<()> {
        self.add(sections, code)
    }

    fn add(&mut self, sections: &[(String, Vec<u8>)], code: &CodeMap) -> Result<()> {
        let read = read::Dwarf::load(|id| -> Result<_, gimli::Error> {
            let data = sections.iter().find(|(n, _)| n == id.name()).map_or(&[][..], |(_, b)| b.as_slice());
            Ok(EndianSlice::new(data, LittleEndian))
        })?;
        let code = |a| code.map(a);
        let mut convert = self.out.convert(&read)?;
        while let Some((mut unit, root)) = convert.read_unit()? {
            convert_unit(&mut unit, root, &code)?;
        }
        Ok(())
    }

    /// The sections to append to the module: (name, contents).
    pub fn finish(mut self) -> Result<Vec<(&'static str, Vec<u8>)>> {
        let mut sections = write::Sections::new(write::EndianVec::new(LittleEndian));
        self.out.write(&mut sections)?;
        let mut out = Vec::new();
        sections.for_each(|id, data| -> Result<(), gimli::Error> {
            if !data.slice().is_empty() {
                out.push((id.name(), data.slice().to_vec()));
            }
            Ok(())
        })?;
        Ok(out)
    }
}

type R<'a> = EndianSlice<'a, LittleEndian>;

fn convert_unit<'a, 'r>(
    unit: &mut write::ConvertUnit<'a, R<'r>>,
    root: write::ConvertUnitEntry<'a, R<'r>>,
    code: &dyn Fn(u64) -> Option<u64>,
) -> write::ConvertResult<()> {
    if let Some(mut lines) = unit.read_line_program(None, None)? {
        while let Some(seq) = lines.read_sequence()? {
            let Some(start) = seq.start else { continue };
            let Some(to) = code(start) else { continue };
            lines.begin_sequence(Some(Address::Constant(to)));
            let mut last = 0;
            for mut row in seq.rows {
                let Some(a) = code(start + row.address_offset) else { continue };
                row.address_offset = (a - to).max(last);
                last = row.address_offset;
                lines.generate_row(row);
            }
            let end = match seq.end {
                ConvertLineSequenceEnd::Length(n) => code(start + n),
                ConvertLineSequenceEnd::Address(a) => code(a),
            };
            lines.end_sequence(end.map_or(last, |e| (e - to).max(last)));
        }
        let (program, files) = lines.program();
        unit.set_line_program(program, files);
    }
    let id = unit.unit.root();
    attributes(unit, id, &root, code)?;
    let mut entry = root;
    while let Some(id) = unit.read_entry(&mut entry)? {
        if id.is_none() {
            continue;
        }
        let id = unit.add_entry(id, &entry);
        attributes(unit, id, &entry, code)?;
    }
    Ok(())
}

/// An entry's attributes, its code addresses mapped and its data
/// addresses kept.
fn attributes<'a, 'r>(
    unit: &mut write::ConvertUnit<'a, R<'r>>,
    id: write::UnitEntryId,
    entry: &write::ConvertUnitEntry<'a, R<'r>>,
    code: &dyn Fn(u64) -> Option<u64>,
) -> write::ConvertResult<()> {
    let data = |a: u64| Some(Address::Constant(a));
    let read_unit = entry.read_unit;
    let address = |v: read::AttributeValue<R<'r>>| -> write::ConvertResult<Option<u64>> {
        Ok(match v {
            read::AttributeValue::Addr(a) => Some(a),
            read::AttributeValue::DebugAddrIndex(i) => Some(read_unit.address(i)?),
            _ => None,
        })
    };
    // Lengths from here (high_pc, DWARF 5's entry_pc) are mapped as the
    // addresses they end at.
    let low = match entry.attr_value(DW_AT_low_pc) {
        Some(v) => address(v)?,
        None => None,
    };
    let unit_tag = entry.tag == DW_TAG_compile_unit || entry.tag == DW_TAG_partial_unit;
    for attr in &entry.attrs {
        let name = attr.name();
        if name == DW_AT_GNU_locviews {
            continue;
        }
        let value = match attr.value() {
            v @ (read::AttributeValue::Addr(_) | read::AttributeValue::DebugAddrIndex(_)) => {
                let a = address(v)?.unwrap_or(0);
                // A unit's base address of 0: its ranges say where it is.
                let to = if unit_tag && a == 0 { Some(0) } else { code(a) };
                AttributeValue::Address(Address::Constant(to.unwrap_or(TOMBSTONE)))
            }
            _ if (name == DW_AT_high_pc || name == DW_AT_entry_pc) && attr.udata_value().is_some() => {
                let len = attr.udata_value().unwrap_or(0);
                let mapped = low.and_then(|l| Some((code(l)?, code(l + len)?)));
                AttributeValue::Udata(mapped.map_or(0, |(b, e)| e.saturating_sub(b)))
            }
            read::AttributeValue::RangeListsRef(o) => {
                let offset = read_unit.ranges_offset_from_raw(o);
                ranges(unit, read_unit.ranges(offset)?, code)?
            }
            read::AttributeValue::DebugRngListsIndex(i) => {
                let offset = read_unit.ranges_offset(i)?;
                ranges(unit, read_unit.ranges(offset)?, code)?
            }
            read::AttributeValue::LocationListsRef(o) => locations(unit, read_unit, read_unit.locations(o)?, code)?,
            read::AttributeValue::DebugLocListsIndex(i) => {
                let offset = read_unit.locations_offset(i)?;
                locations(unit, read_unit, read_unit.locations(offset)?, code)?
            }
            read::AttributeValue::Exprloc(e) => AttributeValue::Exprloc(unit.convert_expression(read_unit, e, &data)?),
            _ => unit.convert_attribute_value(read_unit, attr, &data)?,
        };
        unit.unit.get_mut(id).set(name, value);
    }
    Ok(())
}

fn ranges<'a, 'r>(
    unit: &mut write::ConvertUnit<'a, R<'r>>,
    mut it: read::RngListIter<R<'r>>,
    code: &dyn Fn(u64) -> Option<u64>,
) -> write::ConvertResult<AttributeValue> {
    let mut list = vec![write::Range::BaseAddress { address: Address::Constant(0) }];
    while let Some(r) = it.next()? {
        if let (Some(b), Some(e)) = (code(r.begin), code(r.end)) {
            if e > b {
                list.push(write::Range::StartEnd { begin: Address::Constant(b), end: Address::Constant(e) });
            }
        }
    }
    Ok(AttributeValue::RangeListRef(unit.unit.ranges.add(write::RangeList(list))))
}

fn locations<'a, 'r>(
    unit: &mut write::ConvertUnit<'a, R<'r>>,
    read_unit: read::UnitRef<'a, R<'r>>,
    mut it: read::LocListIter<R<'r>>,
    code: &dyn Fn(u64) -> Option<u64>,
) -> write::ConvertResult<AttributeValue> {
    let data = |a: u64| Some(Address::Constant(a));
    let mut list = vec![write::Location::BaseAddress { address: Address::Constant(0) }];
    while let Some(l) = it.next()? {
        if let (Some(b), Some(e)) = (code(l.range.begin), code(l.range.end)) {
            if e > b {
                let data = unit.convert_expression(read_unit, l.data, &data)?;
                list.push(write::Location::StartEnd { begin: Address::Constant(b), end: Address::Constant(e), data });
            }
        }
    }
    Ok(AttributeValue::LocationListRef(unit.unit.locations.add(write::LocationList(list))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_instruction_starts_and_ends() {
        // A body whose locals take 3 bytes; ops at 3, 5, 9; length 10. The
        // rewrite added a 4-byte prologue and expanded the op at 5.
        let m = CodeMap::new(vec![FuncMap { from: 100, to: 1000, ops: vec![(3, 7), (5, 9), (9, 20), (10, 21)] }]);
        assert_eq!(m.map(100), Some(1000), "the function's own address");
        assert_eq!(m.map(103), Some(1007));
        assert_eq!(m.map(105), Some(1009));
        assert_eq!(m.map(109), Some(1020));
        assert_eq!(m.map(110), Some(1021), "the body's end");
        assert_eq!(m.map(111), None);
        assert_eq!(m.map(99), None);
    }
}
