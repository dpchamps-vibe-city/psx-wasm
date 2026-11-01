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
    pub fn text(&self, n: Node) -> &str {
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
    /// `struct name` / `union name` with a body. A bare `struct X { … };`
    /// is a specifier node whose `;` is a separate token; the end is past
    /// that `;`.
    pub fn record_definition_end(&self, keyword: &str, name: &str, after: usize) -> Option<usize> {
        let root = self.tree.root_node();
        let mut cursor = root.walk();
        let kind = format!("{keyword}_specifier");
        root.named_children(&mut cursor)
            .filter(|n| n.start_byte() >= after)
            .find(|n| self.defines_record(*n, &kind, name))
            .map(|n| match n.next_sibling() {
                Some(s) if s.kind() == ";" => s.end_byte(),
                _ => n.end_byte(),
            })
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
    /// Divisions by a non-literal are rewritten in place, nested ones
    /// inside their operands, on the lines they took; a literal divisor
    /// stays, and so does a compound assignment whose left side isn't a
    /// plain variable (rewriting it would evaluate that side twice).
    #[test]
    fn divisions() {
        let src = "int f(int a, int b, int *p, int i, int arr[4]) {\n  a /= b;\n  *p++ /= b;\n  arr[i] /= b;\n  return a\n    / b + a / 4 + (a / b) % b + a / b / b;\n}\n";
        let text = crate::edit::apply(src, super::divisions(&super::parse(src)));
        assert_eq!(text.lines().count(), src.lines().count());
        assert!(text.contains("  a = __builtin_choose_expr(__builtin_constant_p(b)"), "{text}");
        assert!(text.contains("*p++ /= b;") && text.contains("arr[i] /= b;") && text.contains(" + a / 4 + "), "{text}");
        assert!(text.contains("__psxw_rem_s(((__builtin_choose_expr(") && !text.contains("a / b"), "{text}");
    }

    /// Every loop body starts with a barrier, a body that isn't a block
    /// becoming one; a label only a later `goto` reaches is a loop head;
    /// `do { } while (0)` isn't a loop.
    #[test]
    fn loop_barriers() {
        let src = "int g;\nvoid f(int n) {\n  while (g == 0) ;\n  for (; n; n--) { g++; }\n  do g--; while (g);\nback:\n  if (g) goto back;\n  goto out;\nout:\n  do { g = 1; } while (0);\n}\n";
        let text = crate::edit::apply(src, super::loop_barriers(&super::parse(src)));
        let b = super::BARRIER;
        assert_eq!(text.matches(b).count(), 4, "{text}");
        assert!(text.contains(&format!("while (g == 0) {{ {b} ; }}")), "{text}");
        assert!(text.contains(&format!("{{ {b} g++; }}")), "{text}");
        assert!(text.contains(&format!("do {{ {b} g--; }} while (g);")), "{text}");
        assert!(text.contains(&format!("back:\n  {b} if (g)")), "{text}");
        assert_eq!(text.lines().count(), src.lines().count());
    }

    #[test]
    fn nested_definitions() {
        let src = "int outer(int a) {\n  auto int bound(int) __asm__(\"Elsewhere\");\n  auto void inner(int r) __asm__(\"Original\");\n  int plain(int x) { return x; }\n  void inner(int r) { }\n  return plain(a) + bound(a);\n}\n";
        let ast = super::parse(src);
        let f = &ast.functions()[0];
        assert_eq!(f.name, "outer");
        assert_eq!(ast.nested_functions(f), vec!["plain".to_string(), "inner".to_string()]);
    }

    #[test]
    fn record_definition_ends_past_its_semicolon() {
        let bare = "extern struct D a[];\nstruct D {\n int x;\n};\nint y;\n";
        let ast = super::parse(bare);
        assert_eq!(ast.record_definition_end("struct", "D", 20), Some(bare.find("};").unwrap() + 2));
        let typedef = "extern struct E b[];\ntypedef struct E {\n int x;\n} E;\nint y;\n";
        let ast = super::parse(typedef);
        assert_eq!(ast.record_definition_end("struct", "E", 20), Some(typedef.find("} E;").unwrap() + 4));
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

/// A compiler memory barrier, as lifted loop heads have (psxw-lift's emit).
pub const BARRIER: &str = "__asm__ __volatile__(\"\" ::: \"memory\");";

/// Edits putting [`BARRIER`] at the top of every loop body in the TU's
/// functions (`for`, `while`, `do`, and a label a later `goto` jumps back
/// to), on the body's own line. Interrupt handlers and callbacks run at
/// safepoints the linker adds to loops after clang, so clang must not keep
/// a load in a register across iterations: the original reads memory each
/// pass. A body that isn't a block becomes one; `do { } while (0)` runs once.
pub fn loop_barriers(ast: &Ast) -> Vec<crate::edit::Edit> {
    use crate::edit::Edit;
    fn walk(ast: &Ast, n: Node, labels: &mut Vec<(String, usize, usize)>, gotos: &mut Vec<(String, usize)>, out: &mut Vec<Edit>) {
        // `do { } while (0)` (a statement macro's) runs once: no loop.
        let once = n.kind() == "do_statement"
            && n.child_by_field_name("condition").is_some_and(|c| ast.text(c).trim_matches(['(', ')', ' ']) == "0");
        match n.kind() {
            "for_statement" | "while_statement" | "do_statement" if !once => {
                if let Some(body) = n.child_by_field_name("body") {
                    if body.kind() == "compound_statement" {
                        out.push(Edit::insert(body.start_byte() + 1, format!(" {BARRIER}")));
                    } else {
                        out.push(Edit::insert(body.start_byte(), format!("{{ {BARRIER} ")));
                        out.push(Edit::insert(body.end_byte(), " }"));
                    }
                }
            }
            "labeled_statement" => {
                if let (Some(l), Some(s)) = (n.child_by_field_name("label"), n.named_child(1)) {
                    labels.push((ast.text(l).to_string(), n.start_byte(), s.start_byte()));
                }
            }
            "goto_statement" => {
                if let Some(l) = n.child_by_field_name("label") {
                    gotos.push((ast.text(l).to_string(), n.start_byte()));
                }
            }
            _ => {}
        }
        let mut cursor = n.walk();
        for c in n.named_children(&mut cursor) {
            walk(ast, c, labels, gotos, out);
        }
    }
    let mut out = Vec::new();
    let root = ast.tree.root_node();
    let mut cursor = root.walk();
    for f in root.named_children(&mut cursor).filter(|n| n.kind() == "function_definition") {
        let (mut labels, mut gotos) = (Vec::new(), Vec::new());
        walk(ast, f, &mut labels, &mut gotos, &mut out);
        for (name, at, stmt) in labels {
            if gotos.iter().any(|(g, from)| *g == name && *from > at) {
                out.push(Edit::insert(stmt, format!("{BARRIER} ")));
            }
        }
    }
    out
}

/// Edits making every `/`, `%`, `/=` and `%=` in the TU's functions whose
/// divisor is not a literal call [`crate::DIVISIONS`] when clang finds it
/// a 32-bit integer division by a non-constant (D36). Division by zero is
/// undefined in C, so clang may sink a division into the branch that uses
/// it or drop an unused one, and the original's `break` on a zero divisor
/// moves or goes; a call it can neither move nor drop, and the link lowers
/// it to the R3000's division. `__builtin_choose_expr` keeps constant
/// divisors, floating-point and 64-bit divisions as they are, so constant
/// expressions stay constant. The operands appear several times (all but
/// one unevaluated), on one line; the lines they took follow, so the TU
/// keeps its line count. A compound assignment is rewritten only when its
/// left side is a plain variable: `E op= X` evaluates `E` once, while
/// `E = E op X` evaluates it twice.
pub fn divisions(ast: &Ast) -> Vec<crate::edit::Edit> {
    use crate::edit::Edit;
    /// No identifier outside `sizeof`: a literal, a cast of one, and so on.
    fn literal(n: Node) -> bool {
        match n.kind() {
            "identifier" | "call_expression" => false,
            "sizeof_expression" | "alignof_expression" | "type_descriptor" => true,
            _ => {
                let mut c = n.walk();
                n.named_children(&mut c).all(literal)
            }
        }
    }
    /// `n` as C with its divisions rewritten, on one line.
    fn text(ast: &Ast, n: Node) -> String {
        // An operand can itself be a division without parentheses (a/b/c).
        if let Some(t) = division(ast, n) {
            return t;
        }
        fn splice(ast: &Ast, n: Node, s: &mut String, pos: &mut usize) {
            let mut c = n.walk();
            for k in n.named_children(&mut c) {
                match division(ast, k) {
                    Some(t) => {
                        s.push_str(&ast.src[*pos..k.start_byte()]);
                        s.push_str(&t);
                        *pos = k.end_byte();
                    }
                    None => splice(ast, k, s, pos),
                }
            }
        }
        let (mut s, mut pos) = (String::new(), n.start_byte());
        splice(ast, n, &mut s, &mut pos);
        s.push_str(&ast.src[pos..n.end_byte()]);
        s.replace('\n', " ")
    }
    fn division(ast: &Ast, n: Node) -> Option<String> {
        let compound = n.kind() == "assignment_expression";
        if !compound && n.kind() != "binary_expression" {
            return None;
        }
        let (l, r) = (n.child_by_field_name("left")?, n.child_by_field_name("right")?);
        let op = ast.text(n.child_by_field_name("operator")?).trim_end_matches('=');
        let name = match op {
            "/" => "div",
            "%" => "rem",
            _ => return None,
        };
        if literal(r) || (compound && l.kind() != "identifier") {
            return None;
        }
        let (a, b) = (text(ast, l), text(ast, r));
        let both = format!("({a}) + ({b})");
        let call = format!(
            "__builtin_choose_expr(__builtin_constant_p({b}) || !_Generic({both}, int: 1, long: 1, unsigned int: 1, unsigned long: 1, default: 0), ({a}) {op} ({b}), (__typeof__({both}))__builtin_choose_expr(_Generic({both}, unsigned int: 1, unsigned long: 1, default: 0), __psxw_{name}_u(({a}), ({b})), __psxw_{name}_s(({a}), ({b}))))"
        );
        Some(if compound { format!("{a} = {call}") } else { call })
    }
    fn walk(ast: &Ast, n: Node, out: &mut Vec<Edit>) {
        if let Some(t) = division(ast, n) {
            let lines = ast.text(n).matches('\n').count();
            out.push(Edit::replace(n.byte_range(), t + &"\n".repeat(lines)));
            return;
        }
        let mut c = n.walk();
        for k in n.named_children(&mut c) {
            walk(ast, k, out);
        }
    }
    let mut out = Vec::new();
    let root = ast.tree.root_node();
    let mut cursor = root.walk();
    for f in root.named_children(&mut cursor).filter(|n| n.kind() == "function_definition") {
        if let Some(body) = f.child_by_field_name("body") {
            walk(ast, body, &mut out);
        }
    }
    out
}

/// The hooked variant of a normalized TU (plans/M11): each function in
/// `names` it defines becomes `weak` and `noinline`, so clang neither
/// inlines its calls nor uses its body to fold or delete them, the way
/// GCC 2.95 at `-O2` left every call to a non-inline function. A `static`
/// one loses `static` (a weak definition can't be internal) on every
/// file-scope declaration. `inline` functions are left alone: GCC inlined
/// them too, so the original program has no such calls. A function in
/// `boundaries` (a declared binding operation, plans/dispatch-binding)
/// keeps its call boundary even when written `inline`: it loses `inline`
/// and becomes weak and noinline like the rest, so every call reaches its
/// instrumentation.
pub fn hook_variant(text: &str, names: &std::collections::BTreeSet<String>, boundaries: &std::collections::BTreeSet<String>) -> String {
    let ast = parse(text);
    let mut edits: Vec<crate::edit::Edit> = Vec::new();
    let words = |s: &str| -> Vec<String> { s.split(|c: char| !c.is_alphanumeric() && c != '_').map(str::to_string).collect() };
    let mut made_extern: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let is_inline = |s: &str| matches!(s, "inline" | "__inline" | "__inline__");
    for f in ast.functions() {
        let boundary = boundaries.contains(&f.name);
        if !names.contains(&f.name) && !boundary {
            continue;
        }
        let head = &ast.src[f.range.start..f.decl_end];
        let specifiers = head.split(&f.name).next().unwrap_or("");
        let w = words(specifiers);
        let node = ast.top_level_at(f.range.start).filter(|n| n.kind() == "function_definition");
        // A boundary's `inline` specifiers go; the first takes the attribute
        // when there's no `static` to.
        let mut inline_specs = Vec::new();
        if w.iter().any(|w| is_inline(w)) {
            if !boundary {
                continue;
            }
            let mut c = node.map(|n| n.walk());
            inline_specs = node
                .zip(c.as_mut())
                .into_iter()
                .flat_map(|(n, c)| n.named_children(c).collect::<Vec<_>>())
                .filter(|s| s.kind() == "storage_class_specifier" && is_inline(ast.text(*s)))
                .map(|s| s.byte_range())
                .collect();
        }
        const ATTR: &str = "__attribute__((weak, noinline))";
        let mut c2 = node.map(|n| n.walk());
        let stat = node.zip(c2.as_mut()).and_then(|(n, c)| {
            n.named_children(c).find(|s| s.kind() == "storage_class_specifier" && ast.text(*s) == "static")
        });
        let mut inline_specs = inline_specs.into_iter();
        match stat {
            Some(st) => {
                made_extern.insert(f.name.clone());
                edits.push(crate::edit::Edit::replace(st.byte_range(), ATTR));
            }
            None => match inline_specs.next() {
                Some(r) => edits.push(crate::edit::Edit::replace(r, ATTR)),
                None => edits.push(crate::edit::Edit::insert(f.range.start, format!("{ATTR} "))),
            },
        }
        edits.extend(inline_specs.map(|r| crate::edit::Edit::replace(r, "")));
    }
    // `static` off every file-scope declaration of those functions.
    let root = ast.tree.root_node();
    let mut cursor = root.walk();
    for node in root.named_children(&mut cursor) {
        if node.kind() != "declaration" {
            continue;
        }
        let Some(d) = node.child_by_field_name("declarator") else { continue };
        let Some(name) = ast.declarator_name(d) else { continue };
        if !made_extern.contains(&name) {
            continue;
        }
        let mut c2 = node.walk();
        for spec in node.named_children(&mut c2) {
            if spec.kind() == "storage_class_specifier" && ast.text(spec) == "static" {
                edits.push(crate::edit::Edit::replace(spec.byte_range(), ""));
            }
        }
    }
    crate::edit::apply(text, edits)
}

#[cfg(test)]
mod hook_tests {
    #[test]
    fn hooked_functions_become_weak() {
        let src = "static int g(int);\nint f(int a) { return g(a) + 1; }\nstatic int g(int a) { return a * 2; }\nstatic inline int h(int a) { return a; }\n";
        let names = ["f", "g", "h"].iter().map(|s| s.to_string()).collect();
        let out = super::hook_variant(src, &names, &Default::default());
        assert_eq!(
            out,
            " int g(int);\n__attribute__((weak, noinline)) int f(int a) { return g(a) + 1; }\n__attribute__((weak, noinline)) int g(int a) { return a * 2; }\nstatic inline int h(int a) { return a; }\n"
        );
    }

    #[test]
    fn declared_operations_keep_their_boundary() {
        let src = "static inline int h(int a) { return a; }\ninline int k(int a) { return a; }\nint f(int a) { return h(a) + k(a); }\n";
        let names = ["f"].iter().map(|s| s.to_string()).collect();
        let ops = ["h", "k"].iter().map(|s| s.to_string()).collect();
        let out = super::hook_variant(src, &names, &ops);
        assert!(!out.contains("inline int"), "{out}");
        assert!(out.contains("__attribute__((weak, noinline))  int h(int a)"), "{out}");
        assert!(out.contains("__attribute__((weak, noinline)) int k(int a)"), "{out}");
    }
}
