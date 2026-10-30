//! The GPU front end (docs/design.md "Runtime", gpu; plans/completed/M04-pixels.md).
//!
//! It speaks the GPU's own vocabulary: GP0 words (as the port or DMA
//! delivers them) and GP1 commands. It keeps the draw and display state,
//! decodes every GP0 packet, and emits the renderer command stream of
//! [`psxw_abi::render`] with each primitive's attributes resolved:
//! vertices with the drawing offset applied, colours, texture coordinates,
//! texture page and depth, colour-table slot, texture window, blend mode,
//! dither and mask flags. The backend never sees PS1 state.
//!
//! The decode follows psx-spx's GPU chapter, and Beetle PSX (the
//! provisional oracle) where psx-spx is silent:
//! - polygons take their texture page (and blend mode) from vertex 1's
//!   texture-coordinate word, which also updates the draw mode; quads are
//!   two triangles, 0-1-2 then 1-2-3;
//! - rectangles sign-extend their position to 11 bits after the offset,
//!   take texture flips from GP0(E1h) bits 12–13, and never dither;
//! - lines dither whenever dithering is on; polylines end at a word
//!   matching `5xxx5xxx` where a vertex would start;
//! - dither applies to Gouraud untextured polygons and to modulated
//!   textured ones;
//! - the colour-table cache reloads only when the table address or depth
//!   changes, or after GP0(01h) or a reset ([`render::op::CLUT_LOAD`]).
//!
//! # Batching and hazards
//!
//! Primitives accumulate in a DRAW batch. The backend reads textures,
//! colour tables, blend destinations and mask bits from its read copy of
//! VRAM, so the front end tracks the regions written since the last
//! [`render::op::SYNC`] (at bounding-box granularity) and, before
//! anything that reads one of them, ends the batch and syncs. Within a
//! batch every read therefore sees VRAM as it was before the batch. A
//! primitive that reads its own destination (semi-transparency, the mask
//! check) is held to the pixels written since the sync, polygons as the
//! GPU's edge walkers plot them ([`crate::raster`]): a translucent mesh's
//! neighbouring triangles share a batch, only overlapping ones sync.
//!
//! # VRAM→CPU transfers
//!
//! VRAM lives in the backend. GP0(C0h) ends the batch and leaves a
//! [`Gpu::pending_readback`]; the caller submits the commands so far,
//! reads the rectangle (the `render_readback` host import), and hands the
//! pixels to [`Gpu::finish_readback`], which serves them through GPUREAD.

use crate::raster::Triangle;
use psxw_abi::render::{self, CLUT_SLOTS, PRIM_WORDS, PresentDesc, header, op, pack_xy, prim};

/// Regions tracked individually before they are merged into one box.
const MAX_DIRTY: usize = 64;

/// Sign-extend the low 11 bits.
#[inline]
pub fn sext11(v: u32) -> i32 {
    ((v << 21) as i32) >> 21
}

/// A half-open VRAM area `[x0, x1) × [y0, y1)` that does not wrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Area {
    x0: u16,
    y0: u16,
    x1: u16,
    y1: u16,
}

impl Area {
    fn overlaps(&self, o: &Area) -> bool {
        self.x0 < o.x1 && o.x0 < self.x1 && self.y0 < o.y1 && o.y0 < self.y1
    }

    fn union(&self, o: &Area) -> Area {
        Area { x0: self.x0.min(o.x0), y0: self.y0.min(o.y0), x1: self.x1.max(o.x1), y1: self.y1.max(o.y1) }
    }

    fn contains(&self, o: &Area) -> bool {
        self.x0 <= o.x0 && self.y0 <= o.y0 && self.x1 >= o.x1 && self.y1 >= o.y1
    }

    fn rect(&self) -> render::Rect {
        render::Rect { x: self.x0 as u32, y: self.y0 as u32, w: (self.x1 - self.x0) as u32, h: (self.y1 - self.y0) as u32 }
    }
}

/// Up to four areas: a wrapping rectangle split at the VRAM edges.
#[derive(Debug, Clone, Copy, Default)]
struct Areas {
    a: [Option<Area>; 4],
}

impl Areas {
    fn none() -> Self {
        Areas::default()
    }

    /// The pixels `((x + i) & 1023, (y + j) & 511)` for `i < w`, `j < h`
    /// (`w <= 1024`, `h <= 512`).
    fn wrapping(x: u32, y: u32, w: u32, h: u32) -> Self {
        let mut out = Areas::none();
        if w == 0 || h == 0 {
            return out;
        }
        let (x, y) = (x & 1023, y & 511);
        let xs = if x + w <= 1024 { [(x, x + w), (0, 0)] } else { [(x, 1024), (0, x + w - 1024)] };
        let ys = if y + h <= 512 { [(y, y + h), (0, 0)] } else { [(y, 512), (0, y + h - 512)] };
        let mut n = 0;
        for &(x0, x1) in &xs {
            for &(y0, y1) in &ys {
                if x1 > x0 && y1 > y0 {
                    out.a[n] = Some(Area { x0: x0 as u16, y0: y0 as u16, x1: x1 as u16, y1: y1 as u16 });
                    n += 1;
                }
            }
        }
        out
    }

    fn one(a: Area) -> Self {
        let mut out = Areas::none();
        out.a[0] = Some(a);
        out
    }

    fn iter(&self) -> impl Iterator<Item = Area> + '_ {
        self.a.iter().flatten().copied()
    }

    fn push(&mut self, a: Area) {
        if let Some(slot) = self.a.iter_mut().find(|s| s.is_none()) {
            *slot = Some(a);
        } else if let Some(last) = self.a[3].as_mut() {
            *last = last.union(&a);
        }
    }

    /// Their pixels, row by row.
    fn spans(&self) -> Vec<Span> {
        self.iter().flat_map(|a| (a.y0..a.y1).map(move |y| (y as u32, a.x0 as u32, a.x1 as u32))).collect()
    }
}

/// Pixels of a VRAM row: (row, first column, end column).
type Span = (u32, u32, u32);

/// Most layers a batch is split into; a primitive that would need another
/// syncs everything first.
const MAX_LAYERS: u32 = 64;

/// Primitives of the open batch that are drawn together, in their order:
/// none reads what another of its layer plots (see [`Gpu::add_prim`]).
#[derive(Debug, Clone, Default)]
struct Layer {
    recs: Vec<u32>,
    n: u32,
    /// Where its primitives plot, and where they sample textures.
    writes: Vec<Area>,
    texels: Vec<Area>,
}

/// Which write last plotted each VRAM pixel since the last SYNC, as
/// primitives plot them: a value above `batch` is the open batch's, its
/// layer `value - batch - 1`; one above `synced`, an earlier batch's.
#[derive(Clone, Default)]
struct Written {
    last: Vec<u32>,
    synced: u32,
    batch: u32,
}

impl std::fmt::Debug for Written {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Written {{ synced: {}, batch: {} }}", self.synced, self.batch)
    }
}

impl Written {
    /// Of the pixels in `spans`: the open batch's highest layer that
    /// plotted one, and whether an earlier batch's unsynced write did.
    fn scan(&self, spans: &[Span]) -> (Option<u32>, bool) {
        let (mut top, mut earlier) = (None, false);
        if self.last.is_empty() {
            return (top, earlier);
        }
        for &(row, x0, x1) in spans {
            let o = row as usize * 1024;
            for &v in &self.last[o + x0 as usize..o + x1 as usize] {
                if v > self.batch {
                    let l = v - self.batch - 1;
                    top = Some(top.map_or(l, |t: u32| t.max(l)));
                } else if v > self.synced {
                    earlier = true;
                }
            }
        }
        (top, earlier)
    }

    fn mark(&mut self, spans: &[Span], value: u32) {
        if self.last.is_empty() {
            self.last = vec![0; 1024 * 512];
        }
        for &(row, x0, x1) in spans {
            let o = row as usize * 1024;
            self.last[o + x0 as usize..o + x1 as usize].fill(value);
        }
    }

    /// The open batch went out in `layers` layers, a sync before each but
    /// the first: all but its last layer's writes are synced.
    fn closed(&mut self, layers: u32) {
        if layers >= 2 {
            self.synced = self.batch + layers - 1;
        }
        self.batch += MAX_LAYERS + 1;
    }

    /// A write outside a batch (a fill, an upload, a copy): to the next
    /// batch, an earlier one's.
    fn outside(&mut self, spans: &[Span]) {
        if self.batch == self.synced {
            self.batch += MAX_LAYERS + 1;
        }
        self.mark(spans, self.batch);
    }

    /// Everything is synced.
    fn all_synced(&mut self) {
        self.synced = self.batch;
        if self.batch > u32::MAX / 2 {
            self.last.fill(0);
            (self.synced, self.batch) = (0, 0);
        }
    }
}

/// `w` into a list of areas, unless one holds it; past [`MAX_DIRTY`] the
/// list becomes their union.
fn push_area(list: &mut Vec<Area>, w: Area) {
    if list.iter().any(|d| d.contains(&w)) {
        return;
    }
    list.push(w);
    if list.len() > MAX_DIRTY {
        let all = list.iter().skip(1).fold(list[0], |a, d| a.union(d));
        list.clear();
        list.push(all);
    }
}

/// A VRAM rectangle of a transfer: pixels `((x + i) & 1023, (y + j) & 511)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VramRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl VramRect {
    pub fn pixels(&self) -> usize {
        self.w as usize * self.h as usize
    }

    fn render(&self) -> render::Rect {
        render::Rect { x: self.x, y: self.y, w: self.w, h: self.h }
    }
}

/// The display state GP1 sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Display {
    /// GP1(03h) bit 0: 1 = display off.
    pub off: bool,
    /// GP1(05h): top-left of the display area (x even).
    pub x: u32,
    pub y: u32,
    /// GP1(06h): horizontal range in GPU clock ticks.
    pub h_start: u32,
    pub h_end: u32,
    /// GP1(07h): vertical range in scanlines.
    pub v_start: u32,
    pub v_end: u32,
    /// GP1(08h): display mode bits 0–7.
    pub mode: u32,
    /// GP1(04h): DMA direction.
    pub dma: u32,
}

impl Display {
    const RESET: Display =
        Display { off: true, x: 0, y: 0, h_start: 0x200, h_end: 0xC00, v_start: 0x10, v_end: 0x100, mode: 0, dma: 0 };

    /// 480-line interlace (GP1(08h) bits 2 and 5).
    pub fn interlaced_480(&self) -> bool {
        self.mode & 0x24 == 0x24
    }
}

/// One polygon vertex as the GPU parsed it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Vertex {
    x: i32,
    y: i32,
    rgb: u32,
    uv: u32,
}

impl Vertex {
    fn words(&self) -> [u32; 3] {
        [pack_xy(self.x, self.y), self.rgb, self.uv & 0xFFFF]
    }
}

/// A polyline point carried between segments.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LinePoint {
    x: i32,
    y: i32,
    rgb: u32,
}

/// A command spanning several FIFO phases.
#[derive(Debug, Clone, PartialEq, Eq)]
enum InCmd {
    None,
    /// The first triangle of a quad is decoded and waits for vertex 3.
    Quad { cc: u8, rec: [u32; PRIM_WORDS], first: [Vertex; 3], textured: bool, gouraud: bool },
    /// A polyline waits for its next vertex.
    Polyline { cc: u8, prev: LinePoint },
    /// CPU→VRAM: pixels received so far.
    Write { rect: VramRect, mask: u32, pixels: Vec<u16> },
    /// VRAM→CPU: waiting for the caller, then serving GPUREAD.
    Read { rect: VramRect, data: Option<Vec<u16>>, pos: usize },
}

/// The texture page state (GP0(E1h) bits 0–8 and 11, and polygons'
/// texture-page attribute).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TexPage {
    /// In units of 64 pixels.
    x64: u32,
    /// 0 or 1 (units of 256 rows).
    y256: u32,
    abr: u32,
    /// 0, 1, 2 (3 draws as 2).
    mode: u32,
    disable: bool,
}

/// The GPU front end.
#[derive(Debug, Clone)]
pub struct Gpu {
    // FIFO and multi-phase commands.
    fifo: Vec<u32>,
    in_cmd: InCmd,
    // Draw state.
    page: TexPage,
    tex_disable_allowed: bool,
    sprite_flip: u32,
    dither: bool,
    draw_to_display: bool,
    window: u32,
    clip: [u32; 4],
    offset: (i32, i32),
    mask_set: bool,
    mask_check: bool,
    irq: bool,
    read_latch: u32,
    // Display state.
    display: Display,
    /// Which field the display is reading (interlace line skip).
    field: bool,
    // Colour-table cache: the key the last load used, and its slot.
    clut_key: Option<u32>,
    clut_slot: u32,
    loads_in_batch: u32,
    // Output.
    out: Vec<u32>,
    layers: Vec<Layer>,
    emitted_state: Option<[u32; 3]>,
    dirty: Vec<Area>,
    written: Written,
}

impl Default for Gpu {
    fn default() -> Self {
        Gpu::new()
    }
}

/// Words a GP0 command needs before it can execute (Beetle's table):
/// the whole command, except polylines (more vertices until the
/// terminator) and CPU→VRAM transfers (pixel data after the header).
pub fn command_len(cc: u8) -> usize {
    match cc {
        0x02 => 3,
        0x20..=0x3F => {
            let (t, g) = ((cc & 4 != 0) as usize, (cc & 0x10 != 0) as usize);
            1 + 3 * (1 + t + g) - g
        }
        0x40..=0x5F => 3 + (cc & 0x10 != 0) as usize,
        0x60..=0x7F => 2 + (cc & 4 != 0) as usize + (cc & 0x18 == 0) as usize,
        0x80..=0x9F => 4,
        0xA0..=0xDF => 3,
        _ => 1,
    }
}

fn to15(rgb: u32) -> u32 {
    (rgb & 0xFF) >> 3 | ((rgb >> 8) & 0xFF) >> 3 << 5 | ((rgb >> 16) & 0xFF) >> 3 << 10
}

impl Gpu {
    pub fn new() -> Self {
        let mut g = Gpu {
            fifo: Vec::new(),
            in_cmd: InCmd::None,
            page: TexPage::default(),
            tex_disable_allowed: false,
            sprite_flip: 0,
            dither: false,
            draw_to_display: false,
            window: 0,
            clip: [0; 4],
            offset: (0, 0),
            mask_set: false,
            mask_check: false,
            irq: false,
            read_latch: 0,
            display: Display::RESET,
            field: false,
            clut_key: None,
            clut_slot: CLUT_SLOTS - 1,
            loads_in_batch: 0,
            out: Vec::new(),
            layers: Vec::new(),
            emitted_state: None,
            dirty: Vec::new(),
            written: Written::default(),
        };
        g.soft_reset();
        g
    }

    // ---------------------------------------------------------------
    // Ports

    /// One GP0 word (port write or DMA).
    pub fn gp0(&mut self, word: u32) {
        self.fifo.push(word);
        self.process();
    }

    /// Several GP0 words in order.
    pub fn gp0_words(&mut self, words: &[u32]) {
        for &w in words {
            self.gp0(w);
        }
    }

    /// One GP1 command.
    pub fn gp1(&mut self, word: u32) {
        let v = word & 0x00FF_FFFF;
        match word >> 24 {
            0x00 => self.soft_reset(),
            0x01 => {
                // psx-spx: resets the command buffer and the colour-table
                // cache. (Beetle keeps its colour-table cache here; no
                // vector can tell, since vectors carry no GP1 commands.)
                self.abort_command();
                self.fifo.clear();
                self.clut_key = None;
            }
            0x02 => self.irq = false,
            0x03 => self.display.off = v & 1 != 0,
            0x04 => self.display.dma = v & 3,
            0x05 => {
                self.display.x = v & 0x3FE;
                self.display.y = (v >> 10) & 0x1FF;
            }
            0x06 => {
                self.display.h_start = v & 0xFFF;
                self.display.h_end = (v >> 12) & 0xFFF;
            }
            0x07 => {
                self.display.v_start = v & 0x3FF;
                self.display.v_end = (v >> 10) & 0x3FF;
            }
            0x08 => self.display.mode = v & 0xFF,
            0x09 => self.tex_disable_allowed = v & 1 != 0,
            0x10 => {
                let ex = |latch: u32, mask: u32, val: u32| (latch & mask) | val;
                match v & 0xF {
                    0x2 => self.read_latch = ex(self.read_latch, 0xFFF0_0000, self.window),
                    0x3 => self.read_latch = ex(self.read_latch, 0xFFF0_0000, self.clip[1] << 10 | self.clip[0]),
                    0x4 => self.read_latch = ex(self.read_latch, 0xFFF0_0000, self.clip[3] << 10 | self.clip[2]),
                    0x5 => {
                        let (ox, oy) = self.offset;
                        self.read_latch = ex(self.read_latch, 0xFFC0_0000, (ox as u32 & 2047) | (oy as u32 & 2047) << 11)
                    }
                    0x7 => self.read_latch = 2,
                    0x8 => self.read_latch = 0,
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// GPUSTAT (1F801814h), as far as it depends on what the front end
    /// knows. Timing bits (31, 13) come from the field the caller set.
    pub fn status(&self) -> u32 {
        let d = &self.display;
        let mut s = (((d.mode << 1) & 0x7F) | ((d.mode >> 6) & 1)) << 16;
        s |= (d.mode & 0x80) << 7;
        s |= d.dma << 29;
        s |= (self.field as u32) << 31;
        s |= (!self.field as u32) << 13;
        if d.dma & 2 != 0 {
            s |= 1 << 25;
        }
        s |= (self.irq as u32) << 24;
        s |= (d.off as u32) << 23;
        let reading = matches!(self.in_cmd, InCmd::Read { .. });
        if matches!(self.in_cmd, InCmd::None) && self.fifo.is_empty() {
            s |= 1 << 26;
        }
        if reading {
            s |= 1 << 27;
        }
        if !matches!(self.in_cmd, InCmd::Quad { .. } | InCmd::Polyline { .. }) {
            s |= 1 << 28;
        }
        let p = &self.page;
        s |= p.x64 | p.y256 << 4 | p.abr << 5 | p.mode << 7;
        s |= (self.dither as u32) << 9 | (self.draw_to_display as u32) << 10;
        s |= (self.mask_set as u32) << 11 | (self.mask_check as u32) << 12;
        s |= (p.disable as u32) << 15;
        s
    }

    /// GPUREAD (1F801810h): VRAM data during a VRAM→CPU transfer whose
    /// pixels were supplied, else the GP1(10h) latch.
    pub fn read(&mut self) -> u32 {
        let InCmd::Read { rect, data: Some(data), pos } = &mut self.in_cmd else {
            return self.read_latch;
        };
        let mut word = 0;
        let mut done = false;
        for i in 0..2 {
            // Past the end the GPU reads on along the row; those pixels
            // are not part of the rectangle and read as 0 here.
            let px = data.get(*pos).copied().unwrap_or(0);
            word |= (px as u32) << (16 * i);
            *pos += 1;
            if *pos >= rect.pixels() {
                done = true;
            }
        }
        self.read_latch = word;
        if done {
            self.in_cmd = InCmd::None;
            self.process();
        }
        word
    }

    /// The display state.
    pub fn display(&self) -> &Display {
        &self.display
    }

    /// Which field the display reads (odd = true), for GPUSTAT and the
    /// interlace line skip; the runtime sets it at each vblank.
    pub fn set_field(&mut self, odd: bool) {
        self.field = odd;
    }

    // ---------------------------------------------------------------
    // Output

    /// Close the DRAW batch being built: a DRAW per layer, a sync before
    /// each but the first.
    pub fn flush(&mut self) {
        let layers = std::mem::take(&mut self.layers);
        let n = layers.len() as u32;
        for (i, l) in layers.into_iter().enumerate() {
            if i > 0 {
                self.sync_dirty();
            }
            self.out.push(header(op::DRAW, 1 + l.recs.len() as u32));
            self.out.push(l.n);
            self.out.extend_from_slice(&l.recs);
            for w in l.writes {
                push_area(&mut self.dirty, w);
            }
        }
        if n > 0 {
            self.written.closed(n);
            if self.written.batch > u32::MAX / 2 + u32::MAX / 4 {
                self.sync_dirty();
                self.written.all_synced();
            }
        }
        self.loads_in_batch = 0;
    }

    /// Everything emitted so far (the open batch closed), for
    /// `render_submit`.
    /// The draw state as the GP0 words that set it (E1h-E6h), when a
    /// recorded batch could start from it: between commands, and with no
    /// state that only GP1 sets (texture disable, an interlaced 480-line
    /// display, which skips rows).
    pub fn recordable_state(&self) -> Option<[u32; 6]> {
        if !self.fifo.is_empty() || !matches!(self.in_cmd, InCmd::None) || self.tex_disable_allowed || self.display.interlaced_480() {
            return None;
        }
        let p = &self.page;
        let e1 = p.x64 | p.y256 << 4 | p.abr << 5 | p.mode << 7 | (self.dither as u32) << 9 | (self.draw_to_display as u32) << 10 | self.sprite_flip;
        let [x0, y0, x1, y1] = self.clip;
        let (ox, oy) = self.offset;
        Some([
            0xE100_0000 | e1,
            0xE200_0000 | self.window,
            0xE300_0000 | x0 | y0 << 10,
            0xE400_0000 | x1 | y1 << 10,
            0xE500_0000 | (ox as u32 & 0x7FF) | (oy as u32 & 0x7FF) << 11,
            0xE600_0000 | self.mask_set as u32 | (self.mask_check as u32) << 1,
        ])
    }

    pub fn take_commands(&mut self) -> Vec<u32> {
        self.flush();
        std::mem::take(&mut self.out)
    }

    /// The VRAM→CPU transfer waiting for its pixels, if any.
    pub fn pending_readback(&self) -> Option<VramRect> {
        match &self.in_cmd {
            InCmd::Read { rect, data: None, .. } => Some(*rect),
            _ => None,
        }
    }

    /// Hand over the pending transfer's pixels (`w * h`, row-major) to be
    /// read through GPUREAD; `None` ends the transfer without data (for a
    /// caller that took the pixels from the backend directly).
    pub fn finish_readback(&mut self, pixels: Option<&[u16]>) {
        if let InCmd::Read { data, .. } = &mut self.in_cmd {
            match pixels {
                Some(px) => *data = Some(px.to_vec()),
                None => {
                    self.in_cmd = InCmd::None;
                    self.process();
                }
            }
        }
    }

    /// What the display shows now.
    pub fn present_desc(&self) -> PresentDesc {
        let d = &self.display;
        let mut flags = 0;
        if !d.off {
            flags |= PresentDesc::ENABLED;
        }
        if d.mode & 0x10 != 0 {
            flags |= PresentDesc::DEPTH24;
        }
        if d.interlaced_480() {
            flags |= PresentDesc::INTERLACED;
        }
        if d.mode & 0x08 != 0 {
            flags |= PresentDesc::PAL;
        }
        // Dot clock divider: 368-pixel mode (bit 6), else bits 0-1.
        let div = if d.mode & 0x40 != 0 { 7 } else { [10, 8, 5, 4][(d.mode & 3) as usize] };
        let ticks = d.h_end.saturating_sub(d.h_start);
        let width = ((ticks / div + 2) & !3).max(if ticks == 0 { 0 } else { 4 });
        let mut height = d.v_end.saturating_sub(d.v_start);
        if d.interlaced_480() {
            height *= 2;
        }
        PresentDesc { flags, x: d.x, y: d.y, width, height }
    }

    /// Append a PRESENT command for the current display state.
    pub fn emit_present(&mut self) {
        self.flush();
        let desc = self.present_desc();
        self.out.push(header(op::PRESENT, render::PRESENT_WORDS as u32));
        self.out.extend_from_slice(&desc.to_words());
    }

    // ---------------------------------------------------------------
    // Reset and abort

    fn soft_reset(&mut self) {
        self.abort_command();
        self.fifo.clear();
        self.irq = false;
        self.clut_key = None;
        self.display = Display::RESET;
        self.page = TexPage::default();
        self.sprite_flip = 0;
        self.dither = false;
        self.draw_to_display = false;
        self.window = 0;
        self.clip = [0; 4];
        self.offset = (0, 0);
        self.mask_set = false;
        self.mask_check = false;
        self.tex_disable_allowed = false;
    }

    /// End the multi-phase command in progress, keeping what the GPU had
    /// already done: the first triangle of a quad, the pixels of a
    /// CPU→VRAM transfer received so far.
    fn abort_command(&mut self) {
        match std::mem::replace(&mut self.in_cmd, InCmd::None) {
            InCmd::Quad { rec, first, .. } => {
                let mut tri = rec;
                tri[0] = (tri[0] & !prim::KIND_MASK) | prim::TRIANGLE;
                tri[12..].fill(0);
                self.add_polygon(tri, &first, false);
            }
            InCmd::Write { rect, mask, pixels } => {
                let full = pixels.len() / rect.w as usize;
                if full > 0 {
                    let r = VramRect { h: full as u32, ..rect };
                    self.emit_upload(r, mask, &pixels[..full * rect.w as usize]);
                }
                let rest = &pixels[full * rect.w as usize..];
                if !rest.is_empty() {
                    let r = VramRect { x: rect.x, y: rect.y + full as u32, w: rest.len() as u32, h: 1 };
                    self.emit_upload(r, mask, rest);
                }
            }
            _ => {}
        }
    }

    // ---------------------------------------------------------------
    // The FIFO

    fn process(&mut self) {
        loop {
            match &mut self.in_cmd {
                InCmd::Read { .. } => return,
                InCmd::Write { rect, pixels, .. } => {
                    if self.fifo.is_empty() {
                        return;
                    }
                    let word = self.fifo.remove(0);
                    let total = rect.pixels();
                    for half in [word & 0xFFFF, word >> 16] {
                        if pixels.len() < total {
                            pixels.push(half as u16);
                        }
                    }
                    if pixels.len() == total {
                        let InCmd::Write { rect, mask, pixels } = std::mem::replace(&mut self.in_cmd, InCmd::None) else {
                            unreachable!()
                        };
                        self.emit_upload(rect, mask, &pixels);
                    }
                }
                InCmd::Quad { cc, .. } => {
                    let cc = *cc;
                    let n = 1 + (cc & 4 != 0) as usize + (cc & 0x10 != 0) as usize;
                    if self.fifo.len() < n {
                        return;
                    }
                    let words: Vec<u32> = self.fifo.drain(..n).collect();
                    self.quad_second(&words);
                }
                InCmd::Polyline { cc, .. } => {
                    let cc = *cc;
                    let Some(&first) = self.fifo.first() else { return };
                    if first & 0xF000_F000 == 0x5000_5000 {
                        self.fifo.remove(0);
                        self.in_cmd = InCmd::None;
                        continue;
                    }
                    let n = 1 + (cc & 0x10 != 0) as usize;
                    if self.fifo.len() < n {
                        return;
                    }
                    let words: Vec<u32> = self.fifo.drain(..n).collect();
                    self.polyline_next(&words);
                }
                InCmd::None => {
                    let Some(&first) = self.fifo.first() else { return };
                    let cc = (first >> 24) as u8;
                    let n = command_len(cc);
                    if self.fifo.len() < n {
                        return;
                    }
                    let words: Vec<u32> = self.fifo.drain(..n).collect();
                    self.execute(cc, &words);
                }
            }
        }
    }

    fn execute(&mut self, cc: u8, cb: &[u32]) {
        match cc {
            0x01 => self.clut_key = None,
            0x02 => self.fill(cb),
            0x1F => self.irq = true,
            0x20..=0x3F => self.polygon(cc, cb),
            0x40..=0x5F => self.line(cc, cb),
            0x60..=0x7F => self.rect(cc, cb),
            0x80..=0x9F => self.copy(cb),
            0xA0..=0xBF => self.write_start(cb),
            0xC0..=0xDF => self.read_start(cb),
            0xE1 => {
                self.set_page(cb[0]);
                self.sprite_flip = cb[0] & 0x3000;
                self.dither = cb[0] & (1 << 9) != 0;
                self.draw_to_display = cb[0] & (1 << 10) != 0;
            }
            0xE2 => self.window = cb[0] & 0xF_FFFF,
            0xE3 => {
                self.clip[0] = cb[0] & 1023;
                self.clip[1] = (cb[0] >> 10) & 1023;
            }
            0xE4 => {
                self.clip[2] = cb[0] & 1023;
                self.clip[3] = (cb[0] >> 10) & 1023;
            }
            0xE5 => self.offset = (sext11(cb[0]), sext11(cb[0] >> 11)),
            0xE6 => {
                self.mask_set = cb[0] & 1 != 0;
                self.mask_check = cb[0] & 2 != 0;
            }
            _ => {}
        }
    }

    /// Texture page bits 0–8 (and 11 when GP1(09h) allows it).
    fn set_page(&mut self, w: u32) {
        self.page.x64 = w & 0xF;
        self.page.y256 = (w >> 4) & 1;
        self.page.abr = (w >> 5) & 3;
        self.page.mode = (w >> 7) & 3;
        if self.tex_disable_allowed {
            self.page.disable = w & (1 << 11) != 0;
        }
    }

    // ---------------------------------------------------------------
    // Hazards and emission

    /// The batch state the primitives that follow need.
    fn state_words(&self) -> [u32; 3] {
        let [x0, y0, x1, y1] = self.clip;
        let skip = if self.display.interlaced_480() && !self.draw_to_display {
            // Skip rows of the parity the display is reading.
            if (self.display.y + self.field as u32) & 1 == 0 { render::state::SKIP_EVEN } else { render::state::SKIP_ODD }
        } else {
            0
        };
        [x0 | y0 << 16, x1 | y1 << 16, skip]
    }

    fn ensure_state(&mut self) {
        let s = self.state_words();
        if self.emitted_state != Some(s) {
            self.flush();
            self.out.push(header(op::SET_STATE, 3));
            self.out.extend_from_slice(&s);
            self.emitted_state = Some(s);
        }
    }

    /// Before reading `reads`: if any was written since the last sync,
    /// close the batch and sync everything written.
    fn before_reads(&mut self, reads: &Areas) {
        let open = self.layers.iter().flat_map(|l| &l.writes);
        let dirty: Vec<Area> = self.dirty.iter().chain(open).copied().collect();
        if reads.iter().any(|r| dirty.iter().any(|d| d.overlaps(&r))) {
            self.sync();
        }
    }

    /// Close the batch and sync everything written.
    fn sync(&mut self) {
        self.flush();
        self.sync_dirty();
        self.written.all_synced();
    }

    fn sync_dirty(&mut self) {
        if self.dirty.is_empty() {
            return;
        }
        self.out.push(header(op::SYNC, 1 + 2 * self.dirty.len() as u32));
        self.out.push(self.dirty.len() as u32);
        for d in self.dirty.drain(..) {
            self.out.extend_from_slice(&d.rect().words());
        }
    }

    /// A write outside a batch (a fill, an upload, a copy's destination).
    fn wrote(&mut self, writes: &Areas) {
        self.flush();
        self.written.outside(&writes.spans());
        for w in writes.iter() {
            push_area(&mut self.dirty, w);
        }
    }

    /// A primitive that reads `reads` (its texels) and plots `cover`
    /// within `writes`; with semi-transparency or the mask check it reads
    /// what it plots over too.
    ///
    /// The batch is drawn in layers with a sync between each two, each
    /// layer in the primitives' order, so a primitive goes in the first
    /// layer that keeps what it sees: one that reads what it plots over
    /// goes above the layer of the last write to any of its pixels, any
    /// other one not below it, nor below a primitive that samples where
    /// it plots. A translucent model's triangles that cover each other
    /// then take as many syncs as it has surfaces, not one each.
    fn add_prim(&mut self, rec: [u32; PRIM_WORDS], reads: Areas, writes: Areas, cover: &[Span]) {
        self.ensure_state();
        self.before_reads(&reads);
        let reader = rec[0] & (prim::SEMI | prim::MASK_CHECK) != 0;
        let (top, earlier) = self.written.scan(cover);
        let mut layer = top.map_or(0, |t| t + reader as u32);
        let sampled = self.layers.iter().rposition(|l| l.texels.iter().any(|t| writes.iter().any(|w| w.overlaps(t))));
        layer = layer.max(sampled.unwrap_or(0) as u32);
        if (reader && earlier) || layer >= MAX_LAYERS {
            self.sync();
            layer = 0;
        }
        if self.layers.len() <= layer as usize {
            self.layers.resize_with(layer as usize + 1, Layer::default);
        }
        self.written.mark(cover, self.written.batch + 1 + layer);
        let l = &mut self.layers[layer as usize];
        l.recs.extend_from_slice(&rec);
        l.n += 1;
        for w in writes.iter() {
            push_area(&mut l.writes, w);
        }
        if rec[0] & prim::TEXTURED != 0 {
            for t in reads.iter() {
                push_area(&mut l.texels, t);
            }
        }
    }

    /// The pixels a polygon plots, as the GPU's edge walkers decide.
    fn polygon_cover(&self, rec: &[u32; PRIM_WORDS], quad: bool) -> Vec<Span> {
        let xy = |n: usize| render::unpack_xy(rec[3 + 3 * n]);
        let tris: &[[usize; 3]] = if quad { &[[0, 1, 2], [1, 2, 3]] } else { &[[0, 1, 2]] };
        let clip = self.clip.map(|c| c as i32);
        let mut out = Vec::new();
        for t in tris {
            if let Some(t) = Triangle::new(t.map(xy)) {
                t.spans(clip, |row, x0, x1| out.push((row, x0, x1)));
            }
        }
        out
    }

    /// The colour-table cache: load the table if its address or depth
    /// changed since the last load. Returns the slot primitives use.
    fn load_clut(&mut self, raw: u32, mode: u32) -> u32 {
        if mode >= 2 {
            return self.clut_slot;
        }
        let key = (raw & 0x7FFF) | mode << 16;
        if self.clut_key == Some(key) {
            return self.clut_slot;
        }
        let (x, y) = ((raw & 0x3F) << 4, (raw >> 6) & 0x1FF);
        let count = if mode == 1 { 256 } else { 16 };
        self.before_reads(&Areas::wrapping(x, y, count, 1));
        if self.loads_in_batch + 1 >= CLUT_SLOTS {
            self.flush();
        }
        self.clut_slot = (self.clut_slot + 1) % CLUT_SLOTS;
        self.loads_in_batch += 1;
        self.out.push(header(op::CLUT_LOAD, 3));
        self.out.extend_from_slice(&[self.clut_slot, x | y << 16, count]);
        self.clut_key = Some(key);
        self.clut_slot
    }

    /// The VRAM a textured primitive can sample: texture coordinates in
    /// `u.0..=u.1` and `v.0..=v.1` (a range outside 0..=255 wraps, so it
    /// means all of them), through the window, on its page.
    fn texture_reads(page: &TexPage, window: u32, u: (i32, i32), v: (i32, i32)) -> Areas {
        let full = |r: (i32, i32), mask: u32| if r.0 < 0 || r.1 > 255 || mask != 0 { (0, 255) } else { (r.0 as u32, r.1 as u32) };
        let (u0, u1) = full(u, window & 0x1F);
        let (v0, v1) = full(v, (window >> 5) & 0x1F);
        let shift = 2 - page.mode.min(2);
        let x0 = page.x64 * 64 + (u0 >> shift);
        let x1 = page.x64 * 64 + (u1 >> shift);
        Areas::wrapping(x0, page.y256 * 256 + v0, x1 - x0 + 1, v1 - v0 + 1)
    }

    /// Where a primitive with this coordinate bounding box can plot,
    /// clipped to the drawing area. `exact` says the coordinates need no
    /// wrap handling; otherwise the whole drawing area is assumed.
    fn plot_areas(&self, x0: i32, x1: i32, y0: i32, y1: i32, exact: bool) -> Areas {
        let [cx0, cy0, cx1, cy1] = self.clip.map(|v| v as i32);
        let (ax0, ax1, ay0, ay1) = if exact { (x0.max(cx0), x1.min(cx1), y0.max(cy0), y1.min(cy1)) } else { (cx0, cx1, cy0, cy1) };
        if ax0 > ax1 || ay0 > ay1 {
            return Areas::none();
        }
        // Rows plot at y & 511; the drawing area spans up to 1024 rows.
        let rows = if ay0 >> 9 == ay1 >> 9 { (ay0 & 511, (ay1 & 511) + 1) } else { (0, 512) };
        Areas::one(Area { x0: ax0 as u16, y0: rows.0 as u16, x1: (ax1 + 1) as u16, y1: rows.1 as u16 })
    }

    fn base_flags(&self, semi: bool, abr: u32) -> u32 {
        let mut f = 0;
        if semi {
            f |= prim::SEMI | abr << prim::ABR_SHIFT;
        }
        if self.mask_set {
            f |= prim::MASK_SET;
        }
        if self.mask_check {
            f |= prim::MASK_CHECK;
        }
        f
    }

    fn reads_for(flags: u32, texels: &Areas) -> Areas {
        if flags & prim::TEXTURED != 0 { *texels } else { Areas::none() }
    }

    // ---------------------------------------------------------------
    // Polygons

    fn polygon(&mut self, cc: u8, cb: &[u32]) {
        let gouraud = cc & 0x10 != 0;
        let quad = cc & 0x08 != 0;
        let textured = cc & 0x04 != 0;
        let semi = cc & 0x02 != 0;
        let raw = cc & 0x01 != 0;
        if textured {
            self.set_page(cb[4 + gouraud as usize] >> 16);
        }
        let mut v = [Vertex::default(); 3];
        let mut i = 0;
        let mut clut_raw = 0;
        for (n, vert) in v.iter_mut().enumerate() {
            if n == 0 || gouraud {
                vert.rgb = cb[i] & 0xFF_FFFF;
                i += 1;
            } else {
                vert.rgb = cb[0] & 0xFF_FFFF;
            }
            vert.x = sext11(cb[i]) + self.offset.0;
            vert.y = sext11(cb[i] >> 16) + self.offset.1;
            i += 1;
            if textured {
                vert.uv = cb[i];
                if n == 0 {
                    clut_raw = cb[i] >> 16;
                }
                i += 1;
            }
        }
        let page = self.page;
        let mut flags = self.base_flags(semi, page.abr) | prim::TRIANGLE;
        let mut slot = 0;
        if textured {
            flags |= prim::TEXTURED | page.mode.min(2) << prim::DEPTH_SHIFT;
            if raw {
                flags |= prim::RAW;
            }
            slot = self.load_clut(clut_raw, page.mode);
        }
        if gouraud {
            flags |= prim::GOURAUD;
        }
        if self.dither && if textured { !raw } else { gouraud } {
            flags |= prim::DITHER;
        }
        let mut rec = [0u32; PRIM_WORDS];
        rec[0] = flags;
        rec[1] = prim::texture_word(page.x64, page.y256, slot);
        rec[2] = self.window;
        for (n, vert) in v.iter().enumerate() {
            rec[3 + 3 * n..6 + 3 * n].copy_from_slice(&vert.words());
        }
        if quad {
            self.in_cmd = InCmd::Quad { cc, rec, first: v, textured, gouraud };
        } else {
            self.add_polygon(rec, &v, false);
        }
    }

    fn quad_second(&mut self, words: &[u32]) {
        let InCmd::Quad { mut rec, first, textured, gouraud, .. } = std::mem::replace(&mut self.in_cmd, InCmd::None) else {
            return;
        };
        let mut i = 0;
        let mut v = Vertex { rgb: first[0].rgb, ..Vertex::default() };
        if gouraud {
            v.rgb = words[i] & 0xFF_FFFF;
            i += 1;
        }
        v.x = sext11(words[i]) + self.offset.0;
        v.y = sext11(words[i] >> 16) + self.offset.1;
        i += 1;
        if textured {
            v.uv = words[i];
        }
        rec[0] = (rec[0] & !prim::KIND_MASK) | prim::QUAD;
        rec[12..15].copy_from_slice(&v.words());
        let all = [first[0], first[1], first[2], v];
        self.add_polygon(rec, &all, true);
    }

    fn add_polygon(&mut self, mut rec: [u32; PRIM_WORDS], v: &[Vertex], quad: bool) {
        let xs = v.iter().map(|p| p.x);
        let ys = v.iter().map(|p| p.y);
        let (x0, x1) = (xs.clone().min().unwrap_or(0), xs.max().unwrap_or(0));
        let (y0, y1) = (ys.clone().min().unwrap_or(0), ys.max().unwrap_or(0));
        let exact = v.iter().all(|p| (-1024..1024).contains(&p.x) && (-1024..1024).contains(&p.y));
        let writes = self.plot_areas(x0 - 1, x1 + 1, y0, y1, exact);
        let page = TexPage {
            x64: rec[1] & 0xF,
            y256: (rec[1] >> 4) & 1,
            mode: (rec[0] & prim::DEPTH_MASK) >> prim::DEPTH_SHIFT,
            ..TexPage::default()
        };
        // Interpolated coordinates stay within the vertices' range, give or
        // take the rounding of the setup.
        let us = v.iter().map(|p| (p.uv & 0xFF) as i32);
        let vs = v.iter().map(|p| ((p.uv >> 8) & 0xFF) as i32);
        let u = (us.clone().min().unwrap_or(0) - 1, us.max().unwrap_or(0) + 1);
        let vv = (vs.clone().min().unwrap_or(0) - 1, vs.max().unwrap_or(0) + 1);
        let texels = Self::texture_reads(&page, rec[2], u, vv);
        rec[0] |= Self::serial(rec[0], &texels, &writes);
        let reads = Self::reads_for(rec[0], &texels);
        let cover = self.polygon_cover(&rec, quad);
        self.add_prim(rec, reads, writes, &cover);
    }

    /// [`prim::SERIAL`] when a textured primitive can sample where it
    /// draws.
    fn serial(flags: u32, texels: &Areas, writes: &Areas) -> u32 {
        let feedback = flags & prim::TEXTURED != 0 && texels.iter().any(|t| writes.iter().any(|w| w.overlaps(&t)));
        if feedback { prim::SERIAL } else { 0 }
    }

    // ---------------------------------------------------------------
    // Lines

    fn line(&mut self, cc: u8, cb: &[u32]) {
        let gouraud = cc & 0x10 != 0;
        let p0 = LinePoint { rgb: cb[0] & 0xFF_FFFF, x: sext11(cb[1]) + self.offset.0, y: sext11(cb[1] >> 16) + self.offset.1 };
        self.line_to(cc, p0, &cb[2..], gouraud);
    }

    fn polyline_next(&mut self, words: &[u32]) {
        let InCmd::Polyline { cc, prev } = self.in_cmd else { return };
        self.line_to(cc, prev, words, cc & 0x10 != 0);
    }

    /// The segment from `p0` to the vertex in `rest` ([colour,] xy).
    fn line_to(&mut self, cc: u8, p0: LinePoint, rest: &[u32], gouraud: bool) {
        let (rgb1, xy) = if gouraud { (rest[0] & 0xFF_FFFF, rest[1]) } else { (p0.rgb, rest[0]) };
        let p1 = LinePoint { rgb: rgb1, x: sext11(xy) + self.offset.0, y: sext11(xy >> 16) + self.offset.1 };
        if cc & 0x08 != 0 {
            self.in_cmd = InCmd::Polyline { cc, prev: p1 };
        }
        if (p1.x - p0.x).abs() >= 1024 || (p1.y - p0.y).abs() >= 512 {
            return;
        }
        let mut flags = self.base_flags(cc & 0x02 != 0, self.page.abr) | prim::LINE;
        if gouraud {
            flags |= prim::GOURAUD;
        }
        if self.dither {
            flags |= prim::DITHER;
        }
        let mut rec = [0u32; PRIM_WORDS];
        rec[0] = flags;
        rec[3] = pack_xy(p0.x, p0.y);
        rec[4] = p0.rgb;
        rec[5] = pack_xy(p1.x, p1.y);
        rec[6] = p1.rgb;
        let exact = [p0, p1].iter().all(|p| (0..1024).contains(&p.x) && (0..1024).contains(&p.y));
        let writes =
            self.plot_areas(p0.x.min(p1.x), p0.x.max(p1.x), p0.y.min(p1.y), p0.y.max(p1.y), exact);
        self.add_prim(rec, Areas::none(), writes, &writes.spans());
    }

    // ---------------------------------------------------------------
    // Rectangles

    fn rect(&mut self, cc: u8, cb: &[u32]) {
        let textured = cc & 0x04 != 0;
        let semi = cc & 0x02 != 0;
        let raw = cc & 0x01 != 0;
        let color = cb[0] & 0xFF_FFFF;
        let (mut x, mut y) = (sext11(cb[1]), sext11(cb[1] >> 16));
        let mut i = 2;
        let mut uv = 0;
        let mut slot = 0;
        let page = self.page;
        if textured {
            uv = cb[2] & 0xFFFF;
            slot = self.load_clut(cb[2] >> 16, page.mode);
            i += 1;
        }
        let (w, h) = match (cc >> 3) & 3 {
            0 => (cb[i] & 0x3FF, (cb[i] >> 16) & 0x1FF),
            1 => (1, 1),
            2 => (8, 8),
            _ => (16, 16),
        };
        x = sext11((x + self.offset.0) as u32);
        y = sext11((y + self.offset.1) as u32);
        if w == 0 || h == 0 {
            return;
        }
        let mut flags = self.base_flags(semi, page.abr) | prim::RECT;
        if textured {
            flags |= prim::TEXTURED | page.mode.min(2) << prim::DEPTH_SHIFT;
            if raw {
                flags |= prim::RAW;
            }
            if self.sprite_flip & 0x1000 != 0 {
                flags |= prim::FLIP_X;
            }
            if self.sprite_flip & 0x2000 != 0 {
                flags |= prim::FLIP_Y;
            }
        }
        let mut rec = [0u32; PRIM_WORDS];
        rec[0] = flags;
        rec[1] = prim::texture_word(page.x64, page.y256, slot);
        rec[2] = self.window;
        rec[3] = pack_xy(x, y);
        rec[4] = w | h << 16;
        rec[5] = color;
        rec[6] = uv;
        let writes = self.plot_areas(x, x + w as i32 - 1, y, y + h as i32 - 1, true);
        // Texture coordinates step by one per pixel (down with a flip).
        let (u0, v0) = ((uv & 0xFF) as i32, ((uv >> 8) & 0xFF) as i32);
        let u = if flags & prim::FLIP_X != 0 { ((u0 | 1) - (w as i32 - 1), u0 | 1) } else { (u0, u0 + w as i32 - 1) };
        let v = if flags & prim::FLIP_Y != 0 { (v0 - (h as i32 - 1), v0) } else { (v0, v0 + h as i32 - 1) };
        let texels = Self::texture_reads(&page, self.window, u, v);
        rec[0] |= Self::serial(flags, &texels, &writes);
        let reads = Self::reads_for(flags, &texels);
        self.add_prim(rec, reads, writes, &writes.spans());
    }

    // ---------------------------------------------------------------
    // Fills and transfers

    fn fill(&mut self, cb: &[u32]) {
        let x = cb[1] & 0x3F0;
        let y = (cb[1] >> 16) & 0x1FF;
        let w = ((cb[2] & 0x3FF) + 0xF) & !0xF;
        let h = (cb[2] >> 16) & 0x1FF;
        if w == 0 || h == 0 {
            return;
        }
        self.ensure_state();
        self.flush();
        let r = VramRect { x, y, w, h }.render();
        self.out.push(header(op::FILL, 3));
        self.out.extend_from_slice(&[r.words()[0], r.words()[1], to15(cb[0])]);
        self.wrote(&Areas::wrapping(x, y, w, h));
    }

    fn mask_flags(&self) -> u32 {
        ((self.mask_set as u32) * render::mask::SET) | ((self.mask_check as u32) * render::mask::CHECK)
    }

    fn emit_upload(&mut self, rect: VramRect, mask: u32, pixels: &[u16]) {
        self.flush();
        let dst = Areas::wrapping(rect.x, rect.y, rect.w, rect.h);
        if mask & render::mask::CHECK != 0 {
            self.before_reads(&dst);
        }
        let r = rect.render();
        self.out.push(header(op::UPLOAD, 3 + pixels.len().div_ceil(2) as u32));
        self.out.extend_from_slice(&[r.words()[0], r.words()[1], mask]);
        for pair in pixels.chunks(2) {
            self.out.push(pair[0] as u32 | (pair.get(1).copied().unwrap_or(0) as u32) << 16);
        }
        self.wrote(&dst);
    }

    fn write_start(&mut self, cb: &[u32]) {
        let x = cb[1] & 0x3FF;
        let y = (cb[1] >> 16) & 0x1FF;
        let w = match cb[2] & 0x3FF {
            0 => 0x400,
            w => w,
        };
        let h = match (cb[2] >> 16) & 0x1FF {
            0 => 0x200,
            h => h,
        };
        let rect = VramRect { x, y, w, h };
        self.in_cmd = InCmd::Write { rect, mask: self.mask_flags(), pixels: Vec::with_capacity(rect.pixels()) };
    }

    fn read_start(&mut self, cb: &[u32]) {
        let x = cb[1] & 0x3FF;
        let y = (cb[1] >> 16) & 0x1FF;
        let w = match cb[2] & 0x3FF {
            0 => 0x400,
            w => w,
        };
        let mut h = (cb[2] >> 16) & 0x3FF;
        if h > 0x200 {
            h &= 0x1FF;
        }
        if h == 0 {
            return;
        }
        self.flush();
        self.in_cmd = InCmd::Read { rect: VramRect { x, y, w, h }, data: None, pos: 0 };
    }

    fn copy(&mut self, cb: &[u32]) {
        let (sx, sy) = (cb[1] & 0x3FF, (cb[1] >> 16) & 0x1FF);
        let (dx, dy) = (cb[2] & 0x3FF, (cb[2] >> 16) & 0x1FF);
        let w = match cb[3] & 0x3FF {
            0 => 0x400,
            w => w,
        };
        let h = match (cb[3] >> 16) & 0x1FF {
            0 => 0x200,
            h => h,
        };
        let mask = self.mask_flags();
        self.flush();
        let src = Areas::wrapping(sx, sy, w, h);
        let dst = Areas::wrapping(dx, dy, w, h);
        let overlap = src.iter().any(|s| dst.iter().any(|d| d.overlaps(&s)));
        if !overlap {
            self.emit_copy(CopyPiece { sx, sy, dx, dy, w, h }, mask);
            return;
        }
        for piece in ordered_copy(sx, sy, dx, dy, w, h) {
            self.emit_copy(piece, mask);
        }
    }

    fn emit_copy(&mut self, p: CopyPiece, mask: u32) {
        let mut reads = Areas::wrapping(p.sx, p.sy, p.w, p.h);
        let dst = Areas::wrapping(p.dx, p.dy, p.w, p.h);
        if mask & render::mask::CHECK != 0 {
            for d in dst.iter() {
                reads.push(d);
            }
        }
        self.before_reads(&reads);
        self.out.push(header(op::COPY, 4));
        self.out.extend_from_slice(&[(p.sx | p.sy << 16), (p.dx | p.dy << 16), (p.w | p.h << 16), mask]);
        self.wrote(&dst);
    }
}

/// A piece of a VRAM→VRAM copy that can execute as a snapshot copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CopyPiece {
    sx: u32,
    sy: u32,
    dx: u32,
    dy: u32,
    w: u32,
    h: u32,
}

/// The GPU copies row by row, each row in chunks of 128 pixels: a chunk
/// reads its source pixels, then writes them. When the source and
/// destination overlap, later chunks can read what earlier ones wrote.
/// Split such a copy into pieces whose chunks never read a pixel written
/// by an earlier chunk of the same piece, so each piece is a snapshot
/// copy; the hazard tracking syncs between pieces.
fn ordered_copy(sx: u32, sy: u32, dx: u32, dy: u32, w: u32, h: u32) -> Vec<CopyPiece> {
    // Destination x-intervals written per row by the current piece.
    let mut written: std::collections::BTreeMap<u32, Vec<(u32, u32)>> = Default::default();
    let mut pieces: Vec<CopyPiece> = Vec::new();
    // Units of the current piece: (row, chunk x, chunk len).
    let mut units: Vec<(u32, u32, u32)> = Vec::new();
    let intervals = |x: u32, len: u32| -> [(u32, u32); 2] {
        let x = x & 1023;
        if x + len <= 1024 { [(x, x + len), (0, 0)] } else { [(x, 1024), (0, x + len - 1024)] }
    };
    let flush = |units: &mut Vec<(u32, u32, u32)>, pieces: &mut Vec<CopyPiece>| {
        // Merge consecutive units: same row and contiguous into one row
        // run, then equal runs on consecutive rows into one rectangle.
        let mut runs: Vec<(u32, u32, u32)> = Vec::new();
        for &(row, x, len) in units.iter() {
            match runs.last_mut() {
                Some((r, rx, rl)) if *r == row && *rx + *rl == x => *rl += len,
                _ => runs.push((row, x, len)),
            }
        }
        let mut i = 0;
        while i < runs.len() {
            let (row, x, len) = runs[i];
            let mut j = i + 1;
            while j < runs.len() && runs[j] == (row + (j - i) as u32, x, len) {
                j += 1;
            }
            pieces.push(CopyPiece {
                sx: (sx + x) & 1023,
                sy: (sy + row) & 511,
                dx: (dx + x) & 1023,
                dy: (dy + row) & 511,
                w: len,
                h: (j - i) as u32,
            });
            i = j;
        }
        units.clear();
    };
    for row in 0..h {
        let mut x = 0;
        while x < w {
            let len = (w - x).min(128);
            let srow = (sy + row) & 511;
            let reads_written = written.get(&srow).is_some_and(|iv| {
                intervals(sx + x, len).iter().filter(|(a, b)| b > a).any(|&(a, b)| iv.iter().any(|&(c, d)| a < d && c < b))
            });
            if reads_written {
                flush(&mut units, &mut pieces);
                written.clear();
            }
            units.push((row, x, len));
            let drow = (dy + row) & 511;
            let e = written.entry(drow).or_default();
            for (a, b) in intervals(dx + x, len) {
                if b > a {
                    e.push((a, b));
                }
            }
            x += len;
        }
    }
    flush(&mut units, &mut pieces);
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;
    use psxw_abi::render::{Command, commands, unpack_xy};

    fn cmds(g: &mut Gpu) -> Vec<u32> {
        g.take_commands()
    }

    /// Decoded commands, owned, for comparisons.
    #[derive(Debug, Clone, PartialEq)]
    enum C {
        State([u32; 4], u32),
        Draw(Vec<[u32; PRIM_WORDS]>),
        Fill(render::Rect, u32),
        Upload(render::Rect, u32, Vec<u32>),
        Copy([u32; 7]),
        Sync(Vec<render::Rect>),
        Clut(u32, u32, u32, u32),
        Present(PresentDesc),
        Readback(render::Rect),
    }

    fn decode(words: &[u32]) -> Vec<C> {
        commands(words)
            .map(|c| match c.expect("valid stream") {
                Command::SetState { clip, flags } => C::State(clip, flags),
                Command::Draw(recs) => C::Draw(recs.chunks_exact(PRIM_WORDS).map(|r| r.try_into().unwrap()).collect()),
                Command::Fill { rect, color } => C::Fill(rect, color),
                Command::Upload { rect, mask, pixels } => C::Upload(rect, mask, pixels.to_vec()),
                Command::Copy { src_x, src_y, dst_x, dst_y, w, h, mask } => C::Copy([src_x, src_y, dst_x, dst_y, w, h, mask]),
                Command::Sync(r) => C::Sync(r.chunks_exact(2).map(|c| render::Rect::from_words(c[0], c[1])).collect()),
                Command::ClutLoad { slot, x, y, count } => C::Clut(slot, x, y, count),
                Command::Present(d) => C::Present(d),
                Command::Readback(r) => C::Readback(r),
            })
            .collect()
    }

    /// A GPU with a full-VRAM drawing area.
    fn gpu() -> Gpu {
        let mut g = Gpu::new();
        g.gp0_words(&[0xE300_0000, 0xE400_0000 | 511 << 10 | 1023, 0xE500_0000]);
        g
    }

    fn draws(cs: &[C]) -> Vec<[u32; PRIM_WORDS]> {
        cs.iter()
            .filter_map(|c| match c {
                C::Draw(r) => Some(r.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    #[test]
    fn command_lengths_match_the_gpu() {
        assert_eq!(command_len(0x20), 4);
        assert_eq!(command_len(0x28), 4); // quads read their 4th vertex later
        assert_eq!(command_len(0x24), 7);
        assert_eq!(command_len(0x30), 6);
        assert_eq!(command_len(0x34), 9);
        assert_eq!(command_len(0x3C), 9);
        assert_eq!(command_len(0x40), 3);
        assert_eq!(command_len(0x50), 4);
        assert_eq!(command_len(0x60), 3);
        assert_eq!(command_len(0x64), 4);
        assert_eq!(command_len(0x68), 2);
        assert_eq!(command_len(0x7C), 3);
        assert_eq!(command_len(0x02), 3);
        assert_eq!(command_len(0x80), 4);
        assert_eq!(command_len(0xA0), 3);
        assert_eq!(command_len(0xC0), 3);
        assert_eq!(command_len(0xE1), 1);
        assert_eq!(command_len(0x00), 1);
    }

    /// Every polygon opcode decodes into one primitive with the flags its
    /// bits imply; quads carry all four vertices.
    #[test]
    fn every_polygon_opcode() {
        for cc in 0x20u32..=0x3F {
            let mut g = gpu();
            g.gp0(0xE100_0200); // dither on, abr 0
            let (gouraud, quad, textured, semi, raw) = (cc & 0x10 != 0, cc & 8 != 0, cc & 4 != 0, cc & 2 != 0, cc & 1 != 0);
            let mut words = vec![cc << 24 | 0x10_2030];
            let nv = if quad { 4 } else { 3 };
            for n in 0..nv {
                if n > 0 && gouraud {
                    words.push(0x40_5060 + n);
                }
                words.push(pack_xy(10 * n as i32, 20 + n as i32));
                if textured {
                    let attr = match n {
                        0 => 0x7FC2u32 << 16, // CLUT at (32, 511)
                        1 => (1 << 7 | 3 << 5 | 2) << 16, // 8-bit, abr 3, page x 128
                        _ => 0,
                    };
                    words.push(attr | ((n * 9) << 8) | (n * 7));
                }
            }
            g.gp0_words(&words);
            let cs = decode(&cmds(&mut g));
            let recs = draws(&cs);
            assert_eq!(recs.len(), 1, "{cc:#x}");
            let r = recs[0];
            assert_eq!(r[0] & prim::KIND_MASK, if quad { prim::QUAD } else { prim::TRIANGLE }, "{cc:#x}");
            assert_eq!(r[0] & prim::GOURAUD != 0, gouraud);
            assert_eq!(r[0] & prim::TEXTURED != 0, textured);
            assert_eq!(r[0] & prim::SEMI != 0, semi);
            assert_eq!(r[0] & prim::RAW != 0, textured && raw);
            // Blend mode: the polygon's own page for textured ones.
            if semi {
                assert_eq!((r[0] & prim::ABR_MASK) >> prim::ABR_SHIFT, if textured { 3 } else { 0 });
            }
            assert_eq!(r[0] & prim::DITHER != 0, if textured { !raw } else { gouraud }, "{cc:#x}");
            for n in 0..nv as usize {
                assert_eq!(unpack_xy(r[3 + 3 * n]), (10 * n as i32, 20 + n as i32));
                let rgb = if gouraud && n > 0 { 0x40_5060 + n as u32 } else { 0x10_2030 };
                assert_eq!(r[4 + 3 * n], rgb, "{cc:#x} vertex {n}");
                if textured {
                    assert_eq!(r[5 + 3 * n], ((n as u32 * 9) << 8) | (n as u32 * 7));
                }
            }
            if textured {
                assert_eq!(r[1] & 0xF, 2);
                assert_eq!((r[0] & prim::DEPTH_MASK) >> prim::DEPTH_SHIFT, 1);
                assert!(cs.contains(&C::Clut(r[1] >> 16, 32, 511, 256)), "{cc:#x}: {cs:?}");
                assert_eq!(g.status() & 0x1FF, 2 | 3 << 5 | 1 << 7, "the texture page updates the draw mode");
            }
        }
    }

    #[test]
    fn drawing_offset_applies_to_polygons_and_lines() {
        let mut g = gpu();
        g.gp0(0xE500_0000 | (-5i32 as u32 & 2047) | (7 << 11));
        g.gp0_words(&[0x2000_0000, pack_xy(1, 1), pack_xy(9, 1), pack_xy(1, 9)]);
        g.gp0_words(&[0x4000_0000, pack_xy(0, 0), pack_xy(3, 3)]);
        let r = draws(&decode(&cmds(&mut g)));
        assert_eq!(unpack_xy(r[0][3]), (-4, 8));
        assert_eq!(unpack_xy(r[1][3]), (-5, 7));
        assert_eq!(unpack_xy(r[1][5]), (-2, 10));
    }

    #[test]
    fn every_line_opcode_and_polylines() {
        for cc in 0x40u32..=0x5F {
            let mut g = gpu();
            g.gp0(0xE100_0200);
            let (gouraud, poly) = (cc & 0x10 != 0, cc & 8 != 0);
            let mut words = vec![cc << 24 | 0x00_00FF, pack_xy(0, 0)];
            let pts = if poly { 4 } else { 2 };
            for n in 1..pts {
                if gouraud {
                    words.push(0x00FF_0000 | n);
                }
                words.push(pack_xy(5 * n as i32, 3 * n as i32));
            }
            if poly {
                words.push(0x5555_5555);
            }
            g.gp0_words(&words);
            g.gp0_words(&[0x2000_0000, pack_xy(0, 0), pack_xy(1, 0), pack_xy(0, 1)]); // after the terminator
            let recs = draws(&decode(&cmds(&mut g)));
            assert_eq!(recs.len(), pts as usize, "{cc:#x}");
            // The triangle may go out before segments it doesn't touch (a
            // translucent polyline's segments are layers of their own).
            let lines: Vec<_> = recs.iter().filter(|r| r[0] & prim::KIND_MASK == prim::LINE).collect();
            assert_eq!(lines.len(), pts as usize - 1, "{cc:#x}");
            for (n, r) in lines.into_iter().enumerate() {
                assert_eq!(r[0] & prim::KIND_MASK, prim::LINE);
                assert!(r[0] & prim::DITHER != 0, "lines dither whenever dithering is on");
                assert_eq!(r[0] & prim::SEMI != 0, cc & 2 != 0);
                assert_eq!(unpack_xy(r[3]), (5 * n as i32, 3 * n as i32));
                assert_eq!(unpack_xy(r[5]), (5 * (n as i32 + 1), 3 * (n as i32 + 1)));
                let c1 = if gouraud { 0x00FF_0000 | (n as u32 + 1) } else { 0xFF };
                assert_eq!(r[6], c1);
            }
            assert_eq!(recs.iter().filter(|r| r[0] & prim::KIND_MASK == prim::TRIANGLE).count(), 1);
        }
    }

    #[test]
    fn polyline_terminator_only_where_a_vertex_starts() {
        let mut g = gpu();
        // Gouraud polyline: 5xxx5xxx as an xy word (after a colour) is a
        // vertex; where a colour would start it terminates.
        g.gp0_words(&[0x5800_0000, pack_xy(0, 0), 0x0000_0001, 0x5005_5005, 0x5000_5000]);
        g.gp0_words(&[0x6800_0000, 0]);
        let recs = draws(&decode(&cmds(&mut g)));
        assert_eq!(recs.len(), 2);
        assert_eq!(unpack_xy(recs[0][5]), (5, 5));
        assert_eq!(recs[1][0] & prim::KIND_MASK, prim::RECT);
    }

    #[test]
    fn long_lines_and_large_polygons_are_dropped() {
        let mut g = gpu();
        // x 1024 reads as -1024: 1024 wide. 512 rows tall. Both dropped.
        g.gp0_words(&[0x4000_0000, pack_xy(0, 0), pack_xy(1024, 0)]);
        g.gp0_words(&[0x4000_0000, pack_xy(0, 0), pack_xy(0, 512)]);
        g.gp0_words(&[0x4000_0000, pack_xy(0, 0), pack_xy(1023, 511)]);
        let recs = draws(&decode(&cmds(&mut g)));
        assert_eq!(recs.len(), 1);
        assert_eq!(unpack_xy(recs[0][5]), (1023, 511));
    }

    #[test]
    fn every_rect_opcode() {
        for cc in 0x60u32..=0x7F {
            let mut g = gpu();
            g.gp0(0xE100_3000 | 2 << 7 | 1); // flips, 15-bit page x 64
            let (textured, size) = (cc & 4 != 0, (cc >> 3) & 3);
            let mut words = vec![cc << 24 | 0x80_8080, pack_xy(100, 50)];
            if textured {
                words.push(0x1234_0000 | 0x2010);
            }
            if size == 0 {
                words.push(17 << 16 | 33);
            }
            g.gp0_words(&words);
            let cs = decode(&cmds(&mut g));
            let recs = draws(&cs);
            assert_eq!(recs.len(), 1, "{cc:#x}");
            let r = recs[0];
            assert_eq!(r[0] & prim::KIND_MASK, prim::RECT);
            assert_eq!(r[0] & prim::DITHER, 0, "rectangles never dither");
            let wh = [17 << 16 | 33, 1 << 16 | 1, 8 << 16 | 8, 16 << 16 | 16][size as usize];
            assert_eq!(r[4], wh);
            assert_eq!(unpack_xy(r[3]), (100, 50));
            if textured {
                assert_eq!(r[6], 0x2010);
                assert_eq!(r[0] & (prim::FLIP_X | prim::FLIP_Y), prim::FLIP_X | prim::FLIP_Y);
                assert_eq!(r[0] & prim::RAW != 0, cc & 1 != 0);
                assert_eq!((r[0] & prim::DEPTH_MASK) >> prim::DEPTH_SHIFT, 2);
                assert!(!cs.iter().any(|c| matches!(c, C::Clut(..))), "15-bit loads no colour table");
            }
        }
    }

    #[test]
    fn rect_position_wraps_to_11_bits_after_offset() {
        let mut g = gpu();
        g.gp0(0xE500_0000 | 1000 | 1000 << 11);
        g.gp0_words(&[0x6800_0000, pack_xy(100, 30)]);
        let r = draws(&decode(&cmds(&mut g)));
        assert_eq!(unpack_xy(r[0][3]), (1100 - 2048, 1030 - 2048));
    }

    #[test]
    fn clut_cache_reloads_only_on_change() {
        let mut g = gpu();
        g.gp0(0xE100_0000); // 4-bit
        let sprite = |clut: u32| [0x6500_0000u32, pack_xy(0, 0), clut << 16, 1 << 16 | 1];
        g.gp0_words(&sprite(0x40)); // load
        g.gp0_words(&sprite(0x40)); // cached
        g.gp0_words(&sprite(0x41)); // load
        g.gp0(0x0100_0000); // clear cache
        g.gp0_words(&sprite(0x41)); // load
        g.gp0(0xE100_0080); // 8-bit: same address, different depth
        g.gp0_words(&sprite(0x41)); // load
        let cs = decode(&cmds(&mut g));
        let loads: Vec<_> = cs.iter().filter_map(|c| if let C::Clut(s, x, y, n) = c { Some((*s, *x, *y, *n)) } else { None }).collect();
        assert_eq!(loads.len(), 4);
        assert_eq!(loads[0].1..=loads[0].3, 0..=16);
        assert_eq!((loads[0].2, loads[1].1), (1, 16));
        assert_eq!(loads[3].3, 256);
        let recs = draws(&cs);
        assert_eq!(recs.len(), 5);
        assert_eq!(recs[0][1] >> 16, recs[1][1] >> 16);
        assert_ne!(recs[1][1] >> 16, recs[2][1] >> 16);
    }

    #[test]
    fn fill_decodes_and_ignores_the_drawing_area() {
        let mut g = Gpu::new(); // drawing area 0..0
        g.gp0_words(&[0x0212_3456, 500 << 16 | 1023, 20 << 16 | 33]);
        let cs = decode(&cmds(&mut g));
        assert!(cs.contains(&C::Fill(render::Rect { x: 1008, y: 500, w: 48, h: 20 }, to15(0x12_3456))), "{cs:?}");
        // Zero width or height fills nothing.
        g.gp0_words(&[0x0200_0000, 0, 0]);
        assert!(decode(&cmds(&mut g)).iter().all(|c| !matches!(c, C::Fill(..))));
    }

    #[test]
    fn upload_with_mask_and_partial_abort() {
        let mut g = gpu();
        g.gp0(0xE600_0003);
        g.gp0_words(&[0xA000_0000, 10 << 16 | 1020, 2 << 16 | 3, 0x0002_0001, 0x0004_0003, 0x0006_0005]);
        let cs = decode(&cmds(&mut g));
        assert_eq!(cs, vec![C::Upload(render::Rect { x: 1020, y: 10, w: 3, h: 2 }, 3, vec![0x0002_0001, 0x0004_0003, 0x0006_0005])]);
        // Aborted after 4 of 6 pixels: one full row, then one pixel.
        g.gp0_words(&[0xA000_0000, 0, 2 << 16 | 3, 0x0002_0001, 0x0004_0003]);
        g.gp1(0x0100_0000);
        let cs = decode(&cmds(&mut g));
        assert_eq!(cs.len(), 2, "{cs:?}");
        assert_eq!(cs[0], C::Upload(render::Rect { x: 0, y: 0, w: 3, h: 1 }, 3, vec![0x0002_0001, 0x0000_0003]));
        assert_eq!(cs[1], C::Upload(render::Rect { x: 0, y: 1, w: 1, h: 1 }, 3, vec![0x0000_0004]));
        // A masked upload over a region written since the last sync reads
        // its mask bits from the read copy, so it syncs first.
        g.gp0_words(&[0xA000_0000, 0, 1 << 16 | 2, 0x0002_0001]);
        let cs = decode(&cmds(&mut g));
        assert!(matches!(&cs[0], C::Sync(r) if r.len() == 3), "{cs:?}");
    }

    #[test]
    fn readback_flow() {
        let mut g = gpu();
        g.gp0_words(&[0x6800_FFFF, 0]);
        g.gp0_words(&[0xC000_0000, 0, 1 << 16 | 3]);
        // Commands queued behind the read wait for it.
        g.gp0_words(&[0x6800_0000, pack_xy(1, 0)]);
        assert_eq!(g.pending_readback(), Some(VramRect { x: 0, y: 0, w: 3, h: 1 }));
        assert_eq!(g.status() & (1 << 27), 1 << 27);
        let first = decode(&cmds(&mut g));
        assert_eq!(draws(&first).len(), 1);
        g.finish_readback(Some(&[0x1111, 0x2222, 0x3333]));
        assert_eq!(g.read(), 0x2222_1111);
        assert_eq!(g.read(), 0x0000_3333);
        assert_eq!(g.pending_readback(), None);
        assert_eq!(draws(&decode(&cmds(&mut g))).len(), 1, "the queued rectangle ran after the read");
    }

    #[test]
    fn copies_and_overlap_pieces() {
        let mut g = gpu();
        g.gp0_words(&[0x8000_0000, 0, 100 << 16, 4 << 16 | 8]);
        let cs = decode(&cmds(&mut g));
        assert_eq!(cs, vec![C::Copy([0, 0, 0, 100, 8, 4, 0])]);
        // Down by one row: every row reads the row the previous one wrote.
        let pieces = ordered_copy(0, 0, 0, 1, 8, 3);
        assert_eq!(pieces.len(), 3);
        assert_eq!(pieces[1], CopyPiece { sx: 0, sy: 1, dx: 0, dy: 2, w: 8, h: 1 });
        // Up by one row: no row reads an earlier write; one piece.
        assert_eq!(ordered_copy(0, 1, 0, 0, 8, 3).len(), 1);
        // Right by 128 in one row: each chunk reads the previous one's
        // output.
        let pieces = ordered_copy(0, 0, 128, 0, 300, 1);
        assert_eq!(pieces.len(), 3);
        assert_eq!((pieces[0].w, pieces[1].sx, pieces[2].w), (128, 128, 44));
        // Right by 200: chunk 1 reads [128, 256), chunk 0 wrote [200, 328).
        assert_eq!(ordered_copy(0, 0, 200, 0, 256, 1).len(), 2);
        // The emitted stream syncs between dependent pieces.
        g.gp0_words(&[0x8000_0000, 0, 1 << 16, 3 << 16 | 8]);
        let cs = decode(&cmds(&mut g));
        assert_eq!(cs.iter().filter(|c| matches!(c, C::Copy(_))).count(), 3);
        assert_eq!(cs.iter().filter(|c| matches!(c, C::Sync(_))).count(), 2);
    }

    #[test]
    fn hazards_split_batches_and_sync() {
        let mut g = gpu();
        // Opaque rectangles batch together.
        g.gp0_words(&[0x6000_00FF, pack_xy(0, 0), 8 << 16 | 8]);
        g.gp0_words(&[0x6000_00FF, pack_xy(4, 4), 8 << 16 | 8]);
        // A semi-transparent one over them reads what they wrote.
        g.gp0_words(&[0x6200_00FF, pack_xy(2, 2), 8 << 16 | 8]);
        // One elsewhere doesn't.
        g.gp0_words(&[0x6200_00FF, pack_xy(100, 100), 8 << 16 | 8]);
        let cs = decode(&cmds(&mut g));
        let kinds: Vec<_> = cs
            .iter()
            .map(|c| match c {
                C::State(..) => "state",
                C::Draw(r) => if r.len() == 2 { "draw2" } else { "draw" },
                C::Sync(_) => "sync",
                _ => "other",
            })
            .collect();
        // The one elsewhere joins the first layer, before the one that
        // reads the others.
        assert_eq!(kinds, ["state", "draw", "sync", "draw"]);
        assert_eq!(draws(&cs).len(), 4);
        // Texturing from a page a rectangle just drew into syncs too.
        g.gp0(0xE100_0100); // 15-bit, page 0
        g.gp0_words(&[0x6500_0000, pack_xy(300, 0), 0, 4 << 16 | 4]);
        let cs = decode(&cmds(&mut g));
        assert!(matches!(cs[0], C::Sync(_)), "{cs:?}");
    }

    /// Semi-transparent polygons sync where they plot over each other,
    /// not where their bounding boxes meet: the two halves of a quad
    /// share a batch, a third triangle over them doesn't.
    #[test]
    fn semi_transparent_mesh_syncs_by_pixels() {
        let mut g = gpu();
        let quad = [pack_xy(10, 10), pack_xy(60, 14), pack_xy(8, 50), pack_xy(57, 61)];
        g.gp0_words(&[0x2200_00FF, quad[0], quad[1], quad[2]]);
        g.gp0_words(&[0x2200_00FF, quad[1], quad[2], quad[3]]);
        g.gp0_words(&[0x2200_00FF, pack_xy(20, 20), pack_xy(40, 20), pack_xy(30, 40)]);
        let cs = decode(&cmds(&mut g));
        let kinds: Vec<_> = cs
            .iter()
            .map(|c| match c {
                C::State(..) => "state",
                C::Draw(r) => if r.len() == 2 { "draw2" } else { "draw" },
                C::Sync(_) => "sync",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["state", "draw2", "sync", "draw"]);
    }

    /// Translucent surfaces over each other take a sync per layer, not per
    /// primitive: A, A over A, B, B over B draw as [A, B], sync, [A, B].
    #[test]
    fn translucent_overdraw_syncs_per_layer() {
        let mut g = gpu();
        let a = [pack_xy(10, 10), pack_xy(40, 10), pack_xy(10, 40)];
        let b = [pack_xy(100, 10), pack_xy(130, 10), pack_xy(100, 40)];
        for t in [a, a, b, b] {
            g.gp0_words(&[0x2200_00FF, t[0], t[1], t[2]]);
        }
        let cs = decode(&cmds(&mut g));
        let shape: Vec<_> = cs
            .iter()
            .filter_map(|c| match c {
                C::Draw(r) => Some(r.iter().map(|p| unpack_xy(p[3]).0).collect::<Vec<_>>()),
                C::Sync(_) => Some(vec![]),
                _ => None,
            })
            .collect();
        assert_eq!(shape, [vec![10, 100], vec![], vec![10, 100]]);
    }

    #[test]
    fn environment_commands_and_gp1() {
        let mut g = Gpu::new();
        g.gp0_words(&[0xE100_06AF, 0xE200_0000 | 0xF_FFFF, 0xE300_0000 | 3 << 10 | 5, 0xE400_0000 | 200 << 10 | 300]);
        g.gp0_words(&[0xE500_0000 | 1 << 11 | 2047, 0xE600_0002]);
        assert_eq!(g.status() & 0x7FF, 0x6AF);
        assert_eq!(g.status() & (3 << 11), 2 << 11);
        g.gp1(0x1000_0003);
        assert_eq!(g.read(), 3 << 10 | 5);
        g.gp1(0x1000_0004);
        assert_eq!(g.read(), 200 << 10 | 300);
        g.gp1(0x1000_0005);
        assert_eq!(g.read(), 2047 | 1 << 11);
        g.gp1(0x1000_0002);
        assert_eq!(g.read(), 0xF_FFFF);
        g.gp1(0x0300_0000);
        g.gp1(0x0500_0000 | 240 << 10 | 321);
        g.gp1(0x0600_0000 | (0x260 + 320 * 8) << 12 | 0x260);
        g.gp1(0x0700_0000 | (0x10 + 240) << 10 | 0x10);
        g.gp1(0x0800_0001);
        let d = g.present_desc();
        assert_eq!(d, PresentDesc { flags: PresentDesc::ENABLED, x: 320, y: 240, width: 320, height: 240 });
        g.gp1(0x0800_0037); // 640x480i, 24-bit
        let d = g.present_desc();
        assert_eq!(d.flags, PresentDesc::ENABLED | PresentDesc::DEPTH24 | PresentDesc::INTERLACED);
        assert_eq!(d.height, 480);
        g.gp1(0x0000_0000);
        assert_eq!(g.status() & 0x7FF, 0);
        assert_eq!(g.present_desc().flags & PresentDesc::ENABLED, 0);
    }

    #[test]
    fn interlace_line_skip_state() {
        let mut g = gpu();
        g.gp1(0x0800_0024); // 480i
        g.gp0_words(&[0x6000_00FF, pack_xy(0, 0), 8 << 16 | 8]);
        g.set_field(true);
        g.gp0_words(&[0x6000_00FF, pack_xy(0, 0), 8 << 16 | 8]);
        g.gp0(0xE100_0400); // draw to display: no skip
        g.gp0_words(&[0x6000_00FF, pack_xy(0, 0), 8 << 16 | 8]);
        let flags: Vec<u32> = decode(&cmds(&mut g)).iter().filter_map(|c| if let C::State(_, f) = c { Some(*f) } else { None }).collect();
        assert_eq!(flags, [render::state::SKIP_EVEN, render::state::SKIP_ODD, 0]);
    }

    #[test]
    fn quad_aborted_after_first_triangle_draws_it() {
        let mut g = gpu();
        g.gp0_words(&[0x2800_00FF, pack_xy(0, 0), pack_xy(10, 0), pack_xy(0, 10)]);
        g.gp1(0x0100_0000);
        let r = draws(&decode(&cmds(&mut g)));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0][0] & prim::KIND_MASK, prim::TRIANGLE);
    }

    #[test]
    fn recordable_state_round_trips() {
        let mut g = Gpu::new();
        let words = [0xE100_37AF, 0xE20A_BCDE, 0xE300_0000 | 7 << 10 | 3, 0xE400_0000 | 239 << 10 | 319, 0xE500_0000 | 0x7FF << 11 | 5, 0xE600_0003];
        g.gp0_words(&words);
        assert_eq!(g.recordable_state(), Some(words));
        // Mid-command (a quad waiting for its fourth vertex): no state.
        g.gp0_words(&[0x2800_00FF, 0, 1, 2]);
        assert_eq!(g.recordable_state(), None);
    }
}
