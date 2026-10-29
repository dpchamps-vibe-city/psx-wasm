//! The analysis joined with one game's link: which of the SDK functions it
//! places need adapters.
//!
//! - **Needed:** placed functions of an adapted library (a chip group's,
//!   or a BIOS-internals library) that reach hardware or BIOS internals,
//!   or are state accessors.
//! - **Adapters:** the needed functions that code staying original calls:
//!   the game's code (calls, function pointers in code or data), or a
//!   placed SDK function outside the needed set. The internals behind
//!   them need none.

use crate::scan::{Scan, is_hardware};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// One placed SDK function and the scan's function it is.
#[derive(Debug, Clone, Serialize)]
pub struct Placed {
    pub container: String,
    pub addr: u32,
    /// The game's name for it.
    pub name: String,
    pub member: String,
    /// The scan's key.
    pub key: String,
    /// What the translator's constant-address facts say it touches (a
    /// subset of what the scan must find).
    pub lift_chips: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Need {
    pub key: String,
    pub name: String,
    pub library: String,
    /// Hardware chips it reaches; empty for a state accessor.
    pub chips: Vec<String>,
    pub bios_internal: bool,
    pub state_accessor: bool,
    /// Called by the game's own code (else only by other SDK code).
    pub from_game: bool,
    /// The runtime has an adapter of this name.
    pub have: bool,
}

/// How the scan classes an adapter the runtime has and the game binds.
#[derive(Debug, Clone, Serialize)]
pub struct Bound {
    pub name: String,
    /// `adapter` (in the set), `internal` (needed, but only adapted
    /// functions call it), `bios-call` (a BIOS call's trampoline, routed
    /// to the runtime), `rider` (reaches nothing itself: it calls other
    /// libraries' functions, which carry it).
    pub class: String,
    pub calls: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GameNeeds {
    pub game: String,
    /// Placed SDK functions the scan knows.
    pub placed: usize,
    /// Placed SDK functions it doesn't (no object of that name).
    pub unmatched: Vec<String>,
    pub needed: usize,
    pub adapters: Vec<Need>,
    /// Needed functions only adapted functions call.
    pub internal: Vec<String>,
    /// Adapters the game binds today, classed.
    pub bound: Vec<Bound>,
    /// Placed functions whose constant-address accesses the scan misses:
    /// (name, chips).
    pub misses: Vec<(String, Vec<String>)>,
}

/// The join: `placed` (the game's SDK functions), `refs` (scan keys the
/// game's own code references), the adapted libraries, the runtime's
/// adapters, and the adapters the game binds today (by name).
pub fn needs(
    game: &str,
    scan: &Scan,
    placed: &[Placed],
    unmatched: Vec<String>,
    refs: &BTreeSet<String>,
    adapted: &BTreeSet<&str>,
    have: &BTreeSet<&str>,
    bound: &BTreeSet<String>,
) -> GameNeeds {
    let reach = scan.reach();
    let bios = |k: &str| reach[k].iter().any(|c| !is_hardware(c));
    let state = scan.state_accessors(&reach, adapted);
    let linked: BTreeSet<&str> = placed.iter().map(|p| p.key.as_str()).collect();
    let lib = |k: &str| scan.functions.get(k).map_or("", |f| f.lib.as_str());
    let needed: BTreeSet<&str> =
        linked.iter().copied().filter(|k| adapted.contains(lib(k)) && (!reach[k].is_empty() || state.contains(k))).collect();
    let mut called: BTreeSet<&str> = BTreeSet::new();
    for k in linked.iter().filter(|k| !needed.contains(*k)) {
        if let Some(f) = scan.functions.get(*k) {
            called.extend(f.calls.iter().map(String::as_str).filter(|c| needed.contains(c)));
        }
    }
    let name_of: BTreeMap<&str, &str> = placed.iter().map(|p| (p.key.as_str(), p.name.as_str())).collect();
    let in_set = |k: &&&str| refs.contains(**k) || called.contains(**k);
    let adapters: Vec<Need> = needed
        .iter()
        .filter(in_set)
        .map(|k| {
            let name = name_of.get(k).copied().unwrap_or(k);
            Need {
                key: k.to_string(),
                name: name.to_string(),
                library: lib(k).to_string(),
                chips: reach[k].iter().filter(|c| is_hardware(c)).cloned().collect(),
                bios_internal: bios(k),
                state_accessor: state.contains(k),
                from_game: refs.contains(*k),
                have: have.contains(name) || have.contains(k),
            }
        })
        .collect();
    let internal = needed.iter().filter(|k| !in_set(k)).map(|k| name_of.get(k).copied().unwrap_or(k).to_string()).collect();
    let by_name: BTreeMap<&str, &str> = placed.iter().map(|p| (p.name.as_str(), p.key.as_str())).collect();
    let bound = bound
        .iter()
        .map(|n| {
            let k = by_name.get(n.as_str()).copied().unwrap_or(n);
            let f = scan.functions.get(k);
            let class = if needed.contains(k) && in_set(&&k) {
                "adapter"
            } else if needed.contains(k) {
                "internal"
            } else if f.is_some_and(|f| !f.bios_calls.is_empty() && f.calls.is_empty()) {
                "bios-call"
            } else {
                "rider"
            };
            Bound { name: n.clone(), class: class.into(), calls: f.map(|f| f.calls.iter().chain(&f.bios_calls).cloned().collect()).unwrap_or_default() }
        })
        .collect();
    let misses = placed
        .iter()
        .filter_map(|p| {
            let missed: Vec<String> = p.lift_chips.iter().filter(|c| !reach[p.key.as_str()].contains(*c)).cloned().collect();
            (!missed.is_empty()).then(|| (p.name.clone(), missed))
        })
        .collect();
    GameNeeds { game: game.into(), placed: linked.len(), unmatched, needed: needed.len(), adapters, internal, bound, misses }
}
