//! The binding plan (plans/dispatch-binding A): a game's `[dispatch]` facts
//! resolved against its address map and implementations, and what remains
//! on the legacy sampled-word dispatch. Shared by `psxw build` and the
//! dispatch conformance fixtures.
//!
//! Every function whose entry lies in a placement's text is a changing
//! slot, bound only by boot (the main image) or by a declared operation's
//! publication. Missing facts and unimplemented rules are build errors,
//! never a fallback to code probes.

use anyhow::{Context as _, Result, bail, ensure};
use psxw_abi::WINDOW_MASK;
use psxw_inputs::AddressMap;
use psxw_inputs::dispatch::{self as facts, DispatchSection, Effect, Event, HEADER, SITE_PREFIX, Site};
use psxw_link::binding as link;
use psxw_link::game::Tier;
use psxw_link::marshal;
use psxw_link::object::{NameClass, classify_name};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

/// Candidates and sampled checks at shared, uncovered entry addresses.
pub type Overlays = BTreeMap<u32, Vec<(String, Vec<(u32, u32)>)>>;

/// What the build reports about its dispatch (`build.json` `binding`).
#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    /// The build has `[dispatch]` facts.
    pub explicit: bool,
    pub placements: Vec<String>,
    /// By index: the index `psx.binding_fault` reports.
    pub rules: Vec<String>,
    /// Writer summaries: their bounds' provenance and covering rules.
    pub writers: Vec<String>,
    /// Each covered address: its implementations, the window ranges each
    /// depends on, and which one boot binds.
    pub slots: Vec<String>,
    /// Entries per reference category: `fixed` (no other implementation
    /// can occupy the address), `slot` (explicit binding), `legacy-sampled`
    /// (resident code words choose), `legacy-unique` (an uncovered
    /// overlay's entry, callable whatever is resident).
    pub classification: BTreeMap<&'static str, usize>,
    /// Code references by how the linker bound them: `fixed`, `slot`,
    /// `lookup` (a guest address through the dispatch table) and the
    /// legacy classes.
    pub references: BTreeMap<&'static str, usize>,
    /// What still bypasses explicit bindings.
    pub legacy: Vec<String>,
    /// Required work this build doesn't do yet (milestone C).
    pub unfinished: Vec<&'static str>,
}

/// Milestone C obligations, reported with every explicit build.
const UNFINISHED: &[&str] = &[
    "partial writes: an implementation whose bytes span two publications stays unbound until C composes them",
    "after-write at lifted instruction addresses, begin/end, keyed requests, native-boundary sites",
    "active continuations: suspended lifted code and explicit C continuation handlers",
    "runtime merge: native loaders and host reads publish nothing yet",
    "mod residency: hooks at covered addresses and overlay data patches",
    "callable namespace: pointer calls still index the masked dispatch table",
];

/// The explicit binding plan and the legacy overlay plan for the rest.
pub(crate) fn dispatch(
    section: Option<&DispatchSection>,
    map: &AddressMap,
    program: &psxw_lift::program::Program,
    originals: &psxw_inputs::originals::Originals,
    translations: &[psxw_lift::Translation],
    tiers: &BTreeMap<(String, u32), Tier>,
    inputs: &[psxw_link::Input],
    mod_residency: bool,
) -> Result<(Option<link::Binding>, Overlays, Report)> {
    let (binding, mut report) = match section {
        Some(s) => {
            let (b, r) = plan(s, map, translations, tiers, inputs)?;
            (Some(b), r)
        }
        None => (None, Report::default()),
    };
    let mut overlays = crate::game::overlay_plan(program, originals, tiers)?;
    if let Some(b) = &binding {
        overlays.retain(|a, _| !b.slots.contains_key(a));
    }
    classify(&mut report, map, tiers, binding.as_ref(), &overlays, mod_residency);
    Ok((binding, overlays, report))
}

/// The functions whose call boundary must survive C optimization: declared
/// operations (an inlined call would skip the wrapper) and every function
/// at a changing slot (an inlined body would skip the binding lookup). By
/// address, so whatever name the C or the facts use for them counts.
pub(crate) struct Boundaries {
    operations: BTreeSet<(String, u32)>,
    /// The placements' text, as window offsets.
    covered: Vec<Range<u32>>,
}

impl Boundaries {
    pub(crate) fn new(section: Option<&DispatchSection>, map: &AddressMap) -> Self {
        let mut operations = BTreeSet::new();
        let mut covered = Vec::new();
        if let Some(s) = section {
            for r in &s.rules {
                if let (Some(c), Some(f)) = (&r.container, &r.function) {
                    operations.extend(map.resolve(c, f).map(|r| (c.clone(), r.addr)));
                }
            }
            for p in &s.placements {
                covered.extend(map.container(&p.container).into_iter().flat_map(|c| c.text.iter().map(window)));
            }
        }
        Self { operations, covered }
    }

    /// Whether C in `container` defining `name` must keep its boundary.
    pub(crate) fn keeps(&self, map: &AddressMap, container: &str, name: &str) -> bool {
        map.resolve(container, name).is_some_and(|r| {
            self.operations.contains(&(container.to_string(), r.addr)) || self.covered.iter().any(|k| k.contains(&(r.addr & WINDOW_MASK)))
        })
    }
}

fn window(r: &Range<u32>) -> Range<u32> {
    (r.start & WINDOW_MASK)..((r.end - 1) & WINDOW_MASK) + 1
}

fn overlaps(a: &Range<u32>, b: &Range<u32>) -> bool {
    a.start < b.end && b.start < a.end
}

fn hex(r: &Range<u32>) -> String {
    format!("0x{:08X}..0x{:08X}", r.start, r.end)
}

/// Resolve the facts. The deterministic order of everything here is the
/// order of the facts and of the tiers.
pub(crate) fn plan(
    section: &DispatchSection,
    map: &AddressMap,
    translations: &[psxw_lift::Translation],
    tiers: &BTreeMap<(String, u32), Tier>,
    inputs: &[psxw_link::Input],
) -> Result<(link::Binding, Report)> {
    let f = section.check()?;
    ensure!(!f.require_explicit, "binding.unimplemented: require_explicit = true (strict builds) is milestone C");
    ensure!(
        f.c_continuations.is_empty(),
        "binding.unimplemented: c_continuations ({}) are milestone C",
        f.c_continuations.iter().map(|c| c.slot.as_str()).collect::<Vec<_>>().join(", ")
    );
    let container = |id: &str, what: &str| map.container(id).with_context(|| format!("binding.invalid_fact: {what} names no container `{id}`"));

    // Placements: their containers' own layouts.
    let mut placements = Vec::new();
    let mut covered: Vec<Range<u32>> = Vec::new();
    for p in &f.placements {
        let c = container(&p.container, &format!("placement `{}`", p.id))?;
        ensure!(!c.text.is_empty(), "binding.invalid_fact: placement `{}`: {} has no text", p.id, p.container);
        placements.push(link::Placement { id: p.id.clone(), container: p.container.clone(), segments: c.segments.clone() });
        covered.extend(c.text.iter().map(window));
    }
    let in_covered = |a: u32| covered.iter().any(|r| r.contains(&(a & WINDOW_MASK)));
    let placement_of: BTreeMap<&str, usize> = f.placements.iter().enumerate().map(|(i, p)| (p.id.as_str(), i)).collect();
    let placed: BTreeSet<&str> = f.placements.iter().map(|p| p.container.as_str()).collect();
    // Another image that can occupy covered bytes needs a placement, or its
    // arrival would leave a stale binding.
    for c in &map.containers {
        if c.is_main || placed.contains(c.id.as_str()) {
            continue;
        }
        if let Some(r) = c.text.iter().map(window).find(|r| covered.iter().any(|k| overlaps(k, r))) {
            bail!("binding.missing_placement: {}'s text {} overlaps a placement's; declare a placement for it", c.id, hex(&r));
        }
    }

    // Implementations and what each depends on.
    let by_key: BTreeMap<(&str, u32), &psxw_lift::Translation> =
        translations.iter().map(|t| ((t.func.container.as_str(), t.func.addr), t)).collect();
    let mut slots: BTreeMap<u32, Vec<link::Candidate>> = BTreeMap::new();
    for ((c, a), tier) in tiers {
        let t = by_key.get(&(c.as_str(), *a)).with_context(|| format!("{c} {a:#010x}: no translation to locate"))?;
        let regions: Vec<(u32, u32, String)> = match tier {
            Tier::Lift { .. } => t.graph.regions.clone(),
            _ => vec![(t.func.addr, t.func.end, c.clone())],
        };
        let mut footprint = Vec::new();
        for (s, e, rc) in regions.into_iter().filter(|(s, e, _)| s < e) {
            let r = window(&(s..e));
            let changing = covered.iter().any(|k| overlaps(k, &r));
            if rc != *c && changing {
                bail!(
                    "binding.replaceable_dependency: {c} {a:#010x}'s translation embeds {rc}'s code at {}, which a placement can replace",
                    hex(&(s..e))
                );
            }
            if rc == *c {
                footprint.push(r);
            }
        }
        if in_covered(*a) {
            ensure!(!footprint.is_empty(), "{c} {a:#010x}: an implementation with no extent");
            slots.entry(*a).or_default().push(link::Candidate { container: c.clone(), footprint, boot: *c == map.main });
        } else if let Some(r) = footprint.iter().find(|r| covered.iter().any(|k| overlaps(k, r))) {
            bail!(
                "binding.replaceable_dependency: {c} {a:#010x} is entered outside the placements but its code at window {:#x}..{:#x} can be replaced",
                r.start,
                r.end
            );
        }
    }

    // Each compiled function's o32 shape, to validate what rules read
    // before code generation: its argument words, and whether it returns.
    let shape = |key: &(String, u32)| -> (u32, bool) {
        match tiers.get(key) {
            Some(&Tier::C { input, func }) => {
                let o = &inputs[input].obj;
                let sig = &o.types[o.funcs[func as usize].ty as usize];
                let name = o.func_symbol(func + o.num_imported_funcs()).map(|s| s.name.clone()).unwrap_or_default();
                let guest = match classify_name(&name) {
                    NameClass::Guest(n) => n.to_string(),
                    _ => name,
                };
                let (kinds, variadic) = marshal::param_kinds(sig, inputs[input].protos.get(&guest));
                (marshal::words(&kinds, variadic).0.len() as u32, !sig.results.is_empty())
            }
            _ => (facts::MAX_ARG, true),
        }
    };
    // The function whose extent holds an instruction.
    let function_at = |c: &str, pc: u32| {
        translations
            .iter()
            .filter(|t| t.func.container == c && t.func.interior_of.is_none() && (t.func.addr..t.func.end).contains(&pc))
            .max_by_key(|t| t.func.addr)
    };
    let site_key = |site: &Site, what: &str| -> Result<(String, u32)> {
        let Site::Function { container: c, function } = site else { unreachable!("function sites only") };
        container(c, what)?;
        let r = map.resolve(c, function).with_context(|| format!("binding.unknown_site: {what}: {c} has no function `{function}`"))?;
        let key = (c.clone(), r.addr);
        ensure!(tiers.contains_key(&key), "binding.unknown_site: {what}: {c}:{function} ({:#010x}) is not a compiled function of {c}", r.addr);
        Ok(key)
    };
    let value = |v: &facts::Value, what: &str| -> Result<link::Value> {
        Ok(match v {
            facts::Value::Const(c) => link::Value::Const(*c),
            facts::Value::Arg(a) => link::Value::Arg(*a),
            facts::Value::Return => link::Value::Return,
            facts::Value::Field { arg, offset } => link::Value::Field { arg: *arg, offset: *offset },
            facts::Value::Global { container: c, symbol } => {
                container(c, what)?;
                link::Value::Load(map.resolve(c, symbol).with_context(|| format!("binding.invalid_fact: {what}: {c} has no symbol `{symbol}`"))?.addr)
            }
            facts::Value::WriteAddress | facts::Value::WriteSize => {
                bail!("binding.unimplemented_rule: {what} reads a lifted write (instruction-address after-write is milestone C)")
            }
        })
    };
    let mut rules = Vec::new();
    let mut operations: BTreeMap<(String, u32), Vec<usize>> = BTreeMap::new();
    let mut markers: BTreeMap<String, usize> = BTreeMap::new();
    let mut rule_lines = Vec::new();
    for r in &f.rules {
        let what = format!("rule `{}`", r.id);
        let unimplemented = |why: &str| anyhow::anyhow!("binding.unimplemented_rule: {what} {why}; milestone C implements it");
        if matches!(r.request, Some(facts::Request::Keyed { .. })) {
            return Err(unimplemented("uses a keyed request"));
        }
        // Where the rule applies: an operation's wrapper, or a c_marker
        // call in authored C after the write it describes.
        let (key, marker) = match &r.site {
            Site::Boundary(_) => return Err(unimplemented("names a native software boundary")),
            Site::Function { .. } => {
                ensure!(r.event != Event::AfterWrite, "binding.invalid_fact: {what}: after-write names its write's instruction address (`pc`)");
                (site_key(&r.site, &what)?, None)
            }
            Site::Pc { container: c, pc } => {
                container(c, &what)?;
                ensure!(r.event == Event::AfterWrite, "binding.invalid_fact: {what}: an instruction-address site takes event `after-write`");
                let t = function_at(c, *pc).with_context(|| format!("binding.unknown_site: {what}: no compiled function of {c} holds 0x{pc:08X}"))?;
                let key = (c.clone(), t.func.addr);
                let (input, func) = match tiers.get(&key) {
                    Some(&Tier::C { input, func }) => (input, func),
                    Some(Tier::Lift { .. }) => return Err(unimplemented("instruments a lifted write at an instruction address")),
                    _ => return Err(unimplemented("names a write inside an adapter")),
                };
                let m = r.c_marker.as_deref().with_context(|| {
                    format!(
                        "binding.missing_c_write_site: {what}: 0x{pc:08X} is in {}, supplied C with no verified mapping of its writes; \
                         declare c_marker = \"<name>\" and call {SITE_PREFIX}<name>(destination, length, source_offset, selector, \
                         request_key) from {HEADER} right after the write",
                        t.func.name
                    )
                })?;
                // The call must be in the function holding the write, not
                // merely somewhere in its object.
                let symbol = format!("{SITE_PREFIX}{m}");
                let o = &inputs[input].obj;
                let body = &o.funcs[func as usize].body;
                let body = (body.start - o.code_start) as u32..(body.end - o.code_start) as u32;
                let calls = o.code_relocs.iter().any(|rel| {
                    rel.ty == wasmparser::RelocationType::FunctionIndexLeb
                        && body.contains(&rel.offset)
                        && o.symbols.get(rel.symbol as usize).is_some_and(|s| s.name == symbol)
                });
                ensure!(
                    calls,
                    "binding.missing_c_write_site: {what}: {} ({}) never calls {symbol}; call it there, right after the write",
                    t.func.name,
                    inputs[input].label
                );
                ensure!(!markers.contains_key(m), "binding.invalid_fact: two rules declare c_marker `{m}`");
                (key, Some(m.to_string()))
            }
        };
        // At a c_marker the authored C passes the normalized values:
        // destination, length, source offset, selector.
        let val = |v: &facts::Value, word: u32| -> Result<link::Value> {
            if marker.is_some() { Ok(link::Value::Arg(word)) } else { value(v, &what) }
        };
        let event = match r.event {
            Event::Enter => link::Event::Enter,
            Event::Return => link::Event::Return,
            Event::AfterWrite => link::Event::AfterWrite,
        };
        let effect = match (r.effect, &r.target) {
            (Effect::Invalidate, _) => link::Effect::Invalidate,
            // Request initiation is never completion.
            (Effect::Publish, _) if event == link::Event::Enter => return Err(unimplemented("publishes at entry, before the operation completes")),
            (Effect::Publish, Some(t)) => {
                let target = match t {
                    facts::Target::Literal(p) => link::Target::Placement(placement_of[p.as_str()]),
                    facts::Target::Selector { value: v, cases } => link::Target::Selector {
                        value: val(v, 3)?,
                        cases: cases.iter().map(|(k, p)| (*k, placement_of[p.as_str()])).collect(),
                    },
                    facts::Target::Request => return Err(unimplemented("publishes a request's placement")),
                };
                let source_offset = match r.source_offset.as_ref().context("validated: source_offset")? {
                    // A marker's offset must agree with the placement's mapping.
                    facts::Offset::Placement if marker.is_some() => link::Offset::DeriveChecked(link::Value::Arg(2)),
                    facts::Offset::Placement => link::Offset::Derive,
                    facts::Offset::Value(v) => link::Offset::Value(val(v, 2)?),
                };
                link::Effect::Publish { target, source_offset }
            }
            (Effect::Begin | Effect::End, _) => return Err(unimplemented("begins or ends a request")),
            (Effect::Publish, None) => bail!("validated: a publication has a placement"),
        };
        // At a c_marker, what the rule states literally (a constant, or a
        // global read at the call) constrains the C's value for that word.
        let mut checks = Vec::new();
        if marker.is_some() {
            let mut declared = vec![(0, r.destination.as_ref()), (1, r.length.as_ref())];
            if let Some(facts::Offset::Value(v)) = &r.source_offset {
                declared.push((2, Some(v)));
            }
            if let Some(facts::Target::Selector { value: v, .. }) = &r.target {
                declared.push((3, Some(v)));
            }
            for (word, v) in declared {
                if let Some(v @ (facts::Value::Const(_) | facts::Value::Global { .. })) = v {
                    checks.push((word, value(v, &what)?));
                }
            }
        }
        let rule = link::Rule {
            id: r.id.clone(),
            event,
            effect,
            destination: val(r.destination.as_ref().context("validated: destination")?, 0)?,
            length: val(r.length.as_ref().context("validated: length")?, 1)?,
            success: match r.success {
                facts::Success::Always => link::Success::Always,
                facts::Success::Zero => link::Success::Zero,
                facts::Success::Nonzero => link::Success::Nonzero,
            },
            checks,
        };
        if let Some(m) = marker {
            rule_lines.push(format!("#{} {} at {}: {:?} {:?} through {SITE_PREFIX}{m} in authored C", rules.len(), r.id, r.site, r.event, r.effect));
            markers.insert(m, rules.len());
            rules.push(rule);
            continue;
        }
        // What the rule reads must exist in the operation's signature.
        let (words, returns) = shape(&key);
        for v in reads(&rule) {
            match v {
                link::Value::Arg(a) | link::Value::Field { arg: a, .. } => ensure!(
                    *a < words,
                    "binding.invalid_fact: {what}: {} {:#010x} has {words} argument words; the rule reads arg:{a}",
                    key.0,
                    key.1
                ),
                link::Value::Return => ensure!(returns, "binding.invalid_fact: {what}: {} {:#010x} returns nothing; the rule reads `return`", key.0, key.1),
                _ => {}
            }
        }
        ensure!(
            rule.success == link::Success::Always || returns,
            "binding.invalid_fact: {what}: success = {:?} tests a result {} {:#010x} doesn't return",
            r.success,
            key.0,
            key.1
        );
        // The translator expands some callees into their callers: the
        // operation's boundary would disappear there.
        let expanded: Vec<String> = translations
            .iter()
            .filter(|t| matches!(tiers.get(&(t.func.container.clone(), t.func.addr)), Some(Tier::Lift { .. })))
            .filter(|t| t.graph.regions.iter().skip(1).any(|(s, e, c)| *c == key.0 && (*s..*e).contains(&key.1)))
            .map(|t| t.func.name.clone())
            .collect();
        ensure!(
            expanded.is_empty(),
            "binding.expanded_operation: {what}: {} {:#010x} is expanded into {}; instrument its callers or keep its boundary (milestone C)",
            key.0,
            key.1,
            expanded.join(", ")
        );
        rule_lines.push(format!("#{} {} at {} ({:#010x}): {:?} {:?}", rules.len(), r.id, r.site, key.1, r.event, r.effect));
        operations.entry(key).or_default().push(rules.len());
        rules.push(rule);
    }

    // Writer summaries: bounds inferred first, declared where analysis
    // can't establish them, and every known write inside the declaration.
    // A writer that can change a placement's code needs a rule.
    let by_key: BTreeMap<(&str, u32), &psxw_lift::Translation> =
        translations.iter().map(|t| ((t.func.container.as_str(), t.func.addr), t)).collect();
    let mut writers = Vec::new();
    for w in &f.writers {
        let what = format!("writer `{}`", w.id);
        let (analysed, extent) = match &w.site {
            Site::Function { .. } => {
                let key = site_key(&w.site, &what)?;
                let t = by_key[&(key.0.as_str(), key.1)];
                (writes(&key, &by_key, tiers, &map.main), Some((key.0.clone(), t.func.addr..t.func.end)))
            }
            Site::Pc { container: c, pc } => {
                container(c, &what)?;
                let t = function_at(c, *pc).with_context(|| format!("binding.unknown_site: {what}: no compiled function of {c} holds 0x{pc:08X}"))?;
                let node = t.graph.nodes.iter().find(|(n, d)| n.pc() == *pc && d.insn.op.is_store());
                let (node, _) = node.with_context(|| format!("binding.invalid_fact: {what}: 0x{pc:08X} is not a store"))?;
                let mut one = Writes::default();
                if matches!(tiers.get(&(c.clone(), t.func.addr)), Some(Tier::Lift { .. })) {
                    store(&mut one, t, *node);
                } else {
                    one.unbounded = Some(format!("{} is supplied C", t.func.name));
                }
                (one, None)
            }
            Site::Boundary(b) => (Writes { unbounded: Some(format!("{b} is a native boundary")), ..Writes::default() }, None),
        };
        let declared: Vec<Range<u32>> = w.destinations.iter().map(|d| d.start..d.end).collect();
        if !declared.is_empty() {
            for (pc, r) in &analysed.known {
                ensure!(
                    declared.iter().any(|d| d.start <= r.start && r.end <= d.end),
                    "binding.invalid_fact: {what} declares {} but writes {} at 0x{pc:08X}",
                    declared.iter().map(hex).collect::<Vec<_>>().join(", "),
                    hex(r)
                );
            }
        }
        let (bounds, how) = match &analysed.unbounded {
            None => (analysed.known.iter().map(|(_, r)| r.clone()).collect::<Vec<_>>(), "inferred".to_string()),
            Some(why) if !declared.is_empty() => (declared.clone(), format!("declared (analysis can't bound it: {why})")),
            Some(why) => bail!("binding.invalid_fact: {what}: analysis can't bound its writes ({why}); declare `destinations`"),
        };
        let code: Vec<String> = bounds.iter().filter(|d| covered.iter().any(|k| overlaps(k, &window(d)))).map(hex).collect();
        // A rule at its site (by address: any name for the function), or
        // at a write inside its function.
        let at = |site: &Site| match site {
            Site::Function { container: c, function } => map.resolve(c, function).map(|r| (c.clone(), r.addr)),
            _ => None,
        };
        let by: Vec<&str> = f
            .rules
            .iter()
            .filter(|r| {
                r.site == w.site
                    || at(&r.site).is_some_and(|k| at(&w.site) == Some(k))
                    || matches!((&r.site, &extent), (Site::Pc { container: c, pc }, Some((wc, e))) if c == wc && e.contains(pc))
            })
            .map(|r| r.id.as_str())
            .collect();
        ensure!(
            code.is_empty() || !by.is_empty(),
            "binding.uncovered_writer: {what} at {} can write placement code at {}; add a rule for its code-changing effect",
            w.site,
            code.join(", ")
        );
        let mut known: BTreeSet<String> = analysed.known.iter().map(|(_, r)| hex(r)).collect();
        if analysed.stack {
            known.insert("its stack frames".into());
        }
        writers.push(format!(
            "{} at {}: {how} bounds {}; known writes {}; {}",
            w.id,
            w.site,
            if bounds.is_empty() { "none".to_string() } else { bounds.iter().map(hex).collect::<Vec<_>>().join(", ") },
            if known.is_empty() { "none".to_string() } else { known.into_iter().collect::<Vec<_>>().join(", ") },
            if code.is_empty() { "disjoint from placement code".to_string() } else { format!("changes code, covered by {}", by.join(", ")) }
        ));
    }

    let binding = link::Binding { slots, placements, rules, operations, markers };
    let report = Report {
        explicit: true,
        placements: binding
            .placements
            .iter()
            .map(|p| {
                let text: Vec<String> = map.container(&p.container).map(|c| c.text.iter().map(hex).collect()).unwrap_or_default();
                format!("{} = {} (text {})", p.id, p.container, text.join(", "))
            })
            .collect(),
        rules: rule_lines,
        writers,
        slots: binding
            .slots
            .iter()
            .map(|(a, cs)| {
                let each: Vec<String> = cs
                    .iter()
                    .map(|c| {
                        let fp: Vec<String> = c.footprint.iter().map(|r| format!("{:#x}..{:#x}", r.start, r.end)).collect();
                        format!("{}{} [{}]", c.container, if c.boot { " (boot)" } else { "" }, fp.join(" "))
                    })
                    .collect();
                format!("0x{a:08X}: {}", each.join("; "))
            })
            .collect(),
        unfinished: UNFINISHED.to_vec(),
        ..Report::default()
    };
    Ok((binding, report))
}

/// What analysis establishes about an operation's writes, its callees'
/// included: stores at constant addresses (pc, bytes), whether it writes
/// its stack frames (`$sp`-relative), and the first write it can't bound.
#[derive(Debug, Default)]
struct Writes {
    known: Vec<(u32, Range<u32>)>,
    stack: bool,
    unbounded: Option<String>,
}

/// One store of a lifted function, by the register constants on entry to
/// its node.
fn store(out: &mut Writes, t: &psxw_lift::Translation, node: psxw_lift::graph::Node) {
    use psxw_lift::decode::Op;
    let insn = &t.graph.nodes[&node].insn;
    if insn.rs == 29 {
        out.stack = true;
        return;
    }
    match t.facts.consts.get(&node).and_then(|c| c[insn.rs as usize]) {
        Some(base) => {
            let at = base.wrapping_add(insn.simm as u32);
            let bytes = match insn.op {
                Op::Sb => at..at + 1,
                Op::Sh => at..at + 2,
                Op::Swl | Op::Swr => (at & !3)..(at & !3) + 4,
                _ => at..at + 4,
            };
            out.known.push((node.pc(), bytes));
        }
        None => {
            out.unbounded.get_or_insert_with(|| format!("{} stores through a register at 0x{:08X}", t.func.name, node.pc()));
        }
    }
}

/// The writes of the function at `key` and everything it calls.
fn writes(
    key: &(String, u32),
    by_key: &BTreeMap<(&str, u32), &psxw_lift::Translation>,
    tiers: &BTreeMap<(String, u32), Tier>,
    main: &str,
) -> Writes {
    use psxw_lift::graph::{Callee, Dest, Transfer};
    let mut out = Writes::default();
    let mut seen: BTreeSet<(String, u32)> = BTreeSet::new();
    let mut work = vec![key.clone()];
    while let Some((c, a)) = work.pop() {
        if !seen.insert((c.clone(), a)) {
            continue;
        }
        let Some(t) = by_key.get(&(c.as_str(), a)) else {
            out.unbounded.get_or_insert_with(|| format!("a call to 0x{a:08X}, which nothing implements"));
            continue;
        };
        match tiers.get(&(c.clone(), a)) {
            Some(Tier::Lift { .. }) => {}
            Some(Tier::C { .. }) => {
                out.unbounded.get_or_insert_with(|| format!("{} is supplied C", t.func.name));
                continue;
            }
            _ => {
                out.unbounded.get_or_insert_with(|| format!("{} is an adapter", t.func.name));
                continue;
            }
        }
        for (node, d) in &t.graph.nodes {
            if d.insn.op.is_store() {
                store(&mut out, t, *node);
            }
            let tail = |d: &Dest| match d {
                Dest::Tail(c) => vec![c.clone()],
                Dest::Local(_) => vec![],
            };
            let callees: Vec<Callee> = match &d.transfer {
                Transfer::Call { callee, next } | Transfer::CondCall { callee, next } => [vec![callee.clone()], tail(next)].concat(),
                Transfer::FallOff(c) => vec![c.clone()],
                Transfer::Jump(dest) => tail(dest),
                Transfer::Cond { taken, fall } => [tail(taken), tail(fall)].concat(),
                _ => vec![],
            };
            for callee in callees {
                match callee {
                    Callee::Direct { addr, .. } => {
                        // As the linker locates it: the caller's container,
                        // the main one, else every container holding it.
                        let own = [c.as_str(), main].into_iter().find(|k| by_key.contains_key(&(*k, addr)));
                        match own {
                            Some(k) => work.push((k.to_string(), addr)),
                            None => work.extend(by_key.keys().filter(|(_, x)| *x == addr).map(|(k, x)| (k.to_string(), *x))),
                        }
                        if !by_key.keys().any(|(_, x)| *x == addr) {
                            out.unbounded.get_or_insert_with(|| format!("a call to 0x{addr:08X}, which nothing implements"));
                        }
                    }
                    Callee::Indirect { .. } => {
                        out.unbounded.get_or_insert_with(|| format!("{} calls through a pointer at 0x{:08X}", t.func.name, node.pc()));
                    }
                    Callee::Bios { .. } | Callee::Kernel { .. } => {
                        out.unbounded.get_or_insert_with(|| format!("{} calls the BIOS or kernel at 0x{:08X}", t.func.name, node.pc()));
                    }
                }
            }
        }
    }
    out
}

/// The values a rule reads.
fn reads(rule: &link::Rule) -> Vec<&link::Value> {
    let mut v = vec![&rule.destination, &rule.length];
    if let link::Effect::Publish { target, source_offset } = &rule.effect {
        if let link::Target::Selector { value, .. } = target {
            v.push(value);
        }
        if let link::Offset::Value(s) | link::Offset::DeriveChecked(s) = source_offset {
            v.push(s);
        }
    }
    v
}

/// Count each entry's reference category and list what still bypasses
/// explicit bindings.
fn classify(
    report: &mut Report,
    map: &AddressMap,
    tiers: &BTreeMap<(String, u32), Tier>,
    binding: Option<&link::Binding>,
    overlays: &Overlays,
    mod_residency: bool,
) {
    let covered = |a: u32| binding.is_some_and(|b| b.slots.contains_key(&a));
    let placed = |c: &str| binding.is_some_and(|b| b.placements.iter().any(|p| p.container == c));
    let mut unique: BTreeMap<&str, usize> = BTreeMap::new();
    for (c, a) in tiers.keys() {
        let class = if covered(*a) {
            "slot"
        } else if overlays.contains_key(a) {
            "legacy-sampled"
        } else if *c != map.main && !placed(c) {
            *unique.entry(c.as_str()).or_default() += 1;
            "legacy-unique"
        } else {
            "fixed"
        };
        *report.classification.entry(class).or_default() += 1;
    }
    for (a, cs) in overlays {
        let names: Vec<&str> = cs.iter().map(|(c, _)| c.as_str()).collect();
        report.legacy.push(format!("0x{a:08X}: resident code words choose among {} (main calls exempt)", names.join(", ")));
    }
    // An overlay without a placement is legacy whether or not it shares
    // an address: its unique entries are callable whatever is resident.
    for (c, n) in unique {
        report.legacy.push(format!("{c}: {n} entries callable without any residency check"));
    }
    if mod_residency {
        report.legacy.push("mod data patches: an overlay's first two code words decide it is resident".into());
    }
}
