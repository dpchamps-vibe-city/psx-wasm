//! The C-header frontend (D55, plans/M12 phase 2): the decomp's headers as
//! a snapshot. Every header the census probes (`[probe] include_dirs` minus
//! `[probe] exclude`) is preprocessed in the context the decomp includes it
//! in, and read with tree-sitter:
//!
//! - **Declarations** become type expressions: functions' signatures,
//!   globals' types (an `__asm__` label names the global), typedefs, enums
//!   and records with their fields. A global declared as an array behind a
//!   scalar `#define` keeps its declared array type.
//! - **Layouts** come from the layout probe (`psxw_cc::probe`): sizes,
//!   alignments, and each field's offset and size, under clang for wasm32
//!   and the manifest's reference compiler. What the two disagree on is
//!   dropped and listed (`Snapshot::dropped`).
//! - **Bitfields** appear as the word that holds them (named after the
//!   bitfields, joined with `_`), until the DWARF frontend gives their bits.
//!
//! The result is the symbol-list snapshot with types: names and addresses
//! stay the address map's.

use crate::snapshot::{Field, Param, Provenance, Signature, Snapshot, Source, TypeDef, TypeExpr};
use anyhow::Result;
use psxw_cc::ast::{self, Ast};
use psxw_cc::linemap::LineMap;
use psxw_cc::probe::{self, Outcome, Reference};
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use tree_sitter::Node;

/// Bumped when the frontend reads headers differently (its cache key).
pub const FRONTEND_VERSION: u32 = 7;

/// An array whose count comes from the probed layout.
const COUNT_FROM_LAYOUT: u32 = u32::MAX;

fn w(s: &str) -> TypeExpr {
    TypeExpr::Width(s.into())
}

/// A primitive C type's width (GCC's MIPS and clang's wasm32 agree on
/// these; the probe checks every layout built from them).
fn primitive(text: &str) -> Option<TypeExpr> {
    let t: Vec<&str> = text.split_whitespace().filter(|t| *t != "int" || text.split_whitespace().count() == 1).collect();
    let t = t.join(" ");
    // psx-wasm's C is compiled as the reference compiler's: plain `char`
    // unsigned (`-funsigned-char`).
    let unsigned_char = psxw_cc::UB_CONTAINMENT_FLAGS.contains(&"-funsigned-char");
    Some(w(match t.as_str() {
        "char" if unsigned_char => "u8",
        "char" | "signed char" => "s8",
        "unsigned char" => "u8",
        "short" | "signed short" | "short signed" => "s16",
        "unsigned short" | "short unsigned" => "u16",
        "int" | "signed" | "long" | "signed long" | "long signed" => "s32",
        "unsigned" | "unsigned long" | "long unsigned" => "u32",
        "long long" | "signed long long" => "s64",
        "unsigned long long" => "u64",
        "float" => "f32",
        "double" | "long double" => "f64",
        "void" => "void",
        "_Bool" | "bool" => "u8",
        _ => return None,
    }))
}

/// A record as one header defines it.
#[derive(Debug, Clone)]
struct RecordDecl {
    id: String,
    union: bool,
    /// How C names it, for probe expressions: `struct Tag`, a typedef name,
    /// or `None` when only a parent reaches it (`via`).
    ctype: Option<String>,
    /// A record inlined in a named field: (the root record's C type, the
    /// path to it there).
    via: Option<(String, String)>,
    /// Paths (`a`, or `inner.a` for an inline record's), types and bit
    /// widths; flattened anonymous members' fields are direct.
    fields: Vec<(String, TypeExpr, Option<u32>)>,
    start: usize,
}

#[derive(Debug, Clone)]
struct GlobalDecl {
    /// The symbol: its `__asm__` label, else its C name.
    name: String,
    cname: String,
    ty: TypeExpr,
    text: String,
    start: usize,
}

#[derive(Debug, Clone)]
struct FuncDecl {
    name: String,
    sig: Signature,
    text: String,
    start: usize,
}

/// One preprocessed header, read.
#[derive(Default)]
struct Tu {
    typedefs: HashMap<String, TypeExpr>,
    /// Typedef names in the header itself, in order.
    own_typedefs: Vec<(String, TypeExpr, usize)>,
    enums: BTreeMap<String, (BTreeMap<String, i64>, usize)>,
    constants: HashMap<String, i64>,
    records: Vec<RecordDecl>,
    globals: Vec<GlobalDecl>,
    functions: Vec<FuncDecl>,
}

struct Reader<'a> {
    ast: &'a Ast,
    tu: Tu,
}

impl<'a> Reader<'a> {
    fn text(&self, n: Node) -> &'a str {
        let s: &'a str = &self.ast.src;
        &s[n.byte_range()]
    }

    /// An integer constant expression: literals, enumerators, arithmetic.
    fn eval(&self, n: Node) -> Option<i64> {
        match n.kind() {
            "number_literal" => {
                let t = self.text(n).trim_end_matches(['u', 'U', 'l', 'L']);
                if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                    i64::from_str_radix(h, 16).ok()
                } else if t.len() > 1 && t.starts_with('0') {
                    i64::from_str_radix(&t[1..], 8).ok()
                } else {
                    t.parse().ok()
                }
            }
            "char_literal" => self.text(n).trim_matches('\'').bytes().next().map(i64::from),
            "identifier" => self.tu.constants.get(self.text(n)).copied(),
            "parenthesized_expression" => self.eval(n.named_child(0)?),
            "cast_expression" => self.eval(n.child_by_field_name("value")?),
            "unary_expression" => {
                let v = self.eval(n.child_by_field_name("argument")?)?;
                match self.text(n.child_by_field_name("operator")?) {
                    "-" => Some(-v),
                    "+" => Some(v),
                    "~" => Some(!v),
                    "!" => Some((v == 0) as i64),
                    _ => None,
                }
            }
            "binary_expression" => {
                let a = self.eval(n.child_by_field_name("left")?)?;
                let b = self.eval(n.child_by_field_name("right")?)?;
                Some(match self.text(n.child_by_field_name("operator")?) {
                    "+" => a + b,
                    "-" => a - b,
                    "*" => a * b,
                    "/" if b != 0 => a / b,
                    "%" if b != 0 => a % b,
                    "<<" => a << b,
                    ">>" => a >> b,
                    "|" => a | b,
                    "&" => a & b,
                    "^" => a ^ b,
                    _ => return None,
                })
            }
            _ => None,
        }
    }

    /// The type a specifier names. `anon` names an anonymous record it
    /// defines; `via` is how C reaches it when no name does.
    fn specifier(&mut self, n: Node, anon: Option<String>, via: Option<(String, String)>) -> TypeExpr {
        match n.kind() {
            "primitive_type" | "sized_type_specifier" => primitive(self.text(n)).unwrap_or_else(|| w("s32")),
            "type_identifier" => {
                let name = self.text(n);
                self.tu.typedefs.get(name).cloned().unwrap_or(TypeExpr::Named { named: name.to_string() })
            }
            "struct_specifier" | "union_specifier" => {
                let keyword = if n.kind() == "struct_specifier" { "struct" } else { "union" };
                let id = match n.child_by_field_name("name") {
                    Some(name) => format!("{keyword} {}", self.text(name)),
                    None => match anon {
                        Some(a) => a,
                        None => return w("u8"),
                    },
                };
                if let Some(body) = n.child_by_field_name("body") {
                    let named = n.child_by_field_name("name").is_some();
                    let ctype = if named { Some(id.clone()) } else { via.is_none().then(|| id.clone()) };
                    self.record(body, id.clone(), keyword == "union", ctype, if named { None } else { via }, n.start_byte());
                }
                TypeExpr::Record { record: id }
            }
            "enum_specifier" => {
                if let Some(body) = n.child_by_field_name("body") {
                    let mut values = BTreeMap::new();
                    let mut next = 0i64;
                    let mut c = body.walk();
                    for e in body.named_children(&mut c).filter(|e| e.kind() == "enumerator") {
                        let Some(name) = e.child_by_field_name("name").map(|x| self.text(x).to_string()) else { continue };
                        let v = match e.child_by_field_name("value") {
                            Some(x) => self.eval(x).unwrap_or(next),
                            None => next,
                        };
                        self.tu.constants.insert(name.clone(), v);
                        values.insert(name, v);
                        next = v + 1;
                    }
                    let id = n.child_by_field_name("name").map(|x| format!("enum {}", self.text(x))).or(anon);
                    if let Some(id) = &id {
                        self.tu.enums.insert(id.clone(), (values, n.start_byte()));
                    }
                }
                match n.child_by_field_name("name") {
                    Some(x) => TypeExpr::Named { named: format!("enum {}", self.text(x)) },
                    None => w("s32"),
                }
            }
            _ => w("s32"),
        }
    }

    /// Apply a declarator to `base`: (its name, its type).
    fn declarator(&mut self, n: Node, base: TypeExpr) -> (Option<String>, TypeExpr) {
        match n.kind() {
            "identifier" | "field_identifier" | "type_identifier" => (Some(self.text(n).to_string()), base),
            "pointer_declarator" | "abstract_pointer_declarator" => {
                let t = TypeExpr::Ptr { ptr: Box::new(base) };
                match n.child_by_field_name("declarator") {
                    Some(d) => self.declarator(d, t),
                    None => (None, t),
                }
            }
            "array_declarator" | "abstract_array_declarator" => {
                let count = n.child_by_field_name("size").map_or(Some(0), |s| self.eval(s)).map_or(COUNT_FROM_LAYOUT, |v| v as u32);
                let t = TypeExpr::Array { array: Box::new(base), count };
                match n.child_by_field_name("declarator") {
                    Some(d) => self.declarator(d, t),
                    None => (None, t),
                }
            }
            "function_declarator" | "abstract_function_declarator" => {
                let sig = self.signature(n, base);
                let t = TypeExpr::Fn { func: Box::new(sig) };
                match n.child_by_field_name("declarator") {
                    Some(d) => self.declarator(d, t),
                    None => (None, t),
                }
            }
            "parenthesized_declarator" | "abstract_parenthesized_declarator" | "attributed_declarator" => match n.named_child(0) {
                Some(d) => self.declarator(d, base),
                None => (None, base),
            },
            "init_declarator" => match n.child_by_field_name("declarator") {
                Some(d) => self.declarator(d, base),
                None => (None, base),
            },
            _ => (None, base),
        }
    }

    fn signature(&mut self, f: Node, result: TypeExpr) -> Signature {
        let mut params = Vec::new();
        let mut variadic = false;
        if let Some(list) = f.child_by_field_name("parameters") {
            let mut c = list.walk();
            for p in list.named_children(&mut c) {
                match p.kind() {
                    "variadic_parameter" => variadic = true,
                    "parameter_declaration" => {
                        let Some(spec) = p.child_by_field_name("type") else { continue };
                        let base = self.specifier(spec, None, None);
                        let (name, ty) = match p.child_by_field_name("declarator") {
                            Some(d) => self.declarator(d, base),
                            None => (None, base),
                        };
                        // `f(void)`.
                        if ty == w("void") && name.is_none() {
                            continue;
                        }
                        // Arrays as parameters are pointers.
                        let ty = match ty {
                            TypeExpr::Array { array, .. } => TypeExpr::Ptr { ptr: array },
                            t => t,
                        };
                        params.push(Param { name, ty });
                    }
                    _ => {}
                }
            }
        }
        Signature { params, result, variadic }
    }

    /// A record's fields (`body`, a field_declaration_list).
    fn record(&mut self, body: Node, id: String, union: bool, ctype: Option<String>, via: Option<(String, String)>, start: usize) {
        if self.tu.records.iter().any(|r| r.id == id) {
            return;
        }
        // Inline records reached through this one take its C type, or its own root's.
        let root = match (&ctype, &via) {
            (Some(c), _) => Some((c.clone(), String::new())),
            (None, Some((c, p))) => Some((c.clone(), format!("{p}."))),
            _ => None,
        };
        let mut fields = Vec::new();
        self.fields(body, &id, root.as_ref(), "", &mut fields);
        self.tu.records.push(RecordDecl { id, union, ctype, via, fields, start });
    }

    fn fields(&mut self, body: Node, id: &str, root: Option<&(String, String)>, prefix: &str, out: &mut Vec<(String, TypeExpr, Option<u32>)>) {
        let mut c = body.walk();
        let decls: Vec<Node> = body.named_children(&mut c).filter(|n| n.kind() == "field_declaration").collect();
        for d in decls {
            let Some(spec) = d.child_by_field_name("type") else { continue };
            let mut c2 = d.walk();
            let declarators: Vec<Node> = d.children_by_field_name("declarator", &mut c2).collect();
            let mut c3 = d.walk();
            let bits = d.named_children(&mut c3).find(|n| n.kind() == "bitfield_clause").and_then(|b| b.named_child(0)).and_then(|e| self.eval(e)).map(|v| v as u32);
            if declarators.is_empty() {
                // An anonymous member: its fields are the record's own.
                if let Some(inner) = spec.child_by_field_name("body") {
                    self.fields(inner, id, root, prefix, out);
                }
                continue;
            }
            for decl in declarators {
                let name = self.ast.declarator_name(decl).unwrap_or_default();
                let path = format!("{prefix}{name}");
                // An inline record is reached through the field: an
                // array's first element, never through a pointer.
                let via = reach(decl).and_then(|index| root.map(|(c, p)| (c.clone(), format!("{p}{path}{index}"))));
                let base = self.specifier(spec, Some(format!("{id}.{name}")), via);
                let (_, ty) = self.declarator(decl, base);
                out.push((path, ty, bits));
            }
        }
    }

    /// Everything one preprocessed header declares.
    fn read(&mut self) {
        let root = self.ast.tree.root_node();
        let mut c = root.walk();
        let nodes: Vec<Node> = root.named_children(&mut c).collect();
        for n in nodes {
            match n.kind() {
                "type_definition" => {
                    let Some(spec) = n.child_by_field_name("type") else { continue };
                    let mut c2 = n.walk();
                    let decls: Vec<Node> = n.children_by_field_name("declarator", &mut c2).collect();
                    // `typedef struct { … } Name;` names the record.
                    let plain = decls.iter().find(|d| d.kind() == "type_identifier").map(|d| self.text(*d).to_string());
                    let base = self.specifier(spec, plain.clone(), None);
                    for d in decls {
                        let (name, ty) = self.declarator(d, base.clone());
                        if let Some(name) = name {
                            self.tu.typedefs.insert(name.clone(), ty.clone());
                            self.tu.own_typedefs.push((name, ty, n.start_byte()));
                        }
                    }
                }
                "declaration" => {
                    let Some(spec) = n.child_by_field_name("type") else { continue };
                    let mut c2 = n.walk();
                    if n.named_children(&mut c2).any(|x| x.kind() == "storage_class_specifier" && self.text(x) == "typedef") {
                        continue;
                    }
                    let mut c3 = n.walk();
                    let decls: Vec<Node> = n.children_by_field_name("declarator", &mut c3).collect();
                    let text = self.text(n).split_whitespace().collect::<Vec<_>>().join(" ");
                    let label = asm_label(&text);
                    for d in decls {
                        let base = self.specifier(spec, None, None);
                        let (name, ty) = self.declarator(d, base);
                        let Some(cname) = name else { continue };
                        match ty {
                            // A function (a pointer to one is a global).
                            TypeExpr::Fn { func } => {
                                if has_parameters(d) {
                                    self.tu.functions.push(FuncDecl { name: cname, sig: *func, text: text.clone(), start: n.start_byte() });
                                }
                            }
                            ty => self.tu.globals.push(GlobalDecl { name: label.clone().unwrap_or_else(|| cname.clone()), cname, ty, text: text.clone(), start: n.start_byte() }),
                        }
                    }
                }
                _ => {
                    // `struct Tag { … };`, `enum { … };` and the like.
                    if matches!(n.kind(), "struct_specifier" | "union_specifier" | "enum_specifier") {
                        self.specifier(n, None, None);
                    }
                }
            }
        }
    }
}

/// How a field's declarator reaches its type: `[0]` per array level, or
/// `None` through a pointer or function.
fn reach(mut d: Node) -> Option<String> {
    let mut index = String::new();
    loop {
        match d.kind() {
            "field_identifier" | "identifier" => return Some(index),
            "array_declarator" => index.push_str("[0]"),
            "parenthesized_declarator" | "attributed_declarator" => {}
            _ => return None,
        }
        d = d.child_by_field_name("declarator").or_else(|| d.named_child(0))?;
    }
}

/// `f(…)` with a parameter list (`f()` declares none: unprototyped).
fn has_parameters(mut d: Node) -> bool {
    while d.kind() != "function_declarator" {
        match d.child_by_field_name("declarator").or_else(|| d.named_child(0)) {
            Some(x) => d = x,
            None => return false,
        }
    }
    d.child_by_field_name("parameters").is_some_and(|p| p.named_child_count() > 0)
}

fn asm_label(decl: &str) -> Option<String> {
    let i = decl.find("__asm__").or_else(|| decl.find("asm("))?;
    let rest = &decl[i..];
    let q = rest.find('"')?;
    let end = rest[q + 1..].find('"')?;
    Some(rest[q + 1..q + 1 + end].to_string())
}

/// One header's result: what it defines, with probed layouts.
struct HeaderResult {
    rel: String,
    records: Vec<(RecordDecl, Option<(u32, u32)>, BTreeMap<String, (u32, u32)>)>,
    enums: Vec<(String, BTreeMap<String, i64>)>,
    typedefs: Vec<(String, TypeExpr)>,
    globals: Vec<(GlobalDecl, Option<u32>)>,
    functions: Vec<FuncDecl>,
    dropped: Vec<String>,
    unchecked: Vec<String>,
}

/// Read and probe one header.
fn header(clang: &psxw_cc::Clang, reference: Option<&Reference>, context: &[String], dir: &str, rel: &str, work: &Path) -> Result<Option<HeaderResult>> {
    let stem = format!("cfront_{}", rel.replace(['/', '.'], "_"));
    let src = work.join(format!("{stem}.c"));
    let mut head = String::new();
    for p in context {
        head.push_str(&format!("#include \"{p}\"\n"));
    }
    head.push_str(&format!("#include \"{rel}\"\n"));
    std::fs::write(&src, &head)?;
    let Ok(text) = clang.preprocess(&src, &["PSXW_CLANG"]) else { return Ok(None) };
    let map = LineMap::new(&text);
    let tree = ast::parse(&text);
    let path = format!("{dir}/{rel}");
    let mine = |offset: usize| map.presumed(map.line_of(offset)).is_some_and(|(f, _)| f == path);
    let mut r = Reader { ast: &tree, tu: Tu::default() };
    r.read();
    let tu = r.tu;

    // Probe entries: each record's size and alignment, each field's
    // offset and size; each global's size.
    let mut entries = Vec::new();
    let records: Vec<RecordDecl> = tu.records.iter().filter(|x| mine(x.start)).cloned().collect();
    for rec in records.iter().filter(|x| x.ctype.is_some()) {
        let c = rec.ctype.as_deref().unwrap_or_default();
        entries.push(probe::entry(format!("sizeof({c})"), &rec.id, None));
        entries.push(probe::entry(format!("__alignof__({c})"), &rec.id, None));
        for (p, _, bits) in &rec.fields {
            if bits.is_none() {
                entries.push(probe::entry(format!("PSXW_OFF({c}, {p})"), &rec.id, Some(&format!("off {p}"))));
                entries.push(probe::entry(format!("sizeof((({c} *)0)->{p})"), &rec.id, Some(&format!("size {p}"))));
            }
        }
    }
    for rec in records.iter().filter(|x| x.ctype.is_none()) {
        if let Some((c, at)) = &rec.via {
            entries.push(probe::entry(format!("sizeof((({c} *)0)->{at})"), &rec.id, None));
            entries.push(probe::entry(format!("__alignof__((({c} *)0)->{at})"), &rec.id, None));
            for (p, _, bits) in &rec.fields {
                if bits.is_none() {
                    entries.push(probe::entry(format!("PSXW_OFF({c}, {at}.{p}) - PSXW_OFF({c}, {at})"), &rec.id, Some(&format!("off {p}"))));
                    entries.push(probe::entry(format!("sizeof((({c} *)0)->{at}.{p})"), &rec.id, Some(&format!("size {p}"))));
                }
            }
        }
    }
    let globals: Vec<GlobalDecl> = tu.globals.iter().filter(|g| mine(g.start)).cloned().collect();
    for g in &globals {
        if !matches!(&g.ty, TypeExpr::Array { count: 0, .. }) {
            entries.push(probe::entry(format!("sizeof({})", g.cname), &format!("global {}", g.cname), None));
        }
    }
    let mut dropped = Vec::new();
    let mut unchecked = Vec::new();
    let mut values: BTreeMap<(String, String), u32> = BTreeMap::new();
    if !entries.is_empty() {
        // Types the header uses that its context doesn't define (m2c's,
        // which the decomp defines in a C file) get an opaque declaration:
        // enough through a pointer; by value, the entry fails alone.
        // (Found with clang alone, which names them; then both compilers.)
        let mut head = head.clone();
        let mut opaque = Vec::new();
        for _ in 0..512 {
            let Outcome::ClangFailed(why) = probe::probe_entries(clang, None, &head, &stem, entries.clone(), 0, work)? else { break };
            let Some(name) = why.split("unknown type name '").nth(1).and_then(|r| r.split('\'').next()).map(str::to_string) else { break };
            if opaque.contains(&name) {
                break;
            }
            let include = format!("#include \"{rel}\"\n");
            head = head.replace(&include, &format!("typedef struct psxw_opaque_{name} {name};\n{include}"));
            opaque.push(name);
        }
        if !opaque.is_empty() {
            unchecked.push(format!("{rel}: probed with opaque {} (its context doesn't define them)", opaque.join(", ")));
        }
        let mut outcome = probe::probe_entries(clang, reference, &head, &stem, entries.clone(), 0, work)?;
        let mut with_reference = reference.is_some();
        // A header the reference compiler can't compile in its context
        // (the census skips it too) has clang's layouts, unchecked.
        if let Outcome::Skipped(why) = &outcome {
            unchecked.push(format!("{rel}: layouts from clang alone (the reference compiler: {why})"));
            outcome = probe::probe_entries(clang, None, &head, &stem, entries, 0, work)?;
            with_reference = false;
        }
        match outcome {
            Outcome::Probed { entries, .. } => {
                for e in entries {
                    let key = (e.entry.ty.clone(), e.entry.expr.clone());
                    match (e.clang, e.reference, with_reference) {
                        (Some(c), Some(g), true) if c == g => {
                            values.insert(key, c);
                        }
                        (Some(c), None, false) => {
                            values.insert(key, c);
                        }
                        (c, g, _) => {
                            if e.clang_error.is_none() && e.reference_error.is_none() {
                                dropped.push(format!("{rel}: {}: clang {c:?}, reference {g:?}", e.entry.expr));
                            }
                        }
                    }
                }
            }
            Outcome::Skipped(why) | Outcome::ClangFailed(why) => dropped.push(format!("{rel}: not probed: {why}")),
        }
    }
    let get = |ty: &str, expr: String| values.get(&(ty.to_string(), expr)).copied();
    let mut out_records = Vec::new();
    for rec in records {
        let (size_e, align_e, field) = match (&rec.ctype, &rec.via) {
            (Some(c), _) => (format!("sizeof({c})"), format!("__alignof__({c})"), None),
            (None, Some((c, at))) => (format!("sizeof((({c} *)0)->{at})"), format!("__alignof__((({c} *)0)->{at})"), Some((c.clone(), at.clone()))),
            _ => continue,
        };
        let layout = get(&rec.id, size_e).zip(get(&rec.id, align_e));
        let mut fields = BTreeMap::new();
        for (p, _, bits) in &rec.fields {
            if bits.is_some() {
                continue;
            }
            let (off, size) = match (&rec.ctype, &field) {
                (Some(c), _) => (format!("PSXW_OFF({c}, {p})"), format!("sizeof((({c} *)0)->{p})")),
                (None, Some((c, at))) => (format!("PSXW_OFF({c}, {at}.{p}) - PSXW_OFF({c}, {at})"), format!("sizeof((({c} *)0)->{at}.{p})")),
                _ => continue,
            };
            if let (Some(o), Some(s)) = (get(&rec.id, off), get(&rec.id, size)) {
                fields.insert(p.clone(), (o, s));
            }
        }
        out_records.push((rec, layout, fields));
    }
    let out_globals = globals.into_iter().map(|g| {
        let size = get(&format!("global {}", g.cname), format!("sizeof({})", g.cname));
        (g, size)
    });
    Ok(Some(HeaderResult {
        rel: rel.to_string(),
        records: out_records,
        enums: tu.enums.iter().filter(|(_, (_, s))| mine(*s)).map(|(k, (v, _))| (k.clone(), v.clone())).collect(),
        typedefs: tu.own_typedefs.iter().filter(|(_, _, s)| mine(*s)).map(|(n, t, _)| (n.clone(), t.clone())).collect(),
        globals: out_globals.collect(),
        functions: tu.functions.iter().filter(|f| mine(f.start)).cloned().collect(),
        dropped,
        unchecked,
    }))
}

/// A type's size, from widths and the records' probed sizes.
pub fn size_of(t: &TypeExpr, types: &BTreeMap<String, TypeDef>) -> Option<u32> {
    match t {
        TypeExpr::Width(s) => Some(match s.as_str() {
            "u8" | "s8" => 1,
            "u16" | "s16" => 2,
            "u32" | "s32" | "f32" => 4,
            "u64" | "s64" | "f64" => 8,
            _ => return None,
        }),
        TypeExpr::Ptr { .. } | TypeExpr::Fn { .. } => Some(4),
        TypeExpr::Array { array, count } => size_of(array, types).map(|s| s * count),
        TypeExpr::Record { record } => match types.get(record)? {
            TypeDef::Struct { size, .. } | TypeDef::Union { size, .. } => Some(*size),
            _ => None,
        },
        TypeExpr::Named { named } => match types.get(named)? {
            TypeDef::Enum { size, .. } => Some(*size),
            TypeDef::Typedef { ty } => size_of(ty, types),
            _ => None,
        },
    }
}

/// An array whose count the declaration doesn't give: from its size.
fn fix_count(t: TypeExpr, size: Option<u32>, types: &BTreeMap<String, TypeDef>) -> TypeExpr {
    match t {
        TypeExpr::Array { array, count } if count == COUNT_FROM_LAYOUT || count == 0 => {
            let n = size.zip(size_of(&array, types)).filter(|(_, e)| *e > 0).map(|(s, e)| s / e).unwrap_or(0);
            TypeExpr::Array { array, count: n }
        }
        t => t,
    }
}

/// The C-header frontend's snapshot: `base` (the symbol-list snapshot)
/// with every declaration's type and every record's probed layout.
pub fn snapshot(
    base: Snapshot,
    clang: &psxw_cc::Clang,
    reference: Option<&Reference>,
    snap: &Path,
    manifest: &psxw_inputs::GameManifest,
    map: &psxw_inputs::AddressMap,
    work: &Path,
) -> Result<Snapshot> {
    let headers = crate::headers::list(snap, manifest)?;
    let contexts = psxw_census::include_contexts(snap, map);
    std::fs::create_dir_all(work)?;
    let results: Vec<Result<Option<HeaderResult>>> = headers
        .par_iter()
        .map(|(dir, rel)| {
            let context = contexts.get(rel).cloned().unwrap_or_else(|| manifest.probe.prelude.clone());
            header(clang, reference, &context, dir, rel, work)
        })
        .collect();
    let mut s = base;
    s.source = Source { kind: "c".into(), version: s.source.version.clone(), name: None, merged: vec![] };
    let mut types: BTreeMap<String, TypeDef> = BTreeMap::new();
    let mut globals: BTreeMap<String, (TypeExpr, Option<u32>, Provenance, bool)> = BTreeMap::new();
    let mut functions: BTreeMap<String, (Signature, Provenance)> = BTreeMap::new();
    let mut pending: Vec<(String, bool, Vec<(String, TypeExpr, Option<u32>)>, BTreeMap<String, (u32, u32)>, (u32, u32), String)> = Vec::new();
    for r in results {
        let Some(h) = r? else { continue };
        s.dropped.extend(h.dropped);
        s.unchecked.extend(h.unchecked);
        let prov = |text: Option<String>| Provenance { source: "c".into(), file: Some(h.rel.clone()), text };
        for (id, values) in h.enums {
            types.entry(id).or_insert(TypeDef::Enum { size: 4, values });
        }
        for (rec, layout, fields) in h.records {
            match layout {
                Some(l) if !types.contains_key(&rec.id) && !pending.iter().any(|p| p.0 == rec.id) => pending.push((rec.id, rec.union, rec.fields, fields, l, h.rel.clone())),
                Some(_) => {}
                None => s.dropped.push(format!("{}: {}: no layout both compilers agree on", h.rel, rec.id)),
            }
        }
        for (name, ty) in h.typedefs {
            // `typedef struct { … } Name;` is the record `Name` itself.
            if ty == (TypeExpr::Record { record: name.clone() }) {
                continue;
            }
            types.entry(name).or_insert(TypeDef::Typedef { ty });
        }
        for (g, size) in h.globals {
            // Of several declarations of one symbol (views under other C
            // names, by `__asm__` label), the one under its own name.
            let plain = g.cname == g.name;
            match globals.get(&g.name) {
                Some((_, _, _, true)) => {}
                Some(_) if !plain => {}
                _ => {
                    globals.insert(g.name.clone(), (g.ty, size, prov(Some(g.text)), plain));
                }
            }
        }
        for f in h.functions {
            functions.entry(f.name.clone()).or_insert((f.sig, prov(Some(f.text))));
        }
    }
    // Records' sizes first, then their fields' types (an array's count
    // can need another record's size).
    for (id, union, _, _, (size, align), rel) in &pending {
        let def = if *union {
            TypeDef::Union { size: *size, align: *align, fields: vec![], provenance: Some(Provenance { source: "c".into(), file: Some(rel.clone()), text: None }) }
        } else {
            TypeDef::Struct { size: *size, align: *align, fields: vec![], provenance: Some(Provenance { source: "c".into(), file: Some(rel.clone()), text: None }) }
        };
        types.entry(id.clone()).or_insert(def);
    }
    for (id, _, decls, layout, _, rel) in pending {
        let mut fields = Vec::new();
        let mut run: Vec<(String, TypeExpr)> = Vec::new();
        let mut end = 0u32;
        let flush = |run: &mut Vec<(String, TypeExpr)>, end: u32, fields: &mut Vec<Field>| {
            if let Some((_, t)) = run.first() {
                let unit = size_of(t, &BTreeMap::new()).unwrap_or(4).max(1);
                let offset = end.div_ceil(unit) * unit;
                let name = run.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join("_");
                fields.push(Field { name, offset, ty: t.clone(), bits: None });
            }
            run.clear();
        };
        for (path, ty, bits) in decls {
            if path.contains('.') {
                continue;
            }
            if bits.is_some() {
                run.push((path, ty));
                continue;
            }
            flush(&mut run, end, &mut fields);
            match layout.get(&path) {
                Some(&(offset, size)) => {
                    end = offset + size;
                    fields.push(Field { name: path, offset, ty: fix_count(ty, Some(size), &types), bits: None });
                }
                None => s.dropped.push(format!("{rel}: {id}.{path}: no layout both compilers agree on")),
            }
        }
        flush(&mut run, end, &mut fields);
        if let Some(TypeDef::Struct { fields: f, .. } | TypeDef::Union { fields: f, .. }) = types.get_mut(&id) {
            *f = fields;
        }
    }
    for g in &mut s.globals {
        let found = g.names.iter().find_map(|n| globals.get(n));
        if let Some((ty, size, prov, _)) = found {
            g.ty = Some(fix_count(ty.clone(), size.or(g.size), &types));
            if g.size.is_none() {
                g.size = *size;
            }
            g.provenance = Some(prov.clone());
        }
    }
    for f in &mut s.functions {
        if let Some((sig, prov)) = f.names.iter().find_map(|n| functions.get(n)) {
            f.signature = Some(sig.clone());
            f.provenance = Some(prov.clone());
        }
    }
    s.types = types;
    s.dropped.sort();
    s.dropped.dedup();
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(src: &str) -> Tu {
        let ast = ast::parse(src);
        let mut r = Reader { ast: &ast, tu: Tu::default() };
        r.read();
        r.tu
    }

    #[test]
    fn declarations_as_types() {
        let tu = read(
            "typedef unsigned char u8; typedef int s32; typedef unsigned int u32;\n\
             typedef struct { s32 hp; u8 name[4]; struct { short x, y; } pos; union { s32 a; u32 b; }; u32 f: 3; } Player;\n\
             extern Player gPlayer;\n\
             extern s32 _gBase[3] __asm__(\"gBase\");\n\
             u32 Rand(s32 limit);\n\
             void (*gHandlers[4])(void *work);\n\
             void Nothing(void);\n\
             int Old();\n\
             enum Kind { A, B = 5, C };\n",
        );
        let p = tu.records.iter().find(|r| r.id == "Player").unwrap();
        let names: Vec<&str> = p.fields.iter().map(|f| f.0.as_str()).collect();
        assert_eq!(names, ["hp", "name", "pos", "a", "b", "f"]);
        assert_eq!(p.fields[1].1, TypeExpr::Array { array: Box::new(w("u8")), count: 4 });
        assert_eq!(p.fields[2].1, TypeExpr::Record { record: "Player.pos".into() });
        assert_eq!(p.fields[5].2, Some(3));
        let pos = tu.records.iter().find(|r| r.id == "Player.pos").unwrap();
        assert_eq!(pos.via, Some(("Player".into(), "pos".into())));
        let g: Vec<(&str, &str)> = tu.globals.iter().map(|g| (g.name.as_str(), g.cname.as_str())).collect();
        assert_eq!(g, [("gPlayer", "gPlayer"), ("gBase", "_gBase"), ("gHandlers", "gHandlers")]);
        assert_eq!(tu.globals[1].ty, TypeExpr::Array { array: Box::new(w("s32")), count: 3 });
        let f: Vec<&str> = tu.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(f, ["Rand", "Nothing"]);
        assert_eq!(tu.functions[0].sig.params, [Param { name: Some("limit".into()), ty: w("s32") }]);
        assert_eq!(tu.functions[0].sig.result, w("u32"));
        assert!(tu.functions[1].sig.params.is_empty());
        assert_eq!(tu.enums["enum Kind"].0["C"], 6);
    }
}

/// What [`verify`] found.
#[derive(Debug, Default, serde::Serialize)]
pub struct Agreement {
    /// Census probe entries whose record or global the snapshot holds.
    pub layouts: usize,
    /// Names whose address the address map gives.
    pub addresses: usize,
    pub disagreements: Vec<String>,
}

/// Plans/M12 phase 2 exit criterion 2: the snapshot agrees with the census's
/// layout probe (`psxw_cc::probe::probe_header`, as `psxw census` runs it) on
/// every layout it holds (each record's size and alignment, each field's
/// offset, each typed global's size), and with the address map on every
/// address.
pub fn verify(
    s: &Snapshot,
    clang: &psxw_cc::Clang,
    reference: &Reference,
    snap: &Path,
    manifest: &psxw_inputs::GameManifest,
    map: &psxw_inputs::AddressMap,
    work: &Path,
) -> Result<Agreement> {
    let headers = crate::headers::list(snap, manifest)?;
    let contexts = psxw_census::include_contexts(snap, map);
    let outcomes: Vec<Result<Outcome>> = headers
        .par_iter()
        .map(|(dir, rel)| {
            let prelude = contexts.get(rel).cloned().unwrap_or_else(|| manifest.probe.prelude.clone());
            probe::probe_header(clang, reference, &prelude, dir, rel, work)
        })
        .collect();
    let mut a = Agreement::default();
    let globals: HashMap<&str, &crate::snapshot::Global> = s.globals.iter().flat_map(|g| g.names.iter().map(move |n| (n.as_str(), g))).collect();
    for o in outcomes {
        let Outcome::Probed { entries, .. } = o? else { continue };
        for e in entries {
            let (Some(v), Some(r)) = (e.clang, e.reference) else { continue };
            if v != r {
                continue;
            }
            let ty = e.entry.ty.as_str();
            let expr = e.entry.expr.as_str();
            let got = match s.types.get(ty) {
                Some(TypeDef::Struct { size, align, fields, .. } | TypeDef::Union { size, align, fields, .. }) => {
                    if expr == format!("sizeof({ty})") {
                        Some(*size)
                    } else if expr == format!("__alignof__({ty})") {
                        Some(*align)
                    } else if let Some(f) = &e.entry.field {
                        match fields.iter().find(|x| x.name == *f) {
                            Some(x) => Some(x.offset),
                            None => {
                                // A field the snapshot dropped isn't held.
                                if !s.dropped.iter().any(|d| d.contains(&format!("{ty}.{f}"))) {
                                    a.disagreements.push(format!("{ty}.{f}: the census probes it, the snapshot has no such field"));
                                }
                                continue;
                            }
                        }
                    } else {
                        continue;
                    }
                }
                _ => {
                    // A symbol several declarations view (`__asm__` labels,
                    // different C names): the snapshot holds one of them.
                    let Some(g) = globals.get(ty) else { continue };
                    let (Some(t), Some(cname)) = (g.ty.as_ref(), expr.strip_prefix("sizeof(").and_then(|r| r.strip_suffix(')'))) else { continue };
                    let declared = g.provenance.as_ref().and_then(|p| p.text.as_deref()).is_some_and(|text| {
                        let text = text.split("__asm__").next().unwrap_or(text);
                        text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).any(|w| w == cname)
                    });
                    if !declared {
                        continue;
                    }
                    size_of(t, &s.types)
                }
            };
            a.layouts += 1;
            if got != Some(v) {
                a.disagreements.push(format!("{expr}: the census probe says {v}, the snapshot {got:?}"));
            }
        }
    }
    let check = |container: &str, address: u32, names: &[String], a: &mut Agreement| {
        for n in names {
            match map.resolve(container, n) {
                Some(r) if r.addr == address => a.addresses += 1,
                Some(r) => a.disagreements.push(format!("{n}: the address map says {container} 0x{:08X}, the snapshot 0x{address:08X}", r.addr)),
                None => a.disagreements.push(format!("{n}: the address map doesn't resolve it in {container}")),
            }
        }
    };
    for f in &s.functions {
        check(&f.container, f.address, &f.names, &mut a);
    }
    for g in &s.globals {
        check(&g.container, g.address, &g.names, &mut a);
    }
    Ok(a)
}
