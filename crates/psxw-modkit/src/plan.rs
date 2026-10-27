//! What a set of mods does to a game, from their declarations alone
//! (plans/M11 "Static checks"): the load order, every hooked function's
//! chains, the data patches, the conflicts (warnings, the last applied
//! wins) and each mod's own defects (errors).

use crate::decl::{DataValue, Impl, Kind, Mod, Signature};
use crate::headers::{Decl, DeclParam};
use crate::resolve::Func;
use crate::symbols::{SymbolTable, TierName};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// A hook implementation, as a layer uses it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HookUse {
    pub imp: Impl,
    /// Called with no arguments: a named point's hook.
    pub bare: bool,
    /// Index into its mod's `hooks`.
    pub hook: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PatchUse {
    pub imp: Impl,
    pub original: Option<String>,
    pub hook: usize,
}

/// One mod's hooks on one function, in one scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LayerPlan {
    /// Index into the mods passed to [`plan`].
    pub module: usize,
    pub befores: Vec<HookUse>,
    pub patch: Option<PatchUse>,
    pub afters: Vec<HookUse>,
}

impl LayerPlan {
    /// Its kinds, as the chain table shows them.
    pub fn kinds(&self) -> String {
        let mut k = Vec::new();
        if !self.befores.is_empty() {
            k.push("before".to_string());
        }
        match &self.patch {
            Some(p) if p.original.is_some() => k.push("patch, wrapping".into()),
            Some(_) => k.push("patch".into()),
            None => {}
        }
        if !self.afters.is_empty() {
            k.push("after".into());
        }
        k.join(", ")
    }

    fn replaces(&self) -> bool {
        self.patch.as_ref().is_some_and(|p| p.original.is_none())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TargetPlan {
    pub container: String,
    pub addr: u32,
    pub name: String,
    pub tier: TierName,
    /// The function returns a value.
    pub returns: bool,
    /// Innermost (first applied) first.
    pub default: Vec<LayerPlan>,
    /// By caller: its name and its layers, which wrap the default chain.
    pub from: BTreeMap<(String, u32), (String, Vec<LayerPlan>)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DataPlan {
    pub module: usize,
    /// Index into its mod's `data`.
    pub index: usize,
    pub what: String,
    pub container: String,
    pub addr: u32,
    pub size: u32,
    #[serde(skip)]
    pub value: DataValue,
}

/// A hook's view of its target's C type: for `mod.h`.
#[derive(Debug, Clone)]
pub struct TargetType {
    /// The name C code calls it by.
    pub cname: String,
    pub returns: String,
    pub params: Vec<DeclParam>,
    pub variadic: bool,
    /// A header declares it (else `mod.h` does, from a signature).
    pub header: Option<Decl>,
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// Load order: indices into the mods passed to [`plan`].
    pub order: Vec<usize>,
    pub targets: BTreeMap<(String, u32), TargetPlan>,
    pub data: Vec<DataPlan>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    /// Per (mod, hook index): its target's type.
    pub types: BTreeMap<(usize, usize), TargetType>,
    /// Per (mod, hook index): the resolved target and `from`.
    pub resolved: BTreeMap<(usize, usize), (Func, Option<Func>)>,
}

/// `load_after` constraints first, then the order given.
fn load_order(mods: &[Mod], errors: &mut Vec<String>, warnings: &mut Vec<String>) -> Vec<usize> {
    let ids: BTreeMap<&str, usize> = mods.iter().enumerate().map(|(i, m)| (m.id(), i)).collect();
    let mut order = Vec::new();
    let mut placed = vec![false; mods.len()];
    for m in mods {
        for a in &m.decl.load_after {
            if !ids.contains_key(a.as_str()) {
                warnings.push(format!("{}: load_after {a}, which isn't applied", m.id()));
            }
        }
    }
    while order.len() < mods.len() {
        let next = (0..mods.len()).find(|&i| {
            !placed[i] && mods[i].decl.load_after.iter().all(|a| ids.get(a.as_str()).is_none_or(|&j| placed[j]))
        });
        match next {
            Some(i) => {
                placed[i] = true;
                order.push(i);
            }
            None => {
                let left: Vec<&str> = (0..mods.len()).filter(|&i| !placed[i]).map(|i| mods[i].id()).collect();
                errors.push(format!("load_after constraints form a cycle among {}", left.join(", ")));
                order.extend((0..mods.len()).filter(|&i| !placed[i]));
                break;
            }
        }
    }
    order
}

fn sig_type(s: &Signature, cname: String) -> TargetType {
    TargetType {
        cname,
        returns: s.returns.trim().to_string(),
        params: s
            .params
            .iter()
            .map(|p| DeclParam { name: Some(p.name.clone()), ty: p.ty.trim().to_string(), text: crate::genfiles::spell(&p.ty, &p.name) })
            .collect(),
        variadic: s.variadic,
        header: None,
    }
}

/// Resolve and check every declaration of `mods` (in the order the user
/// gave them) against `table`.
pub fn plan(table: &SymbolTable, mods: &[Mod]) -> Plan {
    let mut p = Plan::default();
    let mut seen = BTreeSet::new();
    for m in mods {
        if !seen.insert(m.id()) {
            p.errors.push(format!("{} is applied twice", m.id()));
        }
        if m.decl.game != table.game {
            p.errors.push(format!("{}: made for game {}, not {}", m.id(), m.decl.game, table.game));
        }
    }
    p.order = load_order(mods, &mut p.errors, &mut p.warnings);
    for &mi in &p.order.clone() {
        let m = &mods[mi];
        // (target, scope) → this mod's layer.
        let mut layers: BTreeMap<((String, u32), Option<(String, u32)>), LayerPlan> = BTreeMap::new();
        for (hi, h) in m.decl.hooks.iter().enumerate() {
            let at = format!("{}: hooks[{hi}] ({} {})", m.id(), h.kind, h.target);
            let f = match table.resolve_func(&h.target) {
                Ok(f) => f,
                Err(e) => {
                    p.errors.push(format!("{at}: {e:#}"));
                    continue;
                }
            };
            let from = match &h.from {
                Some(t) => match table.resolve_func(t) {
                    Ok(c) => Some(c),
                    Err(e) => {
                        p.errors.push(format!("{at}: from: {e:#}"));
                        continue;
                    }
                },
                None => None,
            };
            let info = table.function(&f.container, f.addr).expect("resolved");
            let name = table.display(&f.container, f.addr);
            let mut bad = |e: String| p.errors.push(format!("{at}: {e}"));
            if let Some((pt, k)) = &f.point {
                if *k != h.kind {
                    bad(format!("hook point {pt} is {k} {name}: hook it with kind {k}"));
                    continue;
                }
            }
            if info.expanded {
                bad(format!("{name} is expanded into its translated callers, so those calls never reach it; it can't be hooked"));
                continue;
            }
            if info.tier == TierName::Adapter && h.kind == Kind::Patch && h.original.is_none() {
                bad(format!("{name} is a runtime adapter: wrap it (before, after, or a patch with original), don't replace it"));
                continue;
            }
            // The target's C type, for mod.h and the chain.
            let ty = match (&h.signature, table.decl(info)) {
                // mod.h declares it from the signature; under another name
                // when a header declares it too, so the two can differ.
                (Some(s), d) => Some(sig_type(s, if d.is_some() { format!("psxw_sig_{}", info.name()) } else { info.name().to_string() })),
                (None, Some(d)) if d.prototyped => {
                    let cname = info.names.iter().find(|n| table.headers.functions.contains_key(*n)).cloned().unwrap_or_else(|| info.name().to_string());
                    Some(TargetType { cname, returns: d.returns.clone(), params: d.params.clone(), variadic: d.variadic, header: Some(d.clone()) })
                }
                (None, Some(_)) => {
                    bad(format!("{name}'s declaration has no prototype; give the hook a signature"));
                    continue;
                }
                (None, None) => {
                    bad(format!("no header declares {name}; give the hook a signature"));
                    continue;
                }
            };
            let ty = ty.expect("set");
            if h.kind == Kind::Patch && ty.variadic && h.original.is_some() {
                p.warnings.push(format!("{at}: {name} is variadic; its original can only be called with the arguments the patch names"));
            }
            p.types.insert((mi, hi), ty.clone());
            p.resolved.insert((mi, hi), (f.clone(), from.clone()));
            let returns = ty.returns != "void";
            let key = (f.container.clone(), f.addr);
            let tp = p.targets.entry(key.clone()).or_insert_with(|| TargetPlan {
                container: f.container.clone(),
                addr: f.addr,
                name: name.clone(),
                tier: info.tier,
                returns,
                default: vec![],
                from: BTreeMap::new(),
            });
            if tp.returns != returns {
                p.errors.push(format!("{at}: {name} is declared returning {}, which disagrees with another hook's declaration", ty.returns));
                continue;
            }
            let scope = from.as_ref().map(|c| (c.container.clone(), c.addr));
            let layer = layers.entry((key, scope)).or_insert_with(|| LayerPlan { module: mi, befores: vec![], patch: None, afters: vec![] });
            let bare = f.point.is_some();
            match h.kind {
                Kind::Before => layer.befores.push(HookUse { imp: h.imp.clone(), bare, hook: hi }),
                Kind::After => layer.afters.push(HookUse { imp: h.imp.clone(), bare, hook: hi }),
                Kind::Patch => {
                    if layer.patch.is_some() {
                        p.errors.push(format!("{at}: {} already patches {name} in this scope", m.id()));
                        continue;
                    }
                    layer.patch = Some(PatchUse { imp: h.imp.clone(), original: h.original.clone(), hook: hi });
                }
            }
        }
        for ((key, scope), layer) in layers {
            let tp = p.targets.get_mut(&key).expect("target");
            match scope {
                None => tp.default.push(layer),
                Some(c) => {
                    let n = table.display(&c.0, c.1);
                    tp.from.entry(c).or_insert_with(|| (n, vec![])).1.push(layer);
                }
            }
        }
        // Data patches.
        for (di, d) in m.decl.data.iter().enumerate() {
            let at = format!("{}: data[{di}] ({})", m.id(), d.target);
            let (c, base, size, what) = match table.resolve_data(&d.target) {
                Ok(r) => r,
                Err(e) => {
                    p.errors.push(format!("{at}: {e:#}"));
                    continue;
                }
            };
            let n = d.size();
            if let Some(s) = size {
                if d.offset + n > s {
                    p.errors.push(format!("{at}: offset {} + {n} bytes runs past {what} ({s} bytes)", d.offset));
                    continue;
                }
            }
            let addr = base + d.offset;
            let ci = table.container(&c).expect("container");
            if ci.text.iter().any(|r| addr < r[1] && r[0] < addr + n) {
                p.errors.push(format!("{at}: 0x{addr:08X} is code in {c}; a data patch writes data"));
                continue;
            }
            let loaded = addr >= ci.loaded[0] && addr + n <= ci.loaded[1];
            let bss = ci.bss.is_some_and(|b| addr >= b[0] && addr + n <= b[1]);
            if !loaded && !bss {
                let why = if ci.main { "outside the executable" } else { "outside what the disc loads for the overlay (its bss, which the game clears itself)" };
                p.errors.push(format!("{at}: 0x{addr:08X} is {why}; set it from code instead"));
                continue;
            }
            let value = match d.value() {
                Ok(v) => v,
                Err(e) => {
                    p.errors.push(format!("{at}: {e:#}"));
                    continue;
                }
            };
            p.data.push(DataPlan { module: mi, index: di, what: format!("{what}+{}", d.offset), container: c, addr, size: n, value });
        }
    }
    conflicts(&mut p, mods);
    p
}

/// Warnings for what a later mod makes unreachable (plans/M11 "Conflicts").
fn conflicts(p: &mut Plan, mods: &[Mod]) {
    let id = |m: usize| mods[m].id().to_string();
    for t in p.targets.values() {
        let name = &t.name;
        // The default chain: a full replacement hides everything below it.
        let check = |layers: &[&LayerPlan], scope: &str, out: &mut Vec<String>| {
            if let Some(top) = layers.iter().rposition(|l| l.replaces()) {
                let winner = layers[top];
                for l in &layers[..top] {
                    if l.replaces() {
                        out.push(format!(
                            "{} and {} both replace {name}{scope}; {}'s runs (applied last), {}'s never does",
                            id(l.module),
                            id(winner.module),
                            id(winner.module),
                            id(l.module)
                        ));
                    } else if l.module != winner.module {
                        out.push(format!(
                            "{}'s replacement of {name}{scope} hides {}'s {} on it, applied earlier: they never run",
                            id(winner.module),
                            id(l.module),
                            l.kinds()
                        ));
                    }
                }
            }
        };
        let default: Vec<&LayerPlan> = t.default.iter().collect();
        let mut out = Vec::new();
        check(&default, "", &mut out);
        for (cname, layers) in t.from.values() {
            let scope = format!(" for calls from {cname}");
            // A from scope's layers wrap the default chain.
            let all: Vec<&LayerPlan> = t.default.iter().chain(layers.iter()).collect();
            check(&all, &scope, &mut out);
            if let (Some(s), Some(d)) = (layers.iter().find(|l| l.replaces()), t.default.iter().find(|l| l.replaces())) {
                let later = p.order.iter().position(|&m| m == d.module) > p.order.iter().position(|&m| m == s.module);
                if later {
                    out.push(format!(
                        "{}'s from-scoped replacement of {name} wraps the default chain, so calls from {cname} run it although {} was applied later",
                        id(s.module),
                        id(d.module)
                    ));
                }
            }
        }
        out.dedup();
        p.warnings.extend(out);
    }
    // Data patches: the last applied value is written.
    for (j, b) in p.data.iter().enumerate() {
        for a in &p.data[..j] {
            if a.container == b.container && a.addr < b.addr + b.size && b.addr < a.addr + a.size {
                p.warnings.push(format!(
                    "{}'s data patch {} and {}'s {} overlap; {}'s is written (applied last)",
                    id(a.module),
                    a.what,
                    id(b.module),
                    b.what,
                    id(b.module)
                ));
            }
        }
    }
}

impl Plan {
    /// Every hooked function's chains, outermost link first.
    pub fn chain_table(&self, mods: &[Mod]) -> Vec<String> {
        let id = |m: usize| mods[m].id().to_string();
        let mut out = Vec::new();
        for t in self.targets.values() {
            let links = |ls: &[LayerPlan]| -> String {
                ls.iter().rev().map(|l| format!(" ← {} ({})", id(l.module), l.kinds())).collect::<String>()
            };
            if !t.default.is_empty() {
                out.push(format!("{}{}", t.name, links(&t.default)));
            }
            for (cname, ls) in t.from.values() {
                let rest = if t.default.is_empty() { String::new() } else { format!(" ← the default chain") };
                out.push(format!("{} from {cname}{}{rest}", t.name, links(ls)));
            }
        }
        out
    }

    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
}
