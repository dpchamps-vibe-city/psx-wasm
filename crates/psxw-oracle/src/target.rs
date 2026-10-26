//! Device targets: something that runs the runner's batches and returns
//! the results. The dockerized emulator is the target now; a serial-linked
//! console (M10) implements the same trait.

use crate::golden::{Provenance, Source};
use crate::images::{self, Images};
use crate::script::{self, ExchangeResult, Op};
use crate::store::Store;
use crate::vector::{Vector, VectorId};
use crate::wire;
use anyhow::{Context, Result, bail, ensure};
use psxw_inputs::Workspace;
use std::path::PathBuf;
use std::process::Command;

pub trait Target {
    /// Golden directory name, e.g. `emu-beetle-7fea57a-openbios-8a86b7e-runner-1`.
    fn id(&self) -> &str;
    /// Provenance shared by every result of this target.
    fn template(&self) -> &Provenance;
    /// Run batches (one per exchange) and return one result per batch.
    fn exchange(&mut self, batches: &[Vec<u8>]) -> Result<Vec<ExchangeResult>>;
}

/// Run vectors on a target. Per vector: its result, or why it failed.
/// Vectors whose ops were skipped because an earlier op in the same batch
/// failed are retried once, each in batches of its own.
pub fn run_vectors(target: &mut dyn Target, store: &Store, vectors: &[(VectorId, Vector)]) -> Result<Vec<Result<Vec<u8>, String>>> {
    let mut out: Vec<Result<Vec<u8>, String>> = vec![Err("not run".into()); vectors.len()];
    let mut compiled: Vec<(usize, Vec<Op>)> = Vec::new();
    for (i, (_, v)) in vectors.iter().enumerate() {
        match script::compile(v, store) {
            Ok(ops) => compiled.push((i, ops)),
            Err(e) => out[i] = Err(format!("compile: {e:#}")),
        }
    }
    let retry = pass(target, &compiled, false, &mut out)?;
    if !retry.is_empty() {
        let again: Vec<(usize, Vec<Op>)> = compiled.into_iter().filter(|(i, _)| retry.contains(i)).collect();
        pass(target, &again, true, &mut out)?;
    }
    Ok(out)
}

/// One target run. Returns the vectors that only failed by being skipped.
fn pass(target: &mut dyn Target, compiled: &[(usize, Vec<Op>)], isolate: bool, out: &mut [Result<Vec<u8>, String>]) -> Result<Vec<usize>> {
    if compiled.is_empty() {
        return Ok(Vec::new());
    }
    let ops: Vec<Vec<Op>> = compiled.iter().map(|(_, o)| o.clone()).collect();
    let plan = if isolate {
        let mut plan = script::Plan::default();
        for (v, vops) in ops.iter().enumerate() {
            let p = script::pack(std::slice::from_ref(vops));
            plan.batches.extend(p.batches);
            plan.slots.extend(p.slots.into_iter().map(|s| s.into_iter().map(|(_, o)| (v, o)).collect()));
        }
        plan
    } else {
        script::pack(&ops)
    };
    let results = target.exchange(&plan.batches)?;
    ensure!(results.len() == plan.batches.len(), "target returned {} results for {} batches", results.len(), plan.batches.len());
    let mut per_vector: Vec<Vec<(u32, Vec<u8>)>> = ops.iter().map(|o| Vec::with_capacity(o.len())).collect();
    for (r, slots) in results.iter().zip(&plan.slots) {
        for ((v, _), res) in slots.iter().zip(script::split_batch(r, slots.len())?) {
            per_vector[*v].push(res);
        }
    }
    let mut retry = Vec::new();
    for (k, (i, vops)) in compiled.iter().enumerate() {
        let res = &per_vector[k];
        let skipped_only = res.iter().any(|(s, _)| *s == wire::ERR_SKIPPED)
            && res.iter().all(|(s, _)| *s == wire::ERR_NONE || *s == wire::ERR_SKIPPED);
        if skipped_only && !isolate {
            retry.push(*i);
            continue;
        }
        out[*i] = script::assemble(vops, res).map_err(|e| format!("{e:#}"));
    }
    Ok(retry)
}

/// The dockerized emulator: `psxw-oracle-emu` running the runner disc.
pub struct DockerTarget {
    image: String,
    runner_dir: PathBuf,
    work_root: PathBuf,
    log_dir: PathBuf,
    user: String,
    id: String,
    template: Provenance,
    images: Images,
    pub boot_frames: u64,
    pub timeout_frames: u64,
    runs: u32,
}

/// Target id of the dockerized emulator for the given pins.
pub fn emulator_target_id(beetle7: &str, openbios7: &str) -> String {
    format!("emu-beetle-{beetle7}-openbios-{openbios7}-runner-{}", wire::RUNNER_VERSION)
}

impl DockerTarget {
    /// The target `images build` recorded, after checking it still matches
    /// `inputs.lock` and the local Docker daemon.
    pub fn open(ws: &Workspace) -> Result<Self> {
        ensure!(images::docker_available(), "docker is not available");
        let images = images::load(ws)?;
        images::verify(ws, &images)?;
        let pins = ws.lock.oracle.as_ref().context("inputs.lock has no [oracle] section")?;
        let id = emulator_target_id(pins.beetle.short(), pins.openbios.short());
        let mut commits = std::collections::BTreeMap::new();
        commits.insert("beetle-psx-libretro".to_string(), pins.beetle.commit.clone());
        commits.insert("pcsx-redux".to_string(), pins.openbios.commit.clone());
        commits.insert("nugget".to_string(), pins.openbios.nugget_commit.clone());
        commits.insert("psn00bsdk".to_string(), format!("{} sha256:{}", pins.psn00bsdk.release, pins.psn00bsdk.sha256));
        commits.insert("debian-snapshot".to_string(), pins.debian_snapshot.clone());
        commits.insert("base-image".to_string(), pins.base_image.clone());
        let mut image_ids = std::collections::BTreeMap::new();
        image_ids.insert(images.emu.tag.clone(), images.emu.id.clone());
        image_ids.insert(images.sdk.tag.clone(), images.sdk.id.clone());
        let mut binaries = std::collections::BTreeMap::new();
        for (key, file) in [("core_sha256", "mednafen_psx_libretro.so"), ("host_sha256", "psxw-emu-host")] {
            if let Some(sha) = images.emu.sources.get(key).and_then(|v| v.as_str()) {
                binaries.insert(file.to_string(), sha.to_string());
            }
        }
        let template = Provenance {
            schema_version: crate::golden::PROVENANCE_SCHEMA_VERSION,
            target: id.clone(),
            source: Source::Emulator,
            vector: String::new(),
            kind: String::new(),
            result_sha256: String::new(),
            commits,
            bios_sha1: Some(images.emu_facts.bios_sha1.clone()),
            core: Some(images.emu_facts.core.clone()),
            core_options: images.emu_facts.core_options.clone(),
            runner_version: Some(images.runner.version),
            runner: images.runner.files.clone(),
            images: image_ids,
            binaries,
            generator: None,
            hardware: std::collections::BTreeMap::new(),
            created: String::new(),
        };
        let work_root = images::oracle_dir(ws).join("work");
        std::fs::create_dir_all(&work_root)?;
        let log_dir = ws.out_dir().join("oracle").join("runs");
        std::fs::create_dir_all(&log_dir)?;
        Ok(DockerTarget {
            image: images.emu.id.clone(),
            runner_dir: ws.root.join(&images.runner.dir),
            user: images::owner_of(&work_root)?,
            work_root,
            log_dir,
            id,
            template,
            images,
            boot_frames: 3600,
            timeout_frames: 600,
            runs: 0,
        })
    }

    pub fn images(&self) -> &Images {
        &self.images
    }
}

impl Target for DockerTarget {
    fn id(&self) -> &str {
        &self.id
    }

    fn template(&self) -> &Provenance {
        &self.template
    }

    fn exchange(&mut self, batches: &[Vec<u8>]) -> Result<Vec<ExchangeResult>> {
        self.runs += 1;
        let work = self.work_root.join(format!("run-{}-{}", std::process::id(), self.runs));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work)?;
        std::fs::write(work.join("in.xchg"), script::encode_exchanges(batches))?;
        let args: Vec<String> = [
            "exe",
            "--disc",
            "/runner/psxw-runner.cue",
            "--vectors",
            "/work/in.xchg",
            "--out",
            "/work/out.xres",
            "--provenance",
            "/work/host.json",
            "--log",
            "/work/host.log",
            "--work",
            "/work",
        ]
        .iter()
        .map(|s| s.to_string())
        .chain(["--boot-frames".into(), self.boot_frames.to_string(), "--timeout-frames".into(), self.timeout_frames.to_string()])
        .collect();
        let argv = images::docker_run(
            &self.image,
            &self.user,
            &[(self.runner_dir.clone(), "/runner", true), (work.clone(), "/work", false)],
            None,
            &args,
        );
        let output = Command::new("docker").args(&argv).output().context("running docker")?;
        for (name, keep) in [("host.log", "last-host.log"), ("host.json", "last-host.json")] {
            let _ = std::fs::copy(work.join(name), self.log_dir.join(keep));
        }
        if !output.status.success() {
            bail!(
                "the emulator target failed ({}): {}\nhost log: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim(),
                self.log_dir.join("last-host.log").display()
            );
        }
        let results = script::decode_results(&std::fs::read(work.join("out.xres"))?)?;
        // The run must have used exactly the BIOS, options and runner the
        // provenance claims.
        let host: serde_json::Value = serde_json::from_slice(&std::fs::read(work.join("host.json"))?)?;
        let facts = images::parse_facts(&host)?;
        ensure!(facts == self.images.emu_facts, "the emulator reported different BIOS, core or options than images.json records");
        let runner = host.get("run").and_then(|r| r.get("runner_version")).and_then(|v| v.as_u64());
        ensure!(runner == Some(wire::RUNNER_VERSION as u64), "the runner reported version {runner:?}");
        std::fs::remove_dir_all(&work)?;
        Ok(results)
    }
}
