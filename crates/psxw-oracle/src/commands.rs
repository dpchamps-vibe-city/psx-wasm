//! The oracle commands behind `psxw oracle …`: selftest, capture, check
//! and the CI gate. Each writes a report under `out/oracle/` and returns it;
//! the CLI turns `pass == false` into a non-zero exit.

use crate::exact;
use crate::golden::{EXACT_TARGET, Provenance};
use crate::handwritten;
use crate::images;
use crate::record::game_set;
use crate::report::{Entry, Report, first_difference};
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
    let mut target = DockerTarget::open(ws)?;
    let tid = target.id().to_string();
    let ids: Vec<VectorId> = store.vector_ids()?;
    let mut vectors = Vec::new();
    for (id, v) in load(&store, ids)? {
        if kind.is_some_and(|k| k != v.kind()) || (missing && store.has_golden(&tid, &id)) {
            continue;
        }
        vectors.push((id, v));
    }
    eprintln!("capture: {} vectors on {tid}", vectors.len());
    let results = run_vectors(&mut target, &store, &vectors)?;
    let groups = set_index(&store)?;
    let mut report = Report::new("capture");
    report.target = Some(tid.clone());
    let (mut new, mut same, mut changed) = (0usize, 0usize, 0usize);
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
    report.notes.push(format!("{new} captured, {same} reproduced byte for byte, {changed} changed"));
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
}

pub const DEVICES: [&str; 4] = ["gpu", "gte", "spu", "bios"];

/// The result of `check`.
pub struct Checked {
    pub report: Report,
    pub path: PathBuf,
    /// False when psx-wasm has no implementation of the device yet.
    pub implemented: bool,
}

/// `psxw oracle check <game> --device D`: run psx-wasm's implementation on
/// every archived vector of the device (the game's recorded set plus the
/// harness's handwritten and selftest sets) and judge it against the
/// strongest golden: exact, hardware, then this pin's emulator.
pub fn check(ws: &Workspace, game: &str, device: &str, impls: &mut [Box<dyn DeviceUnderTest>]) -> Result<Checked> {
    ws.game(game).with_context(|| format!("unknown game {game}"))?;
    if !DEVICES.contains(&device) {
        bail!("unknown device {device} (one of {})", DEVICES.join(", "));
    }
    let store = store(ws);
    let mut ids = std::collections::BTreeSet::new();
    let mut group = BTreeMap::new();
    for set in [game_set(game), handwritten::SET.to_string(), SELFTEST_SET.to_string()] {
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
        }
    }
    let path = report.write(&report_dir(ws, &["check", game, device]))?;
    Ok(Checked { report, path, implemented })
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
        let (verdict, reference, detail) = match imp.run(v, store) {
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
/// and the local daemon, the selftest passes, and every archived vector
/// reproduces its golden.
pub fn ci(ws: &Workspace) -> Result<Vec<(Report, PathBuf)>> {
    let imgs = images::load(ws)?;
    images::verify(ws, &imgs)?;
    eprintln!("ci: images match inputs.lock ({}, {})", imgs.emu.id, imgs.sdk.id);
    let st = selftest(ws, DEFAULT_SEED, DEFAULT_SELFTEST)?;
    let cap = capture(ws, None, false)?;
    Ok(vec![st, cap])
}
