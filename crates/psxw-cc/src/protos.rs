//! Prototype facts: for every function a TU defines or calls, how clang
//! lowers it to wasm (parameter kinds, structs by value and their sizes,
//! struct returns, small-integer extension, variadics). The linker needs
//! these to build o32 adapters, because a wasm signature alone can't tell
//! a struct passed by value from a pointer.
//!
//! The facts are read from clang's own LLVM IR for the normalized TU
//! (`-S -emit-llvm`), so they are exactly what the object was compiled
//! with: `byval(%struct.S)`, `sret(...)`, `signext`/`zeroext`, `...`.

use crate::clang::Clang;
use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Kind {
    Void,
    /// 8/16-bit integers, extended to 32 bits as the attribute says.
    I8 { signed: bool },
    I16 { signed: bool },
    I32,
    I64,
    F32,
    F64,
    Ptr,
    /// A struct or union passed by value (wasm: pointer to a copy).
    ByVal { size: u32, align: u32 },
    /// Hidden struct-return pointer (first parameter).
    Sret { size: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proto {
    pub name: String,
    /// Defined in this TU (otherwise only declared or called).
    pub defined: bool,
    pub ret: Kind,
    pub params: Vec<Kind>,
    pub variadic: bool,
}

impl Proto {
    /// o32 argument words the fixed parameters occupy (64-bit values
    /// aligned to an even word, structs by value rounded up to words).
    pub fn o32_words(&self) -> u32 {
        let mut w = 0;
        for p in &self.params {
            match p {
                Kind::I64 | Kind::F64 => w = (w + 1) / 2 * 2 + 2,
                Kind::ByVal { size, align } => {
                    if *align >= 8 {
                        w = (w + 1) / 2 * 2;
                    }
                    w += size.div_ceil(4);
                }
                Kind::Void => {}
                _ => w += 1,
            }
        }
        w
    }
}

/// Prototype facts of one normalized TU (`.i` text as the census compiled
/// it).
pub fn for_tu(clang: &Clang, i_path: &Path) -> Result<Vec<Proto>> {
    let ir = clang.emit_llvm(i_path).with_context(|| format!("emitting IR for {}", i_path.display()))?;
    parse_ir(&ir)
}

/// Cached next to the TU's object as `<stem>.protos.json`.
pub fn cached(clang: &Clang, i_path: &Path) -> Result<Vec<Proto>> {
    let json = i_path.with_extension("protos.json");
    if let (Ok(j), Ok(i)) = (std::fs::metadata(&json), std::fs::metadata(i_path)) {
        if j.modified()? >= i.modified()? {
            let text = std::fs::read_to_string(&json)?;
            return Ok(serde_json::from_str(&text)?);
        }
    }
    let protos = for_tu(clang, i_path)?;
    std::fs::write(&json, serde_json::to_string(&protos)?)?;
    Ok(protos)
}

pub fn parse_ir(ir: &str) -> Result<Vec<Proto>> {
    let mut types: BTreeMap<String, String> = BTreeMap::new();
    for line in ir.lines() {
        if let Some((name, def)) = line.split_once(" = type ") {
            if name.starts_with('%') {
                types.insert(name.to_string(), def.trim().to_string());
            }
        }
    }
    let layout = Layout { types: &types };
    let mut out = Vec::new();
    for line in ir.lines() {
        let defined = line.starts_with("define ");
        if !defined && !line.starts_with("declare ") {
            continue;
        }
        let Some(at) = line.find(" @") else { continue };
        let head = &line[..at];
        let rest = &line[at + 2..];
        let (name, rest) = if let Some(q) = rest.strip_prefix('"') {
            let end = q.find('"').context("unterminated quoted name")?;
            (q[..end].to_string(), &q[end + 1..])
        } else {
            let end = rest.find('(').context("no parameter list")?;
            (rest[..end].to_string(), &rest[end..])
        };
        if name.starts_with("llvm.") {
            continue;
        }
        let open = rest.find('(').context("no parameter list")?;
        let close = matching(rest, open).context("unbalanced parameter list")?;
        let params_text = &rest[open + 1..close];
        let ret = ret_kind(head, &layout)?;
        let mut params = Vec::new();
        let mut variadic = false;
        for p in split_top(params_text) {
            let p = p.trim();
            if p.is_empty() {
                continue;
            }
            if p == "..." {
                variadic = true;
                continue;
            }
            params.push(param_kind(p, &layout)?);
        }
        out.push(Proto { name, defined, ret, params, variadic });
    }
    Ok(out)
}

fn matching(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in s.char_indices().skip_while(|&(i, _)| i < open) {
        match c {
            '(' | '{' | '[' | '<' => depth += 1,
            ')' | '}' | ']' | '>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_top(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '{' | '[' | '<' => depth += 1,
            ')' | '}' | ']' | '>' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// Extract the type named inside an attribute like `byval(%struct.S)`.
fn attr_type<'a>(p: &'a str, attr: &str) -> Option<&'a str> {
    let i = p.find(&format!("{attr}("))?;
    let open = i + attr.len();
    let close = matching(p, open)?;
    Some(&p[open + 1..close])
}

fn param_kind(p: &str, layout: &Layout) -> Result<Kind> {
    if let Some(t) = attr_type(p, "byval") {
        let (size, align) = layout.size_align(t)?;
        return Ok(Kind::ByVal { size, align });
    }
    if let Some(t) = attr_type(p, "sret") {
        return Ok(Kind::Sret { size: layout.size_align(t)?.0 });
    }
    let ty = p.split_whitespace().next().unwrap_or_default();
    scalar(ty, p)
}

fn ret_kind(head: &str, layout: &Layout) -> Result<Kind> {
    // The return type is the last token before the name; attributes like
    // `signext` precede it.
    let tokens: Vec<&str> = head.split_whitespace().collect();
    let ty = tokens.last().copied().unwrap_or("void");
    if ty.starts_with('%') || ty.starts_with('{') {
        // Small structs returned directly don't occur with wasm32's ABI;
        // report them by size so callers notice.
        let (size, _) = layout.size_align(ty)?;
        return Ok(Kind::Sret { size });
    }
    scalar(ty, head)
}

fn scalar(ty: &str, context: &str) -> Result<Kind> {
    let signed = !context.contains("zeroext");
    Ok(match ty {
        "void" => Kind::Void,
        "i1" | "i8" => Kind::I8 { signed },
        "i16" => Kind::I16 { signed },
        "i32" => Kind::I32,
        "i64" => Kind::I64,
        "float" => Kind::F32,
        "double" => Kind::F64,
        "ptr" => Kind::Ptr,
        other => bail!("unhandled IR type {other} in `{context}`"),
    })
}

struct Layout<'a> {
    types: &'a BTreeMap<String, String>,
}

impl Layout<'_> {
    /// Size and alignment of an IR type under wasm32's data layout.
    fn size_align(&self, ty: &str) -> Result<(u32, u32)> {
        let ty = ty.trim();
        Ok(match ty {
            "i1" | "i8" => (1, 1),
            "i16" => (2, 2),
            "i32" | "float" | "ptr" => (4, 4),
            "i64" | "double" => (8, 8),
            _ if ty.starts_with('%') => {
                let def = self.types.get(ty).with_context(|| format!("unknown type {ty}"))?;
                if def == "opaque" {
                    bail!("opaque type {ty} passed by value");
                }
                self.size_align(def)?
            }
            _ if ty.starts_with("<{") => {
                let inner = &ty[2..ty.len() - 2];
                let mut size = 0;
                for f in split_top(inner).into_iter().filter(|f| !f.trim().is_empty()) {
                    size += self.size_align(f)?.0;
                }
                (size, 1)
            }
            _ if ty.starts_with('{') => {
                let inner = &ty[1..ty.len() - 1];
                let (mut size, mut align) = (0u32, 1u32);
                for f in split_top(inner).into_iter().filter(|f| !f.trim().is_empty()) {
                    let (s, a) = self.size_align(f)?;
                    size = size.div_ceil(a) * a + s;
                    align = align.max(a);
                }
                (size.div_ceil(align) * align, align)
            }
            _ if ty.starts_with('[') => {
                let inner = &ty[1..ty.len() - 1];
                let (n, elem) = inner.split_once(" x ").with_context(|| format!("bad array type {ty}"))?;
                let n: u32 = n.trim().parse()?;
                let (s, a) = self.size_align(elem)?;
                (n * s, a)
            }
            other => bail!("unhandled IR type {other}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_signatures() {
        let ir = r#"
%struct.S = type { i16, i16, i32 }
%struct.P = type <{ i8, i32 }>
%union.U = type { [3 x i32] }
define dso_local signext i16 @f(ptr noundef byval(%struct.S) align 4 %s, i32 noundef %n) #0 {
declare void @g(ptr dead_on_unwind writable sret(%union.U) align 4, i8 noundef zeroext) #1
declare i32 @FntPrint(ptr noundef, ...) #1
declare double @"quoted name"(i64 noundef, double noundef, ptr byval(%struct.P) align 1)
"#;
        let p = parse_ir(ir).unwrap();
        assert_eq!(p[0].name, "f");
        assert!(p[0].defined);
        assert_eq!(p[0].ret, Kind::I16 { signed: true });
        assert_eq!(p[0].params, vec![Kind::ByVal { size: 8, align: 4 }, Kind::I32]);
        assert_eq!(p[0].o32_words(), 3);
        assert_eq!(p[1].params, vec![Kind::Sret { size: 12 }, Kind::I8 { signed: false }]);
        assert!(p[2].variadic);
        assert_eq!(p[3].name, "quoted name");
        assert_eq!(p[3].params[2], Kind::ByVal { size: 5, align: 1 });
        assert_eq!(p[3].o32_words(), 6);
    }
}
