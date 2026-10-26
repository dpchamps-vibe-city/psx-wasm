//! The BIOS memory card functions (psx-spx "BIOS Memory Card Functions"
//! and "BIOS File Functions" on device `bu`), over [`Card`] images the host
//! stores (`card0.mcr`, `card1.mcr`).
//!
//! - Slot 1 always holds a card: the stored image, or a freshly formatted
//!   one. Slot 2 holds one only when its image exists (the oracle's
//!   default has memory card 2 off).
//! - Low-level sector I/O (`_card_read`, `_card_write`, `_card_info`,
//!   `_card_load`) is asynchronous: one operation completes per vblank, as
//!   the BIOS paces it. Completion delivers the low-level event
//!   (F0000011h) and the higher-level one (F4000001h) with the same spec:
//!   04h done, 100h no card, 2000h new card, 8000h error.
//! - A card is "new" from insertion until a sector write; while it is,
//!   I/O fails with 2000h unless `_new_card` was called first.
//! - File functions on `buXY:` work on the directory directly. Reads and
//!   writes are in 128-byte frames. A file opened with mode bit 15 is
//!   asynchronous: `read`/`write` return 0 once the request is accepted
//!   (libmcrd retries until they do), and it completes one frame per
//!   vblank, delivering event class `fd` and the card events with spec
//!   04h.
//! - Changed images are written back to host storage at vblank.

use crate::machine::{Machine, Sdk};
use psx_devices::memcard::{self, Card};
use std::collections::{BTreeMap, VecDeque};

pub const EV_LOW: u32 = 0xF000_0011;
pub const EV_HIGH: u32 = 0xF400_0001;
pub const SPEC_DONE: u32 = 0x0004;
pub const SPEC_NO_CARD: u32 = 0x0100;
pub const SPEC_NEW: u32 = 0x2000;
pub const SPEC_ERROR: u32 = 0x8000;

/// `_card_status` values.
pub const READY: u32 = 0x01;
pub const BUSY_READ: u32 = 0x02;
pub const BUSY_WRITE: u32 = 0x04;
pub const BUSY_INFO: u32 = 0x08;
pub const FAIL_TIMEOUT: u32 = 0x11;
pub const FAIL_GENERAL: u32 = 0x21;

/// File error numbers (`_get_errno`).
pub const E_NOT_FOUND: u32 = 0x02;
pub const E_BAD_FD: u32 = 0x09;
pub const E_GENERAL: u32 = 0x10;
pub const E_EXISTS: u32 = 0x11;
pub const E_NO_DEVICE: u32 = 0x13;
pub const E_ALIGN: u32 = 0x16;
pub const E_NO_FDS: u32 = 0x18;
pub const E_FULL: u32 = 0x1C;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    Read { frame: u32, dst: u32 },
    Write { frame: u32, data: Vec<u8> },
    Info,
    Load,
    /// An asynchronous file read or write whose bytes already moved; only
    /// its pacing (a frame per vblank) and completion event are pending.
    FileFrame,
    FileDone { fd: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardFile {
    pub slot: usize,
    pub name: String,
    pub size: u32,
    pub pos: u32,
    pub asynchronous: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Cards {
    pub slots: [Option<Card>; 2],
    loaded: bool,
    ignore_new: [bool; 2],
    pub status: [u32; 2],
    pending: VecDeque<(usize, Op)>,
    pub files: BTreeMap<u32, CardFile>,
    pub errno: u32,
    pub auto_format: bool,
    pub find_mode: u32,
    /// firstfile/nextfile: (slot, pattern, next directory index).
    search: Option<(usize, String, usize)>,
}

/// `buXY:NAME` → (slot, name).
pub fn parse_path(path: &str) -> Option<(usize, String)> {
    let lower = path.to_ascii_lowercase();
    let rest = lower.strip_prefix("bu")?;
    let (dev, _) = rest.split_once(':')?;
    let slot = match dev.as_bytes().first()? {
        b'0' => 0,
        b'1' => 1,
        _ => return None,
    };
    let name = &path[path.find(':')? + 1..];
    Some((slot, name.to_string()))
}

/// Port (00h, 10h, …) → slot.
fn slot_of(port: u32) -> usize {
    ((port >> 4) & 1) as usize
}

impl<S: Sdk> Machine<S> {
    /// Load the card images on first use.
    pub fn cards_ready(&mut self) {
        if self.kernel.cards.loaded {
            return;
        }
        self.kernel.cards.loaded = true;
        let c0 = self.host.storage_read("card0.mcr").and_then(|b| Card::from_image(&b)).unwrap_or_else(Card::formatted);
        let c1 = self.host.storage_read("card1.mcr").and_then(|b| Card::from_image(&b));
        self.kernel.cards.slots = [Some(c0), c1];
        self.kernel.cards.status = [READY; 2];
    }

    fn card_event(&mut self, spec: u32) {
        self.deliver_event(EV_LOW, spec);
        self.deliver_event(EV_HIGH, spec);
    }

    /// Queue a low-level operation; false when the slot has no card
    /// (the failure is delivered at once).
    fn card_queue(&mut self, slot: usize, op: Op) -> bool {
        self.cards_ready();
        let busy = match op {
            Op::Read { .. } => BUSY_READ,
            Op::Write { .. } => BUSY_WRITE,
            _ => BUSY_INFO,
        };
        self.kernel.cards.status[slot] = busy;
        self.kernel.cards.pending.push_back((slot, op));
        true
    }

    /// `_card_read(port, sector, dst)`.
    pub fn card_read(&mut self, port: u32, sector: u32, dst: u32) -> u32 {
        if sector > memcard::FRAMES {
            return 0;
        }
        self.card_queue(slot_of(port), Op::Read { frame: sector.min(memcard::FRAMES - 1), dst }) as u32
    }

    /// `_card_write(port, sector, src)`: the frame is taken now.
    pub fn card_write(&mut self, port: u32, sector: u32, src: u32) -> u32 {
        if sector > memcard::FRAMES {
            return 0;
        }
        let mut data = vec![0u8; memcard::FRAME];
        self.mem.read(src, &mut data);
        self.card_queue(slot_of(port), Op::Write { frame: sector.min(memcard::FRAMES - 1), data }) as u32
    }

    pub fn card_info(&mut self, port: u32) -> u32 {
        self.card_queue(slot_of(port), Op::Info) as u32
    }

    pub fn card_load(&mut self, port: u32) -> u32 {
        self.card_queue(slot_of(port), Op::Load) as u32
    }

    /// `card_write_test(port)`: rewrite frame 3Fh as it is, ignoring the
    /// new-card flag (which the write then clears).
    pub fn card_write_test(&mut self, port: u32) -> u32 {
        let slot = slot_of(port);
        self.cards_ready();
        let data = self.kernel.cards.slots[slot].as_ref().map(|c| c.frame(0x3F).to_vec()).unwrap_or_else(|| vec![0; memcard::FRAME]);
        self.kernel.cards.ignore_new[slot] = true;
        self.card_queue(slot, Op::Write { frame: 0x3F, data }) as u32
    }

    pub fn card_new(&mut self, port: u32) {
        self.cards_ready();
        self.kernel.cards.ignore_new[slot_of(port)] = true;
    }

    /// Complete one pending operation, and save changed images.
    pub fn card_vblank(&mut self) {
        if let Some((slot, op)) = self.kernel.cards.pending.pop_front() {
            self.card_complete(slot, op);
        }
        for (i, name) in ["card0.mcr", "card1.mcr"].iter().enumerate() {
            let Some(c) = self.kernel.cards.slots[i].as_mut().filter(|c| c.dirty) else { continue };
            c.dirty = false;
            let data = c.data.clone();
            self.host.storage_write(name, &data);
        }
    }

    fn card_complete(&mut self, slot: usize, op: Op) {
        match op {
            Op::FileFrame => return,
            Op::FileDone { fd } => {
                self.deliver_event(fd, SPEC_DONE);
                self.card_event(SPEC_DONE);
                return;
            }
            _ => {}
        }
        let ignore = std::mem::take(&mut self.kernel.cards.ignore_new[slot]);
        let Some(card) = self.kernel.cards.slots[slot].as_mut() else {
            self.kernel.cards.status[slot] = FAIL_TIMEOUT;
            self.card_event(SPEC_NO_CARD);
            return;
        };
        if card.new_card && !ignore {
            self.kernel.cards.status[slot] = FAIL_GENERAL;
            self.card_event(SPEC_NEW);
            return;
        }
        match op {
            Op::Read { frame, dst } => {
                let data = card.frame(frame).to_vec();
                self.mem.write(dst, &data);
            }
            Op::Write { frame, data } => card.write_frame(frame, &data),
            Op::Info | Op::Load | Op::FileFrame | Op::FileDone { .. } => {}
        }
        self.kernel.cards.status[slot] = READY;
        self.card_event(SPEC_DONE);
    }

    /// `_card_wait(slot)`: finish the slot's pending operations now.
    pub fn card_wait(&mut self, slot: u32) -> u32 {
        let slot = (slot & 1) as usize;
        while let Some(i) = self.kernel.cards.pending.iter().position(|(s, _)| *s == slot) {
            let (s, op) = self.kernel.cards.pending.remove(i).expect("position");
            self.card_complete(s, op);
        }
        self.kernel.cards.status[slot]
    }

    fn card_fail(&mut self, errno: u32) -> u32 {
        self.kernel.cards.errno = errno;
        u32::MAX
    }

    /// `open("buXY:NAME", mode)`.
    pub fn card_open(&mut self, path: &str, mode: u32, fd: u32) -> u32 {
        self.cards_ready();
        let Some((slot, name)) = parse_path(path) else { return self.card_fail(E_NO_DEVICE) };
        let Some(card) = self.kernel.cards.slots[slot].as_mut() else { return self.card_fail(E_GENERAL) };
        let file = if mode & 0x200 != 0 {
            let blocks = (mode >> 16) as usize;
            if card.find(&name).is_some() {
                return self.card_fail(E_EXISTS);
            }
            match card.create(&name, blocks.max(1)) {
                Some(f) => f,
                None => return self.card_fail(E_FULL),
            }
        } else {
            match card.find(&name) {
                Some(f) => f,
                None => return self.card_fail(E_NOT_FOUND),
            }
        };
        self.kernel.cards.files.insert(fd, CardFile { slot, name: file.name, size: file.size, pos: 0, asynchronous: mode & 0x8000 != 0 });
        fd
    }

    /// `read`/`write` on a card file; `to_card` writes.
    pub fn card_io(&mut self, fd: u32, buf: u32, len: u32, to_card: bool) -> u32 {
        let Some(f) = self.kernel.cards.files.get(&fd).cloned() else { return self.card_fail(E_BAD_FD) };
        if len % memcard::FRAME as u32 != 0 || f.pos % memcard::FRAME as u32 != 0 || f.pos >= f.size {
            return self.card_fail(E_ALIGN);
        }
        let n = len.min(f.size - f.pos);
        let Some(card) = self.kernel.cards.slots[f.slot].as_mut() else { return self.card_fail(E_GENERAL) };
        let Some(info) = card.find(&f.name) else { return self.card_fail(E_NOT_FOUND) };
        let mut off = 0;
        while off < n {
            let Some(frame) = card.file_frame(&info, f.pos + off) else { break };
            if to_card {
                let mut data = vec![0u8; memcard::FRAME];
                self.mem.read(buf + off, &mut data);
                card.write_frame(frame, &data);
            } else {
                let data = card.frame(frame).to_vec();
                self.mem.write(buf + off, &data);
            }
            off += memcard::FRAME as u32;
        }
        let Some(f) = self.kernel.cards.files.get_mut(&fd) else { return off };
        f.pos += off;
        if !f.asynchronous {
            return off;
        }
        let slot = f.slot;
        for _ in 1..(off / memcard::FRAME as u32).max(1) {
            self.kernel.cards.pending.push_back((slot, Op::FileFrame));
        }
        self.kernel.cards.pending.push_back((slot, Op::FileDone { fd }));
        0
    }

    /// `firstfile("buXY:PATTERN", direntry)` and `nextfile(direntry)`.
    pub fn card_find(&mut self, pattern: Option<&str>, dir: u32) -> u32 {
        self.cards_ready();
        if let Some(p) = pattern {
            let Some((slot, name)) = parse_path(p) else { return 0 };
            self.kernel.cards.search = Some((slot, name, 0));
        }
        let Some((slot, pat, from)) = self.kernel.cards.search.clone() else { return 0 };
        let deleted = self.kernel.cards.find_mode == 1;
        let Some(card) = self.kernel.cards.slots[slot].as_ref() else { return 0 };
        let files = card.files();
        let hit = files.iter().enumerate().skip(from).find(|(_, f)| f.deleted == deleted && memcard::name_matches(&pat, &f.name));
        let Some((i, f)) = hit else {
            self.kernel.cards.search = Some((slot, pat, files.len()));
            return 0;
        };
        let mut e = vec![0u8; 0x28];
        let n = f.name.as_bytes();
        e[..n.len().min(0x13)].copy_from_slice(&n[..n.len().min(0x13)]);
        e[0x14..0x18].copy_from_slice(&(if f.deleted { 0xA0u32 } else { 0x50 }).to_le_bytes());
        e[0x18..0x1C].copy_from_slice(&f.size.to_le_bytes());
        e[0x20..0x24].copy_from_slice(&((f.blocks[0] * 64) as u32).to_le_bytes());
        self.mem.write(dir, &e);
        self.kernel.cards.search = Some((slot, pat, i + 1));
        dir
    }

    /// erase, undelete, rename, format on `bu`; 1 ok, 0 failed.
    pub fn card_erase(&mut self, path: &str, undelete: bool) -> u32 {
        self.cards_ready();
        let Some((slot, name)) = parse_path(path) else { return 0 };
        let Some(card) = self.kernel.cards.slots[slot].as_mut() else { return 0 };
        let ok = if undelete { card.undelete(&name) } else { card.delete(&name) };
        if !ok {
            self.kernel.cards.errno = E_NOT_FOUND;
        }
        ok as u32
    }

    pub fn card_rename(&mut self, from: &str, to: &str) -> u32 {
        self.cards_ready();
        let (Some((slot, a)), Some((slot2, b))) = (parse_path(from), parse_path(to)) else { return 0 };
        if slot != slot2 {
            self.kernel.cards.errno = 0x12;
            return 0;
        }
        let Some(card) = self.kernel.cards.slots[slot].as_mut() else { return 0 };
        card.rename(&a, &b) as u32
    }

    pub fn card_format(&mut self, path: &str) -> u32 {
        self.cards_ready();
        let Some((slot, _)) = parse_path(path) else { return 0 };
        match self.kernel.cards.slots[slot].as_mut() {
            Some(c) => {
                c.format();
                c.new_card = false;
                1
            }
            None => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::machine;

    #[test]
    fn raw_io_is_async_and_new_card_needs_accepting() {
        let mut m = machine();
        let (buf, out) = (0x8001_0000, 0x8001_0100);
        m.mem.write(buf, &[0x5A; 128]);
        assert_eq!(m.card_write(0x00, 0x40, buf), 1);
        assert_eq!(m.kernel.cards.status[0], super::BUSY_WRITE);
        m.card_vblank();
        assert_eq!(m.kernel.cards.status[0], super::FAIL_GENERAL, "new card refuses I/O");
        m.card_new(0x00);
        m.card_write(0x00, 0x40, buf);
        m.card_vblank();
        assert_eq!(m.kernel.cards.status[0], super::READY);
        m.card_read(0x00, 0x40, out);
        m.card_vblank();
        let mut back = [0u8; 128];
        m.mem.read(out, &mut back);
        assert_eq!(back, [0x5A; 128]);
        // Slot 2 is empty.
        m.card_info(0x10);
        m.card_vblank();
        assert_eq!(m.kernel.cards.status[1], super::FAIL_TIMEOUT);
    }

    #[test]
    fn files_on_bu00() {
        let mut m = machine();
        let name = "bu00:BASLUS-01115TEST";
        assert_eq!(m.card_open(name, 0x200 | 2 << 16 | 3, 3), 3);
        m.mem.write(0x8001_0000, &[7; 256]);
        assert_eq!(m.card_io(3, 0x8001_0000, 256, true), 256);
        m.kernel.cards.files.remove(&3);
        assert_eq!(m.card_open(name, 1, 4), 4);
        assert_eq!(m.card_io(4, 0x8002_0000, 128, false), 128);
        assert_eq!(m.mem.r8(0x8002_007F), 7);
        assert_eq!(m.card_find(Some("bu00:BASLUS*"), 0x8003_0000), 0x8003_0000);
        assert_eq!(m.mem.r32(0x8003_0018), 0x4000);
        assert_eq!(m.card_find(None, 0x8003_0000), 0);
        assert_eq!(m.card_open(name, 0x200 | 1 << 16, 5), u32::MAX, "exists");
        assert_eq!(m.kernel.cards.errno, super::E_EXISTS);
        assert_eq!(m.card_erase(name, false), 1);
        assert_eq!(m.card_open(name, 1, 5), u32::MAX);
        assert!(m.kernel.cards.slots[0].as_ref().unwrap().valid());
    }
}
