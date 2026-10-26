//! The device oracle harness (docs/design.md "The device oracle harness",
//! plans/M03-oracle-harness.md): vectors and their canonical encoding, the
//! blob, vector and golden stores, provenance, verdicts, the runner wire
//! protocol, device targets (the dockerized emulator now, hardware later),
//! capture, compare and reports, the exact-by-construction generators, and
//! the recorder interface devices call.
//!
//! Everything game-derived that this crate writes lives under `.oracle/`
//! (gitignored); reports go under `out/oracle/`.

pub mod commands;
pub mod exact;
pub mod golden;
pub mod gpu_synth;
pub mod handwritten;
pub mod hash;
pub mod images;
pub mod record;
pub mod report;
pub mod script;
pub mod store;
pub mod target;
pub mod vector;
pub mod verdict;
pub mod wire;

pub use hash::Hash;
pub use record::Recorder;
pub use store::Store;
pub use vector::{Kind, Vector, VectorId};
pub use verdict::Verdict;
pub use wire::c_header;
