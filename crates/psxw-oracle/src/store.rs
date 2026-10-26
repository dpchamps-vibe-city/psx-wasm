//! The oracle's on-disk stores, all under `.oracle/` (gitignored: vectors
//! and goldens are game-derived once the recorder runs on a game):
//!
//! ```text
//! .oracle/vectors/<id>.vec              canonical vector encoding
//! .oracle/blobs/<sha256>                large state, stored once
//! .oracle/sets/<name>.ids               named vector lists, one id per line
//! .oracle/goldens/<target>/<id>.res     a target's result bytes
//! .oracle/goldens/<target>/<id>.json    its provenance
//! ```
//!
//! Content-addressed files are written once and verified on read.

use crate::golden::Provenance;
use crate::hash::Hash;
use crate::vector::{BlobRef, Vector, VectorId};
use anyhow::{Context, Result, bail, ensure};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

/// Write via a temporary file and rename, so readers never see a partial file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = path.with_extension("partial");
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))?;
    Ok(())
}

impl Store {
    /// The store rooted at `root` (normally `<workspace>/.oracle`).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Store { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn blob_path(&self, h: &Hash) -> PathBuf {
        self.root.join("blobs").join(h.hex())
    }

    fn vector_path(&self, id: &VectorId) -> PathBuf {
        self.root.join("vectors").join(format!("{}.vec", id.hex()))
    }

    fn set_path(&self, name: &str) -> PathBuf {
        self.root.join("sets").join(format!("{name}.ids"))
    }

    pub fn golden_dir(&self, target: &str) -> PathBuf {
        self.root.join("goldens").join(target)
    }

    pub fn put_blob(&self, bytes: &[u8]) -> Result<BlobRef> {
        let h = Hash::of(bytes);
        let path = self.blob_path(&h);
        if !path.is_file() {
            write_atomic(&path, bytes)?;
        }
        Ok(BlobRef(h))
    }

    pub fn blob(&self, r: &BlobRef) -> Result<Vec<u8>> {
        let path = self.blob_path(&r.0);
        let bytes = std::fs::read(&path).with_context(|| format!("blob {} missing", r.0.short()))?;
        ensure!(Hash::of(&bytes) == r.0, "blob {} is corrupt", path.display());
        Ok(bytes)
    }

    /// Store a vector (after checking its shape and that its blobs exist).
    pub fn put_vector(&self, v: &Vector) -> Result<VectorId> {
        v.check()?;
        for b in v.blobs() {
            ensure!(self.blob_path(&b.0).is_file(), "vector references missing blob {}", b.0.short());
        }
        let bytes = v.encode();
        let id = Hash::of(&bytes);
        let path = self.vector_path(&id);
        if !path.is_file() {
            write_atomic(&path, &bytes)?;
        }
        Ok(id)
    }

    pub fn vector(&self, id: &VectorId) -> Result<Vector> {
        let path = self.vector_path(id);
        let bytes = std::fs::read(&path).with_context(|| format!("vector {} missing", id.short()))?;
        ensure!(Hash::of(&bytes) == *id, "vector {} is corrupt", path.display());
        Vector::decode(&bytes).with_context(|| format!("decoding {}", path.display()))
    }

    /// Every stored vector id, sorted.
    pub fn vector_ids(&self) -> Result<Vec<VectorId>> {
        let dir = self.root.join("vectors");
        let mut ids = Vec::new();
        if dir.is_dir() {
            for e in std::fs::read_dir(&dir)? {
                let name = e?.file_name();
                let name = name.to_string_lossy();
                if let Some(hex) = name.strip_suffix(".vec") {
                    ids.push(Hash::parse(hex)?);
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// A named set's ids (empty if the set does not exist).
    pub fn set(&self, name: &str) -> Result<BTreeSet<VectorId>> {
        let path = self.set_path(name);
        let mut out = BTreeSet::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                out.insert(Hash::parse(line).with_context(|| format!("in {}", path.display()))?);
            }
        }
        Ok(out)
    }

    /// Add ids to a named set.
    pub fn add_to_set(&self, name: &str, ids: impl IntoIterator<Item = VectorId>) -> Result<()> {
        ensure!(
            !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
            "bad set name {name:?}"
        );
        let mut set = self.set(name)?;
        let before = set.len();
        set.extend(ids);
        if set.len() != before || !self.set_path(name).is_file() {
            let text: String = set.iter().map(|id| format!("{}\n", id.hex())).collect();
            write_atomic(&self.set_path(name), text.as_bytes())?;
        }
        Ok(())
    }

    pub fn set_names(&self) -> Result<Vec<String>> {
        let dir = self.root.join("sets");
        let mut names = Vec::new();
        if dir.is_dir() {
            for e in std::fs::read_dir(&dir)? {
                let name = e?.file_name().to_string_lossy().into_owned();
                if let Some(n) = name.strip_suffix(".ids") {
                    names.push(n.to_string());
                }
            }
        }
        names.sort();
        Ok(names)
    }

    /// A target's golden for a vector, if any.
    pub fn golden(&self, target: &str, id: &VectorId) -> Result<Option<(Vec<u8>, Provenance)>> {
        let dir = self.golden_dir(target);
        let res = dir.join(format!("{}.res", id.hex()));
        if !res.is_file() {
            return Ok(None);
        }
        let bytes = std::fs::read(&res)?;
        let json = dir.join(format!("{}.json", id.hex()));
        let prov: Provenance = serde_json::from_slice(&std::fs::read(&json).with_context(|| format!("{} missing", json.display()))?)
            .with_context(|| format!("parsing {}", json.display()))?;
        if prov.result_sha256 != Hash::of(&bytes).hex() {
            bail!("golden {} does not match its provenance's result hash", res.display());
        }
        Ok(Some((bytes, prov)))
    }

    pub fn has_golden(&self, target: &str, id: &VectorId) -> bool {
        self.golden_dir(target).join(format!("{}.res", id.hex())).is_file()
    }

    /// Store a golden. The provenance's result hash must match.
    pub fn put_golden(&self, id: &VectorId, result: &[u8], prov: &Provenance) -> Result<()> {
        ensure!(prov.result_sha256 == Hash::of(result).hex(), "provenance result hash mismatch");
        ensure!(prov.vector == id.hex(), "provenance names vector {}", prov.vector);
        let dir = self.golden_dir(&prov.target);
        write_atomic(&dir.join(format!("{}.json", id.hex())), serde_json::to_string_pretty(prov)?.as_bytes())?;
        write_atomic(&dir.join(format!("{}.res", id.hex())), result)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::golden::Provenance;

    pub(crate) fn temp_store(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("psxw-oracle-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Store::new(dir)
    }

    #[test]
    fn vectors_blobs_sets_goldens() {
        let s = temp_store("store");
        let b = s.put_blob(&[1, 2, 3, 4]).unwrap();
        assert_eq!(s.blob(&b).unwrap(), vec![1, 2, 3, 4]);
        let v = Vector::Echo(b"hi".to_vec());
        let id = s.put_vector(&v).unwrap();
        assert_eq!(s.vector(&id).unwrap(), v);
        assert_eq!(s.vector_ids().unwrap(), vec![id]);
        s.add_to_set("handwritten", [id]).unwrap();
        assert!(s.set("handwritten").unwrap().contains(&id));
        let prov = Provenance::exact(&id, b"hi", "test", "echo");
        s.put_golden(&id, b"hi", &prov).unwrap();
        let (res, p) = s.golden("exact", &id).unwrap().unwrap();
        assert_eq!(res, b"hi");
        assert_eq!(p.target, "exact");
        let _ = std::fs::remove_dir_all(s.root());
    }
}
