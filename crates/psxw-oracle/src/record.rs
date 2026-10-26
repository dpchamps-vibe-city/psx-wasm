//! The recorder: what psx-wasm's devices call, during real game runs, to
//! emit vectors (design: "The vector recorder"). The per-device hooks land
//! with each device (GPU and GTE in M4, SPU in M6); this module is the
//! interface they call and the store-backed implementation.
//!
//! A hook builds a [`Vector`] from the device's own vocabulary (for the GPU:
//! the VRAM it is about to draw over, the draw state words, the GP0 packets
//! and the rectangles it will touch) and hands it to a [`Recorder`]. Large
//! state goes through [`Recorder::blob`] first, so a VRAM image shared by
//! many vectors is stored once.

use crate::hash::Hash;
use crate::store::Store;
use crate::vector::{BlobRef, Kind, Rect, Vector, VectorId, VramRect, pixels_to_bytes};
use crate::wire;
use anyhow::{Result, ensure};
use std::collections::{BTreeMap, BTreeSet};

pub trait Recorder {
    /// Store large state once; returns its reference.
    fn blob(&mut self, bytes: &[u8]) -> Result<BlobRef>;
    /// Record a vector. Returns its id, or None when the recorder declined
    /// it (already recorded, or over its per-kind budget).
    fn record(&mut self, v: Vector) -> Result<Option<VectorId>>;
}

/// The set name a game's recorded vectors go to.
pub fn game_set(game: &str) -> String {
    format!("game-{game}")
}

/// Snapshot a VRAM rectangle (from a 1024×512 u16 image) into a blob.
pub fn vram_rect(rec: &mut dyn Recorder, vram: &[u16], rect: Rect) -> Result<VramRect> {
    ensure!(vram.len() == (wire::VRAM_WIDTH * wire::VRAM_HEIGHT) as usize, "VRAM image of {} pixels", vram.len());
    rect.check()?;
    let mut px = Vec::with_capacity(rect.pixels());
    for y in rect.y..rect.y + rect.h {
        let row = y as usize * wire::VRAM_WIDTH as usize;
        px.extend_from_slice(&vram[row + rect.x as usize..row + (rect.x + rect.w) as usize]);
    }
    Ok(VramRect { rect, pixels: rec.blob(&pixels_to_bytes(&px))? })
}

/// Records into the oracle store under a named set (normally
/// `game-<id>`), deduplicating, with an optional budget per kind so a long
/// run does not record millions of near-identical draws.
pub struct StoreRecorder {
    store: Store,
    set: String,
    seen: BTreeSet<VectorId>,
    new: Vec<VectorId>,
    per_kind: BTreeMap<Kind, usize>,
    limit: Option<usize>,
}

impl StoreRecorder {
    pub fn new(store: Store, set: &str) -> Result<Self> {
        let seen = store.set(set)?;
        Ok(StoreRecorder { store, set: set.to_string(), seen, new: Vec::new(), per_kind: BTreeMap::new(), limit: None })
    }

    /// Record at most `n` new vectors of each kind.
    pub fn with_limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// Vectors recorded since creation (not counting duplicates).
    pub fn recorded(&self) -> &[VectorId] {
        &self.new
    }

    /// Add the recorded vectors to the set file.
    pub fn flush(&mut self) -> Result<()> {
        self.store.add_to_set(&self.set, self.new.iter().copied())
    }
}

impl Recorder for StoreRecorder {
    fn blob(&mut self, bytes: &[u8]) -> Result<BlobRef> {
        self.store.put_blob(bytes)
    }

    fn record(&mut self, v: Vector) -> Result<Option<VectorId>> {
        let id = v.id();
        if self.seen.contains(&id) {
            return Ok(None);
        }
        let n = self.per_kind.entry(v.kind()).or_default();
        if self.limit.is_some_and(|l| *n >= l) {
            return Ok(None);
        }
        self.store.put_vector(&v)?;
        *n += 1;
        self.seen.insert(id);
        self.new.push(id);
        Ok(Some(id))
    }
}

/// Keeps everything in memory: for tests, and for comparing a device with
/// itself without touching `.oracle/`.
#[derive(Default)]
pub struct MemoryRecorder {
    pub blobs: BTreeMap<Hash, Vec<u8>>,
    pub vectors: Vec<Vector>,
}

impl Recorder for MemoryRecorder {
    fn blob(&mut self, bytes: &[u8]) -> Result<BlobRef> {
        let h = Hash::of(bytes);
        self.blobs.entry(h).or_insert_with(|| bytes.to_vec());
        Ok(BlobRef(h))
    }

    fn record(&mut self, v: Vector) -> Result<Option<VectorId>> {
        v.check()?;
        let id = v.id();
        if self.vectors.iter().any(|x| x.id() == id) {
            return Ok(None);
        }
        self.vectors.push(v);
        Ok(Some(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::{GpuDraw, GpuMode};

    #[test]
    fn store_recorder_dedups_and_limits() {
        let dir = std::env::temp_dir().join(format!("psxw-oracle-rec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::new(&dir);
        let mut rec = StoreRecorder::new(store.clone(), &game_set("test")).unwrap().with_limit(2);
        let vram = vec![0x1234u16; 1024 * 512];
        let init = vram_rect(&mut rec, &vram, Rect::new(0, 0, 8, 8)).unwrap();
        let draw = |c: u32| {
            Vector::GpuDraw(GpuDraw {
                init: vec![init.clone()],
                state: vec![],
                mode: GpuMode::List,
                packets: vec![vec![0x6000_0000 | c, 0, 0x0004_0004]],
                readback: vec![Rect::new(0, 0, 8, 8)],
            })
        };
        assert!(rec.record(draw(1)).unwrap().is_some());
        assert!(rec.record(draw(1)).unwrap().is_none());
        assert!(rec.record(draw(2)).unwrap().is_some());
        assert!(rec.record(draw(3)).unwrap().is_none());
        rec.flush().unwrap();
        assert_eq!(store.set("game-test").unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
