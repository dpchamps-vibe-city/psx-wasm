//! Layout probe: for every struct/union and typed global a header defines,
//! compute `sizeof`, alignment and field offsets with both the matching
//! reference compiler (GCC 2.95 cc1 for MIPS) and clang for wasm32, and
//! compare. Equal layouts are what lets psx-wasm keep the PSX memory map.
//!
//! Each probe TU includes the project prelude headers, then one header; only
//! entities whose presumed location is that header are probed. Entries that
//! either compiler rejects are dropped and recorded, then the probe reruns.

use crate::ast;
use crate::clang::Clang;
use crate::diag::{self, Severity};
use crate::linemap::LineMap;
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

const SENTINEL: u32 = 0xC0DE_0000;
const MAX_ROUNDS: usize = 5;

/// The matching compiler: `cpp` then `cc1`, run from the project root.
#[derive(Debug, Clone)]
pub struct Reference {
    pub cpp: PathBuf,
    pub cppflags: Vec<String>,
    pub cc1: PathBuf,
    pub cc1flags: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    /// The probed C expression.
    pub expr: String,
    pub ty: String,
    pub field: Option<String>,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntryResult {
    #[serde(flatten)]
    pub entry: Entry,
    pub reference: Option<u32>,
    pub clang: Option<u32>,
    pub reference_error: Option<String>,
    pub clang_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub enum Outcome {
    Probed { entries: Vec<EntryResult>, bitfields: u32 },
    /// The reference compiler can't compile the header in this context.
    Skipped(String),
    /// The reference compiles it but clang can't (a portability finding).
    ClangFailed(String),
}

/// Probe one header. `rel` is its path under the include dir `dir`
/// (e.g. `include` + `psyq/libgpu.h`).
pub fn probe_header(
    clang: &Clang,
    reference: &Reference,
    prelude: &[String],
    dir: &str,
    rel: &str,
    work: &Path,
) -> Result<Outcome> {
    std::fs::create_dir_all(work)?;
    let stem = format!("probe_{}", rel.replace(['/', '.'], "_"));
    let src = work.join(format!("{stem}.c"));
    let mut head = String::new();
    for p in prelude {
        head.push_str(&format!("#include \"{p}\"\n"));
    }
    head.push_str(&format!("#include \"{rel}\"\n"));

    // Enumerate what the header defines, from clang's view of it.
    std::fs::write(&src, &head)?;
    let text = match clang.preprocess(&src, &["PSXW_CLANG"]) {
        Ok(t) => t,
        Err(e) => return Ok(Outcome::ClangFailed(first_line(&e.to_string()))),
    };
    let map = LineMap::new(&text);
    let tree = ast::parse(&text);
    let header_path = format!("{dir}/{rel}");
    let in_header = |offset: usize| map.presumed(map.line_of(offset)).is_some_and(|(f, _)| f == header_path);
    let mut entries: Vec<Entry> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut bitfields = 0;
    for r in tree.records().into_iter().filter(|r| in_header(r.start)) {
        if !seen.insert(r.ty.clone()) {
            continue;
        }
        bitfields += r.bitfields;
        let ty = &r.ty;
        entries.push(entry(format!("sizeof({ty})"), ty, None));
        entries.push(entry(format!("__alignof__({ty})"), ty, None));
        for f in &r.fields {
            entries.push(entry(format!("PSXW_OFF({ty}, {f})"), ty, Some(f)));
        }
    }
    for o in tree.objects().into_iter().filter(|o| in_header(o.start)) {
        if seen.insert(format!("object {}", o.name)) {
            let label = o.asm_label.clone().unwrap_or_else(|| o.name.clone());
            entries.push(entry(format!("sizeof({})", o.name), &label, None));
        }
    }
    if entries.is_empty() {
        return Ok(Outcome::Probed { entries: Vec::new(), bitfields });
    }

    let mut results: Vec<EntryResult> = entries
        .into_iter()
        .map(|entry| EntryResult { entry, reference: None, clang: None, reference_error: None, clang_error: None })
        .collect();
    for _ in 0..MAX_ROUNDS {
        let live: Vec<usize> = (0..results.len()).filter(|&i| failed(&results[i]).is_none()).collect();
        if live.is_empty() {
            break;
        }
        let mut probe = head.clone();
        probe.push_str("#ifdef PSXW_GCC\n#define PSXW_OFF(T, f) ((unsigned int)&((T *)0)->f)\n#else\n#define PSXW_OFF(T, f) __builtin_offsetof(T, f)\n#endif\n");
        let mut line_of = BTreeMap::new();
        let mut line = probe.lines().count() as u32;
        for &i in &live {
            line += 1;
            line_of.insert(line, i);
            results[i].entry.line = line;
            probe.push_str(&format!(
                "unsigned int psxw_probe_{i}[2] = {{ 0x{SENTINEL:08X}u, {} }};\n",
                results[i].entry.expr
            ));
        }
        std::fs::write(&src, &probe)?;
        let file_name = format!("{stem}.c");

        // Reference compiler.
        let gcc = run_reference(reference, clang, &src, work, &stem)?;
        let mut progress = false;
        match gcc {
            RefRun::Values(values) => {
                for &i in &live {
                    results[i].reference = values.get(&i).copied();
                }
            }
            RefRun::EntryErrors(errs) => {
                for (line, msg) in errs {
                    if let Some(&i) = line_of.get(&line) {
                        results[i].reference_error = Some(msg);
                        progress = true;
                    }
                }
            }
            RefRun::Failed(msg) => return Ok(Outcome::Skipped(msg)),
        }

        // clang.
        let c = crate::compile::compile(clang, &src, work, &stem, &["PSXW_CLANG"], Some(&file_name))?;
        if c.ok {
            let obj = c.object.as_ref().context("clang object")?;
            let values = read_clang_values(obj)?;
            for &i in &live {
                results[i].clang = values.get(&i).copied();
            }
        } else if !c.errors.is_empty() {
            let e = &c.errors[0];
            return Ok(Outcome::ClangFailed(format!("{}:{}: {}", e.file, e.line, e.message)));
        } else {
            for e in &c.deferred {
                if let Some(&i) = line_of.get(&e.line) {
                    results[i].clang_error = Some(e.message.clone());
                    progress = true;
                }
            }
        }
        if !progress {
            break;
        }
        // Values from a round with failures are incomplete; recompute.
        for r in results.iter_mut() {
            r.reference = None;
            r.clang = None;
        }
    }
    Ok(Outcome::Probed { entries: results, bitfields })
}

fn entry(expr: String, ty: &str, field: Option<&String>) -> Entry {
    Entry { expr, ty: ty.to_string(), field: field.cloned(), line: 0 }
}

fn failed(r: &EntryResult) -> Option<&str> {
    r.reference_error.as_deref().or(r.clang_error.as_deref())
}

fn first_line(s: &str) -> String {
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().to_string()
}

enum RefRun {
    Values(BTreeMap<usize, u32>),
    EntryErrors(Vec<(u32, String)>),
    Failed(String),
}

fn run_reference(r: &Reference, clang: &Clang, src: &Path, work: &Path, stem: &str) -> Result<RefRun> {
    let i_path = work.join(format!("{stem}.gcc.i"));
    let s_path = work.join(format!("{stem}.s"));
    let cpp = Command::new(&r.cpp)
        .current_dir(&clang.cwd)
        .args(&r.cppflags)
        .arg("-DPSXW_GCC")
        .arg(src)
        .arg("-o")
        .arg(&i_path)
        .output()
        .with_context(|| format!("running {}", r.cpp.display()))?;
    if !cpp.status.success() {
        return Ok(RefRun::Failed(format!("cpp: {}", first_line(&String::from_utf8_lossy(&cpp.stderr)))));
    }
    let cc1 = Command::new(&r.cc1)
        .current_dir(&clang.cwd)
        .args(&r.cc1flags)
        .arg(&i_path)
        .arg("-o")
        .arg(&s_path)
        .output()
        .with_context(|| format!("running {}", r.cc1.display()))?;
    let stderr = String::from_utf8_lossy(&cc1.stderr);
    let probe_name = src.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let errors: Vec<_> = diag::parse_gcc(&stderr).into_iter().filter(|d| d.severity == Severity::Error).collect();
    let (ours, theirs): (Vec<_>, Vec<_>) = errors.into_iter().partition(|d| d.file.ends_with(&probe_name));
    if let Some(d) = theirs.first() {
        return Ok(RefRun::Failed(format!("cc1: {}:{}: {}", d.file, d.line, d.message)));
    }
    if !ours.is_empty() {
        return Ok(RefRun::EntryErrors(ours.into_iter().map(|d| (d.line, d.message)).collect()));
    }
    if !cc1.status.success() {
        return Ok(RefRun::Failed(format!("cc1 failed: {}", first_line(&stderr))));
    }
    Ok(RefRun::Values(parse_gcc_words(&std::fs::read_to_string(&s_path)?)))
}

/// `psxw_probe_N:` followed by `.word SENTINEL` and `.word VALUE`.
fn parse_gcc_words(asm: &str) -> BTreeMap<usize, u32> {
    let mut out = BTreeMap::new();
    let mut current: Option<usize> = None;
    let mut words: Vec<u32> = Vec::new();
    for line in asm.lines() {
        let t = line.trim();
        if let Some(label) = t.strip_suffix(':') {
            current = label.strip_prefix("psxw_probe_").and_then(|n| n.parse().ok());
            words.clear();
            continue;
        }
        let Some(i) = current else { continue };
        if let Some(v) = t.strip_prefix(".word") {
            let v = v.trim();
            let n = match v.strip_prefix("0x") {
                Some(h) => i64::from_str_radix(h, 16).ok(),
                None => v.parse::<i64>().ok(),
            };
            if let Some(n) = n {
                words.push(n as u32);
            }
            if words.len() == 2 && words[0] == SENTINEL {
                out.insert(i, words[1]);
                current = None;
            }
        }
    }
    out
}

fn read_clang_values(obj: &Path) -> Result<BTreeMap<usize, u32>> {
    let bytes = std::fs::read(obj)?;
    let mut out = BTreeMap::new();
    let parsed = wasm_data_symbols(&bytes)?;
    for (name, data) in parsed {
        if let Some(i) = name.strip_prefix("psxw_probe_").and_then(|n| n.parse::<usize>().ok()) {
            if data.len() == 8 && u32::from_le_bytes(data[..4].try_into()?) == SENTINEL {
                out.insert(i, u32::from_le_bytes(data[4..8].try_into()?));
            }
        }
    }
    Ok(out)
}

/// Defined data symbols and their bytes. Minimal on purpose: psxw-cc does
/// not depend on the linker crate's object reader.
fn wasm_data_symbols(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    use wasmparser::{KnownCustom, Linking, Parser, Payload, SymbolInfo};
    let mut segments: Vec<Vec<u8>> = Vec::new();
    let mut symbols = Vec::new();
    for payload in Parser::new(0).parse_all(bytes) {
        match payload? {
            Payload::DataSection(reader) => {
                for d in reader {
                    segments.push(d?.data.to_vec());
                }
            }
            Payload::CustomSection(reader) => {
                if let KnownCustom::Linking(linking) = reader.as_known() {
                    for sub in linking.subsections() {
                        if let Linking::SymbolTable(map) = sub? {
                            for info in map {
                                if let SymbolInfo::Data { name, symbol: Some(d), .. } = info? {
                                    symbols.push((name.to_string(), d));
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(symbols
        .into_iter()
        .filter_map(|(n, d)| {
            let seg = segments.get(d.index as usize)?;
            Some((n, seg.get(d.offset as usize..(d.offset + d.size) as usize)?.to_vec()))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End to end against the real compilers: a struct both lay out alike,
    /// and `long double` (8 bytes on MIPS o32, 16 on wasm32) which must be
    /// reported as a mismatch. Needs PSXW_TEST_CC1 (reference cc1) and
    /// PSXW_TEST_CLANG; skipped otherwise.
    #[test]
    fn detects_layout_mismatch() {
        let (Ok(cc1), Ok(clang_path)) = (std::env::var("PSXW_TEST_CC1"), std::env::var("PSXW_TEST_CLANG")) else {
            eprintln!("skipped: set PSXW_TEST_CC1 and PSXW_TEST_CLANG");
            return;
        };
        let root = std::env::temp_dir().join(format!("psxw-probe-test-{}", std::process::id()));
        std::fs::create_dir_all(root.join("include")).unwrap();
        std::fs::write(
            root.join("include/t.h"),
            "typedef struct { char c; short s; int i; char tail[3]; } Same;\nstruct Wide { char c; long double d; };\n",
        )
        .unwrap();
        let clang =
            Clang::new(clang_path.into(), root.clone(), vec!["include".into()], vec![], &root.join("work")).unwrap();
        let reference = Reference {
            cpp: "mips-linux-gnu-cpp".into(),
            cppflags: vec!["-Iinclude".into(), "-undef".into(), "-lang-c".into()],
            cc1: cc1.into(),
            cc1flags: vec!["-O2".into(), "-mips1".into(), "-msoft-float".into(), "-quiet".into()],
        };
        let out = probe_header(&clang, &reference, &[], "include", "t.h", &root.join("work")).unwrap();
        let Outcome::Probed { entries, .. } = out else { panic!("not probed: {out:?}") };
        let get = |expr: &str| entries.iter().find(|e| e.entry.expr == expr).unwrap();
        assert_eq!((get("sizeof(Same)").reference, get("sizeof(Same)").clang), (Some(12), Some(12)));
        assert_eq!(get("PSXW_OFF(Same, tail)").clang, Some(8));
        let wide = get("sizeof(struct Wide)");
        assert_eq!(wide.reference, Some(16));
        assert_eq!(wide.clang, Some(32));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn gcc_words() {
        let asm = "\t.sdata\n\t.align\t2\npsxw_probe_3:\n\t.word\t-1059192832\n\t.word\t12\npsxw_probe_4:\n\t.word\t0xc0de0000\n\t.word\t4\n";
        let v = super::parse_gcc_words(asm);
        assert_eq!(v.get(&3), Some(&12));
        assert_eq!(v.get(&4), Some(&4));
    }
}
