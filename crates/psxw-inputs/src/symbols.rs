//! splat `symbol_addrs` files: `name = 0xADDR; // key:value ...`.

#[derive(Debug, Clone)]
pub struct SymEntry {
    pub name: String,
    pub addr: u32,
    pub size: Option<u32>,
    /// `type:` attribute (`func`, `label`, `s32`, ...).
    pub ty: Option<String>,
}

impl SymEntry {
    pub fn is_func(&self) -> bool {
        self.ty.as_deref() == Some("func")
    }
}

pub fn parse(text: &str) -> Vec<SymEntry> {
    text.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<SymEntry> {
    let line = line.trim();
    if line.is_empty() || line.starts_with("//") {
        return None;
    }
    let (decl, attrs) = match line.split_once("//") {
        Some((d, a)) => (d, a),
        None => (line, ""),
    };
    let (name, value) = decl.split_once('=')?;
    let addr = crate::parse_u32(value.trim().trim_end_matches(';'))?;
    let mut entry = SymEntry { name: name.trim().to_string(), addr, size: None, ty: None };
    for attr in attrs.split_whitespace() {
        match attr.split_once(':') {
            Some(("size", v)) => entry.size = crate::parse_u32(v),
            Some(("type", v)) => entry.ty = Some(v.to_string()),
            _ => {}
        }
    }
    Some(entry)
}
