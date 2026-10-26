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
    if !real.is_empty() {
        let ast = ast::parse(&text);
        let fns = ast.functions();
        let mut done = BTreeSet::new();
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
