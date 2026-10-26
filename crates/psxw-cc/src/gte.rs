//! The `asm-gte` rule (M4 task 12): PsyQ's GTE inline-asm macros
//! (`inline_c.h`, DMPSX version 3) become calls to the runtime's GTE
//! intrinsics, so the C that uses them compiles as C instead of being
//! lifted. A statement is rewritten only if every instruction in its
//! template is one this module models; anything else stays real asm and
//! its function is lifted, as before. `difftest` gates the result against
//! the original instructions.
//!
//! Modelled: the coprocessor moves (`lwc2`/`swc2`, `mtc2`/`mfc2`,
//! `ctc2`/`cfc2`); the CPU loads, stores and ALU operations the macros use
//! on scratch registers (`$12`..); GTE commands as `cop2 imm`, as gas
//! mnemonics (`rtpt`, `nclip`, ...), as real `cop2` words, and as DMPSX
//! placeholder words. DMPSX patched the placeholders (`(index << 6) | 3Fh`,
//! with MVMVA's fields at bits 18-26) into real commands after compiling.
//! The command encodings are psyq-decomp's `include/inline_a.inc`
//! (`fake << 20 | sf << 19 | mx << 17 | v << 15 | cv << 13 | lm << 10 |
//! op`, names and parameters from Sony's `INLINE_A.H`); the placeholder
//! indices are `inline_c.h`'s.

/// A GTE command word (the low 25 bits of `cop2`).
const fn cmd(fake: u32, op: u32, sf: u32, mx: u32, v: u32, cv: u32, lm: u32) -> u32 {
    fake << 20 | sf << 19 | mx << 17 | v << 15 | cv << 13 | lm << 10 | op
}

/// The named commands (`inline_a.inc`).
fn named(name: &str) -> Option<u32> {
    Some(match name {
        "rtps" => cmd(0x01, 0x01, 1, 0, 0, 0, 0),
        "rtpt" => cmd(0x02, 0x30, 1, 0, 0, 0, 0),
        "dcpl" => cmd(0x06, 0x29, 1, 0, 0, 0, 0),
        "dpcs" => cmd(0x07, 0x10, 1, 0, 0, 0, 0),
        "dpct" => cmd(0x0F, 0x2A, 1, 0, 0, 0, 0),
        "intpl" => cmd(0x09, 0x11, 1, 0, 0, 0, 0),
        "ncs" => cmd(0x0C, 0x1E, 1, 0, 0, 0, 1),
        "nct" => cmd(0x0D, 0x20, 1, 0, 0, 0, 1),
        "ncds" => cmd(0x0E, 0x13, 1, 0, 0, 0, 1),
        "ncdt" => cmd(0x0F, 0x16, 1, 0, 0, 0, 1),
        "nccs" => cmd(0x10, 0x1B, 1, 0, 0, 0, 1),
        "ncct" => cmd(0x11, 0x3F, 1, 0, 0, 0, 1),
        "cdp" => cmd(0x12, 0x14, 1, 0, 0, 0, 1),
        "cc" => cmd(0x13, 0x1C, 1, 0, 0, 0, 1),
        "nclip" => cmd(0x14, 0x06, 0, 0, 0, 0, 0),
        "avsz3" => cmd(0x15, 0x2D, 1, 0, 0, 0, 0),
        "avsz4" => cmd(0x16, 0x2E, 1, 0, 0, 0, 0),
        "sqr12" => cmd(0x0A, 0x28, 1, 0, 0, 0, 1),
        "sqr0" => cmd(0x0A, 0x28, 0, 0, 0, 0, 1),
        "op12" => cmd(0x17, 0x0C, 1, 0, 0, 0, 0),
        "op0" => cmd(0x17, 0x0C, 0, 0, 0, 0, 0),
        "gpf12" => cmd(0x19, 0x3D, 1, 0, 0, 0, 0),
        "gpf0" => cmd(0x19, 0x3D, 0, 0, 0, 0, 0),
        "gpl12" => cmd(0x1A, 0x3E, 1, 0, 0, 0, 0),
        "gpl0" => cmd(0x1A, 0x3E, 0, 0, 0, 0, 0),
        _ => return None,
    })
}

/// MVMVA with the rotation matrix (mx 0): `v` 0-2 = V0-V2, 3 = IR; `cv`
/// 0 = TR, 1 = BK, 3 = none.
const fn rt(v: u32, cv: u32) -> u32 {
    cmd(0x04, 0x12, 1, 0, v, cv, 0)
}

/// A DMPSX placeholder word's command. The light- and colour-matrix
/// families (indices 15h-32h) and the plain `rt`/`ll`/`lc` forms are left
/// out: their flags aren't pinned down by a source this crate trusts.
pub fn dmpsx(word: u32) -> Option<u32> {
    if word & 0x3F != 0x3F {
        return None;
    }
    let index = (word >> 6) & 0xFFF;
    let fields = word >> 18;
    if index == 0x4E {
        let f = |shift: u32, bits: u32| (word >> shift) & ((1 << bits) - 1);
        return Some(cmd(0x04, 0x12, f(25, 1), f(23, 2), f(21, 2), f(19, 2), f(18, 1)));
    }
    if fields != 0 {
        return None;
    }
    Some(match index {
        0x01 => named("rtps")?,
        0x02 => named("rtpt")?,
        0x04..=0x06 => rt(index - 0x04, 3),
        0x07 => rt(3, 3),
        0x09..=0x0C => rt(index - 0x09, 0),
        0x0D..=0x10 => rt(index - 0x0D, 1),
        0x37 => named("dcpl")?,
        0x38 => named("dpcs")?,
        0x39 => named("dpct")?,
        0x3A => named("intpl")?,
        0x3B => named("sqr12")?,
        0x3C => named("sqr0")?,
        0x3D => named("ncs")?,
        0x3E => named("nct")?,
        0x3F => named("ncds")?,
        0x40 => named("ncdt")?,
        0x41 => named("nccs")?,
        0x42 => named("ncct")?,
        0x43 => named("cdp")?,
        0x44 => named("cc")?,
        0x45 => named("nclip")?,
        0x46 => named("avsz3")?,
        0x47 => named("avsz4")?,
        0x48 => named("op12")?,
        0x49 => named("op0")?,
        0x4A => named("gpf12")?,
        0x4B => named("gpf0")?,
        0x4C => named("gpl12")?,
        0x4D => named("gpl0")?,
        _ => return None,
    })
}

/// A `.word`'s command: a placeholder, or a real `cop2` instruction.
fn word_command(w: u32) -> Option<u32> {
    if w >> 25 == 0x25 { Some(w & 0x01FF_FFFF) } else { dmpsx(w) }
}

#[derive(Debug, Clone)]
struct Operand {
    output: bool,
    expr: String,
}

/// Split `: outputs : inputs : clobbers` at top-level colons.
fn sections(rest: &str) -> Vec<&str> {
    let b = rest.as_bytes();
    let (mut out, mut depth, mut start, mut i) = (Vec::new(), 0i32, 0usize, 0usize);
    while i < b.len() {
        match b[i] {
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'(' => depth += 1,
            b')' => depth -= 1,
            b':' if depth == 0 => {
                out.push(&rest[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&rest[start..]);
    out
}

/// `"constraint"(expr), ...` in one section.
fn operands(section: &str, output: bool) -> Option<Vec<Operand>> {
    let mut out = Vec::new();
    let b = section.as_bytes();
    let mut i = 0;
    loop {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b',') {
            i += 1;
        }
        if i >= b.len() {
            return Some(out);
        }
        if b[i] != b'"' {
            return None;
        }
        let close = i + 1 + section[i + 1..].find('"')?;
        let constraint = &section[i + 1..close];
        let ok = if output { matches!(constraint, "=r" | "=&r") } else { matches!(constraint, "r" | "g" | "n" | "i") };
        if !ok {
            return None;
        }
        let mut j = close + 1;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        if b.get(j) != Some(&b'(') {
            return None;
        }
        let (mut depth, mut k) = (0, j);
        while k < b.len() {
            match b[k] {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            k += 1;
        }
        out.push(Operand { output, expr: section[j + 1..k].trim().to_string() });
        i = k + 1;
    }
}

/// A constant expression of the macros' kind: integers, parentheses,
/// `|`, `&`, `+`, `<<`, `>>`.
fn eval(s: &str) -> Option<u32> {
    let toks: Vec<String> = {
        let mut v = Vec::new();
        let b = s.as_bytes();
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            if c.is_ascii_whitespace() {
                i += 1;
            } else if c.is_ascii_digit() {
                let st = i;
                while i < b.len() && b[i].is_ascii_alphanumeric() {
                    i += 1;
                }
                v.push(s[st..i].to_string());
            } else if s[i..].starts_with("<<") || s[i..].starts_with(">>") {
                v.push(s[i..i + 2].to_string());
                i += 2;
            } else if b"()|&+".contains(&c) {
                v.push((c as char).to_string());
                i += 1;
            } else {
                return None;
            }
        }
        v
    };
    fn num(t: &str) -> Option<u32> {
        let t = t.trim_end_matches(['u', 'U', 'l', 'L']);
        match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
            Some(h) => u32::from_str_radix(h, 16).ok(),
            None => t.parse().ok(),
        }
    }
    // Precedence climbing: | < & < + < shifts.
    fn level(t: &[String], p: &mut usize, lv: usize) -> Option<u32> {
        const OPS: [&[&str]; 4] = [&["|"], &["&"], &["+"], &["<<", ">>"]];
        if lv == OPS.len() {
            let tok = t.get(*p)?;
            *p += 1;
            if tok == "(" {
                let v = level(t, p, 0)?;
                (t.get(*p)? == ")").then_some(())?;
                *p += 1;
                return Some(v);
            }
            return num(tok);
        }
        let mut v = level(t, p, lv + 1)?;
        while let Some(op) = t.get(*p).filter(|o| OPS[lv].contains(&o.as_str())).cloned() {
            *p += 1;
            let r = level(t, p, lv + 1)?;
            v = match op.as_str() {
                "|" => v | r,
                "&" => v & r,
                "+" => v.wrapping_add(r),
                "<<" => v.checked_shl(r)?,
                _ => v.checked_shr(r)?,
            };
        }
        Some(v)
    }
    let mut p = 0;
    let v = level(&toks, &mut p, 0)?;
    (p == toks.len()).then_some(v)
}

/// Lower one asm statement: its template (string contents, C escapes
/// still in place) and what follows it in the parentheses. `None` unless
/// every instruction is modelled.
pub fn lower(template: &str, rest: &str) -> Option<String> {
    let secs = sections(rest);
    if secs.len() > 4 || !secs.first().is_none_or(|s| s.trim().is_empty()) {
        return None;
    }
    let mut ops = operands(secs.get(1).copied().unwrap_or(""), true)?;
    ops.extend(operands(secs.get(2).copied().unwrap_or(""), false)?);
    let t = template.replace("\\n", ";").replace("\\t", " ");
    let insns: Vec<&str> = t.split(';').map(str::trim).filter(|s| !s.is_empty()).collect();
    if insns.is_empty() {
        return None;
    }
    let mut scratch = std::collections::BTreeSet::new();
    let mut body = Vec::new();
    let mut gte = false;
    // A CPU register operand: `%k` (an input value or output slot) or
    // `$n` (scratch, `$0` reads as zero).
    let cpu = |o: &str, scratch: &mut std::collections::BTreeSet<u32>| -> Option<String> {
        let o = o.trim();
        if let Some(k) = o.strip_prefix('%') {
            let k: usize = k.parse().ok()?;
            ops.get(k)?;
            return Some(format!("__op{k}"));
        }
        let n: u32 = o.strip_prefix('$')?.parse().ok().filter(|&n| n < 32)?;
        if n == 0 {
            return Some("0u".into());
        }
        scratch.insert(n);
        Some(format!("__r{n}"))
    };
    let c2 = |o: &str| -> Option<u32> { o.trim().strip_prefix('$')?.parse().ok().filter(|&n: &u32| n < 32) };
    // `off(base)` → C address expression.
    let mem = |o: &str, scratch: &mut std::collections::BTreeSet<u32>| -> Option<String> {
        let o = o.trim();
        let open = o.find('(')?;
        let off = o[..open].trim();
        let off: i64 = if off.is_empty() { 0 } else { off.parse().ok()? };
        let base = cpu(o[open + 1..o.rfind(')')?].trim(), scratch)?;
        Some(format!("({base} + (unsigned int){off})"))
    };
    for insn in insns {
        let (op, args) = insn.split_once(char::is_whitespace).map_or((insn, ""), |(a, b)| (a, b.trim()));
        let a: Vec<&str> = split_args(args);
        let line = match (op, a.as_slice()) {
            ("nop", []) => continue,
            ("lwc2", [r, m]) => format!("psx_gte_mtc2({}u, __psxw_lw({}));", c2(r)?, mem(m, &mut scratch)?),
            ("swc2", [r, m]) => format!("__psxw_sw({}, psx_gte_mfc2({}u));", mem(m, &mut scratch)?, c2(r)?),
            ("mtc2", [s, r]) => format!("psx_gte_mtc2({}u, {});", c2(r)?, cpu(s, &mut scratch)?),
            ("ctc2", [s, r]) => format!("psx_gte_ctc2({}u, {});", c2(r)?, cpu(s, &mut scratch)?),
            ("mfc2", [d, r]) => format!("{} = psx_gte_mfc2({}u);", cpu(d, &mut scratch)?, c2(r)?),
            ("cfc2", [d, r]) => format!("{} = psx_gte_cfc2({}u);", cpu(d, &mut scratch)?, c2(r)?),
            ("lw", [d, m]) => format!("{} = __psxw_lw({});", cpu(d, &mut scratch)?, mem(m, &mut scratch)?),
            ("lh", [d, m]) => format!("{} = (unsigned int)(int)(short)__psxw_lhu({});", cpu(d, &mut scratch)?, mem(m, &mut scratch)?),
            ("lhu", [d, m]) => format!("{} = __psxw_lhu({});", cpu(d, &mut scratch)?, mem(m, &mut scratch)?),
            ("lb", [d, m]) => format!("{} = (unsigned int)(int)(signed char)__psxw_lbu({});", cpu(d, &mut scratch)?, mem(m, &mut scratch)?),
            ("lbu", [d, m]) => format!("{} = __psxw_lbu({});", cpu(d, &mut scratch)?, mem(m, &mut scratch)?),
            ("sw", [s, m]) => format!("__psxw_sw({}, {});", mem(m, &mut scratch)?, cpu(s, &mut scratch)?),
            ("sh", [s, m]) => format!("__psxw_sh({}, {});", mem(m, &mut scratch)?, cpu(s, &mut scratch)?),
            ("sb", [s, m]) => format!("__psxw_sb({}, {});", mem(m, &mut scratch)?, cpu(s, &mut scratch)?),
            ("sll" | "srl" | "sra", [d, s, n]) => {
                let n: u32 = n.trim().parse().ok().filter(|&n| n < 32)?;
                let (d, s) = (cpu(d, &mut scratch)?, cpu(s, &mut scratch)?);
                match op {
                    "sll" => format!("{d} = {s} << {n};"),
                    "srl" => format!("{d} = {s} >> {n};"),
                    _ => format!("{d} = (unsigned int)((int){s} >> {n});"),
                }
            }
            ("addu" | "subu" | "and" | "or", [d, x, y]) => {
                let sym = match op {
                    "addu" => "+",
                    "subu" => "-",
                    "and" => "&",
                    _ => "|",
                };
                format!("{} = {} {sym} {};", cpu(d, &mut scratch)?, cpu(x, &mut scratch)?, cpu(y, &mut scratch)?)
            }
            ("addi" | "addiu", [d, s, n]) => {
                let n: i32 = n.trim().parse().ok()?;
                format!("{} = {} + (unsigned int){n};", cpu(d, &mut scratch)?, cpu(s, &mut scratch)?)
            }
            ("cop2", [n]) => {
                gte = true;
                format!("psx_gte_exec({:#x}u);", eval(n)? & 0x01FF_FFFF)
            }
            (".word", [w]) => {
                let w = w.trim();
                let value = match w.strip_prefix('%') {
                    Some(k) => eval(&ops.get(k.parse::<usize>().ok()?)?.expr)?,
                    None => eval(w)?,
                };
                gte = true;
                format!("psx_gte_exec({:#x}u);", word_command(value)?)
            }
            (name, []) => {
                gte = true;
                format!("psx_gte_exec({:#x}u);", named(name)?)
            }
            _ => return None,
        };
        body.push(line);
    }
    // Only templates that touch the GTE are this rule's.
    if !gte && !body.iter().any(|l| l.contains("psx_gte_")) {
        return None;
    }
    let mut out = String::from("do { ");
    for (k, o) in ops.iter().enumerate() {
        if o.output {
            out.push_str(&format!("unsigned int __op{k} = 0; "));
        } else {
            out.push_str(&format!("unsigned int __op{k} = (unsigned int)({}); ", o.expr));
        }
    }
    if !scratch.is_empty() {
        let regs: Vec<String> = scratch.iter().map(|n| format!("__r{n} = 0")).collect();
        out.push_str(&format!("unsigned int {}; ", regs.join(", ")));
    }
    for l in &body {
        out.push_str(l);
        out.push(' ');
    }
    for (k, o) in ops.iter().enumerate() {
        if o.output {
            out.push_str(&format!("({}) = __op{k}; ", o.expr));
        }
    }
    out.push_str("} while (0)");
    Some(out)
}

/// Instruction operands: commas outside parentheses.
fn split_args(s: &str) -> Vec<&str> {
    if s.trim().is_empty() {
        return vec![];
    }
    let (mut out, mut depth, mut start) = (Vec::new(), 0, 0);
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].trim());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_match_psyq_encodings() {
        assert_eq!(named("rtps"), Some(0x0018_0001));
        assert_eq!(named("rtpt"), Some(0x0028_0030));
        assert_eq!(named("nclip"), Some(0x0140_0006));
        assert_eq!(named("avsz3"), Some(0x0158_002D));
        assert_eq!(named("avsz4"), Some(0x0168_002E));
        assert_eq!(dmpsx(0x0000_007F), named("rtps"));
        assert_eq!(dmpsx(0x0000_117F), named("nclip"));
        // gte_mvmva(1, 0, 0, 3, 0): rotation matrix times V0, no vector.
        let w = 0x0000_13BF | 1 << 25 | 3 << 19;
        assert_eq!(dmpsx(w), Some(0x0048_6012));
        assert_eq!(dmpsx(0x0000_01FF), Some(rt(3, 3)), "rtir");
        assert_eq!(dmpsx(0x0000_027F), Some(rt(0, 0)), "rtv0tr");
        assert_eq!(dmpsx(0x0000_057F), None, "the light-matrix family stays lifted");
        assert_eq!(word_command(0x4A28_0030), Some(0x0028_0030), "a real cop2 word");
    }

    #[test]
    fn lowers_the_macro_shapes() {
        // gte_ldv0(p)
        let c = lower("lwc2\t$0, 0( %0 );lwc2\t$1, 4( %0 )", " : : \"r\"( p ) ").unwrap();
        assert_eq!(
            c,
            "do { unsigned int __op0 = (unsigned int)(p); psx_gte_mtc2(0u, __psxw_lw((__op0 + (unsigned int)0))); \
             psx_gte_mtc2(1u, __psxw_lw((__op0 + (unsigned int)4))); } while (0)"
        );
        // gte_SetRotMatrix(m): scratch registers.
        let c = lower("lw\t$12, 0( %0 );lw\t$13, 4( %0 );ctc2\t$12, $0;ctc2\t$13, $1", ": : \"r\"( m ) : \"$12\", \"$13\"").unwrap();
        assert!(c.contains("unsigned int __r12 = 0, __r13 = 0;"));
        assert!(c.contains("psx_gte_ctc2(1u, __r13);"));
        // gte_rtps(): hazard nops and a placeholder.
        assert_eq!(lower("nop;nop;.word 0x0000007f", "").unwrap(), "do { psx_gte_exec(0x180001u); } while (0)");
        // gte_mvmva(...) through the "g" operand.
        let c = lower("nop;nop;.word %0", ": : \"g\"( 0x000013bf | ((1)<<25) | ((0)<<23) | ((0)<<21) | ((3)<<19) | ((0)<<18) )").unwrap();
        assert!(c.contains("psx_gte_exec(0x486012u);"), "{c}");
        // gte_stflg(p)
        let c = lower("cfc2\t$12, $31;nop;sw\t$12, 0( %0 )", ": : \"r\"( p ) : \"$12\", \"memory\"").unwrap();
        assert!(c.contains("__r12 = psx_gte_cfc2(31u); __psxw_sw((__op0 + (unsigned int)0), __r12);"));
        // An output operand.
        let c = lower("mfc2 %0, $7", ": \"=r\"( z ) :").unwrap();
        assert!(c.ends_with("__op0 = psx_gte_mfc2(7u); (z) = __op0; } while (0)"), "{c}");
    }

    #[test]
    fn leaves_the_rest_alone() {
        assert_eq!(lower("lw %0,32($sp)", ": \"=r\"(x)"), None, "no GTE");
        assert_eq!(lower("nop;nop;.word 0x0000057f", ""), None, "unmapped placeholder");
        assert_eq!(lower("mult $2, $3", ""), None, "unmodelled instruction");
        assert_eq!(lower("lwc2 $0, 0(%0)", ": : \"m\"(x)"), None, "memory operand");
    }
}
