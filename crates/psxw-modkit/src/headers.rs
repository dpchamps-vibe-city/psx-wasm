//! The header index (plans/M11 task 2): for each function the decomp's
//! headers declare, the header that declares it and its return and
//! parameter types as written; for each global, its header. Headers are
//! the census's (`[probe] include_dirs` minus `[probe] exclude`), each
//! preprocessed in the context the decomp includes it in, and read with
//! tree-sitter; only declarations located in the header itself count.

use anyhow::Result;
use psxw_cc::ast::{self, Ast};
use psxw_cc::linemap::LineMap;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use tree_sitter::Node;

/// Bumped when the index reads headers differently (its cache key).
pub const INDEX_VERSION: u32 = 3;

/// A function's declaration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Decl {
    /// As `#include "…"` names it, relative to an include directory.
    pub header: String,
    /// The headers the decomp includes before it (its context).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<String>,
    /// The declaration as written, whitespace collapsed.
    pub text: String,
    /// The return type, as a type name (`u32`, `u8 *`, `void (*)(int)`).
    pub returns: String,
    pub params: Vec<DeclParam>,
    pub variadic: bool,
    /// `f()` declares no parameters at all (not even `void`).
    pub prototyped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeclParam {
    pub name: Option<String>,
    /// The type, as a type name (the declaration without its name).
    #[serde(rename = "type")]
    pub ty: String,
    /// As written, with its name.
    pub text: String,
}

impl Decl {
    pub fn returns_void(&self) -> bool {
        self.returns == "void"
    }
}

/// A global's declaring header.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DataDecl {
    pub header: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<String>,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HeaderIndex {
    pub functions: BTreeMap<String, Decl>,
    pub data: BTreeMap<String, DataDecl>,
    /// Names more than one header declares: the others (the one with the
    /// fewest errors in its context wins, then the first in path order).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub also: BTreeMap<String, Vec<String>>,
    /// Headers clang couldn't preprocess, with why.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub failed: BTreeMap<String, String>,
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").replace("( ", "(").replace(" )", ")").replace(" ,", ",")
}

/// Text of `node` with `cut` (a descendant) removed.
fn without(ast: &Ast, node: Node, cut: Node) -> String {
    let t = ast.text(node);
    let (s, a, b) = (node.start_byte(), cut.start_byte(), cut.end_byte());
    format!("{}{}", &t[..a - s], &t[b - s..])
}

/// The function declarator directly holding the name.
fn innermost_function(mut d: Node) -> Option<Node> {
    let mut found = None;
    loop {
        if d.kind() == "function_declarator" {
            found = Some(d);
        }
        match d.child_by_field_name("declarator") {
            Some(n) => d = n,
            None => {
                // parenthesized_declarator has no field; step into its child.
                let mut c = d.walk();
                let next = d.named_children(&mut c).find(|n| n.kind().ends_with("declarator"));
                match next {
                    Some(n) => d = n,
                    None => return found,
                }
            }
        }
    }
}

fn name_node(mut d: Node) -> Option<Node> {
    loop {
        if d.kind() == "identifier" {
            return Some(d);
        }
        d = match d.child_by_field_name("declarator") {
            Some(n) => n,
            None => {
                let mut c = d.walk();
                let next = d.named_children(&mut c).find(|n| n.kind() == "identifier" || n.kind().ends_with("declarator"));
                next?
            }
        };
    }
}

const STORAGE: &[&str] = &["extern", "static", "inline", "__inline", "__inline__", "register", "auto"];

/// A function declaration or definition node, read.
fn decl_of(ast: &Ast, node: Node, header: &str, context: &[String]) -> Option<(String, Decl)> {
    let d = node.child_by_field_name("declarator")?;
    let f = innermost_function(d)?;
    // A function's declarator holds its name directly; `(*table[4])(int)`
    // is data (an array of pointers to functions).
    let name_n = f.child_by_field_name("declarator").filter(|n| n.kind() == "identifier")?;
    let name = ast.text(name_n).to_string();
    let params_n = f.child_by_field_name("parameters")?;
    let mut params = Vec::new();
    let mut variadic = false;
    let mut c = params_n.walk();
    let children: Vec<Node> = params_n.named_children(&mut c).collect();
    for p in &children {
        match p.kind() {
            "variadic_parameter" => variadic = true,
            "parameter_declaration" => {
                let text = collapse(ast.text(*p));
                let (pname, ty) = match p.child_by_field_name("declarator").and_then(name_node) {
                    Some(n) => (Some(ast.text(n).to_string()), collapse(&without(ast, *p, n))),
                    None => (None, text.clone()),
                };
                params.push(DeclParam { name: pname, ty, text });
            }
            _ => {}
        }
    }
    if ast.text(params_n).contains("...") {
        variadic = true;
    }
    let prototyped = !children.is_empty() || variadic;
    if params.len() == 1 && params[0].name.is_none() && params[0].ty == "void" {
        params.clear();
    }
    // The return type: the declaration up to its declarator's end, without
    // the function declarator, storage classes and attributes.
    let head_end = d.end_byte();
    let start = node.start_byte();
    let full = &ast.text(node)[..head_end - start];
    let (a, b) = (f.start_byte() - start, f.end_byte() - start);
    let mut ret = format!("{}{}", &full[..a], &full[b..]);
    while let Some(i) = ret.find("__attribute__") {
        let rest = &ret[i..];
        let mut depth = 0;
        let mut end = rest.len();
        for (j, ch) in rest.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = j + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        ret = format!("{}{}", &ret[..i], &ret[i + end..]);
    }
    let words: Vec<&str> = ret.split_whitespace().collect();
    let kept: Vec<&str> = words.into_iter().filter(|w| !STORAGE.contains(w)).collect();
    let returns = collapse(&kept.join(" "));
    let text = collapse(full);
    Some((name, Decl { header: header.to_string(), context: context.to_vec(), text, returns, params, variadic, prototyped }))
}

/// The functions a preprocessed text declares (with prototypes, at file
/// scope), in order.
pub fn function_names(text: &str) -> Vec<String> {
    let tree = ast::parse(text);
    let root = tree.tree.root_node();
    let mut c = root.walk();
    let mut out = Vec::new();
    for node in root.named_children(&mut c) {
        if !matches!(node.kind(), "declaration" | "function_definition") {
            continue;
        }
        let mut c2 = node.walk();
        if node.named_children(&mut c2).any(|n| n.kind() == "storage_class_specifier" && matches!(tree.text(n), "typedef" | "static")) {
            continue;
        }
        if let Some((name, d)) = decl_of(&tree, node, "", &[]) {
            if d.prototyped && !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// Each function a preprocessed text declares, as a typedef of a pointer
/// to its type, spelled from its own declaration (the name becomes
/// `(*<prefix><name>)`): (name, typedef).
pub fn function_typedefs(text: &str, prefix: &str) -> Vec<(String, String)> {
    let tree = ast::parse(text);
    let root = tree.tree.root_node();
    let mut c = root.walk();
    let mut out: Vec<(String, String)> = Vec::new();
    for node in root.named_children(&mut c) {
        if node.kind() != "declaration" {
            continue;
        }
        let mut c2 = node.walk();
        if node.named_children(&mut c2).any(|n| n.kind() == "storage_class_specifier" && matches!(tree.text(n), "typedef" | "static")) {
            continue;
        }
        let Some(d) = node.child_by_field_name("declarator") else { continue };
        let Some(f) = innermost_function(d) else { continue };
        let Some(id) = f.child_by_field_name("declarator").filter(|n| n.kind() == "identifier") else { continue };
        let name = tree.text(id).to_string();
        if out.iter().any(|(n, _)| *n == name) || f.child_by_field_name("parameters").is_none_or(|p| tree.text(p) == "()") {
            continue;
        }
        let start = node.start_byte();
        let full = tree.text(node);
        let (a, b) = (id.start_byte() - start, id.end_byte() - start);
        let spelled = format!("{}(*{prefix}{name}){}", &full[..a], &full[b..]);
        let words: Vec<&str> = spelled.split_whitespace().collect();
        let kept: Vec<&str> = words.into_iter().filter(|w| !STORAGE.contains(w)).collect();
        out.push((name, format!("typedef {}", kept.join(" "))));
    }
    out
}

fn ast_has_function_declarator(node: Node) -> bool {
    node.child_by_field_name("declarator").is_some_and(|d| innermost_function(d).is_some())
}

/// Read one preprocessed header: (functions, globals).
fn read(text: &str, header_path: &str, rel: &str, context: &[String]) -> (Vec<(String, Decl)>, Vec<(String, DataDecl)>) {
    let map = LineMap::new(text);
    let tree = ast::parse(text);
    let in_header = |offset: usize| map.presumed(map.line_of(offset)).is_some_and(|(f, _)| f == header_path);
    let mut funcs = Vec::new();
    let mut fptrs = Vec::new();
    let root = tree.tree.root_node();
    let mut c = root.walk();
    for node in root.named_children(&mut c) {
        if !in_header(node.start_byte()) || !matches!(node.kind(), "declaration" | "function_definition") {
            continue;
        }
        let mut c2 = node.walk();
        if node.named_children(&mut c2).any(|n| n.kind() == "storage_class_specifier" && tree.text(n) == "typedef") {
            continue;
        }
        match decl_of(&tree, node, rel, context) {
            Some(d) => funcs.push(d),
            // Pointers to functions, and tables of them, are data.
            None if node.kind() == "declaration" && ast_has_function_declarator(node) => {
                if let Some(name) = node.child_by_field_name("declarator").and_then(|d| tree.declarator_name(d)) {
                    fptrs.push((name, DataDecl { header: rel.to_string(), context: context.to_vec(), text: collapse(tree.text(node)) }));
                }
            }
            None => {}
        }
    }
    let mut data: Vec<(String, DataDecl)> = tree
        .objects()
        .into_iter()
        .filter(|o| in_header(o.start))
        .map(|o| {
            let text = tree.top_level_at(o.start).map(|n| collapse(tree.text(n))).unwrap_or_default();
            (o.name, DataDecl { header: rel.to_string(), context: context.to_vec(), text })
        })
        .collect();
    data.extend(fptrs);
    (funcs, data)
}

/// Build the index over a game's headers in snapshot `snap`.
pub fn index(
    clang: &psxw_cc::Clang,
    snap: &Path,
    manifest: &psxw_inputs::GameManifest,
    map: &psxw_inputs::AddressMap,
    work: &Path,
) -> Result<HeaderIndex> {
    let mut headers: Vec<(String, String)> = Vec::new();
    for dir in &manifest.probe.include_dirs {
        let root = snap.join(dir);
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d)? {
                let p = e?.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "h") {
                    let rel = p.strip_prefix(&root)?.to_string_lossy().into_owned();
                    let name = p.file_name().unwrap_or_default().to_string_lossy();
                    if !manifest.probe.exclude.iter().any(|x| *x == rel || *x == name) {
                        headers.push((dir.to_string_lossy().into_owned(), rel));
                    }
                }
            }
        }
    }
    headers.sort();
    let contexts = psxw_census::include_contexts(snap, map);
    std::fs::create_dir_all(work)?;
    type Read = (Vec<(String, Decl)>, Vec<(String, DataDecl)>, usize);
    let results: Vec<(String, Result<Read>)> = headers
        .par_iter()
        .map(|(dir, rel)| {
            let context = contexts.get(rel).cloned().unwrap_or_else(|| manifest.probe.prelude.clone());
            let r = (|| {
                let src = work.join(format!("index_{}.c", rel.replace(['/', '.'], "_")));
                let mut head = String::new();
                for p in &context {
                    head.push_str(&format!("#include \"{p}\"\n"));
                }
                head.push_str(&format!("#include \"{rel}\"\n"));
                std::fs::write(&src, &head)?;
                let text = clang.preprocess(&src, &["PSXW_CLANG"])?;
                // A name several headers declare comes from the one that
                // compiles best (not an m2c catch-all of unknown types).
                let i = src.with_extension("i");
                std::fs::write(&i, &text)?;
                let errors = clang.mod_errors(&i)?;
                let (funcs, data) = read(&text, &format!("{dir}/{rel}"), rel, &context);
                Ok((funcs, data, errors))
            })();
            (rel.clone(), r)
        })
        .collect();
    let mut idx = HeaderIndex::default();
    let mut errors: BTreeMap<String, usize> = BTreeMap::new();
    for (rel, r) in results {
        match r {
            Ok((funcs, data, n)) => {
                errors.insert(rel.clone(), n);
                for (name, d) in funcs {
                    match idx.functions.get(&name) {
                        Some(first) if first.header != d.header => {
                            let header = if n < errors[&first.header] { idx.functions.insert(name.clone(), d).map(|o| o.header) } else { Some(d.header) };
                            idx.also.entry(name).or_default().extend(header);
                        }
                        Some(_) => {}
                        None => {
                            idx.functions.insert(name, d);
                        }
                    }
                }
                for (name, d) in data {
                    match idx.data.get(&name) {
                        Some(first) if n < errors[&first.header] => {
                            idx.data.insert(name, d);
                        }
                        Some(_) => {}
                        None => {
                            idx.data.insert(name, d);
                        }
                    }
                }
            }
            Err(e) => {
                let msg: String = e.to_string().lines().find(|l| l.contains("error")).unwrap_or("preprocessing failed").to_string();
                idx.failed.insert(rel, msg);
            }
        }
    }
    Ok(idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decls(src: &str) -> Vec<(String, Decl)> {
        read(&format!("# 1 \"inc/h.h\"\n{src}"), "inc/h.h", "h.h", &[]).0
    }

    #[test]
    fn spelled_types() {
        let d = decls("extern u32 Rand(s32 limit);\nextern int (*gTable[4])(int);\nu8 *GetPtr(void);\nvoid (*GetCb(int a))(int);\nint printf(const char *fmt, ...);\nvoid Old();\nvoid Cb(void (*fn)(int), int arr[4]);\n");
        let by: BTreeMap<_, _> = d.into_iter().collect();
        let r = &by["Rand"];
        assert_eq!((r.returns.as_str(), r.params[0].ty.as_str(), r.params[0].name.as_deref()), ("u32", "s32", Some("limit")));
        assert_eq!(by["GetPtr"].returns, "u8 *");
        assert!(by["GetPtr"].params.is_empty() && by["GetPtr"].prototyped);
        assert_eq!(by["GetCb"].returns, "void (*)(int)");
        assert!(by["printf"].variadic);
        assert!(!by["Old"].prototyped);
        assert_eq!(by["Cb"].params[0].ty, "void (*)(int)");
        assert_eq!(by["Cb"].params[1].ty, "int [4]");
        assert!(!by.contains_key("gTable"));
    }

    #[test]
    fn function_typedefs() {
        let t = super::function_typedefs("extern u32 Rand(s32 limit);\nvoid (*GetCb(int a))(int);\nvoid Old();\nint printf(const char *f, ...);\n", "P_");
        let by: BTreeMap<_, _> = t.into_iter().collect();
        assert_eq!(by["Rand"], "typedef u32 (*P_Rand)(s32 limit);");
        assert_eq!(by["GetCb"], "typedef void (*(*P_GetCb)(int a))(int);");
        assert_eq!(by["printf"], "typedef int (*P_printf)(const char *f, ...);");
        assert!(!by.contains_key("Old"));
    }

    #[test]
    fn function_pointer_tables_are_data() {
        let (f, d) = read("# 1 \"inc/h.h\"\nextern int (*gTable[21])(short *);\nextern void (*gHook)(void);\n", "inc/h.h", "h.h", &[]);
        assert!(f.is_empty());
        let names: Vec<&str> = d.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["gTable", "gHook"]);
    }
}
