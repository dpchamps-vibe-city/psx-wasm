//! GPU vectors from a game run (M4 task 10). The runtime offers each
//! GPU linked-list transfer (a `DrawOTag`) to the native host, which keeps
//! VRAM as it was before it, the GP0 state words it started from and its
//! nodes (`psxw_host_native::GpuBatch`). [`draw_vector`] turns one into a
//! GPU_DRAW vector in list mode: the whole of VRAM as its initial state,
//! the state words, one packet per node, and as readback every drawing area
//! the batch draws in plus whatever its fills, copies and uploads write.
//!
//! A batch the device target can't replay as the game ran it is skipped,
//! with the reason: a node that doesn't hold whole commands, a command over
//! 16 words (the target's linked-list DMA would overflow the GPU's FIFO,
//! M4 Findings), a VRAM→CPU read, or a command list over one exchange.

use anyhow::Result;
use psx_devices::gpu::command_len;
use psxw_oracle::record::{Recorder, vram_rect};
use psxw_oracle::script;
use psxw_oracle::store::Store;
use psxw_oracle::vector::{GpuDraw, GpuMode, Rect, Vector, VectorId};
use psxw_oracle::wire;

/// What became of a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Recorded(VectorId),
    /// Already recorded (or over the recorder's budget).
    Declined,
    Skipped(String),
}

/// VRAM rectangles covering `(x, y, w, h)` as it wraps at 1024×512.
fn wrapped(x: u32, y: u32, w: u32, h: u32) -> Vec<Rect> {
    let (x, y) = (x & 1023, y & 511);
    let (w, h) = (w.min(1024), h.min(512));
    let mut out = Vec::new();
    for (rx, rw) in [(x, w.min(1024 - x)), (0, w.saturating_sub(1024 - x))] {
        for (ry, rh) in [(y, h.min(512 - y)), (0, h.saturating_sub(512 - y))] {
            if rw > 0 && rh > 0 {
                out.push(Rect::new(rx as u16, ry as u16, rw as u16, rh as u16));
            }
        }
    }
    out
}

/// The commands in one node, as (opcode, length); Err when the node
/// doesn't end on a command boundary.
fn commands(node: &[u32]) -> Result<Vec<(u8, usize)>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < node.len() {
        let cc = (node[i] >> 24) as u8;
        let n = match cc {
            // Polylines run to the terminator word.
            0x48..=0x4F | 0x58..=0x5F => {
                let step = if cc & 0x10 != 0 { 2 } else { 1 };
                let mut j = i + 1 + step;
                while j < node.len() && node[j] & 0xF000_F000 != 0x5000_5000 {
                    j += step;
                }
                if j >= node.len() {
                    return Err(format!("a polyline at word {i} runs past its node"));
                }
                j + 1 - i
            }
            0xA0..=0xBF if i + 2 < node.len() => {
                let wh = node[i + 2];
                let (w, h) = (((wh & 0xFFFF).wrapping_sub(1) & 0x3FF) + 1, ((wh >> 16).wrapping_sub(1) & 0x1FF) + 1);
                3 + (w * h).div_ceil(2) as usize
            }
            _ => command_len(cc),
        };
        if i + n > node.len() {
            return Err(format!("command {cc:02X}h at word {i} runs past its node"));
        }
        out.push((cc, n));
        i += n;
    }
    Ok(out)
}

/// The rectangles a batch can write, from the state words and the nodes.
fn written(state: &[u32], nodes: &[Vec<u32>]) -> Result<Vec<Rect>, String> {
    let mut area = [0u32; 4];
    for &w in state {
        match w >> 24 {
            0xE3 => area[..2].copy_from_slice(&[w & 1023, (w >> 10) & 1023]),
            0xE4 => area[2..].copy_from_slice(&[w & 1023, (w >> 10) & 1023]),
            _ => {}
        }
    }
    let mut out: Vec<Rect> = Vec::new();
    let mut add = |rs: Vec<Rect>| {
        for r in rs {
            if !out.contains(&r) {
                out.push(r);
            }
        }
    };
    for node in nodes {
        let mut i = 0;
        for (cc, n) in commands(node)? {
            if n > wire::GPU_PACKET_MAX_WORDS as usize {
                return Err(format!("command {cc:02X}h of {n} words"));
            }
            let c = &node[i..i + n];
            match cc {
                0xE3 => area[..2].copy_from_slice(&[c[0] & 1023, (c[0] >> 10) & 1023]),
                0xE4 => area[2..].copy_from_slice(&[c[0] & 1023, (c[0] >> 10) & 1023]),
                0x02 => {
                    let (x, y) = (c[1] & 0x3F0, (c[1] >> 16) & 0x1FF);
                    let (w, h) = (((c[2] & 0x3FF) + 0xF) & !0xF, (c[2] >> 16) & 0x1FF);
                    add(wrapped(x, y, w, h));
                }
                0x20..=0x7F => {
                    let [x0, y0, x1, y1] = area;
                    if x1 >= x0 && y1 >= y0 {
                        add(wrapped(x0, y0, x1 - x0 + 1, (y1 - y0 + 1).min(512)));
                    }
                }
                0x80..=0x9F => {
                    let (w, h) = (((c[3] & 0xFFFF).wrapping_sub(1) & 0x3FF) + 1, ((c[3] >> 16).wrapping_sub(1) & 0x1FF) + 1);
                    add(wrapped(c[2] & 0x3FF, (c[2] >> 16) & 0x1FF, w, h));
                }
                0xA0..=0xBF => {
                    let (w, h) = (((c[2] & 0xFFFF).wrapping_sub(1) & 0x3FF) + 1, ((c[2] >> 16).wrapping_sub(1) & 0x1FF) + 1);
                    add(wrapped(c[1] & 0x3FF, (c[1] >> 16) & 0x1FF, w, h));
                }
                0xC0..=0xDF => return Err("a VRAM→CPU read in the list".into()),
                _ => {}
            }
            i += n;
        }
    }
    Ok(out)
}

/// Record one batch as a GPU_DRAW vector.
pub fn draw_vector(rec: &mut dyn Recorder, store: &Store, vram: &[u16], state: &[u32], nodes: &[Vec<u32>]) -> Result<Outcome> {
    let readback = match written(state, nodes) {
        Ok(r) if r.is_empty() => return Ok(Outcome::Skipped("draws nothing".into())),
        Ok(r) => r,
        Err(why) => return Ok(Outcome::Skipped(why)),
    };
    let init = vec![vram_rect(rec, vram, Rect::new(0, 0, 1024, 512))?];
    let v = Vector::GpuDraw(GpuDraw { init, state: state.to_vec(), mode: GpuMode::List, packets: nodes.to_vec(), readback });
    if let Err(e) = script::compile(&v, store) {
        return Ok(Outcome::Skipped(format!("{e:#}")));
    }
    Ok(match rec.record(v)? {
        Some(id) => Outcome::Recorded(id),
        None => Outcome::Declined,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping_rects() {
        assert_eq!(wrapped(10, 20, 30, 40), vec![Rect::new(10, 20, 30, 40)]);
        assert_eq!(
            wrapped(1000, 500, 48, 20),
            vec![Rect::new(1000, 500, 24, 12), Rect::new(1000, 0, 24, 8), Rect::new(0, 500, 24, 12), Rect::new(0, 0, 24, 8)]
        );
    }

    #[test]
    fn areas_fills_and_long_commands() {
        let state = [0xE300_0000 | 5 << 10, 0xE400_0000 | 100 << 10 | 319];
        // A flat triangle in the area, then a fill at (16, 300) 20×4.
        let tri = vec![0x2000_00FF, 0, 1 << 16, 1];
        let fill = vec![0x0200_0000, 300 << 16 | 16, 4 << 16 | 20];
        let r = written(&state, &[tri, fill]).unwrap();
        assert_eq!(r, vec![Rect::new(0, 5, 320, 96), Rect::new(16, 300, 32, 4)]);
        // A CPU→VRAM upload of 8×8 pixels is 35 words: too long for a node.
        let mut up = vec![0xA000_0000, 0, 8 << 16 | 8];
        up.resize(3 + 32, 0);
        assert!(written(&state, &[up]).unwrap_err().contains("35 words"));
        // A node that stops mid-command.
        assert!(written(&state, &[vec![0x2000_00FF, 0]]).is_err());
    }
}
