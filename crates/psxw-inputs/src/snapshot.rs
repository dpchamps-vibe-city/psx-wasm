//! Read-only snapshots of pinned repos via `git archive`. The local checkout
//! is only an object store: no worktree metadata is added to it and its
//! working tree is never read.

use crate::lock::RepoPin;
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const STAMP: &str = ".psxw-snapshot";

/// Return the snapshot directory for `pin`, creating it if missing or stale.
pub fn ensure(ws: &Workspace, name: &str, pin: &RepoPin) -> Result<PathBuf> {
    let dir = ws.inputs_dir().join(format!("{name}-{}", pin.short()));
    if std::fs::read_to_string(dir.join(STAMP)).is_ok_and(|s| s.trim() == pin.commit) {
        return Ok(dir);
    }
    let repo = ws.checkout(pin);
    let status = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["cat-file", "-e", &format!("{}^{{commit}}", pin.commit)])
        .status()
        .with_context(|| format!("running git in {}", repo.display()))?;
    if !status.success() {
        bail!("{} does not contain commit {}", repo.display(), pin.commit);
    }

    let tmp = dir.with_extension("partial");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let mut git = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["archive", "--format=tar", &pin.commit])
        .stdout(Stdio::piped())
        .spawn()?;
    let tar = Command::new("tar")
        .arg("-x")
        .arg("-C")
        .arg(&tmp)
        .stdin(git.stdout.take().expect("piped"))
        .status()?;
    if !git.wait()?.success() || !tar.success() {
        bail!("git archive {} from {} failed", pin.commit, repo.display());
    }
    std::fs::write(tmp.join(STAMP), &pin.commit)?;
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::rename(&tmp, &dir)?;
    Ok(dir)
}
