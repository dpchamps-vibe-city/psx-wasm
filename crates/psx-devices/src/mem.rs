//! Guest memory as the BIOS and devices see it: the guest window, addressed
//! by guest address through the console's map (psxw-abi `map`, D70), as
//! guest code addresses it.

use psxw_abi::WINDOW_MASK;
use psxw_abi::map::{self, Region};

pub trait Mem {
    /// `out.len()` bytes of the window from offset `off`.
    fn window_read(&self, off: u32, out: &mut [u8]);
    /// Write `data` to the window from offset `off`.
    fn window_write(&mut self, off: u32, data: &[u8]);

    /// A guest pointer reached no memory: the console takes a bus error.
    fn bus_error(&self, addr: u32, len: usize, store: bool) -> ! {
        panic!("{}", bus_error_text(addr, len, store))
    }

    /// `out.len()` bytes from guest address `addr`.
    fn read(&self, addr: u32, out: &mut [u8]) {
        let mut i = 0;
        while i < out.len() {
            let a = addr.wrapping_add(i as u32);
            let Some(p) = map::place(a) else { self.bus_error(a, out.len() - i, false) };
            let n = (p.len as usize).min(out.len() - i);
            match p.region {
                Region::Expansion(v) => out[i..i + n].fill(v),
                _ => self.window_read(p.offset, &mut out[i..i + n]),
            }
            i += n;
        }
    }
    /// Write `data` from guest address `addr`.
    fn write(&mut self, addr: u32, data: &[u8]) {
        let mut i = 0;
        while i < data.len() {
            let a = addr.wrapping_add(i as u32);
            let Some(p) = map::place(a) else { self.bus_error(a, data.len() - i, true) };
            let n = (p.len as usize).min(data.len() - i);
            if !matches!(p.region, Region::Expansion(_) | Region::BiosRom) {
                self.window_write(p.offset, &data[i..i + n]);
            }
            i += n;
        }
    }

    fn r8(&self, addr: u32) -> u8 {
        let mut b = [0; 1];
        self.read(addr, &mut b);
        b[0]
    }
    fn r16(&self, addr: u32) -> u16 {
        let mut b = [0; 2];
        self.read(addr, &mut b);
        u16::from_le_bytes(b)
    }
    fn r32(&self, addr: u32) -> u32 {
        let mut b = [0; 4];
        self.read(addr, &mut b);
        u32::from_le_bytes(b)
    }
    fn w8(&mut self, addr: u32, v: u8) {
        self.write(addr, &[v]);
    }
    fn w16(&mut self, addr: u32, v: u16) {
        self.write(addr, &v.to_le_bytes());
    }
    fn w32(&mut self, addr: u32, v: u32) {
        self.write(addr, &v.to_le_bytes());
    }
    /// A NUL-terminated string (at most `max` bytes).
    fn cstr(&self, addr: u32, max: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for i in 0..max as u32 {
            let b = self.r8(addr.wrapping_add(i));
            if b == 0 {
                break;
            }
            out.push(b);
        }
        out
    }
    fn bytes(&self, addr: u32, len: usize) -> Vec<u8> {
        let mut v = vec![0; len];
        self.read(addr, &mut v);
        v
    }
}

/// What a bus error at `addr` says.
pub fn bus_error_text(addr: u32, len: usize, store: bool) -> String {
    let what = if store { "store" } else { "load" };
    format!("bus error: {what} of {len} bytes at {addr:#010x}")
}

/// A window in a `Vec` (tests, native tools).
pub struct VecMem(pub Vec<u8>);

impl Default for VecMem {
    fn default() -> Self {
        VecMem(vec![0; WINDOW_MASK as usize + 1])
    }
}

impl Mem for VecMem {
    fn window_read(&self, off: u32, out: &mut [u8]) {
        out.copy_from_slice(&self.0[off as usize..off as usize + out.len()]);
    }
    fn window_write(&mut self, off: u32, data: &[u8]) {
        self.0[off as usize..off as usize + data.len()].copy_from_slice(data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_pointers_follow_the_map() {
        let mut m = VecMem::default();
        // A store through a mirror and KSEG1 is RAM's.
        m.w32(0xA060_0100, 0x1234_5678);
        assert_eq!(m.r32(0x8000_0100), 0x1234_5678);
        // A read across RAM's end wraps to its start, as the mirror does.
        m.w8(0x8000_0000, 0xAB);
        assert_eq!(m.bytes(0x801F_FFFF, 2)[1], 0xAB);
        // The BIOS ROM ignores stores; expansion region 1 reads FFh.
        m.w32(0xBFC0_0000, 0xFFFF_FFFF);
        assert_eq!(m.r32(0xBFC0_0000), 0);
        assert_eq!(m.r16(0x1F00_0084), 0xFFFF);
        let unmapped = std::panic::catch_unwind(|| VecMem::default().r32(0x0080_0000));
        assert!(unmapped.is_err());
    }
}
