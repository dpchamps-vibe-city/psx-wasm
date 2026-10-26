//! Guest memory as devices see it: the 16 MiB window, addressed by guest
//! address (any segment; the mask folds them).

use psxw_abi::WINDOW_MASK;

pub trait Mem {
    fn read(&self, addr: u32, out: &mut [u8]);
    fn write(&mut self, addr: u32, data: &[u8]);

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

/// A window in a `Vec` (tests, native tools).
pub struct VecMem(pub Vec<u8>);

impl Default for VecMem {
    fn default() -> Self {
        VecMem(vec![0; WINDOW_MASK as usize + 1])
    }
}

impl Mem for VecMem {
    fn read(&self, addr: u32, out: &mut [u8]) {
        for (i, b) in out.iter_mut().enumerate() {
            *b = self.0[(addr.wrapping_add(i as u32) & WINDOW_MASK) as usize];
        }
    }
    fn write(&mut self, addr: u32, data: &[u8]) {
        for (i, b) in data.iter().enumerate() {
            self.0[(addr.wrapping_add(i as u32) & WINDOW_MASK) as usize] = *b;
        }
    }
}
