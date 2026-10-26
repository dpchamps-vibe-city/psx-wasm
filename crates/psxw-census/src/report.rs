use crate::{Context, checks};
use psxw_cc::compile::Compiled;
use psxw_link::object::WasmObject;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write;

#[derive(Debug, Serialize)]
pub struct TuReport {
    pub container: String,
    pub name: String,
    pub path: String,
    #[serde(flatten)]
    pub compiled: Compiled,
    #[serde(skip)]
    pub object: Option<WasmObject>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Named {
    pub container: String,
    pub name: String,
    pub addr: Option<String>,
    pub detail: String,
}

#[derive(Debug, Default, Serialize)]
pub struct FunctionReport {
    /// container → tier → count (by address).
    pub per_container: BTreeMap<String, BTreeMap<String, u32>>,
    pub lift_reasons: BTreeMap<String, u32>,
    pub lifted: Vec<Named>,
    /// Defined or lifted, but no address for the name.
    pub unplaceable: Vec<Named>,
    /// One address implemented more than once.
    pub duplicates: Vec<Named>,
    /// Game functions in the symbol files with no implementation.
    pub missing: Vec<Named>,
    /// Symbol-file function names inside another function's extent
    /// (alternate entries or mistyped labels): the linker needs entry stubs
    /// if anything calls them.
    pub interior: Vec<Named>,
    /// Calls whose signature differs from the callee's declaration
    /// (target → thunks); these need o32 adapters at link time.
    pub signature_thunks: BTreeMap<String, u32>,
}

#[derive(Debug, Default, Serialize)]
pub struct ReferenceReport {
    pub resolved: BTreeMap<String, u32>,
    pub unresolved: Vec<Named>,
}

#[derive(Debug, Default, Serialize)]
pub struct DataReport {
    pub initialized_checked: u32,
    pub initialized_matched: u32,
    /// Pointer fields whose target has no original address (string
    /// literals, static locals): excluded from comparison.
    pub unchecked_pointer_fields: u32,
    pub bss: u32,
    /// String literals and function-static variables (no original name).
    pub anonymous: u32,
    /// Same address defined by several TUs (C tentative definitions).
    pub multiply_defined: u32,
    pub mismatches: Vec<Named>,
    pub size_mismatches: Vec<Named>,
    pub unplaceable: Vec<Named>,
}

#[derive(Debug, Default, Serialize)]
pub struct LayoutReport {
    pub reference_cc1_sha256: String,
    pub headers: u32,
    pub headers_skipped: Vec<Named>,
    pub headers_clang_failed: Vec<Named>,
    pub entries: u32,
    pub matched: u32,
    pub bitfields_unchecked: u32,
    pub mismatches: Vec<Named>,
    /// Entries GCC computes but clang rejects.
    pub clang_only_failures: Vec<Named>,
    pub reference_failures: u32,
}

#[derive(Debug, Serialize)]
pub struct Verdict {
    pub pass: bool,
    pub failures: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub game: String,
    pub commit: String,
    pub clang: String,
    pub partial: bool,
    pub verdict: Verdict,
    /// Rule → (TUs it applied in, total applications).
    pub rules: BTreeMap<String, (u32, u32)>,
    pub warnings: BTreeMap<String, u32>,
    pub functions: FunctionReport,
    pub references: ReferenceReport,
    pub data: DataReport,
    pub layout: Option<LayoutReport>,
    pub tus: Vec<TuReport>,
}

impl Report {
    pub fn build(
        game: &str,
        commit: &str,
        clang: &str,
        ctx: &Context,
        tus: Vec<TuReport>,
        layout: Option<LayoutReport>,
        partial: bool,
    ) -> Self {
        let functions = checks::functions(ctx, &tus, partial);
        let references = checks::references(ctx, &tus);
        let data = checks::data(ctx, &tus);
        let mut rules: BTreeMap<String, (u32, u32)> = BTreeMap::new();
        let mut warnings: BTreeMap<String, u32> = BTreeMap::new();
        for tu in &tus {
            for (r, n) in &tu.compiled.rules {
                let e = rules.entry(r.clone()).or_default();
                e.0 += 1;
                e.1 += n;
            }
            for (w, n) in &tu.compiled.warnings {
                *warnings.entry(w.clone()).or_default() += n;
            }
        }

        let mut failures = Vec::new();
        let failed = tus.iter().filter(|t| !t.compiled.ok).count();
        let mut fail = |n: usize, what: &str| {
            if n > 0 {
                failures.push(format!("{n} {what}"));
            }
        };
        fail(failed, "translation units failed to compile");
        fail(functions.unplaceable.len(), "functions without an original address");
        fail(functions.duplicates.len(), "addresses implemented more than once");
        fail(functions.missing.len(), "game functions with no implementation");
        fail(references.unresolved.len(), "unresolved references");
        fail(data.mismatches.len(), "C-defined data differing from the original bytes");
        fail(data.size_mismatches.len(), "data symbols whose size differs from the symbol file");
        fail(data.unplaceable.len(), "data symbols without an original address");
        if let Some(l) = &layout {
            fail(l.mismatches.len(), "layout entries differing between GCC 2.95 and clang");
        }
        let verdict = Verdict { pass: failures.is_empty(), failures };
        Self {
            game: game.into(),
            commit: commit.into(),
            clang: clang.into(),
            partial,
            verdict,
            rules,
            warnings,
            functions,
            references,
            data,
            layout,
            tus,
        }
    }

    pub fn summary(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "psx-wasm census: {} @ {}{}", self.game, &self.commit[..7], if self.partial { " (partial)" } else { "" });
        let _ = writeln!(s, "  clang: {}", self.clang);
        let ok = self.tus.iter().filter(|t| t.compiled.ok).count();
        let _ = writeln!(s, "\nTranslation units: {} compiled, {} failed (of {})", ok, self.tus.len() - ok, self.tus.len());
        for tu in self.tus.iter().filter(|t| !t.compiled.ok).take(10) {
            let first = tu.compiled.errors.first().or(tu.compiled.deferred.first());
            let msg = first.map(|e| format!("{}:{}: {}", e.file, e.line, e.message)).unwrap_or_default();
            let _ = writeln!(s, "  FAILED {}: {}", tu.path, msg);
        }
        if !self.rules.is_empty() {
            let _ = writeln!(s, "  normalization rules (TUs / applications):");
            for (r, (t, n)) in &self.rules {
                let _ = writeln!(s, "    {r:28} {t:5} / {n}");
            }
        }

        let _ = writeln!(s, "\nFunctions (by address):");
        for (c, tiers) in &self.functions.per_container {
            let parts: Vec<String> = tiers.iter().map(|(t, n)| format!("{t} {n}")).collect();
            let _ = writeln!(s, "  {c:8} {}", parts.join(", "));
        }
        if !self.functions.lift_reasons.is_empty() {
            let _ = writeln!(s, "  lift reasons:");
            for (r, n) in &self.functions.lift_reasons {
                let _ = writeln!(s, "    {n:5}  {r}");
            }
        }
        list(&mut s, "unplaceable", &self.functions.unplaceable);
        list(&mut s, "duplicate", &self.functions.duplicates);
        list(&mut s, "missing", &self.functions.missing);

        let resolved: u32 = self.references.resolved.values().sum();
        let via: Vec<String> = self.references.resolved.iter().map(|(k, v)| format!("{k} {v}")).collect();
        let _ = writeln!(
            s,
            "\nReferences: {resolved} resolved ({}), {} unresolved",
            via.join(", "),
            self.references.unresolved.len()
        );
        list(&mut s, "unresolved", &self.references.unresolved);

        let d = &self.data;
        let _ = writeln!(
            s,
            "\nC-defined data: {}/{} initialized symbols match the original bytes ({} pointer fields unchecked); {} bss, {} anonymous, {} multiply defined",
            d.initialized_matched, d.initialized_checked, d.unchecked_pointer_fields, d.bss, d.anonymous, d.multiply_defined
        );
        list(&mut s, "mismatch", &d.mismatches);
        list(&mut s, "size mismatch", &d.size_mismatches);
        list(&mut s, "unplaceable", &d.unplaceable);

        if let Some(l) = &self.layout {
            let _ = writeln!(
                s,
                "\nLayout probe: {} headers ({} skipped, {} clang-failed), {}/{} entries match; {} bitfields unchecked",
                l.headers,
                l.headers_skipped.len(),
                l.headers_clang_failed.len(),
                l.matched,
                l.entries,
                l.bitfields_unchecked
            );
            list(&mut s, "MISMATCH", &l.mismatches);
            list(&mut s, "clang-only failure", &l.clang_only_failures);
            list(&mut s, "clang failed on header", &l.headers_clang_failed);
        }

        if !self.warnings.is_empty() {
            let _ = writeln!(s, "\nclang warnings (portability signals):");
            let mut w: Vec<_> = self.warnings.iter().collect();
            w.sort_by(|a, b| b.1.cmp(a.1));
            for (flag, n) in w.into_iter().take(12) {
                let _ = writeln!(s, "  {n:6}  {flag}");
            }
        }

        let _ = writeln!(s, "\nVERDICT: {}", if self.verdict.pass { "PASS" } else { "FAIL" });
        for f in &self.verdict.failures {
            let _ = writeln!(s, "  - {f}");
        }
        s
    }
}

fn list(s: &mut String, label: &str, items: &[Named]) {
    const SHOWN: usize = 8;
    for n in items.iter().take(SHOWN) {
        let addr = n.addr.as_deref().map(|a| format!(" @ {a}")).unwrap_or_default();
        let _ = writeln!(s, "  {label}: {}:{}{} — {}", n.container, n.name, addr, n.detail);
    }
    if items.len() > SHOWN {
        let _ = writeln!(s, "  ... {} more {label} (see census.json)", items.len() - SHOWN);
    }
}
