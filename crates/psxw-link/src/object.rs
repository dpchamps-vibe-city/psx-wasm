//! Reader for relocatable wasm objects (tool-conventions `Linking.md`): the
//! symbol table, function types, imports, function bodies with their code
//! relocations, and data segments with their relocations. This is the input
//! side of the final link; the census uses it to see what each translation
//! unit defines and references.

use anyhow::{Context, Result, bail};
use wasmparser::{
    KnownCustom, Linking, Parser, Payload, RelocationType, SymbolFlags, SymbolInfo, TypeRef, ValType,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Data,
    Global,
    Other,
}

#[derive(Debug, Clone)]
pub struct Symbol {
    pub kind: SymbolKind,
    pub name: String,
    pub defined: bool,
    pub local: bool,
    pub weak: bool,
    /// Function or global index in the object's index space (imports
    /// first), for function and global symbols.
    pub index: Option<u32>,
    /// For defined data symbols.
    pub data: Option<DataDef>,
}

#[derive(Debug, Clone, Copy)]
pub struct DataDef {
    pub segment: u32,
    pub offset: u32,
    pub size: u32,
}

#[derive(Debug, Clone)]
pub struct Segment {
    /// e.g. `.rodata.foo`, `.data.bar`, `.bss.baz`.
    pub name: String,
    pub bytes: Vec<u8>,
    /// log2 of the alignment.
    pub align_log2: u32,
    /// Offset of `bytes` within the DATA section payload; relocation offsets
    /// are relative to that payload.
    pub section_offset: u32,
}

impl Segment {
    pub fn is_bss(&self) -> bool {
        self.name.starts_with(".bss")
    }

    pub fn is_rodata(&self) -> bool {
        self.name.starts_with(".rodata")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocKind {
    /// A data address stored in data (`R_WASM_MEMORY_ADDR_I32`).
    MemoryAddrI32,
    /// A function pointer stored in data (`R_WASM_TABLE_INDEX_I32`).
    TableIndexI32,
    Other,
}

#[derive(Debug, Clone, Copy)]
pub struct Reloc {
    pub kind: RelocKind,
    /// Offset within the DATA section payload.
    pub offset: u32,
    pub symbol: u32,
    pub addend: i64,
}

/// A relocation in the CODE section.
#[derive(Debug, Clone, Copy)]
pub struct CodeReloc {
    pub ty: RelocationType,
    /// Offset within the CODE section payload.
    pub offset: u32,
    pub symbol: u32,
    pub addend: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FuncType {
    pub params: Vec<ValType>,
    pub results: Vec<ValType>,
}

#[derive(Debug, Clone)]
pub struct FuncImport {
    pub module: String,
    pub name: String,
    pub ty: u32,
}

#[derive(Debug, Clone)]
pub struct Func {
    pub ty: u32,
    /// Body (locals + expression) as a range of the object's bytes.
    pub body: std::ops::Range<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct WasmObject {
    pub bytes: Vec<u8>,
    pub types: Vec<FuncType>,
    pub func_imports: Vec<FuncImport>,
    pub global_imports: Vec<String>,
    pub funcs: Vec<Func>,
    /// File offset of the CODE section payload (code relocation base).
    pub code_start: usize,
    pub symbols: Vec<Symbol>,
    pub segments: Vec<Segment>,
    pub data_relocs: Vec<Reloc>,
    pub code_relocs: Vec<CodeReloc>,
}

impl WasmObject {
    pub fn read(path: &std::path::Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(bytes).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parse(bytes: Vec<u8>) -> Result<Self> {
        let mut obj = WasmObject::default();
        let mut segment_info: Vec<(String, u32)> = Vec::new();
        let mut saw_linking = false;
        let mut func_types: Vec<u32> = Vec::new();

        for payload in Parser::new(0).parse_all(&bytes) {
            match payload? {
                Payload::TypeSection(reader) => {
                    for ty in reader.into_iter_err_on_gc_types() {
                        let ty = ty?;
                        obj.types.push(FuncType { params: ty.params().to_vec(), results: ty.results().to_vec() });
                    }
                }
                Payload::ImportSection(reader) => {
                    for import in reader.into_imports() {
                        let import = import?;
                        match import.ty {
                            TypeRef::Func(t) => obj.func_imports.push(FuncImport {
                                module: import.module.to_string(),
                                name: import.name.to_string(),
                                ty: t,
                            }),
                            TypeRef::Global(_) => obj.global_imports.push(import.name.to_string()),
                            _ => {}
                        }
                    }
                }
                Payload::FunctionSection(reader) => {
                    for t in reader {
                        func_types.push(t?);
                    }
                }
                Payload::CodeSectionStart { unchecked_range, .. } => obj.code_start = unchecked_range.start as usize,
                Payload::CodeSectionEntry(body) => {
                    let ty = *func_types.get(obj.funcs.len()).context("more bodies than functions")?;
                    let r = body.range();
                    obj.funcs.push(Func { ty, body: r.start as usize..r.end as usize });
                }
                Payload::DataSection(reader) => {
                    let section_start = reader.range().start;
                    for data in reader {
                        let data = data?;
                        let start = data.range.end - data.data.len() as u64;
                        obj.segments.push(Segment {
                            name: String::new(),
                            bytes: data.data.to_vec(),
                            align_log2: 0,
                            section_offset: (start - section_start) as u32,
                        });
                    }
                }
                Payload::CustomSection(reader) => match reader.as_known() {
                    KnownCustom::Linking(linking) => {
                        saw_linking = true;
                        for sub in linking.subsections() {
                            match sub? {
                                Linking::SegmentInfo(map) => {
                                    for seg in map {
                                        let seg = seg?;
                                        segment_info.push((seg.name.to_string(), seg.alignment));
                                    }
                                }
                                Linking::SymbolTable(map) => {
                                    for info in map {
                                        let s = symbol(info?, &obj.func_imports, &obj.global_imports);
                                        obj.symbols.push(s);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    KnownCustom::Reloc(relocs) if reader.name() == "reloc.DATA" => {
                        for entry in relocs.entries() {
                            let entry = entry?;
                            obj.data_relocs.push(Reloc {
                                kind: match entry.ty {
                                    RelocationType::MemoryAddrI32 => RelocKind::MemoryAddrI32,
                                    RelocationType::TableIndexI32 => RelocKind::TableIndexI32,
                                    _ => RelocKind::Other,
                                },
                                offset: entry.offset,
                                symbol: entry.index,
                                addend: entry.addend,
                            });
                        }
                    }
                    KnownCustom::Reloc(relocs) if reader.name() == "reloc.CODE" => {
                        for entry in relocs.entries() {
                            let entry = entry?;
                            obj.code_relocs.push(CodeReloc {
                                ty: entry.ty,
                                offset: entry.offset,
                                symbol: entry.index,
                                addend: entry.addend,
                            });
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        if !saw_linking {
            bail!("not a relocatable wasm object (no linking section)");
        }
        for (seg, (name, align)) in obj.segments.iter_mut().zip(segment_info) {
            seg.name = name;
            seg.align_log2 = align;
        }
        obj.code_relocs.sort_by_key(|r| r.offset);
        obj.bytes = bytes;
        Ok(obj)
    }

    /// Bytes of a defined data symbol.
    pub fn data_bytes(&self, def: DataDef) -> &[u8] {
        let seg = &self.segments[def.segment as usize];
        &seg.bytes[def.offset as usize..(def.offset + def.size) as usize]
    }

    /// Data relocations that land inside a defined data symbol, with their
    /// offset relative to the symbol's start.
    pub fn relocs_in(&self, def: DataDef) -> impl Iterator<Item = (u32, &Reloc)> {
        let seg = &self.segments[def.segment as usize];
        let start = seg.section_offset + def.offset;
        let end = start + def.size;
        self.data_relocs
            .iter()
            .filter(move |r| r.offset >= start && r.offset < end)
            .map(move |r| (r.offset - start, r))
    }

    pub fn segment_of(&self, def: DataDef) -> &Segment {
        &self.segments[def.segment as usize]
    }

    /// Function type of a function index (imports first).
    pub fn func_type(&self, index: u32) -> &FuncType {
        let n = self.func_imports.len() as u32;
        let t = if index < n { self.func_imports[index as usize].ty } else { self.funcs[(index - n) as usize].ty };
        &self.types[t as usize]
    }

    pub fn num_imported_funcs(&self) -> u32 {
        self.func_imports.len() as u32
    }

    /// The symbol naming a function index, preferring a defined one.
    pub fn func_symbol(&self, index: u32) -> Option<&Symbol> {
        self.symbols.iter().find(|s| s.kind == SymbolKind::Function && s.index == Some(index))
    }

    /// Code relocations inside a body's byte range (offsets rebased to the
    /// file).
    pub fn relocs_for(&self, body: &std::ops::Range<usize>) -> Vec<CodeReloc> {
        let lo = (body.start - self.code_start) as u32;
        let hi = (body.end - self.code_start) as u32;
        let i = self.code_relocs.partition_point(|r| r.offset < lo);
        self.code_relocs[i..].iter().take_while(|r| r.offset < hi).copied().collect()
    }
}

/// What a clang-emitted function name stands for on the guest side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameClass<'a> {
    /// A guest symbol (clang's wasm renaming of `main` undone).
    Guest(&'a str),
    /// clang's wasm `main` wrappers (`__main_void`, `__main_argc_argv`).
    MainWrapper,
    /// `.L<f>_bitcast[.N]`: a thunk clang emits when a call's signature
    /// differs from the callee's declaration — exactly the calls that need
    /// o32 indirect-ABI adapters at link time.
    SignatureThunk { target: &'a str },
}

pub fn classify_name(name: &str) -> NameClass<'_> {
    match name {
        "__original_main" => return NameClass::Guest("main"),
        "__main_void" | "__main_argc_argv" => return NameClass::MainWrapper,
        _ => {}
    }
    if let Some(rest) = name.strip_prefix(".L") {
        let base = rest.rsplit_once('.').filter(|(_, n)| n.bytes().all(|b| b.is_ascii_digit())).map_or(rest, |(b, _)| b);
        if let Some(target) = base.strip_suffix("_bitcast") {
            return NameClass::SignatureThunk { target };
        }
    }
    NameClass::Guest(name)
}

fn symbol(info: SymbolInfo<'_>, func_imports: &[FuncImport], global_imports: &[String]) -> Symbol {
    let (kind, flags, name, index, data) = match info {
        SymbolInfo::Func { flags, index, name } => {
            let name = name
                .map(str::to_string)
                .or_else(|| func_imports.get(index as usize).map(|i| i.name.clone()))
                .unwrap_or_default();
            (SymbolKind::Function, flags, name, Some(index), None)
        }
        SymbolInfo::Data { flags, name, symbol } => {
            let def = symbol.map(|d| DataDef { segment: d.index, offset: d.offset as u32, size: d.size as u32 });
            (SymbolKind::Data, flags, name.to_string(), None, def)
        }
        SymbolInfo::Global { flags, index, name } => {
            let name = name
                .map(str::to_string)
                .or_else(|| global_imports.get(index as usize).cloned())
                .unwrap_or_default();
            (SymbolKind::Global, flags, name, Some(index), None)
        }
        SymbolInfo::Section { flags, .. } => (SymbolKind::Other, flags, String::new(), None, None),
        SymbolInfo::Event { flags, .. } => (SymbolKind::Other, flags, String::new(), None, None),
        SymbolInfo::Table { flags, name, index } => {
            (SymbolKind::Other, flags, name.unwrap_or_default().to_string(), Some(index), None)
        }
    };
    Symbol {
        kind,
        name,
        defined: !flags.contains(SymbolFlags::UNDEFINED),
        local: flags.contains(SymbolFlags::BINDING_LOCAL),
        weak: flags.contains(SymbolFlags::BINDING_WEAK),
        index,
        data,
    }
}
