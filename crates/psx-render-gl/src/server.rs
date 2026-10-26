//! The render server's protocol: a process (in the oracle, the
//! `psxw-render-gl` Docker image running `psxw-render-server`) that owns
//! a headless context and executes command streams sent over a pipe.
//! Both ends use this module, so the framing can't drift.
//!
//! Every message is little-endian: a request is `u32 kind, u32 words`,
//! then that many `u32`s; a response is `u32 status, u32 bytes`, then that
//! many bytes (status 0 = ok, else a UTF-8 error message).

use anyhow::{Result, bail, ensure};
use std::io::{Read, Write};

/// Execute a command stream on the current VRAM; the response is the
/// READBACK pixels, `u16` little-endian, in order.
pub const RUN: u32 = 1;
/// Zero VRAM and the colour-table slots.
pub const RESET: u32 = 2;
/// Describe the server and its GL implementation (JSON).
pub const INFO: u32 = 3;
/// Stop.
pub const QUIT: u32 = 4;
/// The last presented frame: `u32 width, u32 height`, then RGBA8 rows
/// from the top.
pub const FRAME: u32 = 5;

pub const STATUS_OK: u32 = 0;
pub const STATUS_ERROR: u32 = 1;

/// Longest request accepted, in words (a whole-VRAM upload is 256 Ki).
pub const MAX_REQUEST_WORDS: u32 = 64 << 20;

pub fn write_request(w: &mut impl Write, kind: u32, words: &[u32]) -> Result<()> {
    let mut buf = Vec::with_capacity(8 + 4 * words.len());
    buf.extend_from_slice(&kind.to_le_bytes());
    buf.extend_from_slice(&(words.len() as u32).to_le_bytes());
    for v in words {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    w.write_all(&buf)?;
    w.flush()?;
    Ok(())
}

/// The next request, or None at end of input.
pub fn read_request(r: &mut impl Read) -> Result<Option<(u32, Vec<u32>)>> {
    let mut hdr = [0u8; 8];
    match r.read_exact(&mut hdr) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let kind = u32::from_le_bytes(hdr[..4].try_into()?);
    let n = u32::from_le_bytes(hdr[4..].try_into()?);
    ensure!(n <= MAX_REQUEST_WORDS, "request of {n} words");
    let mut body = vec![0u8; 4 * n as usize];
    r.read_exact(&mut body)?;
    Ok(Some((kind, body.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())))
}

pub fn write_response(w: &mut impl Write, status: u32, bytes: &[u8]) -> Result<()> {
    w.write_all(&status.to_le_bytes())?;
    w.write_all(&(bytes.len() as u32).to_le_bytes())?;
    w.write_all(bytes)?;
    w.flush()?;
    Ok(())
}

/// The response's bytes; an error status becomes an error.
pub fn read_response(r: &mut impl Read) -> Result<Vec<u8>> {
    let mut hdr = [0u8; 8];
    r.read_exact(&mut hdr)?;
    let status = u32::from_le_bytes(hdr[..4].try_into()?);
    let n = u32::from_le_bytes(hdr[4..].try_into()?) as usize;
    let mut body = vec![0u8; n];
    r.read_exact(&mut body)?;
    if status != STATUS_OK {
        bail!("render server: {}", String::from_utf8_lossy(&body));
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_round_trips() {
        let mut buf = Vec::new();
        write_request(&mut buf, RUN, &[1, 2, 0xDEAD_BEEF]).unwrap();
        let mut r = &buf[..];
        assert_eq!(read_request(&mut r).unwrap(), Some((RUN, vec![1, 2, 0xDEAD_BEEF])));
        assert_eq!(read_request(&mut r).unwrap(), None);
        let mut buf = Vec::new();
        write_response(&mut buf, STATUS_ERROR, b"nope").unwrap();
        assert!(read_response(&mut &buf[..]).unwrap_err().to_string().contains("nope"));
    }
}
