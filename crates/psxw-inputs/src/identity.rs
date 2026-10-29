//! SDK object identity (plans/completed/M08-sdk-generality.md, task 1): which PsyQ
//! object, of which SDK version, each linked SDK member in a game image
//! is.
//!
//! Members come from the splat `o` subsegments (`lib/<library>/<member>`).
//! Their placed text is matched against the pinned signature database
//! (`[toolchain.psyq-signatures]`: per SDK version and library, each
//! object's text as a byte pattern with relocated bytes masked, plus its
//! functions' offsets). The identity key is the sha256 of the placed text
//! with the signature's masked bytes zeroed: relocation-insensitive, equal
//! for the same object wherever it is linked.

use crate::map::{AddressMap, SdkPlacement};
use crate::originals::Originals;
use crate::workspace::Workspace;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const SIGNATURES: &str = "psyq-signatures";

/// One object's signature: its text, `None` where relocated.
#[derive(Debug, Clone)]
pub struct SigObject {
    pub name: String,
    pub sig: Vec<Option<u8>>,
    /// (function, byte offset in the object's text).
    pub labels: Vec<(String, u32)>,
}

impl SigObject {
    pub fn matches(&self, bytes: &[u8]) -> bool {
        bytes.len() >= self.sig.len() && self.sig.iter().zip(bytes).all(|(s, b)| s.is_none_or(|s| s == *b))
    }

    /// The identity key of `bytes` under this signature's mask.
    pub fn key(&self, bytes: &[u8]) -> String {
        let masked: Vec<u8> = self.sig.iter().zip(bytes).map(|(s, b)| if s.is_some() { *b } else { 0 }).collect();
        crate::sha256_hex(&masked)
    }
}

#[derive(Deserialize)]
struct RawObject {
    name: String,
    /// Absent for objects with no text (only `xbss`).
    #[serde(default)]
    sig: String,
    #[serde(default)]
    labels: Vec<RawLabel>,
}

#[derive(Deserialize)]
struct RawLabel {
    name: String,
    offset: u32,
}

/// version (e.g. `470`) → library file (e.g. `LIBPAD.LIB`) → objects.
#[derive(Debug, Clone, Default)]
pub struct SigDb {
    pub versions: BTreeMap<String, BTreeMap<String, Vec<SigObject>>>,
}

fn parse_sig(s: &str) -> Option<Vec<Option<u8>>> {
    s.split_whitespace().map(|t| if t == "??" { Some(None) } else { u8::from_str_radix(t, 16).ok().map(Some) }).collect()
}

impl SigDb {
    pub fn load(dir: &Path) -> Result<Self> {
        let mut db = SigDb::default();
        for v in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let v = v?.path();
            let Some(version) = v.file_name().and_then(|n| n.to_str()).filter(|n| n.chars().all(|c| c.is_ascii_digit())) else {
                continue;
            };
            let version = version.to_string();
            for f in std::fs::read_dir(&v)? {
                let f = f?.path();
                let Some(lib) = f.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".json")) else { continue };
                let raw: Vec<RawObject> = serde_json::from_slice(&std::fs::read(&f)?).with_context(|| format!("parsing {}", f.display()))?;
                let objs = raw
                    .into_iter()
                    .filter(|o| !o.sig.is_empty())
                    .filter_map(|o| {
                        Some(SigObject {
                            sig: parse_sig(&o.sig)?,
                            name: o.name,
                            labels: o.labels.into_iter().map(|l| (l.name, l.offset)).collect(),
                        })
                    })
                    .collect();
                db.versions.entry(version.clone()).or_default().insert(lib.to_uppercase(), objs);
            }
        }
        Ok(db)
    }

    /// The pinned database.
    pub fn pinned(ws: &Workspace) -> Result<Self> {
        let pin = ws.lock.toolchain.get(SIGNATURES).context("inputs.lock has no [toolchain.psyq-signatures]")?;
        let dir = ws.toolchain_dir().join(&pin.dist);
        anyhow::ensure!(dir.is_dir(), "{} is missing; run `psxw toolchain fetch`", dir.display());
        Self::load(&dir)
    }

    /// Signatures of `<library>/<member>` in every version, as
    /// (version, object). A member `<object>__<function>` is one function
    /// of an object (the decomp split it): its signature is that slice.
    pub fn candidates(&self, member: &str) -> Vec<(&str, SigObject)> {
        let Some((lib, obj)) = member.split_once('/') else { return vec![] };
        let (obj, function) = match obj.split_once("__") {
            Some((o, f)) => (o, Some(f)),
            None => (obj, None),
        };
        let lib_file = format!("{}.LIB", lib.to_uppercase());
        let obj_file = format!("{}.OBJ", obj.to_uppercase());
        let mut out = Vec::new();
        for (v, libs) in &self.versions {
            // Objects linked directly (`2MBYTE.OBJ`) are files of their own.
            let objs = libs.get(&lib_file).or_else(|| libs.get(&obj_file));
            let Some(o) = objs.and_then(|os| os.iter().find(|o| o.name.eq_ignore_ascii_case(&obj_file))) else { continue };
            match function {
                None => out.push((v.as_str(), o.clone())),
                Some(f) => {
                    // The slice from this function's label to the next.
                    let mut offs: Vec<u32> = o.labels.iter().map(|l| l.1).collect();
                    offs.sort();
                    let Some(&(_, at)) = o.labels.iter().find(|(n, _)| n == f) else { continue };
                    let end = offs.iter().copied().find(|&x| x > at).unwrap_or(o.sig.len() as u32) as usize;
                    let sig = o.sig.get(at as usize..end.min(o.sig.len())).unwrap_or(&[]).to_vec();
                    let labels = vec![(f.to_string(), 0)];
                    out.push((v.as_str(), SigObject { name: format!("{}__{f}", o.name), sig, labels }));
                }
            }
        }
        out
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MemberIdentity {
    pub container: String,
    pub member: String,
    pub text: String,
    pub text_bytes: u32,
    /// SDK versions whose signature matches the placed text.
    pub versions: Vec<String>,
    /// Versions with a signature for this object that doesn't match.
    pub mismatched: Vec<String>,
    /// Relocation-insensitive key (from the first matching signature).
    pub key: Option<String>,
    /// Where the signature's function name differs from the address
    /// map's: (signature name, address, map name). Naming only; the
    /// bytes matched.
    pub names_differ: Vec<(String, String, String)>,
    pub how: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct IdentityReport {
    pub schema_version: u32,
    pub game: String,
    pub members: usize,
    pub by_signature: usize,
    pub by_subsegment_only: usize,
    /// Members whose text no signature version matches although some
    /// version has the object (patched or unknown builds).
    pub unmatched: Vec<String>,
    /// SDK versions by member count.
    pub versions: BTreeMap<String, usize>,
    pub identities: Vec<MemberIdentity>,
}

/// Every SDK member placed in the game's text, identified.
pub fn identify(game: &str, map: &AddressMap, originals: &Originals, db: &SigDb) -> IdentityReport {
    let mut ids = Vec::new();
    for c in &map.containers {
        let texts: Vec<&SdkPlacement> = c.sdk.iter().filter(|p| p.is_text()).collect();
        for p in texts {
            let len = (p.vram.end - p.vram.start) as usize;
            let bytes = originals.read(&c.id, p.vram.start, len).unwrap_or(&[]);
            let cands = db.candidates(&p.member);
            let (matched, mismatched): (Vec<_>, Vec<_>) = cands.iter().partition(|(_, o)| o.matches(bytes));
            let key = matched.first().map(|(_, o)| o.key(bytes));
            let matched: Vec<(&str, &SigObject)> = matched.iter().map(|(v, o)| (*v, o)).collect();
            let mismatched: Vec<(&str, &SigObject)> = mismatched.iter().map(|(v, o)| (*v, o)).collect();
            let mut conflicts = Vec::new();
            if let Some((_, o)) = matched.first() {
                for (name, off) in &o.labels {
                    let addr = p.vram.start + off;
                    let here = map.symbols(&c.id).iter().find(|s| s.addr == addr).map(|s| s.name.clone());
                    match here {
                        Some(h) if h != *name && !h.starts_with("func_") => {
                            conflicts.push((name.clone(), format!("0x{addr:08X}"), h))
                        }
                        _ => {}
                    }
                }
            }
            let how = if key.is_some() {
                "signature"
            } else if cands.is_empty() {
                "subsegment (no signature for this object)"
            } else {
                "subsegment (no signature version matches)"
            };
            ids.push(MemberIdentity {
                container: c.id.clone(),
                member: p.member.clone(),
                text: format!("0x{:08X}..0x{:08X}", p.vram.start, p.vram.end),
                text_bytes: len as u32,
                versions: matched.iter().map(|(v, _)| v.to_string()).collect(),
                mismatched: mismatched.iter().map(|(v, _)| v.to_string()).collect(),
                key,
                names_differ: conflicts,
                how,
            });
        }
    }
    let mut versions: BTreeMap<String, usize> = BTreeMap::new();
    for i in &ids {
        for v in &i.versions {
            *versions.entry(v.clone()).or_default() += 1;
        }
    }
    IdentityReport {
        schema_version: 1,
        game: game.into(),
        members: ids.len(),
        by_signature: ids.iter().filter(|i| i.key.is_some()).count(),
        by_subsegment_only: ids.iter().filter(|i| i.key.is_none()).count(),
        unmatched: ids.iter().filter(|i| i.key.is_none() && !i.mismatched.is_empty()).map(|i| i.member.clone()).collect(),
        versions,
        identities: ids,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masked_match_and_key() {
        let o = SigObject { name: "X.OBJ".into(), sig: parse_sig("?? ?? 02 3C 08 00").unwrap(), labels: vec![] };
        assert!(o.matches(&[0x12, 0x34, 0x02, 0x3C, 0x08, 0x00, 0xFF]));
        assert!(!o.matches(&[0x12, 0x34, 0x02, 0x3D, 0x08, 0x00]));
        assert_eq!(o.key(&[0x12, 0x34, 0x02, 0x3C, 0x08, 0x00]), o.key(&[0x99, 0x88, 0x02, 0x3C, 0x08, 0x00]));
    }
}
