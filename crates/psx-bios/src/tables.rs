//! Guest-visible BIOS call tables and vector data (psx-spx "BIOS Memory
//! Map"). Table entries name native Rust services, or guest patches. The
//! vector/dispatcher bytes are compatibility data for software that reads
//! them; this runtime never executes them as BIOS machine code.

use psxw_abi::kernel as k;

/// Synthetic callable addresses, clear of the kernel's IRQ functions.
pub const STAND_INS: u32 = 0xBFC0_A000;
const TABLES: [(u32, u32, u32); 3] = [(0xA0, 0x000, k::A0_LEN), (0xB0, 0x300, k::B0_LEN), (0xC0, 0x700, k::C0_LEN)];

/// Numbers psx-spx gives the same function, and C(80h..)'s B0h aliases.
/// The individual table entries remain independently patchable.
pub fn canonical(table: u32, func: u32) -> (u32, u32) {
    match (table, func) {
        (0xA0, 0x00..=0x09) => (0xB0, 0x32 + func),
        (0xA0, 0x3B..=0x3E) => (0xB0, func + 1),
        (0xA0, 0x1E) => (0xA0, 0x1C),
        (0xA0, 0x1F) => (0xA0, 0x1D),
        (0xA0, 0x70) => (0xA0, 0x55),
        (0xA0, 0x71) => (0xA0, 0x54),
        (0xA0, 0x72) => (0xA0, 0x56),
        (0xC0, 0x80..) => (0xB0, func - 0x80),
        _ => (table, func),
    }
}

pub fn stand_in(table: u32, func: u32) -> u32 {
    let (t, f) = canonical(table, func);
    let (_, base, _) = TABLES.iter().find(|e| e.0 == t).copied().expect("BIOS table");
    STAND_INS + base + 4 * f
}

pub fn stand_in_fn(addr: u32) -> Option<(u32, u32)> {
    let off = addr.checked_sub(STAND_INS)?;
    if off % 4 != 0 {
        return None;
    }
    TABLES.iter().find(|&&(_, b, n)| (b..b + 4 * n).contains(&off)).map(|&(t, b, _)| (t, (off - b) / 4))
}

pub fn entry(table: u32, func: u32) -> Option<u32> {
    match table {
        0xA0 if func < k::A0_LEN => Some(k::A0_TABLE + 4 * func),
        0xB0 if func < k::B0_LEN => Some(k::B0_TABLE + 4 * func),
        0xC0 if func < k::C0_LEN + k::B0_LEN => Some(k::C0_TABLE + 4 * func),
        _ => None,
    }
}

/// Retail vector data from psx-spx. Games use the halfword at 86h to
/// distinguish a retail BIOS (275Ah) from a debugger (375Ah).
pub const EXCEPTION_VECTOR: [u32; 4] = [0x3C1A_0000, 0x275A_0C80, 0x0340_0008, 0];

/// OpenBIOS's table-vector representation, exposed for readers/patchers.
pub fn vector(i: u32) -> [u32; 4] {
    [0x2408_0000 | (k::DISPATCHERS + 0x20 * i), 0x0100_0008, 0, 0]
}

fn dispatcher(table: u32, at: u32) -> [u32; 8] {
    [0x3C08_0000, 0x2508_0000 | at, 0x0009_5080, 0x0148_5020, 0x8D4A_0000, 0x2408_0000 | table, 0x0140_0008, 0]
}

impl<S: crate::Sdk> crate::Machine<S> {
    pub fn lay_out_tables(&mut self) {
        self.install_exception_handlers();
        self.mem.w32(k::GARBAGE, 3);
        for (i, v) in [2, 0, 0xFF].into_iter().enumerate() {
            self.mem.w32(k::RAM_SIZE + 4 * i as u32, v);
        }
        self.install_vectors();
        for (i, (table, at)) in [(0xA0, k::A0_TABLE), (0xB0, k::B0_TABLE), (0xC0, k::C0_TABLE)].into_iter().enumerate() {
            for (j, w) in dispatcher(table, at).into_iter().enumerate() {
                self.mem.w32(k::DISPATCHERS + 0x20 * i as u32 + 4 * j as u32, w);
            }
        }
        for &(table, _, n) in &TABLES {
            for f in 0..n {
                self.mem.w32(entry(table, f).expect("in table"), stand_in(table, f));
            }
        }
    }

    /// C(07h): restore the exception vector, including its copy at zero.
    pub fn install_exception_handlers(&mut self) {
        for (i, w) in EXCEPTION_VECTOR.into_iter().enumerate() {
            self.mem.w32(k::EXCEPTION_VECTOR + 4 * i as u32, w);
            self.mem.w32(k::GARBAGE + 4 * i as u32, w);
        }
    }

    /// A(45h): restore the A0h/B0h/C0h vector data.
    pub fn install_vectors(&mut self) {
        for i in 0..3 {
            for (j, w) in vector(i).into_iter().enumerate() {
                self.mem.w32(k::A0_VECTOR + 0x10 * i + 4 * j as u32, w);
            }
        }
    }

    /// C(1Ch): copy B(32h..3Bh), B(3Ch..3Fh) into A(00h..09h), A(3Bh..3Eh).
    pub fn adjust_a0_table(&mut self) {
        for (from, to, n) in [(0x32, 0x00, 10), (0x3C, 0x3B, 4)] {
            for i in 0..n {
                let v = self.mem.r32(k::B0_TABLE + 4 * (from + i));
                self.mem.w32(k::A0_TABLE + 4 * (to + i), v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Flow, GuestCall, bios, testing::machine};

    fn ret(f: Result<Flow<crate::testing::NoSdk>, String>) -> u32 {
        match f.unwrap() {
            Flow::Return(v) => v as u32,
            _ => panic!("not a return"),
        }
    }

    #[test]
    fn guest_tables_dispatch_native_functions_and_patches() {
        let mut m = machine();
        for &(t, _, n) in &TABLES {
            for f in 0..n {
                assert_eq!(stand_in_fn(m.mem.r32(entry(t, f).unwrap())), Some(canonical(t, f)));
            }
        }
        assert_eq!(stand_in_fn(STAND_INS - 4), None);
        assert_eq!(stand_in_fn(STAND_INS + 0x900), None);
        assert_eq!(m.mem.r16(k::EXCEPTION_VECTOR + 6), 0x275A);
        assert_eq!(m.mem.r32(k::RAM_SIZE), 2);
        m.mem.write(0x8001_0000, b"abc\0");
        assert_eq!(ret(bios::dispatch(&mut m, 0xA0, 0x1B, [0x8001_0000, 0, 0, 0])), 3);
        assert_eq!(ret(bios::dispatch(&mut m, 0xC0, 0x80 + 0x57, [0; 4])), k::B0_TABLE);
        m.mem.w32(entry(0xB0, 0x57).unwrap(), stand_in(0xB0, 0x56));
        assert_eq!(ret(bios::dispatch(&mut m, 0xB0, 0x57, [0; 4])), k::C0_TABLE);
        m.mem.w32(entry(0xB0, 0x32).unwrap(), 0x8002_0000);
        assert_eq!(m.mem.r32(k::A0_TABLE), stand_in(0xB0, 0x32), "alias entries are independent until adjusted");
        ret(bios::call(&mut m, 0xC0, 0x1C, [0; 4]));
        match bios::dispatch(&mut m, 0xA0, 0x00, [1, 2, 3, 4]).unwrap() {
            Flow::Call { call, then } => {
                assert_eq!(call, GuestCall { addr: 0x8002_0000, args: [1, 2, 3, 4] });
                assert!(matches!(then(&mut m, 7 | 9 << 32), Flow::Return(v) if v == 7 | 9 << 32));
            }
            _ => panic!("not a guest call"),
        }
    }
}
