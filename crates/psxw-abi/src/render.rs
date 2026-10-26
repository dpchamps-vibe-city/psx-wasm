//! The renderer command interface (M4; docs/design.md "Runtime", gpu;
//! decision D20): what the GPU front end in the runtime
//! (`psx_devices::gpu`) hands the backend in the host (`psx-render-gl`).
//!
//! The front end resolves every piece of PS1 state into explicit
//! per-primitive attributes, so the backend is a pure function from a
//! command stream and the VRAM state to a new VRAM state. That makes the
//! backend checkable on its own: `psxw oracle check --device gpu` replays
//! a vector through front end plus backend and compares VRAM with the
//! golden.
//!
//! # Encoding
//!
//! A command stream is a sequence of little-endian `u32` words. Each
//! command is a header word, `op | payload_words << 8` ([`header`]),
//! followed by its payload. Rectangles wrap like VRAM does: pixel `(i, j)`
//! of a rectangle at `(x, y)` is `((x + i) & 1023, (y + j) & 511)`.
//!
//! | op | payload |
//! |---|---|
//! | [`op::SET_STATE`] | clip top-left `x | y << 16`, clip bottom-right (inclusive), flags ([`state`]) |
//! | [`op::DRAW`] | count, then `count` primitive records of [`PRIM_WORDS`] words ([`prim`]) |
//! | [`op::FILL`] | `x | y << 16`, `w | h << 16`, 15-bit colour |
//! | [`op::UPLOAD`] | `x | y << 16`, `w | h << 16`, mask flags ([`mask`]), then `w*h` pixels two per word, low half first |
//! | [`op::COPY`] | source `x | y << 16`, destination `x | y << 16`, `w | h << 16`, mask flags |
//! | [`op::READBACK`] | `x | y << 16`, `w | h << 16` |
//! | [`op::PRESENT`] | a [`PresentDesc`] as [`PRESENT_WORDS`] words |
//! | [`op::SYNC`] | count, then `count` rectangles as `x | y << 16`, `w | h << 16` |
//! | [`op::CLUT_LOAD`] | slot, `x | y << 16`, entry count (16 or 256) |
//!
//! `w` and `h` are 1..=1024 and 1..=512; 1024 and 512 are encoded as is
//! (they need 11 and 10 bits).
//!
//! # Semantics
//!
//! The backend keeps two copies of VRAM: the *draw* copy every command
//! writes, and a *read* copy that textures, colour tables, blending, mask
//! checks and copies read from. Only [`op::SYNC`] brings regions of the
//! read copy up to date. The front end inserts a SYNC before any command
//! or primitive that reads a region written since the last SYNC, and ends
//! the current DRAW batch there; so within one DRAW every read sees VRAM
//! as it was before the batch, which is what makes a batch one GPU draw.
//! Primitives in a batch are applied in order.
//!
//! - **SET_STATE** sets the drawing area (clip rectangle) and the
//!   interlace line skip for the DRAW and FILL commands that follow.
//! - **DRAW** rasterizes primitives exactly as the PS1 GPU does at 1×:
//!   coverage, Gouraud and texture-coordinate interpolation, texture
//!   window, 4/8/15-bit texels through the colour-table cache, modulation,
//!   semi-transparency, dither and the mask bit, all from the record's
//!   explicit attributes.
//! - **FILL** writes the colour (mask bit clear) to every pixel of the
//!   rectangle, ignoring the drawing area and mask settings; rows the
//!   line skip excludes are left alone.
//! - **UPLOAD** writes the pixels, ORing in the mask bit when
//!   [`mask::SET`], and skipping pixels whose destination has bit 15 set
//!   when [`mask::CHECK`].
//! - **COPY** copies a rectangle as if all source pixels were read before
//!   any is written, with the same mask rules. The front end splits the
//!   GPU's ordered copies of overlapping rectangles into such pieces.
//! - **READBACK** appends the rectangle's pixels (row-major `u16`) to the
//!   backend's readback output, after everything before it. In a running
//!   game the runtime reads VRAM through the synchronous host import
//!   [`imports::RENDER_READBACK`] instead.
//! - **PRESENT** shows the display area a [`PresentDesc`] describes.
//! - **SYNC** copies the rectangles from the draw copy to the read copy.
//! - **CLUT_LOAD** copies a colour table (16 or 256 entries along a row)
//!   from the read copy into a colour-table slot, which later primitives
//!   name. This is the GPU's colour-table cache: a primitive keeps using
//!   the table as it was loaded, even if VRAM changes later, until the
//!   front end loads the slot again.

/// Bumped on any change to the encoding or semantics of this module.
/// Part of [`ABI_VERSION`](crate::ABI_VERSION).
pub const RENDER_VERSION: u32 = 1;

pub const VRAM_WIDTH: u32 = 1024;
pub const VRAM_HEIGHT: u32 = 512;

/// Command opcodes (the low byte of a header word).
pub mod op {
    pub const SET_STATE: u8 = 0x01;
    pub const DRAW: u8 = 0x02;
    pub const FILL: u8 = 0x03;
    pub const UPLOAD: u8 = 0x04;
    pub const COPY: u8 = 0x05;
    pub const READBACK: u8 = 0x06;
    pub const PRESENT: u8 = 0x07;
    pub const SYNC: u8 = 0x08;
    pub const CLUT_LOAD: u8 = 0x09;
}

/// SET_STATE flags (third payload word).
pub mod state {
    /// Bits 0–1: interlace line skip. 0 = draw every row.
    pub const SKIP_MASK: u32 = 3;
    /// Skip rows whose y is even.
    pub const SKIP_EVEN: u32 = 2;
    /// Skip rows whose y is odd.
    pub const SKIP_ODD: u32 = 3;
}

/// UPLOAD and COPY mask flags.
pub mod mask {
    /// OR bit 15 into every pixel written.
    pub const SET: u32 = 1;
    /// Leave pixels whose destination has bit 15 set.
    pub const CHECK: u32 = 2;
}

/// Words per primitive record in a DRAW batch.
pub const PRIM_WORDS: usize = 16;

/// Colour-table slots the backend provides; slot numbers are below this.
pub const CLUT_SLOTS: u32 = 1024;

/// Primitive records. Word 0 holds the kind and flags, word 1 the texture
/// page and colour-table slot, word 2 the texture window; the rest depend
/// on the kind. Coordinates are drawing-offset-applied PS1 coordinates
/// (signed 16-bit), colours `r | g << 8 | b << 16`, texture coordinates
/// `u | v << 8`.
///
/// | kind | words 3.. |
/// |---|---|
/// | [`TRIANGLE`](prim::TRIANGLE) | 3 vertices: `x | y << 16`, colour, `u | v << 8` |
/// | [`QUAD`](prim::QUAD) | 4 vertices as for a triangle; drawn as triangles 0-1-2 then 1-2-3 |
/// | [`RECT`](prim::RECT) | `x | y << 16`, `w | h << 16`, colour, `u | v << 8` |
/// | [`LINE`](prim::LINE) | `x0 | y0 << 16`, colour 0, `x1 | y1 << 16`, colour 1 |
///
/// Flat primitives carry the same colour in every vertex. A rectangle's
/// position is already sign-extended to 11 bits after the offset (as the
/// GPU does for rectangles); polygon and line vertices are not.
pub mod prim {
    pub const KIND_MASK: u32 = 7;
    pub const TRIANGLE: u32 = 1;
    pub const QUAD: u32 = 2;
    pub const RECT: u32 = 3;
    pub const LINE: u32 = 4;

    /// Sampled from the texture page.
    pub const TEXTURED: u32 = 1 << 3;
    /// Texels are used as is ("raw texture"), not modulated by the colour.
    pub const RAW: u32 = 1 << 4;
    /// Colours are interpolated across the primitive.
    pub const GOURAUD: u32 = 1 << 5;
    /// Semi-transparent, with the blend mode in [`ABR_SHIFT`].
    pub const SEMI: u32 = 1 << 6;
    pub const ABR_SHIFT: u32 = 7;
    pub const ABR_MASK: u32 = 3 << ABR_SHIFT;
    /// Apply the 4×4 dither before truncating to 5 bits per channel.
    pub const DITHER: u32 = 1 << 9;
    /// OR bit 15 into every pixel written.
    pub const MASK_SET: u32 = 1 << 10;
    /// Leave pixels whose destination has bit 15 set.
    pub const MASK_CHECK: u32 = 1 << 11;
    /// Texel depth: 0 = 4-bit, 1 = 8-bit, 2 = 15-bit.
    pub const DEPTH_SHIFT: u32 = 12;
    pub const DEPTH_MASK: u32 = 3 << DEPTH_SHIFT;
    /// Rectangles: texture coordinates decrease along x (GP0(E1h) bit 12).
    pub const FLIP_X: u32 = 1 << 14;
    /// Rectangles: texture coordinates decrease along y (GP0(E1h) bit 13).
    pub const FLIP_Y: u32 = 1 << 15;
    /// The primitive samples texels where it also draws. The GPU draws
    /// row by row (polygons in its own row order, rectangles top to
    /// bottom), so a row samples what the rows before it drew; the
    /// backend must too, instead of reading VRAM as it was before the
    /// batch.
    pub const SERIAL: u32 = 1 << 16;

    /// Word 1: texture page x in units of 64 pixels (bits 0–3), y in
    /// units of 256 rows (bit 4), colour-table slot in bits 16–31.
    pub const fn texture_word(page_x64: u32, page_y256: u32, clut_slot: u32) -> u32 {
        (page_x64 & 0xF) | (page_y256 & 1) << 4 | clut_slot << 16
    }
}

/// `x | y << 16` for signed 16-bit coordinates.
#[inline]
pub const fn pack_xy(x: i32, y: i32) -> u32 {
    (x as u32 & 0xFFFF) | (y as u32) << 16
}

/// The inverse of [`pack_xy`].
#[inline]
pub const fn unpack_xy(w: u32) -> (i32, i32) {
    (w as u16 as i16 as i32, (w >> 16) as u16 as i16 as i32)
}

/// A command header word.
#[inline]
pub const fn header(op: u8, payload_words: u32) -> u32 {
    op as u32 | payload_words << 8
}

/// What a frame shows: the display area of VRAM, as GP1 configured it.
/// `present(desc_ptr)` reads it from memory as [`PRESENT_WORDS`]
/// little-endian `u32`s, in field order.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PresentDesc {
    /// [`PresentDesc::ENABLED`], [`PresentDesc::DEPTH24`],
    /// [`PresentDesc::INTERLACED`], [`PresentDesc::PAL`].
    pub flags: u32,
    /// Top-left of the display area in VRAM (GP1(05h)).
    pub x: u32,
    pub y: u32,
    /// Visible picture in display pixels (24-bit pixels when DEPTH24):
    /// from the horizontal range and dot clock (GP1(06h), GP1(08h)), and
    /// the vertical range, doubled when interlaced (GP1(07h)).
    pub width: u32,
    pub height: u32,
}

impl PresentDesc {
    /// Display enabled (GP1(03h) bit 0 clear).
    pub const ENABLED: u32 = 1;
    /// 24-bit colour (GP1(08h) bit 4).
    pub const DEPTH24: u32 = 2;
    /// 480-line interlace (GP1(08h) bits 2 and 5).
    pub const INTERLACED: u32 = 4;
    /// PAL timing (GP1(08h) bit 3).
    pub const PAL: u32 = 8;

    pub const fn to_words(&self) -> [u32; PRESENT_WORDS] {
        [self.flags, self.x, self.y, self.width, self.height]
    }

    pub const fn from_words(w: &[u32; PRESENT_WORDS]) -> Self {
        PresentDesc { flags: w[0], x: w[1], y: w[2], width: w[3], height: w[4] }
    }
}

pub const PRESENT_WORDS: usize = 5;

/// The host imports the renderer interface adds to module
/// [`host`](crate::host).
pub mod imports {
    /// `(ptr, len) -> ()`: execute `len` bytes (a multiple of 4) of
    /// encoded commands at `ptr`.
    pub const RENDER_SUBMIT: &str = "render_submit";
    /// `(x, y, w, h, dst) -> ()`: after everything submitted so far,
    /// write the VRAM rectangle (wrapping) to `dst` as `w * h`
    /// little-endian `u16`s, row-major. Synchronous.
    pub const RENDER_READBACK: &str = "render_readback";
    /// `(desc_ptr) -> ()`: show a frame described by a
    /// [`PresentDesc`](super::PresentDesc) at `desc_ptr`.
    pub const PRESENT: &str = "present";
    /// `() -> i32`: a GPU linked-list transfer is about to start, and
    /// everything before it has been submitted. Nonzero: the host records
    /// this batch (and takes VRAM as it is now); the runtime then calls
    /// [`GPU_RECORD`] when the transfer ends. Only called when the run
    /// records ([`BootOptions::TRACE_RECORD_GPU`](crate::BootOptions)).
    pub const GPU_RECORD_BEGIN: &str = "gpu_record_begin";
    /// `(ptr, len) -> ()`: the batch, `len` bytes of little-endian `u32`s:
    /// the count of state words, the GP0 state words (E1h-E6h) in force
    /// when it started, then each linked-list node as its word count and
    /// words.
    pub const GPU_RECORD: &str = "gpu_record";
}

/// A VRAM rectangle in a command; `w` and `h` are 1..=1024 and 1..=512.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn words(&self) -> [u32; 2] {
        [(self.x & 0x3FF) | (self.y & 0x1FF) << 16, self.w | self.h << 16]
    }

    pub const fn from_words(a: u32, b: u32) -> Self {
        Rect { x: a & 0x3FF, y: (a >> 16) & 0x1FF, w: b & 0xFFFF, h: b >> 16 }
    }

    /// Size within limits.
    pub const fn valid(&self) -> bool {
        self.w >= 1 && self.w <= VRAM_WIDTH && self.h >= 1 && self.h <= VRAM_HEIGHT
    }

    pub const fn pixels(&self) -> usize {
        self.w as usize * self.h as usize
    }
}

/// A decoded command, borrowing its payload from the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command<'a> {
    SetState { clip: [u32; 4], flags: u32 },
    /// Primitive records, [`PRIM_WORDS`] each.
    Draw(&'a [u32]),
    Fill { rect: Rect, color: u32 },
    /// Pixels two per word, low half first.
    Upload { rect: Rect, mask: u32, pixels: &'a [u32] },
    Copy { src_x: u32, src_y: u32, dst_x: u32, dst_y: u32, w: u32, h: u32, mask: u32 },
    Readback(Rect),
    Present(PresentDesc),
    /// Rectangles as word pairs.
    Sync(&'a [u32]),
    ClutLoad { slot: u32, x: u32, y: u32, count: u32 },
}

/// Why a stream did not decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The command at this word index runs past the end of the stream.
    Truncated(usize),
    /// Unknown opcode at this word index.
    UnknownOp(usize, u8),
    /// A payload of the wrong size or with out-of-range fields.
    Malformed(usize, u8),
}

/// Iterates over the commands of a stream.
pub struct Commands<'a> {
    words: &'a [u32],
    pos: usize,
}

/// Decode a command stream.
pub fn commands(words: &[u32]) -> Commands<'_> {
    Commands { words, pos: 0 }
}

impl<'a> Iterator for Commands<'a> {
    type Item = Result<Command<'a>, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.words.len() {
            return None;
        }
        let at = self.pos;
        let h = self.words[at];
        let (code, n) = ((h & 0xFF) as u8, (h >> 8) as usize);
        if self.words.len() - at - 1 < n {
            self.pos = self.words.len();
            return Some(Err(DecodeError::Truncated(at)));
        }
        let p = &self.words[at + 1..at + 1 + n];
        self.pos = at + 1 + n;
        let bad = || Err(DecodeError::Malformed(at, code));
        let rect = |a: u32, b: u32| {
            let r = Rect::from_words(a, b);
            if r.valid() { Ok(r) } else { Err(DecodeError::Malformed(at, code)) }
        };
        Some(match code {
            op::SET_STATE => match p {
                [a, b, f] => Ok(Command::SetState { clip: [a & 0x3FF, (a >> 16) & 0x3FF, b & 0x3FF, (b >> 16) & 0x3FF], flags: *f }),
                _ => bad(),
            },
            op::DRAW => match p.split_first() {
                Some((&count, recs)) if recs.len() == count as usize * PRIM_WORDS => Ok(Command::Draw(recs)),
                _ => bad(),
            },
            op::FILL => match p {
                [a, b, c] => rect(*a, *b).map(|r| Command::Fill { rect: r, color: *c & 0x7FFF }),
                _ => bad(),
            },
            op::UPLOAD => match p {
                [a, b, m, px @ ..] => match rect(*a, *b) {
                    Ok(r) if px.len() == r.pixels().div_ceil(2) => Ok(Command::Upload { rect: r, mask: *m & 3, pixels: px }),
                    Ok(_) => bad(),
                    Err(e) => Err(e),
                },
                _ => bad(),
            },
            op::COPY => match p {
                [s, d, wh, m] => rect(*s, *wh).map(|r| Command::Copy {
                    src_x: r.x,
                    src_y: r.y,
                    dst_x: d & 0x3FF,
                    dst_y: (d >> 16) & 0x1FF,
                    w: r.w,
                    h: r.h,
                    mask: *m & 3,
                }),
                _ => bad(),
            },
            op::READBACK => match p {
                [a, b] => rect(*a, *b).map(Command::Readback),
                _ => bad(),
            },
            op::PRESENT => match <&[u32; PRESENT_WORDS]>::try_from(p) {
                Ok(w) => Ok(Command::Present(PresentDesc::from_words(w))),
                Err(_) => bad(),
            },
            op::SYNC => match p.split_first() {
                Some((&count, rects)) if rects.len() == 2 * count as usize => {
                    if rects.chunks_exact(2).all(|c| Rect::from_words(c[0], c[1]).valid()) {
                        Ok(Command::Sync(rects))
                    } else {
                        bad()
                    }
                }
                _ => bad(),
            },
            op::CLUT_LOAD => match p {
                [slot, xy, count] if *slot < CLUT_SLOTS && (*count == 16 || *count == 256) => {
                    Ok(Command::ClutLoad { slot: *slot, x: xy & 0x3FF, y: (xy >> 16) & 0x1FF, count: *count })
                }
                _ => bad(),
            },
            other => Err(DecodeError::UnknownOp(at, other)),
        })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    #[test]
    fn xy_round_trip() {
        for (x, y) in [(0, 0), (-1, 5), (-2048, 2046), (1023, -1024)] {
            assert_eq!(unpack_xy(pack_xy(x, y)), (x, y));
        }
    }

    #[test]
    fn decodes_every_command() {
        let r = Rect { x: 1020, y: 510, w: 8, h: 3 };
        let mut s = [0u32; 64];
        let mut n = 0;
        let mut put = |w: &[u32]| {
            s[n..n + w.len()].copy_from_slice(w);
            n += w.len();
        };
        put(&[header(op::SET_STATE, 3), 0, 511 << 16 | 1023, state::SKIP_ODD]);
        let mut rec = [0u32; PRIM_WORDS];
        rec[0] = prim::RECT;
        put(&[header(op::DRAW, 1 + PRIM_WORDS as u32), 1]);
        put(&rec);
        put(&[header(op::FILL, 3), r.words()[0], r.words()[1], 0x7FFF]);
        put(&[header(op::UPLOAD, 3 + 12), r.words()[0], r.words()[1], mask::SET]);
        put(&[7; 12]);
        put(&[header(op::COPY, 4), 1, 2 << 16 | 3, 1 << 16 | 1024, mask::CHECK]);
        put(&[header(op::READBACK, 2), r.words()[0], r.words()[1]]);
        put(&[header(op::PRESENT, PRESENT_WORDS as u32), 1, 0, 0, 320, 240]);
        put(&[header(op::SYNC, 3), 1, r.words()[0], r.words()[1]]);
        put(&[header(op::CLUT_LOAD, 3), 5, 7 << 16 | 32, 256]);
        let got: Vec<_> = commands(&s[..n]).collect::<Result<_, _>>().unwrap();
        assert_eq!(got.len(), 9);
        assert!(matches!(got[0], Command::SetState { clip: [0, 0, 1023, 511], flags: 3 }));
        assert!(matches!(got[1], Command::Draw(recs) if recs.len() == PRIM_WORDS));
        assert_eq!(got[2], Command::Fill { rect: r, color: 0x7FFF });
        assert!(matches!(got[3], Command::Upload { rect, mask: 1, pixels } if rect == r && pixels.len() == 12));
        assert!(matches!(got[4], Command::Copy { src_x: 1, src_y: 0, dst_x: 3, dst_y: 2, w: 1024, h: 1, mask: 2 }));
        assert_eq!(got[5], Command::Readback(r));
        assert_eq!(got[6], Command::Present(PresentDesc { flags: 1, x: 0, y: 0, width: 320, height: 240 }));
        assert!(matches!(got[7], Command::Sync(rs) if rs.len() == 2));
        assert_eq!(got[8], Command::ClutLoad { slot: 5, x: 32, y: 7, count: 256 });
    }

    #[test]
    fn rejects_bad_streams() {
        assert_eq!(commands(&[header(op::FILL, 3), 0]).next(), Some(Err(DecodeError::Truncated(0))));
        assert_eq!(commands(&[0xEE]).next(), Some(Err(DecodeError::UnknownOp(0, 0xEE))));
        // w = 0 is not a valid rectangle.
        assert_eq!(commands(&[header(op::READBACK, 2), 0, 1 << 16]).next(), Some(Err(DecodeError::Malformed(0, op::READBACK))));
        assert_eq!(commands(&[header(op::DRAW, 2), 1, 0]).next(), Some(Err(DecodeError::Malformed(0, op::DRAW))));
    }
}
