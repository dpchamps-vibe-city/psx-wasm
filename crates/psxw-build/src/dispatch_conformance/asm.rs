//! A small MIPS I assembler for the fixtures: the instructions they use,
//! labels and absolute symbols. Every pseudo-instruction has a fixed size,
//! so a fixture's layout doesn't depend on its operands.

use anyhow::{Context as _, Result, ensure};
use std::collections::BTreeMap;

pub const ZERO: u8 = 0;
pub const AT: u8 = 1;
pub const V0: u8 = 2;
pub const V1: u8 = 3;
pub const A0: u8 = 4;
pub const A1: u8 = 5;
pub const A2: u8 = 6;
pub const A3: u8 = 7;
pub const T0: u8 = 8;
pub const T1: u8 = 9;
pub const T2: u8 = 10;
pub const T3: u8 = 11;
pub const T7: u8 = 15;
pub const S0: u8 = 16;
pub const T8: u8 = 24;
pub const T9: u8 = 25;
pub const SP: u8 = 29;
pub const RA: u8 = 31;

#[derive(Debug, Clone, Copy)]
enum Fix {
    Branch,
    Jump,
    Hi,
    Lo,
}

/// Code at `base`, assembled in order.
#[derive(Debug, Clone)]
pub struct Asm {
    base: u32,
    words: Vec<u32>,
    labels: BTreeMap<String, u32>,
    fixups: Vec<(usize, String, Fix)>,
}

fn r(rs: u8, rt: u8, rd: u8, sa: u32, f: u32) -> u32 {
    (rs as u32) << 21 | (rt as u32) << 16 | (rd as u32) << 11 | (sa & 31) << 6 | f
}

fn i(op: u32, rs: u8, rt: u8, imm: u32) -> u32 {
    op << 26 | (rs as u32) << 21 | (rt as u32) << 16 | (imm & 0xFFFF)
}

/// `%hi` and `%lo` of an address for `lui` + a signed 16-bit offset.
pub fn hi_lo(addr: u32) -> (u32, u32) {
    (addr.wrapping_add(0x8000) >> 16, addr & 0xFFFF)
}

impl Asm {
    pub fn new(base: u32) -> Self {
        Self { base, words: Vec::new(), labels: BTreeMap::new(), fixups: Vec::new() }
    }

    /// The address of the next instruction.
    pub fn here(&self) -> u32 {
        self.base + 4 * self.words.len() as u32
    }

    pub fn label(&mut self, name: &str) -> u32 {
        let at = self.here();
        self.labels.insert(name.to_string(), at);
        at
    }

    /// An absolute symbol the code refers to by name.
    pub fn define(&mut self, name: &str, addr: u32) {
        self.labels.insert(name.to_string(), addr);
    }

    pub fn addr_of(&self, name: &str) -> Option<u32> {
        self.labels.get(name).copied()
    }

    pub fn word(&mut self, w: u32) -> &mut Self {
        self.words.push(w);
        self
    }

    fn fix(&mut self, w: u32, to: &str, how: Fix) -> &mut Self {
        self.fixups.push((self.words.len(), to.to_string(), how));
        self.word(w)
    }

    /// Pad with zero words up to `addr`.
    pub fn pad_to(&mut self, addr: u32) -> &mut Self {
        while self.here() < addr {
            self.word(0);
        }
        self
    }

    pub fn nop(&mut self) -> &mut Self {
        self.word(0)
    }
    pub fn addu(&mut self, rd: u8, rs: u8, rt: u8) -> &mut Self {
        self.word(r(rs, rt, rd, 0, 0x21))
    }
    pub fn subu(&mut self, rd: u8, rs: u8, rt: u8) -> &mut Self {
        self.word(r(rs, rt, rd, 0, 0x23))
    }
    pub fn xor(&mut self, rd: u8, rs: u8, rt: u8) -> &mut Self {
        self.word(r(rs, rt, rd, 0, 0x26))
    }
    pub fn or(&mut self, rd: u8, rs: u8, rt: u8) -> &mut Self {
        self.word(r(rs, rt, rd, 0, 0x25))
    }
    pub fn mov(&mut self, rd: u8, rs: u8) -> &mut Self {
        self.addu(rd, rs, ZERO)
    }
    pub fn sll(&mut self, rd: u8, rt: u8, sa: u32) -> &mut Self {
        self.word(r(0, rt, rd, sa, 0x00))
    }
    pub fn jr(&mut self, rs: u8) -> &mut Self {
        self.word(r(rs, 0, 0, 0, 0x08))
    }
    pub fn jalr(&mut self, rs: u8) -> &mut Self {
        self.word(r(rs, 0, RA, 0, 0x09))
    }
    pub fn addiu(&mut self, rt: u8, rs: u8, imm: i32) -> &mut Self {
        self.word(i(0x09, rs, rt, imm as u32))
    }
    pub fn sltiu(&mut self, rt: u8, rs: u8, imm: i32) -> &mut Self {
        self.word(i(0x0B, rs, rt, imm as u32))
    }
    pub fn ori(&mut self, rt: u8, rs: u8, imm: u32) -> &mut Self {
        self.word(i(0x0D, rs, rt, imm))
    }
    pub fn lui(&mut self, rt: u8, imm: u32) -> &mut Self {
        self.word(i(0x0F, 0, rt, imm))
    }
    pub fn lw(&mut self, rt: u8, off: i32, base: u8) -> &mut Self {
        self.word(i(0x23, base, rt, off as u32))
    }
    pub fn sw(&mut self, rt: u8, off: i32, base: u8) -> &mut Self {
        self.word(i(0x2B, base, rt, off as u32))
    }
    /// `li` as `lui` + `ori`: always two words.
    pub fn li(&mut self, rt: u8, v: u32) -> &mut Self {
        self.lui(rt, v >> 16).ori(rt, rt, v & 0xFFFF)
    }
    /// `la` as `lui` + `addiu` of a label: always two words.
    pub fn la(&mut self, rt: u8, to: &str) -> &mut Self {
        self.fix(i(0x0F, 0, rt, 0), to, Fix::Hi).fix(i(0x09, rt, rt, 0), to, Fix::Lo)
    }
    /// `lw rt, %lo(to)(base)` after `lui base, %hi(to)`.
    pub fn lw_lo(&mut self, rt: u8, to: &str, base: u8) -> &mut Self {
        self.fix(i(0x23, base, rt, 0), to, Fix::Lo)
    }
    pub fn sw_lo(&mut self, rt: u8, to: &str, base: u8) -> &mut Self {
        self.fix(i(0x2B, base, rt, 0), to, Fix::Lo)
    }
    pub fn lui_hi(&mut self, rt: u8, to: &str) -> &mut Self {
        self.fix(i(0x0F, 0, rt, 0), to, Fix::Hi)
    }
    pub fn beq(&mut self, rs: u8, rt: u8, to: &str) -> &mut Self {
        self.fix(i(0x04, rs, rt, 0), to, Fix::Branch)
    }
    pub fn bne(&mut self, rs: u8, rt: u8, to: &str) -> &mut Self {
        self.fix(i(0x05, rs, rt, 0), to, Fix::Branch)
    }
    pub fn b(&mut self, to: &str) -> &mut Self {
        self.beq(ZERO, ZERO, to)
    }
    pub fn j(&mut self, to: &str) -> &mut Self {
        self.fix(2 << 26, to, Fix::Jump)
    }
    pub fn jal(&mut self, to: &str) -> &mut Self {
        self.fix(3 << 26, to, Fix::Jump)
    }
    /// `jal` to an absolute address.
    pub fn jal_abs(&mut self, addr: u32) -> &mut Self {
        self.word(3 << 26 | (addr & 0x0FFF_FFFF) >> 2)
    }
    pub fn j_abs(&mut self, addr: u32) -> &mut Self {
        self.word(2 << 26 | (addr & 0x0FFF_FFFF) >> 2)
    }

    /// The words, labels resolved.
    pub fn finish(&self) -> Result<Vec<u32>> {
        let mut words = self.words.clone();
        for (at, to, how) in &self.fixups {
            let target = *self.labels.get(to).with_context(|| format!("undefined label {to}"))?;
            let pc = self.base + 4 * *at as u32;
            let w = &mut words[*at];
            match how {
                Fix::Branch => {
                    let off = (target as i64 - (pc as i64 + 4)) / 4;
                    ensure!((-0x8000..0x8000).contains(&off) && target % 4 == 0, "branch to {to} out of range");
                    *w |= off as u32 & 0xFFFF;
                }
                Fix::Jump => {
                    ensure!((pc + 4) & 0xF000_0000 == target & 0xF000_0000, "jump to {to} leaves the region");
                    *w |= (target & 0x0FFF_FFFF) >> 2;
                }
                Fix::Hi => *w |= hi_lo(target).0 & 0xFFFF,
                Fix::Lo => *w |= hi_lo(target).1,
            }
        }
        Ok(words)
    }

    pub fn bytes(&self) -> Result<Vec<u8>> {
        Ok(self.finish()?.iter().flat_map(|w| w.to_le_bytes()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_as_the_lifter_decodes() {
        let mut a = Asm::new(0x8010_0000);
        a.label("top");
        a.addiu(SP, SP, -24).li(T0, 0x1234_5678).la(T1, "data").beq(T0, ZERO, "top").nop().jal("top").nop();
        a.define("data", 0x8013_8004);
        let w = a.finish().unwrap();
        assert_eq!(w[0], 0x27BD_FFE8);
        assert_eq!(&w[1..3], &[0x3C08_1234, 0x3508_5678]);
        // %hi rounds up when %lo is negative.
        assert_eq!(&w[3..5], &[0x3C09_8014, 0x2529_8004]);
        assert_eq!(w[5], 0x1100_FFFA);
        assert_eq!(w[7], 0x0C04_0000);
    }
}
