//! Pinned toolchain distributions under `.toolchain/`.

use crate::workspace::Workspace;
use anyhow::{Context, Result, anyhow, bail, ensure};
use std::path::PathBuf;
use std::process::Command;

/// Where the debug build's Binaryen patches and build image live.
const DWARF_BINARYEN: &str = "toolchain/binaryen-dwarf";
/// Applied in this order to `[toolchain.binaryen-src]`.
const DWARF_BINARYEN_PATCHES: [&str; 2] = ["upstream-scope-ranges.patch", "value-provenance.patch"];
const DWARF_BINARYEN_IMAGE: &str = "psxw-binaryen-build:local";
/// Runs in the image, with the source at /src, the patches at /patches and
/// /work writable. Static: the image's glibc is newer than a host's may be.
const DWARF_BINARYEN_BUILD: &str = r#"cp -r /src /work/src
cd /work/src
for p in "$@"; do patch --batch --fuzz=0 -p1 -i "/patches/$p"; done
cmake -S . -B /work/build -DCMAKE_BUILD_TYPE=Release -DBUILD_LLVM_DWARF=ON \
  -DBUILD_TESTS=OFF -DBUILD_SHARED_LIBS=OFF -DBYN_ENABLE_LTO=OFF \
  -DENABLE_WERROR=OFF -DCMAKE_EXE_LINKER_FLAGS=-static
cmake --build /work/build --target wasm-opt --parallel "$JOBS"
mkdir -p /work/out/bin
cp /work/build/bin/wasm-opt /work/out/bin/wasm-opt"#;

/// The debug build's wasm-opt for threaded games (D57): Binaryen 133 from
/// `[toolchain.binaryen-src]` with the DWARF repair patches in
/// `toolchain/binaryen-dwarf/`, built in Docker on `[oracle]`'s pinned base
/// image and Debian snapshot, so the compiler is pinned too. Cached under
/// `.toolchain/binaryen-dwarf-<hash of all of those>/`. Release builds keep
/// `[toolchain.binaryen]`.
pub fn dwarf_binaryen(ws: &Workspace) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};
    let src = ws.lock.toolchain.get("binaryen-src").context("inputs.lock has no [toolchain.binaryen-src]")?;
    let base = ws.lock.oracle.as_ref().context("inputs.lock has no [oracle] (the build image's base)")?;
    let dir = ws.root.join(DWARF_BINARYEN);
    let mut id = Sha256::new();
    for part in [&src.sha256, &base.base_image, &base.debian_snapshot, DWARF_BINARYEN_BUILD] {
        id.update(part.as_bytes());
        id.update([0]);
    }
    let mut patches = serde_json::Map::new();
    for name in ["build.Dockerfile"].into_iter().chain(DWARF_BINARYEN_PATCHES) {
        let bytes = std::fs::read(dir.join(name)).with_context(|| format!("reading {DWARF_BINARYEN}/{name}"))?;
        id.update(&bytes);
        id.update([0]);
        patches.insert(name.into(), crate::sha256_file(&dir.join(name))?.into());
    }
    let id = crate::hex(&id.finalize());
    let dest = ws.toolchain_dir().join(format!("binaryen-dwarf-{}", &id[..16]));
    let tool = dest.join("bin/wasm-opt");
    if tool.is_file() {
        return Ok(tool);
    }

    fetch(ws, "binaryen-src")?;
    let docker = |args: &[&str]| -> Result<String> {
        let out = Command::new("docker").args(args).output().context("running docker")?;
        ensure!(out.status.success(), "docker {} failed: {}", args[0], String::from_utf8_lossy(&out.stderr).trim());
        Ok(String::from_utf8(out.stdout)?.trim().to_string())
    };
    eprintln!("binaryen-dwarf: building the build image {DWARF_BINARYEN_IMAGE}");
    let dockerfile = dir.join("build.Dockerfile");
    let image = docker(&[
        "build",
        "-q",
        "-f",
        &dockerfile.to_string_lossy(),
        "-t",
        DWARF_BINARYEN_IMAGE,
        "--build-arg",
        &format!("BASE_IMAGE={}", base.base_image),
        "--build-arg",
        &format!("DEBIAN_SNAPSHOT={}", base.debian_snapshot),
        &ws.root.join("oracle/docker").to_string_lossy(),
    ])?;

    let work = ws.toolchain_dir().join(".binaryen-dwarf-build");
    if work.exists() {
        std::fs::remove_dir_all(&work)?;
    }
    std::fs::create_dir_all(&work)?;
    let user = {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(&work)?;
        format!("{}:{}", m.uid(), m.gid())
    };
    let jobs = std::thread::available_parallelism().map_or(4, |n| n.get()).to_string();
    let log = work.join("build.log");
    eprintln!("binaryen-dwarf: building wasm-opt in Docker (log: {})", log.display());
    let mount = |host: PathBuf, guest: &str, ro: bool| format!("{}:{guest}{}", host.display(), if ro { ":ro" } else { "" });
    let file = std::fs::File::create(&log)?;
    let status = Command::new("docker")
        .args(["run", "--rm", "--network=none", "--user", &user, "-e", &format!("JOBS={jobs}")])
        .args(["-v", &mount(ws.toolchain_dir().join(&src.dist), "/src", true)])
        .args(["-v", &mount(dir.clone(), "/patches", true)])
        .args(["-v", &mount(work.clone(), "/work", false)])
        .args([DWARF_BINARYEN_IMAGE, "sh", "-euc", DWARF_BINARYEN_BUILD, "build"])
        .args(DWARF_BINARYEN_PATCHES)
        .stdout(file.try_clone()?)
        .stderr(file)
        .status()
        .context("running docker")?;
    ensure!(status.success(), "building the debug wasm-opt failed; see {}", log.display());

    let out = work.join("out");
    let provenance = serde_json::json!({
        "source": {"url": src.url, "sha256": src.sha256},
        "files": patches,
        "base_image": base.base_image,
        "debian_snapshot": base.debian_snapshot,
        "build_image": image,
        "wasm_opt_sha256": crate::sha256_file(&out.join("bin/wasm-opt"))?,
    });
    std::fs::write(out.join("provenance.json"), serde_json::to_string_pretty(&provenance)? + "\n")?;
    std::fs::rename(&log, out.join("build.log"))?;
    std::fs::rename(&out, &dest)?;
    std::fs::remove_dir_all(&work)?;
    eprintln!("binaryen-dwarf: {} ready", dest.display());
    Ok(tool)
}

/// Download, verify and unpack `[toolchain.<name>]`. A no-op when present.
pub fn fetch(ws: &Workspace, name: &str) -> Result<()> {
    let pin = ws
        .lock
        .toolchain
        .get(name)
        .ok_or_else(|| anyhow!("inputs.lock has no [toolchain.{name}]"))?;
    let dest = ws.toolchain_dir().join(&pin.dist);
    if dest.is_dir() {
        eprintln!("{name}: {} already present", pin.dist);
        return Ok(());
    }
    let dl = ws.toolchain_dir().join("dl");
    std::fs::create_dir_all(&dl)?;
    let archive = dl.join(format!("{}.tar.gz", pin.dist));
    eprintln!("{name}: downloading {}", pin.url);
    let ok = Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(&archive)
        .arg(&pin.url)
        .status()?
        .success();
    if !ok {
        bail!("download of {} failed", pin.url);
    }
    let got = crate::sha256_file(&archive)?;
    if got != pin.sha256 {
        bail!("{}: sha256 {got} != pinned {}", archive.display(), pin.sha256);
    }
    let ok = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(ws.toolchain_dir())
        .status()?
        .success();
    if !ok || !dest.is_dir() {
        bail!("unpacking {} did not produce {}", archive.display(), dest.display());
    }
    std::fs::remove_dir_all(&dl)?;
    eprintln!("{name}: {} ready", pin.dist);
    Ok(())
}
