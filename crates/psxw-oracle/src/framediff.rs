//! The displayed frame and VRAM at the whole-game oracle's aligned points
//! (plans/M13-bof4-static-recompilation.md task 21). Both sides write the
//! picture presented at each counter checkpoint beside its RAM dump:
//! psx-wasm as a PNG (`render::png`, stored deflate blocks), the emulator
//! host as a binary PPM. VRAM is 1024×512 16-bit words, little-endian.
//! Like the RAM comparison, this is a diagnostic (D48): differences are
//! measured and reported, never gated.

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;

/// A channel differs when |a − b| exceeds this: one 5-bit colour step
/// widened to 8 bits, so 15-bit colour expanded differently on each side
/// (`c << 3` or `c << 3 | c >> 2`) is not a difference.
pub const TOLERANCE: u8 = 8;
/// When sizes differ, offsets up to this many pixels per axis are tried;
/// beyond it the smaller picture is centred.
pub const MAX_SEARCH: u32 = 32;
pub const VRAM_WORDS: usize = 1024 * 512;

/// An RGB8 picture, rows from the top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

impl Image {
    /// A binary PPM (`P6`, maxval 255).
    pub fn from_ppm(b: &[u8]) -> Result<Image> {
        let mut fields = Vec::new();
        let mut at = 0;
        while fields.len() < 4 {
            while at < b.len() && b[at].is_ascii_whitespace() {
                at += 1;
            }
            if b.get(at) == Some(&b'#') {
                while at < b.len() && b[at] != b'\n' {
                    at += 1;
                }
                continue;
            }
            let start = at;
            while at < b.len() && !b[at].is_ascii_whitespace() {
                at += 1;
            }
            ensure!(at > start, "PPM header truncated");
            fields.push(std::str::from_utf8(&b[start..at])?);
        }
        ensure!(fields[0] == "P6", "not a binary PPM ({})", fields[0]);
        let (width, height, max): (u32, u32, u32) = (fields[1].parse()?, fields[2].parse()?, fields[3].parse()?);
        ensure!(max == 255, "PPM maxval {max} (only 255 is read)");
        let rgb = b.get(at + 1..).context("PPM header truncated")?;
        ensure!(rgb.len() == (width * height * 3) as usize, "{} pixel bytes for {width}x{height}", rgb.len());
        Ok(Image { width, height, rgb: rgb.to_vec() })
    }

    /// A PNG as psxw writes them (8-bit RGBA or RGB, no interlace,
    /// filter 0, stored deflate blocks): no inflate is needed.
    pub fn from_png(b: &[u8]) -> Result<Image> {
        ensure!(b.starts_with(b"\x89PNG\r\n\x1a\n"), "not a PNG");
        let (mut at, mut ihdr, mut z) = (8, None, Vec::new());
        while at + 8 <= b.len() {
            let len = u32::from_be_bytes(b[at..at + 4].try_into()?) as usize;
            let kind = &b[at + 4..at + 8];
            let data = b.get(at + 8..at + 8 + len).context("PNG chunk truncated")?;
            match kind {
                b"IHDR" => ihdr = Some(data.to_vec()),
                b"IDAT" => z.extend_from_slice(data),
                b"IEND" => break,
                _ => {}
            }
            at += 12 + len;
        }
        let h = ihdr.context("PNG without IHDR")?;
        ensure!(h.len() == 13, "PNG IHDR of {} bytes", h.len());
        let (width, height) = (u32::from_be_bytes(h[0..4].try_into()?), u32::from_be_bytes(h[4..8].try_into()?));
        let channels = match (h[8], h[9], h[12]) {
            (8, 6, 0) => 4,
            (8, 2, 0) => 3,
            (d, c, i) => bail!("PNG depth {d}, colour type {c}, interlace {i} (only 8-bit RGB/RGBA, not interlaced)"),
        };
        // zlib header, then deflate blocks; only stored ones (BTYPE 00).
        let mut raw = Vec::new();
        let mut p = 2;
        loop {
            let head = *z.get(p).context("deflate stream truncated")?;
            ensure!(head & 6 == 0, "compressed deflate block (only stored blocks, as psxw writes, are read)");
            let len = u16::from_le_bytes(z.get(p + 1..p + 3).context("deflate stream truncated")?.try_into()?) as usize;
            raw.extend_from_slice(z.get(p + 5..p + 5 + len).context("deflate block truncated")?);
            p += 5 + len;
            if head & 1 == 1 {
                break;
            }
        }
        let stride = width as usize * channels + 1;
        ensure!(raw.len() == stride * height as usize, "{} image bytes for {width}x{height}", raw.len());
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for row in raw.chunks_exact(stride) {
            ensure!(row[0] == 0, "PNG row filter {} (only filter 0 is read)", row[0]);
            for px in row[1..].chunks_exact(channels) {
                rgb.extend_from_slice(&px[..3]);
            }
        }
        Ok(Image { width, height, rgb })
    }

    /// RGBA8 rows, for writing as a PNG.
    pub fn rgba(&self) -> Vec<u8> {
        self.rgb.chunks_exact(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect()
    }

    fn px(&self, x: u32, y: u32) -> &[u8] {
        let i = ((y * self.width + x) * 3) as usize;
        &self.rgb[i..i + 3]
    }

    /// Every `sx`-th column and `sy`-th row.
    fn sample(&self, sx: u32, sy: u32) -> Image {
        let (width, height) = (self.width / sx, self.height / sy);
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for y in 0..height {
            for x in 0..width {
                rgb.extend_from_slice(self.px(x * sx, y * sy));
            }
        }
        Image { width, height, rgb }
    }

    /// The most common colour as `#rrggbb` and its share of the pixels.
    pub fn dominant(&self) -> (String, f64) {
        let mut c: Vec<u32> = self.rgb.chunks_exact(3).map(|p| (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32).collect();
        if c.is_empty() {
            return ("-".into(), 0.0);
        }
        c.sort_unstable();
        let (mut best, mut best_n, mut i) = (c[0], 0usize, 0usize);
        while i < c.len() {
            let n = c[i..].iter().take_while(|&&v| v == c[i]).count();
            if n > best_n {
                (best, best_n) = (c[i], n);
            }
            i += n;
        }
        (format!("#{best:06x}"), best_n as f64 / c.len() as f64)
    }
}

/// Two pictures of one checkpoint compared.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ImageDiff {
    /// The compared region, after any scaling.
    pub compared: [u32; 2],
    /// How differing sizes were reconciled (absent when they agree).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Pixels with a channel differing by more than [`TOLERANCE`].
    pub differing_pixels: u64,
    pub differing_fraction: f64,
    /// Mean absolute difference per channel (0–255).
    pub mae: f64,
}

/// Sum of |a − b| and the differing pixels over a `w`×`h` window at
/// (ax, ay) in `a` and (bx, by) in `b`.
fn window(a: &Image, (ax, ay): (u32, u32), b: &Image, (bx, by): (u32, u32), w: u32, h: u32) -> (u64, u64) {
    let (mut sum, mut differing) = (0u64, 0u64);
    for y in 0..h {
        let ra = ((ay + y) * a.width + ax) as usize * 3;
        let rb = ((by + y) * b.width + bx) as usize * 3;
        for (pa, pb) in a.rgb[ra..ra + w as usize * 3].chunks_exact(3).zip(b.rgb[rb..rb + w as usize * 3].chunks_exact(3)) {
            let d = [pa[0].abs_diff(pb[0]), pa[1].abs_diff(pb[1]), pa[2].abs_diff(pb[2])];
            sum += d.iter().map(|&v| v as u64).sum::<u64>();
            differing += d.iter().any(|&v| v > TOLERANCE) as u64;
        }
    }
    (sum, differing)
}

/// Compares `a` (psx-wasm's picture) with `b` (the emulator's). Equal sizes
/// compare pixel for pixel. Otherwise an axis where one side is exactly
/// twice the other is point-sampled down (a 480-line or 640-pixel mode
/// shown at half size), and the smaller picture is placed in the larger
/// at the offset where they agree best, up to [`MAX_SEARCH`] pixels per
/// axis, else centred; `note` says what was done.
pub fn compare(a: &Image, b: &Image) -> ImageDiff {
    let mut notes = Vec::new();
    if (a.width, a.height) != (b.width, b.height) {
        notes.push(format!("sizes differ: psx-wasm {}x{}, emulator {}x{}", a.width, a.height, b.width, b.height));
    }
    let half = |m: u32, n: u32| if m == 2 * n && n > 0 { 2 } else { 1 };
    let (a, b) = {
        let (sa, sb) = ((half(a.width, b.width), half(a.height, b.height)), (half(b.width, a.width), half(b.height, a.height)));
        if sa != (1, 1) {
            notes.push(format!("psx-wasm's picture sampled 1/{}x1/{}", sa.0, sa.1));
        }
        if sb != (1, 1) {
            notes.push(format!("the emulator's picture sampled 1/{}x1/{}", sb.0, sb.1));
        }
        (if sa == (1, 1) { a.clone() } else { a.sample(sa.0, sa.1) }, if sb == (1, 1) { b.clone() } else { b.sample(sb.0, sb.1) })
    };
    let (w, h) = (a.width.min(b.width), a.height.min(b.height));
    let (dw, dh) = (a.width.abs_diff(b.width), a.height.abs_diff(b.height));
    let search = |d: u32| if d <= MAX_SEARCH { (0..=d).collect::<Vec<_>>() } else { vec![d / 2] };
    // (offset in a, offset in b) per axis: the larger side moves.
    let place = |o: u32, a_larger: bool| if a_larger { (o, 0) } else { (0, o) };
    let mut best: Option<((u32, u32), (u32, u32), (u64, u64))> = None;
    for oy in search(dh) {
        for ox in search(dw) {
            let (ax, bx) = place(ox, a.width > b.width);
            let (ay, by) = place(oy, a.height > b.height);
            let r = window(&a, (ax, ay), &b, (bx, by), w, h);
            if best.as_ref().is_none_or(|(_, _, s)| r.0 < s.0) {
                best = Some(((ax, ay), (bx, by), r));
            }
        }
    }
    let ((ax, ay), (bx, by), (sum, differing)) = best.unwrap_or(((0, 0), (0, 0), (0, 0)));
    if dw > 0 || dh > 0 {
        let how = if dw > MAX_SEARCH || dh > MAX_SEARCH { "centred" } else { "where they agree best" };
        notes.push(format!("compared the {w}x{h} overlap at psx-wasm ({ax},{ay}), emulator ({bx},{by}), {how}"));
    }
    let n = (w as u64 * h as u64).max(1);
    ImageDiff {
        compared: [w, h],
        note: (!notes.is_empty()).then(|| notes.join("; ")),
        differing_pixels: differing,
        differing_fraction: differing as f64 / n as f64,
        mae: sum as f64 / (n * 3) as f64,
    }
}

/// VRAM from a dump: 1024×512 16-bit words, little-endian.
pub fn vram(bytes: &[u8]) -> Result<Vec<u16>> {
    ensure!(bytes.len() == VRAM_WORDS * 2, "{} VRAM bytes, expected 1 MiB", bytes.len());
    Ok(bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect())
}

/// VRAM as RGBA8 for a human to look at (15-bit colour, mask bit dropped).
pub fn vram_rgba(v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|&p| [((p & 31) << 3) as u8, (((p >> 5) & 31) << 3) as u8, (((p >> 10) & 31) << 3) as u8, 255]).collect()
}

/// A 64×256 texture page with differing words.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PageDiff {
    pub x: u32,
    pub y: u32,
    pub words: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct VramDiff {
    pub differing_words: u64,
    pub differing_fraction: f64,
    /// The differences' bounding box, `[x0, y0, x1, y1]` inclusive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<[u32; 4]>,
    /// Pages (of the 32) with differences, by VRAM position.
    pub pages: Vec<PageDiff>,
}

/// Word-for-word comparison (the mask bit included).
pub fn compare_vram(a: &[u16], b: &[u16]) -> VramDiff {
    let mut pages = vec![0u32; 32];
    let (mut n, mut bbox) = (0u64, None::<[u32; 4]>);
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        if x == y {
            continue;
        }
        let (px, py) = ((i % 1024) as u32, (i / 1024) as u32);
        n += 1;
        pages[(py / 256 * 16 + px / 64) as usize] += 1;
        bbox = Some(match bbox {
            None => [px, py, px, py],
            Some([x0, y0, x1, y1]) => [x0.min(px), y0.min(py), x1.max(px), y1.max(py)],
        });
    }
    VramDiff {
        differing_words: n,
        differing_fraction: n as f64 / a.len().min(b.len()).max(1) as f64,
        bbox,
        pages: pages.iter().enumerate().filter(|(_, w)| **w > 0).map(|(i, &words)| PageDiff { x: i as u32 % 16 * 64, y: i as u32 / 16 * 256, words }).collect(),
    }
}

/// How one checkpoint's comparison came out.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    /// No pixel (word) differs beyond the tolerance.
    Equal,
    /// Differences, reported and never gated (D48).
    Differs,
    /// Nothing to compare; `reason` says why.
    Unresolved,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(width: u32, height: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Image {
        let rgb = (0..height).flat_map(|y| (0..width).map(move |x| (x, y))).flat_map(|(x, y)| f(x, y)).collect();
        Image { width, height, rgb }
    }

    /// A PNG the way `psxw_host_native::render::png` writes one (CRCs and
    /// Adler-32 are not checked by the reader, so zeros stand in).
    fn png(i: &Image) -> Vec<u8> {
        let mut raw = Vec::new();
        for row in i.rgba().chunks_exact(i.width as usize * 4) {
            raw.push(0);
            raw.extend_from_slice(row);
        }
        let mut z = vec![0x78, 0x01];
        let blocks: Vec<&[u8]> = raw.chunks(7).collect();
        for (k, b) in blocks.iter().enumerate() {
            z.push((k + 1 == blocks.len()) as u8);
            z.extend_from_slice(&(b.len() as u16).to_le_bytes());
            z.extend_from_slice(&(!(b.len() as u16)).to_le_bytes());
            z.extend_from_slice(b);
        }
        z.extend_from_slice(&[0; 4]);
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |kind: &[u8; 4], data: &[u8]| {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            out.extend_from_slice(&[0; 4]);
        };
        let mut ihdr = i.width.to_be_bytes().to_vec();
        ihdr.extend_from_slice(&i.height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(b"IHDR", &ihdr);
        chunk(b"IDAT", &z);
        chunk(b"IEND", &[]);
        out
    }

    #[test]
    fn decodes_both_formats() {
        let i = img(5, 3, |x, y| [x as u8 * 40, y as u8 * 80, 7]);
        assert_eq!(Image::from_png(&png(&i)).unwrap(), i);
        let mut ppm = b"P6\n# a comment\n5 3\n255\n".to_vec();
        ppm.extend_from_slice(&i.rgb);
        assert_eq!(Image::from_ppm(&ppm).unwrap(), i);
        assert!(Image::from_ppm(b"P6\n5 3\n255\n\x00").is_err());
    }

    #[test]
    fn measures_differences_beyond_the_tolerance() {
        let a = img(4, 2, |_, _| [100, 100, 100]);
        let b = img(4, 2, |x, _| if x == 0 { [100, 100, 200] } else { [104, 100, 100] });
        let d = compare(&a, &b);
        assert_eq!((d.compared, d.differing_pixels, d.note.clone()), ([4, 2], 2, None));
        assert_eq!(d.differing_fraction, 0.25);
        assert!((d.mae - (2.0 * 100.0 + 6.0 * 4.0) / 24.0).abs() < 1e-9);
        assert_eq!(a.dominant(), ("#646464".into(), 1.0));
    }

    #[test]
    fn reconciles_sizes() {
        // The same picture with a 2-pixel border: found at its offset.
        let inner = img(8, 6, |x, y| [(x * 30) as u8, (y * 40) as u8, 0]);
        let outer = img(12, 10, |x, y| if (2..10).contains(&x) && (2..8).contains(&y) { inner.px(x - 2, y - 2).try_into().unwrap() } else { [0; 3] });
        let d = compare(&inner, &outer);
        assert_eq!((d.compared, d.differing_pixels), ([8, 6], 0));
        assert!(d.note.unwrap().contains("emulator (2,2)"));
        // Twice as wide and tall: sampled down.
        let big = img(16, 12, |x, y| inner.px(x / 2, y / 2).try_into().unwrap());
        let d = compare(&big, &inner);
        assert_eq!((d.compared, d.differing_pixels), ([8, 6], 0));
        assert!(d.note.unwrap().contains("sampled 1/2x1/2"));
    }

    #[test]
    fn vram_pages_and_bounds() {
        let a = vec![0u16; VRAM_WORDS];
        let mut b = a.clone();
        b[3 * 1024 + 70] = 1;
        b[300 * 1024 + 1000] = 0x8000;
        let d = compare_vram(&a, &b);
        assert_eq!(d.differing_words, 2);
        assert_eq!(d.bbox, Some([70, 3, 1000, 300]));
        assert_eq!(d.pages, [PageDiff { x: 64, y: 0, words: 1 }, PageDiff { x: 960, y: 256, words: 1 }]);
        assert_eq!(vram(&vec![0; VRAM_WORDS * 2]).unwrap().len(), VRAM_WORDS);
        assert_eq!(&vram_rgba(&[0x7C1F])[..4], &[248, 0, 248, 255]);
    }
}
