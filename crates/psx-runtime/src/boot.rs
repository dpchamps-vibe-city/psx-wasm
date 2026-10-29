//! Boot (plans/M02 "Boot sequence"): read `SYSTEM.CNF` from the disc, load
//! the PS-X EXE it names into the window, zero BSS, set `sp` and `gp`, lay
//! out the kernel's thread tables (`TCB=`). The
//! caller then calls the entry point through the dispatch table.

use psx_bios::bios::HostDisc;
use psx_bios::{Machine, Sdk};
use psx_devices::cd;

#[derive(Debug, Clone)]
pub struct Boot {
    pub exe: String,
    pub pc0: u32,
    pub gp0: u32,
    pub t_addr: u32,
    pub t_size: u32,
    pub b_addr: u32,
    pub b_size: u32,
    pub sp: u32,
    pub events: u32,
    /// FNV-1a of the loaded text, for the host's independent check.
    pub text_hash: u64,
}

pub fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

fn le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// `KEY = VALUE` lines.
fn cnf(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k.trim().eq_ignore_ascii_case(key)).then(|| v.trim().split_whitespace().next().unwrap_or("").to_string())
    })
}

pub fn load<S: Sdk>(m: &mut Machine<S>) -> Result<Boot, String> {
    if !m.host.has_disc() {
        return Err("no disc".into());
    }
    let mut disc = HostDisc(&mut *m.host);
    let text = match cd::find(&mut disc, "SYSTEM.CNF;1") {
        Some(e) => String::from_utf8_lossy(&cd::read_file(&mut disc, &e).ok_or("SYSTEM.CNF unreadable")?).into_owned(),
        None => String::new(),
    };
    let exe = cnf(&text, "BOOT").unwrap_or_else(|| "cdrom:\\PSX.EXE;1".into());
    let stack = cnf(&text, "STACK").and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok());
    let events = cnf(&text, "EVENT").and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()).unwrap_or(16);
    let tcbs = cnf(&text, "TCB")
        .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(psx_bios::threads::DEFAULT_TCBS);
    let entry = cd::find(&mut disc, &exe).ok_or_else(|| format!("{exe} not found"))?;
    let file = cd::read_file(&mut disc, &entry).ok_or_else(|| format!("{exe} unreadable"))?;
    if file.len() < 0x800 || &file[..8] != b"PS-X EXE" {
        return Err(format!("{exe} is not a PS-X EXE"));
    }
    let (pc0, gp0, t_addr, t_size) = (le(&file, 0x10), le(&file, 0x14), le(&file, 0x18), le(&file, 0x1C));
    let (b_addr, b_size, s_addr, s_size) = (le(&file, 0x28), le(&file, 0x2C), le(&file, 0x30), le(&file, 0x34));
    let body = file.get(0x800..0x800 + t_size as usize).ok_or("EXE shorter than its header says")?;
    m.mem.write(t_addr, body);
    if b_size != 0 {
        m.mem.write(b_addr, &vec![0; b_size as usize]);
    }
    let sp = if s_addr != 0 { s_addr.wrapping_add(s_size) } else { stack.unwrap_or(0x801F_FF00) };
    m.regs.set_sp(sp);
    m.regs.set_gp(gp0);
    m.kernel.events = vec![Default::default(); events.clamp(1, 64) as usize];
    // The kernel's thread tables live in guest RAM (D57).
    m.lay_out_threads(tcbs);
    // The CD controller, for SDK code that drives the drive (D58).
    m.cdc_insert();
    Ok(Boot { exe, pc0, gp0, t_addr, t_size, b_addr, b_size, sp, events, text_hash: fnv(body) })
}
