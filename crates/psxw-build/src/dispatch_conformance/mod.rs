//! Dispatch conformance (plans/dispatch-conformance.md): synthetic
//! original-MIPS programs whose overlays share addresses, compiled by the
//! production pipeline, run as Wasm, and compared with closed-form values
//! and the same programs on the reference CPU.
//!
//! The proposed contract under test (not an architectural approval): within the supported execution model,
//! transferring to guest address P preserves the observable behaviour of
//! the code resident at P, whatever the containers are named or however
//! the code arrived; a state outside that model is rejected explicitly
//! before incompatible guest effects.
//!
//! ```text
//! original MIPS + initial state -> reference CPU (psxw-oracle) -> observations
//!              |
//!              +-> discovery -> lift / C -> overlay planning (crate::game)
//!                  -> game-mode link -> wasm-opt -O2 -> Wasm observations
//! ```

pub mod asm;
pub mod corpus;
pub mod fixture;
pub mod pipeline;
pub mod reference;
pub mod wasm;

use anyhow::Result;
use fixture::{Build, Evidence, Expect, Observation, Outcome, Step, Variant};
use psxw_inputs::Workspace;
use rayon::prelude::*;
use serde::Serialize;
use std::path::PathBuf;

/// 2: each variant's dispatch `path` and explicit-binding plan.
/// 3: `binding.native` (native references and their flows) and `native`,
/// the linked module's native table and calls as built.
pub const SCHEMA_VERSION: u32 = 3;

/// A variant's verdict class (docs/conventions.md "Reports").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    /// Agrees with closed-form values (port-only contracts).
    Verified,
    /// Agrees with the emulator: not hardware-verified.
    Provisional,
    /// The evidence is missing or disagrees with itself.
    Unresolved,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct Checkpoint {
    pub step: usize,
    pub expect: String,
    pub linked: Option<Observation>,
    pub optimized: Option<Observation>,
    pub reference: Option<Observation>,
    /// The first difference, if any.
    pub difference: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VariantReport {
    pub id: String,
    pub case: &'static str,
    pub seed: u64,
    /// `execution`, or `safety` when the variant expects a rejection.
    pub kind: &'static str,
    pub note: &'static str,
    pub review_required: Option<&'static str>,
    pub evidence: String,
    pub verdict: Verdict,
    pub problems: Vec<String>,
    pub checkpoints: Vec<Checkpoint>,
    /// What the planner derived: each overlay address's candidates and
    /// their checks, and each function's tier.
    pub dispatch: Vec<String>,
    pub tiers: Vec<String>,
    pub untranslated: Vec<String>,
    /// `explicit` when `[dispatch]` facts bind the variant's placements,
    /// else `legacy` (sampled code words).
    pub path: &'static str,
    /// The explicit-binding plan and what stays legacy.
    pub binding: Option<crate::binding::Report>,
    /// The linked module's native table and calls, as built.
    pub native: Option<pipeline::NativeBuilt>,
    /// sha256 of the fixture as built.
    pub fixture_sha256: String,
}

/// The Wasm side of a variant.
pub struct WasmResult {
    pub variant: Variant,
    pub build: Result<pipeline::Built, String>,
    pub linked: Result<Vec<Observation>, String>,
    pub optimized: Result<Vec<Observation>, String>,
}

/// Where a variant builds.
pub fn work_dir(ws: &Workspace, v: &Variant) -> PathBuf {
    ws.out_dir().join("conform").join("dispatch").join("work").join(v.case).join(v.name.replace('@', "-"))
}

/// Build and run every variant on Wasm, in parallel.
pub fn run_wasm(ws: &Workspace, variants: Vec<Variant>) -> Vec<WasmResult> {
    let engine = wasm::engine();
    variants
        .into_par_iter()
        .map(|variant| {
            let build = pipeline::build(ws, &variant, &work_dir(ws, &variant)).map_err(|e| format!("{e:#}"));
            let run = |which: fn(&pipeline::Built) -> &[u8]| -> Result<Vec<Observation>, String> {
                let b = build.as_ref().map_err(|_| "not built".to_string())?;
                let engine = engine.as_ref().map_err(|e| format!("{e:#}"))?;
                wasm::run(engine, which(b), &variant).map_err(|e| format!("{e:#}"))
            };
            let linked = run(|b| &b.linked);
            let optimized = run(|b| &b.optimized);
            WasmResult { variant, build, linked, optimized }
        })
        .collect()
}

fn expect_text(e: &Expect) -> String {
    match e {
        Expect::Value { v0, v1, log } => {
            let hex = |v: &Option<u32>| v.map_or("-".to_string(), |v| format!("{v:#x}"));
            format!("returns v0={} v1={} log={}", hex(v0), hex(v1), log.as_ref().map_or("-".into(), |l| fmt_log(l)))
        }
        Expect::Reject { diag, log } => format!("rejects `{diag}` log={}", fmt_log(log)),
        Expect::Suspend { log } => format!("suspends log={}", fmt_log(log)),
    }
}

fn fmt_log(l: &[u32]) -> String {
    format!("[{}]", l.iter().map(|w| format!("{w:#x}")).collect::<Vec<_>>().join(" "))
}

fn fmt_obs(o: &Observation) -> String {
    let what = match &o.outcome {
        Outcome::Returned { v0, v1 } => format!("returned v0={v0:#x} v1={v1:#x}"),
        Outcome::Suspended => "suspended".into(),
        Outcome::Rejected { diag } => format!("rejected `{diag}`"),
        Outcome::Failed { why } => format!("failed: {why}"),
    };
    format!("{what} log={}", fmt_log(&o.log))
}

/// What's wrong with a Wasm observation, judged against the expectation.
fn judge(e: &Expect, o: &Observation) -> Option<String> {
    let bad = |want: String| Some(format!("expected {want}, got {}", fmt_obs(o)));
    match (e, &o.outcome) {
        (Expect::Value { v0, v1, log }, Outcome::Returned { v0: g0, v1: g1 }) => {
            let ok = v0.is_none_or(|v| v == *g0) && v1.is_none_or(|v| v == *g1) && log.as_ref().is_none_or(|l| *l == o.log);
            if ok { None } else { bad(expect_text(e)) }
        }
        (Expect::Reject { diag, log }, Outcome::Rejected { diag: got }) if got.starts_with(diag.as_str()) && *log == o.log => None,
        (Expect::Suspend { log }, Outcome::Suspended) if *log == o.log => None,
        _ => bad(expect_text(e)),
    }
}

/// What's wrong with a Wasm observation against the reference's.
fn against_reference(e: &Expect, wasm: &Observation, r: &Observation) -> Option<String> {
    let (Expect::Value { v0, v1, .. }, Outcome::Returned { v0: w0, v1: w1 }, Outcome::Returned { v0: r0, v1: r1 }) =
        (e, &wasm.outcome, &r.outcome)
    else {
        return Some(format!("the reference {}, Wasm {}", fmt_obs(r), fmt_obs(wasm)));
    };
    let ok = wasm.log == r.log && (v0.is_none() || w0 == r0) && (v1.is_none() || w1 == r1);
    (!ok).then(|| format!("the reference {}, Wasm {}", fmt_obs(r), fmt_obs(wasm)))
}

/// Whether the reference itself agrees with the closed-form values.
fn reference_trusted(e: &Expect, r: &Observation) -> bool {
    judge(e, r).is_none()
}

/// Judge a variant from its Wasm runs and (for reference evidence) its
/// reference run.
pub fn report_variant(w: &WasmResult, reference: Option<&Result<Vec<Observation>, String>>) -> VariantReport {
    let v = &w.variant;
    let mut problems = Vec::new();
    let mut unresolved = Vec::new();
    match (&v.build, &w.build) {
        (Build::Links, Err(e)) => problems.push(format!("build failed: {e}")),
        (Build::Rejected(want), Ok(_)) => problems.push(format!("expected the build to reject `{want}`; it linked")),
        (Build::Rejected(want), Err(e)) if !e.contains(want.as_str()) => problems.push(format!("expected the build to reject `{want}`: {e}")),
        _ => {}
    }
    let calls: Vec<(usize, &Expect)> = v.steps.iter().enumerate().filter_map(|(i, s)| s.expect().map(|e| (i, e))).collect();
    let refs = match (&v.evidence, reference) {
        // Its first checkpoint is already outside the original program's
        // supported domain: nothing runs on the reference.
        (Evidence::Reference, _) if !needs_reference(v) => Some(&[][..]),
        (Evidence::Reference, Some(Ok(r))) => Some(r.as_slice()),
        (Evidence::Reference, Some(Err(e))) => {
            unresolved.push(format!("no reference run: {e}"));
            None
        }
        (Evidence::Reference, None) => {
            unresolved.push("the reference wasn't run".into());
            None
        }
        (Evidence::ClosedForm(_), _) => None,
    };
    let mut checkpoints = Vec::new();
    if matches!(v.build, Build::Links) {
        for (k, (step, e)) in calls.iter().enumerate() {
            let get = |r: &Result<Vec<Observation>, String>| r.as_ref().ok().and_then(|o| o.get(k).cloned());
            let (linked, optimized) = (get(&w.linked), get(&w.optimized));
            let r = refs.and_then(|r| r.get(k).cloned());
            let mut difference = None;
            for (which, o) in [("linked", &linked), ("optimized", &optimized)] {
                let d = match o {
                    None => Some("not reached".to_string()),
                    Some(o) => judge(e, o).or_else(|| r.as_ref().and_then(|r| against_reference(e, o, r))),
                };
                if let (None, Some(d)) = (&difference, d) {
                    difference = Some(format!("{which}: {d}"));
                }
            }
            if let (Some(l), Some(o)) = (&linked, &optimized) {
                if l != o && difference.is_none() {
                    difference = Some(format!("the optimized module {} but the linked one {}", fmt_obs(o), fmt_obs(l)));
                }
            }
            if let Some(d) = &difference {
                problems.push(format!("step {step}: {d}"));
            }
            match (&r, e) {
                (Some(r), _) if !reference_trusted(e, r) => {
                    unresolved.push(format!("step {step}: the reference disagrees with the closed form: {}", fmt_obs(r)))
                }
                (None, Expect::Value { .. }) if refs.is_some() => unresolved.push(format!("step {step}: no reference observation")),
                _ => {}
            }
            checkpoints.push(Checkpoint { step: *step, expect: expect_text(e), linked, optimized, reference: r, difference });
        }
        for (which, r) in [("linked", &w.linked), ("optimized", &w.optimized)] {
            if let Err(e) = r {
                problems.push(format!("{which} run: {e}"));
            }
        }
    }
    if let (Some(e), Ok(b)) = (&v.native, &w.build) {
        problems.extend(native_problems(e, b));
    }
    if let Some(why) = v.review_required {
        unresolved.push(why.into());
    }
    let verdict = if !problems.is_empty() {
        Verdict::Failed
    } else if !unresolved.is_empty() {
        Verdict::Unresolved
    } else if !needs_reference(v) {
        Verdict::Verified
    } else {
        Verdict::Provisional
    };
    // A mismatch and missing/unreviewed evidence can coexist. Preserve both.
    problems.extend(unresolved);
    let built = w.build.as_ref().ok();
    VariantReport {
        id: v.id(),
        case: v.case,
        seed: v.seed,
        kind: if v.rejects() { "safety" } else { "execution" },
        note: v.note,
        review_required: v.review_required,
        evidence: match &v.evidence {
            Evidence::Reference if !needs_reference(v) => "port rejection contract (no reference checkpoint)".into(),
            Evidence::Reference => "emulator (provisional, supported checkpoints only)".into(),
            Evidence::ClosedForm(why) => format!("closed form ({why})"),
        },
        verdict,
        problems,
        checkpoints,
        dispatch: built
            .map(|b| {
                b.overlays
                    .iter()
                    .map(|(a, cs)| {
                        let cands: Vec<String> = cs
                            .iter()
                            .map(|(container, checks)| {
                                let ws: Vec<String> = checks.iter().map(|(at, w)| format!("{at:08X}={w:08X}")).collect();
                                format!("{container} [{}]", ws.join(" "))
                            })
                            .collect();
                        format!("0x{a:08X}: {}", cands.join("; "))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        tiers: built.map(|b| b.tiers.iter().map(|(k, t)| format!("{k} {t}")).collect()).unwrap_or_default(),
        untranslated: built.map(|b| b.untranslated.clone()).unwrap_or_default(),
        path: if v.dispatch.is_some() { "explicit" } else { "legacy" },
        binding: built.map(|b| b.binding.clone()),
        native: built.map(|b| b.native.clone()),
        fixture_sha256: psxw_oracle::Hash::of(format!("{v:?}").as_bytes()).hex(),
    }
}

/// What's wrong with a build's native references, judged against the
/// variant's structural expectation: the table's handles, the bodies its
/// entries enter, its calls, and each named flow's representation.
fn native_problems(e: &fixture::NativeExpect, b: &pipeline::Built) -> Vec<String> {
    let mut out = Vec::new();
    let report = &b.binding.native;
    let table = &b.native.table;
    if report.handles.len() != e.handles.len() || table.len() != report.handles.len() + usize::from(!report.handles.is_empty()) {
        out.push(format!("native table: expected {} handles, the report has [{}], the module [{}]", e.handles.len(), report.handles.join(", "), table.join(", ")));
    }
    for (id, kind) in &e.handles {
        let Some(k) = report.handles.iter().position(|h| h.split_once(' ').is_some_and(|(_, i)| i == id || (id.starts_with(':') && i.ends_with(id.as_str())))) else {
            out.push(format!("native table: no handle for {id}"));
            continue;
        };
        let name = table.get(k + 1).map_or("", String::as_str);
        let ok = match *kind {
            "c" | "helper" => name.starts_with("o32:") && !name.starts_with("o32:hook"),
            "lift" => name.contains(":psxw_t_"),
            "operation" => name.starts_with("operation:"),
            _ => false,
        };
        if !ok {
            out.push(format!("native handle {} ({id}) enters `{name}`, not a {kind} body", k + 1));
        }
    }
    if b.native.calls < e.calls || b.native.calls != report.native_calls || (e.calls > 0 && b.native.native_indirect == 0) {
        out.push(format!(
            "expected at least {} native call sites: the module calls its native-call wrappers {} times ({} through the native table), the report counts {}",
            e.calls, b.native.calls, b.native.native_indirect, report.native_calls
        ));
    }
    let site = |f: &psxw_link::native::Flow, func: &str, sym: &str| f.sites.iter().any(|s| s.starts_with(&format!("{func} ")) && s.ends_with(&format!(": &{sym}")));
    for (func, sym) in &e.native {
        if !report.flows.iter().any(|f| f.representation == "native" && site(f, func, sym)) {
            out.push(format!("expected {func}'s reference to {sym} in a native flow"));
        }
    }
    let carried = (b.native.transports, b.native.lifted_native_calls, report.native_transports);
    if carried.0 < e.transports || carried.0 != carried.2 || (e.transports > 0 && carried.1 == 0) {
        out.push(format!(
            "expected at least {} native transports into lifted code: the module calls {} transport wrappers and lifted code calls through the native table {} times, the report counts {}",
            e.transports, carried.0, carried.1, carried.2
        ));
    }
    for (func, k) in &e.carriers {
        if !report.lifted_carriers.iter().any(|l| l.contains(&format!(" {func} a{k}: "))) {
            out.push(format!("expected {func}'s a{k} to carry a native handle; carriers: [{}]", report.lifted_carriers.join(", ")));
        }
    }
    for (func, to) in &e.devirtualized {
        if !report.devirtualized.iter().any(|l| l.contains(&format!(" {func} 0x")) && l.ends_with(&format!("→ 0x{to:08X}"))) {
            out.push(format!("expected {func}'s pointer call to 0x{to:08X} devirtualized; devirtualized: [{}]", report.devirtualized.join(", ")));
        }
    }
    for func in &e.not_devirtualized {
        if let Some(l) = report.devirtualized.iter().find(|l| l.contains(&format!(" {func} 0x"))) {
            out.push(format!("expected {func}'s pointer calls through the dispatch table; {l}"));
        }
    }
    for (func, sym, reason) in &e.retained {
        let found = report.flows.iter().find(|f| site(f, func, sym));
        match found {
            Some(f) if f.representation == "guest" && f.reason.map(|r| r.name()) == Some(*reason) => {}
            Some(f) => out.push(format!("expected {func}'s reference to {sym} kept as guest ({reason}); it is {} ({:?})", f.representation, f.detail)),
            None => out.push(format!("expected {func}'s reference to {sym} kept as guest ({reason}); no flow has it")),
        }
    }
    out
}

/// Whether a variant has supported checkpoints to run on the reference.
/// Rejection checkpoints are port-contract assertions, not console faults.
pub fn needs_reference(v: &Variant) -> bool {
    matches!(v.evidence, Evidence::Reference) && v.steps.iter().any(|s| matches!(s, Step::Call { expect: Expect::Value { .. }, .. }))
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Counts {
    /// Positive variants agreeing with the emulator (provisional) or with
    /// closed-form values (verified).
    pub execution: usize,
    /// Rejection variants that rejected as specified (safety coverage, not
    /// execution of the original program).
    pub safety: usize,
    pub failed: usize,
    pub unresolved: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub pass: bool,
    /// The variants selected (`--case`), or all.
    pub selection: Option<String>,
    pub counts: Counts,
    /// The compiler under test: commit, and whether the tree had changes.
    pub compiler: String,
    /// The pinned toolchains it compiled with.
    pub toolchain: Vec<String>,
    /// The reference's target id and provenance, or why there is none.
    pub reference: serde_json::Value,
    pub variants: Vec<VariantReport>,
}

impl Report {
    pub fn summary(&self) -> String {
        let mut s = String::new();
        for v in &self.variants {
            let verdict = match v.verdict {
                Verdict::Verified => "verified",
                Verdict::Provisional => "provisional",
                Verdict::Unresolved => "UNRESOLVED",
                Verdict::Failed => "FAIL",
            };
            s += &format!("{verdict:<11} {:<9} {:<8} {}\n", v.kind, v.path, v.id);
            for p in &v.problems {
                s += &format!("            {p}\n");
            }
        }
        if let Some(error) = self.reference.get("error").and_then(|e| e.as_str()) {
            s += &format!("reference unavailable: {error}\n");
        }
        let c = &self.counts;
        s += &format!(
            "dispatch: {} — {} execution, {} safety (expected rejections), {} failed, {} unresolved\n",
            if self.pass { "pass" } else { "FAIL" },
            c.execution,
            c.safety,
            c.failed,
            c.unresolved
        );
        s
    }
}

/// Count and wrap variant reports.
pub fn report(ws: &Workspace, selection: Option<String>, variants: Vec<VariantReport>, reference: serde_json::Value) -> Report {
    let mut counts = Counts::default();
    for v in &variants {
        match (v.verdict, v.kind) {
            (Verdict::Failed, _) => counts.failed += 1,
            (Verdict::Unresolved, _) => counts.unresolved += 1,
            (_, "safety") => counts.safety += 1,
            _ => counts.execution += 1,
        }
    }
    let git = |args: &[&str]| {
        std::process::Command::new("git").current_dir(&ws.root).args(args).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    let compiler = format!("{}{}", git(&["rev-parse", "HEAD"]).unwrap_or_default(), if dirty { " (with uncommitted changes)" } else { "" });
    let toolchain = ["clang", "binaryen"].iter().filter_map(|t| ws.lock.toolchain.get(*t).map(|p| format!("{t} {} sha256:{}", p.dist, p.sha256))).collect();
    let pass = !variants.is_empty() && counts.failed == 0 && counts.unresolved == 0
        && reference.get("live").and_then(|v| v.as_bool()) == Some(true);
    Report { schema_version: SCHEMA_VERSION, pass, selection, counts, compiler, toolchain, reference, variants }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reporting_preserves_failures_and_review_requirements() {
        let variant = corpus::corpus(Some("D10/nothing-resident")).remove(0);
        let Expect::Reject { diag, log } = variant.steps[0].expect().unwrap() else { panic!("rejection fixture") };
        let observations = vec![Observation { outcome: Outcome::Rejected { diag: diag.clone() }, log: log.clone() }];
        let mut w = WasmResult {
            variant,
            build: Ok(pipeline::Built {
                linked: vec![], optimized: vec![], overlays: Default::default(),
                tiers: Default::default(), untranslated: vec![], binding: Default::default(), native: Default::default(),
            }),
            linked: Ok(observations.clone()), optimized: Ok(observations),
        };
        assert_eq!(report_variant(&w, None).verdict, Verdict::Verified);
        w.variant.review_required = Some("operator review required");
        assert_eq!(report_variant(&w, None).verdict, Verdict::Unresolved);
        w.linked = Ok(vec![Observation { outcome: Outcome::Returned { v0: 1, v1: 0 }, log: vec![1] }]);
        let r = report_variant(&w, None);
        assert_eq!(r.verdict, Verdict::Failed, "review must not hide a mismatch");
        assert!(r.problems.iter().any(|p| p.contains("got returned")));
        assert!(r.problems.iter().any(|p| p == "operator review required"));
        w.variant = corpus::corpus(Some("D01/two-long-prefix")).remove(0);
        let r = report_variant(&w, None);
        assert_eq!(r.verdict, Verdict::Failed);
        assert!(r.problems.iter().any(|p| p == "the reference wasn't run"), "keep missing evidence alongside mismatches");
    }
}
