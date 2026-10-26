//! `psxw oracle images build`: build both Docker images from the pins in
//! `inputs.lock`, check that the sources inside them are the pinned ones,
//! build the runner, and record everything in `.oracle/images.json`.

use crate::store::write_atomic;
use crate::wire;
use anyhow::{Context, Result, bail, ensure};
use psxw_inputs::Workspace;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const EMU_IMAGE: &str = "psxw-oracle-emu:local";
pub const SDK_IMAGE: &str = "psxw-psx-sdk:local";
pub const IMAGES_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Images {
    pub schema_version: u32,
    pub built: String,
    pub base_image: String,
    pub debian_snapshot: String,
    pub emu: ImageRecord,
    pub sdk: ImageRecord,
    /// Facts the emulator host reports (`psxw-emu-host info`).
    pub emu_facts: EmuFacts,
    pub runner: RunnerRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageRecord {
    pub tag: String,
    /// Local image id (`sha256:…`).
    pub id: String,
    /// What the image says it was built from.
    pub sources: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmuFacts {
    /// Library name and version, e.g. `Beetle PSX 0.9.44.1 7fea57a`.
    pub core: String,
    pub bios_sha1: String,
    pub core_options: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerRecord {
    pub version: u32,
    /// Directory holding the runner files, relative to the workspace.
    pub dir: String,
    /// sha256 by file name.
    pub files: BTreeMap<String, String>,
    pub image_end: String,
    /// Where the XA test file (`xa_disc::NAME`) starts on the runner disc.
    #[serde(default)]
    pub xa_lba: Option<u32>,
}

pub fn oracle_dir(ws: &Workspace) -> PathBuf {
    ws.root.join(".oracle")
}

pub fn images_path(ws: &Workspace) -> PathBuf {
    oracle_dir(ws).join("images.json")
}

pub fn runner_dir(ws: &Workspace) -> PathBuf {
    oracle_dir(ws).join("runner")
}

/// Whether a Docker daemon answers.
pub fn docker_available() -> bool {
    Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Run docker, return stdout; on failure, the error carries stderr.
pub fn docker_output(args: &[String]) -> Result<String> {
    let out = Command::new("docker").args(args).output().context("running docker")?;
    if !out.status.success() {
        bail!("docker {} failed: {}", args.first().map(String::as_str).unwrap_or(""), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8(out.stdout)?)
}

pub fn image_id(tag: &str) -> Result<String> {
    Ok(docker_output(&["image".into(), "inspect".into(), "--format".into(), "{{.Id}}".into(), tag.into()])?
        .trim()
        .to_string())
}

/// uid:gid that owns `dir`, so container output stays writable for us.
pub fn owner_of(dir: &Path) -> Result<String> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(dir).with_context(|| format!("stat {}", dir.display()))?;
    Ok(format!("{}:{}", m.uid(), m.gid()))
}

/// `docker run --rm --network=none` with the given mounts and arguments.
pub fn docker_run(image: &str, user: &str, mounts: &[(PathBuf, &str, bool)], entrypoint: Option<&str>, args: &[String]) -> Vec<String> {
    let mut v: Vec<String> = vec!["run".into(), "--rm".into(), "--network=none".into(), "--user".into(), user.into()];
    for (host, guest, ro) in mounts {
        v.push("-v".into());
        v.push(format!("{}:{}{}", host.display(), guest, if *ro { ":ro" } else { "" }));
    }
    if let Some(e) = entrypoint {
        v.push("--entrypoint".into());
        v.push(e.into());
    }
    v.push(image.into());
    v.extend(args.iter().cloned());
    v
}

fn build_image(ws: &Workspace, dockerfile: &str, tag: &str, args: &[(&str, &str)], log: &Path, no_cache: bool) -> Result<()> {
    let mut cmd = Command::new("docker");
    cmd.arg("build").arg("-f").arg(ws.root.join("oracle/docker").join(dockerfile)).arg("-t").arg(tag).arg("--progress=plain");
    if no_cache {
        cmd.arg("--no-cache");
    }
    for (k, v) in args {
        cmd.arg("--build-arg").arg(format!("{k}={v}"));
    }
    cmd.arg(ws.root.join("oracle"));
    let file = std::fs::File::create(log).with_context(|| format!("creating {}", log.display()))?;
    cmd.stdout(file.try_clone()?).stderr(file);
    eprintln!("images: building {tag} (log: {})", log.display());
    let status = cmd.status().context("running docker build")?;
    if !status.success() {
        bail!("docker build of {tag} failed; see {}", log.display());
    }
    Ok(())
}

fn str_field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Parse `psxw-emu-host info` output.
pub fn parse_facts(info: &Value) -> Result<EmuFacts> {
    let core = info.get("core").context("info: no core")?;
    let bios = info.get("bios").context("info: no bios")?;
    ensure!(bios.get("present").and_then(Value::as_bool) == Some(true), "the image has no BIOS at {}", str_field(bios, "path"));
    let mut core_options = BTreeMap::new();
    if let Some(Value::Object(m)) = info.get("core_options") {
        for (k, v) in m {
            core_options.insert(k.clone(), v.as_str().unwrap_or("").to_string());
        }
    }
    Ok(EmuFacts {
        core: format!("{} {}", str_field(core, "library_name"), str_field(core, "library_version")),
        bios_sha1: str_field(bios, "sha1").to_string(),
        core_options,
    })
}

/// Build both images and the runner; record `.oracle/images.json`. With
/// `no_cache`, every layer is rebuilt from the pinned sources.
pub fn build(ws: &Workspace, no_cache: bool) -> Result<Images> {
    let pins = ws.lock.oracle.as_ref().context("inputs.lock has no [oracle] section")?;
    ensure!(docker_available(), "docker is not available (docker version failed)");
    let header = ws.root.join(wire::C_HEADER_PATH);
    ensure!(
        std::fs::read_to_string(&header).unwrap_or_default() == wire::c_header(),
        "{} is stale; run `psxw oracle gen-header`",
        header.display()
    );
    let logs = ws.out_dir().join("oracle").join("images");
    std::fs::create_dir_all(&logs)?;

    build_image(
        ws,
        "emu.Dockerfile",
        EMU_IMAGE,
        &[
            ("BASE_IMAGE", &pins.base_image),
            ("DEBIAN_SNAPSHOT", &pins.debian_snapshot),
            ("BEETLE_REPO", &pins.beetle.repo),
            ("BEETLE_COMMIT", &pins.beetle.commit),
            ("REDUX_REPO", &pins.openbios.repo),
            ("REDUX_COMMIT", &pins.openbios.commit),
            ("NUGGET_REPO", &pins.openbios.nugget_repo),
            ("NUGGET_COMMIT", &pins.openbios.nugget_commit),
        ],
        &logs.join("emu-build.log"),
        no_cache,
    )?;
    let gcc = pins
        .psn00bsdk
        .toolchain
        .as_deref()
        .and_then(|t| t.rsplit('-').next())
        .context("[oracle.psn00bsdk] toolchain must name the GCC version, e.g. gcc-mipsel-none-elf-12.3.0")?;
    build_image(
        ws,
        "sdk.Dockerfile",
        SDK_IMAGE,
        &[
            ("BASE_IMAGE", &pins.base_image),
            ("DEBIAN_SNAPSHOT", &pins.debian_snapshot),
            ("PSN00BSDK_RELEASE", &pins.psn00bsdk.release),
            ("PSN00BSDK_URL", &pins.psn00bsdk.url),
            ("PSN00BSDK_SHA256", &pins.psn00bsdk.sha256),
            ("PSN00BSDK_GCC", gcc),
        ],
        &logs.join("sdk-build.log"),
        no_cache,
    )?;

    let user = {
        std::fs::create_dir_all(oracle_dir(ws))?;
        owner_of(&oracle_dir(ws))?
    };
    // The sources inside the images must be the pinned ones.
    let emu_sources: Value = serde_json::from_str(&docker_output(&docker_run(
        EMU_IMAGE,
        &user,
        &[],
        Some("cat"),
        &["/opt/psxw/share/sources.json".into()],
    ))?)?;
    for (key, want) in [
        ("beetle", &pins.beetle.commit),
        ("pcsx_redux", &pins.openbios.commit),
        ("nugget", &pins.openbios.nugget_commit),
        ("debian_snapshot", &pins.debian_snapshot),
    ] {
        ensure!(str_field(&emu_sources, key) == want, "{EMU_IMAGE}: {key} is {}, inputs.lock pins {want}", str_field(&emu_sources, key));
    }
    let sdk_sources: Value = serde_json::from_str(&docker_output(&docker_run(
        SDK_IMAGE,
        &user,
        &[],
        Some("cat"),
        &["/opt/psn00bsdk/psxw-sources.json".into()],
    ))?)?;
    for (key, want) in [
        ("psn00bsdk_release", &pins.psn00bsdk.release),
        ("psn00bsdk_sha256", &pins.psn00bsdk.sha256),
        ("debian_snapshot", &pins.debian_snapshot),
    ] {
        ensure!(str_field(&sdk_sources, key) == want, "{SDK_IMAGE}: {key} is {}, inputs.lock pins {want}", str_field(&sdk_sources, key));
    }
    let info: Value = serde_json::from_str(&docker_output(&docker_run(EMU_IMAGE, &user, &[], None, &["info".into()]))?)?;
    let emu_facts = parse_facts(&info)?;
    ensure!(
        emu_facts.bios_sha1 == str_field(&emu_sources, "openbios_sha1"),
        "the host reports BIOS {} but the image built {}",
        emu_facts.bios_sha1,
        str_field(&emu_sources, "openbios_sha1")
    );

    // The runner, built by the SDK image from oracle/ mounted read-only.
    let rdir = runner_dir(ws);
    let _ = std::fs::remove_dir_all(&rdir);
    std::fs::create_dir_all(&rdir)?;
    // Generated disc files, mounted at /gen.
    let gen_dir = oracle_dir(ws).join("work").join("gen");
    std::fs::create_dir_all(&gen_dir)?;
    std::fs::write(gen_dir.join("xatest.xa"), crate::xa_disc::file())?;
    eprintln!("images: building the runner");
    let out = docker_output(&docker_run(
        SDK_IMAGE,
        &user,
        &[(ws.root.join("oracle"), "/src", true), (rdir.clone(), "/out", false), (gen_dir, "/gen", true)],
        None,
        &[],
    ))?;
    std::fs::write(logs.join("runner-build.log"), &out)?;
    let runner_json: Value = serde_json::from_slice(&std::fs::read(rdir.join("runner.json"))?)?;
    let mut files = BTreeMap::new();
    for name in ["psxw-runner.exe", "psxw-runner.bin", "psxw-runner.cue"] {
        let got = psxw_inputs::sha256_file(&rdir.join(name))?;
        files.insert(name.to_string(), got);
    }
    ensure!(files["psxw-runner.exe"] == str_field(&runner_json, "exe_sha256"), "runner.json disagrees with psxw-runner.exe");

    let images = Images {
        schema_version: IMAGES_SCHEMA_VERSION,
        built: crate::golden::now_rfc3339(),
        base_image: pins.base_image.clone(),
        debian_snapshot: pins.debian_snapshot.clone(),
        emu: ImageRecord { tag: EMU_IMAGE.into(), id: image_id(EMU_IMAGE)?, sources: emu_sources },
        sdk: ImageRecord { tag: SDK_IMAGE.into(), id: image_id(SDK_IMAGE)?, sources: sdk_sources },
        emu_facts,
        runner: RunnerRecord {
            version: wire::RUNNER_VERSION,
            dir: ".oracle/runner".into(),
            files,
            image_end: str_field(&runner_json, "image_end").to_string(),
            xa_lba: Some(crate::xa_disc::file_lba(&rdir.join("psxw-runner.bin"))?),
        },
    };
    write_atomic(&images_path(ws), serde_json::to_string_pretty(&images)?.as_bytes())?;
    Ok(images)
}

/// The recorded images, if `images build` has run.
pub fn load(ws: &Workspace) -> Result<Images> {
    let path = images_path(ws);
    let text = std::fs::read_to_string(&path).with_context(|| format!("{} missing; run `psxw oracle images build`", path.display()))?;
    let images: Images = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    ensure!(images.schema_version == IMAGES_SCHEMA_VERSION, "{} has schema {}", path.display(), images.schema_version);
    Ok(images)
}

/// Check the recorded images against `inputs.lock` and the local Docker
/// daemon: commits as pinned, image ids still present, runner files intact.
pub fn verify(ws: &Workspace, images: &Images) -> Result<()> {
    let pins = ws.lock.oracle.as_ref().context("inputs.lock has no [oracle] section")?;
    ensure!(images.base_image == pins.base_image, "images were built from {}, inputs.lock pins {}", images.base_image, pins.base_image);
    for (key, want) in [
        ("beetle", &pins.beetle.commit),
        ("pcsx_redux", &pins.openbios.commit),
        ("nugget", &pins.openbios.nugget_commit),
    ] {
        ensure!(
            str_field(&images.emu.sources, key) == want,
            "images.json: {key} {} but inputs.lock pins {want}; rebuild with `psxw oracle images build`",
            str_field(&images.emu.sources, key)
        );
    }
    ensure!(
        str_field(&images.sdk.sources, "psn00bsdk_sha256") == pins.psn00bsdk.sha256,
        "images.json: PSn00bSDK archive differs from inputs.lock; rebuild"
    );
    ensure!(images.runner.version == wire::RUNNER_VERSION, "runner {} built, wire.rs says {}; rebuild", images.runner.version, wire::RUNNER_VERSION);
    for (tag, id) in [(&images.emu.tag, &images.emu.id), (&images.sdk.tag, &images.sdk.id)] {
        let now = image_id(tag).with_context(|| format!("image {tag} missing; run `psxw oracle images build`"))?;
        ensure!(&now == id, "image {tag} is now {now}, images.json recorded {id}; rerun `psxw oracle images build`");
    }
    let dir = ws.root.join(&images.runner.dir);
    for (name, sha) in &images.runner.files {
        let got = psxw_inputs::sha256_file(&dir.join(name))?;
        ensure!(&got == sha, "{} changed since images build", dir.join(name).display());
    }
    Ok(())
}
