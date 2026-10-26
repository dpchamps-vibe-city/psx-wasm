//! The runtime module's imports and exports (psxw-abi): `host.*` and
//! `guest.*` in, the guest's `psx.*` services and the host's `psxw_*`
//! entry points out. Merged into `game.wasm` with the guest module.

use crate::boot;
use crate::driver::{self, Invoke};
use psx_bios::{GuestCall, Host, Machine, Regs};
use psx_devices::mem::Mem;
use psxw_abi::{BootOptions, WINDOW_BASE, WINDOW_MASK};
use sdk_psyq::Psyq;
use std::cell::{OnceCell, RefCell};

#[link(wasm_import_module = "host")]
unsafe extern "C" {
    #[link_name = "log"]
    fn host_log(level: u32, ptr: *const u8, len: usize);
    #[link_name = "trap"]
    fn host_trap(code: u32, ptr: *const u8, len: usize);
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
}

#[link(wasm_import_module = "guest")]
unsafe extern "C" {
    fn guest_invoke(addr: u32, a0: u32, a1: u32, a2: u32, a3: u32) -> u64;
    fn guest_sp_get() -> u32;
    fn guest_sp_set(v: u32);
    fn guest_gp_get() -> u32;
    fn guest_gp_set(v: u32);
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
    fn wait_vblank(&mut self, frame: u64) {
        unsafe { host_wait_vblank(frame) }
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

/// The guest window, at its fixed place in linear memory.
struct Window;

fn window(addr: u32) -> *mut u8 {
    ((WINDOW_BASE + (addr & WINDOW_MASK)) as usize) as *mut u8
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
}

struct Rt {
    m: RefCell<Machine<Psyq>>,
    quantum: u64,
    adapters: Vec<(&'static str, sdk_psyq::Adapter)>,
    gte: RefCell<psx_gte::GteRegs>,
    cop0: RefCell<[u32; 32]>,
}

thread_local! {
    static RT: OnceCell<Rt> = const { OnceCell::new() };
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

/// Boot the disc and run until the host stops the run.
#[unsafe(no_mangle)]
pub extern "C" fn psxw_boot(options: u32) {
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
    let m = Machine::new(Box::new(Window), Box::new(FfiHost), Box::new(FfiRegs), opts, Psyq::default());
    let rt = Rt {
        m: RefCell::new(m),
        quantum,
        adapters: sdk_psyq::adapters(),
        gte: RefCell::new(Default::default()),
        cop0: RefCell::new([0; 32]),
    };
    RT.with(|c| {
        let _ = c.set(rt);
    });
    let b = with(|rt| boot::load(&mut rt.m.borrow_mut())).unwrap_or_else(|e| fatal(3, &format!("boot: {e}")));
    report(format!(
        "{{\"event\":\"boot\",\"exe\":{:?},\"pc0\":{},\"gp0\":{},\"t_addr\":{},\"t_size\":{},\"b_addr\":{},\"b_size\":{},\"sp\":{},\"text_hash\":\"{:016x}\"}}",
        b.exe, b.pc0, b.gp0, b.t_addr, b.t_size, b.b_addr, b.b_size, b.sp, b.text_hash
    ));
    Guest.invoke(GuestCall { addr: b.pc0, args: [0; 4] });
    with(|rt| rt.m.borrow_mut().log("the entry point returned"));
    report("{\"event\":\"exit\"}".into());
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

#[unsafe(export_name = "io_access")]
pub extern "C" fn io_access(addr: u32) {
    fatal(9, &format!("guest access to I/O register {:#010x}; this function needs an adapter", addr | 0x1F00_0000))
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

#[unsafe(export_name = "gte_mfc2")]
pub extern "C" fn gte_mfc2(r: u32) -> u32 {
    with(|rt| rt.gte.borrow().data[(r & 31) as usize])
}

#[unsafe(export_name = "gte_cfc2")]
pub extern "C" fn gte_cfc2(r: u32) -> u32 {
    with(|rt| rt.gte.borrow().ctrl[(r & 31) as usize])
}

#[unsafe(export_name = "gte_mtc2")]
pub extern "C" fn gte_mtc2(r: u32, v: u32) {
    with(|rt| rt.gte.borrow_mut().data[(r & 31) as usize] = v);
}

#[unsafe(export_name = "gte_ctc2")]
pub extern "C" fn gte_ctc2(r: u32, v: u32) {
    with(|rt| rt.gte.borrow_mut().ctrl[(r & 31) as usize] = v);
}

/// GTE commands: the GTE arrives with M4 (psx-gte).
#[unsafe(export_name = "gte_exec")]
pub extern "C" fn gte_exec(cmd: u32) {
    with(|rt| rt.m.borrow_mut().count(&format!("gte.{:#04x}", cmd & 0x3F)));
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
        format!(
            "{{\"event\":\"report\",\"vblanks\":{},\"cycles\":{},\"cd_sectors\":{},\"edc_errors\":[{}],\"counts\":{{{}}}}}",
            m.vblanks,
            m.clock.now,
            m.sdk.cd.sectors,
            errors.join(","),
            counts.join(",")
        )
    });
    report(json);
}
