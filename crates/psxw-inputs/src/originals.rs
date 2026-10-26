//! Original container bytes, addressed by guest address. Every splat config
//! names the file it splits (`target_path`) and its `sha1`, so this needs
//! nothing beyond what any splat-based decomp already declares. The files
//! are the user's (never stored here) and are accepted only on a hash match.

use crate::map::{AddressMap, Container};
use anyhow::{Result, bail};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone)]
struct Image {
    bytes: Vec<u8>,
    segments: Vec<(std::ops::Range<u32>, u32)>,
}

#[derive(Debug, Clone, Default)]
pub struct Originals {
    images: HashMap<String, Image>,
    /// Containers whose original file is absent; checks against them skip.
    pub missing: Vec<String>,
}

impl Originals {
    /// Load every container's original from the user's checkout.
    pub fn load(checkout: &Path, map: &AddressMap) -> Result<Self> {
        let mut out = Originals::default();
        for c in &map.containers {
            match load_one(checkout, c)? {
                Some(img) => {
                    out.images.insert(c.id.clone(), img);
                }
                None => out.missing.push(c.id.clone()),
            }
        }
        Ok(out)
    }

    /// The whole original file of a container.
    pub fn file(&self, container: &str) -> Option<&[u8]> {
        self.images.get(container).map(|img| img.bytes.as_slice())
    }

    /// Little-endian word at a guest address.
    pub fn word(&self, container: &str, addr: u32) -> Option<u32> {
        let b = self.read(container, addr, 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Original bytes at a guest address in a container.
    pub fn read(&self, container: &str, addr: u32, len: usize) -> Option<&[u8]> {
        let img = self.images.get(container)?;
        let (file, vram) = img.segments.iter().find(|(file, vram)| {
            addr >= *vram && (addr - vram) as u64 + len as u64 <= (file.end - file.start) as u64
        })?;
        let start = (file.start + (addr - vram)) as usize;
        img.bytes.get(start..start + len)
    }
}

fn load_one(checkout: &Path, c: &Container) -> Result<Option<Image>> {
    let Some(target) = &c.target_path else { return Ok(None) };
    let path = checkout.join(target);
    let Ok(bytes) = std::fs::read(&path) else { return Ok(None) };
    if let Some(want) = &c.sha1 {
        use sha1::{Digest, Sha1};
        let got: String = Sha1::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        if &got != want {
            bail!("{}: sha1 {got} != splat config {want}", path.display());
        }
    }
    Ok(Some(Image { bytes, segments: c.segments.clone() }))
}
