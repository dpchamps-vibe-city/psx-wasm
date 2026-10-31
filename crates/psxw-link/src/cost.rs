//! Decompiled C's virtual time (D71, TR5 step 3): what each straight-line
//! run of a C function's wasm spends from the safepoint budget, estimated
//! from the original's instruction counts.
//!
//! The original's side is its natural loops ([`LoopTree`], from the
//! lifter's graph of the same function). The wasm's are its `loop`s,
//! which are the source's loops (TR13's barriers and step 1's flags keep
//! clang from unrolling, vectorising or turning them into bulk memory).
//! Paired by nesting and order, each loop and the function outside its
//! loops is a region, whose ratio is the original's instructions in it
//! over the wasm's. Where the shapes differ, the whole function is one
//! region. A segment (the operators between two control instructions)
//! spends its operators × its region's ratio, in the budget's fixed point
//! (psxw-abi `guest::BUDGET_UNIT`).

use wasmparser::Operator;

/// A function's instructions by natural loop: those outside every loop
/// (`insns`), then each outermost loop's, in address order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoopTree {
    pub insns: u32,
    pub loops: Vec<LoopTree>,
}

impl LoopTree {
    pub fn total(&self) -> u64 {
        self.insns as u64 + self.loops.iter().map(LoopTree::total).sum::<u64>()
    }
}

/// A wasm region: a `loop` (or the function), its operators outside the
/// loops inside it, and those.
#[derive(Debug, Default)]
struct Region {
    ops: u64,
    children: Vec<usize>,
}

/// Each segment's spend, by the index of its first operator (in the order
/// the body's operator reader gives them), in budget units. `unit` is the
/// budget an original instruction takes.
pub fn spends(ops: &[Operator], original: &LoopTree, unit: u64) -> Vec<(usize, u32)> {
    let mut regions = vec![Region::default()];
    // The region of each enclosing block: a `loop` opens one.
    let mut frames: Vec<usize> = Vec::new();
    // Segments: (first operator, region, operators counted).
    let mut segs: Vec<(usize, usize, u64)> = Vec::new();
    let mut open = false;
    for (i, op) in ops.iter().enumerate() {
        let here = frames.last().copied().unwrap_or(0);
        if !open {
            segs.push((i, here, 0));
            open = true;
        }
        if counted(op) {
            segs.last_mut().unwrap().2 += 1;
            regions[here].ops += 1;
        }
        match op {
            Operator::Loop { .. } => {
                let r = regions.len();
                regions.push(Region::default());
                regions[here].children.push(r);
                frames.push(r);
            }
            Operator::Block { .. } | Operator::If { .. } | Operator::Try { .. } | Operator::TryTable { .. } => frames.push(here),
            Operator::End => {
                frames.pop();
            }
            _ => {}
        }
        if boundary(op) {
            open = false;
        }
    }
    // A function clang made of nothing (an empty one) spends the
    // original's all at once.
    if regions.iter().all(|r| r.ops == 0) {
        let units = original.total() * unit;
        return if units > 0 { vec![(0, units.min(i32::MAX as u64) as u32)] } else { vec![] };
    }
    // Each region's ratio, as (original instructions, wasm operators).
    let mut ratio = vec![(0u64, 1u64); regions.len()];
    if !pair(&regions, 0, original, &mut ratio) {
        let all = (original.total(), regions.iter().map(|r| r.ops).sum::<u64>().max(1));
        ratio.iter_mut().for_each(|r| *r = all);
    }
    segs.into_iter()
        .filter_map(|(at, r, n)| {
            let (o, w) = ratio[r];
            let units = (2 * n * o * unit + w) / (2 * w);
            (units > 0).then_some((at, units.min(i32::MAX as u64) as u32))
        })
        .collect()
}

/// Pair wasm region `r` with `tree`, recording each region's ratio; false
/// where the shapes differ, or a region with original instructions has no
/// operators of its own to spend them.
fn pair(regions: &[Region], r: usize, tree: &LoopTree, ratio: &mut [(u64, u64)]) -> bool {
    let reg = &regions[r];
    if reg.children.len() != tree.loops.len() || (reg.ops == 0 && tree.insns > 0) {
        return false;
    }
    ratio[r] = (tree.insns as u64, reg.ops.max(1));
    reg.children.iter().zip(&tree.loops).all(|(&c, t)| pair(regions, c, t, ratio))
}

/// Structure, not work: these operators count for nothing.
fn counted(op: &Operator) -> bool {
    !matches!(op, Operator::Block { .. } | Operator::Loop { .. } | Operator::End | Operator::Else | Operator::Nop)
}

/// Control flow: a segment ends after it.
fn boundary(op: &Operator) -> bool {
    matches!(
        op,
        Operator::Block { .. }
            | Operator::Loop { .. }
            | Operator::If { .. }
            | Operator::Else
            | Operator::End
            | Operator::Br { .. }
            | Operator::BrIf { .. }
            | Operator::BrTable { .. }
            | Operator::Return
            | Operator::Unreachable
            | Operator::Try { .. }
            | Operator::TryTable { .. }
            | Operator::Catch { .. }
            | Operator::CatchAll
            | Operator::Delegate { .. }
            | Operator::Throw { .. }
            | Operator::Rethrow { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmparser::BlockType;

    fn tree(insns: u32, loops: Vec<LoopTree>) -> LoopTree {
        LoopTree { insns, loops }
    }

    /// Two operators before a loop of five (a `br_if` among them), one
    /// after: paired, the loop's segments spend its original 8 over its 5
    /// operators, the rest the original 3 over 3; with shapes that differ,
    /// every segment the original 3 over all 8.
    #[test]
    fn regions_pair_by_nesting() {
        let ops = [
            Operator::I32Const { value: 1 },
            Operator::LocalSet { local_index: 0 },
            Operator::Loop { blockty: BlockType::Empty },
            Operator::LocalGet { local_index: 0 },
            Operator::I32Const { value: 1 },
            Operator::I32Sub,
            Operator::BrIf { relative_depth: 0 },
            Operator::LocalGet { local_index: 0 },
            Operator::End,
            Operator::Drop,
            Operator::End,
        ];
        let r = |n: u64, o: u64, w: u64| ((2 * n * o * 16 + w) / (2 * w)) as u32;
        let s = spends(&ops, &tree(3, vec![tree(8, vec![])]), 16);
        assert_eq!(s, vec![(0, 32), (3, r(4, 8, 5)), (7, r(1, 8, 5)), (9, 16)]);
        let s = spends(&ops, &tree(3, vec![]), 16);
        assert_eq!(s, vec![(0, r(2, 3, 8)), (3, r(4, 3, 8)), (7, r(1, 3, 8)), (9, r(1, 3, 8))]);
    }
}
