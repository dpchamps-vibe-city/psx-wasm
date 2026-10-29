//! Which guest functions Asyncify instruments (D57): every function that
//! can be on the wasm stack when a guest thread switches. ChangeThread
//! suspends the running thread by unwinding its stack into a buffer, and
//! resumes it by rewinding, so each frame between the thread's root and
//! the switch must be able to save and restore itself.
//!
//! The scope is the call graph's reverse closure from the functions that
//! make the switch (the seeds), through direct calls. An indirect call
//! can reach any function in the dispatch table, so every function that
//! makes one is in the scope too. Functions with exception handlers
//! (`guest_invoke_irq`) are left out: Asyncify can't instrument them, and
//! the runtime refuses a switch from inside an interrupt handler.

use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use wasmparser::{KnownCustom, Name, Operator, Parser, Payload};

#[derive(Debug, Default)]
pub struct Scope {
    /// Function indices to instrument, imports counted.
    pub instrument: BTreeSet<u32>,
    /// In the closure but not instrumentable (exception handlers).
    pub excluded: BTreeSet<u32>,
    /// Defined functions.
    pub defined: usize,
}

/// The scope in `wasm` for seeds named (name section) `seeds`.
pub fn scope(wasm: &[u8], seeds: &BTreeSet<String>) -> Result<Scope> {
    let mut imported = 0u32;
    let mut names: BTreeMap<String, u32> = BTreeMap::new();
    let mut callers: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    let mut indirect: BTreeSet<u32> = BTreeSet::new();
    let mut handlers: BTreeSet<u32> = BTreeSet::new();
    let mut next = 0u32;
    for payload in Parser::new(0).parse_all(wasm) {
        match payload? {
            Payload::ImportSection(r) => {
                for i in r.into_imports() {
                    if matches!(i?.ty, wasmparser::TypeRef::Func(_) | wasmparser::TypeRef::FuncExact(_)) {
                        imported += 1;
                    }
                }
                next = imported;
            }
            Payload::CodeSectionEntry(body) => {
                let me = next;
                next += 1;
                let mut ops = body.get_operators_reader()?;
                while !ops.eof() {
                    match ops.read()? {
                        Operator::Call { function_index } | Operator::ReturnCall { function_index } => {
                            callers.entry(function_index).or_default().insert(me);
                        }
                        Operator::CallIndirect { .. } | Operator::ReturnCallIndirect { .. } | Operator::CallRef { .. } => {
                            indirect.insert(me);
                        }
                        Operator::TryTable { .. } | Operator::Try { .. } => {
                            handlers.insert(me);
                        }
                        _ => {}
                    }
                }
            }
            Payload::CustomSection(c) => {
                if let KnownCustom::Name(r) = c.as_known() {
                    for n in r {
                        if let Name::Function(map) = n? {
                            for naming in map {
                                let naming = naming?;
                                names.insert(naming.name.to_string(), naming.index);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let mut work: Vec<u32> = seeds
        .iter()
        .map(|s| names.get(s).copied().with_context(|| format!("no function named {s} in the guest module")))
        .collect::<Result<_>>()?;
    work.extend(&indirect);
    let mut seen: BTreeSet<u32> = BTreeSet::new();
    while let Some(f) = work.pop() {
        if !seen.insert(f) {
            continue;
        }
        work.extend(callers.get(&f).into_iter().flatten().copied());
    }
    let (excluded, instrument) = seen.into_iter().partition(|f| handlers.contains(f));
    Ok(Scope { instrument, excluded, defined: (next - imported) as usize })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_encoder::{
        CodeSection, ExportKind, ExportSection, Function, FunctionSection, IndirectNameMap, Instruction as I, Module, NameMap,
        NameSection, TableSection, TableType, TypeSection,
    };

    /// main → yield → switch; worker → helper; caller → [indirect];
    /// leaf is called by nothing that switches.
    #[test]
    fn closure_through_direct_and_indirect_calls() {
        let mut m = Module::new();
        let mut types = TypeSection::new();
        types.ty().function([], []);
        m.section(&types);
        let mut funcs = FunctionSection::new();
        let bodies: Vec<Vec<I>> = vec![
            vec![I::Call(1)],                                    // 0 main
            vec![I::Call(2)],                                    // 1 yield
            vec![],                                              // 2 switch (seed)
            vec![I::Call(4)],                                    // 3 worker
            vec![I::Call(5)],                                    // 4 helper
            vec![],                                              // 5 leaf
            vec![I::I32Const(0), I::CallIndirect { type_index: 0, table_index: 0 }], // 6 indirect
            vec![I::Call(6)],                                    // 7 caller of indirect
        ];
        for _ in &bodies {
            funcs.function(0);
        }
        m.section(&funcs);
        let mut tables = TableSection::new();
        tables.table(TableType {
            element_type: wasm_encoder::RefType::FUNCREF,
            minimum: 1,
            maximum: None,
            table64: false,
            shared: false,
        });
        m.section(&tables);
        let mut ex = ExportSection::new();
        ex.export("main", ExportKind::Func, 0);
        m.section(&ex);
        let mut code = CodeSection::new();
        for b in &bodies {
            let mut f = Function::new([]);
            for i in b {
                f.instruction(i);
            }
            f.instruction(&I::End);
            code.function(&f);
        }
        m.section(&code);
        let mut fnames = NameMap::new();
        for (i, n) in ["main", "yield", "switch", "worker", "helper", "leaf", "indirect", "caller"].iter().enumerate() {
            fnames.append(i as u32, n);
        }
        let mut ns = NameSection::new();
        ns.functions(&fnames);
        ns.locals(&IndirectNameMap::new());
        m.section(&ns);
        let wasm = m.finish();
        let s = scope(&wasm, &["switch".to_string()].into()).unwrap();
        assert_eq!(s.instrument.into_iter().collect::<Vec<_>>(), [0, 1, 2, 6, 7]);
        assert_eq!(s.defined, 8);
        assert!(scope(&wasm, &["nothing".to_string()].into()).is_err());
    }
}
