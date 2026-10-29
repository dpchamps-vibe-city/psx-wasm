//! `psxw sdk scan`: which SDK functions reach hardware, and which of them
//! each game needs adapters for (plans/unified-adaptor-layer, phase 1
//! task 2; docs/design.md D59).
//!
//! Input: an SDK version's relocatable library objects, one directory per
//! library (psyq-decomp's `library-input/redux` for PsyQ 4.7). The scan
//! ([`scan`]) is per function; the join with a game ([`game`]) uses the
//! game's own link: its placed SDK members, and the calls and function
//! pointers in its code and data.

pub mod elf;
pub mod game;
pub mod scan;

use anyhow::{Context, Result};
use psxw_inputs::Workspace;
use psxw_inputs::boundary::Boundary;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Where an SDK's library objects are: `library-input/redux` in the SDK's
/// pinned repo (inputs.lock `[sdk.<id>]`). They are the repo's converted
/// originals, generated in its checkout, not tracked.
pub fn objects_dir(ws: &Workspace, sdk: &str) -> Result<PathBuf> {
    let pin = ws.lock.sdk.get(sdk).with_context(|| format!("inputs.lock has no [sdk.{sdk}]"))?;
    Ok(ws.checkout(pin).join("library-input").join("redux"))
}

/// A function's disassembly, by the scan's key (`SpuSetKey`,
/// `libspu/s_sk:0x1ac`): each instruction with the relocation that
/// patches it.
pub fn disasm(objs: &[scan::Member], s: &scan::Scan, key: &str) -> Result<String> {
    let f = s.functions.get(key).with_context(|| format!("no function {key}"))?;
    let m = objs.iter().find(|m| m.lib == f.lib && m.name == f.member).context("no object")?;
    let o = &m.obj;
    let starts: BTreeSet<u32> = s.functions.values().filter(|g| g.lib == f.lib && g.member == f.member && g.section == f.section).map(|g| g.off).collect();
    let end = starts.range(f.off + 1..).next().copied().unwrap_or(o.data(f.section).len() as u32);
    let rel: BTreeMap<u32, &elf::Reloc> = o.relocs.get(&f.section).into_iter().flatten().map(|r| (r.offset, r)).collect();
    let mut out = format!("{key} ({}/{}, {} +{:#x}):\n", f.lib, f.member, o.sections[f.section].name, f.off);
    for pc in (f.off..end).step_by(4) {
        let w = o.word(f.section, pc);
        let insn = psxw_lift::decode::decode(w);
        out += &format!("  {pc:5x}: {w:08x}  {}", psxw_lift::decode::disasm(&insn, pc));
        if let Some(r) = rel.get(&pc) {
            let sym = &o.symbols[r.sym as usize];
            let name = if sym.ty == elf::STT_SECTION { o.sections[sym.shndx as usize].name.clone() } else { sym.name.clone() };
            let ty = match r.ty {
                elf::R_MIPS_32 => "32",
                elf::R_MIPS_26 => "26",
                elf::R_MIPS_HI16 => "hi16",
                elf::R_MIPS_LO16 => "lo16",
                _ => "?",
            };
            out += &format!("    ; {ty} {name}");
        }
        out += "\n";
    }
    Ok(out)
}

/// The libraries whose functions get adapters: every chip group's, and
/// those with policy `adapter` (BIOS internals).
pub fn adapted_libraries(b: &Boundary) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = b.chips().values().flat_map(|c| c.members.iter()).map(|m| m.split('/').next().unwrap_or(m).to_string()).collect();
    out.extend(b.adapter_libraries());
    out
}

/// A game's placed SDK functions matched to the scan, the names it
/// doesn't match, and the scan keys its own code references.
pub fn join(ws: &Workspace, game: &str, scan: &scan::Scan) -> Result<(Vec<game::Placed>, Vec<String>, BTreeSet<String>)> {
    let mut inputs = psxw_lift::run::load(ws, game)?;
    let (translations, ..) = psxw_lift::run::translate(&mut inputs);
    // The scan's functions per (library, member): by offset in the
    // member's text, and by name.
    let mut by_member: BTreeMap<(&str, &str), BTreeMap<u32, &str>> = BTreeMap::new();
    let mut by_name: BTreeMap<&str, &str> = BTreeMap::new();
    for (k, f) in &scan.functions {
        by_member.entry((&f.lib, &f.member)).or_default().insert(f.off, k);
        by_name.insert(k.rsplit(':').next().unwrap_or(k), k);
    }
    let mut placed = Vec::new();
    let mut unmatched = Vec::new();
    for t in &translations {
        let Some(p) = inputs.map.sdk_at(&t.func.container, t.func.addr).filter(|p| p.is_text()) else { continue };
        let (lib, member) = p.member.split_once('/').unwrap_or((&p.member, ""));
        let funcs = by_member.get(&(lib, member));
        let key = by_name
            .get(t.func.name.as_str())
            .filter(|k| scan.functions.get(**k).is_some_and(|f| f.lib == lib && f.member == member))
            .or_else(|| funcs?.get(&(t.func.addr - p.vram.start)))
            .copied();
        match key {
            Some(k) => placed.push(game::Placed {
                container: t.func.container.clone(),
                addr: t.func.addr,
                name: t.func.name.clone(),
                member: p.member.clone(),
                key: k.to_string(),
                lift_chips: t
                    .facts
                    .io
                    .iter()
                    .filter_map(|(_, a)| scan::chip(*a))
                    .chain((!t.facts.kernel.is_empty()).then_some("kernel"))
                    .map(str::to_string)
                    .collect(),
            }),
            None => unmatched.push(format!("{} ({})", t.func.name, p.member)),
        }
    }
    // References from the game's own code: direct calls and code
    // constants, then words in its data, outside the SDK's placements.
    let at: BTreeMap<(&str, u32), &str> = placed.iter().map(|p| ((p.container.as_str(), p.addr), p.key.as_str())).collect();
    let main = inputs.map.main.clone();
    let find = |c: &str, a: u32| at.get(&(c, a)).or_else(|| at.get(&(main.as_str(), a))).map(|k| k.to_string());
    let mut refs = BTreeSet::new();
    for t in translations.iter().filter(|t| inputs.map.sdk_at(&t.func.container, t.func.addr).is_none()) {
        for a in t.facts.call_sites.iter().map(|c| c.1).chain(t.facts.code_consts.iter().copied()) {
            refs.extend(find(&t.func.container, a));
        }
    }
    for c in &inputs.map.containers {
        for (file, vram) in inputs.originals.segments(&c.id) {
            let Some(bytes) = inputs.originals.read(&c.id, *vram, (file.end - file.start) as usize) else { continue };
            for (i, w) in bytes.chunks_exact(4).enumerate() {
                let addr = vram + 4 * i as u32;
                if c.sdk.iter().any(|p| p.vram.contains(&addr)) {
                    continue;
                }
                refs.extend(find(&c.id, u32::from_le_bytes([w[0], w[1], w[2], w[3]])));
            }
        }
    }
    Ok((placed, unmatched, refs))
}
