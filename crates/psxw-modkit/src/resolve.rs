//! Declarations' targets, resolved against the [`SymbolTable`]: a function
//! (and, for a point, the boundary it stands for) or a data location.

use crate::decl::{Kind, Target, TargetForm};
use crate::symbols::{FuncInfo, SymbolTable};
use anyhow::{Result, bail};

/// A resolved function target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Func {
    pub container: String,
    pub addr: u32,
    /// For a `{point}`: its name and the kind of boundary it is.
    pub point: Option<(String, Kind)>,
}

/// The address `<prefix>_<8 hex digits>` names, splat's auto-naming.
fn auto_addr(name: &str) -> Option<u32> {
    let (prefix, hex) = name.rsplit_once('_')?;
    if prefix.is_empty() || hex.len() != 8 {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

/// Up to five names close to `want`, to suggest.
fn near<'a>(names: impl Iterator<Item = &'a str>, want: &str) -> String {
    let w = want.to_ascii_lowercase();
    let mut hits: Vec<&str> = names.filter(|n| n.to_ascii_lowercase().contains(&w)).collect();
    hits.sort();
    hits.dedup();
    hits.truncate(5);
    if hits.is_empty() { String::new() } else { format!(" (did you mean {}?)", hits.join(", ")) }
}

fn one<'a>(t: &SymbolTable, what: &str, mut found: Vec<&'a FuncInfo>) -> Result<&'a FuncInfo> {
    found.sort_by(|a, b| (&a.container, a.addr).cmp(&(&b.container, b.addr)));
    found.dedup_by(|a, b| a.container == b.container && a.addr == b.addr);
    match found.as_slice() {
        [f] => Ok(f),
        [] => bail!("no function {what}{}", near(t.functions.iter().flat_map(|f| f.names.iter().map(String::as_str)), what)),
        many => bail!(
            "{what} names functions in {}: give {{container, symbol}}",
            many.iter().map(|f| format!("{} (0x{:08X})", f.container, f.addr)).collect::<Vec<_>>().join(", ")
        ),
    }
}

impl SymbolTable {
    /// The function a target names.
    pub fn resolve_func(&self, target: &Target) -> Result<Func> {
        Ok(match target.form()? {
            TargetForm::Symbol(s) => {
                let named: Vec<&FuncInfo> = self.functions.iter().filter(|f| f.names.contains(&s)).collect();
                let f = if named.is_empty() {
                    match auto_addr(&s) {
                        Some(a) => one(self, &s, self.functions.iter().filter(|f| f.addr == a).collect())?,
                        None => {
                            if self.data.iter().any(|d| d.name == s) {
                                bail!("{s} is data, not a function");
                            }
                            one(self, &s, named)?
                        }
                    }
                } else {
                    one(self, &s, named)?
                };
                Func { container: f.container.clone(), addr: f.addr, point: None }
            }
            TargetForm::InContainer(c, s) => {
                if self.container(&c).is_none() {
                    bail!("no container {c}");
                }
                let own: Vec<&FuncInfo> = self.functions.iter().filter(|f| f.container == c && f.names.contains(&s)).collect();
                let f = if !own.is_empty() {
                    one(self, &s, own)?
                } else if let Some(a) = auto_addr(&s).filter(|a| self.function(&c, *a).is_some()) {
                    self.function(&c, a).expect("checked")
                } else {
                    // The executable's functions are visible from every overlay.
                    let main: Vec<&FuncInfo> = self.functions.iter().filter(|f| f.container == self.main && f.names.contains(&s)).collect();
                    one(self, &format!("{c}:{s}"), main)?
                };
                Func { container: f.container.clone(), addr: f.addr, point: None }
            }
            TargetForm::Address(c, a) => {
                if self.container(&c).is_none() {
                    bail!("no container {c}");
                }
                if self.function(&c, a).is_none() {
                    let inside = self.functions.iter().filter(|f| f.container == c && f.addr < a).max_by_key(|f| f.addr);
                    match inside {
                        Some(f) if self.in_text(&c, a) => {
                            bail!("no function starts at 0x{a:08X} in {c}; it is inside {} (0x{:08X}), and a jump into a function isn't a call", f.name(), f.addr)
                        }
                        _ => bail!("no function at 0x{a:08X} in {c}"),
                    }
                }
                Func { container: c, addr: a, point: None }
            }
            TargetForm::Point(p) => {
                let Some(spec) = self.points.get(&p) else {
                    let known: Vec<&str> = self.points.keys().map(String::as_str).collect();
                    bail!("no hook point {p} (the game's manifest names: {})", if known.is_empty() { "none".into() } else { known.join(", ") });
                };
                let (kind, sym) = match (&spec.before, &spec.after) {
                    (Some(s), None) => (Kind::Before, s),
                    (None, Some(s)) => (Kind::After, s),
                    _ => bail!("hook point {p} must be before or after one function"),
                };
                let f = self.resolve_func(&Target { symbol: Some(sym.clone()), ..Default::default() })?;
                Func { point: Some((p, kind)), ..f }
            }
        })
    }

    /// The container and address a data target names (offset not added).
    pub fn resolve_data(&self, target: &Target) -> Result<(String, u32, Option<u32>, String)> {
        let by_addr = |a: u32, what: &str| -> Result<String> {
            let holders: Vec<&str> = self
                .containers
                .iter()
                .filter(|c| (c.loaded[0]..c.loaded[1]).contains(&a) || c.bss.is_some_and(|b| (b[0]..b[1]).contains(&a)))
                .map(|c| c.id.as_str())
                .collect();
            if holders.contains(&self.main.as_str()) {
                return Ok(self.main.clone());
            }
            match holders.as_slice() {
                [c] => Ok(c.to_string()),
                [] => bail!("{what} (0x{a:08X}) is in no container"),
                many => bail!("{what} (0x{a:08X}) is in {}: give {{container, address}}", many.join(", ")),
            }
        };
        Ok(match target.form()? {
            TargetForm::Symbol(s) => {
                let named: Vec<_> = self.data.iter().filter(|d| d.name == s).collect();
                match named.as_slice() {
                    [d] => (d.container.clone(), d.addr, d.size, s),
                    [] => {
                        if self.functions.iter().any(|f| f.names.contains(&s)) {
                            bail!("{s} is a function; a data patch writes data");
                        }
                        match auto_addr(&s) {
                            Some(a) => (by_addr(a, &s)?, a, None, s),
                            None => bail!("no data symbol {s}{}", near(self.data.iter().map(|d| d.name.as_str()), &s)),
                        }
                    }
                    many => bail!(
                        "{s} names data in {}: give {{container, symbol}}",
                        many.iter().map(|d| d.container.as_str()).collect::<Vec<_>>().join(", ")
                    ),
                }
            }
            TargetForm::InContainer(c, s) => {
                let d = self.data.iter().find(|d| d.container == c && d.name == s).or_else(|| self.data.iter().find(|d| d.container == self.main && d.name == s));
                match (d, auto_addr(&s)) {
                    (Some(d), _) => (d.container.clone(), d.addr, d.size, s),
                    (None, Some(a)) => (c, a, None, s),
                    (None, None) => bail!("no data symbol {s} in {c}"),
                }
            }
            TargetForm::Address(c, a) => {
                if self.container(&c).is_none() {
                    bail!("no container {c}");
                }
                (c.clone(), a, None, format!("{c}:0x{a:08X}"))
            }
            TargetForm::Point(_) => bail!("a data patch names data, not a point"),
        })
    }
}
