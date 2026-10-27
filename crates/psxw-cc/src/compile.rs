//! One translation unit through clang: preprocess, asm normalization, then an
//! error-driven loop. File-scope errors with a known GCC 2.95 cause get a
//! named fixup; errors inside a function hand that function to the lift
//! tier. Anything else fails the TU.

use crate::ast::{self, Ast};
use crate::clang::Clang;
use crate::diag::{Diag, Severity};
use crate::edit::{self, Edit};
use crate::linemap::LineMap;
use crate::{LIFT_MARKER_PREFIX, asm};
use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const MAX_ITERATIONS: u32 = 8;
const EXAMPLES_PER_FLAG: usize = 3;

#[derive(Debug, Clone, Serialize)]
pub struct Lifted {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Located {
    pub file: String,
    pub line: u32,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Compiled {
    pub ok: bool,
    pub object: Option<PathBuf>,
    /// Normalization rule → times applied.
    pub rules: BTreeMap<String, u32>,
    /// Functions handed to the lift tier by normalization (INCLUDE_ASM
    /// markers come from the prelude and are read from the object).
    pub lifted: Vec<Lifted>,
    pub warnings: BTreeMap<String, u32>,
    pub warning_examples: BTreeMap<String, Vec<Located>>,
    pub errors: Vec<Located>,
    /// Errors located in the caller's `deferred_file`, left unhandled.
    pub deferred: Vec<Located>,
    pub iterations: u32,
}

pub fn compile(
    clang: &Clang,
    src: &Path,
    work: &Path,
    stem: &str,
    extra_defines: &[&str],
    deferred_file: Option<&str>,
) -> Result<Compiled> {
    let mut out = Compiled::default();
    let mut text = clang.preprocess(src, extra_defines)?;

    // Asm normalization.
    let sites = asm::scan(&text);
    let mut edits = asm::edits(&text, &sites);
    for s in &sites {
        if let Some(rule) = asm::rule(s) {
            *out.rules.entry(rule.to_string()).or_default() += 1;
        }
    }
    let real: Vec<usize> = sites.iter().filter(|s| s.kind == asm::AsmKind::Real).map(|s| s.start).collect();
    // A GNU global register variable keeps its value in a MIPS register
    // across calls (a hidden argument or return channel). C on wasm can't,
    // so every function that uses one is lifted.
    let globals: Vec<String> = sites
        .iter()
        .filter(|s| s.kind == asm::AsmKind::RegisterBinding && s.depth <= 0)
        .filter_map(|s| {
            let stmt = text[..s.start].rfind([';', '}']).map_or(0, |i| i + 1);
            text[stmt..s.start].split(|c: char| !c.is_alphanumeric() && c != '_').rfind(|w| !w.is_empty()).map(str::to_string)
        })
        .collect();
    // A local register variable that is read but never assigned observes
    // whatever the register holds (e.g. the previous call's v0); that too
    // only the original instructions can reproduce.
    let locals: Vec<(usize, String)> = sites
        .iter()
        .filter(|s| s.kind == asm::AsmKind::RegisterBinding && s.depth > 0)
        .filter_map(|s| {
            let stmt = text[..s.start].rfind([';', '{', '}']).map_or(0, |i| i + 1);
            let after = &text[s.end..];
            if after.trim_start().starts_with('=') {
                return None;
            }
            text[stmt..s.start]
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .rfind(|w| !w.is_empty())
                .map(|n| (s.start, n.to_string()))
        })
        .collect();
    if !real.is_empty() || !globals.is_empty() || !locals.is_empty() {
        let ast = ast::parse(&text);
        let fns = ast.functions();
        let mut done = BTreeSet::new();
        for f in &fns {
            let body = &text[f.range.clone()];
            if globals.iter().any(|g| asm::find_word(body, g).is_some()) && done.insert(f.range.start) {
                edits.push(lift(&text, &ast, f, "global-register", &mut out));
            }
        }
        for (at, name) in &locals {
            let Some(f) = fns.iter().find(|f| f.range.contains(at)) else { continue };
            if !assigns(&text[f.range.clone()], name) && done.insert(f.range.start) {
                edits.push(lift(&text, &ast, f, "register-capture", &mut out));
            }
        }
        for off in real {
            match fns.iter().find(|f| f.range.contains(&off)) {
                Some(f) if done.insert(f.range.start) => edits.push(lift(&text, &ast, f, "inline-asm", &mut out)),
                Some(_) => {}
                None => out.errors.push(Located {
                    file: src.display().to_string(),
                    line: 0,
                    message: "MIPS asm outside any function".into(),
                }),
            }
        }
    }
    text = edit::apply(&text, edits);
    if !out.errors.is_empty() {
        return Ok(out);
    }

    let i_path = work.join(format!("{stem}.i"));
    let obj = work.join(format!("{stem}.o"));
    for iteration in 1..=MAX_ITERATIONS {
        out.iterations = iteration;
        let (ok, diags) = clang.compile_i(&text, &i_path, &obj)?;
        let map = LineMap::new(&text);
        let errors: Vec<&Diag> = diags.iter().filter(|d| d.severity == Severity::Error).collect();
        // A non-void function that can fall off its end returns whatever
        // the original left in v0; C has no such value, so it's lifted.
        let falls_off: Vec<&Diag> = diags
            .iter()
            .filter(|d| d.severity == Severity::Warning && d.message.contains("does not return a value"))
            .collect();
        if ok && errors.is_empty() && !falls_off.is_empty() {
            let ast = ast::parse(&text);
            let fns = ast.functions();
            let mut edits = Vec::new();
            let mut done = BTreeSet::new();
            for d in falls_off {
                let Some(offset) = map.offset(d.line) else { continue };
                if let Some(f) = fns.iter().find(|f| f.range.contains(&offset)) {
                    if done.insert(f.range.start) {
                        edits.push(lift(&text, &ast, f, "return-type", &mut out));
                    }
                }
            }
            if !edits.is_empty() {
                text = edit::apply(&text, edits);
                continue;
            }
        }
        if ok && errors.is_empty() && clang.soft_float {
            let floats = float_functions(&obj)?;
            if !floats.is_empty() {
                let ast = ast::parse(&text);
                let edits: Vec<Edit> = ast
                    .functions()
                    .iter()
                    .filter(|f| floats.contains(&f.name))
                    .map(|f| lift(&text, &ast, f, "soft-float", &mut out))
                    .collect();
                if !edits.is_empty() {
                    text = edit::apply(&text, edits);
                    continue;
                }
            }
        }
        if ok && errors.is_empty() {
            record_warnings(&mut out, &diags, &map);
            out.ok = true;
            out.object = Some(obj);
            return Ok(out);
        }

        let ast = ast::parse(&text);
        let fns = ast.functions();
        let mut edits = Vec::new();
        let mut fatal = Vec::new();
        out.deferred.clear();
        let mut lifted_now = BTreeSet::new();
        let mut moved = BTreeSet::new();
        for d in errors {
            let loc = locate(&map, d);
            if deferred_file.is_some_and(|f| loc.file.ends_with(f)) {
                out.deferred.push(loc);
                continue;
            }
            let Some(offset) = map.offset(d.line) else {
                fatal.push(loc);
                continue;
            };
            if let Some(f) = fns.iter().find(|f| f.range.contains(&offset)) {
                if lifted_now.insert(f.range.start) {
                    edits.push(lift(&text, &ast, f, &lift_reason(&d.message), &mut out));
                }
            } else if let Some(e) = incomplete_array_fix(&ast, &text, &d.message, offset, &mut moved, &mut out.rules) {
                edits.extend(e);
            } else if !(d.message.starts_with("array has incomplete element type") && moved.contains(&offset)) {
                fatal.push(loc);
            }
        }
        if !fatal.is_empty() || edits.is_empty() {
            out.errors = fatal;
            return Ok(out);
        }
        text = edit::apply(&text, edits);
    }
    out.errors.push(Located {
        file: src.display().to_string(),
        line: 0,
        message: format!("no clean compile after {MAX_ITERATIONS} fixup rounds"),
    });
    Ok(out)
}

fn locate(map: &LineMap, d: &Diag) -> Located {
    let (file, line) = map.presumed(d.line).unwrap_or((d.file.as_str(), d.line));
    Located { file: file.to_string(), line, message: d.message.clone() }
}

fn record_warnings(out: &mut Compiled, diags: &[Diag], map: &LineMap) {
    for d in diags.iter().filter(|d| d.severity == Severity::Warning) {
        let flag = d.flag.clone().unwrap_or_else(|| "(no flag)".into());
        *out.warnings.entry(flag.clone()).or_default() += 1;
        let ex = out.warning_examples.entry(flag).or_default();
        if ex.len() < EXAMPLES_PER_FLAG {
            ex.push(locate(map, d));
        }
    }
}

/// Replace a function definition with its prototype plus lift markers for it
/// and for the GNU nested functions defined inside it, which have their own
/// original addresses (under their `auto ... __asm__` label, if bound).
fn lift(text: &str, ast: &Ast, f: &ast::FnDef, reason: &str, out: &mut Compiled) -> Edit {
    let proto = text[f.range.start..f.decl_end].trim_end();
    let reason: String = reason.chars().filter(|c| *c != '"' && *c != '\\' && *c != '\n').collect();
    let mut replacement = format!("{proto};");
    let mut seen = BTreeSet::new();
    let mut marker = |name: &str, why: &str| {
        let ident: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
        if seen.insert(ident.clone()) {
            replacement.push_str(&format!("\n__attribute__((used)) static const char {LIFT_MARKER_PREFIX}{ident}[] = \"{why}\";"));
            out.lifted.push(Lifted { name: name.to_string(), reason: why.to_string() });
        }
    };
    marker(&f.name, &reason);
    let labels: std::collections::HashMap<String, String> = asm::auto_labels(&text[f.range.clone()]).into_iter().collect();
    for inner in ast.nested_functions(f) {
        marker(labels.get(&inner).unwrap_or(&inner), "nested-function (inner)");
    }
    Edit::replace(f.range.clone(), replacement)
}

fn lift_reason(message: &str) -> String {
    if message.contains("function definition is not allowed here") || message.contains("illegal storage class on function") {
        "nested-function".into()
    } else if message.contains("register name") || message.contains("invalid operand") || message.contains("instruction") {
        "inline-asm".into()
    } else {
        format!("clang-error: {message}")
    }
}

/// File-scope fixups for an error clang reports at `offset` of `text`:
/// the GCC 2.95-isms of headers ([`incomplete_array_fix`]). Mods (M11)
/// include the decomp's headers and need the same.
pub fn header_fixups(
    ast: &Ast,
    text: &str,
    message: &str,
    offset: usize,
    moved: &mut BTreeSet<usize>,
    rules: &mut BTreeMap<String, u32>,
) -> Option<Vec<Edit>> {
    incomplete_array_fix(ast, text, message, offset, moved, rules)
}

/// GCC 2.95 accepts `extern struct X a[N];` before `struct X` is complete;
/// clang never does. Move the declaration after the definition, or drop it
/// when the TU never completes the type (then nothing can use it).
fn incomplete_array_fix(
    ast: &Ast,
    text: &str,
    message: &str,
    offset: usize,
    moved: &mut BTreeSet<usize>,
    rules: &mut BTreeMap<String, u32>,
) -> Option<Vec<Edit>> {
    let ty = message.strip_prefix("array has incomplete element type '")?.strip_suffix('\'')?;
    let (keyword, name) = ty.split_once(' ')?;
    if !matches!(keyword, "struct" | "union") {
        return None;
    }
    let decl = ast.top_level_at(offset).filter(|n| n.kind() == "declaration")?;
    if !moved.insert(decl.start_byte()) {
        return Some(Vec::new());
    }
    let range = decl.byte_range();
    let body = &text[range.clone()];
    Some(match ast.record_definition_end(keyword, name, range.end) {
        Some(at) => {
            *rules.entry("incomplete-array-moved".into()).or_default() += 1;
            vec![Edit::replace(range.clone(), ""), Edit::insert(at, format!("\n{body}\n"))]
        }
        None => {
            *rules.entry("incomplete-array-dropped".into()).or_default() += 1;
            vec![Edit::replace(range, "")]
        }
    })
}

/// Whether `body` assigns `name` (`=`, compound assignment, `++`/`--`) or
/// takes its address.
fn assigns(body: &str, name: &str) -> bool {
    let b = body.as_bytes();
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    body.match_indices(name).any(|(i, _)| {
        let end = i + name.len();
        if (i > 0 && ident(b[i - 1])) || b.get(end).is_some_and(|&c| ident(c)) {
            return false;
        }
        let before = body[..i].trim_end();
        let after = body[end..].trim_start();
        let assign = after.starts_with('=') && !after.starts_with("==");
        let compound = ["+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>=", "++", "--"]
            .iter()
            .any(|op| after.starts_with(op));
        assign || compound || before.ends_with("++") || before.ends_with("--") || before.ends_with('&') && !before.ends_with("&&")
    })
}

#[cfg(test)]
mod tests {
    use super::assigns;

    #[test]
    fn detects_assignment() {
        assert!(assigns("{ x = 1; }", "x"));
        assert!(assigns("{ x += 1; }", "x"));
        assert!(assigns("{ ++x; }", "x"));
        assert!(assigns("{ f(&x); }", "x"));
        assert!(!assigns("{ y = x; if (x == 2) {} }", "x"));
        assert!(!assigns("{ xx = 1; }", "x"));
    }
}

/// Names of the functions in a wasm object whose code does floating-point
/// arithmetic (not merely moves bits through f32/f64 loads and stores).
fn float_functions(obj: &Path) -> Result<BTreeSet<String>> {
    use wasmparser::{KnownCustom, Linking, Operator as O, Parser, Payload, SymbolInfo};
    let bytes = std::fs::read(obj)?;
    let mut imported = 0u32;
    let mut floaty: Vec<u32> = Vec::new();
    let mut index = 0u32;
    let mut names: BTreeMap<u32, String> = BTreeMap::new();
    for payload in Parser::new(0).parse_all(&bytes) {
        match payload? {
            Payload::ImportSection(r) => {
                for i in r.into_imports() {
                    if matches!(i?.ty, wasmparser::TypeRef::Func(_)) {
                        imported += 1;
                    }
                }
            }
            Payload::CodeSectionEntry(body) => {
                let mut ops = body.get_operators_reader()?;
                let mut uses = false;
                while !ops.eof() {
                    uses |= matches!(
                        ops.read()?,
                        O::F32Add | O::F32Sub | O::F32Mul | O::F32Div | O::F64Add | O::F64Sub | O::F64Mul | O::F64Div
                            | O::F32Sqrt | O::F64Sqrt | O::F32Lt | O::F32Gt | O::F32Le | O::F32Ge | O::F32Eq | O::F32Ne
                            | O::F64Lt | O::F64Gt | O::F64Le | O::F64Ge | O::F64Eq | O::F64Ne
                            | O::F32ConvertI32S | O::F32ConvertI32U | O::F64ConvertI32S | O::F64ConvertI32U
                            | O::F32ConvertI64S | O::F64ConvertI64S | O::F32DemoteF64 | O::F64PromoteF32
                            | O::I32TruncF32S | O::I32TruncF64S | O::I32TruncF32U | O::I32TruncF64U
                            | O::I32TruncSatF32S | O::I32TruncSatF64S | O::I32TruncSatF32U | O::I32TruncSatF64U
                            | O::F32Neg | O::F64Neg | O::F32Abs | O::F64Abs | O::F32Floor | O::F64Floor
                            | O::F32Ceil | O::F64Ceil | O::F32Trunc | O::F64Trunc | O::F32Nearest | O::F64Nearest
                    );
                }
                if uses {
                    floaty.push(imported + index);
                }
                index += 1;
            }
            Payload::CustomSection(r) => {
                if let KnownCustom::Linking(l) = r.as_known() {
                    for sub in l.subsections() {
                        if let Linking::SymbolTable(map) = sub? {
                            for info in map {
                                if let SymbolInfo::Func { index, name: Some(name), .. } = info? {
                                    names.insert(index, name.to_string());
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(floaty.iter().filter_map(|i| names.get(i).cloned()).collect())
}
