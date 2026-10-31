//! A(41h) `LoadTest` and A(42h) `Load` (psx-spx "BIOS File Execute and
//! Flush Cache"; pinned OpenBIOS `kernel/psxexe.c`). Both read the
//! executable header through the ordinary file functions, so a guest
//! device driver's callbacks and the native `cdrom` device behave as they
//! do for any other read.
//!
//! The BIOS reads the header into a sector buffer of its own, not into the
//! caller's header. [`psxw_abi::kernel::LOAD_BUFFER`] is that buffer, in
//! guest-visible kernel RAM because a guest device callback writes it. As
//! with the reference's static buffer, a re-entrant load shares it.

use crate::{bios, files};
use crate::machine::{seq, Flow, Machine, Sdk};

/// The loader's sector buffer as its guest pointer (KSEG1).
const BUFFER: u32 = 0xA000_0000 | psxw_abi::kernel::LOAD_BUFFER;

/// The header sector's size (`800h`); its bytes `10h..4Bh` (60) reach the
/// caller's header buffer.
const SECTOR: u32 = psxw_abi::kernel::LOAD_BUFFER_LEN;
const HEADER_AT: u32 = 0x10;
const HEADER_COPY: u32 = 0x3C;

/// `headerbuf` offsets of the fields `Load` uses (the copied bytes start at
/// the file's `10h`).
const TEXT_ADDR: u32 = 0x08;
const TEXT_SIZE: u32 = 0x0C;

/// A(41h) `LoadTest(filename, headerbuf)`: open for reading, read `800h`
/// bytes, copy file offsets `10h..4Bh` (without a magic check) to
/// `headerbuf`, and close. 1 for a full header, 0 when the open fails or
/// the read is short.
pub fn load_test<S: Sdk>(m: &mut Machine<S>, filename: u32, header: u32) -> Result<Flow<S>, String> {
    load_exe(m, filename, header, false)
}

/// A(42h) `Load(filename, headerbuf)`: `LoadTest`, then the body at the
/// header's destination and size, its read result ignored, then close and
/// `FlushCache`. 1 once the header was read.
pub fn load<S: Sdk>(m: &mut Machine<S>, filename: u32, header: u32) -> Result<Flow<S>, String> {
    load_exe(m, filename, header, true)
}

fn load_exe<S: Sdk>(m: &mut Machine<S>, filename: u32, header: u32, body: bool) -> Result<Flow<S>, String> {
    let open = bios::dispatch(m, 0xB0, 0x32, [filename, files::READ, 0, 0])?;
    Ok(seq(m, open, move |m, v| opened(m, header, body, v as u32)))
}

fn opened<S: Sdk>(m: &mut Machine<S>, header: u32, body: bool, fd: u32) -> Flow<S> {
    if (fd as i32) < 0 {
        return Flow::ret(0);
    }
    let read = bios::dispatch(m, 0xB0, 0x34, [fd, BUFFER, SECTOR, 0]).unwrap_or_else(|e| panic!("Load: {e}"));
    seq(m, read, move |m, n| read_header(m, header, body, fd, n as u32))
}

fn read_header<S: Sdk>(m: &mut Machine<S>, header: u32, body: bool, fd: u32, n: u32) -> Flow<S> {
    if (n as i32) < SECTOR as i32 {
        return finish(m, fd, 0, false);
    }
    let bytes = m.mem.bytes(BUFFER + HEADER_AT, HEADER_COPY as usize);
    m.mem.write(header, &bytes);
    if !body {
        return finish(m, fd, 1, false);
    }
    let (text, size) = (m.mem.r32(header + TEXT_ADDR), m.mem.r32(header + TEXT_SIZE));
    let read = bios::dispatch(m, 0xB0, 0x34, [fd, text, size, 0]).unwrap_or_else(|e| panic!("Load: {e}"));
    seq(m, read, move |m, _| finish(m, fd, 1, true))
}

/// Close `fd` and return `value`; `Load` also calls `FlushCache` before
/// returning, as the retail function does after its body read.
fn finish<S: Sdk>(m: &mut Machine<S>, fd: u32, value: u32, flush: bool) -> Flow<S> {
    let close = bios::dispatch(m, 0xB0, 0x36, [fd, 0, 0, 0]).unwrap_or_else(|e| panic!("Load: {e}"));
    seq(m, close, move |m, _| {
        if !flush {
            return Flow::ret(value);
        }
        let cache = crate::bios::call(m, 0xA0, 0x44, [0; 4]).unwrap_or_else(|e| panic!("Load: FlushCache: {e}"));
        seq(m, cache, move |_, _| Flow::ret(value))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::{dcb, fcb, fcb_at};
    use crate::testing::{machine, NoSdk};
    use crate::GuestCall;
    use core::cell::RefCell;
    use std::rc::Rc;

    const PATH: u32 = 0x8001_0000;
    const INFO: u32 = 0x8002_0000;
    const FUNCS: u32 = 0x8003_0000;
    const HEADER_BUF: u32 = 0x8004_0000;
    const TEXT: u32 = 0x8005_0000;

    #[derive(Default)]
    struct ExeFile {
        header: Vec<u8>,
        body: Vec<u8>,
        short_header: bool,
        short_body: bool,
        open_fail: bool,
        calls: Vec<(u32, [u32; 4])>,
    }

    fn header(text: u32, size: u32) -> Vec<u8> {
        let mut h = vec![0u8; SECTOR as usize];
        // No magic check: these bytes must reach the caller unchanged.
        h[..8].copy_from_slice(b"NOT-EXE!");
        h[0x10..0x14].copy_from_slice(&0x8001_0000u32.to_le_bytes());
        h[0x18..0x1C].copy_from_slice(&text.to_le_bytes());
        h[0x1C..0x20].copy_from_slice(&size.to_le_bytes());
        for (i, b) in h.iter_mut().enumerate().take(0x4C).skip(0x30) {
            *b = i as u8;
        }
        h
    }

    fn returns(file: &mut ExeFile, m: &mut Machine<NoSdk>, call: GuestCall) -> u32 {
        file.calls.push((call.addr, call.args));
        match call.addr - FUNCS {
            dcb::OPEN if file.open_fail => 1,
            dcb::OPEN => 0,
            dcb::READ => {
                let (dst, len) = (call.args[1], call.args[2]);
                if len == SECTOR {
                    let n = if file.short_header { SECTOR - 1 } else { SECTOR };
                    m.mem.write(dst, &file.header[..n as usize]);
                    n
                } else {
                    let n = if file.short_body { file.body.len() as u32 / 2 } else { file.body.len() as u32 };
                    m.mem.write(dst, &file.body[..n as usize]);
                    n
                }
            }
            dcb::CLOSE => 0,
            _ => 0,
        }
    }

    fn run(m: &mut Machine<NoSdk>, f: Result<Flow<NoSdk>, String>, file: &Rc<RefCell<ExeFile>>) -> u32 {
        match f.unwrap() {
            Flow::Return(v) => v as u32,
            Flow::Call { call, then } => {
                let v = returns(&mut file.borrow_mut(), m, call);
                let next = then(m, v as u64);
                run(m, Ok(next), file)
            }
            Flow::Block { .. } => panic!("blocked"),
        }
    }

    fn setup(m: &mut Machine<NoSdk>) -> Rc<RefCell<ExeFile>> {
        m.lay_out_events(16);
        assert_eq!(m.event_count(), 16, "the normal boot's default EvCB count");
        let file = Rc::new(RefCell::new(ExeFile {
            header: header(TEXT, 0x10),
            body: vec![0xA1; 0x10],
            ..Default::default()
        }));
        m.mem.write(PATH, b"sim:FILE\0");
        m.mem.write(PATH + 0x10, b"sim\0");
        m.mem.write(PATH + 0x20, b"fake exe\0");
        m.mem.w32(INFO + dcb::NAME, PATH + 0x10);
        m.mem.w32(INFO + dcb::FLAGS, files::FS);
        m.mem.w32(INFO + dcb::DESC, PATH + 0x20);
        for o in (dcb::INIT..=dcb::CHECK).step_by(4) {
            m.mem.w32(INFO + o, FUNCS + o);
        }
        let flow = m.add_drv(INFO);
        assert_eq!(run(m, flow, &file), 1);
        file.borrow_mut().calls.clear();
        file
    }

    fn count(file: &Rc<RefCell<ExeFile>>, addr: u32) -> usize {
        file.borrow().calls.iter().filter(|(a, _)| *a == addr).count()
    }

    fn closed(m: &Machine<NoSdk>) {
        assert_eq!(m.mem.r32(fcb_at(2) + fcb::FLAGS), 0, "the file is closed");
    }

    #[test]
    fn patched_file_calls_keep_signed_errors_and_cleanup_order() {
        use crate::tables;
        fn resume(m: &mut Machine<NoSdk>, flow: Flow<NoSdk>, addr: u32, args: [u32; 4], result: u32) -> Flow<NoSdk> {
            let Flow::Call { call, then } = flow else { panic!("expected patched file call") };
            assert_eq!(call, GuestCall { addr, args });
            then(m, result as u64)
        }
        let mut m = machine();
        let (open, read, close) = (FUNCS, FUNCS + 4, FUNCS + 8);
        for (func, target) in [(0x32, open), (0x34, read), (0x36, close)] {
            m.mem.w32(tables::entry(0xB0, func).unwrap(), target);
        }
        // Any negative open result is failure, not merely -1.
        let flow = load(&mut m, PATH, HEADER_BUF).unwrap();
        let flow = resume(&mut m, flow, open, [PATH, files::READ, 0, 0], 0x8000_0000);
        assert!(matches!(flow, Flow::Return(0)));

        // A negative header read must close without copying stale buffer data.
        m.mem.write(HEADER_BUF, &[0x5A; 0x40]);
        let flow = load(&mut m, PATH, HEADER_BUF).unwrap();
        let flow = resume(&mut m, flow, open, [PATH, files::READ, 0, 0], 7);
        let flow = resume(&mut m, flow, read, [7, BUFFER, SECTOR, 0], u32::MAX);
        assert_eq!(m.mem.bytes(HEADER_BUF, 0x40), vec![0x5A; 0x40]);
        let flow = resume(&mut m, flow, close, [7, 0, 0, 0], u32::MAX);
        assert!(matches!(flow, Flow::Return(0)));

        // Body and close errors do not change success after a complete header.
        let flow = load(&mut m, PATH, HEADER_BUF).unwrap();
        let flow = resume(&mut m, flow, open, [PATH, files::READ, 0, 0], 7);
        m.mem.write(BUFFER, &header(TEXT, 16));
        let flow = resume(&mut m, flow, read, [7, BUFFER, SECTOR, 0], SECTOR);
        let flow = resume(&mut m, flow, read, [7, TEXT, 16, 0], u32::MAX);
        let flow = resume(&mut m, flow, close, [7, 0, 0, 0], u32::MAX);
        assert!(matches!(flow, Flow::Return(1)));
    }

    #[test]
    fn load_buffer_is_reserved_and_clear_of_kernel_state() {
        use psxw_abi::kernel as k;
        let range = k::LOAD_BUFFER..k::LOAD_BUFFER + k::LOAD_BUFFER_LEN;
        assert_eq!(range, 0x8040..0x8840);
        // The masked native callable slots, the kernel data buffers and the
        // control blocks all stay outside it.
        let files = k::FCBS..k::DCBS + k::DEVICES * k::DCB_SIZE + 0x100;
        let blocks = k::EXCB..k::EVCBS + k::MAX_EVENTS * k::EVCB_SIZE;
        for other in [0x8000..0x8030, k::DATA, files, blocks] {
            assert!(range.start >= other.end || range.end <= other.start, "{range:?} overlaps {other:?}");
        }
        assert!(k::ram_backed(k::LOAD_BUFFER));
        assert!(k::ram_backed(0xA000_0000 | k::LOAD_BUFFER));
    }

    #[test]
    fn load_test_and_load_use_the_file_device_and_a_private_sector_buffer() {
        assert!(crate::bios::implemented(0xA0, 0x41));
        assert!(crate::bios::implemented(0xA0, 0x42));
        let mut m = machine();
        let file = setup(&mut m);

        // LoadTest: a full header, exactly bytes 10h..4Bh (no magic check),
        // no body read, then close.
        m.mem.write(HEADER_BUF, &[0xA5; 0x40]);
        let v = crate::bios::call(&mut m, 0xA0, 0x41, [PATH, HEADER_BUF, 0, 0]);
        assert_eq!(run(&mut m, v, &file), 1);
        assert_eq!(m.mem.bytes(HEADER_BUF, 0x3C), file.borrow().header[0x10..0x4C].to_vec());
        assert_eq!(m.mem.r8(HEADER_BUF + 0x3C), 0xA5, "only 60 bytes are copied");
        assert_eq!((count(&file, FUNCS + dcb::READ), count(&file, FUNCS + dcb::CLOSE)), (1, 1));
        let calls = file.borrow().calls.clone();
        assert_eq!(calls.last().unwrap().0, FUNCS + dcb::CLOSE, "closed after the header");
        let read = calls.iter().find(|(a, _)| *a == FUNCS + dcb::READ).unwrap().1;
        assert_eq!(read[1], BUFFER, "the header read the reserved guest sector buffer");
        assert_ne!(read[1], HEADER_BUF);
        closed(&m);

        // Load: the header, then the body at the header's address and size.
        file.borrow_mut().calls.clear();
        m.mem.write(TEXT, &[0; 0x10]);
        let v = crate::bios::call(&mut m, 0xA0, 0x42, [PATH, HEADER_BUF, 0, 0]);
        assert_eq!(run(&mut m, v, &file), 1);
        assert_eq!(m.mem.bytes(TEXT, 0x10), file.borrow().body);
        assert_eq!(m.mem.bytes(HEADER_BUF, 0x3C), file.borrow().header[0x10..0x4C].to_vec());
        assert_eq!((count(&file, FUNCS + dcb::READ), count(&file, FUNCS + dcb::CLOSE)), (2, 1));
        let calls = file.borrow().calls.clone();
        assert_eq!(calls.last().unwrap().0, FUNCS + dcb::CLOSE);
        let body = calls.iter().filter(|(a, _)| *a == FUNCS + dcb::READ).nth(1).unwrap().1;
        assert_eq!((body[1], body[2]), (TEXT, 0x10), "the body goes where the header says");
        closed(&m);

        // A short header leaves the caller's buffer alone and closes.
        file.borrow_mut().short_header = true;
        file.borrow_mut().calls.clear();
        m.mem.write(HEADER_BUF, &[0x5A; 0x40]);
        let v = crate::bios::call(&mut m, 0xA0, 0x41, [PATH, HEADER_BUF, 0, 0]);
        assert_eq!(run(&mut m, v, &file), 0);
        assert_eq!(m.mem.bytes(HEADER_BUF, 0x40), vec![0x5A; 0x40]);
        assert_eq!((count(&file, FUNCS + dcb::READ), count(&file, FUNCS + dcb::CLOSE)), (1, 1));
        closed(&m);

        // A short body read is ignored: Load still succeeds, the bytes the
        // device wrote stay visible, and the file is still closed.
        file.borrow_mut().short_header = false;
        file.borrow_mut().short_body = true;
        file.borrow_mut().calls.clear();
        m.mem.write(TEXT, &[0; 0x10]);
        let v = crate::bios::call(&mut m, 0xA0, 0x42, [PATH, HEADER_BUF, 0, 0]);
        assert_eq!(run(&mut m, v, &file), 1);
        assert_eq!(m.mem.bytes(TEXT, 8), file.borrow().body[..8].to_vec());
        assert_eq!(count(&file, FUNCS + dcb::CLOSE), 1);
        closed(&m);

        // An open failure returns 0 for both entries, with no read or close.
        file.borrow_mut().open_fail = true;
        file.borrow_mut().calls.clear();
        m.mem.write(HEADER_BUF, &[0x5A; 0x40]);
        for f in [0x41, 0x42] {
            let v = crate::bios::call(&mut m, 0xA0, f, [PATH, HEADER_BUF, 0, 0]);
            assert_eq!(run(&mut m, v, &file), 0, "A0h:{f:02X}h with no file");
        }
        assert_eq!((count(&file, FUNCS + dcb::READ), count(&file, FUNCS + dcb::CLOSE)), (0, 0));
        assert_eq!(m.mem.bytes(HEADER_BUF, 0x40), vec![0x5A; 0x40]);
        closed(&m);
    }
}
