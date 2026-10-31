//! The BIOS function tables A0h/B0h/C0h and `syscall` (psx-spx "BIOS
//! Function Summary"). Only the calls guest code makes are implemented;
//! anything else is an error naming the table and number, so a gap fails
//! loudly instead of returning garbage.

use crate::kernel;
use crate::libc;
use crate::machine::{Flow, GuestCall, Machine, Sdk, Wait};
use crate::{files, tables};
use psx_devices::cd;

/// The BIOS's own copy routines run byte by byte; lengths are bounded so a
/// wild argument can't spin for minutes.
pub(crate) const MAX_LEN: u32 = 0x0020_0000;

pub fn name(table: u32, func: u32) -> &'static str {
    match (table, func) {
        (0xA0, 0x0A) => "todigit",
        (0xA0, 0x0C) => "strtoul",
        (0xA0, 0x0D) => "strtol",
        (0xA0, 0x0E) => "abs",
        (0xA0, 0x0F) => "labs",
        (0xA0, 0x10) => "atoi",
        (0xA0, 0x11) => "atol",
        (0xA0, 0x12) => "atob",
        (0xA0, 0x13) => "setjmp",
        (0xA0, 0x14) => "longjmp",
        (0xA0, 0x15) => "strcat",
        (0xA0, 0x16) => "strncat",
        (0xA0, 0x17) => "strcmp",
        (0xA0, 0x18) => "strncmp",
        (0xA0, 0x19) => "strcpy",
        (0xA0, 0x1A) => "strncpy",
        (0xA0, 0x1B) => "strlen",
        (0xA0, 0x1C) => "index",
        (0xA0, 0x1D) => "rindex",
        (0xA0, 0x1E) => "strchr",
        (0xA0, 0x1F) => "strrchr",
        (0xA0, 0x20) => "strpbrk",
        (0xA0, 0x21) => "strspn",
        (0xA0, 0x22) => "strcspn",
        (0xA0, 0x23) => "strtok",
        (0xA0, 0x24) => "strstr",
        (0xA0, 0x25) => "toupper",
        (0xA0, 0x26) => "tolower",
        (0xA0, 0x27) => "bcopy",
        (0xA0, 0x28) => "bzero",
        (0xA0, 0x29) => "bcmp",
        (0xA0, 0x2A) => "memcpy",
        (0xA0, 0x2B) => "memset",
        (0xA0, 0x2C) => "memmove",
        (0xA0, 0x2D) => "memcmp",
        (0xA0, 0x2E) => "memchr",
        (0xA0, 0x2F) => "rand",
        (0xA0, 0x30) => "srand",
        (0xA0, 0x31) => "qsort",
        (0xA0, 0x33) => "malloc",
        (0xA0, 0x34) => "free",
        (0xA0, 0x35) => "lsearch",
        (0xA0, 0x36) => "bsearch",
        (0xA0, 0x37) => "calloc",
        (0xA0, 0x38) => "realloc",
        (0xA0, 0x39) => "InitHeap",
        (0xA0, 0x3F) => "printf",
        (0xA0, 0x43) => "Exec",
        (0xA0, 0x44) => "FlushCache",
        (0xA0, 0x45) => "init_a0_b0_c0_vectors",
        (0xA0, 0x46) => "GPU_dw",
        (0xA0, 0x47) => "gpu_send_dma",
        (0xA0, 0x48) => "SendGP1Command",
        (0xA0, 0x49) => "GPU_cw",
        (0xA0, 0x4A) => "GPU_cwp",
        (0xA0, 0x4B) => "send_gpu_linked_list",
        (0xA0, 0x4C) => "gpu_abort_dma",
        (0xA0, 0x4D) => "GetGPUStatus",
        (0xA0, 0x4E) => "gpu_sync",
        (0xA0, 0x55) => "_bu_init",
        (0xA0, 0x56) => "_96_remove",
        (0xA0, 0x57) => "dev_null",
        (0xA0, 0x5B) => "dev_tty_init",
        (0xA0, 0x5C) => "dev_tty_open",
        (0xA0, 0x5D) => "dev_tty_in_out",
        (0xA0, 0x5E) => "dev_tty_ioctl",
        (0xA0, 0x5F) => "dev_cd_open",
        (0xA0, 0x60) => "dev_cd_read",
        (0xA0, 0x61) => "dev_cd_close",
        (0xA0, 0x62) => "dev_cd_firstfile",
        (0xA0, 0x63) => "dev_cd_nextfile",
        (0xA0, 0x64) => "dev_cd_chdir",
        (0xA0, 0x65) => "dev_bu_open",
        (0xA0, 0x66) => "dev_bu_read",
        (0xA0, 0x67) => "dev_bu_write",
        (0xA0, 0x68) => "dev_bu_close",
        (0xA0, 0x69) => "dev_bu_firstfile",
        (0xA0, 0x6A) => "dev_bu_nextfile",
        (0xA0, 0x6B) => "dev_bu_erase",
        (0xA0, 0x6C) => "dev_bu_undelete",
        (0xA0, 0x6D) => "dev_bu_format",
        (0xA0, 0x6E) => "dev_bu_rename",
        (0xA0, 0x96) => "AddCdromDevice",
        (0xA0, 0x97) => "AddMemCardDevice",
        (0xA0, 0x98) => "AddDuartTtyDevice",
        (0xA0, 0x99) => "AddNullConDevice",
        (0xA0, 0x9F) => "SetMemSize",
        (0xA0, 0xA1) => "SystemError",
        (0xA0, 0xAB) => "_card_info",
        (0xA0, 0xAC) => "_card_load",
        (0xA0, 0xAD) => "_card_auto",
        (0xA0, 0xAF) => "card_write_test",
        (0xB0, 0x00) => "alloc_kernel_memory",
        (0xB0, 0x01) => "free_kernel_memory",
        (0xB0, 0x02) => "init_timer",
        (0xB0, 0x03) => "get_timer",
        (0xB0, 0x04) => "enable_timer_irq",
        (0xB0, 0x05) => "disable_timer_irq",
        (0xB0, 0x06) => "restart_timer",
        (0xB0, 0x07) => "DeliverEvent",
        (0xB0, 0x08) => "OpenEvent",
        (0xB0, 0x09) => "CloseEvent",
        (0xB0, 0x0A) => "WaitEvent",
        (0xB0, 0x0B) => "TestEvent",
        (0xB0, 0x0C) => "EnableEvent",
        (0xB0, 0x0D) => "DisableEvent",
        (0xB0, 0x0E) => "OpenThread",
        (0xB0, 0x0F) => "CloseThread",
        (0xB0, 0x10) => "ChangeThread",
        (0xB0, 0x12) => "InitPAD",
        (0xB0, 0x13) => "StartPAD",
        (0xB0, 0x14) => "StopPAD",
        (0xB0, 0x15) => "OutdatedPadInitAndStart",
        (0xB0, 0x16) => "OutdatedPadGetButtons",
        (0xB0, 0x17) => "ReturnFromException",
        (0xB0, 0x18) => "SetDefaultExitFromException",
        (0xB0, 0x19) => "SetCustomExitFromException",
        (0xB0, 0x20) => "UnDeliverEvent",
        (0xB0, 0x32) => "open",
        (0xB0, 0x33) => "lseek",
        (0xB0, 0x34) => "read",
        (0xB0, 0x35) => "write",
        (0xB0, 0x36) => "close",
        (0xB0, 0x37) => "ioctl",
        (0xB0, 0x39) => "isatty",
        (0xB0, 0x3A) => "getc",
        (0xB0, 0x3B) => "putc",
        (0xB0, 0x3C) => "getchar",
        (0xB0, 0x3D) => "putchar",
        (0xB0, 0x3E) => "gets",
        (0xB0, 0x3F) => "puts",
        (0xB0, 0x40) => "cd",
        (0xB0, 0x41) => "format",
        (0xB0, 0x42) => "firstfile",
        (0xB0, 0x43) => "nextfile",
        (0xB0, 0x44) => "rename",
        (0xB0, 0x45) => "erase",
        (0xB0, 0x46) => "undelete",
        (0xB0, 0x47) => "AddDrv",
        (0xB0, 0x48) => "DelDrv",
        (0xB0, 0x49) => "PrintInstalledDevices",
        (0xB0, 0x4A) => "InitCard",
        (0xB0, 0x4B) => "StartCard",
        (0xB0, 0x4C) => "StopCard",
        (0xB0, 0x4E) => "write_card_sector",
        (0xB0, 0x4F) => "read_card_sector",
        (0xB0, 0x50) => "allow_new_card",
        (0xB0, 0x51) => "Krom2RawAdd",
        (0xB0, 0x56) => "GetC0Table",
        (0xB0, 0x57) => "GetB0Table",
        (0xB0, 0x54) => "_get_errno",
        (0xB0, 0x55) => "_get_error",
        (0xB0, 0x5B) => "ChangeClearPad",
        (0xB0, 0x5C) => "_card_status",
        (0xB0, 0x5D) => "_card_wait",
        (0xB0, 0x59) => "testdevice",
        (0xC0, 0x02) => "SysEnqIntRP",
        (0xC0, 0x03) => "SysDeqIntRP",
        (0xC0, 0x07) => "InstallExceptionHandlers",
        (0xC0, 0x1C) => "AdjustA0Table",
        (0xC0, 0x0A) => "ChangeClearRCnt",
        (0xC0, 0x0D) => "SetIrqAutoAck",
        (0xC0, 0x12) => "InstallDevices",
        (0xC0, 0x13) => "FlushStdInOutPut",
        (0xC0, 0x1B) => "KernelRedirect",
        (0xC0, 0x1A) => "set_card_find_mode",
        (0xC0, 0x1D) => "get_card_find_mode",
        _ => "?",
    }
}

/// Whether [`call`] implements `(table, func)` (every implemented call is
/// named, and only those).
pub fn implemented(table: u32, func: u32) -> bool {
    let (table, func) = tables::canonical(table, func);
    name(table, func) != "?"
}

fn len(n: u32) -> u32 {
    n.min(MAX_LEN)
}

/// A vector call consults its guest-visible table. A saved native entry
/// still calls its original service; a guest patch receives the same o32
/// arguments and returns through the existing guest-call continuation.
pub fn dispatch<S: Sdk>(m: &mut Machine<S>, table: u32, func: u32, a: [u32; 4]) -> Result<Flow<S>, String> {
    let Some(at) = tables::entry(table, func) else { return call(m, table, func, a) };
    let entry = m.mem.r32(at);
    if let Some((t, f)) = tables::stand_in_fn(entry) {
        return call(m, t, f, a);
    }
    m.count(&format!("bios.{table:02X}h:{func:02X}h patched"));
    Ok(Flow::Call { call: GuestCall { addr: entry, args: a }, then: Box::new(|_, v| Flow::Return(v)) })
}

/// A0h/B0h/C0h function `func` with o32 arguments `a` (more on the guest
/// stack). Errors name a function the BIOS doesn't implement.
pub fn call<S: Sdk>(m: &mut Machine<S>, table: u32, func: u32, a: [u32; 4]) -> Result<Flow<S>, String> {
    let (table, func) = tables::canonical(table, func);
    let label = format!("bios.{:02X}h:{:02X}h {}", table, func, name(table, func));
    m.count(&label);
    let r = |v: u32| Ok(Flow::ret(v));
    let mem = &mut *m.mem;
    match (table, func) {
        // --- A0h: C library (psx-spx's behaviour, its bugs included) -----
        (0xA0, 0x0A) => r(libc::todigit(a[0])),
        (0xA0, 0x0C) | (0xA0, 0x0D) => {
            // strtoul, strtol: [src_end] the end, unless src is 0.
            if a[0] == 0 {
                return r(0);
            }
            let (v, end) = libc::strtol(mem, a[0], a[2], func == 0x0D, false);
            if a[1] != 0 {
                mem.w32(a[1], end);
            }
            r(v)
        }
        (0xA0, 0x0E) | (0xA0, 0x0F) => r((a[0] as i32).wrapping_abs() as u32),
        (0xA0, 0x10) | (0xA0, 0x11) => r(if a[0] == 0 { 0 } else { libc::strtol(mem, a[0], 10, true, true).0 }),
        (0xA0, 0x12) => {
            // atob: strtol(src, &end, 10), its two results exchanged.
            let (v, end) = if a[0] == 0 { (0, 0) } else { libc::strtol(mem, a[0], 10, true, false) };
            mem.w32(a[1], v);
            r(end)
        }
        (0xA0, 0x13) => {
            // setjmp: a translated caller keeps its registers in wasm
            // locals, so only the first return (0) is representable;
            // longjmp traps.
            r(0)
        }
        (0xA0, 0x14) => Err(format!("{label}: longjmp across translated frames is not supported")),
        (0xA0, 0x43) => Err(format!("{label}: loading and running another executable is not supported")),
        (0xA0, 0x15) | (0xA0, 0x16) => {
            // strcat, strncat (at most maxlen characters, none for a
            // negative maxlen); 0 for a null pointer.
            if a[0] == 0 || a[1] == 0 {
                return r(0);
            }
            let n = match func {
                0x15 => MAX_LEN,
                _ if (a[2] as i32) < 0 => 0,
                _ => len(a[2]),
            };
            let d = mem.cstr(a[0], MAX_LEN as usize);
            let mut s = mem.cstr(a[1], n as usize);
            s.push(0);
            mem.write(a[0] + d.len() as u32, &s);
            r(a[0])
        }
        (0xA0, 0x17) | (0xA0, 0x18) => {
            // strcmp, strncmp: the bytes sign-extended; a null pointer
            // compares below a string.
            let n = if func == 0x18 { len(a[2]) } else { MAX_LEN };
            let d = match (a[0], a[1]) {
                (0, 0) => 0,
                (0, _) => -1,
                (_, 0) => 1,
                _ => (0..n)
                    .map(|i| (mem.r8(a[0] + i) as i8 as i32, mem.r8(a[1] + i) as i8 as i32))
                    .find(|&(x, y)| x != y || x == 0)
                    .map_or(0, |(x, y)| x - y),
            };
            r(d as u32)
        }
        (0xA0, 0x19) | (0xA0, 0x1A) => {
            // strcpy, strncpy (exactly maxlen bytes: no terminator for a
            // string as long, zeros after a shorter one); 0 for a null
            // pointer.
            if a[0] == 0 || a[1] == 0 {
                return r(0);
            }
            let s = if func == 0x19 {
                let mut s = mem.cstr(a[1], MAX_LEN as usize);
                s.push(0);
                s
            } else {
                let mut s = mem.cstr(a[1], len(a[2]) as usize);
                s.resize(len(a[2]) as usize, 0);
                s
            };
            mem.write(a[0], &s);
            r(a[0])
        }
        (0xA0, 0x1B) => r(if a[0] == 0 { 0 } else { mem.cstr(a[0], MAX_LEN as usize).len() as u32 }),
        (0xA0, 0x1C..=0x1F) => {
            // index and strchr, rindex and strrchr: 0 for a null string;
            // char 0 finds the terminator.
            if a[0] == 0 {
                return r(0);
            }
            let s = mem.cstr(a[0], MAX_LEN as usize);
            let c = a[1] as u8;
            let hit = if func & 1 == 0 { s.iter().position(|&b| b == c) } else { s.iter().rposition(|&b| b == c) };
            let hit = hit.or((c == 0).then_some(s.len()));
            r(hit.map_or(0, |i| a[0] + i as u32))
        }
        (0xA0, 0x20) => r(libc::strpbrk(mem, a[0], a[1])),
        (0xA0, 0x21) | (0xA0, 0x22) => r(libc::span(mem, a[0], a[1], func == 0x21)),
        (0xA0, 0x23) => r(libc::strtok(m, a[0], a[1])),
        (0xA0, 0x24) => r(libc::strstr(mem, a[0], a[1])),
        (0xA0, 0x25) => r((a[0] as u8).to_ascii_uppercase() as u32),
        (0xA0, 0x26) => r((a[0] as u8).to_ascii_lowercase() as u32),
        (0xA0, 0x27) | (0xA0, 0x2A) => {
            // bcopy(src, dst, len), memcpy(dst, src, len): nothing copied
            // when the first argument is 0 or len is over 7FFFFFFFh; that
            // argument returned.
            let (dst, src) = if func == 0x27 { (a[1], a[0]) } else { (a[0], a[1]) };
            if a[0] != 0 && (a[2] as i32) >= 0 {
                libc::copy(mem, dst, src, len(a[2]));
            }
            r(a[0])
        }
        (0xA0, 0x28) | (0xA0, 0x2B) => {
            // bzero(dst, len), memset(dst, c, len): dst, or 0 when nothing
            // is filled (dst 0, len 0 or over 7FFFFFFFh).
            let (n, c) = if func == 0x28 { (a[1], 0) } else { (a[2], a[1] as u8) };
            if a[0] == 0 || n == 0 || (n as i32) < 0 {
                return r(0);
            }
            mem.write(a[0], &vec![c; len(n) as usize]);
            r(a[0])
        }
        (0xA0, 0x29) | (0xA0, 0x2D) => {
            // bcmp, memcmp: psx-spx's bug, the difference of the bytes
            // after the first mismatch; 0 for a null pointer.
            if a[0] == 0 || a[1] == 0 {
                return r(0);
            }
            let hit = (0..len(a[2])).find(|&i| mem.r8(a[0] + i) != mem.r8(a[1] + i));
            r(hit.map_or(0, |i| (mem.r8(a[0] + i + 1) as i32 - mem.r8(a[1] + i + 1) as i32) as u32))
        }
        (0xA0, 0x2C) => {
            // memmove: as memcpy, but backwards when src < dst and (psx-
            // spx's condition) dst >= src+len; psx-spx's bug: the
            // backwards copy moves len+1 bytes.
            let (dst, src) = (a[0], a[1]);
            if dst != 0 && (a[2] as i32) >= 0 {
                let n = len(a[2]);
                if src < dst && dst >= src.wrapping_add(n) {
                    let b = mem.bytes(src, n as usize + 1);
                    mem.write(dst, &b);
                } else {
                    libc::copy(mem, dst, src, n);
                }
            }
            r(dst)
        }
        (0xA0, 0x2E) => {
            // memchr: 0 for a null pointer or len over 7FFFFFFFh.
            if a[0] == 0 || (a[2] as i32) < 0 {
                return r(0);
            }
            let hit = (0..len(a[2])).find(|&i| mem.r8(a[0] + i) == a[1] as u8);
            r(hit.map_or(0, |i| a[0] + i))
        }
        (0xA0, 0x2F) => {
            // The kernel's LCG: x = x * 41C64E6Dh + 3039h, 15 bits of it.
            m.kernel.rand = m.kernel.rand.wrapping_mul(0x41C6_4E6D).wrapping_add(0x3039);
            r((m.kernel.rand >> 16) & 0x7FFF)
        }
        (0xA0, 0x30) => {
            m.kernel.rand = a[0];
            r(0)
        }
        (0xA0, 0x31) => Ok(libc::qsort(m, a)),
        (0xA0, 0x33) => r(libc::malloc(mem, m.kernel.heap, a[0])),
        (0xA0, 0x34) | (0xB0, 0x01) => libc::free(mem, a[0]).map(|_| Flow::ret(0)).map_err(|e| format!("{label}: {e}")),
        (0xA0, 0x35) => Ok(libc::lsearch(m, a)),
        (0xA0, 0x36) => Ok(libc::bsearch(m, a)),
        (0xA0, 0x37) => r(libc::calloc(mem, m.kernel.heap, a[0], a[1])),
        (0xA0, 0x38) => libc::realloc(mem, m.kernel.heap, a[0], a[1]).map(Flow::ret).map_err(|e| format!("{label}: {e}")),
        (0xA0, 0x39) => {
            m.kernel.heap = libc::heap_init(mem, a[0], a[1]);
            r(0)
        }
        (0xA0, 0x3F) => {
            // Through putchar, to the console (psx-spx).
            let text = printf(m, a);
            let n = text.len() as u32;
            let flow = m.putchars(text.into_bytes())?;
            Ok(crate::machine::seq(m, flow, move |_, _| Flow::ret(n)))
        }
        (0xA0, 0x44) | (0xA0, 0x56) | (0xA0, 0x9F) => r(0),
        // "return 0" (psx-spx), and the dummy device functions.
        (0xA0, 0x57) => r(0),
        // --- A0h: the built-in devices, over the native backends ----------
        (0xA0, 0x5B) | (0xA0, 0x5E) => r(0),
        (0xA0, 0x5C) => r(m.tty_open(a[0])),
        (0xA0, 0x5D) => m.tty_in_out(a[0], a[1]).map(Flow::ret).map_err(|e| format!("{label}: {e}")),
        (0xA0, 0x5F) => r(m.dev_cd_open(a[0], a[1])),
        (0xA0, 0x60) => r(m.dev_cd_read(a[0], a[1], a[2])),
        (0xA0, 0x61) => r(m.dev_cd_close(a[0])),
        (0xA0, 0x62) => r(m.dev_cd_firstfile(a[0], a[1], a[2])),
        (0xA0, 0x63) => r(m.dev_cd_nextfile(a[0], a[1])),
        (0xA0, 0x64) => r(m.dev_cd_chdir(a[1])),
        (0xA0, 0x65) => r(m.dev_bu_open(a[0], a[1], a[2])),
        (0xA0, 0x66) | (0xA0, 0x67) => r(m.dev_bu_io(a[0], a[1], a[2], func == 0x67)),
        (0xA0, 0x68) => r(m.dev_bu_close(a[0])),
        (0xA0, 0x69) => r(m.dev_bu_find(a[0], a[1], a[2])),
        (0xA0, 0x6A) => r(m.dev_bu_find(a[0], 0, a[1])),
        (0xA0, 0x6B) | (0xA0, 0x6C) => r(m.dev_bu_erase(a[0], a[1], func == 0x6C)),
        (0xA0, 0x6D) => r(m.dev_bu_format(a[0])),
        (0xA0, 0x6E) => r(m.dev_bu_rename(a[0], a[1], a[3])),
        (0xA0, 0x96..=0x99) => r(m.add_device(func)),
        (0xA0, 0x45) => {
            m.install_vectors();
            r(0)
        }
        (0xA0, 0x55) => {
            m.cards_ready();
            r(0)
        }
        (0xA0, 0x49) => {
            // GPU_cw: a GP0 word (after gpu_sync, which returns 0 here:
            // the GPU front end takes words at once).
            m.gp0(a[0]);
            r(0)
        }
        (0xA0, 0x46) | (0xA0, 0x47) => {
            // GPU_dw, gpu_send_dma: GP0(A0h) and w*h/2 pixel words, by
            // software or by channel 2. Both leave their arguments in the
            // caller's stack frame; GPU_dw returns the end address (and
            // leaves it in [sp+10h]), gpu_send_dma GP0's address.
            let dma = func == 0x47;
            if dma {
                m.gpu_sync()?;
            } else if m.gpu_status()? & 1 << 26 == 0 {
                return Err(format!("{label}: GPUSTAT bit 26 stays clear (a GP0 command is half sent)"));
            }
            let (src, words) = (m.arg(&a, 4), len(a[2].wrapping_mul(a[3]) / 2));
            for w in [0xA000_0000, a[1] << 16 | a[0] & 0xFFFF, a[3] << 16 | a[2] & 0xFFFF] {
                m.gp0(w);
            }
            let sp = m.regs.sp();
            for (k, v) in [a[1], a[2], a[3]].into_iter().enumerate() {
                m.mem.w32(sp + 4 * (k as u32 + 1), v);
            }
            if dma {
                m.gp1(0x0400_0002)?;
                let blocks = words / 16;
                m.gpu_dma(src, blocks << 16 | 0x10, 0x0100_0201, false)?;
                return r(0x1F80_1810);
            }
            for i in 0..words {
                let w = m.mem.r32(src.wrapping_add(4 * i));
                m.gp0(w);
            }
            let end = src.wrapping_add(4 * words);
            m.mem.w32(sp + 0x10, end);
            r(end)
        }
        (0xA0, 0x48) => {
            // SendGP1Command: a GP1 word, nothing returned.
            m.gp1(a[0])?;
            r(0)
        }
        (0xA0, 0x4A) => {
            // GPU_cwp: GPU words from memory, after a sync.
            m.gpu_sync()?;
            for i in 0..len(a[1]) {
                let w = m.mem.r32(a[0].wrapping_add(4 * i));
                m.gp0(w);
            }
            r(0)
        }
        (0xA0, 0x4B) => {
            // send_gpu_linked_list: channel 2 walks the list; DICR is
            // cleared first as psx-spx gives it.
            m.gpu_sync()?;
            m.gp1(0x0400_0002)?;
            m.gpu_dma(a[0], 0, 0x0100_0401, true)?;
            r(0)
        }
        (0xA0, 0x4C) => {
            // gpu_abort_dma: the channel stopped; its result is GP1's
            // address.
            m.gpu_abort_dma()?;
            r(0x1F80_1814)
        }
        (0xA0, 0x4D) => r(m.gpu_status()?),
        (0xA0, 0x4E) => r(m.gpu_sync()?),
        (0xA0, 0xA1) => Err(format!("{label}: SystemError({:#x}, {:#x})", a[0], a[1])),
        (0xA0, 0xAB) => r(m.card_info(a[0])),
        (0xA0, 0xAC) => r(m.card_load(a[0])),
        (0xA0, 0xAD) => {
            m.kernel.cards.auto_format = a[0] != 0;
            r(0)
        }
        (0xA0, 0xAF) => r(m.card_write_test(a[0])),

        // --- B0h: kernel memory, timers, events, pads, files, cards -----
        (0xB0, 0x00) => {
            let heap = m.kernel_heap();
            r(libc::malloc(&mut *m.mem, heap, a[0]))
        }
        (0xB0, 0x02..=0x06) => timer(m, func, a).map(Flow::ret).map_err(|e| format!("{label}: {e}")),
        (0xB0, 0x07) => {
            m.deliver_event(a[0], a[1]);
            r(1)
        }
        (0xB0, 0x08) => r(m.open_event(a[0], a[1], a[2], a[3])),
        (0xB0, 0x09) => {
            m.set_ev_status(a[0], kernel::EV_UNUSED);
            r(1)
        }
        (0xB0, 0x0A) => {
            let h = a[0];
            match m.ev(h).map(|e| e.status) {
                Some(kernel::EV_READY) => {
                    m.set_ev_status(h, kernel::EV_ENABLED);
                    r(1)
                }
                Some(kernel::EV_ENABLED) => Ok(Flow::Block {
                    until: Wait::Event(h),
                    then: Box::new(move |m: &mut Machine<S>| {
                        m.set_ev_status(h, kernel::EV_ENABLED);
                        Flow::ret(1)
                    }),
                }),
                _ => r(0),
            }
        }
        (0xB0, 0x0B) => {
            let ready = m.ev(a[0]).is_some_and(|e| e.status == kernel::EV_READY);
            if ready {
                m.set_ev_status(a[0], kernel::EV_ENABLED);
            }
            r(ready as u32)
        }
        // EnableEvent, DisableEvent: a closed slot stays closed; enabling
        // a ready event makes it busy again (OpenBIOS `events.c`).
        (0xB0, 0x0C) | (0xB0, 0x0D) => {
            if m.ev(a[0]).is_some_and(|e| e.status != kernel::EV_UNUSED) {
                m.set_ev_status(a[0], if func == 0x0C { kernel::EV_ENABLED } else { kernel::EV_DISABLED });
            }
            r(1)
        }
        (0xB0, 0x0E) => r(m.open_thread(a[0], a[1], a[2])),
        (0xB0, 0x0F) => r(m.close_thread(a[0])),
        (0xB0, 0x10) => m.change_thread(a[0]).map(Flow::ret).map_err(|e| format!("{label}: {e}")),
        (0xB0, 0x12) => {
            m.kernel.init_pad(&mut *m.mem, [(a[0], a[1]), (a[2], a[3])]);
            r(1)
        }
        (0xB0, 0x13) => {
            m.start_sio0(false);
            r(1)
        }
        (0xB0, 0x14) => {
            m.stop_sio0(false);
            r(1)
        }
        (0xB0, 0x15) => {
            let v = m.kernel.pad_init2(&mut *m.mem, a[0], a[1]);
            if v != 0 {
                m.start_sio0(false);
            }
            r(v)
        }
        (0xB0, 0x16) => r(m.kernel.pad_dr(&mut *m.mem)),
        (0xB0, 0x17) => Err(format!("{label}: called outside an exception handler")),
        (0xB0, 0x18) => {
            // ResetEntryInt: restore and return the default exit structure.
            m.kernel.custom_exit = 0;
            r(m.lay_out_default_exit())
        }
        (0xB0, 0x19) => {
            m.kernel.custom_exit = if a[0] & 0x1FFF_FFFF == kernel::DEFAULT_EXIT & 0x1FFF_FFFF { 0 } else { a[0] };
            r(0)
        }
        (0xB0, 0x20) => {
            m.undeliver_event(a[0], a[1]);
            r(1)
        }
        // --- B0h: the file functions over the device table (crate::files)
        (0xB0, 0x32) => m.open(a[0], a[1]),
        (0xB0, 0x33) => r(m.lseek(a[0], a[1], a[2])),
        (0xB0, 0x34) | (0xB0, 0x35) => m.transfer(a[0], a[1], a[2], func == 0x35),
        (0xB0, 0x36) => m.close(a[0]),
        (0xB0, 0x37) => m.ioctl(a[0], a[1], a[2]),
        (0xB0, 0x39) => r(m.isatty(a[0])),
        (0xB0, 0x3A) => m.getc(a[0]),
        (0xB0, 0x3B) => m.putc(a[0], a[1]),
        (0xB0, 0x3C) => m.getchar(),
        (0xB0, 0x3D) => m.putchars(vec![a[0] as u8]),
        (0xB0, 0x3E) => Err(format!("{label}: pending host console input for gets")),
        (0xB0, 0x3F) => {
            // puts: "<NULL>" for a null pointer (psx-spx).
            let s = if a[0] == 0 { b"<NULL>".to_vec() } else { mem.cstr(a[0], MAX_LEN as usize) };
            m.putchars(s)
        }
        (0xB0, 0x40) => m.cd(a[0]),
        (0xB0, 0x41) => m.erase(a[0], files::dcb::FORMAT),
        (0xB0, 0x42) => m.firstfile(a[0], a[1]),
        (0xB0, 0x43) => m.nextfile(a[0]),
        (0xB0, 0x44) => m.rename(a[0], a[1]),
        (0xB0, 0x45) => m.erase(a[0], files::dcb::ERASE),
        (0xB0, 0x46) => m.erase(a[0], files::dcb::UNDELETE),
        (0xB0, 0x47) => m.add_drv(a[0]),
        (0xB0, 0x48) => m.del_drv(a[0]),
        (0xB0, 0x49) => m.print_devices(),
        (0xB0, 0x59) => m.test_device(a[0]),
        (0xB0, 0x4A) => r(m.card_init(a[0])),
        // StartCARD2, StopCARD2: the pad and card handler in or out of the
        // chain.
        (0xB0, 0x4B) | (0xB0, 0x4C) => {
            m.cards_ready();
            if func == 0x4B { m.start_sio0(true) } else { m.stop_sio0(true) }
            r(1)
        }
        (0xB0, 0x4E) => r(m.card_write(a[0], a[1], a[2])),
        (0xB0, 0x4F) => r(m.card_read(a[0], a[1], a[2])),
        (0xB0, 0x50) => {
            m.card_new(0x00);
            m.card_new(0x10);
            r(1)
        }
        (0xB0, 0x54) => r(m.kernel.files.errno),
        (0xB0, 0x55) => r(m.get_error(a[0])),
        (0xB0, 0x5C) => {
            m.cards_ready();
            r(m.kernel.cards.status[(a[0] & 1) as usize])
        }
        (0xB0, 0x5D) => r(m.card_wait(a[0])),
        // No BIOS ROM, so no ROM kanji font.
        (0xB0, 0x51) => r(u32::MAX),
        // Kernel tables: addresses in kernel RAM, as the Sony BIOS has them.
        (0xB0, 0x56) => r(psxw_abi::kernel::C0_TABLE),
        (0xB0, 0x57) => r(psxw_abi::kernel::B0_TABLE),
        // ChangeClearPad: whether the pad handler acknowledges vblank; the
        // old setting (OpenBIOS `setSIO0AutoAck`).
        (0xB0, 0x5B) => r(std::mem::replace(&mut m.kernel.clear_pad, a[0])),

        // --- C0h: interrupt plumbing -------------------------------------
        (0xC0, 0x02) => {
            m.enq_int_rp(a[0], a[1]);
            r(0)
        }
        (0xC0, 0x03) => r(m.deq_int_rp(a[0], a[1])),
        (0xC0, 0x07) => {
            m.install_exception_handlers();
            r(0)
        }
        (0xC0, 0x1C) => {
            m.adjust_a0_table();
            r(0)
        }
        (0xC0, 0x1A) => {
            m.kernel.cards.find_mode = a[0];
            r(0)
        }
        (0xC0, 0x1D) => r(m.kernel.cards.find_mode),
        (0xC0, 0x0A) => {
            let i = (a[0] as usize).min(3);
            let old = m.kernel.clear_rcnt[i];
            m.kernel.clear_rcnt[i] = a[1];
            r(old)
        }
        // SetIrqAutoAck(irq, flag): the IRQ events' acknowledge.
        (0xC0, 0x0D) => {
            if let Some(f) = m.kernel.irq_auto_ack.get_mut(a[0] as usize) {
                *f = a[1];
            }
            r(0)
        }
        (0xC0, 0x12) => {
            // InstallDevices(ttyflag): the control blocks, the console as
            // KernelRedirect mounts it, cdrom and bu.
            m.lay_out_files();
            m.kernel_redirect(a[0])
        }
        (0xC0, 0x13) => m.reopen_stdio(),
        (0xC0, 0x1B) => m.kernel_redirect(a[0]),
        _ => Err(format!("{label}: not implemented")),
    }
}

/// `syscall` with `a0` selecting the function.
pub fn syscall<S: Sdk>(m: &mut Machine<S>, a: [u32; 4]) -> Result<u32, String> {
    m.count(&format!("syscall.{}", a[0]));
    match a[0] {
        // EnterCriticalSection: returns whether interrupts were enabled.
        1 => {
            let was = !m.crit;
            m.crit = true;
            Ok(was as u32)
        }
        // ExitCriticalSection: an interrupt left pending is taken.
        2 => {
            m.crit = false;
            if m.io.as_ref().is_some_and(|io| io.irq.pending()) && !m.in_irq {
                m.queue_irq_chains();
            }
            Ok(0)
        }
        // ChangeThreadSubFunction.
        3 => Ok(1),
        0 => Ok(0),
        // Other syscall numbers deliver the unresolved-exception event.
        // The current seam has no incoming v0: it returns 0 rather than
        // preserving that register as the console does.
        _ => {
            m.deliver_event(0xF000_0010, 0x4000);
            Ok(0)
        },
    }
}

/// B0h:02h-06h over the shared timer core and IRQ mask. Unlike the SDK's
/// SetRCnt, the BIOS uses the same mode calculation for all three timers
/// (psx-spx "BIOS Timer Functions"). Counter 3 selects vblank's IRQ only.
fn timer<S: Sdk>(m: &mut Machine<S>, func: u32, a: [u32; 4]) -> Result<u32, String> {
    let t = (a[0] & 0xFFFF) as usize;
    match func {
        0x02 if t < 3 => {
            let mut mode = if a[2] & 0x10 == 0 { 0x48 } else { 0x49 };
            if a[2] & 1 == 0 {
                mode |= 0x100;
            }
            if a[2] & 0x1000 != 0 {
                mode |= 0x10;
            }
            let now = m.timer_now(t);
            m.timers[t].target = a[1] as u16;
            m.timers[t].write_mode(mode, now);
            m.reschedule_timer(t);
            Ok(1)
        }
        0x03 if t < 3 => Ok(m.timers[t].value(t, m.clock.video, m.timer_now(t)) as u32),
        0x04 | 0x05 => {
            let irq = [0x10, 0x20, 0x40, 0x01].get(t).copied().unwrap_or(0);
            let mask = m.reg_read(psx_devices::irq::I_MASK, 4)?;
            m.reg_write(psx_devices::irq::I_MASK, 4, if func == 0x04 { mask | irq } else { mask & !irq })?;
            m.release_irq();
            Ok((func == 0x05 || t < 3) as u32)
        }
        0x06 if t < 3 => {
            let (video, now) = (m.clock.video, m.timer_now(t));
            m.timers[t].write_count(t, video, now, 0);
            m.reschedule_timer(t);
            Ok(1)
        }
        _ => Ok(0),
    }
}

/// Raw sectors through the host.
pub struct HostDisc<'a>(pub &'a mut dyn crate::machine::Host);

impl cd::Disc for HostDisc<'_> {
    fn read_raw(&mut self, lba: u32, out: &mut [u8; cd::RAW]) -> bool {
        self.0.disc_read(lba, out)
    }
}

/// The BIOS printf: `%d %i %u %x %X %o %c %s %p %%`, with flags, width and
/// precision. Arguments come from a1..a3, then the guest stack.
pub fn printf<S: Sdk>(m: &mut Machine<S>, a: [u32; 4]) -> String {
    let mut k = 1;
    format(m, a[0], &mut |m| {
        let v = m.arg(&a, k);
        k += 1;
        v
    })
}

/// [`printf`]'s formatting of the string at guest address `fmt`, each
/// argument word from `next`: psx-spx's conversions (`c s d i D u U o O p
/// x X n`) and prefixes (`+`, space, `-`, `#`, `0`, a width or `*`, a
/// precision or `.*`, `h`, `l`, `L`), with C's meaning. `%n` writes at
/// its conversion, before later conversions or stack arguments are read.
pub fn format<S: Sdk>(m: &mut Machine<S>, fmt: u32, next: &mut dyn FnMut(&Machine<S>) -> u32) -> String {
    let fmt = m.mem.cstr(fmt, 1024);
    let mut out = String::new();
    let mut i = 0;
    let digits = |fmt: &[u8], i: &mut usize| {
        let mut n = 0usize;
        while *i < fmt.len() && fmt[*i].is_ascii_digit() {
            n = n * 10 + (fmt[*i] - b'0') as usize;
            *i += 1;
        }
        n
    };
    while i < fmt.len() {
        let c = fmt[i];
        i += 1;
        if c != b'%' {
            out.push(c as char);
            continue;
        }
        let mut flags = String::new();
        while i < fmt.len() && b"-+ #0".contains(&fmt[i]) {
            flags.push(fmt[i] as char);
            i += 1;
        }
        let mut left = flags.contains('-');
        let width = if fmt.get(i) == Some(&b'*') {
            i += 1;
            let w = next(m) as i32;
            left |= w < 0;
            w.unsigned_abs() as usize
        } else {
            digits(&fmt, &mut i)
        };
        let mut prec: Option<usize> = None;
        if fmt.get(i) == Some(&b'.') {
            i += 1;
            prec = Some(if fmt.get(i) == Some(&b'*') {
                i += 1;
                (next(m) as i32).max(0) as usize
            } else {
                digits(&fmt, &mut i)
            });
        }
        let mut half = false;
        while i < fmt.len() && b"hlL".contains(&fmt[i]) {
            half = fmt[i] == b'h';
            i += 1;
        }
        let Some(&conv) = fmt.get(i) else { break };
        i += 1;
        // h: the argument sign-extended from 16 bits, for every conversion
        // (psx-spx: unsigned ones too).
        let mut arg = || {
            let v = next(m);
            if half { v as i16 as i32 as u32 } else { v }
        };
        let (sign, prefix, mut body) = match conv {
            b'd' | b'i' | b'D' => {
                let v = arg() as i32;
                let sign = if v < 0 {
                    "-"
                } else if flags.contains('+') {
                    "+"
                } else if flags.contains(' ') {
                    " "
                } else {
                    ""
                };
                (sign, "", v.unsigned_abs().to_string())
            }
            b'u' | b'U' => ("", "", arg().to_string()),
            b'o' | b'O' => ("", if flags.contains('#') { "0" } else { "" }, format!("{:o}", arg())),
            b'x' | b'X' | b'p' => {
                let v = arg();
                let prefix = match (flags.contains('#') && v != 0 && conv != b'p', conv) {
                    (true, b'X') => "0X",
                    (true, _) => "0x",
                    _ => "",
                };
                ("", prefix, if conv == b'X' { format!("{v:X}") } else { format!("{v:x}") })
            }
            b'c' => ("", "", ((arg() as u8) as char).to_string()),
            b's' => {
                let s = m.mem.cstr(next(m), prec.unwrap_or(1024));
                ("", "", String::from_utf8_lossy(&s).into_owned())
            }
            b'n' => {
                let addr = next(m);
                if half { m.mem.w16(addr, out.len() as u16) } else { m.mem.w32(addr, out.len() as u32) }
                continue;
            }
            b'%' => ("", "", "%".into()),
            other => ("", "", format!("%{}", other as char)),
        };
        let numeric = b"diDuUoOxXp".contains(&conv);
        if numeric {
            if let Some(p) = prec {
                if body.len() < p {
                    body = "0".repeat(p - body.len()) + &body;
                }
            }
            if prefix == "0" && body.starts_with('0') {
                body.remove(0);
            }
        }
        let pad = width.saturating_sub(sign.len() + prefix.len() + body.len());
        if left {
            out.push_str(&format!("{sign}{prefix}{body}{}", " ".repeat(pad)));
        } else if flags.contains('0') && numeric && prec.is_none() {
            out.push_str(&format!("{sign}{prefix}{}{body}", "0".repeat(pad)));
        } else {
            out.push_str(&format!("{}{sign}{prefix}{body}", " ".repeat(pad)));
        }
    }
    out
}
