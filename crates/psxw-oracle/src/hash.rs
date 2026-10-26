//! Content hashes: sha256 identifies vectors, blobs and results.

use anyhow::{Result, bail};
use std::fmt;

/// A sha256 digest.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hash(pub [u8; 32]);

impl Hash {
    pub fn of(bytes: &[u8]) -> Self {
        use sha2::{Digest, Sha256};
        let d = Sha256::digest(bytes);
        let mut out = [0u8; 32];
        out.copy_from_slice(&d);
        Hash(out)
    }

    pub fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn parse(s: &str) -> Result<Self> {
        if s.len() != 64 || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
            bail!("not a sha256: {s}");
        }
        let mut out = [0u8; 32];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16)?;
        }
        Ok(Hash(out))
    }

    /// First 12 hex digits, for logs.
    pub fn short(&self) -> String {
        self.hex()[..12].to_string()
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.short())
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

/// Lowercase hex sha1 (BIOS images are conventionally identified by sha1).
pub fn sha1_hex(bytes: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    Sha1::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_digest() {
        assert_eq!(Hash::of(b"abc").hex(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        let h = Hash::of(b"x");
        assert_eq!(Hash::parse(&h.hex()).unwrap(), h);
    }
}
