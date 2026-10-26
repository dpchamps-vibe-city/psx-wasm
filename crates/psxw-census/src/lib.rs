//! M0 census (docs/design.md, Roadmap): compile every game translation unit
//! at the pinned commit with clang for wasm32 and check the result against
//! the original binary — function coverage, symbol resolution, C-defined
//! data, and (via the layout probe) struct layout against the matching
//! compiler. Read-only: nothing is linked or run.

mod checks;
pub mod report;

use anyhow::{Context as _, Result};
use psxw_cc::Clang;
use psxw_inputs::{AddressMap, Workspace, originals::Originals, snapshot};
use psxw_link::object::WasmObject;
use rayon::prelude::*;
use report::{Report, TuReport};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Options {
    pub game: String,
    /// Only TUs whose path contains this substring.
    pub only: Option<String>,
    pub probe: bool,
    /// Only probe headers whose path contains this substring.
    pub only_header: Option<String>,
}

/// Everything the checks need, loaded once.
pub struct Context {
    pub map: AddressMap,
    pub originals: Originals,
}

pub fn run(ws: &Workspace, opts: &Options) -> Result<(Report, PathBuf)> {
    let manifest = ws.game(&opts.game)?;
    let pin = ws.game_pin(&manifest)?;
    eprintln!("census: {} @ {}", manifest.game.id, pin.short());
    let snap = snapshot::ensure(ws, &manifest.game.input, pin)?;
    let map = AddressMap::load(&snap, &manifest)?;
    let originals = Originals::load(&ws.checkout(pin), &map)?;
    if !originals.missing.is_empty() {
        eprintln!("census: no original bytes for {} (data checks skip them)", originals.missing.join(", "));
    }

    let out_dir = ws.out_dir().join("census").join(&manifest.game.id).join(pin.short());
    let obj_dir = out_dir.join("obj");
    std::fs::create_dir_all(&obj_dir)?;
    let mut clang = Clang::new(
        ws.clang()?,
        snap.clone(),
        manifest.c.include.clone(),
        manifest.c.defines.clone(),
        &out_dir,
    )?;
    clang.soft_float = manifest.reference.cc1flags.iter().any(|f| f == "-msoft-float");

    let tus: Vec<_> = map
        .containers
        .iter()
        .flat_map(|c| c.tus.iter())
        .filter(|t| opts.only.as_deref().is_none_or(|s| t.path.to_string_lossy().contains(s)))
        .cloned()
        .collect();
    eprintln!("census: compiling {} translation units", tus.len());
    let done = AtomicUsize::new(0);
    let total = tus.len();
    let mut tu_reports: Vec<TuReport> = tus
        .par_iter()
        .map(|tu| {
            let work = obj_dir.join(&tu.container);
            std::fs::create_dir_all(&work)?;
            let stem = tu.name.replace('/', "__");
            let compiled = psxw_cc::compile(&clang, &tu.path, &work, &stem, &[], None)
                .with_context(|| format!("compiling {}", tu.path.display()))?;
            let object = match &compiled.object {
                Some(p) => Some(WasmObject::read(p)?),
                None => None,
            };
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 200 == 0 || n == total {
                eprintln!("census: {n}/{total}");
            }
            Ok(TuReport {
                container: tu.container.clone(),
                name: tu.name.clone(),
                path: tu.path.display().to_string(),
                compiled,
                object,
            })
        })
        .collect::<Result<_>>()?;
    tu_reports.sort_by(|a, b| (&a.container, &a.name).cmp(&(&b.container, &b.name)));

    let ctx = Context { map, originals };
    let layout = if opts.probe {
        Some(checks::layout(&clang, &snap, &manifest, &ctx.map, &ws.checkout(pin), &out_dir, opts.only_header.as_deref())?)
    } else {
        None
    };
    let report = Report::build(&manifest.game.id, &pin.commit, &clang.version()?, &ctx, tu_reports, layout, opts.only.is_some());
    let json = out_dir.join("census.json");
    std::fs::write(&json, serde_json::to_string_pretty(&report)?)?;
    Ok((report, json))
}
