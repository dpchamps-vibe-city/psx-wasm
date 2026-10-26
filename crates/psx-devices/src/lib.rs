//! Devices the runtime models, in the hardware's own vocabulary
//! (docs/design.md "Runtime"): virtual time, the root counters, the CD.
//! The GPU front end (M4), SPU (M6) and SIO (M7) arrive with their
//! milestones; until then the SDK adapters stub them.
//!
//! Nothing here is a register-level model of the console: devices are
//! driven by the SDK adapters and the BIOS, and their behaviour is a
//! function of virtual time.

pub mod cd;
pub mod clock;
pub mod gpu;
pub mod mem;
pub mod timers;

pub use mem::{Mem, VecMem};
