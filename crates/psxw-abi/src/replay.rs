//! Replay files: a header, then each frame's pad state. With the same disc
//! and build, a replay determines a run exactly. psx-wasm and the oracle's
//! emulator host consume the same file.
//!
//! ```text
//! 0   magic "PSXWRPL\0"
//! 8   version (u32)
//! 12  pad type (u32): 0x41 digital, 0x73 analog
//! 16  frames (u32)
//! 20  game id (16 bytes, NUL padded)
//! 36  disc sha1 (20 bytes; zero when unknown)
//! 56  frame 0: port 1 buttons (u16), port 2 buttons (u16)
//! ...
//! ```
//!
//! Buttons are the pad's 16-bit word as libpad reports it, active low
//! (`0xFFFF` = nothing pressed).

pub const MAGIC: &[u8; 8] = b"PSXWRPL\0";
pub const VERSION: u32 = 1;
pub const HEADER_LEN: usize = 56;
pub const FRAME_LEN: usize = 4;
pub const NOTHING_PRESSED: u16 = 0xFFFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub version: u32,
    pub pad_type: u32,
    pub frames: u32,
    pub game_id: [u8; 16],
    pub disc_sha1: [u8; 20],
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

impl Header {
    pub fn parse(bytes: &[u8]) -> Option<Header> {
        if bytes.len() < HEADER_LEN || &bytes[..8] != MAGIC {
            return None;
        }
        let mut game_id = [0; 16];
        game_id.copy_from_slice(&bytes[20..36]);
        let mut disc_sha1 = [0; 20];
        disc_sha1.copy_from_slice(&bytes[36..56]);
        let h = Header { version: u32_at(bytes, 8), pad_type: u32_at(bytes, 12), frames: u32_at(bytes, 16), game_id, disc_sha1 };
        if h.version != VERSION || bytes.len() < HEADER_LEN + h.frames as usize * FRAME_LEN {
            return None;
        }
        Some(h)
    }

    pub fn encode(&self, out: &mut [u8; HEADER_LEN]) {
        out[..8].copy_from_slice(MAGIC);
        out[8..12].copy_from_slice(&self.version.to_le_bytes());
        out[12..16].copy_from_slice(&self.pad_type.to_le_bytes());
        out[16..20].copy_from_slice(&self.frames.to_le_bytes());
        out[20..36].copy_from_slice(&self.game_id);
        out[36..56].copy_from_slice(&self.disc_sha1);
    }
}

/// Port 1 and port 2 buttons for `frame`; nothing pressed past the end.
pub fn frame(bytes: &[u8], frame: u32) -> [u16; 2] {
    let at = HEADER_LEN + frame as usize * FRAME_LEN;
    match bytes.get(at..at + FRAME_LEN) {
        Some(b) if Header::parse(bytes).is_some_and(|h| frame < h.frames) => {
            [u16::from_le_bytes([b[0], b[1]]), u16::from_le_bytes([b[2], b[3]])]
        }
        _ => [NOTHING_PRESSED; 2],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let h = Header { version: VERSION, pad_type: 0x41, frames: 2, game_id: *b"btn\0\0\0\0\0\0\0\0\0\0\0\0\0", disc_sha1: [7; 20] };
        let mut buf = [0u8; HEADER_LEN + 2 * FRAME_LEN];
        let mut head = [0u8; HEADER_LEN];
        h.encode(&mut head);
        buf[..HEADER_LEN].copy_from_slice(&head);
        buf[HEADER_LEN..HEADER_LEN + 2].copy_from_slice(&0xFFEFu16.to_le_bytes());
        assert_eq!(Header::parse(&buf), Some(h));
        assert_eq!(frame(&buf, 0), [0xFFEF, 0]);
        assert_eq!(frame(&buf, 5), [NOTHING_PRESSED; 2]);
    }
}
