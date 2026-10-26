//! Oracle reports: JSON with `schema_version` plus a human summary, under
//! `out/oracle/<command>/`.

use crate::store::write_atomic;
use crate::verdict::Verdict;
use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};

pub const REPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize)]
pub struct Entry {
    pub vector: String,
    pub kind: String,
    /// Generator, set or device the vector belongs to.
    pub group: String,
    pub verdict: Verdict,
    pub reference: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub command: String,
    pub created: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Generator seed (hex), when vectors were generated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
    pub counts: BTreeMap<Verdict, usize>,
    pub by_group: BTreeMap<String, BTreeMap<Verdict, usize>>,
    pub notes: Vec<String>,
    pub pass: bool,
    pub entries: Vec<Entry>,
}

impl Report {
    pub fn new(command: &str) -> Self {
        Report {
            schema_version: REPORT_SCHEMA_VERSION,
            command: command.to_string(),
            created: crate::golden::now_rfc3339(),
            target: None,
            seed: None,
            counts: BTreeMap::new(),
            by_group: BTreeMap::new(),
            notes: Vec::new(),
            pass: true,
            entries: Vec::new(),
        }
    }

    pub fn add(&mut self, e: Entry) {
        *self.counts.entry(e.verdict).or_default() += 1;
        *self.by_group.entry(e.group.clone()).or_default().entry(e.verdict).or_default() += 1;
        if e.verdict == Verdict::Failed {
            self.pass = false;
        }
        self.entries.push(e);
    }

    pub fn count(&self, v: Verdict) -> usize {
        self.counts.get(&v).copied().unwrap_or(0)
    }

    pub fn summary(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "psxw oracle {}: {}", self.command, if self.pass { "PASS" } else { "FAIL" });
        if let Some(t) = &self.target {
            let _ = writeln!(s, "  target: {t}");
        }
        if let Some(seed) = &self.seed {
            let _ = writeln!(s, "  seed: {seed}");
        }
        let line = |m: &BTreeMap<Verdict, usize>| {
            [Verdict::Verified, Verdict::Provisional, Verdict::Unresolved, Verdict::Failed]
                .iter()
                .map(|v| format!("{v} {}", m.get(v).copied().unwrap_or(0)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let _ = writeln!(s, "  {} vectors: {}", self.entries.len(), line(&self.counts));
        for (g, m) in &self.by_group {
            let _ = writeln!(s, "    {g:<22} {}", line(m));
        }
        for n in &self.notes {
            let _ = writeln!(s, "  note: {n}");
        }
        let failed: Vec<_> = self.entries.iter().filter(|e| e.verdict == Verdict::Failed).collect();
        for e in failed.iter().take(20) {
            let _ = writeln!(s, "  FAILED {} {} {}: {}", &e.vector[..12.min(e.vector.len())], e.kind, e.group, e.detail.as_deref().unwrap_or(""));
        }
        if failed.len() > 20 {
            let _ = writeln!(s, "  … {} more failures in the JSON report", failed.len() - 20);
        }
        s
    }

    /// Write `report.json` and `summary.txt` under `dir`.
    pub fn write(&self, dir: &Path) -> Result<PathBuf> {
        let json = dir.join("report.json");
        write_atomic(&json, serde_json::to_string_pretty(self)?.as_bytes())?;
        write_atomic(&dir.join("summary.txt"), self.summary().as_bytes())?;
        Ok(json)
    }
}

/// First difference between two results, for report details.
pub fn first_difference(got: &[u8], want: &[u8]) -> String {
    if got.len() != want.len() {
        return format!("{} bytes, expected {}", got.len(), want.len());
    }
    let n = got.iter().zip(want).filter(|(a, b)| a != b).count();
    match got.iter().zip(want).position(|(a, b)| a != b) {
        Some(i) => format!("{n} bytes differ; first at byte {i}: {:#04x}, expected {:#04x}", got[i], want[i]),
        None => "equal".to_string(),
    }
}
