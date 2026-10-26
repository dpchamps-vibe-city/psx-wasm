//! Replay files: a header, then each entry's pad state. With the same disc
//! and build, a replay determines a run exactly. psx-wasm and the oracle's
//! emulator host consume the same file.
//!
//! ```text
//! 0   magic "PSXWRPL\0"
//! 8   version (u32): 1 or 2
//! 12  pad type (u32): 0x41 digital, 0x73 analog
//! 16  entries (u32)
//! 20  game id (16 bytes, NUL padded)
//! 36  disc sha1 (20 bytes; zero when unknown)
//! 56  version 2: clock (u32), reserved (u32)
//! 56 or 64: entry 0: port 1 buttons (u16), port 2 buttons (u16)
//! ...
//! ```
//!
//! **The clock** picks the entry a host uses at each vblank. 0 (and every
//! version 1 file) means the vblank number. Otherwise it is the guest
//! address of a u32 the game counts frames in, and the entry is that
//! counter's value. That way two runs see the same input at the same game
//! state even when their load times differ (the whole-game oracle, where
//! the emulator also spends its BIOS's boot first).
//!
//! Buttons are the pad's 16-bit word as libpad reports it, active low
//! (`0xFFFF` = nothing pressed).

pub const MAGIC: &[u8; 8] = b"PSXWRPL\0";
/// The version written.
pub const VERSION: u32 = 2;
/// The header written (version 2).
pub const HEADER_LEN: usize = 64;
const HEADER_LEN_V1: usize = 56;
pub const FRAME_LEN: usize = 4;
pub const NOTHING_PRESSED: u16 = 0xFFFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub version: u32,
    pub pad_type: u32,
    pub frames: u32,
    pub game_id: [u8; 16],
    pub disc_sha1: [u8; 20],
    /// 0: entries by vblank; else the guest address of a u32 frame counter.
    pub clock: u32,
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

impl Header {
    pub fn parse(bytes: &[u8]) -> Option<Header> {
        if bytes.len() < HEADER_LEN_V1 || &bytes[..8] != MAGIC {
            return None;
        }
        let mut game_id = [0; 16];
        game_id.copy_from_slice(&bytes[20..36]);
        let mut disc_sha1 = [0; 20];
        disc_sha1.copy_from_slice(&bytes[36..56]);
        let version = u32_at(bytes, 8);
        let clock = match version {
            1 => 0,
            2 if bytes.len() >= HEADER_LEN => u32_at(bytes, 56),
            _ => return None,
        };
        let h = Header { version, pad_type: u32_at(bytes, 12), frames: u32_at(bytes, 16), game_id, disc_sha1, clock };
        if bytes.len() < h.data_offset() + h.frames as usize * FRAME_LEN {
            return None;
        }
        Some(h)
    }

    /// Where entry 0 starts.
    pub fn data_offset(&self) -> usize {
        if self.version == 1 { HEADER_LEN_V1 } else { HEADER_LEN }
    }

    /// The entry for a vblank: the vblank number, or the clock's value
    /// (read with `read_u32`, a guest address in, the word out).
    pub fn entry(&self, vblank: u64, read_u32: impl FnOnce(u32) -> u32) -> u32 {
        if self.clock == 0 { vblank.min(u32::MAX as u64) as u32 } else { read_u32(self.clock) }
    }

    pub fn encode(&self, out: &mut [u8; HEADER_LEN]) {
        out[..8].copy_from_slice(MAGIC);
        out[8..12].copy_from_slice(&self.version.to_le_bytes());
        out[12..16].copy_from_slice(&self.pad_type.to_le_bytes());
        out[16..20].copy_from_slice(&self.frames.to_le_bytes());
        out[20..36].copy_from_slice(&self.game_id);
        out[36..56].copy_from_slice(&self.disc_sha1);
        out[56..60].copy_from_slice(&self.clock.to_le_bytes());
        out[60..64].fill(0);
    }
}

/// Port 1 and port 2 buttons for entry `index`; nothing pressed past the
/// end.
pub fn frame(bytes: &[u8], index: u32) -> [u16; 2] {
    let Some(h) = Header::parse(bytes) else { return [NOTHING_PRESSED; 2] };
    let at = h.data_offset() + index as usize * FRAME_LEN;
    match bytes.get(at..at + FRAME_LEN) {
        Some(b) if index < h.frames => [u16::from_le_bytes([b[0], b[1]]), u16::from_le_bytes([b[2], b[3]])],
        _ => [NOTHING_PRESSED; 2],
    }
}

/// Button names in pad-word bit order.
pub const BUTTONS: [&str; 16] = [
    "SELECT", "L3", "R3", "START", "UP", "RIGHT", "DOWN", "LEFT", "L2", "R2", "L1", "R1", "TRIANGLE", "CIRCLE", "CROSS",
    "SQUARE",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let h = Header { version: VERSION, pad_type: 0x41, frames: 2, game_id: *b"btn\0\0\0\0\0\0\0\0\0\0\0\0\0", disc_sha1: [7; 20], clock: 0x8006_C840 };
        let mut buf = [0u8; HEADER_LEN + 2 * FRAME_LEN];
        let mut head = [0u8; HEADER_LEN];
        h.encode(&mut head);
        buf[..HEADER_LEN].copy_from_slice(&head);
        buf[HEADER_LEN..HEADER_LEN + 2].copy_from_slice(&0xFFEFu16.to_le_bytes());
        assert_eq!(Header::parse(&buf), Some(h));
        assert_eq!(frame(&buf, 0), [0xFFEF, 0]);
        assert_eq!(frame(&buf, 5), [NOTHING_PRESSED; 2]);
        assert_eq!(h.entry(9, |a| a & 0xFF), 0x40, "counter-indexed");
        // Version 1: no clock field, entries from byte 56.
        let mut v1 = [0u8; 56 + 4];
        v1[..56].copy_from_slice(&head[..56]);
        v1[8..12].copy_from_slice(&1u32.to_le_bytes());
        v1[16..20].copy_from_slice(&1u32.to_le_bytes());
        v1[56..58].copy_from_slice(&0xFFFEu16.to_le_bytes());
        let p = Header::parse(&v1).unwrap();
        assert_eq!((p.clock, p.data_offset()), (0, 56));
        assert_eq!(frame(&v1, 0), [0xFFFE, 0]);
        assert_eq!(p.entry(7, |_| unreachable!()), 7);
    }
}
