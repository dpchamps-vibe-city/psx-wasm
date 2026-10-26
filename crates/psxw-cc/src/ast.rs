//! tree-sitter-c over preprocessed text: function extents, top-level
//! declarations, and struct/union definitions. Line markers are blanked
//! first, so byte offsets match the original text.

use std::ops::Range;
use tree_sitter::{Node, Parser, Tree};

pub struct Ast {
    pub tree: Tree,
    pub src: String,
}

pub fn parse(text: &str) -> Ast {
    let src = crate::linemap::blank_markers(text);
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_c::LANGUAGE.into()).expect("tree-sitter-c version");
    let tree = parser.parse(&src, None).expect("parse without timeout");
    Ast { tree, src }
}

#[derive(Debug, Clone)]
pub struct FnDef {
    pub range: Range<usize>,
    /// End of the declarator: `text[range.start..decl_end]` is a prototype.
    pub decl_end: usize,
    pub name: String,
}

impl Ast {
    fn text(&self, n: Node) -> &str {
        n.utf8_text(self.src.as_bytes()).unwrap_or_default()
    }

    /// Function definitions at file scope.
    pub fn functions(&self) -> Vec<FnDef> {
        let root = self.tree.root_node();
        let mut cursor = root.walk();
        root.named_children(&mut cursor)
            .filter(|n| n.kind() == "function_definition")
            .filter_map(|n| {
                let decl = n.child_by_field_name("declarator")?;
                Some(FnDef { range: n.byte_range(), decl_end: decl.end_byte(), name: self.declarator_name(decl)? })
            })
            .collect()
    }

    /// Local names of GNU nested functions *defined* inside `f`.
    pub fn nested_functions(&self, f: &FnDef) -> Vec<String> {
        let Some(body) = self.top_level_at(f.range.start).and_then(|n| n.child_by_field_name("body")) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        self.collect_nested(body, &mut out);
        out
    }

    fn collect_nested(&self, node: Node, out: &mut Vec<String>) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "function_definition" {
                if let Some(name) = child.child_by_field_name("declarator").and_then(|d| self.declarator_name(d)) {
                    out.push(name);
                }
            }
            self.collect_nested(child, out);
        }
    }

    /// The file-scope node containing `offset`.
    pub fn top_level_at(&self, offset: usize) -> Option<Node<'_>> {
        let root = self.tree.root_node();
        let mut cursor = root.walk();
        root.named_children(&mut cursor).find(|n| n.start_byte() <= offset && offset < n.end_byte())
    }

    /// End of the first file-scope node at or after `after` that defines
    /// `struct name` / `union name` with a body.
    pub fn record_definition_end(&self, keyword: &str, name: &str, after: usize) -> Option<usize> {
        let root = self.tree.root_node();
        let mut cursor = root.walk();
        let kind = format!("{keyword}_specifier");
        root.named_children(&mut cursor)
            .filter(|n| n.start_byte() >= after)
            .find(|n| self.defines_record(*n, &kind, name))
            .map(|n| n.end_byte())
    }

    fn defines_record(&self, node: Node, kind: &str, name: &str) -> bool {
        if node.kind() == kind
            && node.child_by_field_name("body").is_some()
            && node.child_by_field_name("name").is_some_and(|n| self.text(n) == name)
        {
            return true;
        }
        let mut cursor = node.walk();
        node.named_children(&mut cursor).any(|c| self.defines_record(c, kind, name))
    }

    pub fn declarator_name(&self, mut node: Node) -> Option<String> {
        loop {
            match node.kind() {
                "identifier" | "field_identifier" | "type_identifier" => return Some(self.text(node).to_string()),
                _ => {
                    node = match node.child_by_field_name("declarator") {
                        Some(d) => d,
                        None => {
                            let mut cursor = node.walk();
                            let next = node.named_children(&mut cursor).find(|c| {
                                matches!(
                                    c.kind(),
                                    "identifier"
                                        | "field_identifier"
                                        | "type_identifier"
                                        | "parenthesized_declarator"
                                        | "pointer_declarator"
                                        | "function_declarator"
                                        | "array_declarator"
                                        | "attributed_declarator"
                                )
                            });
                            next?
                        }
                    }
                }
            }
        }
    }

    /// Every struct/union with a body that can be named as a type, plus its
    /// directly accessible fields (anonymous members flattened).
    pub fn records(&self) -> Vec<Record> {
        let mut out = Vec::new();
        self.collect_records(self.tree.root_node(), &mut out);
        out
    }

    fn collect_records(&self, node: Node, out: &mut Vec<Record>) {
        if matches!(node.kind(), "struct_specifier" | "union_specifier") {
            if let Some(body) = node.child_by_field_name("body") {
                let keyword = if node.kind() == "struct_specifier" { "struct" } else { "union" };
                let ty = match node.child_by_field_name("name") {
                    Some(n) => Some(format!("{keyword} {}", self.text(n))),
                    None => self.typedef_name(node),
                };
                if let Some(ty) = ty {
                    let mut fields = Vec::new();
                    let mut bitfields = 0;
                    self.collect_fields(body, &mut fields, &mut bitfields);
                    out.push(Record { ty, fields, bitfields, start: node.start_byte() });
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.collect_records(child, out);
        }
    }

    /// `typedef struct { ... } Name;` gives the anonymous struct a name.
    fn typedef_name(&self, spec: Node) -> Option<String> {
        let parent = spec.parent()?;
        if parent.kind() != "type_definition" {
            return None;
        }
        let mut cursor = parent.walk();
        let found = parent
            .children_by_field_name("declarator", &mut cursor)
            .find(|d| d.kind() == "type_identifier")
            .map(|d| self.text(d).to_string());
        found
    }

    fn collect_fields(&self, body: Node, fields: &mut Vec<String>, bitfields: &mut u32) {
        let mut cursor = body.walk();
        for decl in body.named_children(&mut cursor).filter(|n| n.kind() == "field_declaration") {
            let mut c2 = decl.walk();
            if decl.named_children(&mut c2).any(|n| n.kind() == "bitfield_clause") {
                *bitfields += 1;
                continue;
            }
            let mut c3 = decl.walk();
            let declarators: Vec<Node> = decl.children_by_field_name("declarator", &mut c3).collect();
            if declarators.is_empty() {
                // Anonymous struct/union member: its fields are direct members.
                if let Some(inner) = decl.child_by_field_name("type").and_then(|t| t.child_by_field_name("body")) {
                    self.collect_fields(inner, fields, bitfields);
                }
                continue;
            }
            for d in declarators {
                if let Some(name) = self.declarator_name(d) {
                    fields.push(name);
                }
            }
        }
    }

    /// File-scope object declarations (`extern T x[3];`, `T y;`), excluding
    /// functions and arrays of unknown size.
    pub fn objects(&self) -> Vec<Object> {
        let root = self.tree.root_node();
        let mut cursor = root.walk();
        let mut out = Vec::new();
        for decl in root.named_children(&mut cursor).filter(|n| n.kind() == "declaration") {
            let mut c2 = decl.walk();
            let is_typedef_like = decl
                .named_children(&mut c2)
                .any(|n| n.kind() == "storage_class_specifier" && self.text(n) == "typedef");
            if is_typedef_like {
                continue;
            }
            let label = asm_label(self.text(decl));
            let mut c3 = decl.walk();
            for d in decl.children_by_field_name("declarator", &mut c3) {
                let d = if d.kind() == "init_declarator" { d.child_by_field_name("declarator").unwrap_or(d) } else { d };
                if d.kind() == "function_declarator" || unsized_array(d) {
                    continue;
                }
                if let Some(name) = self.declarator_name(d) {
                    out.push(Object { name, asm_label: label.clone(), start: decl.start_byte() });
                }
            }
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct Record {
    /// A type expression: `struct Tag` or a typedef name.
    pub ty: String,
    pub fields: Vec<String>,
    pub bitfields: u32,
    pub start: usize,
}

#[derive(Debug, Clone)]
pub struct Object {
    pub name: String,
    pub asm_label: Option<String>,
    pub start: usize,
}

#[cfg(test)]
mod tests {
    #[test]
    fn nested_definitions() {
        let src = "int outer(int a) {\n  auto int bound(int) __asm__(\"Elsewhere\");\n  auto void inner(int r) __asm__(\"Original\");\n  int plain(int x) { return x; }\n  void inner(int r) { }\n  return plain(a) + bound(a);\n}\n";
        let ast = super::parse(src);
        let f = &ast.functions()[0];
        assert_eq!(f.name, "outer");
        assert_eq!(ast.nested_functions(f), vec!["plain".to_string(), "inner".to_string()]);
    }
}

fn unsized_array(d: Node) -> bool {
    d.kind() == "array_declarator" && d.child_by_field_name("size").is_none()
}

fn asm_label(decl: &str) -> Option<String> {
    let i = decl.find("__asm__").or_else(|| decl.find("asm"))?;
    let rest = &decl[i..];
    let q = rest.find('"')?;
    let end = rest[q + 1..].find('"')?;
    Some(rest[q + 1..q + 1 + end].to_string())
}

/// The harness variant of a normalized TU: every non-static, non-inline
/// function definition becomes `weak`, so clang can't use one function's
/// body when compiling another (no inlining, no call deletion, no
/// interprocedural constant propagation). Each function then compiles as
/// it would in isolation, which is what a function-level comparison needs.
pub fn harness_variant(text: &str) -> String {
    let ast = parse(text);
    let mut inserts: Vec<usize> = Vec::new();
    for f in ast.functions() {
        let head = &ast.src[f.range.start..f.decl_end];
        let specifiers = head.split(&f.name).next().unwrap_or("");
        let words: Vec<&str> = specifiers.split(|c: char| !c.is_alphanumeric() && c != '_').collect();
        if words.iter().any(|w| matches!(*w, "static" | "inline" | "__inline" | "__inline__")) {
            continue;
        }
        inserts.push(f.range.start);
    }
    let mut out = String::with_capacity(text.len() + inserts.len() * 24);
    let mut at = 0;
    for i in inserts {
        out.push_str(&text[at..i]);
        out.push_str("__attribute__((weak)) ");
        at = i;
    }
    out.push_str(&text[at..]);
    out
}
