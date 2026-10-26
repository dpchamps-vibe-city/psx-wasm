//! Browser host: `psx-runtime` built as `runtime.wasm` (`--import-memory`,
//! `--global-base=0x02000000`) for the game worker. Implements
//! `psx_runtime::Host` with Atomics.wait pacing, OffscreenCanvas
//! presentation, a SharedArrayBuffer audio ring drained by an AudioWorklet,
//! pad state written by the main thread, and disc reads through an OPFS
//! `FileSystemSyncAccessHandle`. JS/TS glue lives in `web/`.
