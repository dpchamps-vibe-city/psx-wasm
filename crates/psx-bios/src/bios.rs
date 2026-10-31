//! The BIOS function tables A0h/B0h/C0h and `syscall` (psx-spx "BIOS
//! Function Summary"). Only the calls guest code makes are implemented;
//! anything else is an error naming the table and number, so a gap fails
//! loudly instead of returning garbage.

use crate::kernel::{self, File};
use crate::libc;
use crate::machine::{Flow, Machine, Sdk, Wait};
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
        (0xA0, 0x49) => "GPU_cw",
        (0xA0, 0x55) | (0xA0, 0x70) => "_bu_init",
        (0xA0, 0x72) => "_96_remove",
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
        (0xB0, 0x3F) => "puts",
        (0xB0, 0x41) => "format",
        (0xB0, 0x42) => "firstfile",
        (0xB0, 0x43) => "nextfile",
        (0xB0, 0x44) => "rename",
        (0xB0, 0x45) => "erase",
        (0xB0, 0x46) => "undelete",
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
        (0xC0, 0x02) => "SysEnqIntRP",
        (0xC0, 0x03) => "SysDeqIntRP",
        (0xC0, 0x0A) => "ChangeClearRCnt",
        (0xC0, 0x0D) => "SetIrqAutoAck",
        (0xC0, 0x1A) => "set_card_find_mode",
        (0xC0, 0x1D) => "get_card_find_mode",
        _ => "?",
    }
}

/// Whether [`call`] implements `(table, func)` (every implemented call is
/// named, and only those).
pub fn implemented(table: u32, func: u32) -> bool {
    let (table, func) = if table == 0xA0 && func <= 4 { (0xB0, 0x32 + func) } else { (table, func) };
    name(table, func) != "?"
}

fn len(n: u32) -> u32 {
    n.min(MAX_LEN)
}

/// A0h/B0h/C0h function `func` with o32 arguments `a` (more on the guest
/// stack). Errors name a function the BIOS doesn't implement.
pub fn call<S: Sdk>(m: &mut Machine<S>, table: u32, func: u32, a: [u32; 4]) -> Result<Flow<S>, String> {
    // A(00h)..A(04h) are the B(32h)..B(36h) file functions.
    let (table, func) = if table == 0xA0 && func <= 4 { (0xB0, 0x32 + func) } else { (table, func) };
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
            let text = printf(m, a);
            m.host.log(2, text.trim_end_matches('\n'));
            r(text.len() as u32)
        }
        (0xA0, 0x44) | (0xA0, 0x72) | (0xA0, 0x9F) => r(0),
        (0xA0, 0x55) | (0xA0, 0x70) => {
            m.cards_ready();
            r(0)
        }
        (0xA0, 0x49) => {
            // GPU_cw: a GP0 word (after gpu_sync, which returns 0 here:
            // the GPU front end takes words at once).
            m.gp0(a[0]);
            r(0)
        }
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
            // ResetEntryInt: back to the default exit (ReturnFromException).
            m.kernel.custom_exit = 0;
            r(0)
        }
        (0xB0, 0x19) => {
            m.kernel.custom_exit = a[0];
            r(0)
        }
        (0xB0, 0x20) => {
            m.undeliver_event(a[0], a[1]);
            r(1)
        }
        (0xB0, 0x32) => {
            // OpenBIOS `psxopen`: a free descriptor first, then the device.
            let path = String::from_utf8_lossy(&mem.cstr(a[0], 128)).into_owned();
            let Some(fd) = (0..16).find(|f| !m.kernel.files.contains_key(f) && !m.kernel.cards.files.contains_key(f)) else {
                m.kernel.cards.errno = crate::card::E_NO_FDS;
                return r(u32::MAX);
            };
            if crate::card::parse_path(&path).is_some() {
                return r(m.card_open(&path, a[1], fd));
            }
            if !path.to_ascii_lowercase().starts_with("cdrom:") {
                m.kernel.cards.errno = crate::card::E_NO_DEVICE;
                return r(u32::MAX);
            }
            let mut disc = HostDisc(&mut *m.host);
            match cd::find(&mut disc, &path) {
                Some(e) if !e.dir => {
                    m.kernel.files.insert(fd, File { lba: e.lba, size: e.size, pos: 0 });
                    r(fd)
                }
                _ => {
                    m.kernel.cards.errno = crate::card::E_NOT_FOUND;
                    r(u32::MAX)
                }
            }
        }
        (0xB0, 0x33) => {
            if let Some(f) = m.kernel.cards.files.get_mut(&a[0]) {
                f.pos = match a[2] {
                    0 => a[1],
                    1 => f.pos.wrapping_add(a[1]),
                    _ => return r(u32::MAX),
                };
                return r(f.pos);
            }
            let Some(f) = m.kernel.files.get_mut(&a[0]) else { return r(u32::MAX) };
            f.pos = match a[2] {
                0 => a[1],
                1 => f.pos.wrapping_add(a[1]),
                _ => return r(u32::MAX),
            };
            r(f.pos)
        }
        (0xB0, 0x34) => {
            if m.kernel.cards.files.contains_key(&a[0]) {
                return r(m.card_io(a[0], a[1], a[2], false));
            }
            let Some(f) = m.kernel.files.get(&a[0]).copied() else { return r(u32::MAX) };
            let n = a[2].min(f.size.saturating_sub(f.pos));
            let disc = HostDisc(&mut *m.host);
            let mut got = Vec::with_capacity(n as usize);
            let mut raw = [0u8; cd::RAW];
            let mut s = f.pos / cd::USER as u32;
            while (got.len() as u32) < n {
                if !disc.0.disc_read(f.lba + s, &mut raw) {
                    return r(u32::MAX);
                }
                let user = cd::user_data(&raw);
                let start = if s == f.pos / cd::USER as u32 { (f.pos % cd::USER as u32) as usize } else { 0 };
                let take = ((n as usize) - got.len()).min(cd::USER - start);
                got.extend_from_slice(&user[start..start + take]);
                s += 1;
            }
            m.mem.write(a[1], &got);
            m.loaded.push((a[1], got.len() as u32));
            m.kernel.files.get_mut(&a[0]).unwrap().pos += n;
            r(n)
        }
        (0xB0, 0x35) => {
            if m.kernel.cards.files.contains_key(&a[0]) {
                return r(m.card_io(a[0], a[1], a[2], true));
            }
            r(u32::MAX)
        }
        (0xB0, 0x36) => {
            let known = m.kernel.files.remove(&a[0]).is_some() | m.kernel.cards.files.remove(&a[0]).is_some();
            r(if known { a[0] } else { u32::MAX })
        }
        (0xB0, 0x41) => {
            let path = String::from_utf8_lossy(&mem.cstr(a[0], 128)).into_owned();
            r(m.card_format(&path))
        }
        (0xB0, 0x3F) => {
            let s = String::from_utf8_lossy(&mem.cstr(a[0], 1024)).into_owned();
            m.host.log(2, &s);
            r(0)
        }
        (0xB0, 0x42) => {
            let pat = String::from_utf8_lossy(&mem.cstr(a[0], 128)).into_owned();
            if crate::card::parse_path(&pat).is_some() {
                return r(m.card_find(Some(&pat), a[1]));
            }
            r(0)
        }
        (0xB0, 0x43) => r(m.card_find(None, a[0])),
        (0xB0, 0x44) => {
            let from = String::from_utf8_lossy(&mem.cstr(a[0], 128)).into_owned();
            let to = String::from_utf8_lossy(&mem.cstr(a[1], 128)).into_owned();
            r(m.card_rename(&from, &to))
        }
        (0xB0, 0x45) | (0xB0, 0x46) => {
            let path = String::from_utf8_lossy(&mem.cstr(a[0], 128)).into_owned();
            r(m.card_erase(&path, func == 0x46))
        }
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
        (0xB0, 0x54) => r(m.kernel.cards.errno),
        (0xB0, 0x55) => r(if m.kernel.cards.files.contains_key(&a[0]) { m.kernel.cards.errno } else { u32::MAX }),
        (0xB0, 0x5C) => {
            m.cards_ready();
            r(m.kernel.cards.status[(a[0] & 1) as usize])
        }
        (0xB0, 0x5D) => r(m.card_wait(a[0])),
        // No BIOS ROM, so no ROM kanji font.
        (0xB0, 0x51) => r(u32::MAX),
        // Kernel tables: addresses in kernel RAM, as the Sony BIOS has them.
        (0xB0, 0x56) => r(0x0000_0674),
        (0xB0, 0x57) => r(0x0000_0874),
        // ChangeClearPad: whether the pad handler acknowledges vblank; the
        // old setting (OpenBIOS `setSIO0AutoAck`).
        (0xB0, 0x5B) => r(std::mem::replace(&mut m.kernel.clear_pad, a[0])),

        // --- C0h: interrupt plumbing -------------------------------------
        (0xC0, 0x02) => {
            m.enq_int_rp(a[0], a[1]);
            r(0)
        }
        (0xC0, 0x03) => r(m.deq_int_rp(a[0], a[1])),
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
        n => Err(format!("syscall {n} not implemented")),
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
    let mut counts = Vec::new();
    let text = format(m, a[0], &mut counts, &mut || {
        let v = m.arg(&a, k);
        k += 1;
        v
    });
    for (addr, n, half) in counts {
        if half { m.mem.w16(addr, n as u16) } else { m.mem.w32(addr, n) }
    }
    text
}

/// [`printf`]'s formatting of the string at guest address `fmt`, each
/// argument word from `next`: psx-spx's conversions (`c s d i D u U o O p
/// x X n`) and prefixes (`+`, space, `-`, `#`, `0`, a width or `*`, a
/// precision or `.*`, `h`, `l`, `L`), with C's meaning. `%n`'s writes
/// (address, length, 16-bit) go to `counts`.
pub fn format<S: Sdk>(m: &Machine<S>, fmt: u32, counts: &mut Vec<(u32, u32, bool)>, next: &mut dyn FnMut() -> u32) -> String {
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
            let w = next() as i32;
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
                (next() as i32).max(0) as usize
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
            let v = next();
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
                let s = m.mem.cstr(next(), prec.unwrap_or(1024));
                ("", "", String::from_utf8_lossy(&s).into_owned())
            }
            b'n' => {
                counts.push((next(), out.len() as u32, half));
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
