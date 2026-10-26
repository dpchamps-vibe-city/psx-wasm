//! The native host (plans/M02): runs `game.wasm` under wasmtime, headless.
//! It supplies the host interface (psxw-abi): the disc image, pad input
//! from a replay, storage, logging, and vblank pacing, which is also where
//! a run stops after its frame budget. Per-frame RAM hashes, dumps and a
//! traced word support the determinism and whole-game oracles.

pub mod disc;
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
}

/// A RAM dump written during the run.
#[derive(Debug, Clone, Serialize)]
pub struct Dump {
    pub file: String,
    pub frame: u64,
    /// The traced word's value, for dumps taken by counter.
    pub counter: Option<u32>,
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
}

impl State {
    fn new(opts: &Options, disc: Option<Disc>) -> Self {
        State {
            renderer: opts.render.clone().unwrap_or_else(|| std::sync::Arc::new(std::sync::Mutex::new(Box::new(render::NullRender)))),
            render: Default::default(),
            vram_hashes: BTreeMap::new(),
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

fn guest_ram(data: &[u8]) -> &[u8] {
    &data[WINDOW_BASE as usize..WINDOW_BASE as usize + 0x20_0000]
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
    let words = [b.cd_timing, b.video, b.safepoint_budget, b.safepoint_quantum, b.debug, b.trace];
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
    }
}

fn instantiate(opts: &Options, disc: Option<Disc>) -> Result<(Store<State>, wasmtime::Instance)> {
    let mut config = Config::new();
    config.epoch_interruption(opts.deadline_secs > 0);
    // ReturnFromException unwinds guest frames with a wasm exception
    // (libetc's interrupt exit; psxw-link `guest_invoke_irq`).
    config.wasm_exceptions(true);
    let engine = Engine::new(&config)?;
    let module = Module::from_file(&engine, &opts.wasm).map_err(|e| anyhow::anyhow!("loading {}: {e}", opts.wasm.display()))?;
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
    linker.func_wrap(h, host::WAIT_VBLANK, |mut c: Caller<'_, State>, frame: u64| -> wasmtime::Result<()> {
        c.data_mut().frame = frame;
        let m = mem(&mut c);
        let (want_hash, want_dump, trace) = {
            let s = c.data();
            (s.opts.hash_at.contains(&frame), s.opts.dump_at.contains(&frame), s.opts.trace_u32)
        };
        if want_hash || want_dump || trace.is_some() {
            let data = m.data(&c);
            let ram = guest_ram(data);
            let hash = want_hash.then(|| format!("{:016x}", fnv(ram)));
            let word = trace.map(|a| {
                let o = (a & 0x1F_FFFC) as usize;
                u32::from_le_bytes([ram[o], ram[o + 1], ram[o + 2], ram[o + 3]])
            });
            let mut files = Vec::new();
            if want_dump {
                files.push(Dump { file: format!("ram_{frame:06}.bin"), frame, counter: None });
            }
            if let Some(w) = word {
                let s = c.data();
                for (i, v) in s.opts.dump_counter.iter().enumerate() {
                    if *v == w && !s.counter_done[i] {
                        files.push(Dump { file: format!("ram_c{w:010}.bin"), frame, counter: Some(w) });
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
                for d in files {
                    std::fs::write(dir.join(&d.file), &ram).ok();
                    s.dumps.push(d);
                }
            }
        }
        // Overlay residency: whenever an overlay's first words are in RAM,
        // its whole text must be.
        let data = m.data(&c).to_vec();
        let ram = guest_ram(&data);
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
        let buttons = match &c.data().opts.replay {
            Some(r) => replay::frame(r, c.data().frame as u32)[port as usize & 1],
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
        let r = s.renderer.clone();
        let mut r = r.lock().map_err(|_| wasmtime::Error::msg("renderer poisoned"))?;
        r.present(&desc).map_err(|e| wasmtime::Error::msg(format!("present: {e:#}")))?;
        if let Some((dir, _)) = dump {
            if let Some((w, h, rgba)) = r.frame().map_err(|e| wasmtime::Error::msg(format!("frame: {e:#}")))? {
                std::fs::create_dir_all(&dir).ok();
                std::fs::write(dir.join(format!("frame_{frame:06}.png")), render::png(w, h, &rgba)).ok();
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
    linker.func_wrap(h, host::REPORT, |mut c: Caller<'_, State>, ptr: u32, len: u32| {
        let text = String::from_utf8_lossy(&read_bytes(&mut c, ptr, len)).into_owned();
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            c.data_mut().logs.push(format!("bad report event: {text}"));
            return;
        };
        if v["event"] == "boot" {
            // RAM over [t_addr, t_addr + t_size) must equal the disc's EXE.
            let exe = v["exe"].as_str().unwrap_or_default().to_string();
            let (t_addr, t_size) = (v["t_addr"].as_u64().unwrap_or(0) as u32, v["t_size"].as_u64().unwrap_or(0) as usize);
            let want = c.data_mut().disc.as_mut().and_then(|d| disc_exe_text(d, &exe));
            let m = mem(&mut c);
            let at = (t_addr & 0x00FF_FFFF) as usize + WINDOW_BASE as usize;
            let ok = want.is_some_and(|w| m.data(&c)[at..at + t_size] == w[..]);
            let s = c.data_mut();
            s.boot_ok = Some(ok);
            s.boot = Some(v.clone());
        } else if v["event"] == "exit" {
            c.data_mut().stop = Some(Stop::Exited);
        }
        c.data_mut().events.push(v);
    })?;

    let state = State::new(opts, disc);
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
        for (addr, bytes) in ram {
            let at = (addr & 0x00FF_FFFF) as usize + WINDOW_BASE as usize;
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
        let at = (addr & 0x00FF_FFFF) as usize + WINDOW_BASE as usize;
        self.memory.data(&self.store)[at..at + len].to_vec()
    }

    pub fn write(&mut self, addr: u32, bytes: &[u8]) {
        let at = (addr & 0x00FF_FFFF) as usize + WINDOW_BASE as usize;
        self.memory.data_mut(&mut self.store)[at..at + bytes.len()].copy_from_slice(bytes);
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
