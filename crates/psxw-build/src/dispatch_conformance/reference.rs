//! The independent reference: a variant's original MIPS program on the
//! pinned emulator (psxw-oracle's Docker target), one runner GUEST_CALL
//! per checkpoint. Memory persists between a variant's calls; each
//! variant's first call writes everything it starts from.
//!
//! Only checkpoints whose original behaviour is in the supported domain
//! run here: the reference stops before a call the port must reject, or a
//! port-only contract (a suspension).

use super::fixture::{Expect, Observation, Outcome, Step, Variant, decode_log};
use anyhow::{Context as _, Result, ensure};
use psxw_oracle::script::{self, Op};
use psxw_oracle::target::Target;
use psxw_oracle::wire;

fn le(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes());
}

/// The runner ops for a variant's reference checkpoints.
pub fn ops(v: &Variant) -> Vec<Op> {
    let mut ops = Vec::new();
    let mut setup = v.boot();
    let log = v.layout.log_range();
    for step in &v.steps {
        match step {
            Step::Write { at, bytes } => setup.push((*at, bytes.clone())),
            Step::Call { entry, args, expect: Expect::Value { .. } } => {
                let mut p = Vec::new();
                le(&mut p, *entry);
                le(&mut p, v.layout.stack);
                for a in args {
                    le(&mut p, *a);
                }
                le(&mut p, setup.len() as u32);
                for (at, bytes) in setup.drain(..) {
                    le(&mut p, at);
                    le(&mut p, bytes.len() as u32);
                    p.extend_from_slice(&bytes);
                    p.resize(wire::pad4(p.len()), 0);
                }
                le(&mut p, 1);
                le(&mut p, log.start);
                le(&mut p, log.len() as u32);
                ops.push(Op { code: wire::OP_GUEST_CALL, payload: p, result_len: 8 + log.len(), keep: true });
            }
            Step::Call { .. } | Step::Resume { .. } => break,
        }
    }
    ops
}

/// Run every variant on the target; per variant, its reference
/// observations or why there are none. Each variant gets batches of its
/// own, so a failed op only skips the rest of its variant.
pub fn run(target: &mut dyn Target, variants: &[&Variant]) -> Result<Vec<Result<Vec<Observation>, String>>> {
    let per: Vec<Vec<Op>> = variants.iter().map(|v| ops(v)).collect();
    let mut batches = Vec::new();
    let mut spans = Vec::new();
    for ops in &per {
        let plan = script::pack(std::slice::from_ref(ops));
        spans.push((batches.len(), plan.slots.iter().map(Vec::len).collect::<Vec<_>>()));
        batches.extend(plan.batches);
    }
    let results = if batches.is_empty() { Vec::new() } else { target.exchange(&batches)? };
    ensure!(results.len() == batches.len(), "the target returned {} results for {} batches", results.len(), batches.len());
    let mut out = Vec::new();
    for ((v, ops), (first, sizes)) in variants.iter().zip(&per).zip(&spans) {
        let mut statuses = Vec::new();
        for (i, n) in sizes.iter().enumerate() {
            statuses.extend(script::split_batch(&results[first + i], *n)?);
        }
        out.push(script::assemble(ops, &statuses).map(|bytes| decode(v, &bytes)).map_err(|e| format!("{e:#}")));
    }
    Ok(out)
}

fn decode(v: &Variant, bytes: &[u8]) -> Vec<Observation> {
    let n = 8 + v.layout.log_range().len();
    bytes
        .chunks(n)
        .map(|c| {
            let w = |i: usize| u32::from_le_bytes([c[i], c[i + 1], c[i + 2], c[i + 3]]);
            Observation { outcome: Outcome::Returned { v0: w(0), v1: w(4) }, log: decode_log(&v.layout, &c[8..]) }
        })
        .collect()
}

/// The emulator target `psxw oracle images build` recorded.
pub fn target(ws: &psxw_inputs::Workspace) -> Result<psxw_oracle::target::DockerTarget> {
    psxw_oracle::target::DockerTarget::open(ws).context("the reference CPU (run `psxw oracle images build`)")
}
