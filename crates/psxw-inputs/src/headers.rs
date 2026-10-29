//! The decomp's headers: which ones the census probes, the header each
//! `#include` names, and how C written outside the snapshot (probe files,
//! mods) names them.
//!
//! A header is its path in the snapshot (`include/common.h`, or
//! `src/overlays/ovl_11/foo.h` beside its C). An `#include` resolves as
//! the preprocessor resolves it: `"…"` beside the including file first,
//! then each `[c] include` dir in order. Generated C names a header from
//! the include dir that finds it (`common.h`, `psyq/libgpu.h`); a header
//! no include dir finds is named by its path, from the snapshot root.

use crate::GameManifest;
use crate::map::AddressMap;
use anyhow::Result;
use std::collections::{BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

/// The game's headers in one snapshot.
#[derive(Debug, Clone)]
pub struct Headers {
    /// The snapshot, absolute.
    root: PathBuf,
    /// `[c] include`, relative to the snapshot.
    include: Vec<PathBuf>,
    /// The headers the census probes (`[probe] include_dirs` minus
    /// `[probe] exclude`), sorted by dir, then path under it.
    pub list: Vec<Header>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Its path in the snapshot.
    pub path: String,
    /// How an `#include` names it.
    pub spelling: String,
}

impl Headers {
    /// Resolution and spelling only; no headers listed.
    pub fn new(snap: &Path, include: &[PathBuf]) -> Result<Self> {
        Ok(Self { root: normalize(&std::path::absolute(snap)?), include: include.iter().map(|d| normalize(d)).collect(), list: Vec::new() })
    }

    /// The headers under `[probe] include_dirs`.
    pub fn load(snap: &Path, manifest: &GameManifest) -> Result<Self> {
        let mut headers = Self::new(snap, &manifest.c.include)?;
        let mut found: Vec<(String, String, String)> = Vec::new();
        for dir in &manifest.probe.include_dirs {
            let root = snap.join(dir);
            let mut stack = vec![root.clone()];
            while let Some(d) = stack.pop() {
                for e in std::fs::read_dir(&d)? {
                    let p = e?.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if p.extension().is_some_and(|x| x == "h") {
                        let rel = p.strip_prefix(&root)?.to_string_lossy().into_owned();
                        let name = p.file_name().unwrap_or_default().to_string_lossy();
                        let path = text(&normalize(&dir.join(&rel)));
                        if !manifest.probe.exclude.iter().any(|x| *x == rel || *x == name || *x == path) {
                            found.push((dir.to_string_lossy().into_owned(), rel, path));
                        }
                    }
                }
            }
        }
        found.sort();
        headers.list = found
            .into_iter()
            .map(|(dir, rel, path)| {
                // Named from the dir it was found in when that's an include
                // dir that finds it, else from the first that does.
                let from_dir = headers.include.contains(&normalize(Path::new(&dir))) && headers.resolve(None, &rel).as_deref() == Some(path.as_str());
                let spelling = if from_dir { rel } else { headers.spelling(&path) };
                Header { path, spelling }
            })
            .collect();
        Ok(headers)
    }

    /// The header an `#include` names, as the preprocessor finds it:
    /// beside `from` (the including file; `None` for `<…>`), then in each
    /// include dir. `None` when it isn't in the snapshot.
    pub fn resolve(&self, from: Option<&str>, spelling: &str) -> Option<String> {
        let beside = from.map(|f| Path::new(f).parent().unwrap_or(Path::new("")).to_path_buf());
        beside
            .into_iter()
            .chain(self.include.iter().cloned())
            .filter_map(|d| self.inside(&d.join(spelling)))
            .find(|p| self.root.join(p).is_file())
            .map(|p| text(&p))
    }

    /// How generated C names the header at `path`: from the first include
    /// dir that finds it by that name, else by `path` itself.
    pub fn spelling(&self, path: &str) -> String {
        for d in &self.include {
            if let Ok(rel) = Path::new(path).strip_prefix(d) {
                let rel = text(rel);
                if self.resolve(None, &rel).as_deref() == Some(path) {
                    return rel;
                }
            }
        }
        path.to_string()
    }

    /// The header an include dir finds by `spelling`, named so.
    pub fn named(&self, spelling: &str) -> Option<Header> {
        self.resolve(None, spelling).map(|path| Header { path, spelling: spelling.to_string() })
    }

    /// Whether `spelling` names a header from the snapshot root: no include
    /// dir has it, but the snapshot does at that path.
    pub fn rooted(&self, spelling: &str) -> bool {
        self.resolve(None, spelling).is_none() && self.inside(Path::new(spelling)).is_some_and(|p| self.root.join(p).is_file())
    }

    /// For each header (by path), the includes preceding it in the first
    /// translation unit that includes it: the context the decomp compiles
    /// it in, since headers like the SDK's depend on include order. Each
    /// is spelled as written when an include dir finds the same header by
    /// it, else as [`Headers::spelling`] names it.
    pub fn contexts(&self, map: &AddressMap) -> HashMap<String, Vec<String>> {
        self.contexts_of(map.containers.iter().flat_map(|c| &c.tus).map(|t| t.path.as_path()))
    }

    fn contexts_of<'a>(&self, tus: impl IntoIterator<Item = &'a Path>) -> HashMap<String, Vec<String>> {
        let mut contexts = HashMap::new();
        for tu in tus {
            let Ok(source) = std::fs::read_to_string(self.root.join(tu)) else { continue };
            let from = text(tu);
            let mut before: Vec<String> = Vec::new();
            for line in source.lines() {
                let Some(rest) = line.trim_start().strip_prefix("#include") else { continue };
                let rest = rest.trim_start();
                let close = match rest.chars().next() {
                    Some('"') => '"',
                    Some('<') => '>',
                    _ => continue,
                };
                let Some(end) = rest[1..].find(close) else { continue };
                let inc = rest[1..1 + end].to_string();
                let found = self.resolve((close == '"').then_some(from.as_str()), &inc);
                let spelled = match &found {
                    Some(p) if self.resolve(None, &inc).as_ref() != Some(p) => self.spelling(p),
                    _ => inc,
                };
                if let Some(p) = found {
                    contexts.entry(p).or_insert_with(|| before.clone());
                }
                before.push(spelled);
            }
        }
        contexts
    }

    /// Rooted spellings among the listed headers and `contexts`: what a
    /// copy of the headers needs beyond the include dirs.
    pub fn rooted_in(&self, contexts: &HashMap<String, Vec<String>>) -> BTreeSet<String> {
        self.list.iter().map(|h| &h.spelling).chain(contexts.values().flatten()).filter(|s| self.rooted(s)).cloned().collect()
    }

    /// The `#include` line for `spelling` in a file outside the snapshot:
    /// as spelled when an include dir finds it, else the absolute path of
    /// the snapshot's header.
    pub fn include_line(&self, spelling: &str) -> String {
        let operand = if self.rooted(spelling) { text(&self.root.join(spelling)) } else { spelling.to_string() };
        format!("#include \"{operand}\"\n")
    }

    /// A generated file's includes: `context`, then `spelling`.
    pub fn head(&self, context: &[String], spelling: &str) -> String {
        context.iter().map(String::as_str).chain([spelling]).map(|s| self.include_line(s)).collect()
    }

    /// Whether a line marker's file name is `header`.
    pub fn marks(&self, marker: &str, header: &Header) -> bool {
        if marker == header.path {
            return true;
        }
        let name = Path::new(&header.path).file_name().unwrap_or_default().to_string_lossy();
        marker.ends_with(&*name) && self.inside(Path::new(marker)).is_some_and(|p| text(&p) == header.path)
    }

    /// `p` relative to the snapshot, `.` and `..` resolved; `None` outside it.
    fn inside(&self, p: &Path) -> Option<PathBuf> {
        let rel = if p.is_absolute() { normalize(p).strip_prefix(&self.root).ok()?.to_path_buf() } else { normalize(p) };
        (!rel.starts_with("..")).then_some(rel)
    }
}

/// `.` and `..` resolved lexically.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir if out.file_name().is_some() => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

fn text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[(&str, &str)]) -> PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("psxw-headers-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (path, body) in files {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        root
    }

    fn headers(root: &Path) -> Headers {
        Headers::new(root, &["include".into(), "include/psyq".into()]).unwrap()
    }

    #[test]
    fn include_dirs_spell_as_before() {
        let root = tree(&[("include/common.h", ""), ("include/psyq/libgpu.h", ""), ("src/a.c", "")]);
        let h = headers(&root);
        assert_eq!(h.resolve(Some("src/a.c"), "psyq/libgpu.h").as_deref(), Some("include/psyq/libgpu.h"));
        assert_eq!(h.resolve(None, "libgpu.h").as_deref(), Some("include/psyq/libgpu.h"));
        assert_eq!(h.spelling("include/psyq/libgpu.h"), "psyq/libgpu.h");
        assert_eq!(h.spelling("include/common.h"), "common.h");
        assert!(!h.rooted("common.h"));
        assert_eq!(h.include_line("common.h"), "#include \"common.h\"\n");
    }

    #[test]
    fn beside_the_c_first() {
        let root = tree(&[("include/foo.h", ""), ("src/ovl/foo.h", ""), ("src/ovl/bar.h", ""), ("src/x.h", "")]);
        let h = headers(&root);
        assert_eq!(h.resolve(Some("src/ovl/a.c"), "foo.h").as_deref(), Some("src/ovl/foo.h"));
        assert_eq!(h.resolve(None, "foo.h").as_deref(), Some("include/foo.h"));
        assert_eq!(h.resolve(Some("src/ovl/a.c"), "../x.h").as_deref(), Some("src/x.h"));
        assert_eq!(h.resolve(Some("src/a.c"), "../../outside.h"), None);
        assert_eq!(h.spelling("src/ovl/bar.h"), "src/ovl/bar.h");
        assert!(h.rooted("src/ovl/bar.h"));
        let abs = root.join("src/ovl/bar.h");
        assert_eq!(h.include_line("src/ovl/bar.h"), format!("#include \"{}\"\n", abs.display()));
    }

    #[test]
    fn contexts_by_header() {
        let root = tree(&[
            ("include/common.h", ""),
            ("src/o1/foo.h", ""),
            ("src/o2/foo.h", ""),
            ("src/o1/a.c", "#include \"common.h\"\n#include \"foo.h\"\n"),
            ("src/o2/b.c", "#include <common.h>\n#include \"missing.h\"\n#include \"foo.h\"\n"),
            ("src/o1/d.c", "#include \"foo.h\"\n#include \"common.h\"\n"),
        ]);
        let h = headers(&root);
        let c = h.contexts_of([Path::new("src/o1/a.c"), Path::new("src/o2/b.c")]);
        assert_eq!(c["include/common.h"], Vec::<String>::new());
        assert_eq!(c["src/o1/foo.h"], vec!["common.h"]);
        assert_eq!(c["src/o2/foo.h"], vec!["common.h", "missing.h"]);
        assert!(!c.contains_key("missing.h"));
        // A header found beside the C is named by its path.
        let c = h.contexts_of([Path::new("src/o1/d.c")]);
        assert_eq!(c["include/common.h"], vec!["src/o1/foo.h"]);
    }

    #[test]
    fn markers_by_path() {
        let root = tree(&[("include/common.h", ""), ("src/o1/foo.h", "")]);
        let h = headers(&root);
        let common = h.named("common.h").unwrap();
        assert!(h.marks("include/common.h", &common));
        assert!(h.marks("./include/psyq/../common.h", &common));
        assert!(!h.marks("include/psyq/common.h", &common));
        let foo = Header { path: "src/o1/foo.h".into(), spelling: "src/o1/foo.h".into() };
        assert!(h.marks(&root.join("src/o1/foo.h").to_string_lossy(), &foo));
        assert!(!h.marks("/elsewhere/src/o1/foo.h", &foo));
    }
}
