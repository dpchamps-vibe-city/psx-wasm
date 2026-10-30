//! Which pixels a triangle covers: the GPU's setup of its edge walkers
//! (Beetle PSX's `DrawTriangle`, the provisional oracle) and the span each
//! row gets. psx-render-gl's setup hands the shader these edges; the GPU
//! front end uses the spans to know exactly what a polygon plots (its
//! hazard tracking), and psx-render-gl's tests hold the two to the same
//! edges.

/// An edge walker: its 32.32 x at the part's reference row, and its step
/// per row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub coord: i64,
    pub step: i64,
}

/// One part of a triangle: rows `y[0]..y[1]` (upper), then `y[1]..y[2]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Part {
    pub yref: i32,
    /// The part is walked upwards (from `yref - 1`).
    pub dec: bool,
    pub left: Edge,
    pub right: Edge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Triangle {
    /// The vertices' rows, sorted.
    pub y: [i32; 3],
    /// The input vertex at each sorted position.
    pub order: [usize; 3],
    /// The core vertex (whose attributes seed the interpolants), sorted.
    pub core: usize,
    pub parts: [Part; 2],
}

/// `((x << 32) + (1 << 32) - (1 << 11))`: x as a 32.32 edge coordinate,
/// biased so the integer part rounds up.
pub fn xfp(x: i32) -> i64 {
    (((x as u32 as u64) << 32).wrapping_add((1u64 << 32) - (1 << 11))) as i64
}

/// The edge step `dx / dy` in 32.32, rounded away from zero.
pub fn xfp_step(dx: i32, dy: i32) -> i64 {
    let mut e = (dx as i64) << 32;
    if e < 0 {
        e -= (dy - 1) as i64;
    }
    if e > 0 {
        e += (dy - 1) as i64;
    }
    e / dy as i64
}

/// The drawing-area coordinate of an 11-bit value.
pub fn sext11(v: i32) -> i32 {
    (v << 21) >> 21
}

impl Triangle {
    /// The GPU's setup for vertices at `xy`, or None when it draws
    /// nothing: 1024 or more wide or 512 or more tall, no rows, no area.
    pub fn new(xy: [(i32, i32); 3]) -> Option<Triangle> {
        let pairs = [(0, 1), (1, 2), (0, 2)];
        if pairs.iter().any(|&(a, b)| (xy[a].1 - xy[b].1).abs() >= 512 || (xy[a].0 - xy[b].0).abs() >= 1024) {
            return None;
        }
        // The core vertex (from the unsorted x order), then sort by y.
        let mut v = xy;
        let mut order = [0, 1, 2];
        let mut cv: u32 = if v[1].0 <= v[0].0 {
            if v[2].0 <= v[1].0 { 4 } else { 2 }
        } else if v[2].0 < v[0].0 {
            4
        } else {
            1
        };
        let mut swap = |v: &mut [(i32, i32); 3], i: usize, j: usize| {
            v.swap(i, j);
            order.swap(i, j);
        };
        if v[2].1 < v[1].1 {
            swap(&mut v, 2, 1);
            cv = ((cv >> 1) & 2) | ((cv << 1) & 4) | (cv & 1);
        }
        if v[1].1 < v[0].1 {
            swap(&mut v, 1, 0);
            cv = ((cv >> 1) & 1) | ((cv << 1) & 2) | (cv & 4);
        }
        if v[2].1 < v[1].1 {
            swap(&mut v, 2, 1);
            cv = ((cv >> 1) & 2) | ((cv << 1) & 4) | (cv & 1);
        }
        let core = (cv >> 1) as usize;
        let ((x0, y0), (x1, y1), (x2, y2)) = (v[0], v[1], v[2]);
        if y0 == y2 {
            return None;
        }
        if (x1 - x0) as i64 * (y2 - y1) as i64 == (x2 - x1) as i64 * (y1 - y0) as i64 {
            return None;
        }
        let base_coord = xfp(x0);
        let base_step = xfp_step(x2 - x0, y2 - y0);
        let (bound_us, right_facing) = if y1 == y0 {
            (0, x1 > x0)
        } else {
            let s = xfp_step(x1 - x0, y1 - y0);
            (s, s > base_step)
        };
        let bound_ls = if y2 == y1 { 0 } else { xfp_step(x2 - x1, y2 - y1) };
        let vo = (core != 0) as usize;
        let vp = if core == 2 { 3 } else { 0 };
        let part = |yref: i32, dec: bool, short_x: i32, short_step: i64| {
            let short = Edge { coord: xfp(short_x), step: short_step };
            let long = Edge { coord: base_coord.wrapping_add((yref - y0) as i64 * base_step), step: base_step };
            let (left, right) = if right_facing { (long, short) } else { (short, long) };
            Part { yref, dec, left, right }
        };
        let upper = part(v[vo].1, vo != 0, v[vo].0, bound_us);
        let lower = part(v[1 ^ vp].1, vp != 0, v[1 ^ vp].0, bound_ls);
        Some(Triangle { y: [y0, y1, y2], order, core, parts: [upper, lower] })
    }

    /// Row `yi`'s edges (`y[0] <= yi < y[2]`): the walkers' integer x,
    /// left and right; the row plots `sext11(left)` and the `right - left`
    /// pixels after it.
    pub fn span(&self, yi: i32) -> (i32, i32) {
        let p = &self.parts[(yi >= self.y[1]) as usize];
        let n = (yi - p.yref) as i64;
        let at = |e: &Edge| (e.coord.wrapping_add(n.wrapping_mul(e.step)) >> 32) as i32;
        (at(&p.left), at(&p.right))
    }

    /// The pixels it plots inside the drawing area `clip` (inclusive, in
    /// VRAM), row by row: (row, first column, end column). Interlaced
    /// row skipping and the upward walk's stop at the drawing area aren't
    /// applied, so this may hold a few rows more than the GPU plots,
    /// never fewer pixels.
    pub fn spans(&self, clip: [i32; 4], mut f: impl FnMut(u32, u32, u32)) {
        let [cx0, cy0, cx1, cy1] = clip;
        for yi in self.y[0]..self.y[2] {
            let sy = sext11(yi);
            if sy < cy0 || sy > cy1 {
                continue;
            }
            let (xs, xb) = self.span(yi);
            let (mut x, mut w) = (sext11(xs), xb - xs);
            if x < cx0 {
                w -= cx0 - x;
                x = cx0;
            }
            if x + w > cx1 + 1 {
                w = cx1 + 1 - x;
            }
            if w > 0 {
                f((yi & 511) as u32, x as u32, (x + w) as u32);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two triangles of a quad share their diagonal; no pixel is in both.
    #[test]
    fn a_shared_edge_belongs_to_one_side() {
        let quad = [(10, 10), (60, 14), (8, 50), (57, 61)];
        let clip = [0, 0, 1023, 511];
        let mut px = std::collections::HashMap::new();
        for idx in [[0, 1, 2], [1, 2, 3]] {
            let t = Triangle::new(idx.map(|i| quad[i])).unwrap();
            t.spans(clip, |row, x0, x1| {
                for x in x0..x1 {
                    *px.entry((row, x)).or_insert(0) += 1;
                }
            });
        }
        assert!(px.len() > 1500);
        assert!(px.values().all(|&n| n == 1));
    }

    #[test]
    fn nothing_for_degenerate_or_oversized() {
        assert_eq!(Triangle::new([(0, 0), (10, 10), (20, 20)]), None);
        assert_eq!(Triangle::new([(0, 0), (10, 0), (20, 0)]), None);
        assert_eq!(Triangle::new([(0, 0), (1024, 5), (3, 9)]), None);
    }
}
