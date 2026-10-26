//! Disc images: a `.cue` sheet over raw 2352-byte `.bin` files (one for
//! the whole disc, or one per track). LBA 0 is the first sector of the
//! first file (MSF 00:02:00).

use anyhow::{Context as _, Result, bail};
use psxw_abi::DiscInfo;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const SECTOR: u64 = 2352;

#[derive(Debug)]
struct BinFile {
    path: PathBuf,
    file: File,
    start: u32,
    sectors: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Track {
    pub number: u32,
    pub data: bool,
    /// LBA of INDEX 01.
    pub lba: u32,
}

#[derive(Debug)]
pub struct Disc {
    files: Vec<BinFile>,
    pub tracks: Vec<Track>,
}

fn msf(s: &str) -> Result<u32> {
    let p: Vec<u32> = s.split(':').map(|x| x.parse()).collect::<Result<_, _>>().with_context(|| format!("bad MSF {s}"))?;
    if p.len() != 3 {
        bail!("bad MSF {s}");
    }
    Ok((p[0] * 60 + p[1]) * 75 + p[2])
}

impl Disc {
    pub fn open(cue: &Path) -> Result<Disc> {
        let text = std::fs::read_to_string(cue).with_context(|| format!("reading {}", cue.display()))?;
        let dir = cue.parent().unwrap_or(Path::new("."));
        let mut files: Vec<BinFile> = Vec::new();
        let mut tracks = Vec::new();
        let mut cur: Option<(u32, bool)> = None;
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("FILE ") {
                let name = rest.rsplit_once(' ').map_or(rest, |(n, _)| n).trim_matches('"');
                let path = dir.join(name);
                let file = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
                let len = file.metadata()?.len();
                if len % SECTOR != 0 {
                    bail!("{} is not raw 2352-byte sectors", path.display());
                }
                let start = files.last().map_or(0, |f| f.start + f.sectors);
                files.push(BinFile { path, file, start, sectors: (len / SECTOR) as u32 });
            } else if let Some(rest) = line.strip_prefix("TRACK ") {
                let mut it = rest.split_whitespace();
                let n: u32 = it.next().unwrap_or("0").parse()?;
                let kind = it.next().unwrap_or("");
                if kind != "AUDIO" && kind != "MODE2/2352" && kind != "MODE1/2352" {
                    bail!("track {n}: unsupported type {kind}");
                }
                cur = Some((n, kind != "AUDIO"));
            } else if let Some(rest) = line.strip_prefix("INDEX 01 ") {
                let (n, data) = cur.context("INDEX before TRACK")?;
                let f = files.last().context("INDEX before FILE")?;
                tracks.push(Track { number: n, data, lba: f.start + msf(rest.trim())? });
            }
        }
        if files.is_empty() || tracks.is_empty() {
            bail!("{} names no tracks", cue.display());
        }
        Ok(Disc { files, tracks })
    }

    pub fn sectors(&self) -> u32 {
        self.files.last().map_or(0, |f| f.start + f.sectors)
    }

    pub fn read_raw(&mut self, lba: u32, out: &mut [u8; 2352]) -> bool {
        let Some(f) = self.files.iter_mut().find(|f| lba >= f.start && lba < f.start + f.sectors) else { return false };
        let off = (lba - f.start) as u64 * SECTOR;
        f.file.seek(SeekFrom::Start(off)).is_ok() && f.file.read_exact(out).is_ok()
    }

    pub fn info(&self) -> DiscInfo {
        let mut info = DiscInfo { tracks: self.tracks.len() as u32, sectors: self.sectors(), ..Default::default() };
        for (i, t) in self.tracks.iter().take(99).enumerate() {
            info.track_lba[i] = t.lba;
            info.track_data[i] = t.data as u8;
        }
        info
    }

    /// sha1 of each image file, for the pin in `inputs.lock`.
    pub fn file_sha1s(&self) -> Result<Vec<(String, String)>> {
        use sha1::{Digest, Sha1};
        let mut out = Vec::new();
        for f in &self.files {
            let mut h = Sha1::new();
            let mut file = File::open(&f.path)?;
            let mut buf = vec![0u8; 1 << 20];
            loop {
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            let name = f.path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            out.push((name, h.finalize().iter().map(|b| format!("{b:02x}")).collect()));
        }
        Ok(out)
    }
}

impl psx_devices::cd::Disc for Disc {
    fn read_raw(&mut self, lba: u32, out: &mut [u8; 2352]) -> bool {
        Disc::read_raw(self, lba, out)
    }
}
