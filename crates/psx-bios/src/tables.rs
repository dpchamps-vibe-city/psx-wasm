//! The kernel's call tables and vectors in its RAM (D68; psx-spx "BIOS
//! Memory Map"): the A0h, B0h and C0h tables at 200h, 874h and 674h, each
//! entry a function's address, the four-opcode vectors at A0h, B0h and C0h
//! with the dispatchers they jump to, the exception vector and the
//! variables psx-spx gives fixed addresses.
//!
//! The functions are this crate's Rust, so each entry holds a stand-in: an
//! address in BIOS ROM space naming the function, one per function (two
//! numbers psx-spx gives one function share it). A call through the
//! vector ([`crate::bios::dispatch`]) reads the entry as the dispatcher
//! does: a stand-in runs its function, any other address (a game patched
//! the entry) runs as guest code.

use psxw_abi::kernel as k;

/// The stand-ins: BIOS ROM space (psx-spx "Kernel Part 1"), clear of the
/// kernel's element functions ([`crate::kernel::KERNEL_ELEMENTS`]'). A
/// guest call through one reaches its thunk (psxw-abi `BIOS_THUNKS`),
/// which calls the runtime's `bios` with the stand-in's address.
pub const STAND_INS: u32 = psxw_abi::BIOS_STAND_INS.start;

/// (table, offset of its stand-ins, entries).
const TABLES: [(u32, u32, u32); 3] = [(0xA0, 0x000, k::A0_LEN), (0xB0, 0x300, k::B0_LEN), (0xC0, 0x700, k::C0_LEN)];

/// The function `(table, func)` is: psx-spx gives some two numbers ("A(00h)
/// or B(32h)", `index` and `strchr` "exactly the same"), and C(80h..) reads
/// B0h's table.
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

/// The stand-in of `(table, func)`'s function.
pub fn stand_in(table: u32, func: u32) -> u32 {
    let (t, f) = canonical(table, func);
    let (_, base, _) = TABLES.iter().find(|e| e.0 == t).copied().unwrap_or((0, 0, 0));
    STAND_INS + base + 4 * f
}

/// The function stand-in `addr` names, if it is one.
pub fn stand_in_fn(addr: u32) -> Option<(u32, u32)> {
    let off = addr.checked_sub(STAND_INS)?;
    if off % 4 != 0 {
        return None;
    }
    TABLES.iter().find(|&&(_, b, n)| (b..b + 4 * n).contains(&off)).map(|&(t, b, _)| (t, (off - b) / 4))
}

/// Where `(table, func)`'s entry is, while the number is in its table
/// (C(80h..) in B0h's).
pub fn entry(table: u32, func: u32) -> Option<u32> {
    match table {
        0xA0 if func < k::A0_LEN => Some(k::A0_TABLE + 4 * func),
        0xB0 if func < k::B0_LEN => Some(k::B0_TABLE + 4 * func),
        0xC0 if func < k::C0_LEN + k::B0_LEN => Some(k::C0_TABLE + 4 * func),
        _ => None,
    }
}

/// The exception vector's opcodes (psx-spx, the retail BIOS's: `lui k0,0;
/// addiu k0,k0,0C80h; jr k0; nop`). Games read them (the halfword at 86h
/// tells a retail BIOS, 275Ah, from a debugger's, 375Ah).
pub const EXCEPTION_VECTOR: [u32; 4] = [0x3C1A_0000, 0x275A_0C80, 0x0340_0008, 0];

/// The A0h, B0h and C0h vectors (OpenBIOS's: `li t0,dispatcher; jr t0;
/// nop; nop`; psx-spx doesn't give the opcodes).
pub fn vector(i: u32) -> [u32; 4] {
    [0x2408_0000 | (k::DISPATCHERS + 0x20 * i), 0x0100_0008, 0, 0]
}

/// A dispatcher (OpenBIOS's `A0Handler`): `t2 = table[t1]`, `t0` the
/// table's number, and on to `t2`.
fn dispatcher(table: u32, at: u32) -> [u32; 8] {
    [0x3C08_0000, 0x2508_0000 | at, 0x0009_5080, 0x0148_5020, 0x8D4A_0000, 0x2408_0000 | table, 0x0140_0008, 0]
}

impl<S: crate::Sdk> crate::Machine<S> {
    /// Kernel RAM as the BIOS's boot leaves it before running a game: the
    /// garbage area, the variables, the vectors, the dispatchers and the
    /// tables full of stand-ins.
    pub fn lay_out_tables(&mut self) {
        self.install_exception_handlers();
        // CD-ROM delays then smash the first word (psx-spx).
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
                let at = entry(table, f).expect("in the table");
                self.mem.w32(at, stand_in(table, f));
            }
        }
    }

    /// C(07h) `InstallExceptionHandlers`: the exception vector at 80h, and
    /// (psx-spx: "for whatever reason") at 0.
    pub fn install_exception_handlers(&mut self) {
        for (i, w) in EXCEPTION_VECTOR.into_iter().enumerate() {
            self.mem.w32(k::EXCEPTION_VECTOR + 4 * i as u32, w);
            self.mem.w32(k::GARBAGE + 4 * i as u32, w);
        }
    }

    /// A(45h) `init_a0_b0_c0_vectors`.
    pub fn install_vectors(&mut self) {
        for i in 0..3 {
            for (j, w) in vector(i).into_iter().enumerate() {
                self.mem.w32(k::A0_VECTOR + 0x10 * i + 4 * j as u32, w);
            }
        }
    }

    /// C(1Ch) `AdjustA0Table`: B(32h..3Bh) and B(3Ch..3Fh)'s entries into
    /// A(00h..09h) and A(3Bh..3Eh).
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
    use crate::testing::machine;

    #[test]
    fn stand_ins_name_their_functions() {
        assert_eq!(stand_in(0xA0, 0x02), stand_in(0xB0, 0x34), "read");
        assert_eq!(stand_in(0xC0, 0x80 + 0x57), stand_in(0xB0, 0x57));
        assert_ne!(stand_in(0xA0, 0x10), stand_in(0xA0, 0x11));
        for (t, n) in [(0xA0, k::A0_LEN), (0xB0, k::B0_LEN), (0xC0, k::C0_LEN)] {
            for f in 0..n {
                assert_eq!(stand_in_fn(stand_in(t, f)), Some(canonical(t, f)));
            }
        }
        assert_eq!(stand_in_fn(STAND_INS - 4), None);
        assert_eq!(stand_in_fn(STAND_INS + 0x900), None);
        // psxw-abi's range, whose thunks the link puts in the dispatch table.
        assert_eq!(stand_in(0xC0, 0x7F) + 4, psxw_abi::BIOS_STAND_INS.end);
    }

    #[test]
    fn kernel_ram_has_the_tables() {
        let mut m = machine();
        assert_eq!(m.mem.r32(k::A0_TABLE), stand_in(0xB0, 0x32));
        assert_eq!(m.mem.r32(k::B0_TABLE + 4 * 0x32), stand_in(0xB0, 0x32));
        assert_eq!(m.mem.r32(k::C0_TABLE + 4 * 0x80), stand_in(0xB0, 0));
        assert_eq!(m.mem.r16(k::EXCEPTION_VECTOR + 6), 0x275A);
        assert_eq!(m.mem.bytes(0, 8), [3, 0, 0, 0, 0x80, 0x0C, 0x5A, 0x27]);
        assert_eq!(m.mem.r32(k::RAM_SIZE), 2);
        assert_eq!(m.mem.r32(k::B0_VECTOR), 0x2408_05E0);
        assert_eq!(m.mem.r32(k::DISPATCHERS + 0x20 + 4), 0x2508_0874);
        // A game's entry in B(32h), then AdjustA0Table.
        m.mem.w32(k::B0_TABLE + 4 * 0x32, 0x8001_2340);
        m.adjust_a0_table();
        assert_eq!(m.mem.r32(k::A0_TABLE), 0x8001_2340);
    }
}
