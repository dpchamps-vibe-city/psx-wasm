//! The runtime module's imports and exports (psxw-abi): `host.*` and
//! `guest.*` in, the guest's `psx.*` services and the host's `psxw_*`
//! entry points out. Merged into `game.wasm` with the guest module.

use crate::boot;
use crate::driver::{self, Invoke};
use crate::hold::Hold;
use psx_bios::{GuestCall, Host, Machine, Regs};
use psx_devices::mem::Mem;
use psxw_abi::{BootOptions, WINDOW_BASE, WINDOW_MASK};
use sdk_psyq::Psyq;
use std::cell::{Cell, OnceCell, RefCell};
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

#[link(wasm_import_module = "host")]
unsafe extern "C" {
    #[link_name = "log"]
    fn host_log(level: u32, ptr: *const u8, len: usize);
    #[link_name = "trap"]
    fn host_trap(code: u32, ptr: *const u8, len: usize);
    #[link_name = "vblank"]
    fn host_vblank(frame: u64);
    #[link_name = "wait_vblank"]
    fn host_wait_vblank(frame: u64);
    #[link_name = "disc_info"]
    fn host_disc_info(out: *mut u8) -> i32;
    #[link_name = "disc_read"]
    fn host_disc_read(lba: u32, count: u32, mode: u32, dst: *mut u8) -> i32;
    #[link_name = "pad_state"]
    fn host_pad_state(port: u32, out: *mut u8) -> i32;
    #[link_name = "storage_read"]
    fn host_storage_read(name: *const u8, len: usize, dst: *mut u8, cap: usize) -> i32;
    #[link_name = "storage_write"]
    fn host_storage_write(name: *const u8, len: usize, src: *const u8, n: usize) -> i32;
    #[link_name = "report"]
    fn host_report(ptr: *const u8, len: usize);
    #[link_name = "render_submit"]
    fn host_render_submit(ptr: *const u8, len: usize);
    #[link_name = "render_readback"]
    fn host_render_readback(x: u32, y: u32, w: u32, h: u32, dst: *mut u8);
    #[link_name = "present"]
    fn host_present(desc: *const u8);
    #[link_name = "audio"]
    fn host_audio(ptr: *const u8, frames: usize);
    #[link_name = "gpu_record_begin"]
    fn host_gpu_record_begin() -> i32;
    #[link_name = "gpu_record"]
    fn host_gpu_record(ptr: *const u8, len: usize);
    #[link_name = "spu_record_begin"]
    fn host_spu_record_begin() -> i32;
    #[link_name = "spu_record"]
    fn host_spu_record(ptr: *const u8, len: usize);
}

#[link(wasm_import_module = "guest")]
unsafe extern "C" {
    fn guest_invoke(addr: u32, a0: u32, a1: u32, a2: u32, a3: u32) -> u64;
    fn guest_invoke_irq(addr: u32, a0: u32, a1: u32, a2: u32, a3: u32) -> u64;
    fn guest_sp_get() -> u32;
    fn guest_sp_set(v: u32);
    fn guest_gp_get() -> u32;
    fn guest_gp_set(v: u32);
    fn psxmod_init();
    fn psxmod_loaded(addr: u32, len: u32);
    fn guest_window() -> u32;
}

fn fatal(code: u32, msg: &str) -> ! {
    unsafe { host_trap(code, msg.as_ptr(), msg.len()) };
    core::arch::wasm32::unreachable()
}

struct FfiHost;

impl Host for FfiHost {
    fn log(&mut self, level: u32, msg: &str) {
        unsafe { host_log(level, msg.as_ptr(), msg.len()) }
    }
    fn vblank(&mut self, frame: u64) {
        unsafe { host_vblank(frame) }
    }
    fn wait_vblank(&mut self, frame: u64) {
        HOLD.with(|h| h.wait(frame, |f| unsafe { host_wait_vblank(f) }))
    }
    fn disc_read(&mut self, lba: u32, out: &mut [u8; 2352]) -> bool {
        unsafe { host_disc_read(lba, 1, 0, out.as_mut_ptr()) == 0 }
    }
    fn has_disc(&mut self) -> bool {
        let mut info = psxw_abi::DiscInfo::default();
        unsafe { host_disc_info((&mut info as *mut psxw_abi::DiscInfo).cast()) != 0 }
    }
    fn pad_state(&mut self, port: u32) -> Vec<u8> {
        let mut buf = [0u8; 32];
        let n = unsafe { host_pad_state(port, buf.as_mut_ptr()) };
        buf[..n.clamp(0, 32) as usize].to_vec()
    }
    fn storage_read(&mut self, name: &str) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; 256 * 1024];
        let n = unsafe { host_storage_read(name.as_ptr(), name.len(), buf.as_mut_ptr(), buf.len()) };
        (n >= 0).then(|| {
            buf.truncate(n as usize);
            buf
        })
    }
    fn storage_write(&mut self, name: &str, data: &[u8]) -> bool {
        unsafe { host_storage_write(name.as_ptr(), name.len(), data.as_ptr(), data.len()) == 0 }
    }
    fn report(&mut self, json: &str) {
        unsafe { host_report(json.as_ptr(), json.len()) }
    }
    fn render_submit(&mut self, words: &[u32]) {
        unsafe { host_render_submit(words.as_ptr().cast(), words.len() * 4) }
    }
    fn render_readback(&mut self, x: u32, y: u32, w: u32, h: u32) -> Vec<u16> {
        let mut px = vec![0u16; (w * h) as usize];
        unsafe { host_render_readback(x, y, w, h, px.as_mut_ptr().cast()) };
        px
    }
    fn present(&mut self, desc: &psxw_abi::render::PresentDesc) {
        let words = desc.to_words();
        unsafe { host_present(words.as_ptr().cast()) }
    }
    fn audio(&mut self, frames: &[[i16; 2]]) {
        unsafe { host_audio(frames.as_ptr().cast(), frames.len()) }
    }
    fn gpu_record_begin(&mut self) -> bool {
        unsafe { host_gpu_record_begin() != 0 }
    }
    fn gpu_record(&mut self, state: &[u32], nodes: &[Vec<u32>]) {
        let mut words = vec![state.len() as u32];
        words.extend_from_slice(state);
        for n in nodes {
            words.push(n.len() as u32);
            words.extend_from_slice(n);
        }
        unsafe { host_gpu_record(words.as_ptr().cast(), words.len() * 4) }
    }
    fn spu_record_begin(&mut self) -> u32 {
        unsafe { host_spu_record_begin().max(0) as u32 }
    }
    fn spu_record(&mut self, w: &psx_bios::io::SpuWindow) {
        let mut words = vec![w.samples, w.regs.len() as u32];
        words.extend(w.regs.iter().flat_map(|&(o, v)| [o as u32, v as u32]));
        words.push(w.writes.len() as u32);
        words.extend(w.writes.iter().flat_map(|&(s, o, v)| [s, o as u32, v as u32]));
        let mut bytes: Vec<u8> = words.iter().flat_map(|x| x.to_le_bytes()).collect();
        bytes.extend_from_slice(&w.ram);
        unsafe { host_spu_record(bytes.as_ptr(), bytes.len()) }
    }
}

struct FfiRegs;

impl Regs for FfiRegs {
    fn sp(&self) -> u32 {
        unsafe { guest_sp_get() }
    }
    fn set_sp(&mut self, v: u32) {
        unsafe { guest_sp_set(v) }
    }
    fn gp(&self) -> u32 {
        unsafe { guest_gp_get() }
    }
    fn set_gp(&mut self, v: u32) {
        unsafe { guest_gp_set(v) }
    }
}

/// The guest window, where the guest put it (psxw-abi `guest::WINDOW`).
struct Window;

static WINDOW: AtomicU32 = AtomicU32::new(WINDOW_BASE);

fn window(addr: u32) -> *mut u8 {
    ((WINDOW.load(Relaxed) + (addr & WINDOW_MASK)) as usize) as *mut u8
}

impl Mem for Window {
    fn read(&self, addr: u32, out: &mut [u8]) {
        let off = (addr & WINDOW_MASK) as usize;
        if off + out.len() <= WINDOW_MASK as usize + 1 {
            unsafe { core::ptr::copy_nonoverlapping(window(addr), out.as_mut_ptr(), out.len()) };
        } else {
            for (i, b) in out.iter_mut().enumerate() {
                *b = unsafe { *window(addr.wrapping_add(i as u32)) };
            }
        }
    }
    fn write(&mut self, addr: u32, data: &[u8]) {
        let off = (addr & WINDOW_MASK) as usize;
        if off + data.len() <= WINDOW_MASK as usize + 1 {
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), window(addr), data.len()) };
        } else {
            for (i, b) in data.iter().enumerate() {
                unsafe { *window(addr.wrapping_add(i as u32)) = *b };
            }
        }
    }
}

struct Guest;

impl Invoke for Guest {
    fn invoke(&mut self, c: GuestCall) -> u64 {
        unsafe { guest_invoke(c.addr, c.args[0], c.args[1], c.args[2], c.args[3]) }
    }

    fn invoke_irq(&mut self, c: GuestCall) -> u64 {
        unsafe { guest_invoke_irq(c.addr, c.args[0], c.args[1], c.args[2], c.args[3]) }
    }

    fn loaded(&mut self, addr: u32, len: u32) {
        unsafe { psxmod_loaded(addr, len) }
    }

    fn host_calls(&mut self) {
        with(|rt| {
            let Some((id, call)) = rt.execute.take() else { return };
            // Run on the game's normal wasm stack, not by re-entering a
            // JSPI-suspended runtime (wait_vblank holds a machine borrow).
            let sp = unsafe { guest_sp_get() };
            let gp = unsafe { guest_gp_get() };
            let value = psxw_call(call.addr, call.args[0], call.args[1], call.args[2], call.args[3]);
            unsafe {
                guest_sp_set(sp);
                guest_gp_set(gp);
            }
            rt.executing.set(false);
            report(format!("{{\"event\":\"execute\",\"id\":{id},\"value\":\"{value}\"}}"));
        });
    }
}

struct Rt {
    m: RefCell<Machine<Psyq>>,
    quantum: u64,
    adapters: Vec<(&'static str, sdk_psyq::Adapter)>,
    cop0: RefCell<[u32; 32]>,
    /// One pending console call, separate from the borrowed machine.
    execute: Cell<Option<(u32, GuestCall)>>,
    /// Remains set while that call runs (it may itself wait for vblank).
    executing: Cell<bool>,
}

thread_local! {
    static RT: OnceCell<Rt> = const { OnceCell::new() };
    /// Outside the machine: the waits are made from `psxw_js_leave`, with
    /// no machine borrow.
    static HOLD: Hold = const { Hold::new() };
}

fn with<R>(f: impl FnOnce(&Rt) -> R) -> R {
    RT.with(|c| f(c.get().unwrap_or_else(|| fatal(1, "runtime used before psxw_boot"))))
}

fn report(json: String) {
    unsafe { host_report(json.as_ptr(), json.len()) }
}

#[unsafe(no_mangle)]
pub extern "C" fn psxw_abi_version() -> u32 {
    psxw_abi::ABI_VERSION
}

#[unsafe(no_mangle)]
pub extern "C" fn psxw_alloc(len: u32) -> u32 {
    let v = vec![0u8; len as usize].into_boxed_slice();
    Box::leak(v).as_mut_ptr() as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn psxw_ram_hash() -> u64 {
    let mut ram = vec![0u8; 0x20_0000];
    Window.read(0x8000_0000, &mut ram);
    boot::fnv(&ram)
}

fn init(options: u32) {
    WINDOW.store(unsafe { guest_window() }, Relaxed);
    std::panic::set_hook(Box::new(|info| {
        let msg = info.to_string();
        fatal(2, &msg);
    }));
    let opts = if options == 0 {
        BootOptions::default()
    } else {
        unsafe { core::ptr::read_unaligned((options as usize) as *const BootOptions) }
    };
    let quantum = if opts.safepoint_quantum == 0 { 20_000 } else { opts.safepoint_quantum as u64 };
    let mut m = Machine::new(Box::new(Window), Box::new(FfiHost), Box::new(FfiRegs), opts, Psyq::default());
    // Device libraries (D40) reach these registers.
    m.io = Some(Default::default());
    let rt = Rt {
        m: RefCell::new(m),
        quantum,
        adapters: sdk_psyq::adapters(),
        cop0: RefCell::new([0; 32]),
        execute: Cell::new(None),
        executing: Cell::new(false),
    };
    RT.with(|c| {
        let _ = c.set(rt);
    });
}

/// Boot the disc and run until the host stops the run.
#[unsafe(no_mangle)]
pub extern "C" fn psxw_boot(options: u32) {
    init(options);
    let b = with(|rt| boot::load(&mut rt.m.borrow_mut())).unwrap_or_else(|e| fatal(3, &format!("boot: {e}")));
    report(format!(
        "{{\"event\":\"boot\",\"exe\":{:?},\"pc0\":{},\"gp0\":{},\"t_addr\":{},\"t_size\":{},\"b_addr\":{},\"b_size\":{},\"sp\":{},\"text_hash\":\"{:016x}\"}}",
        b.exe, b.pc0, b.gp0, b.t_addr, b.t_size, b.b_addr, b.b_size, b.sp, b.text_hash
    ));
    // Mods' data patches to the executable, then the boot's own load
    // isn't news to them.
    unsafe { psxmod_init() };
    with(|rt| rt.m.borrow_mut().loaded.clear());
    Guest.invoke(GuestCall { addr: b.pc0, args: [0; 4] });
    with(|rt| rt.m.borrow_mut().log("the entry point returned"));
    report("{\"event\":\"exit\"}".into());
}

/// Set up without a disc; the host drives guest calls (psxw-abi
/// `CONFORM_INIT`).
#[unsafe(no_mangle)]
pub extern "C" fn psxw_conform_init(options: u32, sp: u32, gp: u32) {
    init(options);
    with(|rt| {
        let mut m = rt.m.borrow_mut();
        m.regs.set_sp(sp);
        m.regs.set_gp(gp);
    });
}

/// Call a guest function, then run what it queued (psxw-abi `CALL`).
#[unsafe(no_mangle)]
pub extern "C" fn psxw_call(addr: u32, a0: u32, a1: u32, a2: u32, a3: u32) -> u64 {
    let v = Guest.invoke(GuestCall { addr, args: [a0, a1, a2, a3] });
    with(|rt| driver::drain(&rt.m, &mut Guest));
    v
}

/// Queue one host-requested call. Safe even while wait_vblank has the
/// machine borrowed; the driver runs it once that borrow is released.
#[unsafe(no_mangle)]
pub extern "C" fn psxw_execute(id: u32, addr: u32, a0: u32, a1: u32, a2: u32, a3: u32) -> i32 {
    if addr == 0 || addr & 3 != 0 || addr & WINDOW_MASK >= 0x0040_0000 {
        return -3;
    }
    RT.with(|c| {
        let Some(rt) = c.get() else { return -1 };
        if rt.executing.replace(true) {
            return -2;
        }
        rt.execute.set(Some((id, GuestCall { addr, args: [a0, a1, a2, a3] })));
        0
    })
}

/// A shim is about to call JS (psxw-abi `JS_ENTER`): hold the host's
/// vblank waits until it has returned.
#[unsafe(no_mangle)]
pub extern "C" fn psxw_js_enter() {
    HOLD.with(Hold::enter);
}

/// The call into JS has returned or unwound (psxw-abi `JS_LEAVE`); after
/// the outermost, the held waits are made here, with only wasm frames on
/// the stack.
#[unsafe(no_mangle)]
pub extern "C" fn psxw_js_leave() {
    HOLD.with(|h| h.leave(|f| unsafe { host_wait_vblank(f) }));
}

/// Let `n` vblanks pass (psxw-abi `RUN_VBLANKS`).
#[unsafe(no_mangle)]
pub extern "C" fn psxw_run_vblanks(n: u32) {
    with(|rt| {
        let until = rt.m.borrow().vblanks + n as u64;
        let flow = psx_bios::Flow::Block { until: psx_bios::Wait::Vblanks(until), then: Box::new(|_| psx_bios::Flow::Return(0)) };
        driver::run(&rt.m, &mut Guest, flow);
    });
}

// --- psx.*: what the guest imports ---------------------------------------

#[unsafe(export_name = "safepoint")]
pub extern "C" fn safepoint() {
    with(|rt| driver::safepoint(&rt.m, &mut Guest, rt.quantum));
}

#[unsafe(export_name = "untranslated")]
pub extern "C" fn untranslated(addr: u32) -> u64 {
    fatal(4, &format!("no implementation for guest address {addr:#010x}"))
}

#[unsafe(export_name = "bios")]
pub extern "C" fn bios(table: u32, func: u32, a0: u32, a1: u32, a2: u32, a3: u32) -> u64 {
    with(|rt| {
        let flow = psx_bios::bios::call(&mut rt.m.borrow_mut(), table, func, [a0, a1, a2, a3]);
        match flow {
            Ok(f) => driver::run(&rt.m, &mut Guest, f),
            Err(e) => fatal(5, &e),
        }
    })
}

#[unsafe(export_name = "syscall")]
pub extern "C" fn syscall(a0: u32, a1: u32, a2: u32, a3: u32) -> u32 {
    let r = with(|rt| psx_bios::bios::syscall(&mut rt.m.borrow_mut(), [a0, a1, a2, a3]));
    let v = r.unwrap_or_else(|e| fatal(5, &e));
    // Leaving a critical section lets pending interrupts through.
    with(|rt| driver::drain(&rt.m, &mut Guest));
    v
}

#[unsafe(export_name = "brk")]
pub extern "C" fn brk(code: u32) {
    with(|rt| rt.m.borrow_mut().count(&format!("break.{code:#x}")));
}

#[unsafe(export_name = "overflow")]
pub extern "C" fn overflow(pc: u32) {
    fatal(6, &format!("arithmetic overflow exception at {pc:#010x}"))
}

#[unsafe(export_name = "bad_jump")]
pub extern "C" fn bad_jump(pc: u32, target: u32) {
    fatal(7, &format!("jump table at {pc:#010x} read target {target:#010x}"))
}

#[unsafe(export_name = "kernel_call")]
pub extern "C" fn kernel_call(addr: u32, _a0: u32, _a1: u32, _a2: u32, _a3: u32) -> u64 {
    fatal(8, &format!("call into BIOS kernel RAM at {addr:#010x}; this function needs an adapter"))
}

/// A device library's access to the I/O window (D40): a load is about to
/// read window offset `off`, or a store has just written it. The device
/// supplies or takes the value through the window; an unmodelled register
/// is a trap.
/// Any other function's access to the I/O window in a debug build.
#[unsafe(export_name = "io_trap")]
pub extern "C" fn io_trap(off: u32, _kind: u32) {
    fatal(9, &format!("guest access to I/O register {:#010x}; this function needs an adapter", 0x1F00_0000 | (off & 0xFF_FFFF)))
}

#[unsafe(export_name = "io_access")]
pub extern "C" fn io_access(off: u32, kind: u32) {
    let addr = 0x1F00_0000 | (off & 0xFF_FFFF);
    let size = (kind & 0xFF).min(4);
    let store = kind & 0x100 != 0;
    let r = with(|rt| {
        let mut m = rt.m.borrow_mut();
        if store {
            let mut b = [0u8; 4];
            Window.read(addr, &mut b[..size as usize]);
            m.io_write(addr, size, u32::from_le_bytes(b))
        } else {
            m.io_read(addr, size).map(|v| Window.write(addr, &v.to_le_bytes()[..size as usize]))
        }
    });
    if let Err(e) = r {
        fatal(9, &e)
    }
}

/// A mod's `psxmod_log(fmt, ...)`: clang's va-buffer holds the arguments,
/// a word each.
#[unsafe(export_name = "mod_log")]
pub extern "C" fn mod_log(fmt: u32, va: u32) {
    with(|rt| {
        let mut m = rt.m.borrow_mut();
        let mut k = 0;
        let text = psx_bios::bios::format(&m, fmt, &mut || {
            let v = m.mem.r32(va.wrapping_add(4 * k));
            k += 1;
            v
        });
        m.host.log(1, &format!("mod: {text}"));
    });
}

/// Entry of a function the build was asked to watch (the boot check's
/// main-loop symbol).
#[unsafe(export_name = "probe")]
pub extern "C" fn probe(addr: u32) {
    with(|rt| rt.m.borrow_mut().count(&format!("probe.{addr:08X}")));
}

#[unsafe(export_name = "cop0_read")]
pub extern "C" fn cop0_read(r: u32) -> u32 {
    with(|rt| rt.cop0.borrow()[(r & 31) as usize])
}

#[unsafe(export_name = "cop0_write")]
pub extern "C" fn cop0_write(r: u32, v: u32) {
    with(|rt| rt.cop0.borrow_mut()[(r & 31) as usize] = v);
}

#[unsafe(export_name = "rfe")]
pub extern "C" fn rfe() {}

/// ReturnFromException (B0h:17h): the guest unwinds to the
/// `guest_invoke_irq` that entered the exception handler.
#[unsafe(export_name = "return_from_exception")]
pub extern "C" fn return_from_exception() {
    let in_irq = with(|rt| {
        let mut m = rt.m.borrow_mut();
        m.rfe = true;
        m.in_irq
    });
    if !in_irq {
        fatal(5, "ReturnFromException outside an exception handler");
    }
}

/// The GTE's register file, in the runtime-private page of the guest
/// window (psxw-abi `phys::RUNTIME_PRIVATE`): 64 words, data then control.
fn gte() -> &'static mut psx_gte::Gte {
    let words = window(psxw_abi::phys::RUNTIME_PRIVATE.start) as *mut [u32; 64];
    // SAFETY: the page is runtime-private (no guest symbol lives there) and
    // the runtime is single-threaded; each intrinsic takes the reference
    // for one call.
    psx_gte::Gte::view_mut(unsafe { &mut *words })
}

#[unsafe(export_name = "gte_mfc2")]
pub extern "C" fn gte_mfc2(r: u32) -> u32 {
    gte().mfc2(r)
}

#[unsafe(export_name = "gte_cfc2")]
pub extern "C" fn gte_cfc2(r: u32) -> u32 {
    gte().cfc2(r)
}

#[unsafe(export_name = "gte_mtc2")]
pub extern "C" fn gte_mtc2(r: u32, v: u32) {
    gte().mtc2(r, v)
}

#[unsafe(export_name = "gte_ctc2")]
pub extern "C" fn gte_ctc2(r: u32, v: u32) {
    gte().ctc2(r, v)
}

/// A GTE command (the COP2 instruction's low 25 bits).
#[unsafe(export_name = "gte_exec")]
pub extern "C" fn gte_exec(cmd: u32) {
    gte().exec(cmd)
}

/// An SDK function replaced by an adapter (o32 arguments).
#[unsafe(export_name = "adapter")]
pub extern "C" fn adapter(id: u32, a0: u32, a1: u32, a2: u32, a3: u32) -> u64 {
    with(|rt| {
        let Some(&(name, f)) = rt.adapters.get(id as usize) else { fatal(10, &format!("adapter {id} unknown")) };
        let flow = {
            let mut m = rt.m.borrow_mut();
            m.count(&format!("adapter.{name}"));
            f(&mut m, [a0, a1, a2, a3])
        };
        match flow {
            Ok(f) => driver::run(&rt.m, &mut Guest, f),
            Err(e) => fatal(11, &format!("{name}: {e}")),
        }
    })
}

/// Counters for the run report: BIOS calls, adapters, CD sectors and EDC.
/// Called after the host stopped the run with a trap, when frames that
/// borrowed the machine are gone for good, so the cell is read directly.
#[unsafe(no_mangle)]
pub extern "C" fn psxw_report() {
    let json = with(|rt| {
        // SAFETY: the run is over (stopped by a trap or returned); no
        // frame holding a borrow will execute again.
        let m = unsafe { &*rt.m.as_ptr() };
        let counts: Vec<String> = m.counts.iter().map(|(k, v)| format!("{k:?}:{v}")).collect();
        let errors: Vec<String> = m.sdk.cd.edc_errors.iter().map(|e| format!("{e:?}")).collect();
        let events: Vec<String> = m
            .kernel
            .events
            .iter()
            .filter(|e| e.status != 0)
            .map(|e| format!("\"{:08x}/{:x} mode {:x} status {:x} func {:08x}\"", e.class, e.spec, e.mode, e.status, e.func))
            .collect();
        // CD loads under the drive model (M7 task 7's evidence): totals and
        // the longest.
        let loads = &m.sdk.cd.loads;
        let ms = |c: u64| c as f64 * 1000.0 / psx_devices::clock::CPU_HZ as f64;
        let mut longest: Vec<_> = loads.iter().collect();
        longest.sort_by_key(|l| std::cmp::Reverse(l.cycles));
        let longest: Vec<String> = longest
            .iter()
            .take(16)
            .map(|l| format!("{{\"lba\":{},\"sectors\":{},\"ms\":{:.1},\"seek_ms\":{:.1}}}", l.lba, l.sectors, ms(l.cycles), ms(l.seek)))
            .collect();
        let cd_loads = format!(
            "{{\"timing\":\"{:?}\",\"reads\":{},\"sectors\":{},\"ms\":{:.1},\"seek_ms\":{:.1},\"longest\":[{}]}}",
            m.drive.timing,
            loads.len(),
            loads.iter().map(|l| l.sectors as u64).sum::<u64>(),
            ms(loads.iter().map(|l| l.cycles).sum()),
            ms(loads.iter().map(|l| l.seek).sum()),
            longest.join(",")
        );
        let io: Vec<String> = m
            .io
            .iter()
            .flat_map(|io| io.recent.iter())
            .map(|(t, a, n, v, w)| format!("\"{t} {} {a:08x}/{n} {v:#x}\"", if *w { "W" } else { "R" }))
            .collect();
        format!(
            "{{\"event\":\"report\",\"vblanks\":{},\"cycles\":{},\"cd_sectors\":{},\"cd_loads\":{},\"edc_errors\":[{}],\"counts\":{{{}}},\"io_recent\":[{}],\"bios_events\":[{}],\"crit\":{},\"in_irq\":{},\"queued_calls\":{},\"held_waits\":{}}}",
            m.vblanks,
            m.clock.now,
            m.sdk.cd.sectors,
            cd_loads,
            errors.join(","),
            counts.join(","),
            io.join(","),
            events.join(","),
            m.crit,
            m.in_irq,
            m.calls.len(),
            HOLD.with(Hold::total)
        )
    });
    report(json);
}
