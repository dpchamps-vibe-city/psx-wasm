use crate::diag::{self, Diag};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The pinned clang, configured for one guest project.
#[derive(Debug, Clone)]
pub struct Clang {
    pub path: PathBuf,
    /// Project root; include paths and sources are relative to it.
    pub cwd: PathBuf,
    pub includes: Vec<PathBuf>,
    pub defines: Vec<String>,
    /// Where the prelude header is written.
    pub prelude: PathBuf,
}

/// Errors that GCC 2.95-era C trips in modern clang but that are only
/// warnings there. Downgraded so the census can count them instead.
const DOWNGRADED: &[&str] = &[
    "int-conversion",
    "incompatible-pointer-types",
    "incompatible-function-pointer-types",
    "implicit-function-declaration",
    "implicit-int",
    "return-type",
    "return-mismatch",
];

impl Clang {
    pub fn new(path: PathBuf, cwd: PathBuf, includes: Vec<PathBuf>, defines: Vec<String>, work: &Path) -> Result<Self> {
        std::fs::create_dir_all(work)?;
        let prelude = work.join("psxw_prelude.h");
        std::fs::write(&prelude, crate::PRELUDE)?;
        Ok(Self { path, cwd, includes, defines, prelude })
    }

    pub fn version(&self) -> Result<String> {
        let out = Command::new(&self.path).arg("--version").output()?;
        Ok(String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or_default().to_string())
    }

    fn base(&self) -> Command {
        let mut cmd = Command::new(&self.path);
        cmd.current_dir(&self.cwd).args([
            "--no-default-config",
            "--target=wasm32-unknown-unknown",
            "-fno-color-diagnostics",
        ]);
        cmd
    }

    /// Preprocess a source (with the prelude) to text with line markers.
    pub fn preprocess(&self, src: &Path, extra_defines: &[&str]) -> Result<String> {
        let mut cmd = self.base();
        cmd.args(["-E", "-nostdinc", "-undef", "-std=gnu89"]).arg("-include").arg(&self.prelude);
        for inc in &self.includes {
            cmd.arg("-I").arg(inc);
        }
        for d in self.defines.iter().map(String::as_str).chain(extra_defines.iter().copied()) {
            cmd.arg(format!("-D{d}"));
        }
        let out = cmd.arg(src).output().context("running clang -E")?;
        if !out.status.success() {
            bail!("clang -E {} failed:\n{}", src.display(), String::from_utf8_lossy(&out.stderr));
        }
        Ok(String::from_utf8(out.stdout)?)
    }

    /// Compile preprocessed text to a wasm object. Diagnostics locate
    /// physical lines of `i_path` (presumed locations are disabled).
    pub fn compile_i(&self, text: &str, i_path: &Path, obj: &Path) -> Result<(bool, Vec<Diag>)> {
        std::fs::write(i_path, text)?;
        let mut cmd = self.base();
        cmd.args(["-c", "-O2", "-fno-builtin", "-fno-caret-diagnostics", "-ferror-limit=0"])
            .args(["-Xclang", "-fno-diagnostics-use-presumed-location"])
            .args(crate::UB_CONTAINMENT_FLAGS);
        for w in DOWNGRADED {
            cmd.arg(format!("-Wno-error={w}"));
        }
        let out = cmd.arg(i_path).arg("-o").arg(obj).output().context("running clang -c")?;
        Ok((out.status.success(), diag::parse_clang(&String::from_utf8_lossy(&out.stderr))))
    }

    /// Compile self-contained generated C (the lift tier) with the same
    /// optimization and UB-containment flags as guest C. Returns success and
    /// clang's diagnostics.
    pub fn compile_generated(&self, src: &Path, obj: &Path, include: &Path) -> Result<(bool, String)> {
        let mut cmd = self.base();
        cmd.args(["-c", "-O2", "-fno-builtin", "-nostdinc", "-fno-caret-diagnostics"])
            .args(["-Wno-unused-label", "-Wno-unused-variable", "-Wno-unused-but-set-variable"])
            .args(crate::UB_CONTAINMENT_FLAGS)
            .arg("-I")
            .arg(include);
        let out = cmd.arg(src).arg("-o").arg(obj).output().context("running clang -c")?;
        Ok((out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned()))
    }
}
