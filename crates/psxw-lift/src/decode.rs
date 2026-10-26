//! MIPS-I (R3000A) instruction decoding, including COP0 and COP2 (GTE).
//! Anything the R3000 doesn't implement (COP1, COP3, 64-bit ops, the
//! likely branches of later ISAs) decodes as [`Op::Invalid`], so a
//! translator never guesses at data it walked into.

/// One decoded instruction. Fields not used by `op` are still filled from
/// the word, so callers can print them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Insn {
    pub word: u32,
    pub op: Op,
    pub rs: u8,
    pub rt: u8,
    pub rd: u8,
    pub sa: u8,
    /// The 16-bit immediate, sign-extended.
    pub simm: i32,
    /// The 16-bit immediate, zero-extended.
    pub uimm: u32,
    /// The 26-bit jump target field.
    pub target: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Op {
    // SPECIAL
    Sll,
    Srl,
    Sra,
    Sllv,
    Srlv,
    Srav,
    Jr,
    Jalr,
    Syscall,
    Break,
    Mfhi,
    Mthi,
    Mflo,
    Mtlo,
    Mult,
    Multu,
    Div,
    Divu,
    Add,
    Addu,
    Sub,
    Subu,
    And,
    Or,
    Xor,
    Nor,
    Slt,
    Sltu,
    // REGIMM
    Bltz,
    Bgez,
    Bltzal,
    Bgezal,
    // I/J-type
    J,
    Jal,
    Beq,
    Bne,
    Blez,
    Bgtz,
    Addi,
    Addiu,
    Slti,
    Sltiu,
    Andi,
    Ori,
    Xori,
    Lui,
    // COP0
    Mfc0,
    Mtc0,
    Rfe,
    // COP2
    Mfc2,
    Cfc2,
    Mtc2,
    Ctc2,
    /// GTE command; the 25-bit command word is `word & 0x01FF_FFFF`.
    Cop2,
    // Loads and stores
    Lb,
    Lh,
    Lwl,
    Lw,
    Lbu,
    Lhu,
    Lwr,
    Sb,
    Sh,
    Swl,
    Sw,
    Swr,
    Lwc2,
    Swc2,
    Invalid,
}

impl Op {
    pub fn is_branch(self) -> bool {
        matches!(self, Op::Beq | Op::Bne | Op::Blez | Op::Bgtz | Op::Bltz | Op::Bgez | Op::Bltzal | Op::Bgezal)
    }

    pub fn is_jump(self) -> bool {
        matches!(self, Op::J | Op::Jal | Op::Jr | Op::Jalr)
    }

    /// Has a branch delay slot.
    pub fn has_delay_slot(self) -> bool {
        self.is_branch() || self.is_jump()
    }

    /// Loads into a CPU register, with the R3000's one-instruction delay
    /// (memory loads and coprocessor-to-CPU moves).
    pub fn is_delayed_load(self) -> bool {
        matches!(
            self,
            Op::Lb | Op::Lh | Op::Lwl | Op::Lw | Op::Lbu | Op::Lhu | Op::Lwr | Op::Mfc0 | Op::Mfc2 | Op::Cfc2
        )
    }

    pub fn is_load(self) -> bool {
        matches!(self, Op::Lb | Op::Lh | Op::Lwl | Op::Lw | Op::Lbu | Op::Lhu | Op::Lwr | Op::Lwc2)
    }

    pub fn is_store(self) -> bool {
        matches!(self, Op::Sb | Op::Sh | Op::Swl | Op::Sw | Op::Swr | Op::Swc2)
    }

    pub fn mnemonic(self) -> &'static str {
        use Op::*;
        match self {
            Sll => "sll",
            Srl => "srl",
            Sra => "sra",
            Sllv => "sllv",
            Srlv => "srlv",
            Srav => "srav",
            Jr => "jr",
            Jalr => "jalr",
            Syscall => "syscall",
            Break => "break",
            Mfhi => "mfhi",
            Mthi => "mthi",
            Mflo => "mflo",
            Mtlo => "mtlo",
            Mult => "mult",
            Multu => "multu",
            Div => "div",
            Divu => "divu",
            Add => "add",
            Addu => "addu",
            Sub => "sub",
            Subu => "subu",
            And => "and",
            Or => "or",
            Xor => "xor",
            Nor => "nor",
            Slt => "slt",
            Sltu => "sltu",
            Bltz => "bltz",
            Bgez => "bgez",
            Bltzal => "bltzal",
            Bgezal => "bgezal",
            J => "j",
            Jal => "jal",
            Beq => "beq",
            Bne => "bne",
            Blez => "blez",
            Bgtz => "bgtz",
            Addi => "addi",
            Addiu => "addiu",
            Slti => "slti",
            Sltiu => "sltiu",
            Andi => "andi",
            Ori => "ori",
            Xori => "xori",
            Lui => "lui",
            Mfc0 => "mfc0",
            Mtc0 => "mtc0",
            Rfe => "rfe",
            Mfc2 => "mfc2",
            Cfc2 => "cfc2",
            Mtc2 => "mtc2",
            Ctc2 => "ctc2",
            Cop2 => "cop2",
            Lb => "lb",
            Lh => "lh",
            Lwl => "lwl",
            Lw => "lw",
            Lbu => "lbu",
            Lhu => "lhu",
            Lwr => "lwr",
            Sb => "sb",
            Sh => "sh",
            Swl => "swl",
            Sw => "sw",
            Swr => "swr",
            Lwc2 => "lwc2",
            Swc2 => "swc2",
            Invalid => "invalid",
        }
    }
}

pub fn decode(word: u32) -> Insn {
    let rs = ((word >> 21) & 31) as u8;
    let rt = ((word >> 16) & 31) as u8;
    let rd = ((word >> 11) & 31) as u8;
    let sa = ((word >> 6) & 31) as u8;
    let funct = word & 63;
    let op = match word >> 26 {
        0x00 => match funct {
            0x00 => Op::Sll,
            0x02 => Op::Srl,
            0x03 => Op::Sra,
            0x04 => Op::Sllv,
            0x06 => Op::Srlv,
            0x07 => Op::Srav,
            0x08 => Op::Jr,
            0x09 => Op::Jalr,
            0x0C => Op::Syscall,
            0x0D => Op::Break,
            0x10 => Op::Mfhi,
            0x11 => Op::Mthi,
            0x12 => Op::Mflo,
            0x13 => Op::Mtlo,
            0x18 => Op::Mult,
            0x19 => Op::Multu,
            0x1A => Op::Div,
            0x1B => Op::Divu,
            0x20 => Op::Add,
            0x21 => Op::Addu,
            0x22 => Op::Sub,
            0x23 => Op::Subu,
            0x24 => Op::And,
            0x25 => Op::Or,
            0x26 => Op::Xor,
            0x27 => Op::Nor,
            0x2A => Op::Slt,
            0x2B => Op::Sltu,
            _ => Op::Invalid,
        },
        0x01 => match rt {
            0x00 => Op::Bltz,
            0x01 => Op::Bgez,
            0x10 => Op::Bltzal,
            0x11 => Op::Bgezal,
            _ => Op::Invalid,
        },
        0x02 => Op::J,
        0x03 => Op::Jal,
        0x04 => Op::Beq,
        0x05 => Op::Bne,
        0x06 => Op::Blez,
        0x07 => Op::Bgtz,
        0x08 => Op::Addi,
        0x09 => Op::Addiu,
        0x0A => Op::Slti,
        0x0B => Op::Sltiu,
        0x0C => Op::Andi,
        0x0D => Op::Ori,
        0x0E => Op::Xori,
        0x0F => Op::Lui,
        0x10 => match rs {
            0x00 => Op::Mfc0,
            0x04 => Op::Mtc0,
            0x10 if funct == 0x10 => Op::Rfe,
            _ => Op::Invalid,
        },
        0x12 => match rs {
            0x00 => Op::Mfc2,
            0x02 => Op::Cfc2,
            0x04 => Op::Mtc2,
            0x06 => Op::Ctc2,
            r if r & 0x10 != 0 => Op::Cop2,
            _ => Op::Invalid,
        },
        0x20 => Op::Lb,
        0x21 => Op::Lh,
        0x22 => Op::Lwl,
        0x23 => Op::Lw,
        0x24 => Op::Lbu,
        0x25 => Op::Lhu,
        0x26 => Op::Lwr,
        0x28 => Op::Sb,
        0x29 => Op::Sh,
        0x2A => Op::Swl,
        0x2B => Op::Sw,
        0x2E => Op::Swr,
        0x32 => Op::Lwc2,
        0x3A => Op::Swc2,
        _ => Op::Invalid,
    };
    Insn {
        word,
        op,
        rs,
        rt,
        rd,
        sa,
        simm: word as u16 as i16 as i32,
        uimm: word & 0xFFFF,
        target: word & 0x03FF_FFFF,
    }
}

impl Insn {
    /// Branch target for an instruction at `pc` (branches only).
    pub fn branch_target(&self, pc: u32) -> u32 {
        pc.wrapping_add(4).wrapping_add((self.simm << 2) as u32)
    }

    /// Jump target for `j`/`jal` at `pc`.
    pub fn jump_target(&self, pc: u32) -> u32 {
        (pc.wrapping_add(4) & 0xF000_0000) | (self.target << 2)
    }

    /// GPRs read, as a bit set over r0..r31 (r0 excluded).
    pub fn reads(&self) -> u32 {
        use Op::*;
        let s = 1u32 << self.rs;
        let t = 1u32 << self.rt;
        let m = match self.op {
            Sll | Srl | Sra => t,
            Sllv | Srlv | Srav => s | t,
            Jr | Jalr | Mthi | Mtlo => s,
            Mult | Multu | Div | Divu | Add | Addu | Sub | Subu | And | Or | Xor | Nor | Slt | Sltu => s | t,
            Beq | Bne => s | t,
            Bltz | Bgez | Bltzal | Bgezal | Blez | Bgtz => s,
            Addi | Addiu | Slti | Sltiu | Andi | Ori | Xori => s,
            Mtc0 | Mtc2 | Ctc2 => t,
            Lb | Lh | Lw | Lbu | Lhu | Lwc2 => s,
            // Partial loads merge into the old value.
            Lwl | Lwr => s | t,
            Sb | Sh | Swl | Sw | Swr => s | t,
            Swc2 => s,
            // `syscall` reads its arguments in a0..a3 by convention.
            Syscall => 0xF0,
            _ => 0,
        };
        m & !1
    }

    /// GPR written, if any (r0 writes are dropped).
    pub fn writes(&self) -> Option<u8> {
        use Op::*;
        let r = match self.op {
            Sll | Srl | Sra | Sllv | Srlv | Srav | Mfhi | Mflo | Add | Addu | Sub | Subu | And | Or | Xor | Nor | Slt
            | Sltu => self.rd,
            Jalr => self.rd,
            Jal | Bltzal | Bgezal => 31,
            Addi | Addiu | Slti | Sltiu | Andi | Ori | Xori | Lui => self.rt,
            Mfc0 | Mfc2 | Cfc2 => self.rt,
            Lb | Lh | Lwl | Lw | Lbu | Lhu | Lwr => self.rt,
            _ => return None,
        };
        (r != 0).then_some(r)
    }
}

pub const REG_NAMES: [&str; 32] = [
    "zero", "at", "v0", "v1", "a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "s0", "s1", "s2",
    "s3", "s4", "s5", "s6", "s7", "t8", "t9", "k0", "k1", "gp", "sp", "fp", "ra",
];

/// A short disassembly, for comments in generated code and reports.
pub fn disasm(insn: &Insn, pc: u32) -> String {
    use Op::*;
    let r = |n: u8| REG_NAMES[n as usize];
    let m = insn.op.mnemonic();
    match insn.op {
        Sll | Srl | Sra => format!("{m} {}, {}, {}", r(insn.rd), r(insn.rt), insn.sa),
        Sllv | Srlv | Srav => format!("{m} {}, {}, {}", r(insn.rd), r(insn.rt), r(insn.rs)),
        Jr | Mthi | Mtlo => format!("{m} {}", r(insn.rs)),
        Jalr => format!("{m} {}, {}", r(insn.rd), r(insn.rs)),
        Syscall | Break => format!("{m} 0x{:X}", (insn.word >> 6) & 0xFFFFF),
        Mfhi | Mflo => format!("{m} {}", r(insn.rd)),
        Mult | Multu | Div | Divu => format!("{m} {}, {}", r(insn.rs), r(insn.rt)),
        Add | Addu | Sub | Subu | And | Or | Xor | Nor | Slt | Sltu => {
            format!("{m} {}, {}, {}", r(insn.rd), r(insn.rs), r(insn.rt))
        }
        Bltz | Bgez | Bltzal | Bgezal | Blez | Bgtz => format!("{m} {}, 0x{:08X}", r(insn.rs), insn.branch_target(pc)),
        Beq | Bne => format!("{m} {}, {}, 0x{:08X}", r(insn.rs), r(insn.rt), insn.branch_target(pc)),
        J | Jal => format!("{m} 0x{:08X}", insn.jump_target(pc)),
        Addi | Addiu | Slti | Sltiu => format!("{m} {}, {}, {}", r(insn.rt), r(insn.rs), insn.simm),
        Andi | Ori | Xori => format!("{m} {}, {}, 0x{:X}", r(insn.rt), r(insn.rs), insn.uimm),
        Lui => format!("{m} {}, 0x{:X}", r(insn.rt), insn.uimm),
        Mfc0 | Mtc0 | Mfc2 | Mtc2 | Cfc2 | Ctc2 => format!("{m} {}, ${}", r(insn.rt), insn.rd),
        Rfe => m.to_string(),
        Cop2 => format!("{m} 0x{:07X}", insn.word & 0x01FF_FFFF),
        Lwc2 | Swc2 => format!("{m} ${}, {}({})", insn.rt, insn.simm, r(insn.rs)),
        _ if insn.op.is_load() || insn.op.is_store() => format!("{m} {}, {}({})", r(insn.rt), insn.simm, r(insn.rs)),
        _ => format!(".word 0x{:08X}", insn.word),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One encoding per opcode, assembled by hand from the MIPS-I manual's
    /// field layouts and cross-checked against GNU as output.
    #[test]
    fn decodes_every_opcode() {
        let cases: &[(u32, Op, &str)] = &[
            (0x0004_1080, Op::Sll, "sll v0, a0, 2"),
            (0x0004_1082, Op::Srl, "srl v0, a0, 2"),
            (0x0004_1083, Op::Sra, "sra v0, a0, 2"),
            (0x00A4_1004, Op::Sllv, "sllv v0, a0, a1"),
            (0x00A4_1006, Op::Srlv, "srlv v0, a0, a1"),
            (0x00A4_1007, Op::Srav, "srav v0, a0, a1"),
            (0x03E0_0008, Op::Jr, "jr ra"),
            (0x0040_F809, Op::Jalr, "jalr ra, v0"),
            (0x0000_000C, Op::Syscall, "syscall 0x0"),
            (0x0007_000D, Op::Break, "break 0x1C00"),
            (0x0000_1010, Op::Mfhi, "mfhi v0"),
            (0x0080_0011, Op::Mthi, "mthi a0"),
            (0x0000_1012, Op::Mflo, "mflo v0"),
            (0x0080_0013, Op::Mtlo, "mtlo a0"),
            (0x0085_0018, Op::Mult, "mult a0, a1"),
            (0x0085_0019, Op::Multu, "multu a0, a1"),
            (0x0085_001A, Op::Div, "div a0, a1"),
            (0x0085_001B, Op::Divu, "divu a0, a1"),
            (0x0085_1020, Op::Add, "add v0, a0, a1"),
            (0x0085_1021, Op::Addu, "addu v0, a0, a1"),
            (0x0085_1022, Op::Sub, "sub v0, a0, a1"),
            (0x0085_1023, Op::Subu, "subu v0, a0, a1"),
            (0x0085_1024, Op::And, "and v0, a0, a1"),
            (0x0085_1025, Op::Or, "or v0, a0, a1"),
            (0x0085_1026, Op::Xor, "xor v0, a0, a1"),
            (0x0085_1027, Op::Nor, "nor v0, a0, a1"),
            (0x0085_102A, Op::Slt, "slt v0, a0, a1"),
            (0x0085_102B, Op::Sltu, "sltu v0, a0, a1"),
            (0x0480_0003, Op::Bltz, "bltz a0, 0x80010010"),
            (0x0481_0003, Op::Bgez, "bgez a0, 0x80010010"),
            (0x0490_0003, Op::Bltzal, "bltzal a0, 0x80010010"),
            (0x0491_0003, Op::Bgezal, "bgezal a0, 0x80010010"),
            (0x0800_4004, Op::J, "j 0x80010010"),
            (0x0C00_4004, Op::Jal, "jal 0x80010010"),
            (0x1085_0003, Op::Beq, "beq a0, a1, 0x80010010"),
            (0x1485_0003, Op::Bne, "bne a0, a1, 0x80010010"),
            (0x1880_0003, Op::Blez, "blez a0, 0x80010010"),
            (0x1C80_0003, Op::Bgtz, "bgtz a0, 0x80010010"),
            (0x2082_FFFF, Op::Addi, "addi v0, a0, -1"),
            (0x2482_FFFF, Op::Addiu, "addiu v0, a0, -1"),
            (0x2882_0010, Op::Slti, "slti v0, a0, 16"),
            (0x2C82_0010, Op::Sltiu, "sltiu v0, a0, 16"),
            (0x3082_00FF, Op::Andi, "andi v0, a0, 0xFF"),
            (0x3482_00FF, Op::Ori, "ori v0, a0, 0xFF"),
            (0x3882_00FF, Op::Xori, "xori v0, a0, 0xFF"),
            (0x3C02_1F80, Op::Lui, "lui v0, 0x1F80"),
            (0x4002_6000, Op::Mfc0, "mfc0 v0, $12"),
            (0x4082_6000, Op::Mtc0, "mtc0 v0, $12"),
            (0x4200_0010, Op::Rfe, "rfe"),
            (0x4802_4800, Op::Mfc2, "mfc2 v0, $9"),
            (0x4842_F800, Op::Cfc2, "cfc2 v0, $31"),
            (0x4882_4800, Op::Mtc2, "mtc2 v0, $9"),
            (0x48C2_F800, Op::Ctc2, "ctc2 v0, $31"),
            (0x4A18_0001, Op::Cop2, "cop2 0x0180001"),
            (0x8082_0004, Op::Lb, "lb v0, 4(a0)"),
            (0x8482_0004, Op::Lh, "lh v0, 4(a0)"),
            (0x8882_0007, Op::Lwl, "lwl v0, 7(a0)"),
            (0x8C82_0004, Op::Lw, "lw v0, 4(a0)"),
            (0x9082_0004, Op::Lbu, "lbu v0, 4(a0)"),
            (0x9482_0004, Op::Lhu, "lhu v0, 4(a0)"),
            (0x9882_0004, Op::Lwr, "lwr v0, 4(a0)"),
            (0xA082_0004, Op::Sb, "sb v0, 4(a0)"),
            (0xA482_0004, Op::Sh, "sh v0, 4(a0)"),
            (0xA882_0007, Op::Swl, "swl v0, 7(a0)"),
            (0xAC82_0004, Op::Sw, "sw v0, 4(a0)"),
            (0xB882_0004, Op::Swr, "swr v0, 4(a0)"),
            (0xC882_0004, Op::Lwc2, "lwc2 $2, 4(a0)"),
            (0xE882_0004, Op::Swc2, "swc2 $2, 4(a0)"),
        ];
        for &(word, op, text) in cases {
            let insn = decode(word);
            assert_eq!(insn.op, op, "{word:08X}");
            assert_eq!(disasm(&insn, 0x8001_0000), text, "{word:08X}");
        }
    }

    #[test]
    fn rejects_non_r3000_encodings() {
        for word in [
            0x4400_0000u32, // COP1
            0x4C00_0000,    // COP3
            0x5085_0003,    // beql
            0x0000_002C,    // dadd
            0xDC82_0000,    // ld
            0x0482_0003,    // regimm rt=2
        ] {
            assert_eq!(decode(word).op, Op::Invalid, "{word:08X}");
        }
    }

    #[test]
    fn register_sets() {
        let lw = decode(0x8C82_0004); // lw v0, 4(a0)
        assert_eq!(lw.reads(), 1 << 4);
        assert_eq!(lw.writes(), Some(2));
        let lwl = decode(0x8882_0007);
        assert_eq!(lwl.reads(), (1 << 4) | (1 << 2));
        let jal = decode(0x0C00_4004);
        assert_eq!(jal.writes(), Some(31));
        let nop = decode(0);
        assert_eq!(nop.writes(), None);
    }
}
