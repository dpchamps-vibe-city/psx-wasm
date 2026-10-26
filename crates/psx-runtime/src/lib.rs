//! The runtime: the PlayStation's OS and devices as a library
//! (docs/design.md §5–§7). Game-agnostic; built once as `runtime.wasm`
//! (`psxw runtime build`) and merged into each `game.wasm`.
//!
//! - [`boot`] loads the disc's PS-X EXE;
//! - [`driver`] runs BIOS calls and adapters to completion on the virtual
//!   timeline, running callbacks without holding the machine across a
//!   guest call;
//! - the wasm layer (wasm32 only) implements the host and guest imports and
//!   exports the guest's `psx.*` services.

pub use psx_gte as gte;

pub mod boot;
pub mod driver;
#[cfg(target_arch = "wasm32")]
mod wasm;
