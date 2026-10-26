//! Reader for relocatable wasm objects (tool-conventions `Linking.md`): the
//! symbol table, data segments with their bytes, and data relocations. This
//! is the input side of the final link; the census uses it to see what each
//! translation unit defines and references.

use anyhow::{Context, Result, bail};
use wasmparser::{
    KnownCustom, Linking, Parser, Payload, RelocationType, SymbolFlags, SymbolInfo, TypeRef,
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
    /// Offset of `bytes` within the DATA section payload; relocation offsets
    /// are relative to that payload.
    pub section_offset: u32,
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

#[derive(Debug, Clone, Default)]
pub struct WasmObject {
    pub symbols: Vec<Symbol>,
    pub segments: Vec<Segment>,
    pub data_relocs: Vec<Reloc>,
}

impl WasmObject {
    pub fn read(path: &std::path::Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&bytes).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let mut obj = WasmObject::default();
        let mut func_imports: Vec<String> = Vec::new();
        let mut global_imports: Vec<String> = Vec::new();
        let mut segment_names: Vec<String> = Vec::new();
        let mut saw_linking = false;

        for payload in Parser::new(0).parse_all(bytes) {
            match payload? {
                Payload::ImportSection(reader) => {
                    for import in reader.into_imports() {
                        let import = import?;
                        match import.ty {
                            TypeRef::Func(_) => func_imports.push(import.name.to_string()),
                            TypeRef::Global(_) => global_imports.push(import.name.to_string()),
                            _ => {}
                        }
                    }
                }
                Payload::DataSection(reader) => {
                    let section_start = reader.range().start;
                    for data in reader {
                        let data = data?;
                        let start = data.range.end - data.data.len() as u64;
                        obj.segments.push(Segment {
                            name: String::new(),
                            bytes: data.data.to_vec(),
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
                                        segment_names.push(seg?.name.to_string());
                                    }
                                }
                                Linking::SymbolTable(map) => {
                                    for info in map {
                                        obj.symbols.push(symbol(info?, &func_imports, &global_imports));
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
                    _ => {}
                },
                _ => {}
            }
        }
        if !saw_linking {
            bail!("not a relocatable wasm object (no linking section)");
        }
        for (seg, name) in obj.segments.iter_mut().zip(segment_names) {
            seg.name = name;
        }
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

fn symbol(info: SymbolInfo<'_>, func_imports: &[String], global_imports: &[String]) -> Symbol {
    let (kind, flags, name, data) = match info {
        SymbolInfo::Func { flags, index, name } => {
            let name = name
                .map(str::to_string)
                .or_else(|| func_imports.get(index as usize).cloned())
                .unwrap_or_default();
            (SymbolKind::Function, flags, name, None)
        }
        SymbolInfo::Data { flags, name, symbol } => {
            let def = symbol.map(|d| DataDef { segment: d.index, offset: d.offset, size: d.size });
            (SymbolKind::Data, flags, name.to_string(), def)
        }
        SymbolInfo::Global { flags, index, name } => {
            let name = name
                .map(str::to_string)
                .or_else(|| global_imports.get(index as usize).cloned())
                .unwrap_or_default();
            (SymbolKind::Global, flags, name, None)
        }
        SymbolInfo::Section { flags, .. } => (SymbolKind::Other, flags, String::new(), None),
        SymbolInfo::Event { flags, .. } => (SymbolKind::Other, flags, String::new(), None),
        SymbolInfo::Table { flags, name, .. } => {
            (SymbolKind::Other, flags, name.unwrap_or_default().to_string(), None)
        }
    };
    Symbol {
        kind,
        name,
        defined: !flags.contains(SymbolFlags::UNDEFINED),
        local: flags.contains(SymbolFlags::BINDING_LOCAL),
        weak: flags.contains(SymbolFlags::BINDING_WEAK),
        data,
    }
}
