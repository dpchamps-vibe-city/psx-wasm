//! Primitive setup: from a renderer record (explicit PS1 attributes) to
//! the constants the fragment shader evaluates per pixel.
//!
//! The setup is per-primitive integer arithmetic, done once on the CPU
//! the way the GPU's own setup unit does it: vertex sorting, the edge
//! walkers' 32.32 fixed-point start values and steps, the interpolants'
//! 8.24 bases and per-pixel deltas, the line walker's steps, the texture
//! window masks. Every per-pixel decision (coverage, interpolation,
//! texturing, blending, dither, mask) is in the shader. The arithmetic
//! follows Beetle PSX's software rasterizer (the provisional oracle):
//! `gpu_polygon.c` (DrawTriangle, CalcIDeltas), `gpu_sprite.c` and
//! `gpu_line.c`.
//!
//! A setup record is [`STRIDE`] texels of four `u32`s:
//!
//! | texel | contents |
//! |---|---|
//! | 0 | flags (the record's word 0, kinds extended with [`FILL`], [`UPLOAD`], [`COPY`]), texture word, 0, 0 |
//! | 1 | texture window: `u` AND, `u` ADD, `v` AND, `v` ADD |
//! | 2 | covering rectangle in VRAM, inclusive (x0 > x1 when empty) |
//! | 3.. | per kind, below |
//!
//! Triangles (a quad has two, at texels 3 and 14): valid, y0, y1, y2
//! (sorted); for the upper part (rows y0..y1) and the lower part (rows
//! y1..y2) each: reference row and walk direction, left edge start and
//! step, right edge start and step (64-bit, hi then lo); then the u, v,
//! r, g, b bases, x deltas and y deltas.

use psxw_abi::render::{prim, unpack_xy};

/// Texels per setup record.
pub const STRIDE: usize = 25;
pub const WORDS: usize = STRIDE * 4;

/// Backend-only kinds, beyond [`prim`]'s.
pub const FILL: u32 = 5;
pub const UPLOAD: u32 = 6;
pub const COPY: u32 = 7;

/// Word offsets.
const TRI0: usize = 12;
const TRI1: usize = TRI0 + 44;
const BODY: usize = 12;

/// The batch state a setup depends on: the drawing area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clip {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

/// `(hi, lo)` of a 64-bit value.
fn split(v: i64) -> [u32; 2] {
    [(v as u64 >> 32) as u32, v as u32]
}

/// The covering rectangle for plotted coordinates in `[x0, x1] × [y0, y1]`
/// (inclusive, in the primitive's own coordinates), clipped to the
/// drawing area; rows plot at `y & 511`. Without `exact`, wrapped
/// coordinates are possible and the whole drawing area is covered.
fn cover(clip: &Clip, x0: i32, x1: i32, y0: i32, y1: i32, exact: bool) -> [u32; 4] {
    let (ax0, ax1, ay0, ay1) =
        if exact { (x0.max(clip.x0), x1.min(clip.x1), y0.max(clip.y0), y1.min(clip.y1)) } else { (clip.x0, clip.x1, clip.y0, clip.y1) };
    if ax0 > ax1 || ay0 > ay1 {
        return EMPTY;
    }
    let (r0, r1) = if ay0 >> 9 == ay1 >> 9 { (ay0 & 511, ay1 & 511) } else { (0, 511) };
    [ax0 as u32, r0 as u32, ax1 as u32, r1 as u32]
}

const EMPTY: [u32; 4] = [1, 0, 0, 0];

/// The covering rectangle of a VRAM rectangle that may wrap.
fn cover_wrapping(x: u32, y: u32, w: u32, h: u32) -> [u32; 4] {
    if x + w <= 1024 && y + h <= 512 { [x, y, x + w - 1, y + h - 1] } else { [0, 0, 1023, 511] }
}

/// Texture window constants: `fbtex_x = (((u & and_u) + add_u) >> (2 - depth)) & 1023`,
/// `fbtex_y = (v & and_v) + add_v`.
fn window(rec: &[u32]) -> [u32; 4] {
    let depth = ((rec[0] & prim::DEPTH_MASK) >> prim::DEPTH_SHIFT).min(2);
    let page_x = (rec[1] & 0xF) * 64;
    let page_y = ((rec[1] >> 4) & 1) * 256;
    let w = rec[2];
    let (tww, twh, twx, twy) = (w & 0x1F, (w >> 5) & 0x1F, (w >> 10) & 0x1F, (w >> 15) & 0x1F);
    [!(tww << 3), ((twx & tww) << 3) + (page_x << (2 - depth)), !(twh << 3), ((twy & twh) << 3) + page_y]
}

/// One polygon vertex.
#[derive(Debug, Clone, Copy, Default)]
struct V {
    x: i32,
    y: i32,
    u: i32,
    v: i32,
    r: i32,
    g: i32,
    b: i32,
}

fn vertex(rec: &[u32], n: usize) -> V {
    let (x, y) = unpack_xy(rec[3 + 3 * n]);
    let c = rec[4 + 3 * n];
    let t = rec[5 + 3 * n];
    V {
        x,
        y,
        u: (t & 0xFF) as i32,
        v: ((t >> 8) & 0xFF) as i32,
        r: (c & 0xFF) as i32,
        g: ((c >> 8) & 0xFF) as i32,
        b: ((c >> 16) & 0xFF) as i32,
    }
}

/// The GPU drops triangles 1024 or more wide or 512 or more tall.
fn too_big(v: &[V; 3]) -> bool {
    let pairs = [(0, 1), (1, 2), (0, 2)];
    pairs.iter().any(|&(a, b)| (v[a].y - v[b].y).abs() >= 512 || (v[a].x - v[b].x).abs() >= 1024)
}

/// `((x << 32) + (1 << 32) - (1 << 11))`: x as a 32.32 edge coordinate,
/// biased so the integer part rounds up.
fn xfp(x: i32) -> i64 {
    (((x as u32 as u64) << 32).wrapping_add((1u64 << 32) - (1 << 11))) as i64
}

/// The edge step `dx / dy` in 32.32, rounded away from zero.
fn xfp_step(dx: i32, dy: i32) -> i64 {
    let mut e = (dx as i64) << 32;
    if e < 0 {
        e -= (dy - 1) as i64;
    }
    if e > 0 {
        e += (dy - 1) as i64;
    }
    e / dy as i64
}

/// One triangle's setup words (44), or None when the GPU draws nothing.
fn triangle(mut v: [V; 3]) -> Option<[u32; 44]> {
    if too_big(&v) {
        return None;
    }
    // The core vertex (from the unsorted x order), then sort by y.
    let mut cv: u32 = if v[1].x <= v[0].x {
        if v[2].x <= v[1].x { 4 } else { 2 }
    } else if v[2].x < v[0].x {
        4
    } else {
        1
    };
    if v[2].y < v[1].y {
        v.swap(2, 1);
        cv = ((cv >> 1) & 2) | ((cv << 1) & 4) | (cv & 1);
    }
    if v[1].y < v[0].y {
        v.swap(1, 0);
        cv = ((cv >> 1) & 1) | ((cv << 1) & 2) | (cv & 4);
    }
    if v[2].y < v[1].y {
        v.swap(2, 1);
        cv = ((cv >> 1) & 2) | ((cv << 1) & 4) | (cv & 1);
    }
    let core = (cv >> 1) as usize;
    if v[0].y == v[2].y {
        return None;
    }
    let (a, b, c) = (v[0], v[1], v[2]);
    let cis = |f: fn(&V) -> i32, g: fn(&V) -> i32| -> i64 {
        (f(&b) - f(&a)) as i64 * (g(&c) - g(&b)) as i64 - (f(&c) - f(&b)) as i64 * (g(&b) - g(&a)) as i64
    };
    let denom = cis(|p| p.x, |p| p.y);
    if denom == 0 {
        return None;
    }
    // (u32)(cis * 4096 / denom) << 12, as the GPU's 32-bit arithmetic does.
    let delta = |n: i64| ((n * 4096 / denom) as i32 as u32) << 12;
    let dx = [
        delta(cis(|p| p.u, |p| p.y)),
        delta(cis(|p| p.v, |p| p.y)),
        delta(cis(|p| p.r, |p| p.y)),
        delta(cis(|p| p.g, |p| p.y)),
        delta(cis(|p| p.b, |p| p.y)),
    ];
    let dy = [
        delta(cis(|p| p.x, |p| p.u)),
        delta(cis(|p| p.x, |p| p.v)),
        delta(cis(|p| p.x, |p| p.r)),
        delta(cis(|p| p.x, |p| p.g)),
        delta(cis(|p| p.x, |p| p.b)),
    ];
    let k = v[core];
    let seed = |c: i32| (((c as u32) << 12) + (1 << 11)) << 12;
    let cvals = [k.u, k.v, k.r, k.g, k.b];
    let mut base = [0u32; 5];
    for i in 0..5 {
        base[i] = seed(cvals[i])
            .wrapping_add(dx[i].wrapping_mul((-k.x) as u32))
            .wrapping_add(dy[i].wrapping_mul((-k.y) as u32));
    }

    let base_coord = xfp(v[0].x);
    let base_step = xfp_step(v[2].x - v[0].x, v[2].y - v[0].y);
    let (bound_us, right_facing) = if v[1].y == v[0].y {
        (0, v[1].x > v[0].x)
    } else {
        let s = xfp_step(v[1].x - v[0].x, v[1].y - v[0].y);
        (s, s > base_step)
    };
    let bound_ls = if v[2].y == v[1].y { 0 } else { xfp_step(v[2].x - v[1].x, v[2].y - v[1].y) };
    let vo = (core != 0) as usize;
    let vp = if core == 2 { 3 } else { 0 };
    // Each part: (reference row, decrementing, [left, right] (coord, step)).
    let part = |yref: i32, dec: bool, short_x: i32, short_step: i64, long_y: i32| {
        let short = (xfp(short_x), short_step);
        let long = (base_coord.wrapping_add((long_y - v[0].y) as i64 * base_step), base_step);
        let (l, r) = if right_facing { (long, short) } else { (short, long) };
        (yref, dec, l, r)
    };
    let upper = part(v[vo].y, vo != 0, v[vo].x, bound_us, v[vo].y);
    let lower = part(v[1 ^ vp].y, vp != 0, v[1 ^ vp].x, bound_ls, v[1 ^ vp].y);

    let mut w = [0u32; 44];
    w[0..4].copy_from_slice(&[1, v[0].y as u32, v[1].y as u32, v[2].y as u32]);
    for (i, (yref, dec, l, r)) in [upper, lower].into_iter().enumerate() {
        let o = 4 + 12 * i;
        w[o] = yref as u32;
        w[o + 1] = dec as u32;
        w[o + 4..o + 6].copy_from_slice(&split(l.0));
        w[o + 6..o + 8].copy_from_slice(&split(l.1));
        w[o + 8..o + 10].copy_from_slice(&split(r.0));
        w[o + 10..o + 12].copy_from_slice(&split(r.1));
    }
    w[28..33].copy_from_slice(&base);
    w[33..38].copy_from_slice(&dx);
    w[38..43].copy_from_slice(&dy);
    Some(w)
}

/// The rows of a TRIANGLE record in the order the GPU draws them, as the
/// drawing-area test sees them (sign-extended to 11 bits). The GPU walks
/// its two parts in the order of `tripart[]`: the part holding the core
/// vertex's side first, each part up or down as its walk goes.
pub fn triangle_rows(rec: &[u32]) -> Vec<i32> {
    let Some(w) = triangle([vertex(rec, 0), vertex(rec, 1), vertex(rec, 2)]) else { return Vec::new() };
    let (y0, y1, y2) = (w[1] as i32, w[2] as i32, w[3] as i32);
    let (upper_dec, lower_dec) = (w[5] != 0, w[17] != 0);
    let up: Vec<i32> = if upper_dec { (y0..y1).rev().collect() } else { (y0..y1).collect() };
    let low: Vec<i32> = if lower_dec { (y1..y2).rev().collect() } else { (y1..y2).collect() };
    let rows = if upper_dec { low.into_iter().chain(up) } else { up.into_iter().chain(low) };
    rows.map(|y| (y << 21) >> 21).collect()
}

/// The covering rectangle of polygon vertices.
fn polygon_cover(clip: &Clip, v: &[V]) -> [u32; 4] {
    let x0 = v.iter().map(|p| p.x).min().unwrap_or(0);
    let x1 = v.iter().map(|p| p.x).max().unwrap_or(0);
    let y0 = v.iter().map(|p| p.y).min().unwrap_or(0);
    let y1 = v.iter().map(|p| p.y).max().unwrap_or(0);
    let exact = v.iter().all(|p| (-1024..1024).contains(&p.x) && (-1024..1024).contains(&p.y));
    cover(clip, x0 - 1, x1 + 1, y0, y1, exact)
}

/// Set up one DRAW record. `out` receives [`WORDS`] words.
pub fn primitive(rec: &[u32], clip: &Clip, out: &mut [u32]) {
    out[..WORDS].fill(0);
    out[0] = rec[0];
    out[1] = rec[1];
    out[4..8].copy_from_slice(&window(rec));
    let mut bbox = EMPTY;
    match rec[0] & prim::KIND_MASK {
        prim::TRIANGLE | prim::QUAD => {
            let quad = rec[0] & prim::KIND_MASK == prim::QUAD;
            let all = [vertex(rec, 0), vertex(rec, 1), vertex(rec, 2), vertex(rec, 3)];
            let tris: &[[usize; 3]] = if quad { &[[0, 1, 2], [1, 2, 3]] } else { &[[0, 1, 2]] };
            let mut any = false;
            for (t, idx) in tris.iter().enumerate() {
                let o = if t == 0 { TRI0 } else { TRI1 };
                if let Some(w) = triangle(idx.map(|i| all[i])) {
                    out[o..o + 44].copy_from_slice(&w);
                    any = true;
                }
            }
            if any {
                bbox = polygon_cover(clip, &all[..if quad { 4 } else { 3 }]);
            }
        }
        prim::RECT => {
            let (x, y) = unpack_xy(rec[3]);
            let (w, h) = ((rec[4] & 0xFFFF) as i32, (rec[4] >> 16) as i32);
            let textured = rec[0] & prim::TEXTURED != 0;
            let (mut u, mut v) = ((rec[6] & 0xFF) as i32, ((rec[6] >> 8) & 0xFF) as i32);
            let (mut u_inc, mut v_inc) = (1i32, 1i32);
            if textured && rec[0] & prim::FLIP_X != 0 {
                u_inc = -1;
                u |= 1;
            }
            if textured && rec[0] & prim::FLIP_Y != 0 {
                v_inc = -1;
            }
            let (mut xs, mut ys, mut xb, mut yb) = (x, y, x + w, y + h);
            if xs < clip.x0 {
                u += (clip.x0 - xs) * u_inc;
                xs = clip.x0;
            }
            if ys < clip.y0 {
                v += (clip.y0 - ys) * v_inc;
                ys = clip.y0;
            }
            xb = xb.min(clip.x1 + 1);
            yb = yb.min(clip.y1 + 1);
            out[BODY..BODY + 9].copy_from_slice(&[
                xs as u32,
                xb as u32,
                ys as u32,
                yb as u32,
                (u & 0xFF) as u32,
                (v & 0xFF) as u32,
                u_inc as u32,
                v_inc as u32,
                rec[5] & 0xFF_FFFF,
            ]);
            if xb > xs && yb > ys {
                bbox = cover(clip, xs, xb - 1, ys, yb - 1, true);
            }
        }
        prim::LINE => {
            let (mut p0, mut p1) = (unpack_xy(rec[3]), unpack_xy(rec[5]));
            let (mut c0, mut c1) = (rec[4] & 0xFF_FFFF, rec[6] & 0xFF_FFFF);
            let dxa = (p1.0 - p0.0).abs();
            let dya = (p1.1 - p0.1).abs();
            if dxa < 1024 && dya < 512 {
                let k = dxa.max(dya);
                if p0.0 > p1.0 && k != 0 {
                    std::mem::swap(&mut p0, &mut p1);
                    std::mem::swap(&mut c0, &mut c1);
                }
                let divide = |d: i32| -> i64 {
                    let mut e = (d as i64) << 32;
                    if e < 0 {
                        e -= (k - 1) as i64;
                    }
                    if e > 0 {
                        e += (k - 1) as i64;
                    }
                    e / k as i64
                };
                let (sx, sy) = if k == 0 { (0, 0) } else { (divide(p1.0 - p0.0), divide(p1.1 - p0.1)) };
                let ch = |c: u32, s: u32| ((c >> s) & 0xFF) as i32;
                let dc = |s: u32| -> u32 {
                    if k == 0 { 0 } else { ((((ch(c1, s) - ch(c0, s)) as u32) << 12) as i32 / k) as u32 }
                };
                let mut cx = ((p0.0 as u32 as u64) << 32) | (1 << 31);
                let mut cy = ((p0.1 as u32 as u64) << 32) | (1 << 31);
                cx = cx.wrapping_sub(1024);
                if sy < 0 {
                    cy = cy.wrapping_sub(1024);
                }
                let cc = |s: u32| ((ch(c0, s) as u32) << 12) | (1 << 11);
                let x_major = dxa >= dya;
                let w = [
                    p0.0 as u32,
                    p0.1 as u32,
                    k as u32,
                    x_major as u32,
                    (cx >> 32) as u32,
                    cx as u32,
                    (cy >> 32) as u32,
                    cy as u32,
                    split(sx)[0],
                    split(sx)[1],
                    split(sy)[0],
                    split(sy)[1],
                    cc(0),
                    cc(8),
                    cc(16),
                    (sy < 0) as u32,
                    dc(0),
                    dc(8),
                    dc(16),
                    c0,
                ];
                out[BODY..BODY + 20].copy_from_slice(&w);
                let exact = [p0, p1].iter().all(|p| (0..1024).contains(&p.0) && (0..1024).contains(&p.1));
                bbox = cover(clip, p0.0.min(p1.0), p0.0.max(p1.0), p0.1.min(p1.1), p0.1.max(p1.1), exact);
            }
        }
        _ => {}
    }
    out[8..12].copy_from_slice(&bbox);
}

/// A FILL as a setup record.
pub fn fill(x: u32, y: u32, w: u32, h: u32, color: u32, out: &mut [u32]) {
    out[..WORDS].fill(0);
    out[0] = FILL;
    out[8..12].copy_from_slice(&cover_wrapping(x, y, w, h));
    out[BODY..BODY + 5].copy_from_slice(&[x, y, w, h, color]);
}

/// An UPLOAD from the staging texture at `(sx, sy)`.
#[allow(clippy::too_many_arguments)]
pub fn upload(x: u32, y: u32, w: u32, h: u32, sx: u32, sy: u32, mask: u32, out: &mut [u32]) {
    out[..WORDS].fill(0);
    out[0] = UPLOAD;
    out[8..12].copy_from_slice(&cover_wrapping(x, y, w, h));
    out[BODY..BODY + 7].copy_from_slice(&[x, y, w, h, sx, sy, mask]);
}

/// A COPY.
pub fn copy(src: (u32, u32), dst: (u32, u32), w: u32, h: u32, mask: u32, out: &mut [u32]) {
    out[..WORDS].fill(0);
    out[0] = COPY;
    out[8..12].copy_from_slice(&cover_wrapping(dst.0, dst.1, w, h));
    out[BODY..BODY + 7].copy_from_slice(&[dst.0, dst.1, w, h, src.0, src.1, mask]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use psxw_abi::render::pack_xy;

    fn rec_tri(pts: [(i32, i32); 3]) -> [u32; 16] {
        let mut r = [0u32; 16];
        r[0] = prim::TRIANGLE;
        for (n, p) in pts.iter().enumerate() {
            r[3 + 3 * n] = pack_xy(p.0, p.1);
        }
        r
    }

    const CLIP: Clip = Clip { x0: 0, y0: 0, x1: 1023, y1: 511 };

    #[test]
    fn edge_fixed_point() {
        assert_eq!(xfp(3) >> 32, 3);
        assert_eq!(xfp(-1) >> 32, -1);
        assert_eq!(xfp_step(1, 2), 1i64 << 31);
        // Rounded away from zero.
        assert_eq!(xfp_step(1, 3), 1_431_655_766);
        assert_eq!(xfp_step(-1, 3), -1_431_655_766);
        assert_eq!(xfp_step(0, 5), 0);
    }

    #[test]
    fn degenerate_and_oversized_triangles_are_empty() {
        let mut out = [0u32; WORDS];
        primitive(&rec_tri([(0, 0), (10, 0), (20, 0)]), &CLIP, &mut out);
        assert!(out[8] > out[10], "zero height");
        primitive(&rec_tri([(0, 0), (5, 5), (10, 10)]), &CLIP, &mut out);
        assert!(out[8] > out[10], "zero area");
        primitive(&rec_tri([(0, 0), (1024, 0), (0, 10)]), &CLIP, &mut out);
        assert!(out[8] > out[10], "too wide");
        primitive(&rec_tri([(0, 0), (10, 0), (0, 10)]), &CLIP, &mut out);
        assert_eq!(&out[8..12], &[0, 0, 11, 10]);
        assert_eq!(out[TRI0], 1);
    }

    #[test]
    fn flat_colour_base_is_the_vertex_colour() {
        let mut r = rec_tri([(0, 0), (10, 0), (0, 10)]);
        for n in 0..3 {
            r[4 + 3 * n] = 0x0012_3456;
        }
        let mut out = [0u32; WORDS];
        primitive(&r, &CLIP, &mut out);
        // r, g, b bases at x = y = 0 are the colour in 8.24 plus a half.
        assert_eq!(out[TRI0 + 30] >> 24, 0x56);
        assert_eq!(out[TRI0 + 31] >> 24, 0x34);
        assert_eq!(out[TRI0 + 32] >> 24, 0x12);
    }

    #[test]
    fn rect_clipping_advances_texture_coordinates() {
        let mut r = [0u32; 16];
        r[0] = prim::RECT | prim::TEXTURED | prim::FLIP_X;
        r[3] = pack_xy(-3, -2);
        r[4] = 8 | 8 << 16;
        r[6] = 10 | 20 << 8;
        let mut out = [0u32; WORDS];
        primitive(&r, &CLIP, &mut out);
        // u starts at 10 | 1 = 11 and steps -1; three columns are clipped.
        assert_eq!(&out[BODY..BODY + 8], &[0, 5, 0, 6, 8, 22, (-1i32) as u32, 1]);
    }

    #[test]
    fn line_steps() {
        let mut r = [0u32; 16];
        r[0] = prim::LINE;
        r[3] = pack_xy(10, 0);
        r[5] = pack_xy(0, 5);
        let mut out = [0u32; WORDS];
        primitive(&r, &CLIP, &mut out);
        let w = &out[BODY..BODY + 20];
        assert_eq!((w[0], w[1], w[2], w[3]), (0, 5, 10, 1), "swapped so x increases; x-major");
        assert_eq!(((w[8] as u64) << 32 | w[9] as u64), 1u64 << 32);
    }
}
