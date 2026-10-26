//! Exact-by-construction GPU vectors: the harness self-test, and the
//! "verified" subset for M4.
//!
//! Every generator uses only operations whose result is fully determined by
//! the vector's own data, and computes that result in closed form, pixel by
//! pixel, from the definition of the operation:
//!
//! - `fill`: GP0(02h) with x and w multiples of 16, no wrap: every pixel of
//!   the rectangle becomes the 24-bit colour truncated to 15 bits, mask 0.
//! - `copy`: GP0(80h), non-overlapping, no mask settings: destination
//!   pixels equal source pixels, mask bit included.
//! - `upload`: CPU→VRAM (GP0(A0h)) then VRAM→CPU (GP0(C0h)): the same
//!   pixels come back.
//! - `sprite4`/`sprite8`/`sprite15`: 1:1 textured rectangles (variable,
//!   1×1, 8×8, 16×16), opaque, raw texture or modulation by 808080h, no
//!   dithering, no texture window, no flip, no texture-coordinate wrap:
//!   each pixel is its texel (through the colour table for 4 and 8 bits),
//!   except texel 0000h, which leaves the pixel unchanged.
//! - `flat`: untextured opaque rectangles (variable, 1×1, 8×8, 16×16): the
//!   colour truncated to 15 bits, mask 0.
//!
//! The draw state is fixed: drawing area the whole of VRAM, offset 0, mask
//! settings 0, texture window 0, dithering off. Every pixel read back is
//! initialised by the vector. A seeded generator makes the set
//! reproducible; the seed goes into the report.

use crate::store::Store;
use crate::vector::{GpuDraw, GpuMode, GpuTransfer, Rect, TransferStep, Vector, VramRect, pixels_to_bytes};
use anyhow::Result;

/// SplitMix64: small, fast, and stable across platforms and releases.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (n > 0).
    pub fn below(&mut self, n: u32) -> u32 {
        (((self.next_u64() >> 32) * n as u64) >> 32) as u32
    }

    /// Uniform in `lo..=hi`.
    pub fn range(&mut self, lo: u32, hi: u32) -> u32 {
        lo + self.below(hi - lo + 1)
    }

    pub fn chance(&mut self, one_in: u32) -> bool {
        self.below(one_in) == 0
    }

    pub fn u16(&mut self) -> u16 {
        self.next_u64() as u16
    }
}

/// A generated vector and its closed-form result.
pub struct Generated {
    pub generator: &'static str,
    pub vector: Vector,
    pub expected: Vec<u8>,
}

pub const GENERATORS: [&str; 7] = ["fill", "copy", "upload", "sprite4", "sprite8", "sprite15", "flat"];

/// `count` vectors from `seed`, cycling through the generators. Vector i
/// uses its own stream (seed, i), so the set is stable when count grows.
pub fn generate(store: &Store, seed: u64, count: usize) -> Result<Vec<Generated>> {
    (0..count)
        .map(|i| {
            let mut rng = Rng::new(seed ^ (i as u64).wrapping_mul(0xD1B5_4A32_D192_ED03));
            let name = GENERATORS[i % GENERATORS.len()];
            let mut scene = Scene::default();
            let (vector, expected) = match name {
                "fill" => fill(&mut rng, &mut scene, store)?,
                "copy" => copy(&mut rng, &mut scene, store)?,
                "upload" => upload(&mut rng, &mut scene, store)?,
                "sprite4" => sprite(&mut rng, &mut scene, store, Depth::Four)?,
                "sprite8" => sprite(&mut rng, &mut scene, store, Depth::Eight)?,
                "sprite15" => sprite(&mut rng, &mut scene, store, Depth::Fifteen)?,
                _ => flat(&mut rng, &mut scene, store)?,
            };
            Ok(Generated { generator: name, vector, expected })
        })
        .collect()
}

/// The VRAM a vector initialises, for computing expected pixels.
#[derive(Default)]
struct Scene {
    rects: Vec<(Rect, Vec<u16>)>,
}

impl Scene {
    fn add(&mut self, store: &Store, rect: Rect, px: Vec<u16>) -> Result<VramRect> {
        let blob = store.put_blob(&pixels_to_bytes(&px))?;
        self.rects.push((rect, px));
        Ok(VramRect { rect, pixels: blob })
    }

    /// The initial pixel at (x, y); generators only read initialised pixels.
    fn get(&self, x: u16, y: u16) -> u16 {
        for (r, px) in self.rects.iter().rev() {
            if r.contains(x, y) {
                return px[(y - r.y) as usize * r.w as usize + (x - r.x) as usize];
            }
        }
        panic!("generator read uninitialised VRAM at ({x}, {y})");
    }
}

/// Random pixels; one in `zero_one_in` is 0000h (0 = never).
fn random_pixels(rng: &mut Rng, n: usize, zero_one_in: u32) -> Vec<u16> {
    (0..n).map(|_| if zero_one_in > 0 && rng.chance(zero_one_in) { 0 } else { rng.u16() }).collect()
}

/// `r` grown by up to 8 pixels per side, clipped to VRAM.
fn with_margin(rng: &mut Rng, r: Rect) -> Rect {
    let l = rng.below(9).min(r.x as u32) as u16;
    let t = rng.below(9).min(r.y as u32) as u16;
    let right = rng.below(9).min(1024 - (r.x + r.w) as u32) as u16;
    let bottom = rng.below(9).min(512 - (r.y + r.h) as u32) as u16;
    Rect::new(r.x - l, r.y - t, r.w + l + right, r.h + t + bottom)
}

/// A rect of the given size at a random place.
fn place(rng: &mut Rng, w: u16, h: u16) -> Rect {
    Rect::new(rng.below(1024 - w as u32 + 1) as u16, rng.below(512 - h as u32 + 1) as u16, w, h)
}

/// A rect that avoids `taken`, retrying random placements.
fn place_clear(rng: &mut Rng, w: u16, h: u16, taken: &[Rect]) -> Rect {
    loop {
        let r = place(rng, w, h);
        if taken.iter().all(|t| !t.overlaps(&r)) {
            return r;
        }
    }
}

fn to15(color: u32) -> u16 {
    ((color & 0xFF) >> 3 | ((color >> 8) & 0xFF) >> 3 << 5 | ((color >> 16) & 0xFF) >> 3 << 10) as u16
}

/// Draw state for every generator: texpage word, then E2h..E6h.
fn draw_state(texpage: u32) -> Vec<u32> {
    vec![
        0xE100_0000 | texpage | 1 << 10, // drawing to the display area allowed, no dither
        0xE200_0000,                     // no texture window
        0xE300_0000,                     // drawing area top-left (0, 0)
        0xE400_0000 | 511 << 10 | 1023,  // drawing area bottom-right (1023, 511)
        0xE500_0000,                     // offset 0
        0xE600_0000,                     // mask settings off
    ]
}

fn random_mode(rng: &mut Rng) -> GpuMode {
    [GpuMode::Port, GpuMode::Block, GpuMode::List][rng.below(3) as usize]
}

/// Readback of `area`, with `f` giving each pixel's expected value.
fn expect(area: Rect, f: impl Fn(u16, u16) -> u16) -> Vec<u8> {
    let mut px = Vec::with_capacity(area.pixels());
    for y in area.y..area.y + area.h {
        for x in area.x..area.x + area.w {
            px.push(f(x, y));
        }
    }
    pixels_to_bytes(&px)
}

fn fill(rng: &mut Rng, s: &mut Scene, store: &Store) -> Result<(Vector, Vec<u8>)> {
    let w = 16 * rng.range(1, 4) as u16;
    let h = rng.range(1, 48) as u16;
    let r = Rect::new(16 * rng.below((1024 - w as u32) / 16 + 1) as u16, rng.below(512 - h as u32 + 1) as u16, w, h);
    let area = with_margin(rng, r);
    let init = s.add(store, area, random_pixels(rng, area.pixels(), 0))?;
    let color = rng.next_u64() as u32 & 0xFF_FFFF;
    let c15 = to15(color);
    let v = Vector::GpuDraw(GpuDraw {
        init: vec![init],
        state: draw_state(0),
        mode: random_mode(rng),
        packets: vec![vec![0x0200_0000 | color, (r.y as u32) << 16 | r.x as u32, (r.h as u32) << 16 | r.w as u32]],
        readback: vec![area],
    });
    Ok((v, expect(area, |x, y| if r.contains(x, y) { c15 } else { s.get(x, y) })))
}

fn copy(rng: &mut Rng, s: &mut Scene, store: &Store) -> Result<(Vector, Vec<u8>)> {
    let (w, h) = (rng.range(1, 64) as u16, rng.range(1, 48) as u16);
    let src = place(rng, w, h);
    let dst = place_clear(rng, w, h, &[src]);
    let mut area = with_margin(rng, dst);
    if area.overlaps(&src) {
        area = dst;
    }
    let init = vec![
        s.add(store, src, random_pixels(rng, src.pixels(), 0))?,
        s.add(store, area, random_pixels(rng, area.pixels(), 0))?,
    ];
    let v = Vector::GpuDraw(GpuDraw {
        init,
        state: draw_state(0),
        mode: random_mode(rng),
        packets: vec![vec![
            0x8000_0000,
            (src.y as u32) << 16 | src.x as u32,
            (dst.y as u32) << 16 | dst.x as u32,
            (h as u32) << 16 | w as u32,
        ]],
        readback: vec![area, src],
    });
    let mut want = expect(area, |x, y| if dst.contains(x, y) { s.get(src.x + (x - dst.x), src.y + (y - dst.y)) } else { s.get(x, y) });
    want.extend(expect(src, |x, y| s.get(x, y)));
    Ok((v, want))
}

fn upload(rng: &mut Rng, s: &mut Scene, store: &Store) -> Result<(Vector, Vec<u8>)> {
    let (w, h) = (rng.range(1, 64) as u16, rng.range(1, 48) as u16);
    let r = place(rng, w, h);
    let area = with_margin(rng, r);
    let init = s.add(store, area, random_pixels(rng, area.pixels(), 0))?;
    let data = random_pixels(rng, r.pixels(), 0);
    let blob = store.put_blob(&pixels_to_bytes(&data))?;
    let v = Vector::GpuTransfer(GpuTransfer {
        init: vec![init],
        state: vec![0xE600_0000],
        steps: vec![TransferStep::CpuToVram(VramRect { rect: r, pixels: blob }), TransferStep::VramToCpu(r)],
        readback: vec![area],
    });
    let at = |x: u16, y: u16| data[(y - r.y) as usize * r.w as usize + (x - r.x) as usize];
    let mut want = pixels_to_bytes(&data);
    want.extend(expect(area, |x, y| if r.contains(x, y) { at(x, y) } else { s.get(x, y) }));
    Ok((v, want))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Depth {
    Four,
    Eight,
    Fifteen,
}

fn sprite(rng: &mut Rng, s: &mut Scene, store: &Store, depth: Depth) -> Result<(Vector, Vec<u8>)> {
    // Size and opcode: variable, 1x1, 8x8 or 16x16; modulated or raw.
    let (base, w, h) = match rng.below(4) {
        0 => (0x64u32, rng.range(1, 64) as u16, rng.range(1, 64) as u16),
        1 => (0x6C, 1, 1),
        2 => (0x74, 8, 8),
        _ => (0x7C, 16, 16),
    };
    let raw = rng.chance(2);
    let op = base | raw as u32;
    let color = if raw { rng.next_u64() as u32 & 0xFF_FFFF } else { 0x80_8080 };

    // Texture page and texture coordinates, with no wrap in u, v or VRAM x.
    // Page widths in VRAM: 64, 128 and 256 pixels.
    let (mode_bits, max_tpx) = match depth {
        Depth::Four => (0u32, 15u32),
        Depth::Eight => (1, 14),
        Depth::Fifteen => (2, 12),
    };
    let tpx = rng.range(0, max_tpx);
    let tpy = rng.below(2);
    let (tx, ty) = ((64 * tpx) as u16, (256 * tpy) as u16);
    let u0 = rng.below(256 - w as u32 + 1) as u16;
    let v0 = rng.below(256 - h as u32 + 1) as u16;
    let (col_lo, col_hi) = match depth {
        Depth::Four => (u0 / 4, (u0 + w - 1) / 4),
        Depth::Eight => (u0 / 2, (u0 + w - 1) / 2),
        Depth::Fifteen => (u0, u0 + w - 1),
    };
    let tex = Rect::new(tx + col_lo, ty + v0, col_hi - col_lo + 1, h);

    // Colour table (4 and 8 bits) clear of the texture.
    let entries = match depth {
        Depth::Four => 16u16,
        Depth::Eight => 256,
        Depth::Fifteen => 0,
    };
    let clut = if entries > 0 {
        loop {
            let cx = rng.below((1024 - entries as u32) / 16 + 1) as u16;
            let r = Rect::new(16 * cx, rng.below(512) as u16, entries, 1);
            if !r.overlaps(&tex) {
                break Some(r);
            }
        }
    } else {
        None
    };
    let mut taken = vec![tex];
    taken.extend(clut);
    let dst = place_clear(rng, w, h, &taken);
    let mut area = with_margin(rng, dst);
    if taken.iter().any(|t| t.overlaps(&area)) {
        area = dst;
    }

    let mut init = vec![s.add(store, tex, random_pixels(rng, tex.pixels(), if depth == Depth::Fifteen { 8 } else { 0 }))?];
    if let Some(c) = clut {
        init.push(s.add(store, c, random_pixels(rng, c.pixels(), 8))?);
    }
    init.push(s.add(store, area, random_pixels(rng, area.pixels(), 0))?);

    let clut_field = clut.map(|c| (c.y as u32) << 6 | (c.x as u32 / 16)).unwrap_or(0);
    let mut packet = vec![
        op << 24 | color,
        (dst.y as u32) << 16 | dst.x as u32,
        clut_field << 16 | (v0 as u32) << 8 | u0 as u32,
    ];
    if base == 0x64 {
        packet.push((h as u32) << 16 | w as u32);
    }
    let semi = rng.below(4) << 5; // ABR: no effect on opaque primitives
    let v = Vector::GpuDraw(GpuDraw {
        init,
        state: draw_state(tpx | tpy << 4 | semi | mode_bits << 7),
        mode: random_mode(rng),
        packets: vec![packet],
        readback: vec![area],
    });

    let texel = |u: u16, v: u16| -> u16 {
        let row = ty + v;
        match depth {
            Depth::Four => {
                let word = s.get(tx + u / 4, row);
                let idx = (word >> (4 * (u % 4))) & 0xF;
                let c = clut.expect("4-bit has a CLUT");
                s.get(c.x + idx, c.y)
            }
            Depth::Eight => {
                let word = s.get(tx + u / 2, row);
                let idx = (word >> (8 * (u % 2))) & 0xFF;
                let c = clut.expect("8-bit has a CLUT");
                s.get(c.x + idx, c.y)
            }
            Depth::Fifteen => s.get(tx + u, row),
        }
    };
    let want = expect(area, |x, y| {
        if dst.contains(x, y) {
            let t = texel(u0 + (x - dst.x), v0 + (y - dst.y));
            if t == 0 { s.get(x, y) } else { t }
        } else {
            s.get(x, y)
        }
    });
    Ok((v, want))
}

fn flat(rng: &mut Rng, s: &mut Scene, store: &Store) -> Result<(Vector, Vec<u8>)> {
    let (op, w, h) = match rng.below(4) {
        0 => (0x60u32, rng.range(1, 64) as u16, rng.range(1, 64) as u16),
        1 => (0x68, 1, 1),
        2 => (0x70, 8, 8),
        _ => (0x78, 16, 16),
    };
    let r = place(rng, w, h);
    let area = with_margin(rng, r);
    let init = s.add(store, area, random_pixels(rng, area.pixels(), 0))?;
    let color = rng.next_u64() as u32 & 0xFF_FFFF;
    let mut packet = vec![op << 24 | color, (r.y as u32) << 16 | r.x as u32];
    if op == 0x60 {
        packet.push((h as u32) << 16 | w as u32);
    }
    let v = Vector::GpuDraw(GpuDraw { init: vec![init], state: draw_state(0), mode: random_mode(rng), packets: vec![packet], readback: vec![area] });
    let c15 = to15(color);
    Ok((v, expect(area, |x, y| if r.contains(x, y) { c15 } else { s.get(x, y) })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_stable() {
        let mut r = Rng::new(0);
        assert_eq!(r.next_u64(), 0xE220_A839_7B1D_CDAF);
        let mut r = Rng::new(7);
        for _ in 0..1000 {
            assert!(r.below(10) < 10);
        }
    }

    #[test]
    fn colour_truncation() {
        assert_eq!(to15(0xFFFFFF), 0x7FFF);
        assert_eq!(to15(0x0000F8), 0x001F);
        assert_eq!(to15(0x00F800), 0x03E0);
        assert_eq!(to15(0xF80000), 0x7C00);
        assert_eq!(to15(0x070707), 0);
    }

    #[test]
    fn generation_is_reproducible_and_valid() {
        let dir = std::env::temp_dir().join(format!("psxw-oracle-gen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::new(&dir);
        let a = generate(&store, 42, 70).unwrap();
        let b = generate(&store, 42, 70).unwrap();
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.vector, y.vector);
            assert_eq!(x.expected, y.expected);
            x.vector.check().unwrap();
            assert_eq!(x.expected.len(), x.vector.result_len());
            crate::script::compile(&x.vector, &store).unwrap();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The vectors must do something: a draw that leaves its readback as
    /// initialised would pass trivially.
    #[test]
    fn draws_change_pixels() {
        let dir = std::env::temp_dir().join(format!("psxw-oracle-gen2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::new(&dir);
        let mut changed: std::collections::BTreeMap<&str, (usize, usize)> = Default::default();
        for g in generate(&store, 7, 700).unwrap() {
            let Vector::GpuDraw(d) = &g.vector else { continue };
            // The readback's first rect is the last init rect for every draw generator.
            let init = store.blob(&d.init.last().unwrap().pixels).unwrap();
            let e = changed.entry(g.generator).or_default();
            e.0 += 1;
            if g.expected[..init.len()] != init[..] {
                e.1 += 1;
            }
        }
        for (name, (n, c)) in changed {
            assert!(c * 10 >= n * 9, "{name}: only {c} of {n} vectors change pixels");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
