//! The file functions over `AddDrv`'s device table (psx-spx "BIOS File
//! Functions", "BIOS Control Blocks", "BIOS TTY Console"; OpenBIOS
//! `fileio/`): the file control blocks (FCBs) and device control blocks
//! (DCBs) live in kernel RAM through the table of tables at 140h and 150h,
//! and every file function reaches its device through the DCB's function
//! pointers. The built-in devices' pointers are stand-ins for the A0h device
//! functions psx-spx names (`cdrom`: A(5Fh..64h), `bu`: A(65h..6Eh),
//! `tty`: A(5Bh..5Eh)); a guest driver's (`AddDrv`), or a pointer a game
//! patched, is called as guest code.
//!
//! The built-in `cdrom` functions use the native disc (`HostDisc` and
//! `psx_devices::cd`); `bu` reuses [`crate::card`]'s native card
//! operations. The console is the host's log, a line at a time. There is
//! no console input yet: a blocking tty read is reported as pending.

use crate::bios::{HostDisc, MAX_LEN};
use crate::machine::{seq, Flow, GuestCall, Machine, Sdk};
use crate::tables;
use psx_devices::cd;
use psxw_abi::kernel as k;

/// FCB fields (psx-spx).
pub mod fcb {
    pub const FLAGS: u32 = 0x00;
    pub const DEVICE_ID: u32 = 0x04;
    pub const BUFFER: u32 = 0x08;
    pub const COUNT: u32 = 0x0C;
    pub const OFFSET: u32 = 0x10;
    pub const DEVICE_FLAGS: u32 = 0x14;
    pub const ERRNO: u32 = 0x18;
    pub const DCB: u32 = 0x1C;
    pub const LENGTH: u32 = 0x20;
    pub const LBA: u32 = 0x24;
    pub const FD: u32 = 0x28;
}

/// DCB fields (psx-spx): the names, the flags and block size, then the
/// device's functions.
pub mod dcb {
    pub const NAME: u32 = 0x00;
    pub const FLAGS: u32 = 0x04;
    pub const BLOCK: u32 = 0x08;
    pub const DESC: u32 = 0x0C;
    pub const INIT: u32 = 0x10;
    pub const OPEN: u32 = 0x14;
    pub const IN_OUT: u32 = 0x18;
    pub const CLOSE: u32 = 0x1C;
    pub const IOCTL: u32 = 0x20;
    pub const READ: u32 = 0x24;
    pub const WRITE: u32 = 0x28;
    pub const ERASE: u32 = 0x2C;
    pub const UNDELETE: u32 = 0x30;
    pub const FIRSTFILE: u32 = 0x34;
    pub const NEXTFILE: u32 = 0x38;
    pub const FORMAT: u32 = 0x3C;
    pub const CHDIR: u32 = 0x40;
    pub const RENAME: u32 = 0x44;
    pub const REMOVE: u32 = 0x48;
    pub const CHECK: u32 = 0x4C;
}

/// Device flags: a character device, a console, a block device, a file
/// system (whose own read and write the BIOS calls).
pub const CHAR: u32 = 0x01;
pub const CONS: u32 = 0x02;
pub const BLOCK: u32 = 0x04;
pub const FS: u32 = 0x10;

/// Open modes (psx-spx `open`).
pub const READ: u32 = 0x0001;
pub const WRITE: u32 = 0x0002;
pub const NBLOCK: u32 = 0x0004;
pub const CREATE: u32 = 0x0200;
pub const ASYNC: u32 = 0x8000;

/// File error numbers (`_get_errno`, psx-spx).
pub const E_NOT_FOUND: u32 = 0x02;
pub const E_BAD_PORT: u32 = 0x06;
pub const E_BAD_FD: u32 = 0x09;
pub const E_GENERAL: u32 = 0x10;
pub const E_EXISTS: u32 = 0x11;
pub const E_CROSS_DEVICE: u32 = 0x12;
pub const E_NO_DEVICE: u32 = 0x13;
pub const E_ALIGN: u32 = 0x16;
pub const E_NO_FDS: u32 = 0x18;
pub const E_FULL: u32 = 0x1C;

/// `ioctl`'s non-blocking command (OpenBIOS `PSXFIOCNBLOCK`).
const FIOCNBLOCK: u32 = 0x6601;
/// OpenBIOS `PSXF_SCAN2` and `PSXFIOCSCAN` (not the older 0x100 scan bit).
const SCAN: u32 = 0x1000;
const FIOCSCAN: u32 = 0x6602;

/// The devices' names in kernel RAM, after the DCBs.
const NAMES: u32 = k::DCBS + k::DEVICES * k::DCB_SIZE;
const NAME_LIST: [&[u8]; 6] = [b"cdrom", b"CD-ROM", b"bu", b"MEMORY CARD", b"tty", b"CONSOLE"];

/// The kernel's file state outside the control blocks.
#[derive(Debug, Clone, Default)]
pub struct Files {
    /// The last error (`_get_errno`).
    pub errno: u32,
    /// `firstfile`'s search FCB: taken free on the first call and kept,
    /// though never marked used (psx-spx's bug).
    pub first: u32,
    /// The console's column, for TAB (`putchar`).
    pub column: u32,
    /// What the console has been sent since its last line.
    pub line: Vec<u8>,
    /// The `cdrom` device's current directory, upper-cased without its
    /// leading backslash.
    pub cd_cwd: String,
    /// `firstfile`/`nextfile` on `cdrom`: (directory, pattern, next entry).
    pub cd_search: Option<(String, String, usize)>,
}

/// FCB `fd`'s address (KSEG1, as the table of tables points).
pub fn fcb_at(fd: u32) -> u32 {
    0xA000_0000 | (k::FCBS + fd * k::FCB_SIZE)
}

fn name_at(i: usize) -> u32 {
    let off: usize = NAME_LIST[..i].iter().map(|n| (n.len() + 4) & !3).sum();
    0xA000_0000 | (NAMES + off as u32)
}

/// A built-in device: its names, flags, block size and functions (A0h
/// numbers; the rest the A(57h) "return 0").
struct Builtin {
    name: usize,
    flags: u32,
    block: u32,
    funcs: &'static [(u32, u32)],
}

const DUMMY: u32 = 0x57;

const CDROM: Builtin = Builtin {
    name: 0,
    flags: FS | BLOCK,
    block: 0x800,
    funcs: &[
        (dcb::OPEN, 0x5F),
        (dcb::READ, 0x60),
        (dcb::CLOSE, 0x61),
        (dcb::FIRSTFILE, 0x62),
        (dcb::NEXTFILE, 0x63),
        (dcb::CHDIR, 0x64),
        (dcb::REMOVE, 0x56),
    ],
};

const CARD: Builtin = Builtin {
    name: 2,
    flags: FS | BLOCK,
    block: 0x80,
    funcs: &[
        (dcb::OPEN, 0x65),
        (dcb::READ, 0x66),
        (dcb::WRITE, 0x67),
        (dcb::CLOSE, 0x68),
        (dcb::FIRSTFILE, 0x69),
        (dcb::NEXTFILE, 0x6A),
        (dcb::ERASE, 0x6B),
        (dcb::UNDELETE, 0x6C),
        (dcb::FORMAT, 0x6D),
        (dcb::RENAME, 0x6E),
    ],
};

/// The DUART console (`AddDuartTtyDevice`) and the dummy one
/// (`add_nullcon_driver`, which the retail boot installs); both write to
/// the host's log here.
const DUART: Builtin =
    Builtin { name: 4, flags: CHAR | CONS, block: 1, funcs: &[(dcb::INIT, 0x5B), (dcb::OPEN, 0x5C), (dcb::IN_OUT, 0x5D), (dcb::IOCTL, 0x5E)] };
const NULLCON: Builtin = Builtin { name: 4, flags: CHAR, block: 1, funcs: &[(dcb::IN_OUT, 0x5D)] };

/// A file name split at its device (psx-spx "Device Names"): the name up to
/// `:`, less its trailing hex digits, which are the port; where the rest
/// starts.
pub fn split(path: &[u8]) -> (Vec<u8>, u32, u32) {
    let lead = path.iter().take_while(|&&c| c == b' ').count();
    let p = &path[lead..];
    let Some(colon) = p.iter().position(|&c| c == b':') else { return (p.to_vec(), 0, (lead + p.len()) as u32) };
    let dev = &p[..colon];
    let digits = dev.iter().rev().take_while(|c| c.is_ascii_hexdigit()).count();
    let (name, port) = dev.split_at(dev.len() - digits);
    let port = port.iter().fold(0u32, |v, &c| v.wrapping_shl(4) | (c as char).to_digit(16).unwrap_or(0));
    (name.to_vec(), port, (lead + colon + 1) as u32)
}

impl<S: Sdk> Machine<S> {
    /// C(12h) `InstallDevices(ttyflag)` without its console's reopening:
    /// the FCBs and DCBs through the table of tables, all free; the names;
    /// the console, `cdrom` and `bu`; `fd` 0 and 1 the console's, as the
    /// boot leaves them.
    pub fn lay_out_files(&mut self) {
        self.mem.w32(k::FCB_ENTRY, fcb_at(0));
        self.mem.w32(k::FCB_ENTRY + 4, k::FILES * k::FCB_SIZE);
        self.mem.w32(k::DCB_ENTRY, 0xA000_0000 | k::DCBS);
        self.mem.w32(k::DCB_ENTRY + 4, k::DEVICES * k::DCB_SIZE);
        self.mem.write(k::FCBS, &vec![0; ((k::FILES * k::FCB_SIZE) + k::DEVICES * k::DCB_SIZE) as usize]);
        for fd in 0..k::FILES {
            self.mem.w32(fcb_at(fd) + fcb::FD, fd);
        }
        for (i, n) in NAME_LIST.iter().enumerate() {
            let mut b = n.to_vec();
            b.push(0);
            self.mem.write(name_at(i), &b);
        }
        self.kernel.files = Files::default();
        for d in [NULLCON, CDROM, CARD] {
            self.add_builtin(&d);
        }
        // stdin and stdout (OpenBIOS `reopenStdio`; the dummy console's
        // open does nothing).
        let tty = self.find_device(b"tty").expect("the console was added");
        for (fd, mode) in [(0, READ), (1, WRITE)] {
            let f = fcb_at(fd);
            for (o, v) in [(fcb::FLAGS, mode), (fcb::DCB, tty), (fcb::DEVICE_FLAGS, NULLCON.flags)] {
                self.mem.w32(f + o, v);
            }
        }
    }

    /// A built-in device into the first free DCB (its `init` does nothing
    /// for these); whether there was one.
    fn add_builtin(&mut self, d: &Builtin) -> bool {
        let Some(at) = self.free_dcb() else { return false };
        let mut w = [tables::stand_in(0xA0, DUMMY); 20];
        w[0] = name_at(d.name);
        w[1] = d.flags;
        w[2] = d.block;
        w[3] = name_at(d.name + 1);
        for &(o, f) in d.funcs {
            w[(o / 4) as usize] = tables::stand_in(0xA0, f);
        }
        for (i, v) in w.into_iter().enumerate() {
            self.mem.w32(at + 4 * i as u32, v);
        }
        true
    }

    fn free_dcb(&self) -> Option<u32> {
        (0..k::DEVICES).map(|i| 0xA000_0000 | (k::DCBS + i * k::DCB_SIZE)).find(|&d| self.mem.r32(d + dcb::NAME) == 0)
    }

    /// The DCB named `name` (case-sensitive).
    pub fn find_device(&self, name: &[u8]) -> Option<u32> {
        (0..k::DEVICES)
            .map(|i| 0xA000_0000 | (k::DCBS + i * k::DCB_SIZE))
            .find(|&d| self.mem.r32(d + dcb::NAME) != 0 && self.mem.cstr(self.mem.r32(d + dcb::NAME), 32) == name)
    }

    /// The first free FCB (flags 0).
    fn free_fcb(&self) -> Option<u32> {
        (0..k::FILES).map(fcb_at).find(|&f| self.mem.r32(f + fcb::FLAGS) == 0)
    }

    /// FCB `fd`, if `fd` names one in use.
    fn open_fcb(&self, fd: u32) -> Option<u32> {
        (fd < k::FILES).then(|| fcb_at(fd)).filter(|&f| self.mem.r32(f + fcb::FLAGS) != 0)
    }

    fn fail(&mut self, errno: u32, v: u32) -> Flow<S> {
        self.kernel.files.errno = errno;
        Flow::ret(v)
    }

    /// The device `path` names, its port and where the rest of the name
    /// starts; none for an unknown device, whose console message is the
    /// flow the caller must run before failing.
    fn device_of(&mut self, path: u32) -> Result<(Option<(u32, u32, u32)>, Option<Flow<S>>), String> {
        let p = self.mem.cstr(path, 128);
        let (name, port, rest) = split(&p);
        match self.find_device(&name) {
            Some(d) => Ok((Some((d, port, path + rest)), None)),
            None => {
                let text = format!("{} is not known device\n", String::from_utf8_lossy(&name));
                Ok((None, Some(self.console(text.as_bytes())?)))
            }
        }
    }

    /// Device function `off` of `dcb` with `args`, then `then` with its v0:
    /// a stand-in runs here, a guest driver's function is called.
    fn device(&mut self, dcb: u32, off: u32, args: [u32; 4], then: impl FnOnce(&mut Self, u32) -> Flow<S> + 'static) -> Result<Flow<S>, String> {
        let f = self.mem.r32(dcb + off);
        let flow = match tables::stand_in_fn(f) {
            Some((t, n)) => crate::bios::call(self, t, n, args)?,
            None => Flow::Call { call: GuestCall { addr: f, args }, then: Box::new(|_, v| Flow::Return(v)) },
        };
        Ok(seq(self, flow, move |m, v| then(m, v as u32)))
    }

    /// A(00h)/B(32h) `open(path, mode)`: a free FCB, then the device
    /// (OpenBIOS `psxopen`); `fd`, or -1.
    pub fn open(&mut self, path: u32, mode: u32) -> Result<Flow<S>, String> {
        let Some(f) = self.free_fcb() else { return Ok(self.fail(E_NO_FDS, u32::MAX)) };
        let (found, message) = self.device_of(path)?;
        let Some((d, port, rest)) = found else {
            return Ok(match message {
                Some(flow) => seq(self, flow, |m, _| {
                    m.kernel.files.errno = E_NO_DEVICE;
                    Flow::ret(u32::MAX)
                }),
                None => self.fail(E_NO_DEVICE, u32::MAX),
            });
        };
        let flags = self.mem.r32(d + dcb::FLAGS);
        for (o, v) in [(fcb::FLAGS, mode), (fcb::DEVICE_ID, port), (fcb::DCB, d), (fcb::DEVICE_FLAGS, flags)] {
            self.mem.w32(f + o, v);
        }
        self.device(d, dcb::OPEN, [f, rest, mode, 0], move |m, v| {
            if v != 0 {
                m.kernel.files.errno = m.mem.r32(f + fcb::ERRNO);
                m.mem.w32(f + fcb::FLAGS, 0);
                return Flow::ret(u32::MAX);
            }
            m.mem.w32(f + fcb::OFFSET, 0);
            Flow::ret(m.mem.r32(f + fcb::FD))
        })
    }

    /// A(01h)/B(33h) `lseek(fd, offset, seektype)`: from the start or the
    /// position; psx-spx's bug: from the end leaves it.
    pub fn lseek(&mut self, fd: u32, off: u32, whence: u32) -> u32 {
        let Some(f) = self.open_fcb(fd) else {
            self.kernel.files.errno = E_BAD_FD;
            return u32::MAX;
        };
        let pos = self.mem.r32(f + fcb::OFFSET);
        let pos = match whence {
            0 => off,
            1 => pos.wrapping_add(off),
            2 => pos,
            _ => {
                self.mem.w32(f + fcb::ERRNO, E_ALIGN);
                self.kernel.files.errno = E_ALIGN;
                return u32::MAX;
            }
        };
        self.mem.w32(f + fcb::OFFSET, pos);
        pos
    }

    /// A(02h)/B(34h) `read` and A(03h)/B(35h) `write`: a file system's own
    /// function, or the device's in_out on the FCB's transfer fields
    /// (counted in blocks on a block device).
    pub fn transfer(&mut self, fd: u32, buf: u32, len: u32, write: bool) -> Result<Flow<S>, String> {
        let Some(f) = self.open_fcb(fd) else { return Ok(self.fail(E_BAD_FD, u32::MAX)) };
        // OpenBIOS psxread/psxwrite validate the fd, then scan before
        // reading its device/transfer fields. A scan callback may edit them.
        let flow = self.cdevscan()?;
        match flow {
            Flow::Return(_) => self.transfer_after_scan(f, buf, len, write),
            flow => Ok(seq(self, flow, move |m, _| m.transfer_after_scan(f, buf, len, write).unwrap_or_else(|e| panic!("read/write after _cdevscan: {e}")))),
        }
    }

    fn transfer_after_scan(&mut self, f: u32, buf: u32, len: u32, write: bool) -> Result<Flow<S>, String> {
        let d = self.mem.r32(f + fcb::DCB);
        let after = move |m: &mut Self, v: u32| {
            if (v as i32) < 0 {
                m.kernel.files.errno = m.mem.r32(f + fcb::ERRNO);
            }
            Flow::ret(v)
        };
        if self.mem.r32(f + fcb::DEVICE_FLAGS) & FS != 0 {
            return self.device(d, if write { dcb::WRITE } else { dcb::READ }, [f, buf, len, 0], after);
        }
        self.mem.w32(f + fcb::BUFFER, buf);
        self.mem.w32(f + fcb::COUNT, len);
        if self.mem.r32(d + dcb::FLAGS) & BLOCK != 0 {
            let block = self.mem.r32(d + dcb::BLOCK).max(1);
            if self.mem.r32(f + fcb::OFFSET) % block != 0 {
                let flow = self.console(b"offset not on block boundary\n")?;
                return Ok(seq(self, flow, |_, _| Flow::ret(u32::MAX)));
            }
            self.mem.w32(f + fcb::COUNT, len / block);
        }
        self.device(d, dcb::IN_OUT, [f, if write { 2 } else { 1 }, 0, 0], move |m, v| {
            if (v as i32) > 0 {
                let pos = m.mem.r32(f + fcb::OFFSET);
                m.mem.w32(f + fcb::OFFSET, pos.wrapping_add(v));
            }
            after(m, v)
        })
    }

    /// A(04h)/B(36h) `close(fd)`: `fd`, or -1.
    pub fn close(&mut self, fd: u32) -> Result<Flow<S>, String> {
        let Some(f) = self.open_fcb(fd) else { return Ok(self.fail(E_BAD_FD, u32::MAX)) };
        let d = self.mem.r32(f + fcb::DCB);
        self.device(d, dcb::CLOSE, [f, 0, 0, 0], move |m, v| {
            // psxclose clears the flags before checking the device result;
            // the native card state is invalidated for the released fd.
            m.mem.w32(f + fcb::FLAGS, 0);
            m.card_file_closed(fd);
            if v != 0 {
                m.kernel.files.errno = m.mem.r32(f + fcb::ERRNO);
                return Flow::ret(u32::MAX);
            }
            Flow::ret(fd)
        })
    }

    /// A(05h)/B(37h) `ioctl(fd, cmd, arg)`: 1, or 0 when the device fails.
    pub fn ioctl(&mut self, fd: u32, cmd: u32, arg: u32) -> Result<Flow<S>, String> {
        let Some(f) = self.open_fcb(fd) else { return Ok(self.fail(E_BAD_FD, u32::MAX)) };
        if cmd == FIOCNBLOCK {
            let flags = self.mem.r32(f + fcb::FLAGS);
            self.mem.w32(f + fcb::FLAGS, if arg == 0 { flags & !NBLOCK } else { flags | NBLOCK });
            return Ok(Flow::ret(1));
        }
        let d = self.mem.r32(f + fcb::DCB);
        self.device(d, dcb::IOCTL, [f, cmd, arg, 0], move |m, v| {
            if (v as i32) < 0 {
                m.kernel.files.errno = m.mem.r32(f + fcb::ERRNO);
                return Flow::ret(0);
            }
            Flow::ret(1)
        })
    }

    /// C(16h) `_cdevscan()`: scan flagged FCBs through their device's
    /// ioctl, in descriptor order (OpenBIOS fileio/misc.c). Flags are read
    /// live after each callback. This void service has no specified v0;
    /// returning zero here does not indicate available host input.
    pub fn cdevscan(&mut self) -> Result<Flow<S>, String> {
        self.cdevscan_from(0)
    }

    fn cdevscan_from(&mut self, first: u32) -> Result<Flow<S>, String> {
        for fd in first..k::FILES {
            if self.mem.r32(fcb_at(fd) + fcb::FLAGS) & SCAN == 0 {
                continue;
            }
            match self.ioctl(fd, FIOCSCAN, 0)? {
                Flow::Return(_) => {}
                flow => {
                    return Ok(seq(self, flow, move |m, _| m.cdevscan_from(fd + 1).unwrap_or_else(|e| panic!("_cdevscan: {e}"))));
                }
            }
        }
        Ok(Flow::ret(0))
    }

    /// A(07h)/B(39h) `isatty(fd)`: bit 1 of the file's DCB flags.
    pub fn isatty(&self, fd: u32) -> u32 {
        let d = if fd < k::FILES { self.mem.r32(fcb_at(fd) + fcb::DCB) } else { 0 };
        if d == 0 { 0 } else { (self.mem.r32(d + dcb::FLAGS) >> 1) & 1 }
    }

    /// A(08h)/B(3Ah) `getc(fd)`: the byte sign-extended, or -1.
    pub fn getc(&mut self, fd: u32) -> Result<Flow<S>, String> {
        let tmp = self.scratch_byte();
        let flow = self.transfer(fd, tmp, 1, false)?;
        Ok(seq(self, flow, move |m, v| Flow::ret(if (v as i32) < 1 { u32::MAX } else { m.mem.r8(tmp) as i8 as i32 as u32 })))
    }

    /// A(09h)/B(3Bh) `putc(char, fd)`: what `write` returns.
    pub fn putc(&mut self, c: u32, fd: u32) -> Result<Flow<S>, String> {
        let tmp = self.scratch_byte();
        self.mem.w8(tmp, c as u8);
        self.transfer(fd, tmp, 1, true)
    }

    /// The kernel's one-byte buffer for `getc`, `putc` and the console.
    fn scratch_byte(&self) -> u32 {
        0xA000_0000 | (NAMES + 0x40)
    }

    /// A(3Bh)/B(3Ch) `getchar()`: `read(0, tmp, 1)`, ANDed with 7Fh.
    pub fn getchar(&mut self) -> Result<Flow<S>, String> {
        let tmp = self.scratch_byte();
        let flow = self.transfer(0, tmp, 1, false)?;
        Ok(seq(self, flow, move |m, _| Flow::ret(m.mem.r8(tmp) as u32 & 0x7F)))
    }

    /// B(3Eh) `gets(s)`: read a line through `getchar`, echoing each accepted
    /// character through `putchar` (OpenBIOS `psxgets`): backspace/7F erases
    /// the previous byte, TAB becomes a space, 16h quotes the next character
    /// verbatim, CR or LF ends the line, and the buffer holds at most 125
    /// bytes plus its terminator. Control characters and a full buffer ring
    /// the bell (7). The next read waits for any echo to finish.
    pub fn gets(&mut self, s: u32) -> Result<Flow<S>, String> {
        self.gets_loop(s, s, s.wrapping_add(125), false)
    }

    fn gets_loop(&mut self, s: u32, mut ptr: u32, end: u32, mut quoted: bool) -> Result<Flow<S>, String> {
        loop {
            let flow = self.getchar()?;
            let c = match flow {
                Flow::Return(v) => v as u8,
                flow => {
                    return Ok(seq(self, flow, move |m, v| {
                        m.gets_after(s, ptr, end, quoted, v as u8).unwrap_or_else(|e| panic!("gets: {e}"))
                    }))
                }
            };
            if !quoted && c == 0x16 {
                quoted = true;
                continue;
            }
            let was_quoted = quoted;
            quoted = false;
            if !was_quoted && (c == b'\n' || c == b'\r') {
                let echo = self.putchars(vec![b'\n'])?;
                return Ok(seq(self, echo, move |m, _| {
                    m.mem.w8(ptr, 0);
                    Flow::ret(s)
                }));
            }
            let (next, echo) = self.gets_echo(s, ptr, end, c, was_quoted)?;
            match echo {
                Some(flow) => return Ok(flow),
                None => ptr = next,
            }
        }
    }

    fn gets_after(&mut self, s: u32, ptr: u32, end: u32, quoted: bool, c: u8) -> Result<Flow<S>, String> {
        if !quoted && c == 0x16 {
            return self.gets_loop(s, ptr, end, true);
        }
        if !quoted && (c == b'\n' || c == b'\r') {
            let echo = self.putchars(vec![b'\n'])?;
            return Ok(seq(self, echo, move |m, _| {
                m.mem.w8(ptr, 0);
                Flow::ret(s)
            }));
        }
        let (next, echo) = self.gets_echo(s, ptr, end, c, quoted)?;
        match echo {
            Some(flow) => Ok(flow),
            None => self.gets_loop(s, next, end, false),
        }
    }

    /// Apply one input byte and echo it; the new insert position and the
    /// echo's flow when it did not return at once.
    fn gets_echo(&mut self, s: u32, ptr: u32, end: u32, c: u8, quoted: bool) -> Result<(u32, Option<Flow<S>>), String> {
        let bell = vec![7];
        let (next, echo) = if quoted {
            if ptr < end {
                self.mem.w8(ptr, c);
                (ptr + 1, vec![c])
            } else {
                (ptr, bell)
            }
        } else {
            match c {
                b'\x08' | b'\x7f' if ptr > s => (ptr - 1, vec![b'\x08', b' ', b'\x08']),
                b'\t' if ptr < end => {
                    self.mem.w8(ptr, b' ');
                    (ptr + 1, vec![b' '])
                }
                c if c < 0x20 || c == 0x7f || ptr >= end => (ptr, bell),
                c => {
                    self.mem.w8(ptr, c);
                    (ptr + 1, vec![c])
                }
            }
        };
        let flow = self.putchars(echo)?;
        Ok(match flow {
            Flow::Return(_) => (next, None),
            flow => (next, Some(seq(self, flow, move |m, _| m.gets_loop(s, next, end, false).unwrap_or_else(|e| panic!("gets: {e}"))))),
        })
    }

    /// A(3Ch)/B(3Dh) `putchar(char)`, and the characters of `puts` and
    /// `printf`: TAB to the next multiple of 8, LF as CR LF, each through
    /// `write(1, tmp, 1)`.
    pub fn putchars(&mut self, text: Vec<u8>) -> Result<Flow<S>, String> {
        let mut out = Vec::new();
        for c in text {
            match c {
                b'\t' => loop {
                    out.push(b' ');
                    self.kernel.files.column += 1;
                    if self.kernel.files.column % 8 == 0 {
                        break;
                    }
                },
                b'\n' => {
                    out.extend([b'\r', b'\n']);
                    self.kernel.files.column = 0;
                }
                c => {
                    out.push(c);
                    self.kernel.files.column += 1;
                }
            }
        }
        Ok(self.write_each(out, 0))
    }

    /// `out[i..]` a byte at a time to `fd` 1, without nesting while the
    /// writes return at once.
    fn write_each(&mut self, out: Vec<u8>, mut i: usize) -> Flow<S> {
        while i < out.len() {
            let tmp = self.scratch_byte();
            self.mem.w8(tmp, out[i]);
            i += 1;
            match self.transfer(1, tmp, 1, true) {
                Ok(Flow::Return(_)) => {}
                Ok(flow) => return seq(self, flow, move |m, _| m.write_each(out, i)),
                Err(e) => panic!("putchar: {e}"),
            }
        }
        Flow::ret(0)
    }

    /// What the BIOS reports on its console (OpenBIOS's messages), through
    /// the console device: a guest tty driver's writes are calls to run.
    fn console(&mut self, text: &[u8]) -> Result<Flow<S>, String> {
        self.putchars(text.to_vec())
    }

    /// The console's output: the host's log, a line at a time.
    pub fn tty_out(&mut self, bytes: &[u8]) {
        for &b in bytes {
            match b {
                b'\r' => {}
                b'\n' => {
                    let line = std::mem::take(&mut self.kernel.files.line);
                    self.host.log(2, &String::from_utf8_lossy(&line));
                }
                b => self.kernel.files.line.push(b),
            }
        }
    }

    /// A(5Dh) `dev_tty_in_out(fcb, cmd)`: writes the FCB's bytes to the
    /// console (opened for writing); there is no input yet, so a read
    /// returns nothing when it mustn't wait and reports the missing host
    /// console input otherwise.
    pub fn tty_in_out(&mut self, f: u32, cmd: u32) -> Result<u32, String> {
        let (buf, count) = (self.mem.r32(f + fcb::BUFFER), self.mem.r32(f + fcb::COUNT));
        match cmd {
            2 if self.mem.r32(f + fcb::FLAGS) & WRITE != 0 => {
                let bytes = self.mem.bytes(buf, count.min(MAX_LEN) as usize);
                self.tty_out(&bytes);
                self.mem.w32(f + fcb::BUFFER, buf.wrapping_add(count));
                self.mem.w32(f + fcb::COUNT, 0);
                Ok(count)
            }
            1 if self.mem.r32(f + fcb::FLAGS) & NBLOCK != 0 => Ok(0),
            1 => Err("pending: a read from the console waits for host console input".into()),
            _ => Err("tty: bad function (the BIOS's ioabort)".into()),
        }
    }

    /// A(5Ch) `dev_tty_open(fcb, name, mode)`: ports 0 and 1 (psx-spx: 06h
    /// for tty2 and up).
    pub fn tty_open(&mut self, f: u32) -> u32 {
        if self.mem.r32(f + fcb::DEVICE_ID) < 2 {
            return 0;
        }
        self.mem.w32(f + fcb::ERRNO, E_BAD_PORT);
        u32::MAX
    }

    /// A function on a name, with a free FCB borrowed for it (`erase`,
    /// `undelete`, `format`, `cd`, `testdevice`; OpenBIOS `psxerase`): the
    /// device's function `off` with (fcb, rest of the name), then
    /// `done(device's v0) -> (failed, v0)`.
    fn on_name(&mut self, path: u32, off: u32, fail: u32, done: fn(u32) -> (bool, u32)) -> Result<Flow<S>, String> {
        let Some(f) = self.free_fcb() else { return Ok(self.fail(E_NO_FDS, fail)) };
        let (found, message) = self.device_of(path)?;
        let Some((d, port, rest)) = found else {
            return Ok(match message {
                Some(flow) => seq(self, flow, move |m, _| {
                    m.kernel.files.errno = E_NO_DEVICE;
                    Flow::ret(fail)
                }),
                None => self.fail(E_NO_DEVICE, fail),
            });
        };
        self.mem.w32(f + fcb::DEVICE_ID, port);
        self.mem.w32(f + fcb::DCB, d);
        let args = if off == dcb::FORMAT { [f, 0, 0, 0] } else { [f, rest, 0, 0] };
        self.device(d, off, args, move |m, v| {
            m.mem.w32(f + fcb::FLAGS, 0);
            let (failed, v) = done(v);
            if failed {
                m.kernel.files.errno = m.mem.r32(f + fcb::ERRNO);
            }
            Flow::ret(v)
        })
    }

    /// B(45h) `erase`, B(46h) `undelete`, B(41h) `format`: 1, or 0 when the
    /// device fails (returns nonzero).
    pub fn erase(&mut self, path: u32, off: u32) -> Result<Flow<S>, String> {
        self.on_name(path, off, 0, |v| (v != 0, (v == 0) as u32))
    }

    /// B(40h) `cd(path)`: the device's `chdir`, 1 okay or 0 failed.
    pub fn cd(&mut self, path: u32) -> Result<Flow<S>, String> {
        self.on_name(path, dcb::CHDIR, 0, |v| (false, v))
    }

    /// B(59h) `testdevice(name)`: the device's own check.
    pub fn test_device(&mut self, path: u32) -> Result<Flow<S>, String> {
        self.on_name(path, dcb::CHECK, 0, |v| (false, v))
    }

    /// B(44h) `rename(old, new)`: on one device, with two FCBs; 1 or 0.
    pub fn rename(&mut self, old: u32, new: u32) -> Result<Flow<S>, String> {
        let Some(f1) = self.free_fcb() else { return Ok(self.fail(E_NO_FDS, 0)) };
        // The second is the first again: psx-spx's bug, rename "does
        // accidently use only 1 handle".
        let f2 = f1;
        let (old_path, old_message) = self.device_of(old)?;
        let Some((d1, p1, r1)) = old_path else {
            return Ok(match old_message {
                Some(flow) => seq(self, flow, |m, _| {
                    m.kernel.files.errno = E_NO_DEVICE;
                    Flow::ret(0)
                }),
                None => self.fail(E_NO_DEVICE, 0),
            });
        };
        let (new_path, new_message) = self.device_of(new)?;
        let Some((d2, p2, r2)) = new_path else {
            return Ok(match new_message {
                Some(flow) => seq(self, flow, |m, _| {
                    m.kernel.files.errno = E_NO_DEVICE;
                    Flow::ret(0)
                }),
                None => self.fail(E_NO_DEVICE, 0),
            });
        };
        if p1 != p2 || d1 != d2 {
            return Ok(self.fail(E_CROSS_DEVICE, 0));
        }
        self.mem.w32(f1 + fcb::DEVICE_ID, p1);
        self.mem.w32(f1 + fcb::DCB, d1);
        self.device(d1, dcb::RENAME, [f1, r1, f2, r2], move |m, v| {
            m.mem.w32(f1 + fcb::FLAGS, 0);
            if v != 0 {
                m.kernel.files.errno = m.mem.r32(f1 + fcb::ERRNO);
                return Flow::ret(0);
            }
            Flow::ret(1)
        })
    }

    /// B(42h) `firstfile2(path, direntry)`: the search FCB, then the
    /// device's `firstfile`; the entry, or 0. The error number is left
    /// alone (psx-spx).
    pub fn firstfile(&mut self, path: u32, dir: u32) -> Result<Flow<S>, String> {
        if self.kernel.files.first == 0 {
            let Some(f) = self.free_fcb() else { return Ok(Flow::ret(0)) };
            self.kernel.files.first = f;
        }
        let f = self.kernel.files.first;
        let (found, message) = self.device_of(path)?;
        let Some((d, port, rest)) = found else {
            return Ok(match message {
                Some(flow) => seq(self, flow, move |m, _| {
                    m.mem.w32(f + fcb::FLAGS, 0);
                    Flow::ret(0)
                }),
                None => {
                    self.mem.w32(f + fcb::FLAGS, 0);
                    Flow::ret(0)
                }
            });
        };
        self.mem.w32(f + fcb::DEVICE_ID, port);
        self.mem.w32(f + fcb::DCB, d);
        self.device(d, dcb::FIRSTFILE, [f, rest, dir, 0], |_, v| Flow::ret(v))
    }

    /// B(43h) `nextfile(direntry)`: the search FCB's device's `nextfile`.
    pub fn nextfile(&mut self, dir: u32) -> Result<Flow<S>, String> {
        let f = self.kernel.files.first;
        let d = if f == 0 { 0 } else { self.mem.r32(f + fcb::DCB) };
        if d == 0 {
            return Ok(Flow::ret(0));
        }
        self.device(d, dcb::NEXTFILE, [f, dir, 0, 0], |_, v| Flow::ret(v))
    }

    fn bu_path(&self, f: u32, name: u32) -> String {
        let port = self.mem.r32(f + fcb::DEVICE_ID);
        let n = String::from_utf8_lossy(&self.mem.cstr(name, 64)).into_owned();
        format!("bu{port:02X}:{n}")
    }

    fn bu_device_path(&self, f: u32) -> String {
        format!("bu{:02X}:", self.mem.r32(f + fcb::DEVICE_ID))
    }

    /// A(65h) `dev_bu_open(fcb, name, mode)`: the native card's file
    /// operations, over the FCB's port and name; 0, or -1.
    pub fn dev_bu_open(&mut self, f: u32, name: u32, mode: u32) -> u32 {
        let fd = self.mem.r32(f + fcb::FD);
        let path = self.bu_path(f, name);
        let v = self.card_open(&path, mode, fd);
        let Some(file) = self.kernel.cards.files.get(&fd) else {
            self.mem.w32(f + fcb::ERRNO, self.kernel.cards.errno);
            return u32::MAX;
        };
        if v != fd {
            self.mem.w32(f + fcb::ERRNO, self.kernel.cards.errno);
            return u32::MAX;
        }
        let (size, first) = (file.size, {
            let slot = file.slot;
            let name = file.name.clone();
            self.kernel.cards.slots[slot].as_ref().and_then(|c| c.find(&name)).and_then(|i| i.blocks.first().map(|&b| b as u32)).unwrap_or(0)
        });
        for (o, v) in [(fcb::LBA, first), (fcb::LENGTH, size), (fcb::OFFSET, 0), (fcb::ERRNO, 0)] {
            self.mem.w32(f + o, v);
        }
        0
    }

    /// A(66h) `dev_bu_read`, A(67h) `dev_bu_write` (`to_card`): whole
    /// frames from a frame boundary inside the file.
    pub fn dev_bu_io(&mut self, f: u32, buf: u32, len: u32, to_card: bool) -> u32 {
        let fd = self.mem.r32(f + fcb::FD);
        if let Some(file) = self.kernel.cards.files.get_mut(&fd) {
            file.pos = self.mem.r32(f + fcb::OFFSET);
        }
        let v = self.card_io(fd, buf, len, to_card);
        if let Some(file) = self.kernel.cards.files.get(&fd) {
            self.mem.w32(f + fcb::OFFSET, file.pos);
        }
        self.mem.w32(f + fcb::ERRNO, if v == u32::MAX { self.kernel.cards.errno } else { 0 });
        v
    }

    /// A(68h) `dev_bu_close(fcb)`: refuse an active asynchronous operation
    /// with device result 1. Generic close nevertheless releases the FCB;
    /// its native cleanup then cancels work belonging to the old open.
    pub fn dev_bu_close(&mut self, f: u32) -> u32 {
        let fd = self.mem.r32(f + fcb::FD);
        if self.card_file_pending(fd) {
            self.mem.w32(f + fcb::ERRNO, E_GENERAL);
            return 1;
        }
        self.kernel.cards.files.remove(&fd);
        0
    }

    /// A(69h) `dev_bu_firstfile(fcb, pattern, direntry)` and A(6Ah)
    /// `dev_bu_nextfile(fcb, direntry)` (`pattern` 0).
    pub fn dev_bu_find(&mut self, f: u32, pattern: u32, dir: u32) -> u32 {
        if pattern == 0 {
            return self.card_find(None, dir);
        }
        let path = self.bu_path(f, pattern);
        self.card_find(Some(&path), dir)
    }

    /// A(6Bh) `dev_bu_erase`, A(6Ch) `dev_bu_undelete` (fcb, name): 0, or
    /// -1.
    pub fn dev_bu_erase(&mut self, f: u32, name: u32, undelete: bool) -> u32 {
        let path = self.bu_path(f, name);
        if self.card_erase(&path, undelete) == 0 {
            self.mem.w32(f + fcb::ERRNO, self.kernel.cards.errno);
            return u32::MAX;
        }
        0
    }

    /// A(6Dh) `dev_bu_format(fcb)`: 0, or -1 without a card.
    pub fn dev_bu_format(&mut self, f: u32) -> u32 {
        let path = self.bu_device_path(f);
        if self.card_format(&path) == 0 {
            self.mem.w32(f + fcb::ERRNO, E_GENERAL);
            return u32::MAX;
        }
        self.mem.w32(f + fcb::ERRNO, 0);
        0
    }

    /// A(6Eh) `dev_bu_rename(fcb, name, name)`: 0, or -1.
    pub fn dev_bu_rename(&mut self, f: u32, from: u32, to: u32) -> u32 {
        let (a, b) = (self.bu_path(f, from), self.bu_path(f, to));
        if self.card_rename(&a, &b) == 0 {
            self.mem.w32(f + fcb::ERRNO, self.kernel.cards.errno);
            return u32::MAX;
        }
        self.mem.w32(f + fcb::ERRNO, 0);
        0
    }

    /// A(5Fh) `dev_cd_open(fcb, name, mode)`: the native disc's file, its
    /// first sector, size and offset in the FCB; 0, or -1. OpenBIOS
    /// `dev_cd_open` uppercases the filename and adds `;1` only if no
    /// version was supplied. `?`, but not `*`, matches in the file part.
    pub fn dev_cd_open(&mut self, f: u32, name: u32) -> u32 {
        let raw = String::from_utf8_lossy(&self.mem.cstr(name, 128)).into_owned();
        let (dir, mut pattern) = self.cd_search_path(&raw.to_ascii_uppercase());
        if !pattern.contains(';') {
            pattern.push_str(";1");
        }
        let entry = self.cd_entries(&dir).and_then(|entries| {
            entries.into_iter().find(|e| !e.dir && cd_matches(&pattern, &e.name, false))
        });
        match entry {
            Some(e) => {
                for (o, v) in [(fcb::LBA, e.lba), (fcb::LENGTH, e.size), (fcb::OFFSET, 0), (fcb::ERRNO, 0)] {
                    self.mem.w32(f + o, v);
                }
                0
            }
            _ => {
                self.mem.w32(f + fcb::ERRNO, E_NOT_FOUND);
                u32::MAX
            }
        }
    }

    /// A(60h) `dev_cd_read(fcb, dst, len)`: sector-aligned reads through
    /// the native disc; the count, or -1. OpenBIOS `dev_cd_read` transfers
    /// the full request before clipping the result/offset to the file size.
    /// Status/path-table checks and the zero-count command error remain partial.
    pub fn dev_cd_read(&mut self, f: u32, dst: u32, len: u32) -> u32 {
        let (off, size) = (self.mem.r32(f + fcb::OFFSET), self.mem.r32(f + fcb::LENGTH));
        if (len as i32) < 0 || (len | off) & (cd::USER as u32 - 1) != 0 || off >= size {
            self.mem.w32(f + fcb::ERRNO, E_ALIGN);
            return u32::MAX;
        }
        let lba = self.mem.r32(f + fcb::LBA).wrapping_add(off / cd::USER as u32);
        let mut raw = [0u8; cd::RAW];
        for pos in (0..len).step_by(cd::USER) {
            if !self.host.disc_read(lba.wrapping_add(pos / cd::USER as u32), &mut raw) {
                self.mem.w32(f + fcb::ERRNO, E_GENERAL);
                return u32::MAX;
            }
            // A later failure leaves completed sectors in RAM, but does not
            // advance the file offset. The full request may extend past EOF.
            let at = dst.wrapping_add(pos);
            self.mem.write(at, cd::user_data(&raw));
            self.loaded.push((at, cd::USER as u32));
        }
        let n = len.min(size - off);
        self.mem.w32(f + fcb::OFFSET, off + n);
        n
    }

    /// A(61h) `dev_cd_close(fcb)`.
    pub fn dev_cd_close(&mut self, _f: u32) -> u32 {
        0
    }

    /// A(A4h) `CdGetLbn(name)`: the CD file's first sector on the native
    /// disc, or -1.
    pub fn cd_lbn(&mut self, name: u32) -> u32 {
        let name = self.cd_resolve(&String::from_utf8_lossy(&self.mem.cstr(name, 128)));
        let mut disc = HostDisc(&mut *self.host);
        cd::find(&mut disc, &name).map_or(u32::MAX, |e| e.lba)
    }

    /// `rel` from the current directory, upper-cased and without a leading
    /// backslash.
    fn cd_join(&self, rel: &str) -> String {
        let rel = rel.trim_matches('\\');
        if rel.is_empty() {
            return self.kernel.files.cd_cwd.clone();
        }
        if self.kernel.files.cd_cwd.is_empty() {
            rel.to_string()
        } else {
            format!("{}\\{rel}", self.kernel.files.cd_cwd)
        }
    }

    /// A file path absolute from the disc root or relative to the current
    /// directory.
    fn cd_resolve(&self, raw: &str) -> String {
        let p = raw.replace('/', "\\");
        let absolute = p.starts_with('\\');
        let p = p.trim_start_matches('\\');
        (if absolute { p.to_string() } else { self.cd_join(p) }).to_ascii_uppercase()
    }

    /// A search pattern split into its directory and pattern, absolute from
    /// the root or relative to the current directory.
    fn cd_search_path(&self, raw: &str) -> (String, String) {
        let p = raw.replace('/', "\\");
        let absolute = p.starts_with('\\');
        let p = p.trim_start_matches('\\');
        match p.rfind('\\') {
            Some(i) => {
                let dir = if absolute { p[..i].to_string() } else { self.cd_join(&p[..i]) };
                (dir.to_ascii_uppercase(), p[i + 1..].to_string())
            }
            None => {
                let dir = if absolute { String::new() } else { self.kernel.files.cd_cwd.clone() };
                (dir, p.to_string())
            }
        }
    }

    /// The `cdrom` device's directory's entries, walking the native disc's
    /// root and subdirectories.
    fn cd_entries(&mut self, dir: &str) -> Option<Vec<cd::DirEntry>> {
        let mut disc = HostDisc(&mut *self.host);
        let mut here = cd::root(&mut disc)?;
        for part in dir.split('\\').filter(|p| !p.is_empty()) {
            let want = part.to_ascii_uppercase();
            here = cd::list(&mut disc, &here).into_iter().find(|e| e.dir && e.name.to_ascii_uppercase() == want)?;
        }
        Some(cd::list(&mut disc, &here))
    }

    /// A(62h) `dev_cd_firstfile(fcb, pattern, direntry)`: the pattern's
    /// directory listed on the native disc.
    pub fn dev_cd_firstfile(&mut self, _f: u32, name: u32, dir: u32) -> u32 {
        let raw = String::from_utf8_lossy(&self.mem.cstr(name, 128)).into_owned();
        let (where_, pattern) = self.cd_search_path(&raw);
        let pattern = match pattern.to_ascii_uppercase() {
            p if p.is_empty() => "*".to_string(),
            p => p,
        };
        self.kernel.files.cd_search = Some((where_.to_ascii_uppercase(), pattern, 0));
        self.cd_next_entry(dir)
    }

    /// A(63h) `dev_cd_nextfile(fcb, direntry)`: the next entry the search's
    /// pattern matches, into the direntry; the direntry, or 0.
    pub fn dev_cd_nextfile(&mut self, _f: u32, dir: u32) -> u32 {
        self.cd_next_entry(dir)
    }

    fn cd_next_entry(&mut self, dir: u32) -> u32 {
        let Some((where_, pattern, mut next)) = self.kernel.files.cd_search.clone() else { return 0 };
        let Some(entries) = self.cd_entries(&where_) else { return 0 };
        while let Some(e) = entries.get(next) {
            next += 1;
            if e.dir || !cd_matches(&pattern, &e.name, true) {
                continue;
            }
            let mut out = vec![0u8; 0x28];
            let n = e.name.as_bytes();
            out[..n.len().min(0x13)].copy_from_slice(&n[..n.len().min(0x13)]);
            out[0x14..0x18].copy_from_slice(&0u32.to_le_bytes());
            out[0x18..0x1C].copy_from_slice(&e.size.to_le_bytes());
            out[0x20..0x24].copy_from_slice(&e.lba.to_le_bytes());
            self.mem.write(dir, &out);
            self.kernel.files.cd_search = Some((where_, pattern, next));
            return dir;
        }
        self.kernel.files.cd_search = Some((where_, pattern, next));
        0
    }

    /// A(64h) `dev_cd_chdir(fcb, path)`: replace the current directory,
    /// upper-cased, even on failure. The directory need not exist: the
    /// result is the disc metadata read, not a directory lookup (psx-spx;
    /// OpenBIOS `dev_cd_chdir` / `cdromReadPathTable`). Native lookups read
    /// the disc afresh rather than retaining the BIOS's path-table cache.
    pub fn dev_cd_chdir(&mut self, path: u32) -> u32 {
        let raw = String::from_utf8_lossy(&self.mem.cstr(path, 128)).into_owned();
        self.kernel.files.cd_cwd = raw.replace('/', "\\").trim_matches('\\').to_ascii_uppercase();
        let mut raw = [0u8; cd::RAW];
        if !self.host.disc_read(16, &mut raw) {
            return 0;
        }
        let pvd = cd::user_data(&raw);
        if &pvd[1..6] != b"CD001" {
            return 0;
        }
        let table = u32::from_le_bytes(pvd[140..144].try_into().unwrap());
        self.host.disc_read(table, &mut raw) as u32
    }

    /// B(55h) `_get_error(fd)`: the FCB's error, or -1 for a handle not in
    /// use.
    pub fn get_error(&self, fd: u32) -> u32 {
        self.open_fcb(fd).map_or(u32::MAX, |f| self.mem.r32(f + fcb::ERRNO))
    }

    /// B(47h) `AddDrv(device_info)`: the DCB copied into the first free one,
    /// then its `init`; 1, or 0 with none free.
    pub fn add_drv(&mut self, info: u32) -> Result<Flow<S>, String> {
        let Some(d) = self.free_dcb() else { return Ok(Flow::ret(0)) };
        let b = self.mem.bytes(info, k::DCB_SIZE as usize);
        self.mem.write(d, &b);
        self.device(d, dcb::INIT, [0; 4], |_, _| Flow::ret(1))
    }

    /// A(96h..99h): a built-in device, as `AddDrv` adds it.
    pub fn add_device(&mut self, func: u32) -> u32 {
        let d = match func {
            0x96 => CDROM,
            0x97 => CARD,
            0x98 => DUART,
            _ => NULLCON,
        };
        self.add_builtin(&d) as u32
    }

    /// B(48h) `DelDrv(name)`: the device's `remove`, then its DCB free; 1,
    /// or 0 for an unknown device.
    pub fn del_drv(&mut self, name: u32) -> Result<Flow<S>, String> {
        let n = self.mem.cstr(name, 32);
        let Some(d) = self.find_device(&n) else { return Ok(Flow::ret(0)) };
        self.device(d, dcb::REMOVE, [0; 4], move |m, _| {
            m.mem.w32(d + dcb::NAME, 0);
            Flow::ret(1)
        })
    }

    /// B(49h) `PrintInstalledDevices()`.
    pub fn print_devices(&mut self) -> Result<Flow<S>, String> {
        let mut text = String::new();
        for i in 0..k::DEVICES {
            let d = 0xA000_0000 | (k::DCBS + i * k::DCB_SIZE);
            let name = self.mem.r32(d + dcb::NAME);
            if name != 0 {
                text.push_str(&format!(
                    "\t{}:\t{}\n",
                    String::from_utf8_lossy(&self.mem.cstr(name, 32)),
                    String::from_utf8_lossy(&self.mem.cstr(self.mem.r32(d + dcb::DESC), 32))
                ));
            }
        }
        self.console(text.as_bytes())
    }

    /// C(1Bh) `KernelRedirect(ttyflag)`: the console out, the dummy one (0)
    /// or the DUART's (1) in, `fd` 0 and 1 reopened on it; nothing for other
    /// values (psx-spx).
    pub fn kernel_redirect(&mut self, tty: u32) -> Result<Flow<S>, String> {
        if tty > 1 {
            return Ok(Flow::ret(0));
        }
        if let Some(d) = self.find_device(b"tty") {
            self.mem.w32(d + dcb::NAME, 0);
        }
        self.add_builtin(if tty == 0 { &NULLCON } else { &DUART });
        self.reopen_stdio()
    }

    /// C(13h) `FlushStdInOutPut()`: `fd` 0 and 1 closed and reopened on
    /// "tty00:" (OpenBIOS `reopenStdio`).
    pub fn reopen_stdio(&mut self) -> Result<Flow<S>, String> {
        let name = self.scratch_byte() + 4;
        self.mem.write(name, b"tty00:\0");
        let c0 = self.close(0)?;
        Ok(seq(self, c0, move |m, _| {
            let c1 = m.close(1).unwrap_or_else(|e| panic!("FlushStdInOutPut: {e}"));
            seq(m, c1, move |m, _| {
                let o = m.open(name, READ).unwrap_or_else(|e| panic!("FlushStdInOutPut: {e}"));
                seq(m, o, move |m, v| {
                    if v != 0 {
                        return Flow::ret(0);
                    }
                    let o = m.open(name, WRITE).unwrap_or_else(|e| panic!("FlushStdInOutPut: {e}"));
                    seq(m, o, |_, _| Flow::ret(0))
                })
            })
        }))
    }
}

/// BIOS CD matching, not a shell glob: `?` matching the name's terminator
/// accepts the rest too (psx-spx; OpenBIOS `patternMatches`). Directory
/// searches additionally allow `*` to discard the rest of the pattern;
/// open treats it literally. Both compare the ISO version suffix.
fn cd_matches(pattern: &str, name: &str, allow_star: bool) -> bool {
    let p = pattern.as_bytes();
    for (i, c) in name.bytes().enumerate() {
        match p.get(i) {
            Some(b'*') if allow_star => return true,
            Some(b'?') => {}
            Some(&cp) if cp == c.to_ascii_uppercase() => {}
            _ => return false,
        }
    }
    matches!(p.get(name.len()), None | Some(b'?'))
        || (allow_star && p.get(name.len()) == Some(&b'*'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{machine, NoSdk};

    fn ret(m: &mut Machine<NoSdk>, f: Result<Flow<NoSdk>, String>, returns: &dyn Fn(&mut Machine<NoSdk>, GuestCall) -> u32) -> u32 {
        match f.unwrap() {
            Flow::Return(v) => v as u32,
            Flow::Call { call, then } => {
                let v = returns(m, call);
                let next = then(m, v as u64);
                ret(m, Ok(next), returns)
            }
            Flow::Block { .. } => panic!("blocked"),
        }
    }

    #[test]
    fn names_split_at_the_port() {
        assert_eq!(split(b"bu10:FILE"), (b"bu".to_vec(), 0x10, 5));
        assert_eq!(split(b" cdrom:\\A;1"), (b"cdrom".to_vec(), 0, 7));
        assert_eq!(split(b"usb:x"), (b"us".to_vec(), 0xB, 4));
        assert_eq!(split(b"tty"), (b"tty".to_vec(), 0, 3));
    }

    /// A guest driver's functions are called through `Flow::Call` with the
    /// BIOS's arguments, their results reach the caller, and their DCB is
    /// freed by DelDrv. Its tty also receives a diagnostic's writes.
    #[test]
    fn guest_device_callbacks_carry_results() {
        let mut m = machine();
        let s = 0x8001_0000;
        let info = 0x8002_0000;
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let written = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        m.mem.write(s + 0x10, b"sim\0");
        m.mem.w32(info, s + 0x10);
        m.mem.w32(info + dcb::FLAGS, FS);
        for o in (dcb::INIT..=dcb::CHECK).step_by(4) {
            m.mem.w32(info + o, 0x8003_0000 + o);
        }
        let (seen, out) = (calls.clone(), written.clone());
        let returns = move |m: &mut Machine<NoSdk>, call: GuestCall| {
            seen.borrow_mut().push((call.addr, call.args));
            if call.addr == 0x8003_0000 + dcb::READ {
                8
            } else if call.addr == 0x8003_0000 + dcb::IN_OUT {
                let f = call.args[0];
                let (buf, n) = (m.mem.r32(f + fcb::BUFFER), m.mem.r32(f + fcb::COUNT));
                out.borrow_mut().extend(m.mem.bytes(buf, n as usize));
                n
            } else {
                0
            }
        };
        let v = m.add_drv(info);
        assert_eq!(ret(&mut m, v, &returns), 1);
        m.mem.write(s, b"sim:FILE\0");
        let v = m.open(s, READ);
        assert_eq!(ret(&mut m, v, &returns), 2, "fd 0 and 1 are stdio");
        let v = m.transfer(2, 0x8004_0000, 16, false);
        assert_eq!(ret(&mut m, v, &returns), 8, "the driver's count is the read's result");
        let v = m.close(2);
        assert_eq!(ret(&mut m, v, &returns), 2);
        m.mem.write(s, b"sim\0");
        let v = m.del_drv(s);
        assert_eq!(ret(&mut m, v, &returns), 1);
        let fcb = fcb_at(2);
        assert_eq!(m.mem.r32(fcb + fcb::FLAGS), 0);
        assert!(m.find_device(b"sim").is_none());
        let recorded = calls.borrow();
        assert_eq!(recorded[0], (0x8003_0000 + dcb::INIT, [0; 4]));
        assert_eq!(recorded[1], (0x8003_0000 + dcb::OPEN, [fcb, s + 4, READ, 0]));
        assert_eq!(recorded[2], (0x8003_0000 + dcb::READ, [fcb, 0x8004_0000, 16, 0]));
        assert_eq!(recorded[3], (0x8003_0000 + dcb::CLOSE, [fcb, 0, 0, 0]));
        assert_eq!(recorded[4], (0x8003_0000 + dcb::REMOVE, [0; 4]));
        drop(recorded);

        // Redirect stdout to a guest tty: a diagnostic goes through its
        // IN_OUT function, and PrintInstalledDevices does too.
        m.mem.write(s, b"tty\0");
        let v = m.del_drv(s);
        assert_eq!(ret(&mut m, v, &returns), 1);
        let info2 = 0x8002_1000;
        m.mem.write(s + 0x10, b"tty\0");
        m.mem.w32(info2, s + 0x10);
        m.mem.w32(info2 + dcb::FLAGS, CHAR);
        m.mem.w32(info2 + dcb::DESC, s + 0x10);
        for o in (dcb::INIT..=dcb::CHECK).step_by(4) {
            m.mem.w32(info2 + o, 0x8003_1000);
        }
        m.mem.w32(info2 + dcb::IN_OUT, 0x8003_0000 + dcb::IN_OUT);
        let v = m.add_drv(info2);
        assert_eq!(ret(&mut m, v, &returns), 1);
        written.borrow_mut().clear();
        m.mem.write(s, b"nodev:x\0");
        let v = m.open(s, READ);
        assert_eq!(ret(&mut m, v, &returns), u32::MAX);
        assert_eq!(m.kernel.files.errno, E_NO_DEVICE);
        assert_eq!(written.borrow().as_slice(), b"nodev is not known device\r\n");
        let recorded = calls.borrow();
        assert!(recorded.iter().any(|(a, args)| *a == 0x8003_0000 + dcb::IN_OUT && *args == [fcb_at(1), 2, 0, 0]));
        drop(recorded);
        written.borrow_mut().clear();
        let v = crate::bios::call(&mut m, 0xB0, 0x49, [0; 4]);
        assert_eq!(ret(&mut m, v, &returns), 0);
        assert!(String::from_utf8_lossy(&written.borrow()).contains("tty"));
    }

    /// Scanning uses live FCB flags and guest ioctls; read/write defer
    /// their device lookup and transfer fields until all scans return.
    #[test]
    fn cdevscan_visits_live_files_before_transfers() {
        use std::cell::RefCell;

        let mut m = machine();
        let tty = m.find_device(b"tty").unwrap();
        let scan = 0x8003_0000;
        m.mem.w32(tty + dcb::IOCTL, scan);
        for (fd, flags) in [(0, READ | 0x100), (2, READ | SCAN), (3, READ | SCAN), (4, READ), (15, SCAN)] {
            m.mem.w32(fcb_at(fd) + fcb::FLAGS, flags);
            m.mem.w32(fcb_at(fd) + fcb::DCB, tty);
        }
        let seen = RefCell::new(Vec::new());
        let v = crate::bios::dispatch(&mut m, 0xC0, 0x16, [0; 4]);
        ret(&mut m, v, &|m, call| {
            assert_eq!(call.addr, scan);
            let fd = m.mem.r32(call.args[0] + fcb::FD);
            assert_eq!(call.args, [fcb_at(fd), FIOCSCAN, 0, 0]);
            seen.borrow_mut().push(fd);
            if fd == 2 {
                m.mem.w32(fcb_at(3) + fcb::FLAGS, 0);
                m.mem.w32(fcb_at(4) + fcb::FLAGS, READ | SCAN);
            }
            if fd == 4 {
                m.mem.w32(fcb_at(fd) + fcb::ERRNO, E_GENERAL);
                u32::MAX
            } else {
                0
            }
        });
        assert_eq!(*seen.borrow(), [2, 4, 15], "skip cleared/unflagged files, see newly flagged files, continue after ioctl failure");
        assert_eq!(m.kernel.files.errno, E_GENERAL);

        // Keep one scanner and redirect the transfer's device during it.
        for fd in [3, 4, 15] {
            m.mem.w32(fcb_at(fd) + fcb::FLAGS, 0);
        }
        let device = 0x8002_0000;
        let action = 0x8003_0100;
        for off in [dcb::IN_OUT, dcb::READ, dcb::WRITE] {
            m.mem.w32(device + off, action + off);
        }
        for fs in [false, true] {
            for write in [false, true] {
                let fd = u32::from(write);
                let f = fcb_at(fd);
                m.mem.w32(f + fcb::FLAGS, if write { WRITE } else { READ });
                m.mem.w32(f + fcb::DCB, tty);
                m.mem.w32(f + fcb::DEVICE_FLAGS, CHAR);
                m.mem.w32(f + fcb::BUFFER, 0x1234);
                m.mem.w32(f + fcb::COUNT, 0x5678);
                m.mem.w32(f + fcb::OFFSET, 9);
                seen.borrow_mut().clear();
                let buf = 0x8004_0000;
                let v = crate::bios::dispatch(&mut m, 0xB0, if write { 0x35 } else { 0x34 }, [fd, buf, 7, 0]);
                assert_eq!(
                    ret(&mut m, v, &|m, call| {
                        seen.borrow_mut().push(call.addr);
                        if call.addr == scan {
                            assert_eq!(call.args, [fcb_at(2), FIOCSCAN, 0, 0]);
                            assert_eq!(m.mem.r32(f + fcb::BUFFER), 0x1234);
                            assert_eq!(m.mem.r32(f + fcb::COUNT), 0x5678);
                            m.mem.w32(f + fcb::DCB, device);
                            m.mem.w32(f + fcb::DEVICE_FLAGS, if fs { FS } else { CHAR });
                            return 0;
                        }
                        let off = if fs {
                            if write {
                                dcb::WRITE
                            } else {
                                dcb::READ
                            }
                        } else {
                            dcb::IN_OUT
                        };
                        assert_eq!(call.addr, action + off);
                        assert_eq!(call.args, if fs { [f, buf, 7, 0] } else { [f, if write { 2 } else { 1 }, 0, 0] });
                        if !fs {
                            assert_eq!(m.mem.r32(f + fcb::BUFFER), buf);
                            assert_eq!(m.mem.r32(f + fcb::COUNT), 7);
                        }
                        3
                    }),
                    3
                );
                assert_eq!(seen.borrow().len(), 2);
                assert_eq!(m.mem.r32(f + fcb::OFFSET), if fs { 9 } else { 12 });
            }
        }
        let v = m.transfer(k::FILES, 0, 1, false);
        assert_eq!(ret(&mut m, v, &|_, _| panic!("invalid fd must fail before scanning")), u32::MAX);
    }

    fn iso_sector(lba: u32, data: &[u8]) -> [u8; cd::RAW] {
        let mut s = [0u8; cd::RAW];
        s[12..15].copy_from_slice(&cd::lba_to_msf(lba));
        s[15] = 2;
        s[0x18..0x18 + data.len()].copy_from_slice(data);
        s
    }

    fn iso_record(name: &str, lba: u32, size: u32, dir: bool) -> Vec<u8> {
        let mut r = vec![0u8; 33 + name.len() + (name.len() + 1) % 2];
        r[0] = r.len() as u8;
        r[2..6].copy_from_slice(&lba.to_le_bytes());
        r[10..14].copy_from_slice(&size.to_le_bytes());
        r[25] = if dir { 2 } else { 0 };
        r[32] = name.len() as u8;
        r[33..33 + name.len()].copy_from_slice(name.as_bytes());
        r
    }

    /// A `cdrom:` file goes through the device table to the native disc:
    /// open, seek, read, firstfile and close keep the FCB's fields, and
    /// paths are absolute from the root or relative to the current
    /// directory.
    #[test]
    fn native_cdrom_files_through_the_device_table() {
        let mut sectors = vec![[0u8; cd::RAW]; 25];
        let mut pvd = vec![0u8; cd::USER];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        pvd[140..144].copy_from_slice(&17u32.to_le_bytes());
        pvd[156..190].copy_from_slice(&iso_record("\0", 18, 2048, true)[..34]);
        sectors[16] = iso_sector(16, &pvd);
        let mut root = Vec::new();
        root.extend(iso_record("\0", 18, 2048, true));
        root.extend(iso_record("\x01", 18, 2048, true));
        root.extend(iso_record("A.BIN;1", 19, 5, false));
        root.extend(iso_record("DATA", 20, 2048, true));
        root.extend(iso_record("AX.DAT;1", 22, 1, false));
        root.extend(iso_record("A", 23, 1, false));
        sectors[18] = iso_sector(18, &root);
        sectors[19] = iso_sector(19, b"hello");
        let mut sub = Vec::new();
        sub.extend(iso_record("\0", 20, 2048, true));
        sub.extend(iso_record("\x01", 18, 2048, true));
        sub.extend(iso_record("B.BIN;1", 21, 3, false));
        sub.extend(iso_record("B.BIN;2", 24, 7, false));
        sectors[20] = iso_sector(20, &sub);
        sectors[21] = iso_sector(21, b"bye");
        sectors[24] = iso_sector(24, b"version");
        let mut m = crate::testing::machine_with_disc(sectors);
        let returns = |_: &mut Machine<NoSdk>, _: GuestCall| 0;
        m.mem.write(0x8001_0000, b"cdrom:\\A.BIN;1\0");
        let v = crate::bios::call(&mut m, 0xB0, 0x32, [0x8001_0000, READ, 0, 0]);
        assert_eq!(ret(&mut m, v, &returns), 2, "stdin and stdout are fd 0 and 1");
        let f = fcb_at(2);
        assert_eq!((m.mem.r32(f + fcb::LBA), m.mem.r32(f + fcb::LENGTH)), (19, 5));
        m.mem.write(0x8006_0000, b"\\A.BIN;1\0");
        let v = crate::bios::call(&mut m, 0xA0, 0xA4, [0x8006_0000, 0, 0, 0]);
        assert_eq!(ret(&mut m, v, &returns), 19, "CdGetLbn");
        let v = crate::bios::call(&mut m, 0xB0, 0x34, [2, 0x8002_0000, 2048, 0]);
        assert_eq!(ret(&mut m, v, &returns), 5);
        assert_eq!(m.mem.bytes(0x8002_0000, 5), b"hello");
        let v = crate::bios::call(&mut m, 0xB0, 0x33, [2, 1, 0, 0]);
        assert_eq!(ret(&mut m, v, &returns), 1);
        let v = crate::bios::call(&mut m, 0xB0, 0x34, [2, 0x8002_0010, 2048, 0]);
        assert_eq!(ret(&mut m, v, &returns), u32::MAX, "unaligned file position");
        assert_eq!(m.mem.r32(f + fcb::OFFSET), 1);
        assert_eq!(m.kernel.files.errno, E_ALIGN);
        let v = crate::bios::call(&mut m, 0xB0, 0x36, [2, 0, 0, 0]);
        assert_eq!(ret(&mut m, v, &returns), 2);

        // chdir replaces the cwd, even with a nonexistent name. It does
        // not check that directory, and a second relative name is not
        // appended to the first one.
        m.mem.write(0x8005_0000, b"cdrom:missing\0");
        let v = crate::bios::call(&mut m, 0xB0, 0x40, [0x8005_0000, 0, 0, 0]);
        assert_eq!(ret(&mut m, v, &returns), 1);
        assert_eq!(m.kernel.files.cd_cwd, "MISSING");
        m.mem.write(0x8001_0000, b"cdrom:A.BIN;1\0");
        let v = crate::bios::call(&mut m, 0xB0, 0x32, [0x8001_0000, READ, 0, 0]);
        assert_eq!(ret(&mut m, v, &returns), u32::MAX, "lookup uses the nonexistent cwd");
        assert_eq!(m.kernel.files.errno, E_NOT_FOUND);
        m.mem.write(0x8005_0000, b"cdrom:data\0");
        let v = crate::bios::call(&mut m, 0xB0, 0x40, [0x8005_0000, 0, 0, 0]);
        assert_eq!(ret(&mut m, v, &returns), 1);
        assert_eq!(m.kernel.files.cd_cwd, "DATA");
        assert_eq!(m.kernel.files.errno, E_NOT_FOUND, "chdir leaves errno alone");
        m.mem.write(0x8001_0000, b"cdrom:B.BIN;1\0");
        let v = crate::bios::call(&mut m, 0xB0, 0x32, [0x8001_0000, READ, 0, 0]);
        assert_eq!(ret(&mut m, v, &returns), 2, "relative to DATA");
        let f = fcb_at(2);
        assert_eq!((m.mem.r32(f + fcb::LBA), m.mem.r32(f + fcb::LENGTH)), (21, 3));
        let v = crate::bios::call(&mut m, 0xB0, 0x34, [2, 0x8002_0020, 2048, 0]);
        assert_eq!(ret(&mut m, v, &returns), 3);
        assert_eq!(m.mem.bytes(0x8002_0020, 3), b"bye");
        let v = crate::bios::call(&mut m, 0xB0, 0x36, [2, 0, 0, 0]);
        assert_eq!(ret(&mut m, v, &returns), 2);

        m.mem.write(0x8003_0000, b"cdrom:*.BIN\0");
        let v = crate::bios::call(&mut m, 0xB0, 0x42, [0x8003_0000, 0x8004_0000, 0, 0]);
        assert_eq!(ret(&mut m, v, &returns), 0x8004_0000);
        assert_eq!(m.mem.cstr(0x8004_0000, 32), b"B.BIN;1", "relative to DATA");
        assert_eq!(m.mem.r32(0x8004_0020), 21);
        let v = crate::bios::call(&mut m, 0xB0, 0x43, [0x8004_0000, 0, 0, 0]);
        assert_eq!(ret(&mut m, v, &returns), 0x8004_0000);
        assert_eq!(m.mem.cstr(0x8004_0000, 32), b"B.BIN;2");
        assert_eq!(m.mem.r32(0x8004_0020), 24);
        let v = crate::bios::call(&mut m, 0xB0, 0x43, [0x8004_0000, 0, 0, 0]);
        assert_eq!(ret(&mut m, v, &returns), 0);
        m.mem.write(0x8003_0000, b"cdrom:\\*.BIN\0");
        let v = crate::bios::call(&mut m, 0xB0, 0x42, [0x8003_0000, 0x8004_0000, 0, 0]);
        assert_eq!(ret(&mut m, v, &returns), 0x8004_0000);
        assert_eq!(m.mem.cstr(0x8004_0000, 32), b"A.BIN;1", "an absolute pattern ignores the cwd");
        assert_eq!(m.mem.r32(0x8004_0020), 19);
        m.mem.write(0x8005_0000, b"cdrom:\0");
        let v = crate::bios::call(&mut m, 0xB0, 0x40, [0x8005_0000, 0, 0, 0]);
        assert_eq!(ret(&mut m, v, &returns), 1);
        assert!(m.kernel.files.cd_cwd.is_empty(), "empty chdir replaces the cwd with root");
        let v = crate::bios::call(&mut m, 0xB0, 0x32, [0x8001_0000, READ, 0, 0]);
        assert_eq!(ret(&mut m, v, &returns), u32::MAX, "B.BIN is not in root");
        m.mem.write(0x8001_0000, b"cdrom:A.BIN;1\0");
        let v = crate::bios::call(&mut m, 0xB0, 0x32, [0x8001_0000, READ, 0, 0]);
        assert_eq!(ret(&mut m, v, &returns), 2, "relative open after clearing cwd");
        let v = crate::bios::call(&mut m, 0xB0, 0x36, [2, 0, 0, 0]);
        assert_eq!(ret(&mut m, v, &returns), 2);

        // Open defaults to ;1, but does not ignore an explicit version.
        // Only the file part accepts '?'; '*' is for directory searches.
        for (path, expected) in [
            ("a.bin", Some((19, 5))),
            ("?.bin;1", Some((19, 5))),
            ("a?.dat", Some((22, 1))),
            ("a?.absent", Some((23, 1))), // '?' at the name's NUL
            ("data\\?.bin", Some((21, 3))),
            ("data\\b.bin;2", Some((24, 7))),
            ("data\\?.bin;?", Some((21, 3))),
            ("a.bin;2", None),
            ("a.bin;", None),
            ("a*.bin", None),
            ("d?ta\\b.bin", None),
        ] {
            m.mem.write(0x8001_0000, format!("cdrom:{path}\0").as_bytes());
            let v = crate::bios::call(&mut m, 0xB0, 0x32, [0x8001_0000, READ, 0, 0]);
            let fd = ret(&mut m, v, &returns);
            if let Some((lba, size)) = expected {
                assert_eq!(fd, 2, "open {path}");
                assert_eq!((m.mem.r32(f + fcb::LBA), m.mem.r32(f + fcb::LENGTH)), (lba, size), "open {path}");
                assert_eq!((m.mem.r32(f + fcb::OFFSET), m.mem.r32(f + fcb::ERRNO)), (0, 0));
                let v = crate::bios::call(&mut m, 0xB0, 0x36, [fd, 0, 0, 0]);
                assert_eq!(ret(&mut m, v, &returns), fd);
            } else {
                assert_eq!(fd, u32::MAX, "open {path}");
                assert_eq!(m.kernel.files.errno, E_NOT_FOUND);
                assert_eq!(m.mem.r32(f + fcb::FLAGS), 0, "failed open releases its FCB");
            }
        }

        // Search matches the on-disc name, including its version. A star
        // ignores the suffix after it; a question mark at NUL accepts the
        // remaining pattern as well. Exercise both firstfile and nextfile.
        for (pattern, expected) in [
            ("a*.missing", vec!["A.BIN;1", "AX.DAT;1", "A"]),
            ("A?.DAT;1", vec!["AX.DAT;1", "A"]),
            ("A.BIN;1", vec!["A.BIN;1"]),
            ("A.BIN;2", vec![]),
            ("A.BIN", vec![]),
        ] {
            m.mem.write(0x8003_0000, format!("cdrom:{pattern}\0").as_bytes());
            let v = crate::bios::call(&mut m, 0xB0, 0x42, [0x8003_0000, 0x8004_0000, 0, 0]);
            let mut v = ret(&mut m, v, &returns);
            for name in expected {
                assert_eq!(v, 0x8004_0000, "pattern {pattern}, expected {name}");
                assert_eq!(m.mem.cstr(v, 32), name.as_bytes(), "pattern {pattern}");
                let next = crate::bios::call(&mut m, 0xB0, 0x43, [0x8004_0000, 0, 0, 0]);
                v = ret(&mut m, next, &returns);
            }
            assert_eq!(v, 0, "no more matches for {pattern}");
        }
    }

    /// Public read aliases and the direct CD device entry enforce the same
    /// validation, full-sector writes, clipped result and error bookkeeping.
    #[test]
    fn cdrom_reads_validate_and_transfer_whole_sectors() {
        let sector = cd::USER as u32;
        let sectors = (0..3).map(|lba| iso_sector(lba, &vec![0x11 * (lba as u8 + 1); cd::USER])).collect();
        let mut m = crate::testing::machine_with_disc(sectors);
        let f = fcb_at(2);
        let dst = 0x8002_0001; // Only the file position/length require sector alignment.
        m.mem.w32(f + fcb::FLAGS, READ);
        m.mem.w32(f + fcb::DCB, m.find_device(b"cdrom").unwrap());
        m.mem.w32(f + fcb::DEVICE_FLAGS, FS);
        m.mem.w32(f + fcb::LBA, 0);
        let returns = |_: &mut Machine<NoSdk>, _: GuestCall| panic!("no guest callback");
        for (vector, func, arg) in [(0xB0, 0x34, 2), (0xA0, 0x02, 2), (0xA0, 0x60, f)] {
            let direct = func == 0x60;
            for (off, len, size) in [
                (0, 1, sector),
                (1, sector, sector),
                (0, 0xFFFF_F800, sector),
                (sector, sector, sector),
                (2 * sector, sector, sector),
                (0, sector, 0),
                (sector, 0, sector),
            ] {
                m.mem.w32(f + fcb::OFFSET, off);
                m.mem.w32(f + fcb::LENGTH, size);
                m.mem.w32(f + fcb::ERRNO, 0x1234);
                m.kernel.files.errno = 0x5678;
                m.mem.write(dst, &vec![0xCC; cd::USER]);
                m.loaded.clear();
                let v = crate::bios::dispatch(&mut m, vector, func, [arg, dst, len, 0]);
                assert_eq!(ret(&mut m, v, &returns), u32::MAX, "{vector:02X}:{func:02X} offset={off} length={len} size={size}");
                assert_eq!(m.mem.r32(f + fcb::OFFSET), off);
                assert_eq!(m.mem.r32(f + fcb::ERRNO), E_ALIGN);
                assert_eq!(m.kernel.files.errno, if direct { 0x5678 } else { E_ALIGN });
                assert_eq!(m.mem.bytes(dst, cd::USER), vec![0xCC; cd::USER]);
                assert!(m.loaded.is_empty());
            }
            // Clip only the return value and final offset, not the sectors
            // written. Starting at the second sector also covers a short tail.
            for off in [0, sector] {
                m.mem.w32(f + fcb::OFFSET, off);
                m.mem.w32(f + fcb::LENGTH, sector + 5);
                m.mem.w32(f + fcb::ERRNO, 0x1234);
                m.kernel.files.errno = 0x5678;
                m.mem.write(dst - 1, &vec![0xCC; 2 * cd::USER + 2]);
                m.loaded.clear();
                let v = crate::bios::dispatch(&mut m, vector, func, [arg, dst, 2 * sector, 0]);
                assert_eq!(ret(&mut m, v, &returns), sector + 5 - off);
                assert_eq!(m.mem.r32(f + fcb::OFFSET), sector + 5);
                assert_eq!(m.mem.r32(f + fcb::ERRNO), 0x1234, "success does not clear the FCB error");
                assert_eq!(m.kernel.files.errno, 0x5678);
                for i in 0..2 {
                    assert_eq!(m.mem.bytes(dst + i * sector, cd::USER), vec![0x11 * ((off / sector + i) as u8 + 1); cd::USER]);
                }
                assert_eq!((m.mem.r8(dst - 1), m.mem.r8(dst + 2 * sector)), (0xCC, 0xCC));
                assert_eq!(m.loaded, [(dst, sector), (dst + sector, sector)]);
            }
            // A failed later sector cannot turn a request into a short success
            // merely because its first sector already covered the logical EOF.
            m.mem.w32(f + fcb::OFFSET, sector);
            m.mem.w32(f + fcb::LENGTH, sector + 5);
            m.mem.w32(f + fcb::ERRNO, 0x1234);
            m.kernel.files.errno = 0x5678;
            m.mem.write(dst - 1, &vec![0xCC; 3 * cd::USER + 2]);
            m.loaded.clear();
            let v = crate::bios::dispatch(&mut m, vector, func, [arg, dst, 3 * sector, 0]);
            assert_eq!(ret(&mut m, v, &returns), u32::MAX);
            assert_eq!(m.mem.r32(f + fcb::OFFSET), sector, "failure does not advance the offset");
            assert_eq!(m.mem.r32(f + fcb::ERRNO), E_GENERAL);
            assert_eq!(m.kernel.files.errno, if direct { 0x5678 } else { E_GENERAL });
            assert_eq!(m.mem.r8(dst - 1), 0xCC);
            assert_eq!(m.mem.bytes(dst, cd::USER), vec![0x22; cd::USER]);
            assert_eq!(m.mem.bytes(dst + sector, cd::USER), vec![0x33; cd::USER]);
            assert_eq!(m.mem.bytes(dst + 2 * sector, cd::USER + 1), vec![0xCC; cd::USER + 1]);
            assert_eq!(m.loaded, [(dst, sector), (dst + sector, sector)]);
        }
    }

    /// chdir stores its name before either metadata read. Failures return
    /// zero without a directory lookup or an errno write; the FCB is freed.
    #[test]
    fn cdrom_chdir_keeps_name_when_metadata_read_fails() {
        for failure in 0..3 {
            let mut sectors = vec![[0u8; cd::RAW]; if failure == 0 { 0 } else { 17 }];
            if failure != 0 {
                let mut pvd = vec![0u8; cd::USER];
                pvd[0] = 1;
                pvd[1..6].copy_from_slice(if failure == 1 { b"WRONG" } else { b"CD001" });
                pvd[140..144].copy_from_slice(&17u32.to_le_bytes());
                sectors[16] = iso_sector(16, &pvd);
            }
            let mut m = crate::testing::machine_with_disc(sectors);
            m.kernel.files.cd_cwd = "OLD".into();
            m.kernel.files.errno = 0x1234;
            m.mem.write(0x8001_0000, b"cdrom:new\0");
            let v = crate::bios::call(&mut m, 0xB0, 0x40, [0x8001_0000, 0, 0, 0]);
            assert_eq!(ret(&mut m, v, &|_, _| panic!("no guest callback")), 0, "failure {failure}");
            assert_eq!(m.kernel.files.cd_cwd, "NEW");
            assert_eq!(m.kernel.files.errno, 0x1234);
            assert_eq!(m.mem.r32(fcb_at(2) + fcb::FLAGS), 0);
            m.mem.write(0x8001_0000, b"direct\0");
            let v = crate::bios::call(&mut m, 0xA0, 0x64, [fcb_at(2), 0x8001_0000, 0, 0]);
            assert_eq!(ret(&mut m, v, &|_, _| panic!("no guest callback")), 0);
            assert_eq!(m.kernel.files.cd_cwd, "DIRECT", "direct device entry uses the same contract");
        }
    }

    /// `gets` uses the installed console's file callbacks for both read
    /// and echo, in order; editing, quoting and the 125-byte limit follow
    /// OpenBIOS `psxgets`.
    #[test]
    fn gets_reads_and_echoes_through_a_guest_tty() {
        use std::cell::{Cell, RefCell};
        use std::collections::VecDeque;
        use std::rc::Rc;

        let mut m = machine();
        let name = 0x8001_0000;
        let info = 0x8002_0000;
        let func = 0x8003_0000;
        m.mem.write(name, b"tty\0");
        let v = m.del_drv(name);
        assert_eq!(ret(&mut m, v, &|_, _| 0), 1);
        m.mem.write(name, b"tty\0");
        m.mem.w32(info, name);
        m.mem.w32(info + dcb::FLAGS, CHAR | CONS);
        m.mem.w32(info + dcb::BLOCK, 1);
        for o in (dcb::INIT..=dcb::CHECK).step_by(4) {
            m.mem.w32(info + o, func + o);
        }
        m.mem.w32(info + dcb::IN_OUT, func + dcb::IN_OUT);
        let v = m.add_drv(info);
        assert_eq!(ret(&mut m, v, &|_, _| 0), 1);
        let dcb = m.find_device(b"tty").expect("the guest console");
        for (fd, mode) in [(0, READ), (1, WRITE)] {
            let f = fcb_at(fd);
            m.mem.w32(f + fcb::FLAGS, mode);
            m.mem.w32(f + fcb::DCB, dcb);
            m.mem.w32(f + fcb::DEVICE_FLAGS, CHAR | CONS);
        }

        let input = Rc::new(RefCell::new(VecDeque::new()));
        let events = Rc::new(RefCell::new(Vec::new()));
        let terminator = Rc::new(Cell::new(0u32));
        let (queue, seen, tail) = (input.clone(), events.clone(), terminator.clone());
        let returns = move |m: &mut Machine<NoSdk>, call: GuestCall| {
            if call.addr != func + dcb::IN_OUT {
                return 0;
            }
            let f = call.args[0];
            match call.args[1] {
                1 => match queue.borrow_mut().pop_front() {
                    Some(c) => {
                        seen.borrow_mut().push((1, c));
                        let at = m.mem.r32(f + fcb::BUFFER);
                        m.mem.w8(at, c);
                        1
                    }
                    None => u32::MAX,
                },
                2 => {
                    if queue.borrow().is_empty() {
                        assert_eq!(m.mem.r8(tail.get()), 0xCC, "the terminator is written only after the final echo returns");
                    }
                    let (at, n) = (m.mem.r32(f + fcb::BUFFER), m.mem.r32(f + fcb::COUNT));
                    let bytes = m.mem.bytes(at, n as usize);
                    seen.borrow_mut().extend(bytes.into_iter().map(|c| (2, c)));
                    n
                }
                _ => 0,
            }
        };

        input.borrow_mut().extend(b"\x7fab\x08c\t\x16\nQ\r");
        let line = 0x8004_0000;
        terminator.set(line + 5);
        m.mem.w8(line + 5, 0xCC);
        let v = m.gets(line);
        assert_eq!(ret(&mut m, v, &returns), line);
        assert_eq!(m.mem.bytes(line, 6), b"ac \nQ\0");
        let expected = vec![
            (1, 0x7f),
            (2, 7),
            (1, b'a'),
            (2, b'a'),
            (1, b'b'),
            (2, b'b'),
            (1, 0x08),
            (2, 0x08),
            (2, b' '),
            (2, 0x08),
            (1, b'c'),
            (2, b'c'),
            (1, b'\t'),
            (2, b' '),
            (1, 0x16),
            (1, b'\n'),
            (2, b'\r'),
            (2, b'\n'),
            (1, b'Q'),
            (2, b'Q'),
            (1, b'\r'),
            (2, b'\r'),
            (2, b'\n'),
        ];
        assert_eq!(*events.borrow(), expected, "each read precedes its echo");

        events.borrow_mut().clear();
        input.borrow_mut().extend(std::iter::repeat(b'x').take(125));
        input.borrow_mut().extend(b"y\r");
        let full = 0x8005_0000;
        terminator.set(full + 125);
        m.mem.w8(full + 125, 0xCC);
        let v = m.gets(full);
        assert_eq!(ret(&mut m, v, &returns), full);
        assert_eq!(m.mem.bytes(full, 125), vec![b'x'; 125]);
        assert_eq!(m.mem.r8(full + 125), 0, "the terminator still fits");
        let events = events.borrow();
        assert_eq!(events.len(), 255, "125 characters, then a bell and the newline");
        assert_eq!(&events[248..], &[(1, b'x'), (2, b'x'), (1, b'y'), (2, 7), (1, b'\r'), (2, b'\r'), (2, b'\n')]);
    }

    /// The console is the host's log: TAB and LF are expanded through the
    /// tty device's `write`, and there is no host console input yet.
    #[test]
    fn the_console_is_the_log() {
        let mut m = machine();
        // The BIOS tables reach InstallDevices, FlushStdInOutPut and
        // KernelRedirect; an invalid redirect leaves the console alone.
        let v = crate::bios::dispatch(&mut m, 0xC0, 0x12, [0; 4]);
        assert_eq!(ret(&mut m, v, &|_, _| 0), 0);
        let v = crate::bios::dispatch(&mut m, 0xC0, 0x13, [0; 4]);
        assert_eq!(ret(&mut m, v, &|_, _| 0), 0);
        let v = crate::bios::dispatch(&mut m, 0xC0, 0x1B, [2, 0, 0, 0]);
        assert_eq!(ret(&mut m, v, &|_, _| 0), 0);
        assert!(m.find_device(b"tty").is_some(), "an invalid KernelRedirect changes nothing");
        m.mem.write(0x8001_0000, b"a\tb\n\0");
        let v = crate::bios::call(&mut m, 0xB0, 0x3F, [0x8001_0000, 0, 0, 0]);
        ret(&mut m, v, &|_, _| 0);
        assert!(m.kernel.files.line.is_empty());
        let v = crate::bios::call(&mut m, 0xB0, 0x3D, [b'x' as u32, 0, 0, 0]);
        ret(&mut m, v, &|_, _| 0);
        assert_eq!(m.kernel.files.line, b"x");
        assert_eq!(m.mem.r32(fcb_at(1) + fcb::OFFSET), 12, "a TAB to column 8, LF as CR LF, then x");
        let v = crate::bios::call(&mut m, 0xB0, 0x3E, [0x8001_0000, 0, 0, 0]);
        assert!(v.is_err(), "console input is pending");
    }
}
