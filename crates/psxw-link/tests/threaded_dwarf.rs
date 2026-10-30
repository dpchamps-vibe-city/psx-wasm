//! Producer-repair controls, including the actual Rust merge. Opt-in because
//! building Binaryen from source is not part of a normal workspace test run.
//! Real-game/browser acceptance is recorded in toolchain/binaryen-dwarf/.
use gimli::{EndianSlice, LittleEndian, Operation};
use psxw_inputs::Workspace;
use psxw_link::{dwarf, merge};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::process::Command;
use wasm_encoder::{
    CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection, ImportSection, Instruction as I, Module, TypeSection,
    ValType,
};
use wasmparser::{Parser, Payload};

fn run(command: &mut Command) -> Vec<u8> {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn executable_sections(wasm: &[u8]) -> Vec<(u8, Vec<u8>)> {
    Parser::new(0)
        .parse_all(wasm)
        .map(|p| p.unwrap())
        .filter_map(|p| {
            let (id, range) = p.as_section()?;
            (id != 0).then(|| (id, wasm[range.start as usize..range.end as usize].to_vec()))
        })
        .collect()
}

/// Derive the breakpoint and variable location from DWARF, never a magic PC
/// or a hardcoded Flatten local. Use the source row from the C fixture.
fn source_point(wasm: &[u8], variable: &str, line: u64) -> Option<Value> {
    let sections = dwarf::sections(wasm).unwrap();
    let read = gimli::Dwarf::load(|id| -> Result<_, gimli::Error> {
        Ok(EndianSlice::new(sections.iter().find(|(n, _)| n == id.name()).map_or(&[][..], |(_, b)| b.as_slice()), LittleEndian))
    })
    .unwrap();
    let mut units = read.units();
    while let Some(header) = units.next().unwrap() {
        let unit = read.unit(header).unwrap();
        let mut entries = unit.entries();
        while let Some(entry) = entries.next_dfs().unwrap() {
            let Some(name) = entry.attr_value(gimli::DW_AT_name) else {
                continue;
            };
            if read.attr_string(&unit, name).unwrap().to_string_lossy() != variable {
                continue;
            }
            let Some(attr) = entry.attr_value(gimli::DW_AT_location) else {
                return None;
            };
            let mut locations = read.attr_locations(&unit, attr).unwrap().unwrap();
            let mut rows = unit.line_program.clone().unwrap().rows();
            while let Some((_, row)) = rows.next_row().unwrap() {
                if row.end_sequence() || row.line().map(|l| l.get()) != Some(line) {
                    continue;
                }
                while let Some(loc) = locations.next().unwrap() {
                    if loc.range.begin <= row.address() && row.address() < loc.range.end {
                        let mut ops = loc.data.operations(unit.encoding());
                        let (kind, index) = match ops.next().unwrap().unwrap() {
                            Operation::WasmLocal { index } => ("local", index),
                            Operation::WasmStack { index } => ("stack", index),
                            op => panic!("unexpected resumed location: {op:?}"),
                        };
                        assert_eq!(ops.next().unwrap(), Some(Operation::StackValue));
                        assert_eq!(ops.next().unwrap(), None);
                        return Some(json!({"pc": row.address(), "kind": kind, "index": index}));
                    }
                }
                // Restart for another row of the same source line.
                locations = read.attr_locations(&unit, attr).unwrap().unwrap();
            }
        }
    }
    None
}

/// Statement rows per source line.
fn statements(wasm: &[u8]) -> BTreeMap<u64, usize> {
    let sections = dwarf::sections(wasm).unwrap();
    let read = gimli::Dwarf::load(|id| -> Result<_, gimli::Error> {
        Ok(EndianSlice::new(sections.iter().find(|(n, _)| n == id.name()).map_or(&[][..], |(_, b)| b.as_slice()), LittleEndian))
    })
    .unwrap();
    let mut out = BTreeMap::new();
    let mut units = read.units();
    while let Some(header) = units.next().unwrap() {
        let unit = read.unit(header).unwrap();
        let mut rows = unit.line_program.clone().unwrap().rows();
        while let Some((_, row)) = rows.next_row().unwrap() {
            if let (true, false, Some(line)) = (row.is_stmt(), row.end_sequence(), row.line()) {
                *out.entry(line.get()).or_default() += 1;
            }
        }
    }
    out
}

fn sanitize_input(wasm: &[u8]) -> Vec<u8> {
    let mut funcs = Vec::new();
    let mut start = 0;
    for p in Parser::new(0).parse_all(wasm) {
        match p.unwrap() {
            Payload::CodeSectionStart { unchecked_range, .. } => start = unchecked_range.start,
            Payload::CodeSectionEntry(body) => {
                let from = (body.range().start - start) as u32;
                funcs.push(dwarf::FuncMap {
                    from,
                    to: from,
                    ops: (0..=(body.range().end - body.range().start) as u32).map(|i| (i, i)).collect(),
                });
            }
            _ => {}
        }
    }
    let mut debug = dwarf::Dwarf::new();
    debug.add_input_module(wasm, &dwarf::CodeMap::new(funcs)).unwrap();
    let mut module = Module::new();
    for p in Parser::new(0).parse_all(wasm) {
        let p = p.unwrap();
        if matches!(&p, Payload::CustomSection(c) if c.name().starts_with(".debug_")) {
            continue;
        }
        if let Some((id, range)) = p.as_section() {
            module.section(&wasm_encoder::RawSection { id, data: &wasm[range.start as usize..range.end as usize] });
        }
    }
    for (name, bytes) in debug.finish().unwrap() {
        module.section(&wasm_encoder::CustomSection { name: name.into(), data: bytes.into() });
    }
    module.finish()
}

fn runtime() -> Vec<u8> {
    let mut module = Module::new();
    let mut types = TypeSection::new();
    types.ty().function([ValType::I32], [ValType::I32]);
    module.section(&types);
    let mut imports = ImportSection::new();
    imports.import("host", "bios", EntityType::Function(0));
    module.section(&imports);
    let mut functions = FunctionSection::new();
    functions.function(0);
    module.section(&functions);
    let mut exports = ExportSection::new();
    exports.export("bios", ExportKind::Func, 1);
    module.section(&exports);
    let mut code = CodeSection::new();
    let mut f = Function::new([]);
    f.instruction(&I::LocalGet(0)).instruction(&I::Call(0)).instruction(&I::End);
    code.function(&f);
    module.section(&code);
    module.finish()
}

fn merged(wasm: &[u8]) -> Vec<u8> {
    let merged = merge::merge(wasm, &runtime()).unwrap();
    let mut debug = dwarf::Dwarf::new();
    debug.add_module(&dwarf::sections(wasm).unwrap(), &merged.code).unwrap();
    let mut module = Module::new();
    // Append custom sections to the complete merge without re-encoding code.
    let mut result = merged.wasm;
    for (name, bytes) in debug.finish().unwrap() {
        module.section(&wasm_encoder::CustomSection { name: name.into(), data: bytes.into() });
    }
    result.extend_from_slice(&module.finish()[8..]);
    result
}

#[test]
#[ignore = "builds the debug Binaryen in Docker; needs the pinned clang and Node"]
fn real_suspend_variables_and_rust_merge() {
    let ws = Workspace::discover().unwrap();
    let candidate = psxw_inputs::toolchain::dwarf_binaryen(&ws).unwrap();
    let baseline = ws.tool("binaryen", "bin/wasm-opt").unwrap();
    let clang = ws.clang().unwrap();
    let verify = clang.with_file_name("llvm-dwarfdump");
    let dir = ws.out_dir().join("threaded-dwarf-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let source = include_str!("../../../toolchain/binaryen-dwarf/tests/fixture.c");
    for (name, wide, fast, repro) in [
        ("ordinary", false, false, false),
        ("wide", true, false, false),
        ("fast", false, true, false),
        ("input-reproducer", false, true, true),
    ] {
        let dir = dir.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        // Keep >127 live function parameters, so Flatten's spill local index
        // needs a multi-byte ULEB and the rewritten lists change byte lengths.
        let source = if wide {
            let params = (0..160).map(|i| format!(", int unused{i}")).collect::<String>();
            let args = ", 0".repeat(160);
            source
                .replace("worker(int input)", &format!("worker(int input{params})"))
                .replace("worker(input)", &format!("worker(input{args})"))
                .replace("worker(input + 1)", &format!("worker(input + 1{args})"))
        } else {
            source.to_string()
        };
        let c = dir.join("fixture.c");
        let line_of = |marker: &str| source.lines().position(|s| s.contains(marker)).unwrap() as u64 + 1;
        let (typed_line, dead_line) = (line_of("/* typed-stop */"), line_of("/* dead-stop */"));
        std::fs::write(&c, source).unwrap();
        let plain = dir.join("plain.wasm");
        let mut command = Command::new(&clang);
        command
            .args([
                "--no-default-config",
                "--target=wasm32-unknown-unknown",
                "-nostdlib",
                "-O2",
                "-g",
                "-fdebug-compilation-dir=.",
                "-fno-builtin",
                "-Wl,--no-entry",
                "-Wl,--export=root",
                "-Wl,--export=worker",
                "-Wl,--export=leaf",
                "-Wl,--export=branchy",
                "-Wl,--export=typed",
                "-Wl,--export=dead_across",
            ])
            .current_dir(&dir)
            .arg("fixture.c")
            .arg("-o")
            .arg(&plain);
        if fast {
            command.args(["-mllvm", "-fast-isel=true"]);
        }
        if repro {
            command.arg("-DLLVM_INPUT_REPRO");
        }
        run(&mut command);
        let original = std::fs::read(&plain).unwrap();
        let sanitized = sanitize_input(&original);
        assert_eq!(executable_sections(&original), executable_sections(&sanitized), "input metadata validation must not change code");
        if repro {
            // LLVM already loses aggregate pieces / the unused high bits of
            // `big` before Binaryen. Do not let the consumer invent zeroes.
            assert!(source_point(&sanitized, "sample", typed_line).is_none());
            assert!(source_point(&sanitized, "big", typed_line).is_none());
        }
        std::fs::write(&plain, sanitized).unwrap();
        run(Command::new(&verify).arg("--verify").arg(&plain));
        let mut artifacts = vec![("plain", plain.clone(), false)];
        for pass in ["roundtrip", "flatten", "asyncify"] {
            for (kind, tool) in [("baseline", &baseline), ("candidate", &candidate)] {
                let path = dir.join(format!("{kind}-{pass}.wasm"));
                let mut command = Command::new(tool);
                command.arg(&plain).arg("-g");
                match pass {
                    "flatten" => {
                        command.arg("--flatten");
                    }
                    "asyncify" => {
                        command.args([
                            "--asyncify",
                            "--pass-arg=asyncify-imports@psx.bios",
                            "--pass-arg=asyncify-onlylist@worker,root,branchy,typed,dead_across",
                        ]);
                    }
                    _ => {}
                }
                run(command.arg("-o").arg(&path));
                run(Command::new(&verify).arg("--verify").arg(&path));
                artifacts.push((if kind == "baseline" { "baseline" } else { "candidate" }, path, pass == "asyncify"));
            }
            let bytes = |kind| std::fs::read(dir.join(format!("{kind}-{pass}.wasm"))).unwrap();
            assert_eq!(
                executable_sections(&bytes("baseline")),
                executable_sections(&bytes("candidate")),
                "{name} {pass}: metadata repair changed executable sections"
            );
            // An input statement row starts at most one output statement;
            // more would stop a line breakpoint once per opcode.
            let input = statements(&std::fs::read(&plain).unwrap());
            for (line, n) in statements(&bytes("candidate")) {
                assert!(n <= input.get(&line).copied().unwrap_or(0), "{name} {pass}: line {line} has {n} statements");
            }
        }
        let repaired = std::fs::read(dir.join("candidate-asyncify.wasm")).unwrap();
        let point = source_point(&repaired, "resumed", 17).expect("resumed at fixture.c:17");
        assert_eq!(point["kind"], "local");
        if wide {
            assert!(point["index"].as_u64().unwrap() >= 128);
        }
        // `left` crosses another suspension. The output CFG cannot establish
        // the saved spill's identity, so it must not retain a guessed value.
        assert!(source_point(&repaired, "left", 25).is_none());
        // Asyncify saves only the locals live at a suspension, and `dead`
        // was last used before it. The unpatched tool still describes it
        // after the call (the re-entered frame's zero after a resume).
        let unpatched = std::fs::read(dir.join("baseline-asyncify.wasm")).unwrap();
        assert!(source_point(&unpatched, "dead", dead_line).is_some(), "{name}: the fixture must keep `dead` in a local");
        assert!(source_point(&repaired, "dead", dead_line).is_none());
        let joined = dir.join("merged.wasm");
        std::fs::write(&joined, merged(&repaired)).unwrap();
        run(Command::new(&verify).arg("--verify").arg(&joined));
        artifacts.push(("candidate", joined.clone(), true));
        let mut branch_point = source_point(&std::fs::read(&joined).unwrap(), "result", 41).expect("result at fixture.c:41");
        branch_point["entry"] = json!("branchy");
        let branch: Value = serde_json::from_slice(&run(Command::new("node")
            .arg(ws.root.join("toolchain/binaryen-dwarf/tests/inspect.cjs"))
            .arg(&joined)
            .arg(branch_point.to_string())))
        .unwrap();
        assert_eq!(branch["cycles"], 3);
        assert_eq!(branch["values"], json!([729]));
        for (kind, path, asyncified) in artifacts {
            let point = source_point(&std::fs::read(&path).unwrap(), "resumed", 17).expect("resumed at fixture.c:17");
            let output =
                run(Command::new("node").arg(ws.root.join("toolchain/binaryen-dwarf/tests/inspect.cjs")).arg(&path).arg(point.to_string()));
            let result: Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(result["cycles"], if asyncified { 2 } else { 0 });
            let broken_control = kind == "baseline" && path.file_name().unwrap() != "baseline-roundtrip.wasm";
            assert_eq!(result["values"], if broken_control { json!([null, null]) } else { json!([198, 211]) }, "{}", path.display());
        }
    }
}
