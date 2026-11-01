//! Dispatch conformance on the Wasm side (plans/dispatch-conformance.md):
//! every fixture variant through the production pipeline, its linked and
//! optimized modules judged against the closed-form expectations. The
//! reference CPU joins in `psxw conform dispatch`.
//!
//! `PSXW_DISPATCH_CASE=D01` (or `D01/two-short-prefix`) selects variants.
//! Skips without the pinned toolchains.

use psxw_build::dispatch_conformance::{self as dc, Verdict, corpus};
use psxw_inputs::Workspace;

#[test]
fn dispatch() {
    let Ok(ws) = Workspace::discover() else { return };
    if ws.clang().is_err() || ws.tool("binaryen", "bin/wasm-opt").is_err() {
        eprintln!("skipping: run `psxw toolchain fetch`");
        return;
    }
    let filter = std::env::var("PSXW_DISPATCH_CASE").ok();
    let variants = corpus::corpus(filter.as_deref());
    assert!(!variants.is_empty(), "no variant matches {filter:?}");
    let reports: Vec<_> = dc::run_wasm(&ws, variants).iter().map(|w| dc::report_variant(w, None)).collect();
    let mut failed = Vec::new();
    for r in &reports {
        if r.verdict == Verdict::Failed {
            failed.push(format!("{} ({}):\n    {}\n    dispatch: {}", r.id, r.note, r.problems.join("\n    "), r.dispatch.join(" | ")));
        }
    }
    eprintln!("{} variants, {} failed", reports.len(), failed.len());
    assert!(failed.is_empty(), "{} of {} variants fail:\n{}", failed.len(), reports.len(), failed.join("\n"));
}
