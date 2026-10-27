//! `psxw mod writers <game> <symbol>` (plans/M11 task 14): the functions
//! that store to a global, to find what to hook. Read from every
//! function's original instructions, with the translator's register
//! constants (and `$gp` at the executable's value): a store whose base is
//! a known constant hits a known address. Stores through computed pointers
//! are where the analysis loses track; those functions are listed apart,
//! never implied to be writers or not.

use anyhow::{Context as _, Result};
use psxw_inputs::Workspace;
use psxw_lift::decode::Op;
use psxw_modkit::decl::Target;
use std::collections::{BTreeMap, BTreeSet};

pub fn run(ws: &Workspace, game: &str, sym: &str) -> Result<bool> {
    let (prep, table) = psxw_modkit::build::table(ws, game)?;
    let (c, addr, size, what) = table.resolve_data(&Target { symbol: Some(sym.to_string()), ..Default::default() })?;
    let end = addr + size.unwrap_or(4).max(1);
    let gp0 = prep.inputs.exe.gp0;
    let mut writers: BTreeMap<(String, u32), Vec<u32>> = BTreeMap::new();
    let mut lost: BTreeSet<(String, u32)> = BTreeSet::new();
    for t in &prep.translations {
        let gp_fixed = t.facts.gp_writes.is_empty();
        for (n, d) in &t.graph.nodes {
            let i = &d.insn;
            if !i.op.is_store() {
                continue;
            }
            let width = match i.op {
                Op::Sb => 1,
                Op::Sh => 2,
                _ => 4,
            };
            // The stack is the function's own.
            if i.rs == 29 {
                continue;
            }
            let base = t.facts.consts.get(n).and_then(|k| k[i.rs as usize]).or((i.rs == 28 && gp_fixed).then_some(gp0));
            match base {
                Some(b) => {
                    let a = b.wrapping_add(i.simm as u32);
                    // KSEG0/KUSEG/KSEG1 views of the same RAM.
                    let phys = |x: u32| x & 0x1FFF_FFFF;
                    if phys(a) < phys(end) && phys(addr) < phys(a + width) {
                        writers.entry((t.func.container.clone(), t.func.addr)).or_default().push(n.pc());
                    }
                }
                None => {
                    lost.insert((t.func.container.clone(), t.func.addr));
                }
            }
        }
    }
    println!("{what}  {c} 0x{addr:08X}..0x{end:08X}");
    println!("stores to it ({} functions):", writers.len());
    for ((wc, wa), pcs) in &writers {
        let pcs: Vec<String> = pcs.iter().map(|p| format!("0x{p:08X}")).collect();
        println!("  {}  at {}", table.display(wc, *wa), pcs.join(", "));
    }
    let lost_names: Vec<String> = lost.iter().filter(|k| !writers.contains_key(*k)).map(|(lc, la)| table.display(lc, *la)).collect();
    println!(
        "{} more functions store through computed pointers, where this analysis loses track; any of them may write it too",
        lost_names.len()
    );
    let dir = ws.out_dir().join("modkit").join(game);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("writers-{sym}.json"));
    let json = serde_json::json!({
        "symbol": what,
        "container": c,
        "address": format!("0x{addr:08X}"),
        "size": size,
        "writers": writers.iter().map(|((wc, wa), pcs)| serde_json::json!({ "function": table.display(wc, *wa), "container": wc, "address": format!("0x{wa:08X}"), "stores": pcs.iter().map(|p| format!("0x{p:08X}")).collect::<Vec<_>>() })).collect::<Vec<_>>(),
        "lost_track": lost_names,
    });
    std::fs::write(&path, serde_json::to_string_pretty(&json)?).with_context(|| format!("writing {}", path.display()))?;
    eprintln!("full list: {}", path.display());
    Ok(true)
}
