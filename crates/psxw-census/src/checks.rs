use crate::Context;
use crate::report::*;
use psxw_cc::{HOST_SUPPORT, LIFT_MARKER_PREFIX};
use psxw_link::object::{NameClass, RelocKind, SymbolKind, WasmObject, classify_name};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Imports the runtime provides (not guest symbols).
const RUNTIME_IMPORTS: &[&str] =
    &["__psxw_break", "psx_gte_mfc2", "psx_gte_cfc2", "psx_gte_mtc2", "psx_gte_ctc2", "psx_gte_exec"];

fn hex(a: u32) -> String {
    format!("0x{a:08X}")
}

fn named(container: &str, name: &str, addr: Option<u32>, detail: impl Into<String>) -> Named {
    Named { container: container.into(), name: name.into(), addr: addr.map(hex), detail: detail.into() }
}

/// Lift reasons aggregate better without their quoted specifics.
fn reason_class(reason: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    for c in reason.chars() {
        if c == '\'' {
            quoted = !quoted;
            if quoted {
                out.push_str("'…'");
            }
        } else if !quoted {
            out.push(c);
        }
    }
    out
}

fn cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

pub(crate) fn functions(ctx: &Context, tus: &[TuReport], partial: bool) -> FunctionReport {
    let mut r = FunctionReport::default();
    // (container, addr) → [(name, tu, tier)]
    let mut defs: BTreeMap<(String, u32), Vec<(String, String, String)>> = BTreeMap::new();
    for tu in tus {
        let Some(obj) = &tu.object else { continue };
        let mut found: Vec<(String, String)> = Vec::new();
        for s in &obj.symbols {
            if s.defined && s.kind == SymbolKind::Function && !s.name.is_empty() {
                match classify_name(&s.name) {
                    NameClass::Guest(name) => found.push((name.to_string(), "c".into())),
                    NameClass::MainWrapper => {}
                    NameClass::SignatureThunk { target } => {
                        *r.signature_thunks.entry(target.to_string()).or_default() += 1;
                    }
                }
            }
            if let (true, Some(def), Some(f)) = (s.defined, s.data, s.name.strip_prefix(LIFT_MARKER_PREFIX)) {
                let reason = cstr(obj.data_bytes(def));
                *r.lift_reasons.entry(reason_class(&reason)).or_default() += 1;
                r.lifted.push(named(&tu.container, f, None, reason.clone()));
                found.push((f.to_string(), "lift".into()));
            }
        }
        for (name, tier) in found {
            match ctx.map.resolve(&tu.container, &name) {
                Some(res) => {
                    let tier = if ctx.map.sdk_at(&tu.container, res.addr).is_some() { format!("{tier} (sdk range)") } else { tier };
                    defs.entry((tu.container.clone(), res.addr)).or_default().push((name, tu.name.clone(), tier));
                }
                None => r.unplaceable.push(named(&tu.container, &name, None, format!("{tier} in {}", tu.name))),
            }
        }
    }
    for ((c, addr), list) in &defs {
        *r.per_container.entry(c.clone()).or_default().entry(list[0].2.clone()).or_default() += 1;
        let distinct: BTreeSet<_> = list.iter().map(|(n, t, tier)| (n, t, tier)).collect();
        if distinct.len() > 1 {
            let who: Vec<String> = list.iter().map(|(n, t, tier)| format!("{n} ({tier}, {t})")).collect();
            r.duplicates.push(named(c, &list[0].0, Some(*addr), who.join("; ")));
        }
    }
    for (_, list) in r.per_container.iter_mut() {
        list.retain(|_, n| *n > 0);
    }
    if !partial {
        for c in &ctx.map.containers {
            let mut expected: BTreeMap<u32, &str> = BTreeMap::new();
            for e in ctx.map.symbols(&c.id) {
                if e.is_func() && c.vram.contains(&e.addr) && ctx.map.sdk_at(&c.id, e.addr).is_none() {
                    expected.entry(e.addr).or_insert(&e.name);
                }
            }
            // Function extents the symbol file states (`size:`).
            let sized: Vec<(u32, u32)> = ctx
                .map
                .symbols(&c.id)
                .iter()
                .filter(|e| e.is_func())
                .filter_map(|e| e.size.map(|s| (e.addr, e.addr + s)))
                .collect();
            for (addr, name) in expected {
                if defs.contains_key(&(c.id.clone(), addr)) {
                    continue;
                }
                let owner = sized.iter().find(|(s, e)| *s < addr && addr < *e).map(|(s, _)| *s);
                match owner {
                    Some(o) => r.interior.push(named(&c.id, name, Some(addr), format!("inside the function at {}", hex(o)))),
                    None => r.missing.push(named(&c.id, name, Some(addr), "named in symbol file; no C or lift")),
                }
            }
        }
    }
    r
}

pub(crate) fn references(ctx: &Context, tus: &[TuReport]) -> ReferenceReport {
    let mut r = ReferenceReport::default();
    let mut unresolved: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for tu in tus {
        let Some(obj) = &tu.object else { continue };
        for s in &obj.symbols {
            if s.defined || !matches!(s.kind, SymbolKind::Function | SymbolKind::Data) || s.name.is_empty() {
                continue;
            }
            if RUNTIME_IMPORTS.contains(&s.name.as_str()) {
                *r.resolved.entry("runtime".into()).or_default() += 1;
                continue;
            }
            if HOST_SUPPORT.contains(&s.name.as_str()) && ctx.map.resolve(&tu.container, &s.name).is_none() {
                *r.resolved.entry("host-support".into()).or_default() += 1;
                continue;
            }
            match ctx.map.resolve(&tu.container, &s.name) {
                Some(res) => *r.resolved.entry(format!("{:?}", res.via)).or_default() += 1,
                None => {
                    unresolved.entry((tu.container.clone(), s.name.clone())).or_default().insert(tu.name.clone());
                }
            }
        }
    }
    for ((c, name), users) in unresolved {
        let users: Vec<_> = users.into_iter().collect();
        let more = if users.len() > 3 { format!(" +{}", users.len() - 3) } else { String::new() };
        r.unresolved.push(named(&c, &name, None, format!("used by {}{more}", users[..users.len().min(3)].join(", "))));
    }
    r
}

pub(crate) fn data(ctx: &Context, tus: &[TuReport]) -> DataReport {
    let mut r = DataReport::default();
    let mut placed: BTreeMap<(String, u32), u32> = BTreeMap::new();
    for tu in tus {
        let Some(obj) = &tu.object else { continue };
        for s in &obj.symbols {
            let (true, SymbolKind::Data, Some(def)) = (s.defined, s.kind, s.data) else { continue };
            if s.name.starts_with("__psxw_") {
                continue;
            }
            if s.name.starts_with(".L") || s.name.contains('.') {
                r.anonymous += 1;
                continue;
            }
            let Some(res) = ctx.map.resolve(&tu.container, &s.name) else {
                r.unplaceable.push(named(&tu.container, &s.name, None, format!("defined in {}", tu.name)));
                continue;
            };
            let owner = res.container.clone().unwrap_or_else(|| tu.container.clone());
            *placed.entry((owner.clone(), res.addr)).or_default() += 1;
            if let Some(size) = res.size {
                if size != def.size {
                    r.size_mismatches.push(named(
                        &owner,
                        &s.name,
                        Some(res.addr),
                        format!("clang {} bytes, symbol file {size} ({})", def.size, tu.name),
                    ));
                }
            }
            if obj.segment_of(def).name.starts_with(".bss") {
                r.bss += 1;
                continue;
            }
            r.initialized_checked += 1;
            let (ours, mask, unchecked) = expected_bytes(ctx, obj, &tu.container, def);
            r.unchecked_pointer_fields += unchecked;
            let Some(orig) = ctx.originals.read(&owner, res.addr, ours.len()) else {
                r.mismatches.push(named(&owner, &s.name, Some(res.addr), "no original bytes at this address"));
                continue;
            };
            match (0..ours.len()).find(|&i| mask[i] && ours[i] != orig[i]) {
                None => r.initialized_matched += 1,
                Some(i) => {
                    let w = |b: &[u8]| b[i..(i + 8).min(b.len())].iter().map(|x| format!("{x:02x}")).collect::<String>();
                    r.mismatches.push(named(
                        &owner,
                        &s.name,
                        Some(res.addr),
                        format!("first difference at +{i}: clang {} original {} ({})", w(&ours), w(orig), tu.name),
                    ));
                }
            }
        }
    }
    r.multiply_defined = placed.values().filter(|&&n| n > 1).count() as u32;
    r
}

/// A data symbol's bytes as the original would hold them: data pointers
/// resolved to original addresses, function pointers to MIPS addresses.
/// Pointers to things with no original address are masked out.
fn expected_bytes(ctx: &Context, obj: &WasmObject, container: &str, def: psxw_link::object::DataDef) -> (Vec<u8>, Vec<bool>, u32) {
    let mut bytes = obj.data_bytes(def).to_vec();
    let mut mask = vec![true; bytes.len()];
    let mut unchecked = 0;
    for (off, rel) in obj.relocs_in(def) {
        let off = off as usize;
        let target = &obj.symbols[rel.symbol as usize];
        let value = match rel.kind {
            RelocKind::MemoryAddrI32 => ctx.map.resolve(container, &target.name).map(|t| (t.addr as i64 + rel.addend) as u32),
            RelocKind::TableIndexI32 => ctx.map.resolve(container, &target.name).map(|t| t.addr),
            RelocKind::Other => None,
        };
        let end = (off + 4).min(bytes.len());
        match value {
            Some(v) => bytes[off..end].copy_from_slice(&v.to_le_bytes()[..end - off]),
            None => {
                mask[off..end].iter_mut().for_each(|m| *m = false);
                unchecked += 1;
            }
        }
    }
    (bytes, mask, unchecked)
}

/// For each header, as spelled in `#include`, the includes preceding it in
/// the first translation unit that uses it: the context the decomp itself
/// compiles it in. Headers like the SDK's depend on include order.
pub(crate) fn include_contexts(snap: &Path, map: &psxw_inputs::AddressMap) -> std::collections::HashMap<String, Vec<String>> {
    let mut contexts = std::collections::HashMap::new();
    for tu in map.containers.iter().flat_map(|c| &c.tus) {
        let Ok(text) = std::fs::read_to_string(snap.join(&tu.path)) else { continue };
        let mut before: Vec<String> = Vec::new();
        for line in text.lines() {
            let Some(rest) = line.trim_start().strip_prefix("#include") else { continue };
            let rest = rest.trim_start();
            let close = match rest.chars().next() {
                Some('"') => '"',
                Some('<') => '>',
                _ => continue,
            };
            let Some(end) = rest[1..].find(close) else { continue };
            let inc = rest[1..1 + end].to_string();
            contexts.entry(inc.clone()).or_insert_with(|| before.clone());
            before.push(inc);
        }
    }
    contexts
}

pub(crate) fn layout(
    clang: &psxw_cc::Clang,
    snap: &Path,
    manifest: &psxw_inputs::GameManifest,
    map: &psxw_inputs::AddressMap,
    checkout: &Path,
    out: &Path,
    only: Option<&str>,
) -> anyhow::Result<LayoutReport> {
    use psxw_cc::probe::{Outcome, Reference, probe_header};
    use rayon::prelude::*;

    let tool = |p: &Path| if p.components().count() > 1 { checkout.join(p) } else { p.to_path_buf() };
    let reference = Reference {
        cpp: tool(&manifest.reference.cpp),
        cppflags: manifest.reference.cppflags.clone(),
        cc1: tool(&manifest.reference.cc1),
        cc1flags: manifest.reference.cc1flags.clone(),
    };
    let mut r = LayoutReport {
        reference_cc1_sha256: psxw_inputs::sha256_file(&reference.cc1)?,
        ..Default::default()
    };

    let mut headers: Vec<(String, String)> = Vec::new();
    for dir in &manifest.probe.include_dirs {
        let root = snap.join(dir);
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d)? {
                let p = e?.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "h") {
                    let rel = p.strip_prefix(&root)?.to_string_lossy().into_owned();
                    let name = p.file_name().unwrap_or_default().to_string_lossy();
                    let excluded = manifest.probe.exclude.iter().any(|x| *x == rel || *x == name);
                    if !excluded && only.is_none_or(|s| rel.contains(s)) {
                        headers.push((dir.to_string_lossy().into_owned(), rel));
                    }
                }
            }
        }
    }
    headers.sort();
    eprintln!("census: probing {} headers", headers.len());

    let contexts = include_contexts(snap, map);
    let work = out.join("probe");
    let outcomes: Vec<_> = headers
        .par_iter()
        .map(|(dir, rel)| {
            let prelude = contexts.get(rel).unwrap_or(&manifest.probe.prelude);
            probe_header(clang, &reference, prelude, dir, rel, &work).map(|o| (rel.clone(), o))
        })
        .collect::<anyhow::Result<_>>()?;

    for (header, outcome) in outcomes {
        r.headers += 1;
        match outcome {
            Outcome::Skipped(why) => {
                let unused = if contexts.contains_key(&header) { "" } else { "not included by any TU; " };
                r.headers_skipped.push(named("probe", &header, None, format!("{unused}{why}")))
            }
            Outcome::ClangFailed(why) => r.headers_clang_failed.push(named("probe", &header, None, why)),
            Outcome::Probed { entries, bitfields } => {
                r.bitfields_unchecked += bitfields;
                for e in entries {
                    match (e.reference, e.clang, &e.reference_error, &e.clang_error) {
                        (Some(a), Some(b), None, None) => {
                            r.entries += 1;
                            if a == b {
                                r.matched += 1;
                            } else {
                                r.mismatches.push(named(&header, &e.entry.expr, None, format!("gcc {a}, clang {b}")));
                            }
                        }
                        (_, _, Some(_), _) => r.reference_failures += 1,
                        (_, _, None, Some(err)) => r.clang_only_failures.push(named(&header, &e.entry.expr, None, err.clone())),
                        _ => r.reference_failures += 1,
                    }
                }
            }
        }
    }
    Ok(r)
}
