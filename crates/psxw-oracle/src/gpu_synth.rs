//! Synthetic GPU vectors (M4, task 6): seeded generators that each isolate
//! one feature of the GPU — a primitive type, Gouraud shading, a texture
//! depth or window, a blend mode, dither, the mask bit, lines, clipping,
//! fills and transfers — so a mismatch points at one piece of the
//! backend. Their goldens come from the device target (`psxw oracle
//! capture`), so they are provisional.
//!
//! Every vector initialises every pixel it reads back and every texel
//! and colour-table entry its primitives can sample: textures fill their
//! whole page, and the destination area sits in the other half of VRAM.
//! A generator's vectors go to the set `synth-gpu-<generator>`, which
//! `psxw oracle check --device gpu` includes. `install` runs from
//! `psxw oracle capture` with the default seed and count, so the set is
//! the same on every machine.

use crate::exact::Rng;
use crate::store::Store;
use crate::vector::{GpuDraw, GpuMode, GpuTransfer, Rect, TransferStep, Vector, VramRect, pixels_to_bytes};
use anyhow::Result;

/// "PSXW", "M4", 1.
pub const SEED: u64 = 0x5053_5857_4D34_0001;
/// Vectors per generator.
pub const PER_GENERATOR: usize = 40;
/// Set name prefix.
pub const SET_PREFIX: &str = "synth-gpu-";

pub const GENERATORS: &[&str] = &[
    "poly-flat",
    "poly-gouraud",
    "poly-dither",
    "poly-tex4",
    "poly-tex8",
    "poly-tex15",
    "poly-texwin",
    "poly-texdither",
    "rect-flat",
    "rect-tex",
    "rect-flip",
    "rect-texwin",
    "line",
    "polyline",
    "blend-flat",
    "blend-tex",
    "mask",
    "clip",
    "fill",
    "copy",
    "upload",
    "quad-semi",
    "clut-cache",
    "tpage-state",
    "big",
    "coord-wrap",
    "misc",
];

/// A generated vector.
pub struct Synth {
    pub generator: &'static str,
    pub vector: Vector,
}

/// `per` vectors of every generator from `seed`. Vector i of generator g
/// uses its own stream, so the set is stable when `per` grows.
pub fn generate(store: &Store, seed: u64, per: usize) -> Result<Vec<Synth>> {
    let mut out = Vec::with_capacity(per * GENERATORS.len());
    for (g, &name) in GENERATORS.iter().enumerate() {
        for i in 0..per {
            let stream = seed ^ ((g as u64) << 48) ^ (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let mut b = B::new(Rng::new(stream), store);
            build(name, i, &mut b)?;
            out.push(Synth { generator: name, vector: b.finish()? });
        }
    }
    Ok(out)
}

/// Store the default synthetic set; returns how many vectors it holds.
pub fn install(store: &Store) -> Result<usize> {
    let all = generate(store, SEED, PER_GENERATOR)?;
    for &name in GENERATORS {
        let ids: Vec<_> = all
            .iter()
            .filter(|s| s.generator == name)
            .map(|s| store.put_vector(&s.vector))
            .collect::<Result<_>>()?;
        store.add_to_set(&format!("{SET_PREFIX}{name}"), ids)?;
    }
    Ok(all.len())
}

// ---------------------------------------------------------------------
// GP0 words

fn xy(x: i32, y: i32) -> u32 {
    (x as u32 & 0xFFFF) | (y as u32) << 16
}

fn e1(tpx: u32, tpy: u32, abr: u32, depth: u32, dither: bool, flip: u32) -> u32 {
    0xE100_0000 | tpx | tpy << 4 | abr << 5 | depth << 7 | (dither as u32) << 9 | 1 << 10 | flip << 12
}

fn e3(x: u32, y: u32) -> u32 {
    0xE300_0000 | x | y << 10
}

fn e4(x: u32, y: u32) -> u32 {
    0xE400_0000 | x | y << 10
}

fn e5(x: i32, y: i32) -> u32 {
    0xE500_0000 | (x as u32 & 2047) | (y as u32 & 2047) << 11
}

fn tpage(t: &Tex, abr: u32) -> u32 {
    t.tpx | t.tpy << 4 | abr << 5 | t.depth << 7
}

// ---------------------------------------------------------------------
// The vector builder

struct B<'a> {
    rng: Rng,
    store: &'a Store,
    init: Vec<VramRect>,
    state: Vec<u32>,
    packets: Vec<Vec<u32>>,
    readback: Vec<Rect>,
    transfer: Option<GpuTransfer>,
}

impl<'a> B<'a> {
    fn new(rng: Rng, store: &'a Store) -> Self {
        B { rng, store, init: Vec::new(), state: Vec::new(), packets: Vec::new(), readback: Vec::new(), transfer: None }
    }

    fn r(&mut self, lo: i32, hi: i32) -> i32 {
        lo + self.rng.below((hi - lo + 1) as u32) as i32
    }

    fn coin(&mut self) -> bool {
        self.rng.chance(2)
    }

    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.rng.below(xs.len() as u32) as usize]
    }

    fn rgb(&mut self) -> u32 {
        self.rng.next_u64() as u32 & 0xFF_FFFF
    }

    /// Initialise a rectangle with pixels from `f`.
    fn area(&mut self, r: Rect, mut f: impl FnMut(&mut Rng) -> u16) -> Result<()> {
        let px: Vec<u16> = (0..r.pixels()).map(|_| f(&mut self.rng)).collect();
        let blob = self.store.put_blob(&pixels_to_bytes(&px))?;
        self.init.push(VramRect { rect: r, pixels: blob });
        Ok(())
    }

    fn random_area(&mut self, r: Rect) -> Result<()> {
        self.area(r, |g| g.u16())
    }

    /// One packet. At most the FIFO depth: the device target's
    /// linked-list DMA checks the GPU's readiness per node, so a longer
    /// command arriving while the GPU is still drawing loses words.
    fn packet(&mut self, words: Vec<u32>) {
        assert!(words.len() <= 16, "packet of {} words", words.len());
        self.packets.push(words);
    }

    fn finish(mut self) -> Result<Vector> {
        if let Some(mut t) = self.transfer.take() {
            t.init = self.init;
            t.readback = self.readback;
            let v = Vector::GpuTransfer(t);
            v.check()?;
            return Ok(v);
        }
        let mode = [GpuMode::Port, GpuMode::Block, GpuMode::List][self.rng.below(3) as usize];
        let v = Vector::GpuDraw(GpuDraw { init: self.init, state: self.state, mode, packets: self.packets, readback: self.readback });
        v.check()?;
        Ok(v)
    }

    /// A destination area of `w`×`h` in VRAM rows `[y_lo, y_hi)`,
    /// initialised and read back.
    fn dest(&mut self, w: i32, h: i32, y_lo: i32, y_hi: i32) -> Result<Rect> {
        let x = self.r(0, 1024 - w);
        let y = self.r(y_lo, y_hi - h);
        let r = Rect::new(x as u16, y as u16, w as u16, h as u16);
        self.random_area(r)?;
        self.readback.push(r);
        Ok(r)
    }

    /// Draw state: whole-VRAM drawing area, offset at `origin`, the given
    /// draw mode, no window, mask settings `mask`.
    fn state(&mut self, e1w: u32, window: u32, origin: (i32, i32), mask: u32) {
        self.state = vec![e1w, 0xE200_0000 | window, e3(0, 0), e4(1023, 511), e5(origin.0, origin.1), 0xE600_0000 | mask];
    }
}

/// A texture page with its colour table.
#[derive(Clone, Copy)]
struct Tex {
    depth: u32,
    tpx: u32,
    tpy: u32,
    /// Colour-table attribute (`y << 6 | x / 16`).
    clut: u32,
}

/// A whole texture page in VRAM half `half` (0 = rows 0..256), and its
/// colour table beside it.
fn texture(b: &mut B, depth: u32, half: u32) -> Result<Tex> {
    let w = 64u32 << depth;
    let tpx = b.rng.below((1024 - w) / 64 + 1);
    let tpy = half;
    let page = Rect::new((tpx * 64) as u16, (tpy * 256) as u16, w as u16, 256);
    if depth == 2 {
        b.area(page, |g| if g.chance(8) { 0 } else { g.u16() })?;
    } else {
        b.random_area(page)?;
    }
    let mut clut = 0;
    if depth < 2 {
        let entries = if depth == 0 { 16 } else { 256 };
        let (cx, cy) = loop {
            let cx = 16 * b.rng.below((1024 - entries) / 16 + 1);
            if cx + entries <= tpx * 64 || cx >= tpx * 64 + w {
                break (cx, tpy * 256 + b.rng.below(256));
            }
        };
        b.area(Rect::new(cx as u16, cy as u16, entries as u16, 1), |g| if g.chance(8) { 0 } else { g.u16() })?;
        clut = cy << 6 | cx / 16;
    }
    Ok(Tex { depth, tpx, tpy, clut })
}

/// A polygon packet: opcode bits from `op`, `n` vertices in
/// `[x0, x1] × [y0, y1]`, texture attributes when textured.
fn polygon(b: &mut B, op: u32, area: (i32, i32, i32, i32), tex: Option<(&Tex, u32)>) -> Vec<u32> {
    let gouraud = op & 0x10 != 0;
    let textured = op & 0x04 != 0;
    let n = if op & 0x08 != 0 { 4 } else { 3 };
    let mut w = vec![op << 24 | b.rgb()];
    for v in 0..n {
        if v > 0 && gouraud {
            w.push(b.rgb());
        }
        let (x, y) = (b.r(area.0, area.1), b.r(area.2, area.3));
        w.push(xy(x, y));
        if textured {
            let (t, abr) = tex.expect("textured polygon needs a texture");
            let attr = match v {
                0 => t.clut,
                1 => tpage(t, abr),
                _ => 0,
            };
            w.push(attr << 16 | b.rng.below(65536));
        }
    }
    w
}

/// A rectangle packet. `size`: 0 variable, 1 = 1×1, 2 = 8×8, 3 = 16×16.
fn rect(b: &mut B, op: u32, area: (i32, i32, i32, i32), tex: Option<&Tex>) -> Vec<u32> {
    let size = (op >> 3) & 3;
    let mut w = vec![op << 24 | b.rgb()];
    let (x, y) = (b.r(area.0, area.1), b.r(area.2, area.3));
    w.push(xy(x, y));
    if op & 0x04 != 0 {
        let t = tex.expect("textured rectangle needs a texture");
        w.push(t.clut << 16 | b.rng.below(65536));
    }
    if size == 0 {
        let (rw, rh) = (b.r(1, (area.1 - area.0).max(1) + 8), b.r(1, (area.3 - area.2).max(1) + 8));
        w.push((rh as u32) << 16 | rw as u32);
    }
    w
}

/// A line or polyline packet with `pts` points (terminated if poly).
fn line(b: &mut B, op: u32, pts: usize, area: (i32, i32, i32, i32)) -> Vec<u32> {
    let gouraud = op & 0x10 != 0;
    let mut w = vec![op << 24 | b.rgb()];
    for p in 0..pts {
        if p > 0 && gouraud {
            w.push(b.rgb());
        }
        let (x, y) = match b.rng.below(6) {
            // Sometimes axis-aligned or repeated.
            0 if p > 0 => {
                let prev = w[w.len() - 1 - gouraud as usize];
                ((prev & 0xFFFF) as u16 as i16 as i32, b.r(area.2, area.3))
            }
            1 if p > 0 => {
                let prev = w[w.len() - 1 - gouraud as usize];
                (b.r(area.0, area.1), (prev >> 16) as u16 as i16 as i32)
            }
            _ => (b.r(area.0, area.1), b.r(area.2, area.3)),
        };
        w.push(xy(x, y));
    }
    if op & 0x08 != 0 {
        w.push(if b.coin() { 0x5555_5555 } else { 0x5000_5000 });
    }
    w
}

/// The usual scene: a destination area (offset at its origin) in one half
/// of VRAM, a texture page of `depth` in the other when `depth` is Some.
fn scene(b: &mut B, depth: Option<u32>) -> Result<(Rect, Option<Tex>)> {
    let half = b.rng.below(2);
    let tex = match depth {
        Some(d) => Some(texture(b, d, half)?),
        None => None,
    };
    let (w, h) = (b.r(32, 160), b.r(32, 120));
    let other = 1 - half as i32;
    let d = b.dest(w, h, other * 256, other * 256 + 256)?;
    Ok((d, tex))
}

fn local(d: &Rect, m: i32) -> (i32, i32, i32, i32) {
    (-m, d.w as i32 + m, -m, d.h as i32 + m)
}

fn build(name: &str, i: usize, b: &mut B) -> Result<()> {
    match name {
        "poly-flat" | "poly-gouraud" | "poly-dither" => {
            let (d, _) = scene(b, None)?;
            let dither = name == "poly-dither" || (name == "poly-flat" && b.coin());
            b.state(e1(0, 0, 0, 0, dither, 0), 0, (d.x as i32, d.y as i32), 0);
            let ops: &[u32] = if name == "poly-flat" { &[0x20, 0x28] } else { &[0x30, 0x38] };
            for _ in 0..b.r(1, 3) {
                let op = b.pick(ops);
                let p = polygon(b, op, local(&d, 8), None);
                b.packet(p);
            }
        }
        "poly-tex4" | "poly-tex8" | "poly-tex15" | "poly-texwin" | "poly-texdither" => {
            let depth = match name {
                "poly-tex4" => 0,
                "poly-tex8" => 1,
                "poly-tex15" => 2,
                _ => b.rng.below(3),
            };
            let (d, t) = scene(b, Some(depth))?;
            let t = t.expect("texture");
            let dither = name == "poly-texdither";
            let window = if name == "poly-texwin" { b.rng.below(1 << 20) } else { 0 };
            b.state(e1(0, 0, 0, 0, dither, 0), window, (d.x as i32, d.y as i32), 0);
            let ops: &[u32] = if dither { &[0x24, 0x2C, 0x34, 0x3C] } else { &[0x24, 0x25, 0x2C, 0x2D, 0x34, 0x35, 0x3C, 0x3D] };
            for _ in 0..b.r(1, 3) {
                let op = b.pick(ops);
                let abr = b.rng.below(4);
                let p = polygon(b, op, local(&d, 8), Some((&t, abr)));
                b.packet(p);
            }
        }
        "rect-flat" => {
            let (d, _) = scene(b, None)?;
            let dither = b.coin();
            b.state(e1(0, 0, 0, 0, dither, 0), 0, (d.x as i32, d.y as i32), 0);
            for _ in 0..b.r(1, 4) {
                let op = b.pick(&[0x60, 0x68, 0x70, 0x78]);
                let p = rect(b, op, local(&d, 8), None);
                b.packet(p);
            }
        }
        "rect-tex" | "rect-flip" | "rect-texwin" => {
            let depth = b.rng.below(3);
            let (d, t) = scene(b, Some(depth))?;
            let t = t.expect("texture");
            let flip = if name == "rect-flip" { b.r(1, 3) as u32 } else { 0 };
            let window = if name == "rect-texwin" { b.rng.below(1 << 20) } else { 0 };
            let dither = b.coin();
            let abr = b.rng.below(4);
            b.state(e1(t.tpx, t.tpy, abr, t.depth, dither, flip), window, (d.x as i32, d.y as i32), 0);
            for _ in 0..b.r(1, 4) {
                let op = b.pick(&[0x64, 0x65, 0x6C, 0x6D, 0x74, 0x75, 0x7C, 0x7D]);
                let p = rect(b, op, local(&d, 8), Some(&t));
                b.packet(p);
            }
        }
        "line" => {
            let (d, _) = scene(b, None)?;
            let dither = b.coin();
            b.state(e1(0, 0, 0, 0, dither, 0), 0, (d.x as i32, d.y as i32), 0);
            for _ in 0..b.r(1, 4) {
                let op = b.pick(&[0x40, 0x50]);
                let p = line(b, op, 2, local(&d, 16));
                b.packet(p);
            }
        }
        "polyline" => {
            let (d, _) = scene(b, None)?;
            let dither = b.coin();
            b.state(e1(0, 0, 0, 0, dither, 0), 0, (d.x as i32, d.y as i32), 0);
            // Up to 16 words with the terminator.
            let op = b.pick(&[0x48, 0x58]);
            let pts = b.r(2, if op == 0x48 { 14 } else { 7 }) as usize;
            let p = line(b, op, pts, local(&d, 12));
            b.packet(p);
        }
        "blend-flat" => {
            let (d, _) = scene(b, None)?;
            let abr = (i % 4) as u32;
            let dither = b.coin();
            b.state(e1(0, 0, abr, 0, dither, 0), 0, (d.x as i32, d.y as i32), 0);
            for _ in 0..b.r(2, 4) {
                let p = match b.rng.below(3) {
                    0 => {
                        let op = b.pick(&[0x22, 0x2A, 0x32, 0x3A]);
                        polygon(b, op, local(&d, 8), None)
                    }
                    1 => {
                        let op = b.pick(&[0x62, 0x6A, 0x72, 0x7A]);
                        rect(b, op, local(&d, 8), None)
                    }
                    _ => {
                        let op = b.pick(&[0x42, 0x52]);
                        line(b, op, 2, local(&d, 8))
                    }
                };
                b.packet(p);
            }
        }
        "blend-tex" => {
            let depth = b.rng.below(3);
            let (d, t) = scene(b, Some(depth))?;
            let t = t.expect("texture");
            let abr = (i % 4) as u32;
            let dither = b.coin();
            b.state(e1(t.tpx, t.tpy, abr, t.depth, dither, 0), 0, (d.x as i32, d.y as i32), 0);
            for _ in 0..b.r(2, 4) {
                let p = if b.coin() {
                    let op = b.pick(&[0x26, 0x27, 0x2E, 0x2F, 0x36, 0x37, 0x3E, 0x3F]);
                    polygon(b, op, local(&d, 8), Some((&t, abr)))
                } else {
                    let op = b.pick(&[0x66, 0x67, 0x6E, 0x6F, 0x76, 0x77, 0x7E, 0x7F]);
                    rect(b, op, local(&d, 8), Some(&t))
                };
                b.packet(p);
            }
        }
        "mask" | "quad-semi" | "clip" => {
            let depth = b.rng.below(3);
            let (d, t) = scene(b, Some(depth))?;
            let t = t.expect("texture");
            let abr = b.rng.below(4);
            let mask = if name == "mask" { b.r(1, 3) as u32 } else if name == "quad-semi" { b.rng.below(4) } else { 0 };
            let dither = b.coin();
            b.state(e1(t.tpx, t.tpy, abr, t.depth, dither, 0), 0, (d.x as i32, d.y as i32), mask);
            if name == "clip" {
                // A drawing area inside the destination, and an offset.
                let (cx0, cy0) = (d.x as u32 + b.rng.below(d.w as u32 / 2), d.y as u32 + b.rng.below(d.h as u32 / 2));
                let cx1 = cx0 + b.rng.below(d.x as u32 + d.w as u32 - cx0);
                let cy1 = cy0 + b.rng.below(d.y as u32 + d.h as u32 - cy0);
                b.state[2] = e3(cx0, cy0);
                b.state[3] = e4(cx1, cy1);
                let (ox, oy) = (d.x as i32 + b.r(-24, 24), d.y as i32 + b.r(-24, 24));
                b.state[4] = e5(ox, oy);
            }
            for _ in 0..b.r(1, 4) {
                let semi = if name == "quad-semi" { 2 } else { 2 * b.rng.below(2) };
                let p = match if name == "quad-semi" { 0 } else { b.rng.below(4) } {
                    0 => {
                        let base = if name == "quad-semi" { 0x28 } else { b.pick(&[0x20, 0x28]) };
                        let op = base | semi | b.pick(&[0x00, 0x10, 0x04, 0x14, 0x05]);
                        polygon(b, op, local(&d, 8), Some((&t, abr)))
                    }
                    1 => {
                        let op = b.pick(&[0x60, 0x64, 0x65, 0x68, 0x6C, 0x70, 0x74, 0x78, 0x7C]) | semi;
                        rect(b, op, local(&d, 8), Some(&t))
                    }
                    2 => {
                        let op = b.pick(&[0x40, 0x50]) | semi;
                        line(b, op, 2, local(&d, 12))
                    }
                    _ => {
                        let op = b.pick(&[0x48, 0x58]) | semi;
                        let n = b.r(2, 5) as usize;
                        line(b, op, n, local(&d, 12))
                    }
                };
                b.packet(p);
            }
        }
        "fill" => {
            // Arbitrary position and size (the GPU rounds x down and w up
            // to 16), wrapping at the VRAM edges, over a tiny drawing area.
            let x = b.r(0, 1023) as u32;
            let y = b.r(0, 511) as u32;
            let choices = [b.r(1, 64), b.r(1, 300), b.r(1000, 1023)];
            let w = b.pick(&choices) as u32;
            let h = b.r(1, 40) as u32;
            let (fx, fw) = (x & 0x3F0, ((w & 0x3FF) + 15) & !15);
            // Read back the filled area, wrapped, with a margin.
            let area = wrapped((fx + 1008) & 1023, (y + 510) & 511, (fw + 32).min(1024), (h + 4).min(512));
            for r in area {
                b.random_area(r)?;
                b.readback.push(r);
            }
            let mask = b.rng.below(4);
            b.state(e1(0, 0, 0, 0, false, 0), 0, (0, 0), mask);
            b.state[2] = e3(5, 5);
            b.state[3] = e4(6, 6);
            let color = b.rgb();
            b.packet(vec![0x0200_0000 | color, y << 16 | x, h << 16 | w]);
        }
        "copy" => {
            let choices = [b.r(1, 32), b.r(100, 300)];
            let w = b.pick(&choices) as u32;
            let h = b.r(1, 24) as u32;
            let sx = b.r(0, 1023) as u32;
            let sy = b.r(0, 511) as u32;
            let (dx, dy) = if b.coin() {
                // Overlapping: a small shift.
                (((sx as i32 + b.r(-140, 140)) & 1023) as u32, ((sy as i32 + b.r(-3, 3)) & 511) as u32)
            } else {
                (b.r(0, 1023) as u32, b.r(0, 511) as u32)
            };
            // Initialise and read back the source and destination,
            // wrapped; later rectangles win where they overlap.
            for (x, y) in [(sx, sy), (dx, dy)] {
                for r in wrapped(x, y, w, h) {
                    b.random_area(r)?;
                }
            }
            for r in wrapped(dx, dy, w, h).into_iter().chain(wrapped(sx, sy, w, h)) {
                b.readback.push(r);
            }
            let mask = b.rng.below(4);
            b.state(e1(0, 0, 0, 0, false, 0), 0, (0, 0), mask);
            b.packet(vec![0x8000_0000, sy << 16 | sx, dy << 16 | dx, h << 16 | w]);
        }
        "upload" => {
            let mask = b.rng.below(4);
            if b.coin() {
                // GP0(A0h) in the packet stream (at most 26 pixels, to
                // fit the FIFO), wrapping at the edges.
                let w = b.r(1, 13) as u32;
                let h = b.r(1, (26 / w as i32).max(1)) as u32;
                let x = if b.coin() { b.r(1024 - w as i32, 1023) } else { b.r(0, 1023) } as u32;
                let y = if b.coin() { b.r(512 - h as i32, 511) } else { b.r(0, 511) } as u32;
                for r in wrapped(x, y, w, h) {
                    b.random_area(r)?;
                    b.readback.push(r);
                }
                b.state(e1(0, 0, 0, 0, false, 0), 0, (0, 0), mask);
                let n = (w * h) as usize;
                let mut words = vec![0xA000_0000, y << 16 | x, h << 16 | w];
                for _ in 0..n.div_ceil(2) {
                    words.push(b.rng.next_u64() as u32);
                }
                b.packet(words);
            } else {
                let (w, h) = (b.r(1, 64) as u16, b.r(1, 32) as u16);
                let r = Rect::new(b.r(0, 1024 - w as i32) as u16, b.r(0, 512 - h as i32) as u16, w, h);
                b.random_area(r)?;
                let px: Vec<u16> = (0..r.pixels()).map(|_| b.rng.u16()).collect();
                let blob = b.store.put_blob(&pixels_to_bytes(&px))?;
                b.readback.push(r);
                b.transfer = Some(GpuTransfer {
                    init: Vec::new(),
                    state: vec![0xE600_0000 | mask],
                    steps: vec![TransferStep::CpuToVram(VramRect { rect: r, pixels: blob })],
                    readback: Vec::new(),
                });
            }
        }
        "clut-cache" => {
            // Colour tables are cached by address and depth: overwriting
            // one by CPU→VRAM leaves primitives using the cached copy
            // until GP0(01h), a depth change or another address.
            let depth = b.rng.below(2);
            let (d, t) = scene(b, Some(depth))?;
            let t = t.expect("texture");
            b.state(e1(t.tpx, t.tpy, 0, t.depth, false, 0), 0, (d.x as i32, d.y as i32), 0);
            let entries = if depth == 0 { 16u32 } else { 256 };
            let (cx, cy) = ((t.clut & 0x3F) * 16, t.clut >> 6);
            for _ in 0..b.r(2, 5) {
                match b.rng.below(5) {
                    0 => b.packet(vec![0x0100_0000]),
                    1 => {
                        // Rewrite part of the table.
                        let n = b.r(1, 26) as u32;
                        let x = cx + b.rng.below(entries - n + 1);
                        let mut words = vec![0xA000_0000, cy << 16 | x, 1 << 16 | n];
                        for _ in 0..n.div_ceil(2) {
                            words.push(b.rng.next_u64() as u32);
                        }
                        b.packet(words);
                    }
                    2 => {
                        let op = b.pick(&[0x25, 0x2D]);
                        let p = polygon(b, op, local(&d, 4), Some((&t, 0)));
                        b.packet(p);
                    }
                    _ => {
                        let op = b.pick(&[0x65, 0x6D, 0x75, 0x7D]);
                        let p = rect(b, op, local(&d, 4), Some(&t));
                        b.packet(p);
                    }
                }
            }
        }
        "tpage-state" => {
            // A textured polygon's page becomes the draw mode rectangles use.
            let (d, _) = scene(b, None)?;
            let half = 1 - d.y as u32 / 256;
            let (d1, d2) = (b.rng.below(3), b.rng.below(3));
            let t1 = texture(b, d1, half)?;
            let t2 = texture(b, d2, half)?;
            b.state(e1(t1.tpx, t1.tpy, 0, t1.depth, false, 0), 0, (d.x as i32, d.y as i32), 0);
            let p = rect(b, 0x65, local(&d, 0), Some(&t1));
            b.packet(p);
            let abr = b.rng.below(4);
            let p = polygon(b, 0x2D, local(&d, 0), Some((&t2, abr)));
            b.packet(p);
            let op = b.pick(&[0x65, 0x67, 0x7D]);
            let p = rect(b, op, local(&d, 0), Some(&t2));
            b.packet(p);
        }
        "big" => {
            // Near the size limits: 1023 wide or 511 tall is drawn, one
            // more is dropped.
            let wide = b.coin();
            let (w, h) = if wide { (1024, 24) } else { (40, 512) };
            let d = b.dest(w, h, 0, 512)?;
            b.state(e1(0, 0, 0, 0, false, 0), 0, (0, d.y as i32), 0);
            for _ in 0..b.r(1, 2) {
                let p = if wide {
                    // x is 11-bit signed: -2..1023 reaches width 1025.
                    let span = b.r(1020, 1025);
                    let x0 = b.r(-2, 1023 - span);
                    let op = b.pick(&[0x20, 0x30, 0x40, 0x50]);
                    let mut words = vec![op << 24 | b.rgb(), xy(x0, b.r(0, 23))];
                    if op & 0x10 != 0 {
                        words.push(b.rgb());
                    }
                    words.push(xy(x0 + span, b.r(0, 23)));
                    if op < 0x40 {
                        if op & 0x10 != 0 {
                            words.push(b.rgb());
                        }
                        words.push(xy(b.r(x0, x0 + span), b.r(0, 23)));
                    }
                    words
                } else {
                    let span = b.r(508, 513);
                    let y0 = b.r(-2, (511 - span).max(-2));
                    let x = d.x as i32;
                    let op = b.pick(&[0x20, 0x30, 0x40, 0x50]);
                    let mut words = vec![op << 24 | b.rgb(), xy(x + b.r(0, 39), y0)];
                    if op & 0x10 != 0 {
                        words.push(b.rgb());
                    }
                    words.push(xy(x + b.r(0, 39), y0 + span));
                    if op < 0x40 {
                        if op & 0x10 != 0 {
                            words.push(b.rgb());
                        }
                        words.push(xy(x + b.r(0, 39), b.r(y0, y0 + span)));
                    }
                    words
                };
                b.packet(p);
            }
        }
        "coord-wrap" => {
            // Vertices whose coordinate after the offset leaves -1024..1023.
            // The GPU wraps span starts and rows to 11 bits: below -1024
            // they come back 2048 to the right (or down), beyond 1023 they
            // fall off the left (or top).
            let (d, _) = scene(b, None)?;
            let wrap = b.coin();
            let (ox, oy) = if wrap { (-1000, -1000) } else { (b.pick(&[600, 1000]), b.pick(&[300, 500])) };
            let dither = b.coin();
            b.state(e1(0, 0, 0, 0, dither, 0), 0, (ox, oy), 0);
            b.state[2] = e3(d.x as u32, d.y as u32);
            b.state[3] = e4((d.x + d.w - 1) as u32, (d.y + d.h - 1) as u32);
            for _ in 0..b.r(1, 3) {
                let op = b.pick(&[0x20, 0x30, 0x28, 0x40, 0x50, 0x60]);
                let n = match op {
                    0x20 | 0x30 => 3,
                    0x28 => 4,
                    0x60 => 1,
                    _ => 2,
                };
                let mut words = vec![op << 24 | b.rgb()];
                for v in 0..n {
                    if v > 0 && op & 0x10 != 0 {
                        words.push(b.rgb());
                    }
                    // Where the vertex should land, then the coordinate
                    // after the offset that gets it there (or past 1023).
                    let (tx, ty) = (d.x as i32 + b.r(-8, d.w as i32 + 8), d.y as i32 + b.r(-8, d.h as i32 + 8));
                    let (fx, fy) = if wrap {
                        (tx - 2048, ty - 2048)
                    } else if b.coin() {
                        (tx + 1024, ty)
                    } else {
                        (tx, ty + 512)
                    };
                    let raw = |f: i32, o: i32| ((f - o + 1024) & 2047) - 1024;
                    let (x, y) = if op == 0x60 { (raw(tx, ox), raw(ty, oy)) } else { (raw(fx, ox), raw(fy, oy)) };
                    words.push(xy(x, y));
                }
                if op == 0x60 {
                    words.push((b.r(1, 40) as u32) << 16 | b.r(1, 60) as u32);
                }
                b.packet(words);
            }
        }
        "misc" => {
            // No-ops and unused opcodes between primitives: each is one word.
            let (d, _) = scene(b, None)?;
            b.state(e1(0, 0, 0, 0, false, 0), 0, (d.x as i32, d.y as i32), 0);
            for _ in 0..b.r(2, 5) {
                if b.coin() {
                    let op = b.pick(&[0x00, 0x03, 0x10, 0x1E, 0xE0, 0xE7, 0xEF, 0xFF]);
                    let low = b.rng.next_u64() as u32 & 0xFF_FFFF;
                    b.packet(vec![op << 24 | low]);
                } else {
                    let p = polygon(b, 0x30, local(&d, 4), None);
                    b.packet(p);
                }
            }
        }
        other => anyhow::bail!("unknown GPU generator {other}"),
    }
    Ok(())
}

/// A wrapping VRAM rectangle as up to four that don't wrap.
fn wrapped(x: u32, y: u32, w: u32, h: u32) -> Vec<Rect> {
    let xs = if x + w <= 1024 { vec![(x, w)] } else { vec![(x, 1024 - x), (0, x + w - 1024)] };
    let ys = if y + h <= 512 { vec![(y, h)] } else { vec![(y, 512 - y), (0, y + h - 512)] };
    let mut out = Vec::new();
    for &(ry, rh) in &ys {
        for &(rx, rw) in &xs {
            out.push(Rect::new(rx as u16, ry as u16, rw as u16, rh as u16));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_generator_makes_valid_reproducible_vectors() {
        let dir = std::env::temp_dir().join(format!("psxw-oracle-synth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::new(&dir);
        let a = generate(&store, SEED, 6).unwrap();
        let b = generate(&store, SEED, 6).unwrap();
        assert_eq!(a.len(), 6 * GENERATORS.len());
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.vector, y.vector, "{}", x.generator);
            x.vector.check().unwrap();
            crate::script::compile(&x.vector, &store).unwrap_or_else(|e| panic!("{}: {e:#}", x.generator));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
