//! Goldens: a device target's result for a vector, with its provenance.
//!
//! The provenance says where a result came from, precisely enough to
//! reproduce it: the target id, every source commit, the BIOS hash, the
//! emulator's effective core options, the runner version and hash, and when
//! it was made. Hardware results (M10) use the same structure with
//! `source = "hardware"`.

use crate::hash::Hash;
use crate::vector::VectorId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Version of the provenance JSON.
pub const PROVENANCE_SCHEMA_VERSION: u32 = 1;

/// The pseudo-target holding closed-form results of exact-by-construction
/// vectors.
pub const EXACT_TARGET: &str = "exact";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Closed form, computed from the vector's own data.
    Exact,
    /// A headless emulator standing in for the console.
    Emulator,
    /// A real console.
    Hardware,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub schema_version: u32,
    /// e.g. `emu-beetle-7fea57a-openbios-8a86b7e-runner-1`, or `exact`.
    pub target: String,
    pub source: Source,
    /// Vector id (sha256 hex).
    pub vector: String,
    /// Vector kind name.
    pub kind: String,
    /// sha256 of the result bytes.
    pub result_sha256: String,
    /// Source commits and releases, by name.
    #[serde(default)]
    pub commits: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bios_sha1: Option<String>,
    /// Emulator library name and version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core: Option<String>,
    /// Every effective core option.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub core_options: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_version: Option<u32>,
    /// sha256 of the runner program (and its boot disc), by file.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub runner: BTreeMap<String, String>,
    /// Docker image ids, by image name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub images: BTreeMap<String, String>,
    /// sha256 of the target's own binaries (emulator core, host), by file.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub binaries: BTreeMap<String, String>,
    /// For exact results: the generator and its seed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
    /// For hardware results: the rig, console model, BIOS version and
    /// contributor, and hashes of the batch manifest and results
    /// ([`crate::batch`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hardware: BTreeMap<String, String>,
    /// RFC 3339, UTC.
    pub created: String,
}

impl Provenance {
    /// Provenance of a closed-form result.
    pub fn exact(id: &VectorId, result: &[u8], generator: &str, kind: &str) -> Self {
        Provenance {
            schema_version: PROVENANCE_SCHEMA_VERSION,
            target: EXACT_TARGET.to_string(),
            source: Source::Exact,
            vector: id.hex(),
            kind: kind.to_string(),
            result_sha256: Hash::of(result).hex(),
            commits: BTreeMap::new(),
            bios_sha1: None,
            core: None,
            core_options: BTreeMap::new(),
            runner_version: None,
            runner: BTreeMap::new(),
            images: BTreeMap::new(),
            binaries: BTreeMap::new(),
            generator: Some(generator.to_string()),
            hardware: BTreeMap::new(),
            created: now_rfc3339(),
        }
    }

    /// This provenance for another vector and result (a target's template).
    pub fn for_result(&self, id: &VectorId, kind: &str, result: &[u8]) -> Self {
        Provenance {
            vector: id.hex(),
            kind: kind.to_string(),
            result_sha256: Hash::of(result).hex(),
            created: now_rfc3339(),
            ..self.clone()
        }
    }
}

/// Current time as RFC 3339 UTC (`2026-10-26T01:02:03Z`).
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rfc3339(secs)
}

fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_793_000_000), "2026-10-26T07:33:20Z");
    }
}
