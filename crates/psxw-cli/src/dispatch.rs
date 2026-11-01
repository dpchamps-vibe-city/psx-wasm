//! `psxw conform dispatch [--case ID]`: dispatch conformance
//! (plans/dispatch-conformance.md), independent of any game. Every fixture
//! variant through the production pipeline on Wasm, judged against its
//! closed-form values and a live run of the same original MIPS on the
//! reference CPU. Writes `out/conform/dispatch/report.json` and
//! `summary.txt`; fails on any failed or unresolved variant, including a
//! missing reference.

use anyhow::{Result, bail};
use psxw_build::dispatch_conformance::{self as dc, corpus, reference};
use psxw_inputs::Workspace;
use psxw_oracle::target::Target;

pub fn run(ws: &Workspace, args: &[&str]) -> Result<bool> {
    let case = match args {
        [] => None,
        ["--case", id] => Some(id.to_string()),
        _ => bail!("usage: psxw conform dispatch [--case ID]"),
    };
    let variants = corpus::corpus(case.as_deref());
    if variants.is_empty() {
        bail!("no dispatch variant matches {case:?}");
    }
    // Nothing is skipped: the toolchains and the reference are required.
    ws.clang()?;
    ws.tool("binaryen", "bin/wasm-opt")?;
    eprintln!("dispatch: {} variants", variants.len());
    let wasm = dc::run_wasm(ws, variants);
    let needs: Vec<usize> = (0..wasm.len()).filter(|&i| dc::needs_reference(&wasm[i].variant)).collect();
    // Preserve a report even when the reference is absent or fails. A
    // stale green report must not be the only artifact of a failed run.
    let (refs, reference) = match reference::target(ws) {
        Ok(mut target) => {
            eprintln!("dispatch: {} on the reference ({})", needs.len(), target.id());
            let result = reference::run(&mut target, &needs.iter().map(|&i| &wasm[i].variant).collect::<Vec<_>>());
            let mut provenance = target.template().clone();
            provenance.created = psxw_oracle::golden::now_rfc3339();
            let error = result.as_ref().err().map(|e| format!("{e:#}"));
            let metadata = serde_json::json!({ "target": target.id(), "provenance": provenance, "live": result.is_ok(), "error": error });
            (result.unwrap_or_else(|e| vec![Err(format!("{e:#}")); needs.len()]), metadata)
        }
        Err(e) => {
            let error = format!("{e:#}");
            (vec![Err(error.clone()); needs.len()], serde_json::json!({ "live": false, "error": error }))
        }
    };
    let mut by_index: Vec<Option<Result<Vec<dc::fixture::Observation>, String>>> = (0..wasm.len()).map(|_| None).collect();
    for (i, r) in needs.into_iter().zip(refs) {
        by_index[i] = Some(r);
    }
    let reports: Vec<dc::VariantReport> = wasm.iter().zip(&by_index).map(|(w, r)| dc::report_variant(w, r.as_ref())).collect();
    let report = dc::report(ws, case, reports, reference);
    let dir = ws.out_dir().join("conform").join("dispatch");
    std::fs::create_dir_all(&dir)?;
    let json = dir.join("report.json");
    std::fs::write(&json, serde_json::to_string_pretty(&report)?)?;
    let summary = report.summary();
    std::fs::write(dir.join("summary.txt"), &summary)?;
    print!("{summary}");
    if let Some(c) = &report.selection {
        eprintln!("dispatch: a partial selection ({c}), not the full corpus");
    }
    eprintln!("report: {}", json.display());
    Ok(report.pass)
}
