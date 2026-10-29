//! `psxw sdk scan`: the hardware-reach analysis of an SDK's library objects,
//! joined with each game's link (psxw-sdk; plans/unified-adaptor-layer
//! phase 1 task 2).
//!
//! ```text
//! psxw sdk scan [--sdk psyq-470] [--objects DIR] [--game G]…
//! ```
//!
//! Writes `out/sdk/<sdk>/scan.json` (every function's facts) and
//! `out/sdk/<sdk>/<game>.json` (the game's needed adapters), and prints,
//! per library, the adapters each game needs, which exist, and the union.

use anyhow::{Context, Result, bail};
use psxw_inputs::Workspace;
use psxw_inputs::boundary::Boundary;
use psxw_sdk::game::GameNeeds;
use std::collections::{BTreeMap, BTreeSet};

/// `psxw sdk disasm <function>… [--sdk S]`: functions of the SDK's objects,
/// disassembled with their relocations.
pub fn disasm(ws: &Workspace, args: &[&str]) -> Result<()> {
    let mut sdk = "psyq-470".to_string();
    let mut names = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--sdk" => sdk = it.next().context("--sdk needs a value")?.to_string(),
            n => names.push(n.to_string()),
        }
    }
    let members = psxw_sdk::scan::load(&psxw_sdk::objects_dir(ws, &sdk)?)?;
    let scan = psxw_sdk::scan::scan(&members);
    for n in names {
        print!("{}", psxw_sdk::disasm(&members, &scan, &n)?);
    }
    Ok(())
}

pub fn run(ws: &Workspace, args: &[&str]) -> Result<bool> {
    let mut sdk = "psyq-470".to_string();
    let mut objects = None;
    let mut games: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--sdk" => sdk = it.next().context("--sdk needs a value")?.to_string(),
            "--objects" => objects = Some(std::path::PathBuf::from(it.next().context("--objects needs a directory")?)),
            "--game" => games.push(it.next().context("--game needs a value")?.to_string()),
            other => bail!("unknown sdk scan option {other}"),
        }
    }
    if games.is_empty() {
        games = ws.lock.game.keys().filter(|g| ws.game(g).is_ok_and(|m| m.game.sdk.as_deref() == Some(sdk.as_str()))).cloned().collect();
    }
    let dir = match objects {
        Some(d) => d,
        None => psxw_sdk::objects_dir(ws, &sdk)?,
    };
    eprintln!("sdk scan: {}", dir.display());
    let members = psxw_sdk::scan::load(&dir)?;
    let scan = psxw_sdk::scan::scan(&members);
    let reach = scan.reach();
    let hw = reach.values().filter(|r| !r.is_empty()).count();
    eprintln!("sdk scan: {} objects, {} functions, {hw} reach hardware, {} register pointers", scan.objects, scan.functions.len(), scan.pointers.len());
    let out = ws.out_dir().join("sdk").join(&sdk);
    std::fs::create_dir_all(&out)?;
    std::fs::write(out.join("scan.json"), serde_json::to_string_pretty(&scan)?)?;

    let have: Vec<&str> = sdk_psyq::adapters().iter().map(|(n, _)| *n).collect();
    let have: BTreeSet<&str> = have.into_iter().collect();
    let mut all: Vec<GameNeeds> = Vec::new();
    let mut ok = true;
    for g in &games {
        let manifest = ws.game(g)?;
        let boundary = match &manifest.game.boundary {
            Some(p) => Boundary::load(&ws.root.join(p))?,
            None => Boundary::default(),
        };
        let adapted = psxw_sdk::adapted_libraries(&boundary);
        let adapted: BTreeSet<&str> = adapted.iter().map(String::as_str).collect();
        let (placed, unmatched, refs) = psxw_sdk::join(ws, g, &scan)?;
        let bound = bound_adapters(ws, g);
        let needs = psxw_sdk::game::needs(g, &scan, &placed, unmatched, &refs, &adapted, &have, &bound);
        eprintln!(
            "sdk scan: {g}: {} placed SDK functions ({} unmatched), {} need adapting, {} adapters ({} exist)",
            needs.placed,
            needs.unmatched.len(),
            needs.needed,
            needs.adapters.len(),
            needs.adapters.iter().filter(|n| n.have).count()
        );
        // The checks: the scan finds every access the translator's
        // constant-address facts see, and classes every adapter the game
        // binds today.
        if !needs.misses.is_empty() {
            eprintln!("sdk scan: {g}: accesses the scan misses: {:?}", needs.misses);
            ok = false;
        }
        let mut classes: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for b in &needs.bound {
            classes.entry(&b.class).or_default().push(&b.name);
        }
        for (c, names) in &classes {
            eprintln!("sdk scan: {g}: bound adapters, {c}: {}", names.join(", "));
        }
        std::fs::write(out.join(format!("{g}.json")), serde_json::to_string_pretty(&needs)?)?;
        all.push(needs);
    }
    print!("{}", table(&all));
    Ok(ok)
}

/// The adapters a game's last build bound (`tiers.json`).
fn bound_adapters(ws: &Workspace, game: &str) -> BTreeSet<String> {
    let path = ws.out_dir().join(game).join("tiers.json");
    let Ok(text) = std::fs::read_to_string(path) else { return BTreeSet::new() };
    let rows: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap_or_default();
    rows.iter().filter(|r| r["tier"] == "adapter").filter_map(|r| r["name"].as_str().map(str::to_string)).collect()
}

/// Per library: adapters per game, the union, how many exist, and the
/// missing ones by name.
fn table(all: &[GameNeeds]) -> String {
    let mut libs: BTreeMap<&str, BTreeMap<&str, (bool, bool, BTreeSet<&str>)>> = BTreeMap::new();
    for g in all {
        for n in &g.adapters {
            let e = libs.entry(&n.library).or_default().entry(&n.name).or_insert((n.have, n.state_accessor, BTreeSet::new()));
            e.2.insert(&g.game);
        }
    }
    let mut s = String::from("| Library |");
    for g in all {
        s += &format!(" {} |", g.game);
    }
    s += " Need | Have | State accessors | Missing |\n|---|";
    s += &"---|".repeat(all.len() + 4);
    s += "\n";
    let mut total = (vec![0usize; all.len()], 0usize, 0usize);
    for (lib, fs) in &libs {
        s += &format!("| {lib} |");
        for (i, g) in all.iter().enumerate() {
            let n = fs.values().filter(|e| e.2.contains(g.game.as_str())).count();
            total.0[i] += n;
            s += &format!(" {n} |");
        }
        let have = fs.values().filter(|e| e.0).count();
        let state = fs.values().filter(|e| e.1).count();
        total.1 += fs.len();
        total.2 += have;
        let missing: Vec<&str> = fs.iter().filter(|(_, e)| !e.0).map(|(n, _)| *n).collect();
        s += &format!(" {} | {have} | {state} | {} |\n", fs.len(), missing.join(", "));
    }
    s += "| **Total** |";
    for n in &total.0 {
        s += &format!(" **{n}** |");
    }
    s += &format!(" **{}** | **{}** | | |\n", total.1, total.2);
    s
}
