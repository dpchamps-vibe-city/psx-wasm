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
use gimli::LittleEndian;
use gimli::constants::*;
use gimli::read::{self, EndianSlice};
use gimli::write::{self, Address, AttributeValue, ConvertLineSequenceEnd};
use std::collections::{BTreeMap, BTreeSet};
use wasmparser::{Operator, Parser, Payload, RelocationType};

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
pub struct CodeMap(Vec<FuncMap>, Vec<std::ops::Range<u64>>);

impl CodeMap {
    pub fn new(mut funcs: Vec<FuncMap>) -> Self {
        funcs.sort_by_key(|f| f.from);
        Self(funcs, Vec::new())
    }

    /// Exclude the interior of expanded guest rewrites. At the first output
    /// opcode the original operand stack is intact; masking stores, intrinsic
    /// lowering, etc. then consume it and use scratch locals. This is not a
    /// variable lifetime. The merge only re-encodes indices and needs no gaps.
    pub fn exclude_expansions(mut self, ends: &BTreeMap<u64, u64>) -> Self {
        for f in &self.0 {
            for pair in f.ops.windows(2) {
                let [(a, b), (c, d)] = pair else { unreachable!() };
                if d - b > c - a {
                    let begin = u64::from(f.to + b);
                    let end = u64::from(f.to + d);
                    if let Some(&first_end) = ends.get(&begin) {
                        if first_end < end {
                            self.1.push(first_end..end);
                        }
                    }
                }
            }
        }
        self.1.sort_by_key(|r| r.start);
        self
    }

    fn location_ranges(&self, begin: u64, end: u64) -> Vec<std::ops::Range<u64>> {
        let mut out = Vec::new();
        let mut at = begin;
        for gap in &self.1[self.1.partition_point(|r| r.end <= begin)..] {
            if gap.start >= end {
                break;
            }
            if gap.start > at {
                out.push(at..gap.start);
            }
            at = at.max(gap.end);
        }
        if at < end {
            out.push(at..end);
        }
        out
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

/// Output opcode boundaries, indexed once for all objects in a debug link.
pub fn opcode_ends(wasm: &[u8]) -> Result<BTreeMap<u64, u64>> {
    let mut code = 0;
    let mut out = BTreeMap::new();
    for p in Parser::new(0).parse_all(wasm) {
        match p? {
            Payload::CodeSectionStart { unchecked_range, .. } => code = unchecked_range.start,
            Payload::CodeSectionEntry(body) => {
                let mut ops = body.get_operators_reader()?;
                while !ops.eof() {
                    let start = ops.original_position() - code;
                    ops.read()?;
                    out.insert(start, ops.original_position() - code);
                }
            }
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

/// The source files a module's line tables name: each unit's
/// `DW_AT_comp_dir`, the file's directory and its name joined, `.` and
/// `..` resolved, as a debugger joins them.
pub fn source_paths(sections: &[(String, Vec<u8>)]) -> Result<std::collections::BTreeSet<String>> {
    let read = read::Dwarf::load(|id| -> Result<_, gimli::Error> {
        let data = sections.iter().find(|(n, _)| n == id.name()).map_or(&[][..], |(_, b)| b.as_slice());
        Ok(EndianSlice::new(data, LittleEndian))
    })?;
    let mut out = std::collections::BTreeSet::new();
    let mut units = read.units();
    while let Some(header) = units.next()? {
        let unit = read.unit(header)?;
        let Some(program) = &unit.line_program else { continue };
        let header = program.header();
        let comp = unit.comp_dir.map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
        for file in header.file_names() {
            let mut parts = vec![comp.clone()];
            // Directory 0 is the compilation directory itself.
            if file.directory_index() != 0 {
                if let Some(dir) = file.directory(header) {
                    parts.push(read.attr_string(&unit, dir)?.to_string_lossy().into_owned());
                }
            }
            parts.push(read.attr_string(&unit, file.path_name())?.to_string_lossy().into_owned());
            out.insert(join_path(&parts));
        }
    }
    Ok(out)
}

/// `parts` joined with `/`: an absolute part starts over, `.` and `..`
/// are resolved as a URL's are.
fn join_path(parts: &[String]) -> String {
    let mut segs: Vec<&str> = Vec::new();
    let mut absolute = false;
    for p in parts {
        if p.starts_with('/') {
            segs.clear();
            absolute = true;
        }
        for s in p.split('/') {
            match s {
                "" | "." => {}
                ".." => {
                    segs.pop();
                }
                s => segs.push(s),
            }
        }
    }
    let joined = segs.join("/");
    if absolute { format!("/{joined}") } else { joined }
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
        let mut locals = Vec::new();
        for f in &obj.funcs {
            let body = wasmparser::FunctionBody::new(wasmparser::BinaryReader::new(&obj.bytes[f.body.clone()], f.body.start as u64));
            locals.push(mutable_locals(&body, obj.code_start as u64)?);
        }
        self.add(&sections, code, &locals)
    }

    /// Import clang-produced DWARF before transformations. Like add_object,
    /// invalidate fixed parameter locations in locals the compiler reuses.
    /// Do not apply this to transformed code: its synthetic writes are not
    /// evidence of a defect in the original parameter description.
    pub fn add_input_module(&mut self, wasm: &[u8], code: &CodeMap) -> Result<()> {
        let mut locals = Vec::new();
        let mut start = 0;
        for p in Parser::new(0).parse_all(wasm) {
            match p? {
                Payload::CodeSectionStart { unchecked_range, .. } => start = unchecked_range.start,
                Payload::CodeSectionEntry(body) => locals.push(mutable_locals(&body, start)?),
                _ => {}
            }
        }
        self.add(&sections(wasm)?, code, &locals)
    }

    /// A linked module's units (its [`sections`]), its code mapped by
    /// `code`.
    pub fn add_module(&mut self, sections: &[(String, Vec<u8>)], code: &CodeMap) -> Result<()> {
        self.add(sections, code, &[])
    }

    fn add(&mut self, sections: &[(String, Vec<u8>)], code: &CodeMap, locals: &[MutableLocals]) -> Result<()> {
        let read = read::Dwarf::load(|id| -> Result<_, gimli::Error> {
            let data = sections.iter().find(|(n, _)| n == id.name()).map_or(&[][..], |(_, b)| b.as_slice());
            Ok(EndianSlice::new(data, LittleEndian))
        })?;
        let mut convert = self.out.convert(&read)?;
        while let Some((mut unit, root)) = convert.read_unit()? {
            convert_unit(&mut unit, root, code, locals)?;
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

struct MutableLocals {
    body: std::ops::Range<u64>,
    written: BTreeSet<u32>,
}

fn mutable_locals(body: &wasmparser::FunctionBody<'_>, code: u64) -> Result<MutableLocals> {
    let mut ops = body.get_operators_reader()?;
    let mut written = BTreeSet::new();
    while !ops.eof() {
        if let Operator::LocalSet { local_index } | Operator::LocalTee { local_index } = ops.read()? {
            written.insert(local_index);
        }
    }
    Ok(MutableLocals { body: body.range().start - code..body.range().end - code, written })
}

/// A fixed function-wide parameter description in a reused local is not a
/// lifetime description. Clang 23 emits this for worker(input) in the threaded
/// debug fixture: local 0 becomes `before`, but still claims to be `input`.
/// Until LLVM supplies a correct lifetime, omit that description entirely.
/// This is intentionally conservative (including writes of an equal value).
fn reused_parameter(expr: read::Expression<R<'_>>, encoding: gimli::Encoding, written: &BTreeSet<u32>) -> gimli::Result<bool> {
    let mut ops = expr.operations(encoding);
    Ok(matches!(ops.next()?, Some(read::Operation::WasmLocal { index }) if written.contains(&index))
        && ops.next()? == Some(read::Operation::StackValue)
        && ops.next()?.is_none())
}

/// The DevTools consumer materializes missing aggregate pieces as zero instead
/// of unavailable. Also reject compiler-folded, storage-free arithmetic: LLVM
/// can describe only the used low bits of an optimized i64 as its whole value
/// (the typed fixture's `(input << 33) | 7` becomes `0 | 7`). Direct constants,
/// storage-backed compound expressions and complete pieces remain supported.
fn input_expression_available(
    expr: read::Expression<R<'_>>, encoding: gimli::Encoding, value_type: (bool, Option<u64>),
) -> gimli::Result<bool> {
    use read::Operation as O;
    let mut ops = expr.operations(encoding);
    let (mut piece_value, mut storage, mut arithmetic) = (false, false, false);
    let (mut stack_value, mut piece_bits) = (false, None::<u64>);
    while let Some(op) = ops.next()? {
        match op {
            O::Piece { size_in_bits, .. } => {
                if !piece_value { return Ok(false); }
                let Some(bits) = piece_bits.unwrap_or(0).checked_add(size_in_bits) else { return Ok(false) };
                piece_bits = Some(bits);
                piece_value = false;
            }
            O::StackValue => stack_value = true,
            O::Nop => {}
            O::UnsignedConstant { .. } | O::SignedConstant { .. } => piece_value = true,
            O::WasmLocal { .. }
            | O::WasmGlobal { .. }
            | O::WasmStack { .. }
            | O::FrameOffset { .. }
            | O::Address { .. }
            | O::Register { .. }
            | O::RegisterOffset { .. } => {
                storage = true;
                piece_value = true;
            }
            _ => {
                arithmetic = true;
            }
        }
    }
    // A missing tail is just as unavailable as an explicit empty piece. A
    // scalar stack_value is not a complete aggregate representation either.
    let complete = piece_bits.is_none() || value_type.1.is_some_and(|required| piece_bits.unwrap() >= required);
    Ok((storage || !arithmetic) && complete && (value_type.0 || !stack_value || piece_bits.is_some()))
}

fn convert_unit<'a, 'r>(
    unit: &mut write::ConvertUnit<'a, R<'r>>,
    root: write::ConvertUnitEntry<'a, R<'r>>,
    code: &CodeMap,
    locals: &[MutableLocals],
) -> write::ConvertResult<()> {
    if let Some(mut lines) = unit.read_line_program(None, None)? {
        while let Some(seq) = lines.read_sequence()? {
            let Some(start) = seq.start else { continue };
            let Some(to) = code.map(start) else { continue };
            lines.begin_sequence(Some(Address::Constant(to)));
            let end = match seq.end {
                ConvertLineSequenceEnd::Length(n) => code.map(start + n),
                ConvertLineSequenceEnd::Address(a) => code.map(a),
            };
            let mut rows = Vec::new();
            for mut row in seq.rows {
                let Some(a) = code.map(start + row.address_offset) else {
                    continue;
                };
                row.address_offset = a - to;
                rows.push(row);
            }
            // Do not attribute the interior of a masking/intrinsic rewrite
            // to the source operation whose stack it has already consumed.
            let original = rows.clone();
            for gap in &code.1 {
                if gap.start < to || gap.start >= end.unwrap_or(to) {
                    continue;
                }
                if let Some(row) = original.iter().rfind(|r| to + r.address_offset <= gap.start) {
                    // Neither row starts a statement: a line breakpoint
                    // would otherwise stop again after every rewrite.
                    let mut continued = *row;
                    continued.is_statement = false;
                    continued.basic_block = false;
                    continued.prologue_end = false;
                    continued.epilogue_begin = false;
                    let mut zero = continued;
                    zero.address_offset = gap.start - to;
                    zero.line = 0;
                    rows.push(zero);
                    if gap.end < end.unwrap_or(to) && !original.iter().any(|r| to + r.address_offset == gap.end) {
                        let mut restore = continued;
                        restore.address_offset = gap.end - to;
                        rows.push(restore);
                    }
                }
            }
            rows.sort_by_key(|r| r.address_offset);
            let mut last = 0;
            for row in rows {
                last = row.address_offset;
                lines.generate_row(row);
            }
            lines.end_sequence(end.map_or(last, |e| (e - to).max(last)));
        }
        let (program, files) = lines.program();
        unit.set_line_program(program, files);
    }
    let id = unit.unit.root();
    attributes(unit, id, &root, code, None)?;
    let mut entry = root;
    let mut function = None;
    while let Some(id) = unit.read_entry(&mut entry)? {
        if function.is_some_and(|(depth, _)| entry.depth <= depth) {
            function = None;
        }
        if entry.tag == DW_TAG_subprogram {
            if let Some(read::AttributeValue::Addr(pc)) = entry.attr_value(DW_AT_low_pc) {
                function = locals.iter().find(|l| l.body.contains(&pc)).map(|l| (entry.depth, l));
            }
        }
        if id.is_none() {
            continue;
        }
        let parameter_writes =
            function.filter(|(depth, _)| entry.tag == DW_TAG_formal_parameter && entry.depth == depth + 1).map(|(_, l)| l);
        let id = unit.add_entry(id, &entry);
        attributes(unit, id, &entry, code, parameter_writes)?;
    }
    Ok(())
}

/// A numeric constant describes a scalar, not an aggregate's full contents.
/// LLVM can incorrectly promote a changing stack structure to constant zero
/// (ClearVramRectAndWait's RECT). Follow qualifiers/typedefs, failing closed
/// for types whose complete representation we cannot establish.
fn variable_type(entry: &write::ConvertUnitEntry<'_, R<'_>>) -> gimli::Result<(bool, Option<u64>)> {
    let mut ty = entry.attr_value(DW_AT_type);
    let mut origin = entry.attr_value(DW_AT_abstract_origin).or_else(|| entry.attr_value(DW_AT_specification));
    for _ in 0..32 {
        if ty.is_none() {
            let Some(read::AttributeValue::UnitRef(offset)) = origin else { return Ok((false, None)) };
            let die = entry.read_unit.entry(offset)?;
            ty = die.attr_value(DW_AT_type);
            origin = die.attr_value(DW_AT_abstract_origin).or_else(|| die.attr_value(DW_AT_specification));
            continue;
        }
        let Some(read::AttributeValue::UnitRef(offset)) = ty else { return Ok((false, None)) };
        let die = entry.read_unit.entry(offset)?;
        let bits = die.attr_value(DW_AT_byte_size).and_then(|v| v.udata_value()).and_then(|n| n.checked_mul(8));
        match die.tag() {
            gimli::DW_TAG_base_type | gimli::DW_TAG_enumeration_type => return Ok((true, bits)),
            gimli::DW_TAG_pointer_type | gimli::DW_TAG_reference_type | gimli::DW_TAG_rvalue_reference_type =>
                return Ok((true, bits.or(Some(u64::from(entry.read_unit.encoding().address_size) * 8)))),
            gimli::DW_TAG_typedef | gimli::DW_TAG_const_type | gimli::DW_TAG_volatile_type |
            gimli::DW_TAG_restrict_type | gimli::DW_TAG_atomic_type => ty = die.attr_value(DW_AT_type),
            _ => return Ok((false, bits)),
        }
    }
    Ok((false, None))
}

/// An entry's attributes, its code addresses mapped and its data
/// addresses kept.
fn attributes<'a, 'r>(
    unit: &mut write::ConvertUnit<'a, R<'r>>,
    id: write::UnitEntryId,
    entry: &write::ConvertUnitEntry<'a, R<'r>>,
    code: &CodeMap,
    parameter_writes: Option<&MutableLocals>,
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
    let is_variable = entry.tag == DW_TAG_variable || entry.tag == DW_TAG_formal_parameter;
    let value_type = if is_variable { variable_type(entry)? } else { (true, None) };
    for attr in &entry.attrs {
        let name = attr.name();
        if name == DW_AT_GNU_locviews {
            continue;
        }
        if name == DW_AT_const_value
            && is_variable
            && (attr.udata_value().is_some() || attr.sdata_value().is_some())
            && !value_type.0
        {
            continue;
        }
        if name == DW_AT_location {
            if let read::AttributeValue::Exprloc(expr) = attr.value() {
                if !input_expression_available(expr, read_unit.encoding(), value_type)? {
                    continue;
                }
            }
            if let (Some(written), read::AttributeValue::Exprloc(expr)) = (parameter_writes, attr.value()) {
                if reused_parameter(expr, read_unit.encoding(), &written.written)? {
                    continue;
                }
            }
        }
        let value = match attr.value() {
            v @ (read::AttributeValue::Addr(_) | read::AttributeValue::DebugAddrIndex(_)) => {
                let a = address(v)?.unwrap_or(0);
                // A unit's base address of 0: its ranges say where it is.
                let to = if unit_tag && a == 0 { Some(0) } else { code.map(a) };
                AttributeValue::Address(Address::Constant(to.unwrap_or(TOMBSTONE)))
            }
            _ if (name == DW_AT_high_pc || name == DW_AT_entry_pc) && attr.udata_value().is_some() => {
                let len = attr.udata_value().unwrap_or(0);
                let mapped = low.and_then(|l| Some((code.map(l)?, code.map(l + len)?)));
                // Binaryen's DWARF 4 updater expects a fixed-width relative
                // high_pc. A ULEB can grow during Asyncify and would also need
                // DIE-reference rebuilding, not just location-list rebuilding.
                AttributeValue::Data4(mapped.map_or(0, |(b, e)| e.saturating_sub(b)) as u32)
            }
            read::AttributeValue::RangeListsRef(o) => {
                let offset = read_unit.ranges_offset_from_raw(o);
                ranges(unit, read_unit.ranges(offset)?, code)?
            }
            read::AttributeValue::DebugRngListsIndex(i) => {
                let offset = read_unit.ranges_offset(i)?;
                ranges(unit, read_unit.ranges(offset)?, code)?
            }
            read::AttributeValue::LocationListsRef(o) => locations(unit, read_unit, read_unit.locations(o)?, code, parameter_writes, value_type)?,
            read::AttributeValue::DebugLocListsIndex(i) => {
                let offset = read_unit.locations_offset(i)?;
                locations(unit, read_unit, read_unit.locations(offset)?, code, parameter_writes, value_type)?
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
    code: &CodeMap,
) -> write::ConvertResult<AttributeValue> {
    let mut list = vec![write::Range::BaseAddress { address: Address::Constant(0) }];
    while let Some(r) = it.next()? {
        if let (Some(b), Some(e)) = (code.map(r.begin), code.map(r.end)) {
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
    code: &CodeMap,
    parameter: Option<&MutableLocals>,
    value_type: (bool, Option<u64>),
) -> write::ConvertResult<AttributeValue> {
    let data = |a: u64| Some(Address::Constant(a));
    let mut list = vec![write::Location::BaseAddress { address: Address::Constant(0) }];
    let mut entries = Vec::new();
    while let Some(l) = it.next()? {
        entries.push(l);
    }
    let mut reused = false;
    if let (Some(parameter), [entry]) = (parameter, entries.as_slice()) {
        reused = entry.range.begin <= parameter.body.start
            && entry.range.end >= parameter.body.end
            && reused_parameter(entry.data, read_unit.encoding(), &parameter.written)?;
    }
    for l in entries.into_iter().filter(|_| !reused) {
        if !input_expression_available(l.data, read_unit.encoding(), value_type)? {
            continue;
        }
        if let (Some(b), Some(e)) = (code.map(l.range.begin), code.map(l.range.end)) {
            for range in code.location_ranges(b, e) {
                let data = unit.convert_expression(read_unit, l.data, &data)?;
                list.push(write::Location::StartEnd { begin: Address::Constant(range.start), end: Address::Constant(range.end), data });
            }
        }
    }
    Ok(AttributeValue::LocationListRef(unit.unit.locations.add(write::LocationList(list))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalidates_only_fixed_parameters_in_reused_locals() {
        let encoding = gimli::Encoding { format: gimli::Format::Dwarf32, version: 4, address_size: 4 };
        let expression = read::Expression(EndianSlice::new(&[0xed, 0, 0, 0x9f], LittleEndian));
        assert!(reused_parameter(expression, encoding, &[0].into()).unwrap());
        assert!(!reused_parameter(expression, encoding, &[1].into()).unwrap());
        let frame_relative = read::Expression(EndianSlice::new(&[0x91, 0x0c], LittleEndian));
        assert!(!reused_parameter(frame_relative, encoding, &[0].into()).unwrap());
    }

    #[test]
    fn rejects_missing_pieces_and_storage_free_arithmetic() {
        let encoding = gimli::Encoding { format: gimli::Format::Dwarf32, version: 4, address_size: 4 };
        let available =
            |bytes: &[u8]| input_expression_available(read::Expression(EndianSlice::new(bytes, LittleEndian)), encoding, (true, Some(64))).unwrap();
        assert!(!available(&[0x93, 4, 0x37, 0x9f, 0x93, 4]));
        assert!(!available(&[0x30, 0x37, 0x21, 0x9f]));
        assert!(available(&[0xed, 0, 0, 0x9f, 0x93, 4, 0x37, 0x9f, 0x93, 4]));
        assert!(available(&[0x91, 0x0c]));
        assert!(available(&[0x37, 0x9f]));
        assert!(!available(&[0x37, 0x9f, 0x93, 4]), "missing aggregate tail");
        let aggregate = |bytes: &[u8]| input_expression_available(
            read::Expression(EndianSlice::new(bytes, LittleEndian)), encoding, (false, Some(64)),
        ).unwrap();
        assert!(!aggregate(&[0x30, 0x9f]), "scalar zero is not a whole structure");
        assert!(aggregate(&[0x91, 0x0c]), "frame-backed structure remains inspectable");
    }

    #[test]
    fn joins_paths_as_urls_resolve() {
        let j = |p: &[&str]| join_path(&p.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(j(&["source/game", "src/overlays", "a.c"]), "source/game/src/overlays/a.c");
        assert_eq!(j(&["source/lift", "./psxw_rt.h"]), "source/lift/psxw_rt.h");
        assert_eq!(j(&["source/game", "src", "../include/b.h"]), "source/game/include/b.h");
        assert_eq!(j(&["source/game", "/abs/c.h"]), "/abs/c.h");
    }

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
        let m = m.exclude_expansions(&[(1009, 1011)].into());
        assert_eq!(m.location_ranges(1007, 1021), [1007..1011, 1020..1021]);
        assert_eq!(m.location_ranges(1012, 1014), Vec::<std::ops::Range<u64>>::new());
    }
}
