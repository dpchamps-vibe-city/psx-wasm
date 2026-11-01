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
use psxw_inputs::dispatch::{self as facts, DispatchSection, Effect, Event, Site};
use psxw_link::binding as link;
use psxw_link::game::Tier;
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
    "after-write, begin/end, keyed requests, instruction-address and native-boundary sites, c_marker sites",
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
    mod_residency: bool,
) -> Result<(Option<link::Binding>, Overlays, Report)> {
    let (binding, mut report) = match section {
        Some(s) => {
            let (b, r) = plan(s, map, translations, tiers)?;
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

    // Rules: what A implements, resolved.
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
            facts::Value::WriteAddress | facts::Value::WriteSize => bail!("binding.unimplemented_rule: {what} reads a write (after-write is milestone C)"),
        })
    };
    let mut rules = Vec::new();
    let mut operations: BTreeMap<(String, u32), Vec<usize>> = BTreeMap::new();
    let mut rule_lines = Vec::new();
    for r in &f.rules {
        let what = format!("rule `{}`", r.id);
        let unimplemented = |why: &str| anyhow::anyhow!("binding.unimplemented_rule: {what} {why}; milestone C implements it");
        match &r.site {
            Site::Function { .. } => {}
            Site::Pc { .. } => return Err(unimplemented("names an instruction address")),
            Site::Boundary(_) => return Err(unimplemented("names a native software boundary")),
        }
        if r.c_marker.is_some() {
            return Err(unimplemented("declares a c_marker source site"));
        }
        if matches!(r.request, Some(facts::Request::Keyed { .. })) {
            return Err(unimplemented("uses a keyed request"));
        }
        let event = match r.event {
            Event::Enter => link::Event::Enter,
            Event::Return => link::Event::Return,
            Event::AfterWrite => return Err(unimplemented("is an after-write event")),
        };
        let effect = match (r.effect, &r.target) {
            (Effect::Invalidate, _) => link::Effect::Invalidate,
            // Request initiation is never completion.
            (Effect::Publish, _) if event == link::Event::Enter => return Err(unimplemented("publishes at entry, before the operation completes")),
            (Effect::Publish, Some(t)) => {
                let target = match t {
                    facts::Target::Literal(p) => link::Target::Placement(placement_of[p.as_str()]),
                    facts::Target::Selector { value: v, cases } => link::Target::Selector {
                        value: value(v, &what)?,
                        cases: cases.iter().map(|(k, p)| (*k, placement_of[p.as_str()])).collect(),
                    },
                    facts::Target::Request => return Err(unimplemented("publishes a request's placement")),
                };
                let source_offset = match r.source_offset.as_ref().context("validated: source_offset")? {
                    facts::Offset::Placement => link::Offset::Derive,
                    facts::Offset::Value(v) => link::Offset::Value(value(v, &what)?),
                };
                link::Effect::Publish { target, source_offset }
            }
            (Effect::Begin | Effect::End, _) => return Err(unimplemented("begins or ends a request")),
            (Effect::Publish, None) => bail!("validated: a publication has a placement"),
        };
        let key = site_key(&r.site, &what)?;
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
        let rule = link::Rule {
            id: r.id.clone(),
            event,
            effect,
            destination: value(r.destination.as_ref().context("validated: destination")?, &what)?,
            length: value(r.length.as_ref().context("validated: length")?, &what)?,
            success: match r.success {
                facts::Success::Always => link::Success::Always,
                facts::Success::Zero => link::Success::Zero,
                facts::Success::Nonzero => link::Success::Nonzero,
            },
        };
        rule_lines.push(format!("#{} {} at {} ({:#010x}): {:?} {:?}", rules.len(), r.id, r.site, key.1, r.event, r.effect));
        operations.entry(key).or_default().push(rules.len());
        rules.push(rule);
    }

    // Writer summaries: a writer that can change a placement's code needs
    // a rule at its site.
    let mut writers = Vec::new();
    for w in &f.writers {
        let what = format!("writer `{}`", w.id);
        if let Site::Function { .. } = &w.site {
            site_key(&w.site, &what)?;
        }
        let code: Vec<String> =
            w.destinations.iter().map(|d| d.start..d.end).filter(|d| covered.iter().any(|k| overlaps(k, &window(d)))).map(|d| hex(&d)).collect();
        let by: Vec<&str> = f.rules.iter().filter(|r| r.site == w.site).map(|r| r.id.as_str()).collect();
        ensure!(
            code.is_empty() || !by.is_empty(),
            "binding.uncovered_writer: {what} at {} can write placement code at {}; add a rule for its code-changing effect",
            w.site,
            code.join(", ")
        );
        writers.push(format!(
            "{} at {}: declared bounds {}; {}",
            w.id,
            w.site,
            w.destinations.iter().map(|d| hex(&(d.start..d.end))).collect::<Vec<_>>().join(", "),
            if code.is_empty() { "disjoint from placement code".to_string() } else { format!("changes code, covered by {}", by.join(", ")) }
        ));
    }

    let binding = link::Binding { slots, placements, rules, operations };
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
