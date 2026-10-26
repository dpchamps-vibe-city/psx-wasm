//! A memory card (psx-spx "Memory Card Data Format"): 128 KiB in 16 blocks
//! of 64 frames of 128 bytes. Block 0 is the directory: frame 0 the "MC"
//! header, frames 1..15 one entry per data block, frames 16..35 the broken
//! sector list. Every directory frame ends in an XOR checksum. Images are
//! the raw 128 KiB (`.mcr`/`.mcd`).
//!
//! The BIOS's card functions (`psx-bios`) work on this model: raw frames
//! for `_card_read`/`_card_write`, and the directory for the `bu00:` file
//! functions.

pub const SIZE: usize = 0x2_0000;
pub const FRAME: usize = 0x80;
pub const BLOCK: usize = 0x2000;
pub const FRAMES: u32 = (SIZE / FRAME) as u32;
pub const DATA_BLOCKS: usize = 15;

/// Directory entry states.
pub const FREE: u8 = 0xA0;
pub const FIRST: u8 = 0x51;
pub const MIDDLE: u8 = 0x52;
pub const LAST: u8 = 0x53;
/// Deleted states are the in-use ones plus 0x50.
pub const DELETED: u8 = 0x50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub data: Vec<u8>,
    /// The card's "new card" flag (FLAG bit 3): set when inserted, cleared
    /// by a sector write.
    pub new_card: bool,
    /// Written since the image was last saved.
    pub dirty: bool,
}

/// A file found in the directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    pub name: String,
    pub size: u32,
    /// Data blocks (1..=15) in file order.
    pub blocks: Vec<usize>,
    pub deleted: bool,
}

fn checksum(frame: &[u8]) -> u8 {
    frame[..FRAME - 1].iter().fold(0, |a, b| a ^ b)
}

/// `?` matches one character; `*` matches everything after it (the BIOS
/// accepts nothing after `*`).
pub fn name_matches(pattern: &str, name: &str) -> bool {
    let (p, n) = (pattern.as_bytes(), name.as_bytes());
    for (i, &c) in p.iter().enumerate() {
        match c {
            b'*' => return true,
            b'?' => {
                if i > n.len() {
                    return false;
                }
            }
            _ => {
                if n.get(i) != Some(&c) {
                    return false;
                }
            }
        }
    }
    n.len() <= p.len()
}

impl Card {
    /// A freshly formatted card.
    pub fn formatted() -> Self {
        let mut c = Card { data: vec![0; SIZE], new_card: true, dirty: true };
        c.format();
        c
    }

    /// A raw image; `None` unless 128 KiB.
    pub fn from_image(bytes: &[u8]) -> Option<Self> {
        (bytes.len() == SIZE).then(|| Card { data: bytes.to_vec(), new_card: true, dirty: false })
    }

    pub fn format(&mut self) {
        self.data.fill(0);
        let header = |c: &mut Card, f: usize| {
            let at = f * FRAME;
            c.data[at] = b'M';
            c.data[at + 1] = b'C';
            c.seal(f);
        };
        header(self, 0);
        for f in 1..=DATA_BLOCKS {
            let at = f * FRAME;
            self.data[at] = FREE;
            self.data[at + 8..at + 10].copy_from_slice(&0xFFFFu16.to_le_bytes());
            self.seal(f);
        }
        for f in 16..36 {
            let at = f * FRAME;
            self.data[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            self.seal(f);
        }
        for f in 36..63 {
            self.data[f * FRAME..(f + 1) * FRAME].fill(0xFF);
        }
        header(self, 63);
        self.dirty = true;
    }

    /// Recompute a directory frame's checksum.
    fn seal(&mut self, f: usize) {
        let at = f * FRAME;
        self.data[at + FRAME - 1] = checksum(&self.data[at..at + FRAME]);
    }

    pub fn frame(&self, n: u32) -> &[u8] {
        let at = n as usize * FRAME;
        &self.data[at..at + FRAME]
    }

    pub fn write_frame(&mut self, n: u32, src: &[u8]) {
        let at = n as usize * FRAME;
        self.data[at..at + FRAME].copy_from_slice(&src[..FRAME]);
        self.new_card = false;
        self.dirty = true;
    }

    /// The header and every directory and broken-sector frame have valid
    /// checksums (frame 0 also says "MC").
    pub fn valid(&self) -> bool {
        self.data[0] == b'M' && self.data[1] == b'C' && (0..36).all(|f| checksum(self.frame(f)) == self.frame(f)[FRAME - 1])
    }

    fn entry(&self, block: usize) -> &[u8] {
        self.frame(block as u32)
    }

    /// Every file, following each first block's chain.
    pub fn files(&self) -> Vec<FileInfo> {
        let mut out = Vec::new();
        for b in 1..=DATA_BLOCKS {
            let e = self.entry(b);
            let deleted = e[0] == FIRST + DELETED;
            if e[0] != FIRST && !deleted {
                continue;
            }
            let size = u32::from_le_bytes(e[4..8].try_into().unwrap());
            let name_bytes = &e[0x0A..0x1F];
            let end = name_bytes.iter().position(|&c| c == 0).unwrap_or(name_bytes.len());
            let name = String::from_utf8_lossy(&name_bytes[..end]).into_owned();
            let mut blocks = vec![b];
            let mut cur = b;
            while blocks.len() < DATA_BLOCKS {
                let next = u16::from_le_bytes(self.entry(cur)[8..10].try_into().unwrap());
                if next == 0xFFFF || next as usize >= DATA_BLOCKS {
                    break;
                }
                cur = next as usize + 1;
                blocks.push(cur);
            }
            out.push(FileInfo { name, size, blocks, deleted });
        }
        out
    }

    /// The first live file matching `pattern` (`?`, trailing `*`).
    pub fn find(&self, pattern: &str) -> Option<FileInfo> {
        self.files().into_iter().find(|f| !f.deleted && name_matches(pattern, &f.name))
    }

    pub fn free_blocks(&self) -> Vec<usize> {
        (1..=DATA_BLOCKS).filter(|&b| self.entry(b)[0] & 0xF0 == 0xA0).collect()
    }

    /// Create a file of `blocks` blocks; `None` if it exists or the card
    /// is full.
    pub fn create(&mut self, name: &str, blocks: usize) -> Option<FileInfo> {
        if blocks == 0 || self.find(name).is_some() {
            return None;
        }
        let free = self.free_blocks();
        if free.len() < blocks {
            return None;
        }
        let chain = &free[..blocks];
        for (i, &b) in chain.iter().enumerate() {
            let at = b * FRAME;
            self.data[at..at + FRAME].fill(0);
            self.data[at] = match (i, blocks) {
                (0, _) => FIRST,
                (i, n) if i + 1 == n => LAST,
                _ => MIDDLE,
            };
            let next: u16 = chain.get(i + 1).map_or(0xFFFF, |&n| (n - 1) as u16);
            self.data[at + 8..at + 10].copy_from_slice(&next.to_le_bytes());
            if i == 0 {
                self.data[at + 4..at + 8].copy_from_slice(&((blocks * BLOCK) as u32).to_le_bytes());
                let n = name.as_bytes();
                let len = n.len().min(20);
                self.data[at + 0x0A..at + 0x0A + len].copy_from_slice(&n[..len]);
            }
            self.seal(b);
        }
        self.dirty = true;
        self.find(name)
    }

    fn set_states(&mut self, f: &FileInfo, delta: i16) {
        for &b in &f.blocks {
            let at = b * FRAME;
            self.data[at] = (self.data[at] as i16 + delta) as u8;
            self.seal(b);
        }
        self.dirty = true;
    }

    pub fn delete(&mut self, pattern: &str) -> bool {
        match self.find(pattern) {
            Some(f) => {
                self.set_states(&f, DELETED as i16);
                true
            }
            None => false,
        }
    }

    pub fn undelete(&mut self, pattern: &str) -> bool {
        let Some(f) = self.files().into_iter().find(|f| f.deleted && name_matches(pattern, &f.name)) else { return false };
        if self.find(&f.name).is_some() {
            return false;
        }
        self.set_states(&f, -(DELETED as i16));
        true
    }

    pub fn rename(&mut self, from: &str, to: &str) -> bool {
        if self.find(to).is_some() {
            return false;
        }
        let Some(f) = self.find(from) else { return false };
        let at = f.blocks[0] * FRAME;
        self.data[at + 0x0A..at + 0x1F].fill(0);
        let n = to.as_bytes();
        let len = n.len().min(20);
        self.data[at + 0x0A..at + 0x0A + len].copy_from_slice(&n[..len]);
        self.seal(f.blocks[0]);
        self.dirty = true;
        true
    }

    /// The card frame holding byte `offset` of a file.
    pub fn file_frame(&self, f: &FileInfo, offset: u32) -> Option<u32> {
        let block = *f.blocks.get(offset as usize / BLOCK)?;
        Some((block * BLOCK + offset as usize % BLOCK) as u32 / FRAME as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatted_card_is_valid() {
        let c = Card::formatted();
        assert!(c.valid());
        assert_eq!(c.frame(0)[FRAME - 1], 0x0E, "psx-spx: usually 0Eh");
        assert_eq!(c.free_blocks().len(), 15);
        assert!(c.files().is_empty());
    }

    #[test]
    fn create_find_delete_undelete_rename() {
        let mut c = Card::formatted();
        let f = c.create("BASLUS-01115BTN01", 3).unwrap();
        assert_eq!(f.blocks, [1, 2, 3]);
        assert_eq!(f.size, 3 * BLOCK as u32);
        assert!(c.valid());
        assert_eq!(c.frame(1)[0], FIRST);
        assert_eq!(c.frame(2)[0], MIDDLE);
        assert_eq!(c.frame(3)[0], LAST);
        assert!(c.create("BASLUS-01115BTN01", 1).is_none(), "exists");
        assert_eq!(c.find("BASLUS-01115BTN??").unwrap().blocks, [1, 2, 3]);
        assert_eq!(c.find("BASLUS-01115*").unwrap().name, "BASLUS-01115BTN01");
        assert_eq!(c.file_frame(&f, 0x2080), Some((2 * BLOCK + 0x80) as u32 / FRAME as u32));
        assert!(c.rename("BASLUS-01115BTN01", "BASLUS-01115BTN02"));
        assert!(c.delete("BASLUS-01115BTN02"));
        assert!(c.find("BASLUS-01115BTN02").is_none());
        assert_eq!(c.free_blocks().len(), 15);
        assert!(c.undelete("BASLUS-01115BTN02"));
        assert_eq!(c.find("BASLUS-01115BTN02").unwrap().blocks, [1, 2, 3]);
        assert!(c.valid());
        assert!(c.create("BIG", 13).is_none(), "only 12 free");
    }

    #[test]
    fn wildcards() {
        assert!(name_matches("A?C", "ABC"));
        assert!(name_matches("AB*", "ABCDEF"));
        assert!(!name_matches("ABC", "ABCD"));
        assert!(!name_matches("ABCD", "ABC"));
        assert!(name_matches("FILE?", "FILE"), "psx-spx: ? matching the end ignores the rest");
    }
}
