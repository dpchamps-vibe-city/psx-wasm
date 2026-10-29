//! Verdict classes (plans/conventions.md "Reports separate verdict classes").

use crate::vector::Kind;
use serde::Serialize;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// Checked against a true oracle: exact by construction, or hardware.
    Verified,
    /// Checked against an emulator golden.
    Provisional,
    /// No golden, or the emulator is a known-weak oracle for the kind.
    Unresolved,
    Failed,
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Verdict::Verified => "verified",
            Verdict::Provisional => "provisional",
            Verdict::Unresolved => "unresolved",
            Verdict::Failed => "failed",
        })
    }
}

/// Which reference decided a verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Reference {
    Exact,
    Hardware,
    Emulator,
    None,
}

/// Kinds for which an emulator golden is not evidence (design: "Oracle
/// status per device"). None of the v1 kinds; CD timing joins in M6.
pub fn emulator_weak(kind: Kind) -> bool {
    match kind {
        Kind::Echo | Kind::GpuDraw | Kind::GpuTransfer | Kind::GteOp | Kind::SpuRun | Kind::BiosCall | Kind::MdecRun | Kind::BiosThreads => false,
        // The interrupts, responses and sector data are evidence; the
        // times recorded with them are not (D48), and the check leaves
        // them out.
        Kind::CdCmds => false,
        // The check ignores when the captures were read; their values are
        // evidence.
        Kind::CdXa => false,
    }
}

/// Kinds whose result includes when the runner happened to read a buffer
/// still being written, which depends on what ran earlier in the session
/// (an XA capture: the seek starts from wherever the head was). Recapture
/// can't reproduce them byte for byte; their checks compare values, not
/// bytes (`DeviceUnderTest::consistent`).
pub fn timing_variable(kind: Kind) -> bool {
    matches!(kind, Kind::CdXa | Kind::CdCmds)
}

/// For a timing-variable vector whose result separates values from
/// timing: whether two results agree on everything timing doesn't decide.
/// `None` when the kind's values can't be told apart from its timing (an
/// XA capture).
pub fn same_but_timing(v: &crate::vector::Vector, a: &[u8], b: &[u8]) -> Option<bool> {
    match v {
        crate::vector::Vector::CdCmds(c) => Some(a.len() == b.len() && crate::cdc::values(c, a) == crate::cdc::values(c, b)),
        _ => None,
    }
}

/// The verdict for `result`, given the references available. The
/// strongest reference present decides; a weaker one is never consulted
/// when a stronger one exists.
pub fn judge(kind: Kind, result: &[u8], exact: Option<&[u8]>, hardware: Option<&[u8]>, emulator: Option<&[u8]>) -> (Verdict, Reference) {
    if let Some(e) = exact {
        return (if e == result { Verdict::Verified } else { Verdict::Failed }, Reference::Exact);
    }
    if let Some(h) = hardware {
        return (if h == result { Verdict::Verified } else { Verdict::Failed }, Reference::Hardware);
    }
    match emulator {
        Some(e) if !emulator_weak(kind) => (if e == result { Verdict::Provisional } else { Verdict::Failed }, Reference::Emulator),
        _ => (Verdict::Unresolved, Reference::None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strongest_reference_decides() {
        let k = Kind::GpuDraw;
        assert_eq!(judge(k, b"a", Some(b"a"), None, Some(b"b")).0, Verdict::Verified);
        assert_eq!(judge(k, b"a", Some(b"b"), None, Some(b"a")).0, Verdict::Failed);
        assert_eq!(judge(k, b"a", None, None, Some(b"a")).0, Verdict::Provisional);
        assert_eq!(judge(k, b"a", None, None, Some(b"b")).0, Verdict::Failed);
        assert_eq!(judge(k, b"a", None, None, None).0, Verdict::Unresolved);
    }
}
