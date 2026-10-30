//! The native host (plans/M02): runs `game.wasm` under wasmtime, headless.
//! It supplies the host interface (psxw-abi): the disc image, pad input
//! from a replay, storage, logging, and the vblank (`host.vblank`), which
//! is also where a run stops after its frame budget. It doesn't pace. Per-frame RAM hashes, dumps and a
//! traced word support the determinism and whole-game oracles.

pub mod audio;
pub mod disc;
pub mod mods;
pub mod render;

use anyhow::{Context as _, Result, bail};
use disc::Disc;
use psxw_abi::{BootOptions, WINDOW_BASE, host, replay};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use wasmtime::{Caller, Config, Engine, Linker, Memory, Module, Store};

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub wasm: PathBuf,
    pub disc: PathBuf,
    /// Stop after this many vblanks.
    pub frames: u64,
    pub replay: Option<Vec<u8>>,
    pub boot: BootOptions,
    /// Frames at which to hash guest RAM.
    pub hash_at: Vec<u64>,
    /// Frames at which to dump guest RAM (2 MiB) into `dump_dir`.
    pub dump_at: Vec<u64>,
    pub dump_dir: Option<PathBuf>,
    /// A guest word to record every frame (the game's frame counter).
    pub trace_u32: Option<u32>,
    /// Dump guest RAM into `dump_dir` at the first frame the traced word
    /// equals each of these values (`ram_c<value>.bin`, as the oracle's
    /// emulator host names them).
    pub dump_counter: Vec<u32>,
    pub storage_dir: Option<PathBuf>,
    /// Echo guest logs to stderr.
    pub verbose: bool,
    /// Overlay images whose residency is checked at every vblank.
    pub overlays: Vec<Overlay>,
    /// Wall-clock seconds a conformance call may run before it traps
    /// (a runaway guest loop); 0 = no limit.
    pub deadline_secs: u64,
    /// The renderer; `None` runs headless with [`render::NullRender`].
    pub render: Option<render::SharedRender>,
    /// Frames at which to hash VRAM (needs a renderer with VRAM).
    pub vram_hash_at: Vec<u64>,
    /// Write every `.1`th presented frame to `.0` as `frame_NNNNNN.png`
    /// (needs a renderer that draws).
    pub dump_frames: Option<(PathBuf, u64)>,
    /// With `dump_frames`: also write all of VRAM (1024×512, 15-bit colour
    /// as RGB) as `vram_NNNNNN.png` beside each frame. With
    /// `dump_with_frame`: also write it beside each RAM dump.
    pub dump_vram: bool,
    /// Each RAM dump (`dump_at`, `dump_counter`) also writes the frame
    /// presented at that vblank beside it (`ram_X.bin` → `frame_X.png`;
    /// needs a renderer that draws) and, with `dump_vram`, VRAM as it is
    /// then (`vram_X.bin`: 1024×512 16-bit words, little-endian), as the
    /// oracle's emulator host does.
    pub dump_with_frame: bool,
    /// Write the audio output here as a WAV file.
    pub audio_wav: Option<PathBuf>,
    /// Record GPU linked-list batches (M4's recording hook; needs a
    /// renderer with VRAM).
    pub record_gpu: Option<GpuRecording>,
    /// Record SPU windows (M6's recording hook).
    pub record_spu: Option<SpuRecording>,
    /// Guest values to read at every vblank: (address, size in bytes);
    /// `RunReport::watched` has a row per vblank (a mod test's trace).
    pub watch: Vec<(u32, u32)>,
    /// Mod modules to load into a moddable base before it boots, in
    /// order (plans/M11 phase 2).
    pub mods: Vec<PathBuf>,
    /// Mod modules swapped in at vblanks.
    pub swaps: Vec<ModSwap>,
    /// Host calls (`psxw_execute`) queued at vblanks.
    pub execute: Vec<Execute>,
}

/// At vblank `frame`, the module at `path` replaces loaded mod `index`, or
/// loads after the others when there is no such mod.
#[derive(Debug, Clone)]
pub struct ModSwap {
    pub frame: u64,
    pub index: usize,
    pub path: PathBuf,
}

/// At vblank `frame`, queue a call of `function` (an address in hex, or a
/// loaded mod's global function) with up to four o32 argument words. The
/// runtime runs it once it has released the machine; its result is an
/// `execute` event.
#[derive(Debug, Clone)]
pub struct Execute {
    pub frame: u64,
    pub function: String,
    pub args: Vec<u32>,
}

/// Which SPU windows a run records: one of `samples` at the first vblank
/// at least `every` frames after the last window started, up to `budget`
/// windows. Windows in which SPU RAM changed are never sent.
#[derive(Debug, Clone, Copy)]
pub struct SpuRecording {
    pub every: u64,
    pub samples: u32,
    pub budget: usize,
}

/// A recorded SPU window (`psxw_abi::host::SPU_RECORD`).
#[derive(Debug, Clone)]
pub struct SpuWindow {
    pub frame: u64,
    pub samples: u32,
    pub regs: Vec<(u16, u16)>,
    pub writes: Vec<(u32, u16, u16)>,
    pub ram: Vec<u8>,
}

/// Which GPU batches a run records: the first linked-list transfer at
/// least `every` frames after the last one recorded, up to `budget`.
#[derive(Debug, Clone, Copy)]
pub struct GpuRecording {
    pub every: u64,
    pub budget: usize,
}

/// A recorded GPU linked-list transfer: VRAM before it, the GP0 state
/// words it started from, and its nodes.
#[derive(Debug, Clone)]
pub struct GpuBatch {
    pub frame: u64,
    pub vram: Vec<u16>,
    pub state: Vec<u32>,
    pub nodes: Vec<Vec<u32>>,
}

/// An overlay's original text, by guest address.
#[derive(Debug, Clone, Default)]
pub struct Overlay {
    pub container: String,
    pub text: Vec<(u32, Vec<u8>)>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "detail")]
pub enum Stop {
    /// The frame budget ran out (the normal end of a headless run).
    FrameLimit,
    /// The game's entry point returned.
    Exited,
    /// A runtime or guest trap.
    Trap(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    pub stop: Stop,
    pub frames: u64,
    pub ram_hashes: BTreeMap<u64, String>,
    pub trace: Vec<u32>,
    pub boot: Option<serde_json::Value>,
    /// The loaded text matches the EXE on the disc, checked by the host.
    pub boot_text_matches: Option<bool>,
    pub report: Option<serde_json::Value>,
    pub events: Vec<serde_json::Value>,
    pub logs: Vec<String>,
    pub dumps: Vec<Dump>,
    pub render: render::RenderStats,
    /// VRAM hashes by frame (with a renderer that has VRAM).
    pub vram_hashes: BTreeMap<u64, String>,
    pub audio: audio::AudioStats,
    #[serde(skip)]
    pub gpu_batches: Vec<GpuBatch>,
    #[serde(skip)]
    pub spu_windows: Vec<SpuWindow>,
    /// `Options::watch`'s values at each vblank (zero-extended).
    #[serde(skip)]
    pub watched: Vec<Vec<u32>>,
}

/// A RAM dump written during the run.
#[derive(Debug, Clone, Serialize)]
pub struct Dump {
    pub file: String,
    pub frame: u64,
    /// The traced word's value, for dumps taken by counter.
    pub counter: Option<u32>,
    /// With `dump_with_frame`: the presented frame and VRAM beside it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram: Option<String>,
}

struct State {
    /// None in conformance runs.
    disc: Option<Disc>,
    opts: Options,
    frame: u64,
    logs: Vec<String>,
    events: Vec<serde_json::Value>,
    hashes: BTreeMap<u64, String>,
    trace: Vec<u32>,
    boot: Option<serde_json::Value>,
    boot_ok: Option<bool>,
    stop: Option<Stop>,
    dumps: Vec<Dump>,
    counter_done: Vec<bool>,
    resident: Vec<bool>,
    /// Frame since which an overlay's first words are in RAM without the
    /// rest matching (a load in progress, or a bad load).
    partial_since: Vec<Option<u64>>,
    renderer: render::SharedRender,
    render: render::RenderStats,
    vram_hashes: BTreeMap<u64, String>,
    audio: Option<audio::Sink>,
    /// VRAM taken at `gpu_record_begin`, for the batch now running.
    gpu_vram: Option<Vec<u16>>,
    gpu_batches: Vec<GpuBatch>,
    spu_windows: Vec<SpuWindow>,
    /// The frame the last SPU window was asked for.
    spu_asked: Option<u64>,
    watched: Vec<Vec<u32>>,
    /// Mods loaded at run time.
    loader: Option<mods::Loader>,
    /// The guest window's wasm address (psxw-abi `guest::WINDOW`).
    window: u32,
}

impl State {
    fn new(opts: &Options, disc: Option<Disc>) -> Self {
        State {
            renderer: opts.render.clone().unwrap_or_else(|| std::sync::Arc::new(std::sync::Mutex::new(Box::new(render::NullRender)))),
            render: Default::default(),
            vram_hashes: BTreeMap::new(),
            audio: None,
            gpu_vram: None,
            gpu_batches: vec![],
            spu_windows: vec![],
            spu_asked: None,
            watched: vec![],
            loader: None,
            window: WINDOW_BASE,
            disc,
            opts: opts.clone(),
            frame: 0,
            logs: vec![],
            events: vec![],
            hashes: BTreeMap::new(),
            trace: vec![],
            boot: None,
            boot_ok: None,
            stop: None,
            dumps: vec![],
            counter_done: vec![false; opts.dump_counter.len()],
            resident: vec![false; opts.overlays.len()],
            partial_since: vec![None; opts.overlays.len()],
        }
    }

    fn empty() -> Self {
        State::new(&Options::default(), None)
    }
}

/// A partial overlay older than this many frames is a bad load: longer
/// than any whole-overlay read at CD speed.
const LOAD_FRAMES: u64 = 300;

const FRAME_LIMIT: &str = "psxw: frame limit";

fn mem(caller: &mut Caller<'_, State>) -> Memory {
    caller.get_export("memory").and_then(|e| e.into_memory()).expect("game.wasm exports memory")
}

fn read_bytes(caller: &mut Caller<'_, State>, ptr: u32, len: u32) -> Vec<u8> {
    let m = mem(caller);
    m.data(&caller)[ptr as usize..(ptr + len) as usize].to_vec()
}

/// The guest's 2 MiB of RAM in linear memory, its window at `window`.
fn guest_ram(data: &[u8], window: u32) -> &[u8] {
    &data[window as usize..window as usize + 0x20_0000]
}

/// The window a module put its guest at (psxw-abi `guest::WINDOW`).
fn window_of(store: &mut Store<State>, instance: &wasmtime::Instance) -> Result<u32> {
    match instance.get_typed_func::<(), u32>(&mut *store, psxw_abi::guest::WINDOW) {
        Ok(f) => Ok(f.call(&mut *store, ())?),
        Err(_) => Ok(WINDOW_BASE),
    }
}

pub fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// The EXE the disc boots, read independently of the runtime.
fn disc_exe_text(disc: &mut Disc, exe: &str) -> Option<Vec<u8>> {
    let e = psx_devices::cd::find(disc, exe)?;
    let file = psx_devices::cd::read_file(disc, &e)?;
    let t_size = u32::from_le_bytes(file.get(0x1C..0x20)?.try_into().ok()?) as usize;
    file.get(0x800..0x800 + t_size).map(<[u8]>::to_vec)
}

pub fn run(opts: Options) -> Result<RunReport> {
    let disc = Disc::open(&opts.disc)?;
    let (mut store, instance) = instantiate(&opts, Some(disc))?;
    if !opts.mods.is_empty() || !opts.swaps.is_empty() {
        let base = std::fs::read(&opts.wasm)?;
        let mut loader = mods::Loader::new(&mut store, &instance, &base)?
            .with_context(|| format!("{} is not a moddable base (psxw build --moddable)", opts.wasm.display()))?;
        for m in &opts.mods {
            let id = loader.load(&mut store, &std::fs::read(m).with_context(|| format!("reading {}", m.display()))?)?;
            store.data_mut().events.push(serde_json::json!({"event": "mod-load", "id": id, "frame": 0}));
        }
        for (name, addrs) in loader.functions() {
            let addrs: Vec<String> = addrs.iter().map(|a| format!("0x{a:08X}")).collect();
            store.data_mut().events.push(serde_json::json!({"event": "mod-function", "name": name, "address": addrs.join(" ")}));
        }
        store.data_mut().loader = Some(loader);
    }
    let ptr = boot_options(&mut store, &instance, &opts)?;
    let boot = instance.get_typed_func::<u32, ()>(&mut store, psxw_abi::exports::BOOT)?;
    let stop = match boot.call(&mut store, ptr) {
        Ok(()) => Stop::Exited,
        Err(e) => stop_of(e),
    };
    Ok(finish(store, &instance, stop))
}

fn stop_of(e: wasmtime::Error) -> Stop {
    let msg = format!("{e:?}");
    if msg.contains(FRAME_LIMIT) {
        Stop::FrameLimit
    } else {
        let root = e.root_cause().to_string();
        let trace = msg.lines().filter(|l| l.contains(" - ")).take(24).collect::<Vec<_>>().join("\n");
        Stop::Trap(format!("{root}\n{trace}"))
    }
}

/// Writes `opts.boot` into runtime memory; returns the pointer.
fn boot_options(store: &mut Store<State>, instance: &wasmtime::Instance, opts: &Options) -> Result<u32> {
    let alloc = instance.get_typed_func::<u32, u32>(&mut *store, psxw_abi::exports::ALLOC)?;
    let ptr = alloc.call(&mut *store, std::mem::size_of::<BootOptions>() as u32)?;
    let b = opts.boot;
    let trace = b.trace
        | if opts.record_gpu.is_some() { BootOptions::TRACE_RECORD_GPU } else { 0 }
        | if opts.record_spu.is_some() { BootOptions::TRACE_RECORD_SPU } else { 0 };
    let words = [b.cd_timing, b.video, b.safepoint_budget, b.safepoint_quantum, b.debug, trace];
    let memory = instance.get_memory(&mut *store, "memory").context("no memory")?;
    for (i, w) in words.iter().enumerate() {
        memory.data_mut(&mut *store)[ptr as usize + 4 * i..ptr as usize + 4 * i + 4].copy_from_slice(&w.to_le_bytes());
    }
    Ok(ptr)
}

/// The run's report, after the entry point returned or trapped.
fn finish(mut store: Store<State>, instance: &wasmtime::Instance, stop: Stop) -> RunReport {
    // The run's counters (readable after a trap: the runtime reads its
    // state directly).
    if let Ok(report) = instance.get_typed_func::<(), ()>(&mut store, "psxw_report") {
        let _ = report.call(&mut store, ());
    }
    let s = store.into_data();
    let report = s.events.iter().rev().find(|e| e["event"] == "report").cloned();
    RunReport {
        stop,
        frames: s.frame,
        ram_hashes: s.hashes,
        trace: s.trace,
        boot: s.boot,
        boot_text_matches: s.boot_ok,
        report,
        events: s.events.into_iter().filter(|e| e["event"] != "report").collect(),
        logs: s.logs,
        dumps: s.dumps,
        render: s.render,
        vram_hashes: s.vram_hashes,
        audio: s.audio.map(audio::Sink::finish).unwrap_or_default(),
        gpu_batches: s.gpu_batches,
        spu_windows: s.spu_windows,
        watched: s.watched,
    }
}

/// The engine and compiled module for a wasm file, compiled once per
/// process (conformance instantiates the same build many times).
fn compiled(opts: &Options) -> Result<(Engine, Module)> {
    type Key = (std::path::PathBuf, bool, Option<std::time::SystemTime>);
    static CACHE: std::sync::Mutex<BTreeMap<Key, (Engine, Module)>> = std::sync::Mutex::new(BTreeMap::new());
    let key: Key = (opts.wasm.clone(), opts.deadline_secs > 0, std::fs::metadata(&opts.wasm).and_then(|m| m.modified()).ok());
    if let Some(hit) = CACHE.lock().map_err(|_| anyhow::anyhow!("module cache poisoned"))?.get(&key) {
        return Ok(hit.clone());
    }
    let mut config = Config::new();
    config.epoch_interruption(opts.deadline_secs > 0);
    // ReturnFromException unwinds guest frames with a wasm exception
    // (libetc's interrupt exit; psxw-link `guest_invoke_irq`).
    config.wasm_exceptions(true);
    let engine = Engine::new(&config)?;
    let module = Module::from_file(&engine, &opts.wasm).map_err(|e| anyhow::anyhow!("loading {}: {e}", opts.wasm.display()))?;
    CACHE.lock().map_err(|_| anyhow::anyhow!("module cache poisoned"))?.insert(key, (engine.clone(), module.clone()));
    Ok((engine, module))
}

fn instantiate(opts: &Options, disc: Option<Disc>) -> Result<(Store<State>, wasmtime::Instance)> {
    let (engine, module) = compiled(opts)?;
    let mut linker: Linker<State> = Linker::new(&engine);
    let h = host::MODULE;

    linker.func_wrap(h, host::LOG, |mut c: Caller<'_, State>, level: u32, ptr: u32, len: u32| {
        let msg = String::from_utf8_lossy(&read_bytes(&mut c, ptr, len)).into_owned();
        if c.data().opts.verbose {
            eprintln!("[{}] {msg}", c.data().frame);
        }
        let f = c.data().frame;
        c.data_mut().logs.push(format!("{f}:{level}: {msg}"));
    })?;
    linker.func_wrap(h, host::TRAP, |mut c: Caller<'_, State>, code: u32, ptr: u32, len: u32| -> wasmtime::Result<()> {
        let msg = String::from_utf8_lossy(&read_bytes(&mut c, ptr, len)).into_owned();
        Err(wasmtime::Error::msg(format!("runtime trap {code}: {msg}")))
    })?;
    // Everything at a vblank happens in `vblank`: this host runs no JS, so
    // the runtime never holds its waits, and it doesn't pace.
    linker.func_wrap(h, host::WAIT_VBLANK, |_: Caller<'_, State>, _frame: u64| {})?;
    linker.func_wrap(h, host::VBLANK, |mut c: Caller<'_, State>, frame: u64| -> wasmtime::Result<()> {
        c.data_mut().frame = frame;
        let m = mem(&mut c);
        let (want_hash, want_dump, trace) = {
            let s = c.data();
            (s.opts.hash_at.contains(&frame), s.opts.dump_at.contains(&frame), s.opts.trace_u32)
        };
        let window = c.data().window;
        if want_hash || want_dump || trace.is_some() {
            let data = m.data(&c);
            let ram = guest_ram(data, window);
            let hash = want_hash.then(|| format!("{:016x}", fnv(ram)));
            let word = trace.map(|a| {
                let o = (a & 0x1F_FFFC) as usize;
                u32::from_le_bytes([ram[o], ram[o + 1], ram[o + 2], ram[o + 3]])
            });
            let mut files = Vec::new();
            if want_dump {
                files.push(Dump { file: format!("ram_{frame:06}.bin"), frame, counter: None, picture: None, vram: None });
            }
            if let Some(w) = word {
                let s = c.data();
                for (i, v) in s.opts.dump_counter.iter().enumerate() {
                    if *v == w && !s.counter_done[i] {
                        files.push(Dump { file: format!("ram_c{w:010}.bin"), frame, counter: Some(w), picture: None, vram: None });
                    }
                }
            }
            let ram = if files.is_empty() { Vec::new() } else { ram.to_vec() };
            let s = c.data_mut();
            if let Some(hsh) = hash {
                s.hashes.insert(frame, hsh);
            }
            if let Some(w) = word {
                s.trace.push(w);
                for (i, v) in s.opts.dump_counter.iter().enumerate() {
                    s.counter_done[i] |= *v == w;
                }
            }
            if let Some(dir) = s.opts.dump_dir.clone() {
                std::fs::create_dir_all(&dir).ok();
                // The frame presented at this vblank (the runtime presents
                // just before it), and VRAM.
                let (picture, vram) = if s.opts.dump_with_frame && !files.is_empty() {
                    let r = s.renderer.clone();
                    let mut r = r.lock().map_err(|_| wasmtime::Error::msg("renderer poisoned"))?;
                    let picture = r.frame().map_err(|e| wasmtime::Error::msg(format!("frame: {e:#}")))?;
                    let vram = if s.opts.dump_vram { r.vram().map_err(|e| wasmtime::Error::msg(format!("vram: {e:#}")))? } else { None };
                    (picture.map(|(w, h, rgba)| render::png(w, h, &rgba)), vram.map(|v| v.iter().flat_map(|p| p.to_le_bytes()).collect::<Vec<u8>>()))
                } else {
                    (None, None)
                };
                for mut d in files {
                    std::fs::write(dir.join(&d.file), &ram).ok();
                    let suffix = d.file.trim_start_matches("ram_").trim_end_matches(".bin").to_string();
                    if let Some(png) = &picture {
                        let name = format!("frame_{suffix}.png");
                        std::fs::write(dir.join(&name), png).ok();
                        d.picture = Some(name);
                    }
                    if let Some(v) = &vram {
                        let name = format!("vram_{suffix}.bin");
                        std::fs::write(dir.join(&name), v).ok();
                        d.vram = Some(name);
                    }
                    s.dumps.push(d);
                }
            }
        }
        if !c.data().opts.watch.is_empty() {
            let ram = guest_ram(m.data(&c), window);
            let row: Vec<u32> = c
                .data()
                .opts
                .watch
                .iter()
                .map(|&(a, n)| {
                    let o = (a & 0x1F_FFFF) as usize;
                    (0..n as usize).fold(0u32, |v, i| v | (ram.get(o + i).copied().unwrap_or(0) as u32) << (8 * i))
                })
                .collect();
            c.data_mut().watched.push(row);
        }
        // Overlay residency: whenever an overlay's first words are in RAM,
        // its whole text must be.
        let ram = guest_ram(m.data(&c), window).to_vec();
        let mut events = Vec::new();
        let n = c.data().opts.overlays.len();
        for i in 0..n {
            let (container, verdict) = {
                let o = &c.data().opts.overlays[i];
                let Some((a0, t0)) = o.text.first() else { continue };
                let at = (*a0 & 0x1F_FFFF) as usize;
                if ram.get(at..at + 8) != t0.get(..8) {
                    (o.container.clone(), None)
                } else {
                    let ok = o.text.iter().all(|(a, t)| {
                        let at = (*a & 0x1F_FFFF) as usize;
                        ram.get(at..at + t.len()) == Some(t.as_slice())
                    });
                    (o.container.clone(), Some(ok))
                }
            };
            let was = c.data().resident[i];
            let s = c.data_mut();
            match verdict {
                Some(true) => {
                    s.partial_since[i] = None;
                    if !was {
                        events.push(serde_json::json!({"event": "overlay-resident", "container": container, "frame": frame}));
                        s.resident[i] = true;
                    }
                }
                Some(false) => {
                    s.resident[i] = false;
                    let since = *s.partial_since[i].get_or_insert(frame);
                    if frame - since == LOAD_FRAMES {
                        events.push(serde_json::json!({"event": "overlay-mismatch", "container": container, "frame": frame}));
                    }
                }
                None => {
                    s.resident[i] = false;
                    s.partial_since[i] = None;
                }
            }
        }
        c.data_mut().events.extend(events);
        // Mods swapped in at this vblank, while the game waits here.
        let swaps: Vec<ModSwap> = c.data().opts.swaps.iter().filter(|w| w.frame == frame).cloned().collect();
        for w in swaps {
            let Some(mut loader) = c.data_mut().loader.take() else { break };
            let bytes = std::fs::read(&w.path).map_err(|e| wasmtime::Error::msg(format!("reading {}: {e}", w.path.display())))?;
            let r = if w.index < loader.ids().len() { loader.swap(&mut c, w.index, &bytes) } else { loader.load(&mut c, &bytes) };
            c.data_mut().loader = Some(loader);
            let id = r.map_err(|e| wasmtime::Error::msg(format!("swapping in {}: {e:#}", w.path.display())))?;
            c.data_mut().events.push(serde_json::json!({"event": "mod-swap", "id": id, "index": w.index, "frame": frame}));
        }
        let calls: Vec<(usize, Execute)> = c.data().opts.execute.iter().cloned().enumerate().filter(|(_, x)| x.frame == frame).collect();
        for (id, x) in calls {
            let addr = match x.function.strip_prefix("0x").map(|h| u32::from_str_radix(h, 16)) {
                Some(a) => a.map_err(|e| wasmtime::Error::msg(format!("--execute {}: {e}", x.function)))?,
                None => {
                    let found = c.data().loader.as_ref().map(|l| l.function(&x.function));
                    found.unwrap_or_else(|| Err(anyhow::anyhow!("no mods are loaded"))).map_err(|e| wasmtime::Error::msg(format!("--execute: {e}")))?
                }
            };
            let a = |i: usize| x.args.get(i).copied().unwrap_or(0);
            let f = c
                .get_export(psxw_abi::exports::EXECUTE)
                .and_then(wasmtime::Extern::into_func)
                .ok_or_else(|| wasmtime::Error::msg("game.wasm exports no psxw_execute"))?;
            let status = f.typed::<(u32, u32, u32, u32, u32, u32), i32>(&c)?.call(&mut c, (id as u32, addr, a(0), a(1), a(2), a(3)))?;
            if status != 0 {
                return Err(wasmtime::Error::msg(format!("--execute {} at vblank {frame}: rejected ({status})", x.function)));
            }
        }
        if frame >= c.data().opts.frames {
            return Err(wasmtime::Error::msg(FRAME_LIMIT));
        }
        Ok(())
    })?;
    linker.func_wrap(h, host::DISC_INFO, |mut c: Caller<'_, State>, out: u32| -> i32 {
        let Some(info) = c.data().disc.as_ref().map(Disc::info) else { return 0 };
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts((&info as *const psxw_abi::DiscInfo).cast(), std::mem::size_of::<psxw_abi::DiscInfo>())
        };
        let bytes = bytes.to_vec();
        let m = mem(&mut c);
        m.data_mut(&mut c)[out as usize..out as usize + bytes.len()].copy_from_slice(&bytes);
        1
    })?;
    linker.func_wrap(h, host::DISC_READ, |mut c: Caller<'_, State>, lba: u32, count: u32, mode: u32, dst: u32| -> i32 {
        let mut out = Vec::with_capacity(count as usize * 2352);
        for i in 0..count {
            let mut raw = [0u8; 2352];
            if !c.data_mut().disc.as_mut().is_some_and(|d| d.read_raw(lba + i, &mut raw)) {
                return -1;
            }
            if mode == 0 {
                out.extend_from_slice(&raw);
            } else {
                out.extend_from_slice(psx_devices::cd::user_data(&raw));
            }
        }
        let m = mem(&mut c);
        m.data_mut(&mut c)[dst as usize..dst as usize + out.len()].copy_from_slice(&out);
        0
    })?;
    linker.func_wrap(h, host::PAD_STATE, |mut c: Caller<'_, State>, port: u32, out: u32| -> i32 {
        let header = c.data().opts.replay.as_deref().map(replay::Header::parse);
        let buttons = match header {
            Some(None) => return 0,
            Some(Some(h)) if !h.plugged(port) => return 0,
            Some(Some(h)) => {
                let frame = c.data().frame;
                let m = mem(&mut c);
                let window = c.data().window as usize;
                let at = h.entry(frame, |addr| {
                    let o = (addr & 0x1F_FFFC) as usize + window;
                    m.data(&c).get(o..o + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                });
                c.data().opts.replay.as_deref().map_or(replay::NOTHING_PRESSED, |r| replay::frame(r, at)[port as usize & 1])
            }
            None if port == 0 => replay::NOTHING_PRESSED,
            None => return 0,
        };
        let m = mem(&mut c);
        m.data_mut(&mut c)[out as usize..out as usize + 2].copy_from_slice(&buttons.to_le_bytes());
        2
    })?;
    linker.func_wrap(h, host::STORAGE_READ, |mut c: Caller<'_, State>, np: u32, nl: u32, dst: u32, cap: u32| -> i32 {
        let name = String::from_utf8_lossy(&read_bytes(&mut c, np, nl)).into_owned();
        let Some(dir) = c.data().opts.storage_dir.clone() else { return -1 };
        let Ok(data) = std::fs::read(dir.join(&name)) else { return -1 };
        let n = data.len().min(cap as usize);
        let m = mem(&mut c);
        m.data_mut(&mut c)[dst as usize..dst as usize + n].copy_from_slice(&data[..n]);
        n as i32
    })?;
    linker.func_wrap(h, host::STORAGE_WRITE, |mut c: Caller<'_, State>, np: u32, nl: u32, src: u32, n: u32| -> i32 {
        let name = String::from_utf8_lossy(&read_bytes(&mut c, np, nl)).into_owned();
        let data = read_bytes(&mut c, src, n);
        let Some(dir) = c.data().opts.storage_dir.clone() else { return -1 };
        std::fs::create_dir_all(&dir).ok();
        if std::fs::write(dir.join(&name), data).is_ok() { 0 } else { -1 }
    })?;
    use psxw_abi::render::imports as ri;
    linker.func_wrap(h, ri::RENDER_SUBMIT, |mut c: Caller<'_, State>, ptr: u32, len: u32| -> wasmtime::Result<()> {
        let bytes = read_bytes(&mut c, ptr, len);
        let words: Vec<u32> = bytes.chunks_exact(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
        let s = c.data_mut();
        s.render.submits += 1;
        s.render.words += words.len() as u64;
        s.render.hash = words.iter().fold(if s.render.hash == 0 { 0xcbf2_9ce4_8422_2325 } else { s.render.hash }, |h, w| (h ^ *w as u64).wrapping_mul(0x100_0000_01b3));
        let r = s.renderer.clone();
        let mut r = r.lock().map_err(|_| wasmtime::Error::msg("renderer poisoned"))?;
        r.submit(&words).map_err(|e| wasmtime::Error::msg(format!("render_submit: {e:#}")))
    })?;
    linker.func_wrap(h, ri::RENDER_READBACK, |mut c: Caller<'_, State>, x: u32, y: u32, w: u32, hh: u32, dst: u32| -> wasmtime::Result<()> {
        c.data_mut().render.readbacks += 1;
        let r = c.data().renderer.clone();
        let px = {
            let mut r = r.lock().map_err(|_| wasmtime::Error::msg("renderer poisoned"))?;
            r.readback(psxw_abi::render::Rect { x, y, w, h: hh }).map_err(|e| wasmtime::Error::msg(format!("render_readback: {e:#}")))?
        };
        let bytes: Vec<u8> = px.iter().flat_map(|p| p.to_le_bytes()).collect();
        let m = mem(&mut c);
        m.data_mut(&mut c)[dst as usize..dst as usize + bytes.len()].copy_from_slice(&bytes);
        Ok(())
    })?;
    linker.func_wrap(h, ri::GPU_RECORD_BEGIN, |mut c: Caller<'_, State>| -> wasmtime::Result<i32> {
        let s = c.data_mut();
        let Some(rec) = s.opts.record_gpu else { return Ok(0) };
        let due = s.gpu_batches.last().is_none_or(|b| s.frame >= b.frame + rec.every);
        if s.gpu_batches.len() >= rec.budget || !due {
            return Ok(0);
        }
        let r = s.renderer.clone();
        let mut r = r.lock().map_err(|_| wasmtime::Error::msg("renderer poisoned"))?;
        let all = psxw_abi::render::Rect { x: 0, y: 0, w: 1024, h: 512 };
        let vram = r.readback(all).map_err(|e| wasmtime::Error::msg(format!("gpu_record_begin: {e:#}")))?;
        drop(r);
        // A renderer without VRAM (the null renderer) has nothing to record.
        if vram.len() != 1024 * 512 {
            return Ok(0);
        }
        c.data_mut().gpu_vram = Some(vram);
        Ok(1)
    })?;
    linker.func_wrap(h, ri::GPU_RECORD, |mut c: Caller<'_, State>, ptr: u32, len: u32| -> wasmtime::Result<()> {
        let bytes = read_bytes(&mut c, ptr, len);
        let words: Vec<u32> = bytes.chunks_exact(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
        let s = c.data_mut();
        let Some(vram) = s.gpu_vram.take() else { return Ok(()) };
        let mut it = words.into_iter();
        let n = it.next().unwrap_or(0) as usize;
        let state: Vec<u32> = it.by_ref().take(n).collect();
        let mut nodes = Vec::new();
        while let Some(k) = it.next() {
            nodes.push(it.by_ref().take(k as usize).collect());
        }
        s.gpu_batches.push(GpuBatch { frame: s.frame, vram, state, nodes });
        Ok(())
    })?;
    linker.func_wrap(h, ri::PRESENT, |mut c: Caller<'_, State>, ptr: u32| -> wasmtime::Result<()> {
        let b = read_bytes(&mut c, ptr, 20);
        let w: Vec<u32> = b.chunks_exact(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
        let desc = psxw_abi::render::PresentDesc { flags: w[0], x: w[1], y: w[2], width: w[3], height: w[4] };
        let s = c.data_mut();
        s.render.presents += 1;
        s.render.last_present = Some([w[0], w[1], w[2], w[3], w[4]]);
        let frame = s.frame + 1;
        let want_vram = s.opts.vram_hash_at.contains(&frame);
        let dump = s.opts.dump_frames.clone().filter(|(_, every)| frame % (*every).max(1) == 0);
        let c_dump_vram = s.opts.dump_vram;
        let r = s.renderer.clone();
        let mut r = r.lock().map_err(|_| wasmtime::Error::msg("renderer poisoned"))?;
        r.present(&desc).map_err(|e| wasmtime::Error::msg(format!("present: {e:#}")))?;
        if let Some((dir, _)) = dump {
            if let Some((w, h, rgba)) = r.frame().map_err(|e| wasmtime::Error::msg(format!("frame: {e:#}")))? {
                std::fs::create_dir_all(&dir).ok();
                std::fs::write(dir.join(format!("frame_{frame:06}.png")), render::png(w, h, &rgba)).ok();
            }
            if c_dump_vram {
                if let Some(v) = r.vram().map_err(|e| wasmtime::Error::msg(format!("vram: {e:#}")))? {
                    let rgba: Vec<u8> = v
                        .iter()
                        .flat_map(|&p| {
                            let c = |s: u16| (((p >> s) & 31) << 3) as u8;
                            [c(0), c(5), c(10), 255]
                        })
                        .collect();
                    std::fs::write(dir.join(format!("vram_{frame:06}.png")), render::png(1024, 512, &rgba)).ok();
                }
            }
        }
        if want_vram {
            if let Some(v) = r.vram().map_err(|e| wasmtime::Error::msg(format!("vram: {e:#}")))? {
                let bytes: Vec<u8> = v.iter().flat_map(|p| p.to_le_bytes()).collect();
                drop(r);
                c.data_mut().vram_hashes.insert(frame, format!("{:016x}", fnv(&bytes)));
            }
        }
        Ok(())
    })?;
    linker.func_wrap(h, host::SPU_RECORD_BEGIN, |mut c: Caller<'_, State>| -> wasmtime::Result<i32> {
        let s = c.data_mut();
        let Some(rec) = s.opts.record_spu else { return Ok(0) };
        let due = s.spu_asked.is_none_or(|f| s.frame >= f + rec.every);
        if s.spu_windows.len() >= rec.budget || !due {
            return Ok(0);
        }
        s.spu_asked = Some(s.frame);
        Ok(rec.samples as i32)
    })?;
    linker.func_wrap(h, host::SPU_RECORD, |mut c: Caller<'_, State>, ptr: u32, len: u32| -> wasmtime::Result<()> {
        let bytes = read_bytes(&mut c, ptr, len);
        let word = |i: usize| u32::from_le_bytes([bytes[4 * i], bytes[4 * i + 1], bytes[4 * i + 2], bytes[4 * i + 3]]);
        let samples = word(0);
        let nr = word(1) as usize;
        let regs: Vec<(u16, u16)> = (0..nr).map(|i| (word(2 + 2 * i) as u16, word(3 + 2 * i) as u16)).collect();
        let at = 2 + 2 * nr;
        let nw = word(at) as usize;
        let writes: Vec<(u32, u16, u16)> = (0..nw).map(|i| (word(at + 1 + 3 * i), word(at + 2 + 3 * i) as u16, word(at + 3 + 3 * i) as u16)).collect();
        let ram = bytes[4 * (at + 1 + 3 * nw)..].to_vec();
        let s = c.data_mut();
        s.spu_windows.push(SpuWindow { frame: s.frame, samples, regs, writes, ram });
        Ok(())
    })?;
    linker.func_wrap(h, host::AUDIO, |mut c: Caller<'_, State>, ptr: u32, frames: u32| -> wasmtime::Result<()> {
        let bytes = read_bytes(&mut c, ptr, frames * 4);
        if let Some(a) = c.data_mut().audio.as_mut() {
            a.push(&bytes).map_err(|e| wasmtime::Error::msg(format!("audio: {e}")))?;
        }
        Ok(())
    })?;
    linker.func_wrap(h, host::REPORT, |mut c: Caller<'_, State>, ptr: u32, len: u32| {
        let text = String::from_utf8_lossy(&read_bytes(&mut c, ptr, len)).into_owned();
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            c.data_mut().logs.push(format!("bad report event: {text}"));
            return;
        };
        if v["event"] == "boot" {
            if let Some(l) = c.data_mut().loader.as_mut() {
                l.booted = true;
            }
            // RAM over [t_addr, t_addr + t_size) must equal the disc's EXE.
            let exe = v["exe"].as_str().unwrap_or_default().to_string();
            let (t_addr, t_size) = (v["t_addr"].as_u64().unwrap_or(0) as u32, v["t_size"].as_u64().unwrap_or(0) as usize);
            let want = c.data_mut().disc.as_mut().and_then(|d| disc_exe_text(d, &exe));
            let m = mem(&mut c);
            let at = (t_addr & 0x00FF_FFFF) as usize + c.data().window as usize;
            let ok = want.is_some_and(|w| m.data(&c)[at..at + t_size] == w[..]);
            let s = c.data_mut();
            s.boot_ok = Some(ok);
            s.boot = Some(v.clone());
        } else if v["event"] == "exit" {
            c.data_mut().stop = Some(Stop::Exited);
        }
        c.data_mut().events.push(v);
    })?;

    let mut state = State::new(opts, disc);
    state.audio = Some(audio::Sink::new(opts.audio_wav.as_deref()).context("opening the audio WAV")?);
    let mut store = Store::new(&engine, state);
    if opts.deadline_secs > 0 {
        // Armed per call by `Conform`; nothing traps before that.
        store.set_epoch_deadline(1 << 40);
    }
    let instance = linker.instantiate(&mut store, &module)?;
    let version = instance.get_typed_func::<(), u32>(&mut store, psxw_abi::exports::ABI_VERSION)?.call(&mut store, ())?;
    if version != psxw_abi::ABI_VERSION {
        bail!("game.wasm ABI {version}, host ABI {}", psxw_abi::ABI_VERSION);
    }
    store.data_mut().window = window_of(&mut store, &instance)?;
    Ok((store, instance))
}

/// A game module set up without a disc, driven call by call (adapter
/// conformance): the host writes guest RAM, calls guest functions, and
/// lets vblanks pass. Pad input comes from `opts.replay`, by vblank.
pub struct Conform {
    store: Store<State>,
    instance: wasmtime::Instance,
    memory: Memory,
    deadline: u64,
    /// The guest stack pointer between calls (o32 stack arguments go at
    /// `sp + 16`).
    sp: u32,
    /// Ticks the engine's epoch once a second while the run lives.
    ticker: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl Drop for Conform {
    fn drop(&mut self) {
        if let Some(stop) = &self.ticker {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

impl Conform {
    /// `ram`: (guest address, bytes) to write before the runtime starts.
    pub fn new(opts: &Options, ram: &[(u32, Vec<u8>)], sp: u32, gp: u32) -> Result<Self> {
        let mut opts = opts.clone();
        if opts.frames == 0 {
            opts.frames = u64::MAX;
        }
        let (mut store, instance) = instantiate(&opts, None)?;
        let memory = instance.get_memory(&mut store, "memory").context("no memory")?;
        let window = store.data().window as usize;
        for (addr, bytes) in ram {
            let at = (addr & 0x00FF_FFFF) as usize + window;
            memory.data_mut(&mut store)[at..at + bytes.len()].copy_from_slice(bytes);
        }
        let ptr = boot_options(&mut store, &instance, &opts)?;
        let init = instance.get_typed_func::<(u32, u32, u32), ()>(&mut store, psxw_abi::exports::CONFORM_INIT)?;
        init.call(&mut store, (ptr, sp, gp))?;
        let ticker = (opts.deadline_secs > 0).then(|| {
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (engine, flag) = (store.engine().clone(), stop.clone());
            std::thread::spawn(move || {
                while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    engine.increment_epoch();
                }
            });
            stop
        });
        Ok(Conform { store, instance, memory, deadline: opts.deadline_secs, sp, ticker })
    }

    fn arm(&mut self) {
        if self.deadline > 0 {
            self.store.set_epoch_deadline(self.deadline);
        }
    }

    /// Calls the guest function at `addr` with any number of o32
    /// arguments (from the fifth on, on the stack).
    pub fn call_args(&mut self, addr: u32, args: &[u32]) -> Result<u64> {
        for (i, a) in args.iter().enumerate().skip(4) {
            self.write(self.sp + 4 * i as u32, &a.to_le_bytes());
        }
        let r = |i: usize| args.get(i).copied().unwrap_or(0);
        self.call(addr, [r(0), r(1), r(2), r(3)])
    }

    /// Calls the guest function at `addr`; a trap is an error naming it.
    pub fn call(&mut self, addr: u32, args: [u32; 4]) -> Result<u64> {
        let f = self.instance.get_typed_func::<(u32, u32, u32, u32, u32), u64>(&mut self.store, psxw_abi::exports::CALL)?;
        self.arm();
        f.call(&mut self.store, (addr, args[0], args[1], args[2], args[3])).map_err(|e| anyhow::anyhow!("call {addr:#010x}: {:?}", stop_of(e)))
    }

    pub fn run_vblanks(&mut self, n: u32) -> Result<()> {
        let f = self.instance.get_typed_func::<u32, ()>(&mut self.store, psxw_abi::exports::RUN_VBLANKS)?;
        self.arm();
        f.call(&mut self.store, n).map_err(|e| anyhow::anyhow!("run_vblanks: {:?}", stop_of(e)))
    }

    pub fn read(&self, addr: u32, len: usize) -> Vec<u8> {
        let at = (addr & 0x00FF_FFFF) as usize + self.store.data().window as usize;
        self.memory.data(&self.store)[at..at + len].to_vec()
    }

    pub fn write(&mut self, addr: u32, bytes: &[u8]) {
        let at = (addr & 0x00FF_FFFF) as usize + self.store.data().window as usize;
        self.memory.data_mut(&mut self.store)[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// The runtime's counters now (BIOS functions, adapters, probes), from
    /// a mid-run `psxw_report`.
    pub fn counts(&mut self) -> Result<BTreeMap<String, u64>> {
        let f = self.instance.get_typed_func::<(), ()>(&mut self.store, "psxw_report")?;
        f.call(&mut self.store, ())?;
        let report = self.store.data_mut().events.pop().filter(|e| e["event"] == "report").context("no report")?;
        Ok(report["counts"].as_object().map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_u64()?))).collect()).unwrap_or_default())
    }

    /// The pad input from here on: another replay (its header says which
    /// ports are plugged), read at the same frames.
    pub fn set_replay(&mut self, replay: Vec<u8>) {
        self.store.data_mut().opts.replay = Some(replay);
    }

    /// The renderer commands so far, hashed (see `RenderStats::hash`).
    pub fn render_hash(&self) -> u64 {
        self.store.data().render.hash
    }

    /// Vblanks so far.
    pub fn frame(&self) -> u64 {
        self.store.data().frame
    }

    /// Ends the run: its report (counters, logs).
    pub fn finish(mut self) -> RunReport {
        if self.deadline > 0 {
            self.store.set_epoch_deadline(1 << 40);
        }
        let store = std::mem::replace(&mut self.store, Store::new(&Engine::default(), State::empty()));
        finish(store, &self.instance, Stop::Exited)
    }
}
