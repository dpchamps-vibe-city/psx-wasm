//! Native host: embeds `guest.wasm` in wasmtime and provides the `psx.*`
//! imports from `psx-runtime` directly. Headless mode is the test bench:
//! deterministic input replays, golden frame hashes, PNG dumps, per-tier
//! execution profiles. Optional window via winit + wgpu + cpal.
