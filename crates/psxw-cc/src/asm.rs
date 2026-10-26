//! Classify every `asm`/`__asm__` in preprocessed C and rewrite the forms
//! that have a host meaning. Real MIPS instructions can't: their offsets are
//! returned so the enclosing function can be handed to the lift tier.

use crate::edit::Edit;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsmKind {
    /// `register T x asm("$11")`: hard-register pinning, meaningless on wasm.
    RegisterBinding,
    /// `extern T x asm("name")`: a symbol rename; clang supports it.
    Label,
    /// File-scope `asm("...")`: assembler directives (`.include`).
    TopLevel,
    /// An empty template: a scheduling/compiler barrier.
    Barrier,
    /// `break n` with its code expression.
    Break(String),
    /// `nop`: scheduling only.
    Nop,
    /// Anything else: real MIPS instructions.
    Real,
}

#[derive(Debug, Clone)]
pub struct AsmSite {
    pub start: usize,
    pub end: usize,
    pub kind: AsmKind,
    pub template: String,
    /// Brace depth; 0 is file scope.
    pub depth: i32,
}

const BARRIER: &str = "__asm__ volatile(\"\" ::: \"memory\")";

/// splat-style `INCLUDE_ASM(dir, f)` expands, in every variant, to file-scope
/// asm that `.include`s `<dir>/<f>.s` into a text section. Returns `f`.
/// Includes into `.rodata` (`INCLUDE_RODATA`) are data, not functions.
pub fn included_function(template: &str) -> Option<String> {
    let t = template.replace("\\\"", "\"").replace("\\n", "\n");
    if t.contains(".rodata") || t.contains(".rdata") {
        return None;
    }
    let i = t.find(".include")?;
    let rest = &t[i + ".include".len()..];
    let q = rest.find('"')?;
    let path = &rest[q + 1..q + 1 + rest[q + 1..].find('"')?];
    let file = path.rsplit('/').next()?;
    file.strip_suffix(".s").map(str::to_string)
}

/// Rule names counted by the census.
pub fn rule(site: &AsmSite) -> Option<&'static str> {
    match &site.kind {
        AsmKind::RegisterBinding if site.depth <= 0 => Some("asm-global-register"),
        AsmKind::RegisterBinding => Some("asm-register-binding"),
        AsmKind::TopLevel if included_function(&site.template).is_some() => Some("asm-include-asm"),
        AsmKind::TopLevel => Some("asm-toplevel"),
        AsmKind::Barrier => Some("asm-barrier"),
        AsmKind::Break(_) => Some("asm-break"),
        AsmKind::Nop => Some("asm-nop"),
        AsmKind::Label | AsmKind::Real => None,
    }
}

/// Edits for every rewritable site; `Real` sites get no edit.
pub fn edits(text: &str, sites: &[AsmSite]) -> Vec<Edit> {
    let mut out = Vec::new();
    for s in sites {
        match &s.kind {
            AsmKind::RegisterBinding => {
                out.push(Edit::replace(s.start..s.end, ""));
                // A file-scope `register` variable is a GCC global register
                // variable; without its register it is a TU-local variable.
                if s.depth <= 0 {
                    let stmt = text[..s.start].rfind([';', '}']).map_or(0, |i| i + 1);
                    if let Some(i) = find_word(&text[stmt..s.start], "register") {
                        out.push(Edit::replace(stmt + i..stmt + i + "register".len(), "static"));
                    }
                }
            }
            AsmKind::TopLevel => {
                let rest = &text[s.end..];
                let semi = rest.len() - rest.trim_start().len();
                let end = if rest[semi..].starts_with(';') { s.end + semi + 1 } else { s.end };
                let replacement = match included_function(&s.template) {
                    Some(name) => format!(
                        "__attribute__((used)) static const char {}{name}[] = \"include_asm\";",
                        crate::LIFT_MARKER_PREFIX
                    ),
                    None => String::new(),
                };
                out.push(Edit::replace(s.start..end, replacement));
            }
            AsmKind::Barrier | AsmKind::Nop => {
                if text[s.start..s.end] != *BARRIER {
                    out.push(Edit::replace(s.start..s.end, BARRIER));
                }
            }
            AsmKind::Break(expr) => out.push(Edit::replace(s.start..s.end, format!("__psxw_break({expr})"))),
            AsmKind::Label | AsmKind::Real => {}
        }
    }
    out
}

pub fn scan(text: &str) -> Vec<AsmSite> {
    let b = text.as_bytes();
    let mut sites = Vec::new();
    let mut depth = 0i32;
    let mut i = 0;
    let mut line_start = true;
    while i < b.len() {
        let c = b[i];
        if line_start && c == b'#' {
            i = skip_line(b, i);
            continue;
        }
        line_start = c == b'\n';
        match c {
            b'"' | b'\'' => i = skip_literal(b, i),
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                i += 1;
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                if matches!(&text[start..i], "asm" | "__asm" | "__asm__") {
                    if let Some(site) = site_at(text, start, i, depth) {
                        i = site.end;
                        sites.push(site);
                    }
                }
            }
            _ => i += 1,
        }
    }
    sites
}

fn site_at(text: &str, start: usize, after_kw: usize, depth: i32) -> Option<AsmSite> {
    let b = text.as_bytes();
    let mut j = after_kw;
    loop {
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        let w = j;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
            j += 1;
        }
        if w == j {
            break;
        }
        if !matches!(&text[w..j], "volatile" | "__volatile__" | "__volatile" | "goto" | "inline" | "__inline__") {
            return None;
        }
    }
    if b.get(j) != Some(&b'(') {
        return None;
    }
    let close = matching_paren(b, j)?;
    let content = &text[j + 1..close];
    let (template, rest) = leading_strings(content);
    let end = close + 1;

    let prev = text[..start].trim_end().bytes().last().unwrap_or(b';');
    let after_declarator = prev == b')' || prev == b']' || prev.is_ascii_alphanumeric() || prev == b'_';
    let t = template.trim();
    let kind = if rest.trim().is_empty() && after_declarator && depth >= 0 && !t.is_empty() && is_label(t) {
        if t.starts_with('$') { AsmKind::RegisterBinding } else { AsmKind::Label }
    } else if depth <= 0 {
        AsmKind::TopLevel
    } else if t.is_empty() {
        AsmKind::Barrier
    } else if t == "nop" {
        AsmKind::Nop
    } else if t.starts_with("break") {
        match immediate_operand(rest) {
            Some(expr) => AsmKind::Break(expr),
            None => AsmKind::Real,
        }
    } else {
        AsmKind::Real
    };
    Some(AsmSite { start, end, kind, template, depth })
}

/// Byte index of `word` as a whole identifier in `s`.
fn find_word(s: &str, word: &str) -> Option<usize> {
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    s.match_indices(word).map(|(i, _)| i).find(|&i| {
        let before = i == 0 || !ident(s.as_bytes()[i - 1]);
        let after = s.as_bytes().get(i + word.len()).is_none_or(|&c| !ident(c));
        before && after
    })
}

/// `(local name, label)` for each `auto T name(...) __asm__("label");` in a
/// function body: the original symbol a nested function is bound to.
pub fn auto_labels(body: &str) -> Vec<(String, String)> {
    scan(body)
        .into_iter()
        .filter(|s| s.kind == AsmKind::Label)
        .filter_map(|s| {
            let stmt = body[..s.start].rfind([';', '{', '}']).map_or(0, |i| i + 1);
            let decl = &body[stmt..s.start];
            find_word(decl, "auto")?;
            let head = decl[..decl.find('(')?].trim_end();
            let name = head.rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).next()?;
            Some((name.to_string(), s.template))
        })
        .collect()
}

/// A label is a symbol (possibly `sym+offset`, which GNU as resolves) or a
/// register name, never an instruction.
fn is_label(t: &str) -> bool {
    t.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'$' | b'.' | b'+' | b'-'))
}

/// The expression of the first `"n"(expr)` input operand.
fn immediate_operand(rest: &str) -> Option<String> {
    let i = rest.find("\"n\"")?;
    let b = rest.as_bytes();
    let mut j = i + 3;
    while j < b.len() && b[j].is_ascii_whitespace() {
        j += 1;
    }
    if b.get(j) != Some(&b'(') {
        return None;
    }
    let close = matching_paren(b, j)?;
    Some(rest[j + 1..close].trim().to_string())
}

/// Concatenated contents of the leading string literals, and the remainder.
fn leading_strings(content: &str) -> (String, &str) {
    let b = content.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if b.get(i) != Some(&b'"') {
            break;
        }
        let end = skip_literal(b, i);
        out.push_str(&content[i + 1..end - 1]);
        i = end;
    }
    (out, &content[i..])
}

fn matching_paren(b: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => {
                i = skip_literal(b, i);
                continue;
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Index just past a string or char literal starting at `i`.
fn skip_literal(b: &[u8], i: usize) -> usize {
    let q = b[i];
    let mut j = i + 1;
    while j < b.len() && b[j] != q {
        if b[j] == b'\\' {
            j += 1;
        }
        if b[j] == b'\n' {
            break;
        }
        j += 1;
    }
    (j + 1).min(b.len())
}

fn skip_line(b: &[u8], i: usize) -> usize {
    b[i..].iter().position(|&c| c == b'\n').map_or(b.len(), |p| i + p + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<AsmKind> {
        scan(src).into_iter().map(|s| s.kind).collect()
    }

    #[test]
    fn classifies() {
        assert_eq!(kinds("void f(void) { register int x __asm__(\"$11\"); }"), vec![AsmKind::RegisterBinding]);
        assert_eq!(kinds("extern int _D[3] __asm__(\"D_800B7E20\");"), vec![AsmKind::Label]);
        assert_eq!(kinds("extern short D_80076226[] __asm__(\"D_80076224+2\");"), vec![AsmKind::Label]);
        assert_eq!(kinds("__asm__(\".include \\\"include/macro.inc\\\"\\n\");"), vec![AsmKind::TopLevel]);
        assert_eq!(kinds("void f(void) { __asm__ volatile(\"\" ::: \"memory\"); }"), vec![AsmKind::Barrier]);
        assert_eq!(
            kinds("void f(void) { __asm__ volatile(\"break %0\" :: \"n\"((7) * 1024)); }"),
            vec![AsmKind::Break("(7) * 1024".into())]
        );
        assert_eq!(kinds("void f(void) { int x; __asm__ volatile(\"lw %0,32($sp)\" : \"=r\"(x)); }"), vec![AsmKind::Real]);
    }

    #[test]
    fn rewrites() {
        let src = "__asm__(\".include \\\"x\\\"\");\nvoid f(void) { register int x asm(\"$2\"); asm volatile(\"break %0\" :: \"n\"(5)); }";
        let out = crate::edit::apply(src, edits(src, &scan(src)));
        assert_eq!(out, "\nvoid f(void) { register int x ; __psxw_break(5); }");

        let global = "int a;\nregister int prev asm(\"$2\");\n";
        assert_eq!(crate::edit::apply(global, edits(global, &scan(global))), "int a;\nstatic int prev ;\n");
    }

    #[test]
    fn include_asm_any_variant() {
        let btn = "__asm__( \".section .text\\n\" \"    .set noat\\n\" \"    .include \\\"\" \"build/asm/nonmatchings/func_80017C4C\" \"/\" \"func_80017C4C\" \".s\\\"\\n\" \"    .set at\\n\" );";
        let out = crate::edit::apply(btn, edits(btn, &scan(btn)));
        assert_eq!(out, "__attribute__((used)) static const char __psxw_lift_func_80017C4C[] = \"include_asm\";");
        let rodata = "__asm__(\".section .rodata\\n.include \\\"asm/data/D_1.s\\\"\\n.section .text\");";
        assert_eq!(crate::edit::apply(rodata, edits(rodata, &scan(rodata))), "");
    }

    #[test]
    fn nested_labels() {
        let body = "{ auto int check(int i) __asm__(\"Ovl11TestAreaFlagGuard\"); int x __asm__(\"$2\"); }";
        assert_eq!(auto_labels(body), vec![("check".to_string(), "Ovl11TestAreaFlagGuard".to_string())]);
    }
}
