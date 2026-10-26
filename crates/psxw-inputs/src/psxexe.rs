//! The PS-X EXE header (psx-spx "CDROM File Formats").

use anyhow::{Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PsxExe {
    pub pc0: u32,
    pub gp0: u32,
    pub t_addr: u32,
    pub t_size: u32,
    pub d_addr: u32,
    pub d_size: u32,
    pub b_addr: u32,
    pub b_size: u32,
    pub s_addr: u32,
    pub s_size: u32,
}

/// File offset of the text image.
pub const TEXT_OFFSET: usize = 0x800;

pub fn parse(bytes: &[u8]) -> Result<PsxExe> {
    if bytes.len() < TEXT_OFFSET || &bytes[..8] != b"PS-X EXE" {
        bail!("not a PS-X EXE");
    }
    let w = |off: usize| u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]]);
    let exe = PsxExe {
        pc0: w(0x10),
        gp0: w(0x14),
        t_addr: w(0x18),
        t_size: w(0x1C),
        d_addr: w(0x20),
        d_size: w(0x24),
        b_addr: w(0x28),
        b_size: w(0x2C),
        s_addr: w(0x30),
        s_size: w(0x34),
    };
    if TEXT_OFFSET + exe.t_size as usize > bytes.len() {
        bail!("PS-X EXE text (0x{:X} bytes) runs past the file", exe.t_size);
    }
    Ok(exe)
}

impl PsxExe {
    /// The text image as loaded at `t_addr`.
    pub fn text<'a>(&self, bytes: &'a [u8]) -> &'a [u8] {
        &bytes[TEXT_OFFSET..TEXT_OFFSET + self.t_size as usize]
    }
}
