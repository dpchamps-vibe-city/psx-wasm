//! The BIOS memory card functions (psx-spx "BIOS Memory Card Functions"
//! and "BIOS File Functions" on device `bu`), over [`Card`] images the host
//! stores (`card0.mcr`, `card1.mcr`).
//!
//! - Slot 1 always holds a card: the stored image, or a freshly formatted
//!   one. Slot 2 holds one only when its image exists (the oracle's
//!   default has memory card 2 off).
//! - Every card access the BIOS makes delivers the low-level driver's
//!   completion event (F0000011h): 04h done, 100h no card, 2000h new
//!   card, 8000h error. The higher-level event (F4000001h, same specs)
//!   comes only from the A-table functions (`_card_info`, `_card_load`,
//!   `card_write_test`) and asynchronous file I/O, as in OpenBIOS
//!   (`sio0/driver.c`, `card/backupunit.c`). libmcrd waits on exactly
//!   this split: on F0000011h after `_card_read`/`_card_write` and after
//!   a `firstfile` that finds nothing, on F4000001h after `_card_info`,
//!   `_card_load` and file reads and writes.
//! - Sector I/O (`_card_read`, `_card_write`, `_card_info`, `_card_load`)
//!   is asynchronous: one operation completes per vblank, as the BIOS
//!   paces it.
//! - A card is "new" from insertion until a sector write; while it is,
//!   I/O fails with 2000h unless `_new_card` was called first.
//! - File functions on `buXY:` work on the directory directly, after the
//!   check the BIOS makes first (`card_check`: a synchronous
//!   read of frame 0, whose event is delivered during the call). Reads
//!   and writes are in 128-byte frames. A file opened with mode bit 15 is
//!   asynchronous: `read`/`write` return 0 once the request is accepted
//!   (libmcrd retries until they do), and it completes one frame per
//!   vblank, delivering event class `fd` and F4000001h with spec 04h at
//!   the end.
//! - Changed images are written back to host storage at vblank.

use crate::files::{fcb, fcb_at};
use crate::machine::{Flow, Machine, Sdk, Wait};
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
    /// `high`: `card_write_test` (A-table), which also delivers F4000001h.
    Write { frame: u32, data: Vec<u8>, high: bool },
    /// `high`: `_card_info` (A-table), which also delivers F4000001h.
    Info { high: bool },
    Load,
    /// One frame of an asynchronous file read or write, moved when the
    /// operation completes. `buf` is where that frame goes or comes from;
    /// `id` identifies the file open that requested it.
    FileFrame { fd: u32, id: u64, frame: u32, buf: u32, to_card: bool, last: bool },
    /// The final card-info stage of an asynchronous write: its file's
    /// completion event, then the higher-level card event.
    FileDone { fd: u32, id: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardFile {
    /// Identifies this open across fd reuse.
    pub id: u64,
    /// The exact device id the file was opened on (00h or 10h).
    pub device: u32,
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
    /// `InitCARD2` ran before (its return value).
    initialized: bool,
    ignore_new: [bool; 2],
    pub status: [u32; 2],
    /// The slot whose card operation completed last.
    last: Option<usize>,
    /// `mcGetLastDevice`: the requested device id (00h or 10h).
    last_device: Option<u32>,
    pending: VecDeque<(usize, u32, Op)>,
    next_file_id: u64,
    pub files: BTreeMap<u32, CardFile>,
    pub errno: u32,
    pub auto_format: u32,
    pub find_mode: u32,
    /// firstfile/nextfile: (slot, pattern, next directory index).
    search: Option<(usize, String, usize)>,
}

/// `buXY:NAME` → (slot, device id, name).
pub fn parse_path(path: &str) -> Option<(usize, u32, String)> {
    let lower = path.to_ascii_lowercase();
    let rest = lower.strip_prefix("bu")?;
    let (dev, _) = rest.split_once(':')?;
    let device = u32::from_str_radix(dev, 16).ok()?;
    let name = &path[path.find(':')? + 1..];
    Some((slot_of(device), device, name.to_string()))
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

    /// The driver's completion event, and the higher-level one when an
    /// A-table function or file I/O asked for the access.
    fn card_event(&mut self, spec: u32, high: bool) {
        // The completion callback delivers the higher-level event before
        // the driver's low-level event (OpenBIOS `driver.c`).
        if high {
            self.deliver_event(EV_HIGH, spec);
        }
        self.deliver_event(EV_LOW, spec);
    }

    /// The check the BIOS's `bu` functions make before using the
    /// directory (OpenBIOS `buDevInit`): a synchronous read of frame 0,
    /// its event delivered during the call. A new card fails that read
    /// (2000h); the BIOS then reloads the card, accepting it (its write
    /// of frame 3Fh clears the flag), and the rereads complete (04h).
    /// False when the slot is empty (100h).
    fn card_check(&mut self, slot: usize, device: u32) -> bool {
        self.cards_ready();
        self.kernel.cards.last = Some(slot);
        self.kernel.cards.last_device = Some(device);
        let ignore = std::mem::take(&mut self.kernel.cards.ignore_new[slot]);
        if self.kernel.cards.slots[slot].is_none() {
            self.kernel.cards.status[slot] = FAIL_TIMEOUT;
            self.card_event(SPEC_NO_CARD, false);
            return false;
        }
        // The low-level read reports the card-change flag first; only then
        // does buDevInit inspect the MC header and maybe format.
        let new_card = self.kernel.cards.slots[slot].as_ref().is_some_and(|c| c.new_card) && !ignore;
        if new_card {
            let card = self.kernel.cards.slots[slot].as_mut().expect("checked");
            card.new_card = false;
            self.card_event(SPEC_NEW, false);
        }
        let bad_id = !self.kernel.cards.slots[slot].as_ref().is_some_and(|c| c.frame(0).starts_with(b"MC"));
        if bad_id {
            if self.kernel.cards.auto_format != 0 {
                let card = self.kernel.cards.slots[slot].as_mut().expect("checked");
                card.format();
                card.new_card = false;
            } else {
                if !new_card {
                    self.card_event(SPEC_DONE, false);
                }
                self.kernel.cards.status[slot] = FAIL_GENERAL;
                return false;
            }
        }
        self.kernel.cards.status[slot] = READY;
        self.card_event(SPEC_DONE, false);
        true
    }

    /// Queue a low-level operation; false while the slot's last one is
    /// still running (OpenBIOS: flags bit 0 clear). A missing card fails
    /// later, with its event.
    fn card_queue(&mut self, slot: usize, device: u32, op: Op) -> bool {
        self.cards_ready();
        if self.kernel.cards.status[slot] & READY == 0 {
            return false;
        }
        let busy = match op {
            Op::Read { .. } => BUSY_READ,
            Op::Write { .. } => BUSY_WRITE,
            _ => BUSY_INFO,
        };
        self.kernel.cards.status[slot] = busy;
        self.kernel.cards.pending.push_back((slot, device, op));
        true
    }

    /// `_card_read(port, sector, dst)`.
    /// OpenBIOS's check: 400h is accepted, and the card rejects it.
    pub fn card_read(&mut self, port: u32, sector: u32, dst: u32) -> u32 {
        if sector > memcard::FRAMES {
            return 0;
        }
        self.card_queue(slot_of(port), port, Op::Read { frame: sector, dst }) as u32
    }

    /// `_card_write(port, sector, src)`: the frame is taken now.
    pub fn card_write(&mut self, port: u32, sector: u32, src: u32) -> u32 {
        if sector > memcard::FRAMES {
            return 0;
        }
        let mut data = vec![0u8; memcard::FRAME];
        self.mem.read(src, &mut data);
        self.card_queue(slot_of(port), port, Op::Write { frame: sector, data, high: false }) as u32
    }

    pub fn card_info(&mut self, port: u32) -> u32 {
        self.card_queue(slot_of(port), port, Op::Info { high: true }) as u32
    }

    /// `_card_info_subfunc(port)`: the lower-level `_card_info` operation,
    /// whose completion reports only the driver's event.
    pub fn card_info_subfunc(&mut self, port: u32) -> u32 {
        self.card_queue(slot_of(port), port, Op::Info { high: false }) as u32
    }

    pub fn card_load(&mut self, port: u32) -> u32 {
        self.card_queue(slot_of(port), port, Op::Load) as u32
    }

    /// `card_write_test(port)`: rewrite frame 3Fh as it is, ignoring the
    /// new-card flag (which the write then clears).
    pub fn card_write_test(&mut self, port: u32) -> u32 {
        let slot = slot_of(port);
        self.cards_ready();
        let data = self.kernel.cards.slots[slot].as_ref().map(|c| c.frame(0x3F).to_vec()).unwrap_or_else(|| vec![0; memcard::FRAME]);
        let queued = self.card_queue(slot, port, Op::Write { frame: 0x3F, data, high: true });
        self.kernel.cards.ignore_new[slot] |= queued;
        queued as u32
    }

    /// `InitCARD2(pad_enable)`: both ports ready, the pads read with the
    /// cards or not; whether it ran before (OpenBIOS `initCard`).
    pub fn card_init(&mut self, pad_enable: u32) -> u32 {
        self.cards_ready();
        self.kernel.cards.status = [READY; 2];
        self.kernel.pad_enable = pad_enable != 0;
        std::mem::replace(&mut self.kernel.cards.initialized, true) as u32
    }

    pub fn card_new(&mut self, port: u32) {
        self.cards_ready();
        self.kernel.cards.ignore_new[slot_of(port)] = true;
    }

    /// Complete one pending operation, and save changed images.
    pub fn card_vblank(&mut self) {
        if let Some((slot, device, op)) = self.kernel.cards.pending.pop_front() {
            self.card_complete(slot, device, op);
        }
        for (i, name) in ["card0.mcr", "card1.mcr"].iter().enumerate() {
            let Some(c) = self.kernel.cards.slots[i].as_mut().filter(|c| c.dirty) else { continue };
            c.dirty = false;
            let data = c.data.clone();
            self.host.storage_write(name, &data);
        }
    }

    fn op_file(op: &Op) -> Option<(u32, u64)> {
        match op {
            Op::FileFrame { fd, id, .. } | Op::FileDone { fd, id } => Some((*fd, *id)),
            _ => None,
        }
    }

    /// The active asynchronous file operation for `slot`, if one is
    /// pending: (fd, open identity).
    fn card_file_active(&self, slot: usize) -> Option<(u32, u64)> {
        self.kernel.cards.pending.iter().find_map(|(s, _, op)| (*s == slot).then(|| Self::op_file(op)).flatten())
    }

    /// Whether `fd` has an asynchronous operation still in the queue.
    pub fn card_file_pending(&self, fd: u32) -> bool {
        self.kernel.cards.pending.iter().any(|(_, _, op)| matches!(Self::op_file(op), Some((f, _)) if f == fd))
    }

    /// Native close policy: psxclose releases the fd even when the device
    /// close fails, so drop the old open's queued work. This is not a retail
    /// implementation: it keeps a reused fd from receiving stale frames.
    /// The completed offset stays in the FCB; no completion is fabricated.
    pub fn card_file_closed(&mut self, fd: u32) {
        if let Some(f) = self.kernel.cards.files.remove(&fd) {
            self.card_file_cancel(f.slot, fd, f.id);
        }
    }

    /// Drop this open's queued work without touching its buffer, image or
    /// FCB. The identity check keeps a reopened fd's frames intact.
    fn card_file_cancel(&mut self, slot: usize, fd: u32, id: u64) {
        self.kernel.cards.pending.retain(|(s, _, op)| !(*s == slot && Self::op_file(op) == Some((fd, id))));
        if !self.kernel.cards.pending.iter().any(|(s, _, _)| *s == slot) {
            self.kernel.cards.status[slot] = READY;
        }
    }

    /// Record an asynchronous file error where the guest looks for it: the
    /// FCB's errno and `_get_errno`.
    fn card_file_errno(&mut self, fd: u32, errno: u32) {
        self.kernel.cards.errno = errno;
        self.kernel.files.errno = errno;
        self.mem.w32(fcb_at(fd) + fcb::ERRNO, errno);
    }

    /// An asynchronous file operation failed: cancel its remaining steps,
    /// keep the partial offset, and report the driver's and higher-level
    /// events. `low` is false for the bufs callbacks, whose driver low
    /// event already happened and must not be repeated.
    fn card_file_fail(&mut self, slot: usize, fd: u32, id: u64, errno: u32, spec: u32, low: bool) {
        self.card_file_cancel(slot, fd, id);
        self.kernel.cards.status[slot] = if spec == SPEC_NO_CARD { FAIL_TIMEOUT } else { FAIL_GENERAL };
        if self.kernel.cards.files.get(&fd).is_some_and(|f| f.id == id) {
            self.card_file_errno(fd, errno);
        }
        self.deliver_event(EV_HIGH, spec);
        if low {
            self.deliver_event(EV_LOW, spec);
        }
    }

    /// End a write's card-info stage: fd event, high event, then the
    /// info completion's low event. A reused fd is not treated as success.
    fn card_file_done(&mut self, slot: usize, fd: u32, id: u64) {
        let Some(f) = self.kernel.cards.files.get(&fd) else {
            self.card_file_cancel(slot, fd, id);
            return;
        };
        if f.id != id {
            self.card_file_cancel(slot, fd, id);
            return;
        }
        let ignore = std::mem::take(&mut self.kernel.cards.ignore_new[slot]);
        let card_state = self.kernel.cards.slots[slot].as_ref().map(|c| c.new_card && !ignore);
        if card_state.is_none() {
            self.card_file_fail(slot, fd, id, E_GENERAL, SPEC_NO_CARD, true);
            return;
        }
        if card_state == Some(true) {
            self.card_file_fail(slot, fd, id, E_GENERAL, SPEC_NEW, true);
            return;
        }
        self.kernel.cards.status[slot] = READY;
        self.deliver_event(fd, SPEC_DONE);
        self.deliver_event(EV_HIGH, SPEC_DONE);
        self.deliver_event(EV_LOW, SPEC_DONE);
    }

    fn card_complete(&mut self, slot: usize, device: u32, op: Op) {
        self.kernel.cards.last = Some(slot);
        self.kernel.cards.last_device = Some(device);
        match op {
            Op::FileFrame { fd, id, frame, buf, to_card, last } => {
                let Some(f) = self.kernel.cards.files.get(&fd).cloned() else {
                    self.card_file_cancel(slot, fd, id);
                    return;
                };
                if f.id != id {
                    self.card_file_cancel(slot, fd, id);
                    return;
                }
                let ignore = std::mem::take(&mut self.kernel.cards.ignore_new[slot]);
                let new_card = self.kernel.cards.slots[f.slot].as_ref().is_some_and(|c| c.new_card) && !ignore;
                if new_card {
                    self.card_file_fail(slot, fd, id, E_GENERAL, SPEC_NEW, true);
                    return;
                }
                let Some(card) = self.kernel.cards.slots[f.slot].as_mut() else {
                    self.card_file_fail(slot, fd, id, E_GENERAL, SPEC_NO_CARD, true);
                    return;
                };
                if to_card {
                    let mut data = vec![0u8; memcard::FRAME];
                    self.mem.read(buf, &mut data);
                    card.write_frame(frame, &data);
                } else {
                    let data = card.frame(frame).to_vec();
                    self.mem.write(buf, &data);
                }
                if let Some(f) = self.kernel.cards.files.get_mut(&fd) {
                    f.pos += memcard::FRAME as u32;
                    self.mem.w32(fcb_at(fd) + fcb::OFFSET, f.pos);
                }
                if last && !to_card {
                    // Read final: high, fd, then the driver's low event.
                    self.kernel.cards.status[slot] = READY;
                    self.deliver_event(EV_HIGH, SPEC_DONE);
                    self.deliver_event(fd, SPEC_DONE);
                    self.deliver_event(EV_LOW, SPEC_DONE);
                } else {
                    self.card_event(SPEC_DONE, false);
                }
                return;
            }
            Op::FileDone { fd, id } => {
                self.card_file_done(slot, fd, id);
                return;
            }
            _ => {}
        }
        let high = matches!(op, Op::Info { high: true } | Op::Load | Op::Write { high: true, .. });
        let ignore = std::mem::take(&mut self.kernel.cards.ignore_new[slot]);
        let Some(card) = self.kernel.cards.slots[slot].as_mut() else {
            self.kernel.cards.status[slot] = FAIL_TIMEOUT;
            self.card_event(SPEC_NO_CARD, high);
            return;
        };
        if card.new_card && !ignore {
            self.kernel.cards.status[slot] = FAIL_GENERAL;
            self.card_event(SPEC_NEW, high);
            return;
        }
        if let Op::Read { frame, .. } | Op::Write { frame, .. } = op {
            // The card answers a sector past its last with FFFFh, and the
            // driver fails the operation.
            if frame >= memcard::FRAMES {
                self.kernel.cards.status[slot] = FAIL_GENERAL;
                self.card_event(SPEC_ERROR, high);
                return;
            }
        }
        match op {
            Op::Read { frame, dst } => {
                let data = card.frame(frame).to_vec();
                self.mem.write(dst, &data);
            }
            Op::Write { frame, data, .. } => card.write_frame(frame, &data),
            Op::Info { .. } | Op::Load | Op::FileFrame { .. } | Op::FileDone { .. } => {}
        }
        self.kernel.cards.status[slot] = READY;
        self.card_event(SPEC_DONE, high);
    }

    /// `_card_wait(slot)`: wait for a non-busy status. Pending operations
    /// complete one per vblank, driven by the virtual scheduler.
    pub fn card_wait(&mut self, slot: u32) -> Flow<S> {
        let slot = (slot & 1) as usize;
        self.cards_ready();
        self.card_wait_ready(slot)
    }

    fn card_wait_ready(&mut self, slot: usize) -> Flow<S> {
        let status = self.kernel.cards.status[slot];
        if !matches!(status, BUSY_READ | BUSY_WRITE | BUSY_INFO) {
            return Flow::ret(status);
        }
        Flow::Block { until: Wait::Vblanks(self.vblanks + 1), then: Box::new(move |m| m.card_wait_ready(slot)) }
    }

    fn card_slot_busy(&self, slot: usize) -> bool {
        matches!(self.kernel.cards.status[slot], BUSY_READ | BUSY_WRITE | BUSY_INFO)
    }

    /// `_card_chan()`: `mcGetLastDevice`, the exact requested device id
    /// (normally 00h or 10h) of the most recent completion.
    pub fn card_chan(&self) -> u32 {
        self.kernel.cards.last_device.unwrap_or(0)
    }

    /// A(A7h..AAh, AEh) `bufs_cb_*`: the BIOS's completion callbacks.
    /// Native completion already performed A7's state advancement; A7 must
    /// not consume or replay a scheduled stage here. A8/A9/AA/AE cancel
    /// the active file operation with the spec psx-spx assigns them. Their
    /// effects on pending Info/Load/write_test operations remain partial.
    pub fn card_bufs_cb(&mut self, which: u32) -> u32 {
        let slot = self.kernel.cards.last.unwrap_or(0);
        if which == 0 {
            return self.kernel.cards.status[slot];
        }
        let Some((fd, id)) = self.card_file_active(slot) else { return self.kernel.cards.status[slot] };
        let spec = match which {
            1 | 4 => SPEC_ERROR,
            2 => SPEC_NO_CARD,
            3 => SPEC_NEW,
            _ => return self.kernel.cards.status[slot],
        };
        self.card_file_fail(slot, fd, id, E_GENERAL, spec, false);
        self.kernel.cards.status[slot]
    }

    fn card_fail(&mut self, errno: u32) -> u32 {
        self.kernel.cards.errno = errno;
        u32::MAX
    }

    /// `open("buXY:NAME", mode)`.
    pub fn card_open(&mut self, path: &str, mode: u32, fd: u32) -> u32 {
        self.cards_ready();
        let Some((slot, device, name)) = parse_path(path) else { return self.card_fail(E_NO_DEVICE) };
        if self.card_slot_busy(slot) {
            return self.card_fail(E_GENERAL);
        }
        // An asynchronous open skips the check.
        if mode & 0x8000 == 0 && !self.card_check(slot, device) {
            return self.card_fail(E_GENERAL);
        }
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
        let id = self.kernel.cards.next_file_id;
        self.kernel.cards.next_file_id = id.wrapping_add(1);
        self.kernel.cards.files.insert(fd, CardFile { id, device, slot, name: file.name, size: file.size, pos: 0, asynchronous: mode & 0x8000 != 0 });
        fd
    }

    /// `read`/`write` on a card file; `to_card` writes. An asynchronous
    /// request only queues its frames: each moves when it completes, one
    /// per vblank.
    pub fn card_io(&mut self, fd: u32, buf: u32, len: u32, to_card: bool) -> u32 {
        let Some(f) = self.kernel.cards.files.get(&fd).cloned() else { return self.card_fail(E_BAD_FD) };
        if self.card_slot_busy(f.slot) {
            return self.card_fail(E_GENERAL);
        }
        if len % memcard::FRAME as u32 != 0 || f.pos % memcard::FRAME as u32 != 0 || f.pos >= f.size {
            return self.card_fail(E_ALIGN);
        }
        let n = len.min(f.size - f.pos);
        let Some(info) = self.kernel.cards.slots[f.slot].as_ref().and_then(|c| c.find(&f.name)) else {
            return self.card_fail(E_NOT_FOUND);
        };
        let mut frames = Vec::new();
        let mut off = 0;
        while off < n {
            let Some(frame) = self.kernel.cards.slots[f.slot].as_ref().and_then(|c| c.file_frame(&info, f.pos + off)) else { break };
            frames.push((frame, off));
            off += memcard::FRAME as u32;
        }
        if !f.asynchronous {
            let Some(card) = self.kernel.cards.slots[f.slot].as_mut() else { return self.card_fail(E_GENERAL) };
            for &(frame, at) in &frames {
                if to_card {
                    let mut data = vec![0u8; memcard::FRAME];
                    self.mem.read(buf + at, &mut data);
                    card.write_frame(frame, &data);
                } else {
                    let data = card.frame(frame).to_vec();
                    self.mem.write(buf + at, &data);
                }
            }
            if let Some(f) = self.kernel.cards.files.get_mut(&fd) {
                f.pos += off;
            }
            self.kernel.cards.last = Some(f.slot);
            self.kernel.cards.last_device = Some(f.device);
            for _ in &frames {
                self.card_event(SPEC_DONE, false);
            }
            return off;
        }
        let slot = f.slot;
        self.kernel.cards.last = Some(slot);
        self.kernel.cards.last_device = Some(f.device);
        if frames.is_empty() {
            // OpenBIOS's async read/write early path reports completion
            // without a file-handle event.
            self.deliver_event(EV_HIGH, SPEC_DONE);
            return 0;
        }
        self.kernel.cards.status[slot] = if to_card { BUSY_WRITE } else { BUSY_READ };
        let count = frames.len();
        for (i, (frame, at)) in frames.into_iter().enumerate() {
            let last = i + 1 == count;
            self.kernel.cards.pending.push_back((slot, f.device, Op::FileFrame { fd, id: f.id, frame, buf: buf + at, to_card, last }));
        }
        if to_card {
            // A write's final sector is followed by a card-info completion.
            self.kernel.cards.pending.push_back((slot, f.device, Op::FileDone { fd, id: f.id }));
        }
        0
    }

    /// `firstfile("buXY:PATTERN", direntry)` and `nextfile(direntry)`.
    pub fn card_find(&mut self, pattern: Option<&str>, dir: u32) -> u32 {
        self.cards_ready();
        if let Some(p) = pattern {
            let Some((slot, device, name)) = parse_path(p) else { return 0 };
            if self.card_slot_busy(slot) {
                return 0;
            }
            self.kernel.cards.search = Some((slot, name, 0));
            if !self.card_check(slot, device) {
                return 0;
            }
        }
        let Some((slot, pat, from)) = self.kernel.cards.search.clone() else { return 0 };
        if self.card_slot_busy(slot) {
            return 0;
        }
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
        let Some((slot, device, name)) = parse_path(path) else { return 0 };
        if self.card_slot_busy(slot) {
            self.kernel.cards.errno = E_GENERAL;
            return 0;
        }
        if !self.card_check(slot, device) {
            self.kernel.cards.errno = E_GENERAL;
            return 0;
        }
        let Some(card) = self.kernel.cards.slots[slot].as_mut() else { return 0 };
        let ok = if undelete { card.undelete(&name) } else { card.delete(&name) };
        if !ok {
            self.kernel.cards.errno = E_NOT_FOUND;
        }
        ok as u32
    }

    pub fn card_rename(&mut self, from: &str, to: &str) -> u32 {
        self.cards_ready();
        let (Some((slot, device, a)), Some((slot2, _, b))) = (parse_path(from), parse_path(to)) else { return 0 };
        if slot != slot2 {
            self.kernel.cards.errno = 0x12;
            return 0;
        }
        if self.card_slot_busy(slot) {
            self.kernel.cards.errno = E_GENERAL;
            return 0;
        }
        if !self.card_check(slot, device) {
            self.kernel.cards.errno = E_GENERAL;
            return 0;
        }
        let Some(card) = self.kernel.cards.slots[slot].as_mut() else { return 0 };
        card.rename(&a, &b) as u32
    }

    pub fn card_format(&mut self, path: &str) -> u32 {
        self.cards_ready();
        let Some((slot, device, _)) = parse_path(path) else { return 0 };
        if self.card_slot_busy(slot) {
            return 0;
        }
        // Its writes accept a new card.
        self.kernel.cards.last = Some(slot);
        self.kernel.cards.last_device = Some(device);
        match self.kernel.cards.slots[slot].as_mut() {
            Some(c) => {
                c.format();
                c.new_card = false;
                self.card_event(SPEC_DONE, false);
                1
            }
            None => {
                self.card_event(SPEC_NO_CARD, false);
                0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::machine;
    use crate::{bios, tables, Flow, GuestCall};
    use crate::testing::NoSdk;
    use psx_devices::memcard::Card;

    fn ret(f: Result<Flow<NoSdk>, String>) -> u32 {
        match f.unwrap() {
            Flow::Return(v) => v as u32,
            Flow::Block { .. } | Flow::Call { .. } => panic!("not a return"),
        }
    }

    /// Drive a flow the way the runtime does: advance to each event the
    /// wait needs.
    fn finish(m: &mut crate::Machine<NoSdk>, mut flow: Flow<NoSdk>) -> u32 {
        loop {
            match flow {
                Flow::Return(v) => return v as u32,
                Flow::Block { until, then } => {
                    let mut steps = 0;
                    while !m.holds(until) {
                        let next = m.clock.next_time().expect("a vblank");
                        m.step(next);
                        steps += 1;
                        assert!(steps < 10, "wait did not complete");
                    }
                    flow = then(m);
                }
                Flow::Call { .. } => panic!("unexpected guest call"),
            }
        }
    }

    #[test]
    fn raw_io_is_async_and_new_card_needs_accepting() {
        let mut m = machine();
        let (buf, out) = (0x8001_0000, 0x8001_0100);
        m.mem.write(buf, &[0x5A; 128]);
        assert_eq!(m.card_write(0x00, 0x40, buf), 1);
        assert_eq!(m.kernel.cards.status[0], super::BUSY_WRITE);
        assert_eq!(m.card_read(0x00, 0x40, out), 0, "busy");
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
        // Sector 400h reaches the card, which rejects it; 401h doesn't.
        assert_eq!(m.card_read(0x00, 0x400, out), 1);
        m.card_vblank();
        assert_eq!(m.kernel.cards.status[0], super::FAIL_GENERAL);
        assert_eq!(m.card_read(0x00, 0x401, out), 0);
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

    /// Ready-mode events for each card spec on `class`, by spec.
    fn card_events(m: &mut crate::Machine<crate::testing::NoSdk>, class: u32) -> Vec<(u32, u32)> {
        [super::SPEC_DONE, super::SPEC_NO_CARD, super::SPEC_NEW, super::SPEC_ERROR]
            .into_iter()
            .map(|spec| {
                let h = m.open_event(class, spec, 0x2000, 0);
                m.set_ev_status(h, crate::kernel::EV_ENABLED);
                (spec, h)
            })
            .collect()
    }

    fn ready(m: &crate::Machine<crate::testing::NoSdk>, evs: &[(u32, u32)]) -> Vec<u32> {
        evs.iter().filter(|(_, h)| m.event_ready(*h)).map(|(s, _)| *s).collect()
    }

    /// libmcrd's MemCardGetDirentry waits for a low-level event after a
    /// firstfile that finds nothing: the BIOS's check of the card
    /// delivers it during the call. Raw sector I/O delivers only the
    /// low-level event.
    #[test]
    fn bu_functions_deliver_the_low_level_event() {
        let mut m = machine();
        let low = card_events(&mut m, super::EV_LOW);
        // A new card: the read fails (2000h), the BIOS accepts it, and
        // the reread completes (04h).
        assert_eq!(m.card_find(Some("bu00:BASLUS*"), 0x8003_0000), 0);
        assert_eq!(ready(&m, &low), [super::SPEC_DONE, super::SPEC_NEW]);
        assert!(!m.kernel.cards.slots[0].as_ref().unwrap().new_card);
        let mut m = machine();
        let low = card_events(&mut m, super::EV_LOW);
        m.cards_ready();
        m.kernel.cards.slots[0].as_mut().unwrap().new_card = false;
        assert_eq!(m.card_find(Some("bu00:*"), 0x8003_0000), 0);
        assert_eq!(ready(&m, &low), [super::SPEC_DONE]);
        // Slot 2 is empty.
        let low = card_events(&mut m, super::EV_LOW);
        assert_eq!(m.card_find(Some("bu10:*"), 0x8003_0000), 0);
        assert_eq!(ready(&m, &low), [super::SPEC_NO_CARD]);
        assert_eq!(m.card_open("bu10:X", 1, 3), u32::MAX);
        // An asynchronous open doesn't check; sector I/O is low-level only.
        let low = card_events(&mut m, super::EV_LOW);
        let high = card_events(&mut m, super::EV_HIGH);
        assert_eq!(m.card_open("bu00:X", 0x8001, 3), u32::MAX);
        assert_eq!(ready(&m, &low), []);
        m.card_read(0x00, 0x40, 0x8001_0000);
        m.card_vblank();
        assert_eq!(ready(&m, &low), [super::SPEC_DONE]);
        assert_eq!(ready(&m, &high), []);
        m.card_info(0x00);
        m.card_vblank();
        assert_eq!(ready(&m, &high), [super::SPEC_DONE]);
    }

    /// `_card_wait` does not move the operation itself: scheduled vblanks
    /// complete it, and the wait returns the resulting status.
    #[test]
    fn card_wait_waits_for_scheduled_completions() {
        let mut m = machine();
        let low = card_events(&mut m, super::EV_LOW);
        let high = card_events(&mut m, super::EV_HIGH);
        let (src, dst) = (0x8001_0000, 0x8001_0100);
        m.mem.write(src, &[0x33; 128]);
        m.card_new(0x00);
        assert_eq!(m.card_write(0x00, 0x40, src), 1);
        assert_eq!(m.kernel.cards.status[0], super::BUSY_WRITE);
        assert!(matches!(bios::call(&mut m, 0xB0, 0x5D, [0; 4]).unwrap(), Flow::Block { .. }));
        assert_eq!(ready(&m, &low), Vec::<u32>::new(), "the wait did not drain");
        let flow = bios::call(&mut m, 0xB0, 0x5D, [0; 4]).unwrap();
        assert_eq!(finish(&mut m, flow), super::READY);
        assert_eq!(ready(&m, &low), [super::SPEC_DONE]);
        assert_eq!(ready(&m, &high), Vec::<u32>::new(), "raw I/O is low-level only");

        assert_eq!(m.card_read(0x00, 0x40, dst), 1);
        let flow = bios::call(&mut m, 0xB0, 0x5D, [0; 4]).unwrap();
        assert_eq!(finish(&mut m, flow), super::READY);
        let mut back = [0u8; 128];
        m.mem.read(dst, &mut back);
        assert_eq!(back, [0x33; 128]);

        // A missing card wakes the wait through the same completions.
        assert_eq!(m.card_info(0x10), 1);
        let flow = bios::call(&mut m, 0xB0, 0x5D, [1, 0, 0, 0]).unwrap();
        assert_eq!(finish(&mut m, flow), super::FAIL_TIMEOUT);
        assert_eq!(ready(&m, &low), [super::SPEC_DONE, super::SPEC_NO_CARD]);
        assert_eq!(ready(&m, &high), [super::SPEC_NO_CARD]);
    }

    /// Asynchronous file I/O queues one native frame at a time. Until its
    /// vblank, the guest buffer and card are unchanged and the slot refuses
    /// another request; the final frame and the file's event follow.
    #[test]
    fn async_file_io_moves_frames_and_reports_completion() {
        let mut m = machine();
        m.cards_ready();
        m.kernel.cards.slots[0].as_mut().unwrap().new_card = false;
        let name = "bu00:ASYNCTEST";
        assert_eq!(m.card_open(name, 0x200 | 2 << 16, 3), 3);
        let src = 0x8001_0000;
        m.mem.write(src, &[7; 256]);
        assert_eq!(m.card_io(3, src, 256, true), 256);
        m.kernel.cards.files.remove(&3);

        let dst = 0x8002_0000;
        m.mem.write(dst, &[0; 256]);
        let low = card_events(&mut m, super::EV_LOW);
        let high = card_events(&mut m, super::EV_HIGH);
        let fd_event = m.open_event(4, super::SPEC_DONE, 0x2000, 0);
        m.set_ev_status(fd_event, crate::kernel::EV_ENABLED);
        assert_eq!(m.card_open(name, 0x8001, 4), 4);
        assert_eq!(m.card_io(4, dst, 256, false), 0);
        assert_eq!(m.kernel.cards.status[0], super::BUSY_READ);
        assert_eq!(m.mem.r8(dst), 0, "no bytes have moved yet");
        assert_eq!(m.card_io(4, dst, 128, false), u32::MAX, "the slot is busy");
        assert_eq!(m.kernel.cards.errno, super::E_GENERAL);

        m.card_vblank();
        assert_eq!(m.mem.r8(dst), 7, "the first frame is visible");
        assert_eq!(m.mem.r8(dst + 128), 0, "the second frame is not");
        assert_eq!(m.kernel.cards.status[0], super::BUSY_READ);
        assert_eq!(ready(&m, &low), [super::SPEC_DONE]);
        m.card_vblank();
        assert_eq!(m.mem.r8(dst + 128), 7);
        assert_eq!(m.kernel.cards.status[0], super::READY, "a read ends on its final low event");
        assert_eq!(ready(&m, &low), [super::SPEC_DONE]);
        assert_eq!(m.kernel.cards.files[&4].pos, 256);
        assert_eq!(m.mem.r32(crate::files::fcb_at(4) + crate::files::fcb::OFFSET), 256);
        assert!(m.event_ready(fd_event));
        assert_eq!(ready(&m, &high), [super::SPEC_DONE]);
        for (_, h) in low.iter().chain(high.iter()) {
            m.set_ev_status(*h, crate::kernel::EV_UNUSED);
        }
        m.set_ev_status(fd_event, crate::kernel::EV_UNUSED);

        // The write direction moves its frame at the completing vblank too.
        let wname = "bu00:ASYNCW";
        assert_eq!(m.card_open(wname, 0x200 | 1 << 16 | 0x8000, 5), 5);
        let frame = {
            let card = m.kernel.cards.slots[0].as_ref().unwrap();
            card.find("ASYNCW").unwrap().blocks[0] as u32 * 64
        };
        assert_eq!(m.card_io(5, src, 128, true), 0);
        assert_eq!(m.kernel.cards.slots[0].as_ref().unwrap().frame(frame)[0], 0, "not written yet");
        m.card_vblank();
        assert_eq!(m.kernel.cards.slots[0].as_ref().unwrap().frame(frame)[0], 7);
        assert_eq!(m.kernel.cards.status[0], super::BUSY_WRITE);
        m.card_vblank();
        assert_eq!(m.kernel.cards.status[0], super::READY);

        // A frame after the card-change flag is set fails with NEW and keeps
        // the partial offset, without writing the card.
        assert_eq!(m.card_open("bu00:NEWERR", 0x200 | 1 << 16 | 0x8000, 6), 6);
        let low = card_events(&mut m, super::EV_LOW);
        let high = card_events(&mut m, super::EV_HIGH);
        assert_eq!(m.card_io(6, src, 128, true), 0);
        m.kernel.cards.slots[0].as_mut().unwrap().new_card = true;
        let card_before = m.kernel.cards.slots[0].as_ref().unwrap().data.clone();
        m.card_vblank();
        assert_eq!(m.kernel.cards.status[0], super::FAIL_GENERAL);
        assert_eq!(m.kernel.cards.slots[0].as_ref().unwrap().data, card_before);
        assert_eq!(m.kernel.cards.files[&6].pos, 0);
        assert_eq!(m.mem.r32(crate::files::fcb_at(6) + crate::files::fcb::ERRNO), super::E_GENERAL);
        assert_eq!(ready(&m, &high), [super::SPEC_NEW]);
        assert_eq!(ready(&m, &low), [super::SPEC_NEW]);
        for (_, h) in low.iter().chain(high.iter()) {
            m.set_ev_status(*h, crate::kernel::EV_UNUSED);
        }

        // The write-info stage rechecks the card: removing it after the
        // final frame is an error, not success.
        m.kernel.cards.slots[0].as_mut().unwrap().new_card = false;
        m.kernel.cards.status[0] = super::READY;
        assert_eq!(m.card_open("bu00:DONEERR", 0x200 | 2 << 16 | 0x8000, 7), 7);
        let low = card_events(&mut m, super::EV_LOW);
        let high = card_events(&mut m, super::EV_HIGH);
        let fd_event = m.open_event(7, super::SPEC_DONE, 0x2000, 0);
        m.set_ev_status(fd_event, crate::kernel::EV_ENABLED);
        assert_eq!(m.card_io(7, src, 128, true), 0);
        m.card_vblank();
        assert!(m.card_file_pending(7));
        m.kernel.cards.slots[0] = None;
        m.card_vblank();
        assert_eq!(m.kernel.cards.status[0], super::FAIL_TIMEOUT);
        assert_eq!(m.kernel.cards.files[&7].pos, 128);
        assert_eq!(m.mem.r32(crate::files::fcb_at(7) + crate::files::fcb::OFFSET), 128);
        assert_eq!(m.mem.r32(crate::files::fcb_at(7) + crate::files::fcb::ERRNO), super::E_GENERAL);
        assert_eq!(ready(&m, &high), [super::SPEC_NO_CARD]);
        assert_eq!(ready(&m, &low), [super::SPEC_DONE, super::SPEC_NO_CARD]);
        assert!(!m.event_ready(fd_event), "no success event");
    }

    /// Auto format is opt-in, formats only a card whose frame 0 lacks the
    /// "MC" id, and `_bu_init` turns it back off.
    #[test]
    fn card_auto_format_is_opt_in_and_reset_by_bu_init() {
        let mut m = machine();
        m.cards_ready();
        m.kernel.cards.slots[0].as_mut().unwrap().data[0] = 0;
        m.kernel.cards.slots[0].as_mut().unwrap().new_card = false;
        assert_eq!(m.kernel.cards.auto_format, 0);
        assert_eq!(m.card_open("bu00:NOPE", 1, 3), u32::MAX);
        assert!(!m.kernel.cards.slots[0].as_ref().unwrap().valid(), "not formatted silently");
        assert_eq!(m.kernel.cards.errno, super::E_GENERAL);

        assert_eq!(ret(bios::call(&mut m, 0xA0, 0xAD, [2, 0, 0, 0])), 0, "the old setting");
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0xAD, [1, 0, 0, 0])), 2, "preserve the exact old value");
        assert_eq!(m.kernel.cards.auto_format, 1);
        let low = card_events(&mut m, super::EV_LOW);
        assert_eq!(m.card_open("bu00:NOPE", 1, 3), u32::MAX);
        assert!(m.kernel.cards.slots[0].as_ref().unwrap().valid(), "bad MC id was formatted");
        assert_eq!(m.kernel.cards.errno, super::E_NOT_FOUND);
        assert_eq!(ready(&m, &low), [super::SPEC_DONE]);

        ret(bios::call(&mut m, 0xA0, 0x55, [0; 4]));
        assert_eq!(m.kernel.cards.auto_format, 0, "_bu_init resets it");
        // A missing card is not created, and a valid card is not formatted.
        m.kernel.cards.auto_format = 1;
        assert_eq!(m.card_find(Some("bu10:*"), 0x8003_0000), 0);
        assert!(m.kernel.cards.slots[1].is_none());
        let before = m.kernel.cards.slots[0].as_ref().unwrap().data.clone();
        assert_eq!(m.card_open("bu00:NOPE", 1, 3), u32::MAX);
        assert_eq!(m.kernel.cards.slots[0].as_ref().unwrap().data, before);
    }

    /// `_card_info_subfunc` is the low-level info operation, `_card_chan`
    /// names the slot it completed on, and the bufs callbacks report only
    /// the higher-level event with the side effects psx-spx gives them.
    #[test]
    fn card_completion_helpers_follow_the_last_completion() {
        let mut m = machine();
        let high = card_events(&mut m, super::EV_HIGH);
        assert_eq!(m.card_info_subfunc(0x00), 1);
        m.card_vblank();
        assert_eq!(ready(&m, &high), Vec::<u32>::new(), "the subfunction is low-level");
        assert_eq!(m.card_chan(), 0x00, "exact device id");
        assert_eq!(m.card_info_subfunc(0x10), 1);
        m.card_vblank();
        assert_eq!(m.card_chan(), 0x10);
        assert_eq!(m.kernel.cards.status[1], super::FAIL_TIMEOUT);

        // A8/A9/AA/AE are error callbacks: they cancel an active file
        // operation, leave the low-level event alone and report high only.
        for (which, spec, status) in [
            (1, super::SPEC_ERROR, super::FAIL_GENERAL),
            (2, super::SPEC_NO_CARD, super::FAIL_TIMEOUT),
            (3, super::SPEC_NEW, super::FAIL_GENERAL),
            (4, super::SPEC_ERROR, super::FAIL_GENERAL),
        ] {
            let mut m = machine();
            m.cards_ready();
            m.kernel.cards.slots[0].as_mut().unwrap().new_card = false;
            let src = 0x8001_0000;
            m.mem.write(src, &[1; 128]);
            assert_eq!(m.card_open("bu00:CB", 0x200 | 1 << 16, 5), 5);
            assert_eq!(m.card_io(5, src, 128, true), 128);
            m.kernel.cards.files.remove(&5);
            assert_eq!(m.card_open("bu00:CB", 0x8001, 3), 3);
            let dst = 0x8001_0100;
            m.mem.write(dst, &[0; 128]);
            assert_eq!(m.card_io(3, dst, 128, false), 0);
            let card_before = m.kernel.cards.slots[0].as_ref().unwrap().data.clone();
            let low = card_events(&mut m, super::EV_LOW);
            let high = card_events(&mut m, super::EV_HIGH);
            m.card_bufs_cb(which);
            assert_eq!(ready(&m, &low), Vec::<u32>::new(), "no low replay");
            assert_eq!(ready(&m, &high), [spec]);
            assert!(!m.card_file_pending(3), "the remaining frames are canceled");
            assert_eq!(m.kernel.cards.status[0], status);
            assert_eq!(m.mem.r8(dst), 0, "the canceled frame never touched the buffer");
            assert_eq!(m.kernel.cards.slots[0].as_ref().unwrap().data, card_before);
            assert_eq!(m.kernel.cards.files[&3].pos, 0);
            assert_eq!(m.mem.r32(crate::files::fcb_at(3) + crate::files::fcb::OFFSET), 0);
            assert_eq!(m.mem.r32(crate::files::fcb_at(3) + crate::files::fcb::ERRNO), super::E_GENERAL);
        }

        // With no active operation A7 reports nothing.
        let mut m = machine();
        let high = card_events(&mut m, super::EV_HIGH);
        m.card_bufs_cb(0);
        assert_eq!(ready(&m, &high), Vec::<u32>::new());
    }

    /// A7 is dispatched as the low-level completion callback. Native
    /// completion already advanced the frame stage; A7 must not consume the
    /// scheduled write-info stage early, and must not replay the low event.
    #[test]
    fn card_completion_callback_does_not_consume_future_work() {
        let mut m = machine();
        m.cards_ready();
        m.kernel.cards.slots[0].as_mut().unwrap().new_card = false;
        assert_eq!(m.card_open("bu00:CB7", 0x200 | 1 << 16 | 0x8000, 3), 3);
        let src = 0x8001_0000;
        m.mem.write(src, &[5; 128]);
        assert_eq!(m.card_io(3, src, 128, true), 0);

        let a7 = tables::stand_in(0xA0, 0xA7);
        let low = m.open_event(super::EV_LOW, super::SPEC_DONE, 0x1000, a7);
        m.set_ev_status(low, crate::kernel::EV_ENABLED);
        let (fd_call, high_call) = (0x8002_0000, 0x8002_0004);
        let fd_event = m.open_event(3, super::SPEC_DONE, 0x1000, fd_call);
        m.set_ev_status(fd_event, crate::kernel::EV_ENABLED);
        let high_event = m.open_event(super::EV_HIGH, super::SPEC_DONE, 0x1000, high_call);
        m.set_ev_status(high_event, crate::kernel::EV_ENABLED);

        m.card_vblank();
        assert_eq!(m.calls.pop_front(), Some(GuestCall { addr: a7, args: [0; 4] }));
        assert!(m.calls.is_empty(), "the final frame's low event queued A7");
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0xA7, [0; 4])), super::BUSY_WRITE);
        assert!(m.calls.is_empty(), "A7 did not queue fd/high or another low event");
        assert!(m.card_file_pending(3), "the scheduled info stage remains");

        m.card_vblank();
        assert_eq!(m.calls.iter().map(|c| c.addr).collect::<Vec<_>>(), [fd_call, high_call, a7], "fd, high, then low");
        assert_eq!(m.kernel.cards.status[0], super::READY);
        assert!(!m.card_file_pending(3));
        m.calls.clear();
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0xA7, [0; 4])), super::READY);
        assert!(m.calls.is_empty(), "a repeated A7 delivers nothing");
    }

    /// psxclose releases the fd even while the card device reports busy.
    /// The native close policy cancels the old open, so reopening the SAME
    /// fd on the other card cannot receive stale frames, buffers or events.
    #[test]
    fn async_file_close_releases_the_fd_without_stale_card_work() {
        let mut m = machine();
        m.cards_ready();
        m.kernel.cards.slots[1] = Some(Card::formatted());
        m.kernel.cards.status[1] = super::READY;
        for slot in 0..2 {
            m.kernel.cards.slots[slot].as_mut().unwrap().new_card = false;
        }
        let (path0, path1) = (0x8001_0000, 0x8001_0020);
        m.mem.write(path0, b"bu00:CLOSETEST\0");
        m.mem.write(path1, b"bu10:CLOSETEST\0");
        let src = 0x8001_1000;
        m.mem.write(src, &[9; 256]);
        let open_create = 0x200 | 1 << 16;
        let fd0 = ret(bios::call(&mut m, 0xB0, 0x32, [path0, open_create, 0, 0]));
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x35, [fd0, src, 256, 0])), 256);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x36, [fd0, 0, 0, 0])), fd0);
        let fd1 = ret(bios::call(&mut m, 0xB0, 0x32, [path1, open_create, 0, 0]));
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x35, [fd1, src, 256, 0])), 256);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x36, [fd1, 0, 0, 0])), fd1);

        let fd = ret(bios::call(&mut m, 0xB0, 0x32, [path0, 0x8001, 0, 0]));
        let dst0 = 0x8001_2000;
        m.mem.write(dst0, &[0; 256]);
        let low = card_events(&mut m, super::EV_LOW);
        let high = card_events(&mut m, super::EV_HIGH);
        let fd_event = m.open_event(fd, super::SPEC_DONE, 0x2000, 0);
        m.set_ev_status(fd_event, crate::kernel::EV_ENABLED);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x34, [fd, dst0, 256, 0])), 0);
        assert!(m.card_file_pending(fd));
        m.card_vblank();
        assert_eq!(m.mem.r8(dst0), 9);
        assert_eq!(m.mem.r8(dst0 + 128), 0);
        for (_, h) in &low {
            m.set_ev_status(*h, crate::kernel::EV_ENABLED);
        }
        let fcb = crate::files::fcb_at(fd);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x36, [fd, 0, 0, 0])), u32::MAX);
        assert_eq!(m.mem.r32(fcb + crate::files::fcb::FLAGS), 0, "psxclose releases the fd");
        assert!(!m.card_file_pending(fd), "the old open's frames are canceled");
        assert!(!m.kernel.cards.files.contains_key(&fd), "the old open is invalidated");
        assert_eq!(m.mem.r32(fcb + crate::files::fcb::OFFSET), 128, "preserve completed frames");
        assert_eq!(m.mem.r8(dst0 + 128), 0);
        assert_eq!(m.kernel.cards.status[0], super::READY);
        assert_eq!(ready(&m, &low), Vec::<u32>::new(), "no cancellation events");
        assert_eq!(ready(&m, &high), Vec::<u32>::new());
        assert!(!m.event_ready(fd_event));

        // Reusing the SAME fd on slot 2 starts a new open.
        let reused = ret(bios::call(&mut m, 0xB0, 0x32, [path1, 0x8001, 0, 0]));
        assert_eq!(reused, fd);
        let dst1 = 0x8001_2100;
        m.mem.write(dst1, &[0; 256]);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x34, [fd, dst1, 256, 0])), 0);
        m.card_vblank();
        m.card_vblank();
        assert_eq!(m.mem.r8(dst0), 9, "keep the completed frame");
        assert_eq!(m.mem.r8(dst0 + 128), 0, "the canceled frame stays untouched");
        assert_eq!(m.mem.r8(dst1), 9, "the slot-2 read moved its own buffer");
        assert_eq!(m.kernel.cards.status[0], super::READY);
        assert_eq!(m.kernel.cards.status[1], super::READY);
        assert_eq!(m.mem.r32(fcb + crate::files::fcb::OFFSET), 256);
        assert_eq!(ready(&m, &low), [super::SPEC_DONE]);
        assert_eq!(ready(&m, &high), [super::SPEC_DONE]);
        assert!(m.event_ready(fd_event));
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x36, [fd, 0, 0, 0])), fd);
    }
}
