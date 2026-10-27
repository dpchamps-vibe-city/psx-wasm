//! The debug build's merge (D53): `guest.wasm` and `runtime.wasm` joined as
//! `wasm-merge` joins them, without binaryen, which can't carry DWARF
//! through. The guest's `psx.*` imports become calls to the runtime's
//! exports and the runtime's `guest.*` imports the guest's; `host.*` stays
//! imported. Guest functions come first, re-encoded instruction by
//! instruction, and [`Merged::code`] says where each instruction landed,
//! so the guest's DWARF can follow ([`crate::dwarf::Dwarf::add_module`]).
//! Nothing is optimized: the guest's C was, by clang.

use crate::dwarf::{CodeMap, FuncMap, body_starts};
use anyhow::{Context as _, Result, bail, ensure};
use std::collections::{BTreeMap, HashMap};
use wasm_encoder::reencode::{Error, Reencode};
use wasm_encoder::{
    CodeSection, CustomSection, DataCountSection, DataSection, ElementSection, EntityType, ExportSection,
    FunctionSection, GlobalSection, ImportSection, MemorySection, Module, NameMap, NameSection, StartSection,
    TableSection, TagSection, TypeSection,
};
use wasmparser::{ExternalKind, KnownCustom, Name, Parser, Payload, TypeRef};

pub struct Merged {
    pub wasm: Vec<u8>,
    /// The guest's code addresses to the merged module's.
    pub code: CodeMap,
}

/// One input module's sections.
#[derive(Default)]
struct Input<'a> {
    types: Option<wasmparser::TypeSectionReader<'a>>,
    imports: Vec<(String, String, TypeRef)>,
    funcs: Option<wasmparser::FunctionSectionReader<'a>>,
    tables: Option<wasmparser::TableSectionReader<'a>>,
    memories: Option<wasmparser::MemorySectionReader<'a>>,
    tags: Option<wasmparser::TagSectionReader<'a>>,
    globals: Option<wasmparser::GlobalSectionReader<'a>>,
    exports: Vec<(String, ExternalKind, u32)>,
    start: Option<u32>,
    elements: Option<wasmparser::ElementSectionReader<'a>>,
    data: Option<wasmparser::DataSectionReader<'a>>,
    data_count: Option<u32>,
    bodies: Vec<wasmparser::FunctionBody<'a>>,
    names: Vec<(u32, String)>,
    customs: Vec<(String, &'a [u8])>,
    counts: Counts,
}

/// How many of each the module defines, imports included.
#[derive(Default, Clone, Copy)]
struct Counts {
    types: u32,
    func_imports: u32,
    funcs: u32,
    tables: u32,
    tags: u32,
    globals: u32,
    elements: u32,
    data: u32,
}

fn parse(wasm: &[u8]) -> Result<Input<'_>> {
    let mut m = Input::default();
    for p in Parser::new(0).parse_all(wasm) {
        match p? {
            Payload::TypeSection(r) => {
                m.counts.types = r.count();
                m.types = Some(r);
            }
            Payload::ImportSection(r) => {
                for i in r.into_imports() {
                    let i = i?;
                    if matches!(i.ty, TypeRef::Func(_)) {
                        m.counts.func_imports += 1;
                    }
                    m.imports.push((i.module.to_string(), i.name.to_string(), i.ty));
                }
            }
            Payload::FunctionSection(r) => {
                m.counts.funcs = r.count();
                m.funcs = Some(r);
            }
            Payload::TableSection(r) => {
                m.counts.tables = r.count();
                m.tables = Some(r);
            }
            Payload::MemorySection(r) => m.memories = Some(r),
            Payload::TagSection(r) => {
                m.counts.tags = r.count();
                m.tags = Some(r);
            }
            Payload::GlobalSection(r) => {
                m.counts.globals = r.count();
                m.globals = Some(r);
            }
            Payload::ExportSection(r) => {
                for e in r {
                    let e = e?;
                    m.exports.push((e.name.to_string(), e.kind, e.index));
                }
            }
            Payload::StartSection { func, .. } => m.start = Some(func),
            Payload::ElementSection(r) => {
                m.counts.elements = r.count();
                m.elements = Some(r);
            }
            Payload::DataCountSection { count, .. } => m.data_count = Some(count),
            Payload::DataSection(r) => {
                m.counts.data = r.count();
                m.data = Some(r);
            }
            Payload::CodeSectionEntry(b) => m.bodies.push(b),
            Payload::CustomSection(c) => match c.as_known() {
                KnownCustom::Name(names) => {
                    for n in names {
                        if let Name::Function(map) = n? {
                            for naming in map {
                                let naming = naming?;
                                m.names.push((naming.index, naming.name.to_string()));
                            }
                        }
                    }
                }
                _ if c.name().starts_with(".debug_") || matches!(c.name(), "producers" | "target_features") => {}
                _ => m.customs.push((c.name().to_string(), c.data())),
            },
            _ => {}
        }
    }
    Ok(m)
}

/// One input's indices in the merged module.
struct Remap {
    funcs: Vec<u32>,
    types: u32,
    tables: u32,
    tags: u32,
    globals: u32,
    elements: u32,
    data: u32,
}

impl Reencode for Remap {
    type Error = std::convert::Infallible;
    fn function_index(&mut self, f: u32) -> Result<u32, Error<Self::Error>> {
        Ok(self.funcs[f as usize])
    }
    fn type_index(&mut self, t: u32) -> Result<u32, Error<Self::Error>> {
        Ok(self.types + t)
    }
    fn table_index(&mut self, t: u32) -> Result<u32, Error<Self::Error>> {
        Ok(self.tables + t)
    }
    fn tag_index(&mut self, t: u32) -> Result<u32, Error<Self::Error>> {
        Ok(self.tags + t)
    }
    fn global_index(&mut self, g: u32) -> Result<u32, Error<Self::Error>> {
        Ok(self.globals + g)
    }
    fn element_index(&mut self, e: u32) -> Result<u32, Error<Self::Error>> {
        Ok(self.elements + e)
    }
    fn data_index(&mut self, d: u32) -> Result<u32, Error<Self::Error>> {
        Ok(self.data + d)
    }
    fn memory_index(&mut self, _: u32) -> Result<u32, Error<Self::Error>> {
        Ok(0)
    }
}

fn re<E: std::fmt::Debug>(e: Error<E>) -> anyhow::Error {
    anyhow::anyhow!("re-encoding: {e:?}")
}

/// Merge the guest (module name `guest`, importing `psx`) with the runtime
/// (module name `psx`, importing `guest` and `host`).
pub fn merge(guest: &[u8], runtime: &[u8]) -> Result<Merged> {
    let g = parse(guest).context("guest module")?;
    let r = parse(runtime).context("runtime module")?;
    for (m, i) in g.imports.iter().map(|i| ("guest", i)).chain(r.imports.iter().map(|i| ("runtime", i))) {
        let (module, name, ty) = i;
        let ok = match ty {
            TypeRef::Func(_) => true,
            TypeRef::Memory(_) => m == "runtime" && module == psxw_abi::guest::MODULE,
            _ => false,
        };
        ensure!(ok, "the {m} module imports {module}.{name} ({ty:?}), which the merge doesn't join");
    }
    ensure!(g.start.is_none() || r.start.is_none(), "both modules have a start function");

    // Function indices: the host imports, the guest's functions, the
    // runtime's.
    let host: Vec<&(String, String, TypeRef)> = r.imports.iter().filter(|(m, _, _)| m == psxw_abi::host::MODULE).collect();
    let nh = host.len() as u32;
    let (gd, rd) = (g.counts.funcs, r.counts.funcs);
    let export = |m: &Input, name: &str| m.exports.iter().find(|(n, k, _)| n == name && *k == ExternalKind::Func).map(|e| e.2);
    let mut rf: Vec<u32> = Vec::new();
    let mut gf: Vec<u32> = Vec::new();
    // The runtime's guest imports name guest exports, resolved once the
    // guest's own map exists.
    let mut pending: Vec<(usize, u32)> = Vec::new();
    for (module, name, ty) in &r.imports {
        if !matches!(ty, TypeRef::Func(_)) {
            continue;
        }
        if module == psxw_abi::host::MODULE {
            rf.push(host.iter().position(|(m, n, _)| m == module && n == name).unwrap_or_default() as u32);
        } else if module == psxw_abi::guest::MODULE {
            let i = export(&g, name).with_context(|| format!("the runtime imports guest.{name}, which the guest doesn't export"))?;
            pending.push((rf.len(), i));
            rf.push(0);
        } else {
            bail!("the runtime imports {module}.{name}");
        }
    }
    rf.extend((0..rd).map(|d| nh + gd + d));
    for (module, name, _) in g.imports.iter().filter(|(_, _, t)| matches!(t, TypeRef::Func(_))) {
        ensure!(module == psxw_abi::psx::MODULE, "the guest imports {module}.{name}");
        let i = export(&r, name).with_context(|| format!("the guest imports psx.{name}, which the runtime doesn't export"))?;
        ensure!(i >= r.counts.func_imports, "the runtime's psx.{name} is itself an import");
        gf.push(rf[i as usize]);
    }
    gf.extend((0..gd).map(|d| nh + d));
    for (at, i) in pending {
        rf[at] = gf[i as usize];
    }

    let mut gm = Remap { funcs: gf, types: 0, tables: 0, tags: 0, globals: 0, elements: 0, data: 0 };
    let c = g.counts;
    let mut rm = Remap {
        funcs: rf,
        types: c.types,
        tables: c.tables,
        tags: c.tags,
        globals: c.globals,
        elements: c.elements,
        data: c.data,
    };

    let mut module = Module::new();
    let mut types = TypeSection::new();
    for (m, s) in [(&mut gm, &g.types), (&mut rm, &r.types)] {
        if let Some(s) = s.clone() {
            m.parse_type_section(&mut types, s).map_err(re)?;
        }
    }
    module.section(&types);
    let mut imports = ImportSection::new();
    for (module, name, ty) in &host {
        let TypeRef::Func(t) = ty else { unreachable!() };
        imports.import(module, name, EntityType::Function(rm.types + t));
    }
    module.section(&imports);
    let mut functions = FunctionSection::new();
    for (m, s) in [(&mut gm, &g.funcs), (&mut rm, &r.funcs)] {
        if let Some(s) = s.clone() {
            m.parse_function_section(&mut functions, s).map_err(re)?;
        }
    }
    module.section(&functions);
    let mut tables = TableSection::new();
    for (m, s) in [(&mut gm, &g.tables), (&mut rm, &r.tables)] {
        if let Some(s) = s.clone() {
            m.parse_table_section(&mut tables, s).map_err(re)?;
        }
    }
    module.section(&tables);
    let mut memories = MemorySection::new();
    gm.parse_memory_section(&mut memories, g.memories.clone().context("the guest defines no memory")?).map_err(re)?;
    module.section(&memories);
    let mut tags = TagSection::new();
    for (m, s) in [(&mut gm, &g.tags), (&mut rm, &r.tags)] {
        if let Some(s) = s.clone() {
            m.parse_tag_section(&mut tags, s).map_err(re)?;
        }
    }
    module.section(&tags);
    let mut globals = GlobalSection::new();
    for (m, s) in [(&mut gm, &g.globals), (&mut rm, &r.globals)] {
        if let Some(s) = s.clone() {
            m.parse_global_section(&mut globals, s).map_err(re)?;
        }
    }
    module.section(&globals);
    let mut exports = ExportSection::new();
    let mut seen: HashMap<&str, ()> = HashMap::new();
    for (m, e) in [(&mut gm, &g.exports), (&mut rm, &r.exports)] {
        for (name, kind, index) in e {
            ensure!(seen.insert(name, ()).is_none(), "both modules export {name}");
            let index = match kind {
                ExternalKind::Func => m.function_index(*index),
                ExternalKind::Table => m.table_index(*index),
                ExternalKind::Memory => Ok(0),
                ExternalKind::Global => m.global_index(*index),
                ExternalKind::Tag => m.tag_index(*index),
                ExternalKind::FuncExact => m.function_index(*index),
            }
            .map_err(re)?;
            exports.export(name, m.export_kind(*kind).map_err(re)?, index);
        }
    }
    module.section(&exports);
    if let Some(s) = g.start.map(|s| gm.function_index(s)).or(r.start.map(|s| rm.function_index(s))) {
        module.section(&StartSection { function_index: s.map_err(re)? });
    }
    let mut elements = ElementSection::new();
    for (m, s) in [(&mut gm, &g.elements), (&mut rm, &r.elements)] {
        if let Some(s) = s.clone() {
            m.parse_element_section(&mut elements, s).map_err(re)?;
        }
    }
    module.section(&elements);
    if g.data_count.is_some() || r.data_count.is_some() {
        module.section(&DataCountSection { count: g.counts.data + r.counts.data });
    }

    // The guest's bodies with where each instruction landed, then the
    // runtime's.
    let mut code = CodeSection::new();
    let mut moved = Vec::new();
    for body in &g.bodies {
        let mut f = gm.new_function_with_parsed_locals(body).map_err(re)?;
        let start = body.range().start;
        let mut ops = Vec::new();
        let mut reader = body.get_operators_reader()?;
        while !reader.eof() {
            ops.push(((reader.original_position() - start) as u32, f.byte_len() as u32));
            let ins = gm.parse_instruction(&mut reader).map_err(re)?;
            f.instruction(&ins);
        }
        ops.push(((body.range().end - start) as u32, f.byte_len() as u32));
        code.function(&f);
        moved.push(ops);
    }
    for body in &r.bodies {
        rm.parse_function_body(&mut code, body.clone()).map_err(re)?;
    }
    module.section(&code);
    let mut data = DataSection::new();
    for (m, s) in [(&mut gm, &g.data), (&mut rm, &r.data)] {
        if let Some(s) = s.clone() {
            m.parse_data_section(&mut data, s).map_err(re)?;
        }
    }
    module.section(&data);

    // Function names: the host imports by their runtime names.
    let mut names: BTreeMap<u32, String> = BTreeMap::new();
    for (m, n) in [(&mut gm, &g.names), (&mut rm, &r.names)] {
        for (i, name) in n {
            names.entry(m.function_index(*i).map_err(re)?).or_insert_with(|| name.clone());
        }
    }
    let mut fnames = NameMap::new();
    for (i, n) in &names {
        fnames.append(*i, n);
    }
    let mut ns = NameSection::new();
    ns.functions(&fnames);
    module.section(&ns);
    let mut customs: Vec<&str> = Vec::new();
    for (name, data) in g.customs.iter().chain(&r.customs) {
        if !customs.contains(&name.as_str()) {
            customs.push(name);
            module.section(&CustomSection { name: name.into(), data: (*data).into() });
        }
    }
    let wasm = module.finish();

    let (from, to) = (body_starts(guest)?, body_starts(&wasm)?);
    let funcs = moved.into_iter().enumerate().map(|(d, ops)| FuncMap { from: from[d], to: to[d], ops }).collect();
    Ok(Merged { wasm, code: CodeMap::new(funcs) })
}
