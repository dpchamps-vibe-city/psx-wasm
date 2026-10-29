//! The host's renderer behind the `render_submit`, `render_readback` and
//! `present` imports (`psxw_abi::render`). Headless runs default to
//! [`NullRender`], which executes nothing: readbacks are black. A real
//! backend (the render server in its pinned image, M4) plugs in through
//! [`Render`].

use anyhow::Result;
use psxw_abi::render::{PresentDesc, Rect};
use serde::Serialize;

pub trait Render: Send {
    /// Execute renderer commands.
    fn submit(&mut self, words: &[u32]) -> Result<()>;
    /// A VRAM rectangle after everything submitted, `w * h` pixels.
    fn readback(&mut self, r: Rect) -> Result<Vec<u16>>;
    /// Show a frame.
    fn present(&mut self, desc: &PresentDesc) -> Result<()>;
    /// The whole of VRAM (1024 × 512), when the backend has one.
    fn vram(&mut self) -> Result<Option<Vec<u16>>> {
        Ok(None)
    }
    /// The last presented frame as (width, height, RGBA8 rows from the
    /// top), when the backend draws.
    fn frame(&mut self) -> Result<Option<(u32, u32, Vec<u8>)>> {
        Ok(None)
    }
}

/// Executes nothing: readbacks are zeros.
#[derive(Debug, Default)]
pub struct NullRender;

impl Render for NullRender {
    fn submit(&mut self, _words: &[u32]) -> Result<()> {
        Ok(())
    }
    fn readback(&mut self, r: Rect) -> Result<Vec<u16>> {
        Ok(vec![0; (r.w * r.h) as usize])
    }
    fn present(&mut self, _desc: &PresentDesc) -> Result<()> {
        Ok(())
    }
}

/// What the run sent the renderer.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RenderStats {
    pub submits: u64,
    pub words: u64,
    pub readbacks: u64,
    pub presents: u64,
    /// The last display the game presented.
    pub last_present: Option<[u32; 5]>,
    /// FNV-1a over every renderer command word submitted, in order: the
    /// GPU's output however it was batched (conformance compares it).
    pub hash: u64,
}

pub type SharedRender = std::sync::Arc<std::sync::Mutex<Box<dyn Render>>>;

impl std::fmt::Debug for dyn Render {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("dyn Render")
    }
}

/// An RGBA8 image as a PNG (stored, uncompressed: frames for humans, not
/// archives).
pub fn png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    let mut raw = Vec::with_capacity((width * 4 + 1) as usize * height as usize);
    for row in rgba.chunks_exact((width * 4) as usize).take(height as usize) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    // zlib: stored blocks of at most 65535 bytes, then Adler-32.
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = if raw.is_empty() { vec![&[][..]] } else { raw.chunks(65535).collect() };
    for (i, b) in blocks.iter().enumerate() {
        z.push((i + 1 == blocks.len()) as u8);
        z.extend_from_slice(&(b.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(b.len() as u16)).to_le_bytes());
        z.extend_from_slice(b);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&(b << 16 | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn png_shape() {
        let p = super::png(2, 1, &[255, 0, 0, 255, 0, 255, 0, 255]);
        assert_eq!(&p[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&p[12..16], b"IHDR");
        assert_eq!(&p[p.len() - 8..p.len() - 4], b"IEND");
    }
}
