//! Batch export and import for capture on someone else's console (M10
//! task 6), and the emulator–hardware disagreement report (task 5).
//!
//! An export is a directory a contributor with a rig can run without this
//! repository:
//!
//! ```text
//! manifest.json            what to run, with hashes
//! runner/<files>           the runner program, as built here
//! batches/batch-NNNN.bin   mailbox batches, in order, exactly as a
//!                          hardware target would send them
//! README.txt               how to run them and what to send back
//! ```
//!
//! The contributor returns the directory with `rig.json` (rig id, console
//! model, BIOS version, the runner's sha256 as they ran it) and
//! `results/batch-NNNN.res`, one per batch: the mailbox state after it,
//! `state u32, error u32, length u32, data` (little-endian).
//!
//! Import re-packs the same vectors (packing is deterministic) and checks
//! every batch's hash against the manifest. It splits each batch's
//! results back to the vectors and stores them as goldens of target
//! `hw-<rig>`, with provenance naming the rig, the console and the hashes
//! of the manifest and results. `psxw oracle check` then prefers them to
//! the emulator's. The hashes give integrity, not authenticity: nothing is
//! signed.

use crate::golden::{PROVENANCE_SCHEMA_VERSION, Provenance, Source, now_rfc3339};
use crate::hash::Hash;
use crate::script::{self, ExchangeResult, Op};
use crate::store::{Store, write_atomic};
use crate::vector::{Vector, VectorId};
use crate::wire;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const MANIFEST: &str = "manifest.json";
pub const RIG: &str = "rig.json";
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub schema_version: u32,
    pub wire_version: u32,
    pub runner_version: u32,
    /// Runner files and their sha256.
    pub runner: BTreeMap<String, String>,
    /// Vector ids in packing order.
    pub vectors: Vec<String>,
    pub batches: Vec<BatchEntry>,
    pub created: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchEntry {
    pub file: String,
    pub sha256: String,
    pub ops: usize,
}

/// What the contributor reports about the console the batches ran on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rig {
    /// Names the golden target `hw-<rig>`: letters, digits and `-`.
    pub rig: String,
    /// e.g. `SCPH-1001`.
    pub console: String,
    /// The BIOS version the runner reported.
    pub bios: String,
    /// sha256 of the runner executable as run.
    pub runner_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contributor: Option<String>,
}

fn batch_name(i: usize) -> String {
    format!("batch-{i:04}")
}

/// Compile and pack `ids` (vectors that don't compile are left out).
fn plan(store: &Store, ids: &[VectorId]) -> Result<(Vec<(VectorId, Vector, Vec<Op>)>, script::Plan)> {
    let mut vs = Vec::new();
    for id in ids {
        let v = store.vector(id)?;
        if let Ok(ops) = script::compile(&v, store) {
            vs.push((*id, v, ops));
        }
    }
    let ops: Vec<Vec<Op>> = vs.iter().map(|(_, _, o)| o.clone()).collect();
    Ok((vs, script::pack(&ops)))
}

/// Write an export of `ids` into `out` (which must not exist yet), with the
/// runner files from `runner_dir`.
pub fn export(store: &Store, ids: &[VectorId], runner_dir: &Path, runner: &BTreeMap<String, String>, out: &Path) -> Result<Manifest> {
    ensure!(!out.exists(), "{} exists", out.display());
    let (vs, plan) = plan(store, ids)?;
    ensure!(!vs.is_empty(), "no vectors to export");
    for (file, sha) in runner {
        let bytes = std::fs::read(runner_dir.join(file)).with_context(|| format!("runner file {file}"))?;
        ensure!(Hash::of(&bytes).hex() == *sha, "runner file {file} doesn't match its recorded sha256");
        write_atomic(&out.join("runner").join(file), &bytes)?;
    }
    let mut batches = Vec::new();
    for (i, (b, slots)) in plan.batches.iter().zip(&plan.slots).enumerate() {
        let file = format!("batches/{}.bin", batch_name(i));
        write_atomic(&out.join(&file), b)?;
        batches.push(BatchEntry { file, sha256: Hash::of(b).hex(), ops: slots.len() });
    }
    let m = Manifest {
        schema_version: SCHEMA_VERSION,
        wire_version: wire::WIRE_VERSION,
        runner_version: wire::RUNNER_VERSION,
        runner: runner.clone(),
        vectors: vs.iter().map(|(id, _, _)| id.hex()).collect(),
        batches,
        created: now_rfc3339(),
    };
    write_atomic(&out.join(MANIFEST), serde_json::to_string_pretty(&m)?.as_bytes())?;
    let readme = format!(
        "psx-wasm oracle batch: {} vectors in {} batches (wire {}, runner {}).\n\n\
         Upload runner/psxw-runner.exe to the console and send each batches/batch-NNNN.bin\n\
         through the mailbox, in order (psxw oracle capture --target hardware does this).\n\
         Save the mailbox after each as results/batch-NNNN.res: state, error and length\n\
         as little-endian u32, then the data. Write rig.json (rig, console, bios,\n\
         runner_sha256, contributor) and send this directory back.\n",
        m.vectors.len(),
        m.batches.len(),
        m.wire_version,
        m.runner_version
    );
    write_atomic(&out.join("README.txt"), readme.as_bytes())?;
    Ok(m)
}

pub fn encode_result(r: &ExchangeResult) -> Vec<u8> {
    let mut b = Vec::with_capacity(12 + r.data.len());
    b.extend_from_slice(&r.state.to_le_bytes());
    b.extend_from_slice(&r.error.to_le_bytes());
    b.extend_from_slice(&(r.data.len() as u32).to_le_bytes());
    b.extend_from_slice(&r.data);
    b
}

pub fn decode_result(b: &[u8]) -> Result<ExchangeResult> {
    ensure!(b.len() >= 12, "result shorter than its header");
    let w = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    let len = w(8) as usize;
    ensure!(b.len() == 12 + len, "result length {} != header's {len}", b.len() - 12);
    Ok(ExchangeResult { state: w(0), error: w(4), data: b[12..].to_vec() })
}

#[derive(Debug, Default, Serialize)]
pub struct Imported {
    pub target: String,
    pub stored: usize,
    /// Vectors without a golden, and why.
    pub failed: Vec<(String, String)>,
}

/// Import a returned batch directory.
pub fn import(store: &Store, dir: &Path) -> Result<Imported> {
    let m: Manifest = serde_json::from_slice(&std::fs::read(dir.join(MANIFEST)).context("reading the manifest")?)?;
    ensure!(m.schema_version == SCHEMA_VERSION, "manifest schema {}", m.schema_version);
    ensure!(m.wire_version == wire::WIRE_VERSION, "batches are wire {}, this build speaks {}", m.wire_version, wire::WIRE_VERSION);
    let rig: Rig = serde_json::from_slice(&std::fs::read(dir.join(RIG)).context("reading rig.json")?)?;
    ensure!(
        !rig.rig.is_empty() && rig.rig.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'),
        "rig id {:?}: letters, digits and '-' only",
        rig.rig
    );
    let exe = m.runner.iter().find(|(f, _)| f.ends_with(".exe")).map(|(_, s)| s.clone());
    ensure!(exe.as_deref() == Some(rig.runner_sha256.as_str()), "the rig ran runner {}, the export holds {exe:?}", rig.runner_sha256);
    let ids: Vec<VectorId> = m.vectors.iter().map(|h| VectorId::parse(h)).collect::<Result<_>>()?;
    let (vs, plan) = plan(store, &ids)?;
    ensure!(vs.len() == ids.len(), "{} of the manifest's vectors don't compile here", ids.len() - vs.len());
    ensure!(plan.batches.len() == m.batches.len(), "packing differs: {} batches, the manifest has {}", plan.batches.len(), m.batches.len());
    for (i, (b, e)) in plan.batches.iter().zip(&m.batches).enumerate() {
        ensure!(Hash::of(b).hex() == e.sha256, "batch {i} doesn't match the manifest: a different build packed it");
    }
    let manifest_sha = Hash::of(&std::fs::read(dir.join(MANIFEST))?).hex();
    let target = format!("hw-{}", rig.rig);
    let mut per_vector: Vec<Vec<(u32, Vec<u8>)>> = vec![Vec::new(); vs.len()];
    let mut results_hash = Vec::new();
    for (i, slots) in plan.slots.iter().enumerate() {
        let path = dir.join("results").join(format!("{}.res", batch_name(i)));
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        results_hash.extend_from_slice(&Hash::of(&bytes).0);
        let r = decode_result(&bytes)?;
        for ((v, _), res) in slots.iter().zip(script::split_batch(&r, slots.len())?) {
            per_vector[*v].push(res);
        }
    }
    let mut hardware = BTreeMap::new();
    hardware.insert("rig".to_string(), rig.rig.clone());
    hardware.insert("console".to_string(), rig.console.clone());
    hardware.insert("bios".to_string(), rig.bios.clone());
    if let Some(c) = &rig.contributor {
        hardware.insert("contributor".to_string(), c.clone());
    }
    hardware.insert("manifest_sha256".to_string(), manifest_sha);
    hardware.insert("results_sha256".to_string(), Hash::of(&results_hash).hex());
    let template = Provenance {
        schema_version: PROVENANCE_SCHEMA_VERSION,
        target: target.clone(),
        source: Source::Hardware,
        vector: String::new(),
        kind: String::new(),
        result_sha256: String::new(),
        commits: BTreeMap::new(),
        bios_sha1: None,
        core: None,
        core_options: BTreeMap::new(),
        runner_version: Some(m.runner_version),
        runner: m.runner.clone(),
        images: BTreeMap::new(),
        binaries: BTreeMap::new(),
        generator: None,
        hardware,
        created: String::new(),
    };
    let mut out = Imported { target, ..Default::default() };
    for ((id, v, ops), res) in vs.iter().zip(per_vector) {
        match script::assemble(ops, &res) {
            Ok(bytes) => {
                store.put_golden(id, &bytes, &template.for_result(id, v.kind().name(), &bytes))?;
                out.stored += 1;
            }
            Err(e) => out.failed.push((id.hex(), format!("{e:#}"))),
        }
    }
    Ok(out)
}

/// One vector whose hardware and emulator goldens differ.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Disagreement {
    pub vector: String,
    pub kind: String,
    pub hardware: String,
    pub detail: String,
}

#[derive(Debug, Default, Serialize)]
pub struct Disagreements {
    pub emulator: String,
    pub hardware: Vec<String>,
    /// Vectors with both goldens.
    pub compared: usize,
    pub agree: usize,
    /// Hardware goldens with no emulator golden.
    pub hardware_only: usize,
    pub differ: Vec<Disagreement>,
}

/// Every vector with goldens from both the emulator target and one of the
/// hardware targets: where they differ is a finding about the emulator
/// (or about psx-wasm's verdicts so far).
pub fn disagreements(store: &Store, emulator: &str, hardware: &[String]) -> Result<Disagreements> {
    let mut out = Disagreements { emulator: emulator.into(), hardware: hardware.to_vec(), ..Default::default() };
    for hw in hardware {
        let dir = store.golden_dir(hw);
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut ids: Vec<VectorId> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".res").map(str::to_string))
            .filter_map(|h| VectorId::parse(&h).ok())
            .collect();
        ids.sort();
        for id in ids {
            let Some((h, prov)) = store.golden(hw, &id)? else { continue };
            let Some((e, _)) = store.golden(emulator, &id)? else {
                out.hardware_only += 1;
                continue;
            };
            out.compared += 1;
            if h == e {
                out.agree += 1;
            } else {
                out.differ.push(Disagreement {
                    vector: id.hex(),
                    kind: prov.kind.clone(),
                    hardware: hw.clone(),
                    detail: format!("emulator vs hardware: {}", crate::report::first_difference(&e, &h)),
                });
            }
        }
    }
    Ok(out)
}

/// The hardware golden targets in a store (`hw-…`).
pub fn hardware_targets(store: &Store) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(store.root().join("goldens"))
        .map(|d| d.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned())).filter(|n| n.starts_with("hw-")).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// A stand-in console for tests: a runner that only knows ECHO, which
/// returns its payload. Batches are `count`, then per op `code, length,
/// payload` padded to 4; results are per op `status, length, bytes`
/// padded to 4.
#[cfg(test)]
fn answer(batch: &[u8]) -> Result<ExchangeResult> {
    let w = |i: usize| -> Result<u32> { Ok(u32::from_le_bytes(batch.get(i..i + 4).context("short batch")?.try_into()?)) };
    let n = w(0)? as usize;
    let (mut pos, mut data) = (4usize, Vec::new());
    for _ in 0..n {
        let (code, len) = (w(pos)?, w(pos + 4)? as usize);
        pos += 8;
        ensure!(code == wire::OP_ECHO, "the stand-in console only echoes");
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&(len as u32).to_le_bytes());
        data.extend_from_slice(&batch[pos..pos + len]);
        data.resize(wire::pad4(data.len()), 0);
        pos += wire::pad4(len);
    }
    if pos != batch.len() {
        anyhow::bail!("batch has trailing bytes");
    }
    Ok(ExchangeResult { state: wire::MBX_STATE_DONE, error: 0, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("psxw-batch-{}-{}", std::process::id(), rand_suffix()));
        (Store::new(dir.join(".oracle")), dir)
    }

    fn rand_suffix() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64
    }

    #[test]
    fn export_import_round_trip_and_disagreements() {
        let (st, dir) = store();
        let ids: Vec<VectorId> = (0..40u8).map(|i| st.put_vector(&Vector::Echo(vec![i; 64 + i as usize])).unwrap()).collect();
        let runner_dir = dir.join("runner");
        std::fs::create_dir_all(&runner_dir).unwrap();
        std::fs::write(runner_dir.join("psxw-runner.exe"), b"PS-X EXE runner").unwrap();
        let runner: BTreeMap<String, String> =
            [("psxw-runner.exe".to_string(), Hash::of(b"PS-X EXE runner").hex())].into_iter().collect();
        let out = dir.join("export");
        let m = export(&st, &ids, &runner_dir, &runner, &out).unwrap();
        assert_eq!(m.vectors.len(), 40);
        assert!(!m.batches.is_empty());

        // The contributor's side: run every batch, save the results.
        for (i, e) in m.batches.iter().enumerate() {
            let b = std::fs::read(out.join(&e.file)).unwrap();
            let r = answer(&b).unwrap();
            assert_eq!(u32::from_le_bytes(b[..4].try_into().unwrap()) as usize, e.ops);
            write_atomic(&out.join("results").join(format!("{}.res", batch_name(i))), &encode_result(&r)).unwrap();
        }
        let rig = Rig {
            rig: "scph1001-a".into(),
            console: "SCPH-1001".into(),
            bios: "4.1 12/16/97".into(),
            runner_sha256: Hash::of(b"PS-X EXE runner").hex(),
            contributor: None,
        };
        std::fs::write(out.join(RIG), serde_json::to_string(&rig).unwrap()).unwrap();
        let got = import(&st, &out).unwrap();
        assert_eq!((got.target.as_str(), got.stored, got.failed.len()), ("hw-scph1001-a", 40, 0));
        let (res, prov) = st.golden("hw-scph1001-a", &ids[5]).unwrap().unwrap();
        assert_eq!(res, vec![5; 69]);
        assert_eq!(prov.source, Source::Hardware);
        assert_eq!(prov.hardware["console"], "SCPH-1001");

        // An emulator golden that agrees, one that differs.
        let emu = |id: &VectorId, r: &[u8]| Provenance { target: "emu-x".into(), source: Source::Emulator, ..Provenance::exact(id, r, "", "echo") };
        st.put_golden(&ids[5], &[5; 69], &emu(&ids[5], &[5; 69])).unwrap();
        let mut off = vec![6u8; 70];
        off[3] = 0;
        st.put_golden(&ids[6], &off, &emu(&ids[6], &off)).unwrap();
        let d = disagreements(&st, "emu-x", &hardware_targets(&st)).unwrap();
        assert_eq!((d.compared, d.agree, d.hardware_only, d.differ.len()), (2, 1, 38, 1));
        assert_eq!(d.differ[0].vector, ids[6].hex());

        // A batch the export didn't hold is refused.
        std::fs::write(out.join(RIG), serde_json::to_string(&Rig { runner_sha256: "0".repeat(64), ..rig }).unwrap()).unwrap();
        assert!(import(&st, &out).is_err());
        std::fs::remove_dir_all(dir).ok();
    }
}
