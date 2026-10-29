//! The oracle commands behind `psxw oracle …`: selftest, capture, check
//! and the CI gate. Each writes a report under `out/oracle/` and returns it;
//! the CLI turns `pass == false` into a non-zero exit.

use crate::exact;
use crate::golden::{EXACT_TARGET, Provenance};
use crate::gpu_synth;
use crate::gte;
use crate::handwritten;
use crate::images;
use crate::record::game_set;
use crate::report::{Entry, Report, first_difference};
use crate::spu;
use crate::store::Store;
use crate::target::{DockerTarget, Target, emulator_target_id, run_vectors};
use crate::vector::{Kind, Vector, VectorId};
use crate::verdict::{Reference, Verdict, emulator_weak, judge};
use anyhow::{Context, Result, bail, ensure};
use psxw_inputs::Workspace;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The selftest's default seed ("PSXW", "M3", 1).
pub const DEFAULT_SEED: u64 = 0x5053_5857_4D33_0001;
/// Exit criterion 2 of M3: at least this many generated vectors.
pub const MIN_SELFTEST: usize = 1000;
/// Default selftest size: 150 per generator.
pub const DEFAULT_SELFTEST: usize = 150 * exact::GENERATORS.len();
pub const SELFTEST_SET: &str = "selftest";
/// Prefix of the sets synthetic generators fill (`synthetic-gte-commands`,
/// …). `check` includes every such set.
pub const SYNTHETIC_PREFIX: &str = "synthetic-";
/// Devices whose milestone gate also fails on unresolved vectors (no
/// golden): M4 exit criterion 1 wants every GTE vector matched, M6 exit
/// criterion 1 every SPU vector, M13 task 14 every MDEC and CD controller
/// vector.
pub const UNRESOLVED_FAILS: [&str; 4] = ["gte", "spu", "mdec", "cdc"];

/// Store the synthetic GTE vectors (default seed); the note for reports.
fn install_gte(store: &Store) -> Result<String> {
    let counts = gte::install(store, gte::DEFAULT_SEED)?;
    let total: usize = counts.values().sum();
    let parts: Vec<String> = counts.iter().map(|(g, n)| format!("{g} {n}")).collect();
    Ok(format!("synthetic GTE vectors: {total} (seed {:#018x}; {})", gte::DEFAULT_SEED, parts.join(", ")))
}

/// Store the XA vectors for the runner disc's test file; the note for
/// reports.
fn install_xa(ws: &Workspace, store: &Store) -> Result<String> {
    Ok(match images::load(ws)?.runner.xa_lba {
        Some(lba) => format!("XA vectors: {} (test file at LBA {lba})", crate::xa_disc::install(store, lba)?.len()),
        None => "XA vectors: none (the runner disc has no XA test file; run `psxw oracle images build`)".into(),
    })
}

/// Store the MDEC and thread vectors; the note for reports.
fn install_mdec_threads(store: &Store) -> Result<String> {
    let mdec = crate::mdec::install(store)?.len();
    let threads = crate::threads::install(store)?.len();
    Ok(format!("MDEC vectors: {mdec}; BIOS thread vectors: {threads}"))
}

/// Store the CD controller vectors for the runner disc's test file; the
/// note for reports.
fn install_cdc(ws: &Workspace, store: &Store) -> Result<String> {
    let im = images::load(ws)?;
    let bin = ws.root.join(&im.runner.dir).join("psxw-runner.bin");
    Ok(match crate::cdc::install(store, &bin)? {
        Some((lba, ids)) => format!("CD controller vectors: {} (test file at LBA {lba})", ids.len()),
        None => format!("CD controller vectors: none ({} is not on the runner disc; run `psxw oracle images build`)", crate::cdc::NAME),
    })
}

/// Store the synthetic SPU vectors (default seed); the note for reports.
fn install_spu(store: &Store) -> Result<String> {
    let counts = spu::install(store, spu::DEFAULT_SEED)?;
    let total: usize = counts.values().map(|c| c.0).sum();
    let dropped: usize = counts.values().map(|c| c.1).sum();
    let parts: Vec<String> = counts.iter().map(|(g, (n, _))| format!("{g} {n}")).collect();
    Ok(format!(
        "synthetic SPU vectors: {total} (seed {:#018x}; {}); {dropped} candidates dropped as timing-sensitive",
        spu::DEFAULT_SEED,
        parts.join(", ")
    ))
}

pub fn store(ws: &Workspace) -> Store {
    Store::new(images::oracle_dir(ws))
}

fn report_dir(ws: &Workspace, parts: &[&str]) -> PathBuf {
    parts.iter().fold(ws.out_dir().join("oracle"), |p, s| p.join(s))
}

fn load(store: &Store, ids: impl IntoIterator<Item = VectorId>) -> Result<Vec<(VectorId, Vector)>> {
    ids.into_iter().map(|id| Ok((id, store.vector(&id)?))).collect()
}

/// Which set each vector belongs to (first by name), for report groups.
fn set_index(store: &Store) -> Result<BTreeMap<VectorId, String>> {
    let mut out = BTreeMap::new();
    for name in store.set_names()? {
        for id in store.set(&name)? {
            out.entry(id).or_insert_with(|| name.clone());
        }
    }
    Ok(out)
}

/// `psxw oracle selftest`: generate exact-by-construction GPU vectors,
/// capture them through the emulator, and require the closed-form result.
pub fn selftest(ws: &Workspace, seed: u64, count: usize) -> Result<(Report, PathBuf)> {
    let store = store(ws);
    eprintln!("selftest: generating {count} vectors (seed {seed:#018x})");
    let generated = exact::generate(&store, seed, count)?;
    let mut vectors = Vec::with_capacity(generated.len());
    for (i, g) in generated.iter().enumerate() {
        let id = store.put_vector(&g.vector)?;
        match store.golden(EXACT_TARGET, &id)? {
            Some((old, _)) => ensure!(old == g.expected, "generator {} disagrees with its own earlier closed form for {id:?}", g.generator),
            None => {
                let label = format!("{} seed={seed:#018x} index={i}", g.generator);
                store.put_golden(&id, &g.expected, &Provenance::exact(&id, &g.expected, &label, g.vector.kind().name()))?;
            }
        }
        vectors.push((id, g.vector.clone()));
    }
    store.add_to_set(SELFTEST_SET, vectors.iter().map(|(id, _)| *id))?;

    let mut target = DockerTarget::open(ws)?;
    eprintln!("selftest: running on {}", target.id());
    let results = run_vectors(&mut target, &store, &vectors)?;

    let mut report = Report::new("selftest");
    report.target = Some(target.id().to_string());
    report.seed = Some(format!("{seed:#018x}"));
    let mut modes: BTreeMap<String, usize> = BTreeMap::new();
    for ((id, v), (g, res)) in vectors.iter().zip(generated.iter().zip(results)) {
        if let Vector::GpuDraw(d) = v {
            *modes.entry(format!("{:?}", d.mode).to_lowercase()).or_default() += 1;
        }
        let (verdict, detail) = match res {
            Err(e) => (Verdict::Failed, Some(e)),
            Ok(got) => {
                let (mut verdict, _) = judge(v.kind(), &got, Some(&g.expected), None, None);
                let mut detail = (verdict == Verdict::Failed).then(|| first_difference(&got, &g.expected));
                match store.golden(target.id(), id)? {
                    Some((old, _)) if old != got => {
                        verdict = Verdict::Failed;
                        detail = Some(format!("differs from this target's earlier capture: {}", first_difference(&got, &old)));
                    }
                    Some(_) => {}
                    None => store.put_golden(id, &got, &target.template().for_result(id, v.kind().name(), &got))?,
                }
                (verdict, detail)
            }
        };
        report.add(Entry { vector: id.hex(), kind: v.kind().name().into(), group: g.generator.into(), verdict, reference: "exact".into(), detail });
    }
    report.notes.push(format!(
        "GP0 transport: {}",
        modes.iter().map(|(m, n)| format!("{m} {n}")).collect::<Vec<_>>().join(", ")
    ));
    if count < MIN_SELFTEST {
        report.pass = false;
        report.notes.push(format!("{count} vectors; the M3 gate needs at least {MIN_SELFTEST}"));
    }
    if report.count(Verdict::Verified) != count {
        report.pass = false;
    }
    let path = report.write(&report_dir(ws, &["selftest"]))?;
    Ok((report, path))
}

/// `psxw oracle capture`: run vectors through the target and store their
/// goldens. With `missing`, only vectors without a golden for this target;
/// otherwise every vector, and an existing golden must be reproduced byte
/// for byte (emulator determinism).
pub fn capture(ws: &Workspace, kind: Option<Kind>, missing: bool) -> Result<(Report, PathBuf)> {
    let store = store(ws);
    handwritten::install(&store)?;
    let gte_note = install_gte(&store)?;
    let xa_note = install_xa(ws, &store)?;
    let cdc_note = install_cdc(ws, &store)?;
    let mdec_note = install_mdec_threads(&store)?;
    let spu_note = if kind.is_none_or(|k| k == Kind::SpuRun) { Some(install_spu(&store)?) } else { None };
    let synthetic = gpu_synth::install(&store)?;
    let mut target = DockerTarget::open(ws)?;
    let tid = target.id().to_string();
    // The archive is what the sets list: a vector no set names is left
    // over from an earlier version of its generator.
    let groups = set_index(&store)?;
    let ids: Vec<VectorId> = store.vector_ids()?.into_iter().filter(|id| groups.contains_key(id)).collect();
    let mut vectors = Vec::new();
    for (id, v) in load(&store, ids)? {
        if kind.is_some_and(|k| k != v.kind()) || (missing && store.has_golden(&tid, &id)) {
            continue;
        }
        vectors.push((id, v));
    }
    eprintln!("capture: {} vectors on {tid}", vectors.len());
    let results = run_vectors(&mut target, &store, &vectors)?;
    let mut report = Report::new("capture");
    report.target = Some(tid.clone());
    report.seed = Some(format!("{:#018x}", gte::DEFAULT_SEED));
    report.notes.push(gte_note);
    report.notes.push(xa_note);
    report.notes.push(cdc_note);
    report.notes.push(mdec_note);
    report.notes.extend(spu_note);
    let (mut new, mut same, mut changed, mut moved) = (0usize, 0usize, 0usize, 0usize);
    for ((id, v), res) in vectors.iter().zip(results) {
        let group = groups.get(id).cloned().unwrap_or_else(|| "unlisted".into());
        let entry = |verdict, reference: &str, detail: String| Entry {
            vector: id.hex(),
            kind: v.kind().name().into(),
            group: group.clone(),
            verdict,
            reference: reference.into(),
            detail: Some(detail),
        };
        let got = match res {
            Ok(r) => r,
            Err(e) => {
                report.add(entry(Verdict::Failed, "none", e));
                continue;
            }
        };
        let exact = store.golden(EXACT_TARGET, id)?.map(|g| g.0);
        let (mut verdict, reference) = match &exact {
            Some(e) => judge(v.kind(), &got, Some(e), None, None),
            None if emulator_weak(v.kind()) => (Verdict::Unresolved, Reference::None),
            None => (Verdict::Provisional, Reference::Emulator),
        };
        let mut detail = match &exact {
            Some(e) if *e != got => format!("differs from the closed form: {}", first_difference(&got, e)),
            _ => String::new(),
        };
        match store.golden(&tid, id)? {
            Some((old, _))
                if old != got
                    && crate::verdict::timing_variable(v.kind())
                    && crate::verdict::same_but_timing(v, &old, &got) != Some(false) =>
            {
                moved += 1;
                detail = "captured at another moment (timing-variable; its check compares values)".into();
            }
            Some((old, _)) if old != got => {
                changed += 1;
                verdict = Verdict::Failed;
                detail = format!("changed since its golden was captured: {}", first_difference(&got, &old));
            }
            Some(_) => {
                same += 1;
                if detail.is_empty() {
                    detail = "reproduced byte for byte".into();
                }
            }
            None => {
                new += 1;
                store.put_golden(id, &got, &target.template().for_result(id, v.kind().name(), &got))?;
                if detail.is_empty() {
                    detail = "captured".into();
                }
            }
        }
        report.add(entry(verdict, &format!("{reference:?}").to_lowercase(), detail));
    }
    report.notes.push(format!(
        "{new} captured, {same} reproduced byte for byte, {moved} timing-variable captured at another moment, {changed} changed"
    ));
    report.notes.push(format!(
        "synthetic GPU vectors: {synthetic} ({} per generator, seed {:#018x})",
        gpu_synth::PER_GENERATOR,
        gpu_synth::SEED
    ));
    let path = report.write(&report_dir(ws, &["capture"]))?;
    Ok((report, path))
}

/// psx-wasm's implementation of a device, run natively on vectors. The
/// devices register in M4 (gpu, gte) and M6 (spu); bios with M2's BIOS.
pub trait DeviceUnderTest {
    /// `gpu`, `gte`, `spu` or `bios`.
    fn device(&self) -> &str;
    /// Name and version for the report.
    fn name(&self) -> String;
    /// The vector's result, in the same layout as a target's.
    fn run(&mut self, v: &Vector, store: &Store) -> Result<Vec<u8>>;
    /// For results psx-wasm can't reproduce byte for byte without the
    /// target's timing (when the runner read a buffer that was still being
    /// written): whether the reference result is consistent with this
    /// device, instead of `run`. `None`: compare `run`'s bytes.
    fn consistent(&mut self, _v: &Vector, _store: &Store, _reference: &[u8]) -> Option<Result<(), String>> {
        None
    }
}

pub const DEVICES: [&str; 8] = ["gpu", "gte", "spu", "bios", "xa", "mdec", "cdc", "threads"];

/// The result of `check`.
pub struct Checked {
    pub report: Report,
    pub path: PathBuf,
    /// False when psx-wasm has no implementation of the device yet.
    pub implemented: bool,
}

/// `psxw oracle check <game> --device D`: run psx-wasm's implementation on
/// every archived vector of the device (the game's recorded set plus the
/// harness's handwritten, selftest and synthetic sets) and judge it
/// against the strongest golden: exact, hardware, then this pin's
/// emulator.
pub fn check(ws: &Workspace, game: &str, device: &str, impls: &mut [Box<dyn DeviceUnderTest>]) -> Result<Checked> {
    ws.game(game).with_context(|| format!("unknown game {game}"))?;
    if !DEVICES.contains(&device) {
        bail!("unknown device {device} (one of {})", DEVICES.join(", "));
    }
    let store = store(ws);
    let mut notes = Vec::new();
    if device == "gte" {
        notes.push(install_gte(&store)?);
    }
    if device == "xa" {
        notes.push(install_xa(ws, &store)?);
    }
    if device == "spu" {
        notes.push(install_spu(&store)?);
    }
    if device == "cdc" {
        notes.push(install_cdc(ws, &store)?);
    }
    if device == "mdec" || device == "threads" {
        notes.push(install_mdec_threads(&store)?);
    }
    let synthetic: Vec<String> = store.set_names()?.into_iter().filter(|n| n.starts_with(SYNTHETIC_PREFIX)).collect();
    let mut ids = std::collections::BTreeSet::new();
    let mut group = BTreeMap::new();
    for set in [game_set(game), handwritten::SET.to_string(), SELFTEST_SET.to_string()].into_iter().chain(synthetic) {
        for id in store.set(&set)? {
            if ids.insert(id) {
                group.insert(id, set.clone());
            }
        }
    }
    let vectors: Vec<_> = load(&store, ids)?.into_iter().filter(|(_, v)| v.kind().device() == device).collect();
    let pins = ws.lock.oracle.as_ref().context("inputs.lock has no [oracle] section")?;
    let emu = emulator_target_id(pins.beetle.short(), pins.openbios.short());
    let hardware: Vec<String> = std::fs::read_dir(store.root().join("goldens"))
        .map(|d| {
            d.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
                .filter(|n| n.starts_with("hw-"))
                .collect()
        })
        .unwrap_or_default();

    let mut report = Report::new("check");
    if device == "gte" {
        report.seed = Some(format!("{:#018x}", gte::DEFAULT_SEED));
    }
    if device == "spu" {
        report.seed = Some(format!("{:#018x}", spu::DEFAULT_SEED));
    }
    report.notes.extend(notes);
    let imp = impls.iter_mut().find(|i| i.device() == device);
    let implemented = imp.is_some();
    report.target = Some(match &imp {
        Some(i) => format!("{} vs goldens (exact, hardware, {emu})", i.name()),
        None => format!("no implementation of {device}"),
    });
    match imp {
        None => {
            report.pass = false;
            report.notes.push(format!(
                "psx-wasm has no {device} implementation yet (gpu and gte arrive in M4, spu in M6, bios with M2); {} archived vectors wait for it",
                vectors.len()
            ));
            for (id, v) in &vectors {
                report.add(Entry {
                    vector: id.hex(),
                    kind: v.kind().name().into(),
                    group: group[id].clone(),
                    verdict: Verdict::Unresolved,
                    reference: "none".into(),
                    detail: Some("no implementation".into()),
                });
            }
        }
        Some(imp) => {
            let vectors: Vec<_> = vectors.into_iter().map(|(id, v)| (id, v, group[&id].clone())).collect();
            compare(&store, &vectors, imp.as_mut(), &emu, &hardware, &mut report)?;
            disputed_note(&store, device, imp.as_mut(), &emu, &mut report)?;
            let unresolved = report.count(Verdict::Unresolved);
            if UNRESOLVED_FAILS.contains(&device) && unresolved > 0 {
                report.pass = false;
                report.notes.push(format!("{unresolved} {device} vectors have no golden; run `psxw oracle capture --missing`"));
            }
        }
    }
    let path = report.write(&report_dir(ws, &["check", game, device]))?;
    Ok(Checked { report, path, implemented })
}

/// Sets of vectors where the device target and the specification disagree
/// (`disputed-<device>…`, each with a Finding): kept out of the verdicts,
/// reported in a note so the exclusion stays visible.
pub const DISPUTED_PREFIX: &str = "disputed-";

fn disputed_note(store: &Store, device: &str, imp: &mut dyn DeviceUnderTest, emulator_target: &str, report: &mut Report) -> Result<()> {
    for set in store.set_names()?.into_iter().filter(|n| n.starts_with(DISPUTED_PREFIX)) {
        let (mut n, mut agree, mut missing) = (0, 0, 0);
        for (id, v) in load(store, store.set(&set)?)? {
            if v.kind().device() != device {
                continue;
            }
            n += 1;
            match store.golden(emulator_target, &id)? {
                Some((want, _)) => agree += usize::from(imp.run(&v, store).ok().as_deref() == Some(&want[..])),
                None => missing += 1,
            }
        }
        if n > 0 {
            report.notes.push(format!(
                "{set}: {n} disputed vectors kept out of the verdicts (emulator vs specification; see the milestone Findings): {} matches the emulator on {agree}, {missing} without a golden",
                imp.name()
            ));
        }
    }
    Ok(())
}

/// Run `imp` on each vector and judge it against the strongest golden.
pub fn compare(
    store: &Store,
    vectors: &[(VectorId, Vector, String)],
    imp: &mut dyn DeviceUnderTest,
    emulator_target: &str,
    hardware_targets: &[String],
    report: &mut Report,
) -> Result<()> {
    for (id, v, group) in vectors {
        let exact = store.golden(EXACT_TARGET, id)?.map(|g| g.0);
        let mut hw = None;
        for t in hardware_targets {
            if let Some((r, _)) = store.golden(t, id)? {
                hw = Some(r);
                break;
            }
        }
        let em = store.golden(emulator_target, id)?.map(|g| g.0);
        let strongest = exact.as_ref().map(|e| (Reference::Exact, e)).or(hw.as_ref().map(|h| (Reference::Hardware, h))).or(em.as_ref().map(|e| (Reference::Emulator, e)));
        let consistent = match strongest {
            Some((reference, golden)) => imp.consistent(v, store, golden).map(|r| (reference, r)),
            None => None,
        };
        let (verdict, reference, detail) = match consistent {
            Some((reference, Ok(()))) => (if reference == Reference::Emulator { Verdict::Provisional } else { Verdict::Verified }, reference, None),
            Some((reference, Err(e))) => (Verdict::Failed, reference, Some(e)),
            None => match imp.run(v, store) {
            Err(e) => (Verdict::Failed, Reference::None, Some(format!("{e:#}"))),
            Ok(got) => {
                let (verdict, reference) = judge(v.kind(), &got, exact.as_deref(), hw.as_deref(), em.as_deref());
                let want = match reference {
                    Reference::Exact => exact.as_deref(),
                    Reference::Hardware => hw.as_deref(),
                    Reference::Emulator => em.as_deref(),
                    Reference::None => None,
                };
                let detail = match (verdict, want) {
                    (Verdict::Failed, Some(w)) => Some(first_difference(&got, w)),
                    (Verdict::Unresolved, _) => Some("no golden".into()),
                    _ => None,
                };
                (verdict, reference, detail)
            }
            },
        };
        report.add(Entry {
            vector: id.hex(),
            kind: v.kind().name().into(),
            group: group.clone(),
            verdict,
            reference: format!("{reference:?}").to_lowercase(),
            detail,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::golden::Source;

    /// A fake device that answers from a table, to exercise `compare`.
    struct Table(BTreeMap<VectorId, Vec<u8>>);

    impl DeviceUnderTest for Table {
        fn device(&self) -> &str {
            "harness"
        }
        fn name(&self) -> String {
            "table".into()
        }
        fn run(&mut self, v: &Vector, _: &Store) -> Result<Vec<u8>> {
            self.0.get(&v.id()).cloned().context("no answer")
        }
    }

    #[test]
    fn compare_uses_the_strongest_golden() {
        let dir = std::env::temp_dir().join(format!("psxw-oracle-cmp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::new(&dir);
        let vec_of = |s: &str| Vector::Echo(s.as_bytes().to_vec());
        let (a, b, c, d) = (vec_of("exact"), vec_of("emulated"), vec_of("hardware"), vec_of("nothing"));
        let ids: Vec<VectorId> = [&a, &b, &c, &d].iter().map(|v| store.put_vector(v).unwrap()).collect();
        let emu_prov = |id: &VectorId, r: &[u8]| Provenance {
            target: "emu-test".into(),
            source: Source::Emulator,
            ..Provenance::exact(id, r, "", "echo")
        };
        store.put_golden(&ids[0], b"exact", &Provenance::exact(&ids[0], b"exact", "g", "echo")).unwrap();
        store.put_golden(&ids[0], b"wrong", &emu_prov(&ids[0], b"wrong")).unwrap();
        store.put_golden(&ids[1], b"emulated", &emu_prov(&ids[1], b"emulated")).unwrap();
        let hw = Provenance { target: "hw-test".into(), source: Source::Hardware, ..Provenance::exact(&ids[2], b"hardware", "", "echo") };
        store.put_golden(&ids[2], b"hardware", &hw).unwrap();

        let mut answers = BTreeMap::new();
        answers.insert(ids[0], b"exact".to_vec());
        answers.insert(ids[1], b"emulated".to_vec());
        answers.insert(ids[2], b"not hardware".to_vec());
        answers.insert(ids[3], b"nothing".to_vec());
        let vectors: Vec<_> = ids.iter().zip([a, b, c, d]).map(|(id, v)| (*id, v, "test".to_string())).collect();
        let mut report = Report::new("check");
        compare(&store, &vectors, &mut Table(answers), "emu-test", &["hw-test".into()], &mut report).unwrap();
        let verdicts: Vec<_> = report.entries.iter().map(|e| e.verdict).collect();
        assert_eq!(verdicts, vec![Verdict::Verified, Verdict::Provisional, Verdict::Failed, Verdict::Unresolved]);
        assert!(!report.pass);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The oracle's part of `psxw ci`: the images still match `inputs.lock`
/// and the local daemon, the selftest passes, every archived vector
/// reproduces its golden, psx-gte matches every GTE golden of every game
/// (M4 exit criterion 1), and psx-spu every SPU golden (M6 exit criterion
/// 1).
/// `psxw oracle ci`: images, selftest, capture (determinism), then the
/// check of every device in `impls` for every game.
pub fn ci(ws: &Workspace, impls: &mut [Box<dyn DeviceUnderTest>]) -> Result<Vec<(Report, PathBuf)>> {
    let imgs = images::load(ws)?;
    images::verify(ws, &imgs)?;
    eprintln!("ci: images match inputs.lock ({}, {})", imgs.emu.id, imgs.sdk.id);
    let st = selftest(ws, DEFAULT_SEED, DEFAULT_SELFTEST)?;
    let cap = capture(ws, None, false)?;
    let mut out = vec![st, cap];
    let devices: Vec<String> = impls.iter().map(|i| i.device().to_string()).collect();
    for game in ws.lock.game.keys() {
        for d in &devices {
            let checked = check(ws, game, d, impls)?;
            out.push((checked.report, checked.path));
        }
    }
    Ok(out)
}
