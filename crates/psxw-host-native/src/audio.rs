//! The native host's audio sink (M6): what `host.audio` delivers, hashed
//! for determinism checks and optionally written as a WAV file (16-bit
//! stereo at `psxw_abi::AUDIO_HZ`), streamed so a long run needs no
//! buffer.

use serde::Serialize;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// What a run's audio amounted to.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct AudioStats {
    pub frames: u64,
    /// Frames with either channel nonzero.
    pub audible_frames: u64,
    /// FNV-1a (64-bit, hex) of every frame's bytes (left, right, i16
    /// little-endian), as the browser host computes it.
    pub hash: String,
    pub wav: Option<PathBuf>,
}

pub struct Sink {
    frames: u64,
    audible: u64,
    hash: u64,
    wav: Option<(PathBuf, std::io::BufWriter<std::fs::File>)>,
}

const HEADER: usize = 44;

fn header(frames: u64) -> [u8; HEADER] {
    let rate = psxw_abi::AUDIO_HZ;
    let data = (frames * 4).min(u32::MAX as u64 - 36) as u32;
    let mut h = [0u8; HEADER];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&2u16.to_le_bytes()); // stereo
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * 4).to_le_bytes());
    h[32..34].copy_from_slice(&4u16.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data.to_le_bytes());
    h
}

impl Sink {
    pub fn new(wav: Option<&Path>) -> std::io::Result<Self> {
        let wav = match wav {
            Some(p) => {
                if let Some(d) = p.parent() {
                    std::fs::create_dir_all(d)?;
                }
                let mut f = std::io::BufWriter::new(std::fs::File::create(p)?);
                f.write_all(&header(0))?;
                Some((p.to_path_buf(), f))
            }
            None => None,
        };
        Ok(Sink { frames: 0, audible: 0, hash: 0xcbf2_9ce4_8422_2325, wav })
    }

    /// Frames as the guest wrote them: interleaved little-endian i16.
    pub fn push(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        for f in bytes.chunks_exact(4) {
            self.frames += 1;
            if f.iter().any(|&b| b != 0) {
                self.audible += 1;
            }
            for &b in f {
                self.hash = (self.hash ^ b as u64).wrapping_mul(0x100_0000_01b3);
            }
        }
        if let Some((_, w)) = &mut self.wav {
            w.write_all(&bytes[..bytes.len() / 4 * 4])?;
        }
        Ok(())
    }

    /// Frames, audible frames and the hash so far.
    pub fn so_far(&self) -> (u64, u64, u64) {
        (self.frames, self.audible, self.hash)
    }

    /// Patch the WAV header's sizes and report.
    pub fn finish(self) -> AudioStats {
        let mut wav = None;
        if let Some((path, w)) = self.wav {
            let ok = w.into_inner().map_err(|e| e.into_error()).and_then(|mut f| {
                f.seek(SeekFrom::Start(0))?;
                f.write_all(&header(self.frames))
            });
            if ok.is_ok() {
                wav = Some(path);
            }
        }
        AudioStats { frames: self.frames, audible_frames: self.audible, hash: format!("{:016x}", self.hash), wav }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_and_hash() {
        let dir = std::env::temp_dir().join(format!("psxw-audio-{}", std::process::id()));
        let path = dir.join("a.wav");
        let mut s = Sink::new(Some(&path)).unwrap();
        s.push(&[0, 0, 0, 0, 1, 0, 0xFF, 0xFF]).unwrap();
        let st = s.finish();
        assert_eq!((st.frames, st.audible_frames), (2, 1));
        let b = std::fs::read(&path).unwrap();
        assert_eq!(b.len(), HEADER + 8);
        assert_eq!(&b[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 8);
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 44_100);
        assert_eq!(st.hash, format!("{:016x}", crate::fnv(&[0, 0, 0, 0, 1, 0, 0xFF, 0xFF])));
        std::fs::remove_dir_all(dir).ok();
    }
}
