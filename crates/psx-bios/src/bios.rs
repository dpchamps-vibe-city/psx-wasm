//! The BIOS function tables A0h/B0h/C0h and `syscall` (psx-spx "BIOS
//! Function Summary"). Only the calls guest code makes are implemented;
//! anything else is an error naming the table and number, so a gap fails
//! loudly instead of returning garbage.

use crate::kernel::{self, File};
use crate::machine::{Flow, Machine, Sdk, Wait};
use psx_devices::cd;

/// The BIOS's own copy routines run byte by byte; lengths are bounded so a
/// wild argument can't spin for minutes.
const MAX_LEN: u32 = 0x0020_0000;

pub fn name(table: u32, func: u32) -> &'static str {
    match (table, func) {
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
        (0xA0, 0x39) => "InitHeap",
        (0xA0, 0x3F) => "printf",
        (0xA0, 0x44) => "FlushCache",
        (0xA0, 0x49) => "GPU_cw",
        (0xA0, 0x70) => "_bu_init",
        (0xA0, 0x72) => "_96_remove",
        (0xA0, 0x9F) => "SetMemSize",
        (0xA0, 0xA1) => "SystemError",
        (0xA0, 0xAB) => "_card_info",
        (0xA0, 0xAC) => "_card_load",
        (0xB0, 0x07) => "DeliverEvent",
        (0xB0, 0x08) => "OpenEvent",
        (0xB0, 0x09) => "CloseEvent",
        (0xB0, 0x0A) => "WaitEvent",
        (0xB0, 0x0B) => "TestEvent",
        (0xB0, 0x0C) => "EnableEvent",
        (0xB0, 0x0D) => "DisableEvent",
        (0xB0, 0x12) => "InitPAD",
        (0xB0, 0x13) => "StartPAD",
        (0xB0, 0x14) => "StopPAD",
        (0xB0, 0x15) => "OutdatedPadInitAndStart",
        (0xB0, 0x17) => "ReturnFromException",
        (0xB0, 0x18) => "SetDefaultExitFromException",
        (0xB0, 0x19) => "SetCustomExitFromException",
        (0xB0, 0x32) => "open",
        (0xB0, 0x33) => "lseek",
        (0xB0, 0x34) => "read",
        (0xB0, 0x35) => "write",
        (0xB0, 0x36) => "close",
        (0xB0, 0x3F) => "puts",
        (0xB0, 0x42) => "firstfile",
        (0xB0, 0x43) => "nextfile",
        (0xB0, 0x44) => "rename",
        (0xB0, 0x4A) => "InitCard",
        (0xB0, 0x4B) => "StartCard",
        (0xB0, 0x4C) => "StopCard",
        (0xB0, 0x4E) => "write_card_sector",
        (0xB0, 0x4F) => "read_card_sector",
        (0xB0, 0x50) => "allow_new_card",
        (0xB0, 0x51) => "Krom2RawAdd",
        (0xB0, 0x56) => "GetC0Table",
        (0xB0, 0x57) => "GetB0Table",
        (0xB0, 0x5B) => "ChangeClearPad",
        (0xC0, 0x02) => "SysEnqIntRP",
        (0xC0, 0x03) => "SysDeqIntRP",
        (0xC0, 0x0A) => "ChangeClearRCnt",
        _ => "?",
    }
}

/// Whether [`call`] implements `(table, func)` (every implemented call is
/// named, and only those).
pub fn implemented(table: u32, func: u32) -> bool {
    name(table, func) != "?"
}

fn len(n: u32) -> u32 {
    n.min(MAX_LEN)
}

/// A0h/B0h/C0h function `func` with o32 arguments `a` (more on the guest
/// stack). Errors name a function the BIOS doesn't implement.
pub fn call<S: Sdk>(m: &mut Machine<S>, table: u32, func: u32, a: [u32; 4]) -> Result<Flow<S>, String> {
    let label = format!("bios.{:02X}h:{:02X}h {}", table, func, name(table, func));
    m.count(&label);
    let r = |v: u32| Ok(Flow::ret(v));
    let mem = &mut *m.mem;
    match (table, func) {
        // --- A0h: C library ---------------------------------------------
        (0xA0, 0x13) => {
            // setjmp: a translated caller keeps its registers in wasm
            // locals, so only the first return (0) is representable;
            // longjmp traps.
            r(0)
        }
        (0xA0, 0x14) => Err(format!("{label}: longjmp across translated frames is not supported")),
        (0xA0, 0x15) => {
            let dst = a[0];
            let d = mem.cstr(dst, MAX_LEN as usize);
            let s = mem.cstr(a[1], MAX_LEN as usize);
            let mut out = s;
            out.push(0);
            mem.write(dst + d.len() as u32, &out);
            r(dst)
        }
        (0xA0, 0x16) => {
            let dst = a[0];
            let d = mem.cstr(dst, MAX_LEN as usize);
            let mut s = mem.cstr(a[1], len(a[2]) as usize);
            s.push(0);
            mem.write(dst + d.len() as u32, &s);
            r(dst)
        }
        (0xA0, 0x17) | (0xA0, 0x18) => {
            let n = if func == 0x18 { len(a[2]) } else { MAX_LEN };
            let (mut i, mut d) = (0u32, 0i32);
            while i < n {
                let (x, y) = (mem.r8(a[0] + i), mem.r8(a[1] + i));
                if x != y {
                    d = x as i32 - y as i32;
                    break;
                }
                if x == 0 {
                    break;
                }
                i += 1;
            }
            r(d as u32)
        }
        (0xA0, 0x19) => {
            let mut s = mem.cstr(a[1], MAX_LEN as usize);
            s.push(0);
            mem.write(a[0], &s);
            r(a[0])
        }
        (0xA0, 0x1A) => {
            // strncpy pads with zeros up to n.
            let n = len(a[2]) as usize;
            let mut s = mem.cstr(a[1], n);
            s.resize(n, 0);
            mem.write(a[0], &s);
            r(a[0])
        }
        (0xA0, 0x1B) => r(mem.cstr(a[0], MAX_LEN as usize).len() as u32),
        (0xA0, 0x1C) | (0xA0, 0x1D) => {
            let s = mem.cstr(a[0], MAX_LEN as usize);
            let c = a[1] as u8;
            let hit = if func == 0x1C { s.iter().position(|&b| b == c) } else { s.iter().rposition(|&b| b == c) };
            let hit = hit.or((c == 0).then_some(s.len()));
            r(hit.map_or(0, |i| a[0] + i as u32))
        }
        (0xA0, 0x25) => r((a[0] as u8).to_ascii_uppercase() as u32),
        (0xA0, 0x26) => r((a[0] as u8).to_ascii_lowercase() as u32),
        (0xA0, 0x27) => {
            // bcopy(src, dst, len)
            let b = mem.bytes(a[0], len(a[2]) as usize);
            mem.write(a[1], &b);
            r(a[1])
        }
        (0xA0, 0x28) => {
            mem.write(a[0], &vec![0; len(a[1]) as usize]);
            r(a[0])
        }
        (0xA0, 0x29) | (0xA0, 0x2D) => {
            let n = len(a[2]);
            let mut d = 0i32;
            for i in 0..n {
                let (x, y) = (mem.r8(a[0] + i), mem.r8(a[1] + i));
                if x != y {
                    d = x as i32 - y as i32;
                    break;
                }
            }
            r(d as u32)
        }
        (0xA0, 0x2A) | (0xA0, 0x2C) => {
            let b = mem.bytes(a[1], len(a[2]) as usize);
            mem.write(a[0], &b);
            r(a[0])
        }
        (0xA0, 0x2B) => {
            mem.write(a[0], &vec![a[1] as u8; len(a[2]) as usize]);
            r(a[0])
        }
        (0xA0, 0x2E) => {
            let n = len(a[2]);
            let hit = (0..n).find(|&i| mem.r8(a[0] + i) == a[1] as u8);
            r(hit.map_or(0, |i| a[0] + i))
        }
        (0xA0, 0x39) => {
            m.kernel.heap = (a[0], a[1]);
            r(0)
        }
        (0xA0, 0x3F) => {
            let text = printf(m, a);
            m.host.log(2, text.trim_end_matches('\n'));
            r(text.len() as u32)
        }
        (0xA0, 0x44) | (0xA0, 0x70) | (0xA0, 0x72) | (0xA0, 0x9F) => r(0),
        (0xA0, 0x49) => {
            // GPU_cw: a GP0 word (after gpu_sync, which returns 0 here:
            // the GPU front end takes words at once).
            m.gp0(a[0]);
            r(0)
        }
        (0xA0, 0xA1) => Err(format!("{label}: SystemError({:#x}, {:#x})", a[0], a[1])),
        (0xA0, 0xAB) | (0xA0, 0xAC) => {
            // Memory card presence: no card (M7). The kernel signals the
            // result as a card event.
            m.deliver_event(0xF400_0001, 0x0100);
            r(1)
        }

        // --- B0h: events, pads, files, cards -----------------------------
        (0xB0, 0x07) => {
            m.deliver_event(a[0], a[1]);
            r(1)
        }
        (0xB0, 0x08) => r(m.kernel.open_event(a[0], a[1], a[2], a[3])),
        (0xB0, 0x09) => {
            if let Some(e) = m.kernel.ev(a[0]) {
                e.status = kernel::EV_UNUSED;
            }
            r(1)
        }
        (0xB0, 0x0A) => {
            let h = a[0];
            match m.kernel.ev(h).map(|e| e.status) {
                Some(kernel::EV_READY) => {
                    m.kernel.ev(h).unwrap().status = kernel::EV_ENABLED;
                    r(1)
                }
                Some(kernel::EV_ENABLED) => Ok(Flow::Block {
                    until: Wait::Event(h),
                    then: Box::new(move |m: &mut Machine<S>| {
                        if let Some(e) = m.kernel.ev(h) {
                            e.status = kernel::EV_ENABLED;
                        }
                        Flow::ret(1)
                    }),
                }),
                _ => r(0),
            }
        }
        (0xB0, 0x0B) => {
            let ready = m.kernel.ev(a[0]).is_some_and(|e| e.status == kernel::EV_READY);
            if ready {
                m.kernel.ev(a[0]).unwrap().status = kernel::EV_ENABLED;
            }
            r(ready as u32)
        }
        (0xB0, 0x0C) => {
            if let Some(e) = m.kernel.ev(a[0]) {
                if e.status != kernel::EV_READY {
                    e.status = kernel::EV_ENABLED;
                }
            }
            r(1)
        }
        (0xB0, 0x0D) => {
            if let Some(e) = m.kernel.ev(a[0]) {
                e.status = kernel::EV_DISABLED;
            }
            r(1)
        }
        (0xB0, 0x12) => {
            m.kernel.pad_bufs = Some([(a[0], a[1]), (a[2], a[3])]);
            r(1)
        }
        (0xB0, 0x13) => {
            m.kernel.pad_started = true;
            r(1)
        }
        (0xB0, 0x14) => {
            m.kernel.pad_started = false;
            r(1)
        }
        (0xB0, 0x15) => {
            let bufs = [(a[0], 0x22), (a[1], 0x22)];
            m.kernel.pad_bufs = Some(bufs);
            m.kernel.pad_started = true;
            r(2)
        }
        (0xB0, 0x17) => Err(format!("{label}: called outside an exception handler")),
        (0xB0, 0x18) => r(0),
        (0xB0, 0x19) => {
            m.kernel.custom_exit = a[0];
            r(0)
        }
        (0xB0, 0x32) => {
            let path = String::from_utf8_lossy(&mem.cstr(a[0], 128)).into_owned();
            if !path.to_ascii_lowercase().starts_with("cdrom:") {
                // Memory card files (bu00:, bu10:) arrive with M7.
                return r(u32::MAX);
            }
            let mut disc = HostDisc(&mut *m.host);
            match cd::find(&mut disc, &path) {
                Some(e) if !e.dir => {
                    let fd = (0..16).find(|f| !m.kernel.files.contains_key(f)).unwrap_or(16);
                    m.kernel.files.insert(fd, File { lba: e.lba, size: e.size, pos: 0 });
                    r(fd)
                }
                _ => r(u32::MAX),
            }
        }
        (0xB0, 0x33) => {
            let Some(f) = m.kernel.files.get_mut(&a[0]) else { return r(u32::MAX) };
            f.pos = match a[2] {
                0 => a[1],
                1 => f.pos.wrapping_add(a[1]),
                _ => return r(u32::MAX),
            };
            r(f.pos)
        }
        (0xB0, 0x34) => {
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
            m.kernel.files.get_mut(&a[0]).unwrap().pos += n;
            r(n)
        }
        (0xB0, 0x35) => r(u32::MAX),
        (0xB0, 0x36) => {
            m.kernel.files.remove(&a[0]);
            r(a[0])
        }
        (0xB0, 0x3F) => {
            let s = String::from_utf8_lossy(&mem.cstr(a[0], 1024)).into_owned();
            m.host.log(2, &s);
            r(0)
        }
        (0xB0, 0x42) | (0xB0, 0x43) => r(0),
        (0xB0, 0x44) => r(0),
        (0xB0, 0x4A) | (0xB0, 0x4B) | (0xB0, 0x4C) | (0xB0, 0x50) => r(1),
        (0xB0, 0x4E) | (0xB0, 0x4F) => r(0),
        // No BIOS ROM, so no ROM kanji font.
        (0xB0, 0x51) => r(u32::MAX),
        // Kernel tables: addresses in kernel RAM, as the Sony BIOS has them.
        (0xB0, 0x56) => r(0x0000_0674),
        (0xB0, 0x57) => r(0x0000_0874),
        (0xB0, 0x5B) => {
            m.kernel.clear_pad = a[0];
            r(0)
        }

        // --- C0h: interrupt plumbing -------------------------------------
        (0xC0, 0x02) => {
            m.kernel.int_handlers.entry(a[0]).or_default().push(a[1]);
            r(0)
        }
        (0xC0, 0x03) => {
            if let Some(v) = m.kernel.int_handlers.get_mut(&a[0]) {
                v.retain(|&h| h != a[1]);
            }
            r(0)
        }
        (0xC0, 0x0A) => {
            let i = (a[0] as usize).min(3);
            let old = m.kernel.clear_rcnt[i];
            m.kernel.clear_rcnt[i] = a[1];
            r(old)
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
        // ExitCriticalSection.
        2 => {
            m.crit = false;
            Ok(0)
        }
        // ChangeThreadSubFunction.
        3 => Ok(1),
        0 => Ok(0),
        n => Err(format!("syscall {n} not implemented")),
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
pub fn printf<S: Sdk>(m: &Machine<S>, a: [u32; 4]) -> String {
    let fmt = m.mem.cstr(a[0], 1024);
    let mut k = 1;
    let mut next = || {
        let v = m.arg(&a, k);
        k += 1;
        v
    };
    let mut out = String::new();
    let mut i = 0;
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
        let mut width = 0usize;
        while i < fmt.len() && fmt[i].is_ascii_digit() {
            width = width * 10 + (fmt[i] - b'0') as usize;
            i += 1;
        }
        let mut prec: Option<usize> = None;
        if i < fmt.len() && fmt[i] == b'.' {
            i += 1;
            let mut p = 0;
            while i < fmt.len() && fmt[i].is_ascii_digit() {
                p = p * 10 + (fmt[i] - b'0') as usize;
                i += 1;
            }
            prec = Some(p);
        }
        while i < fmt.len() && (fmt[i] == b'l' || fmt[i] == b'h') {
            i += 1;
        }
        let Some(&conv) = fmt.get(i) else { break };
        i += 1;
        let body = match conv {
            b'd' | b'i' => (next() as i32).to_string(),
            b'u' => next().to_string(),
            b'x' | b'p' => format!("{:x}", next()),
            b'X' => format!("{:X}", next()),
            b'o' => format!("{:o}", next()),
            b'c' => ((next() as u8) as char).to_string(),
            b's' => {
                let s = m.mem.cstr(next(), prec.unwrap_or(1024));
                String::from_utf8_lossy(&s).into_owned()
            }
            b'%' => "%".into(),
            other => format!("%{}", other as char),
        };
        let pad = width.saturating_sub(body.len());
        if flags.contains('-') {
            out.push_str(&body);
            out.push_str(&" ".repeat(pad));
        } else if flags.contains('0') && conv != b's' {
            out.push_str(&"0".repeat(pad));
            out.push_str(&body);
        } else {
            out.push_str(&" ".repeat(pad));
            out.push_str(&body);
        }
    }
    out
}
