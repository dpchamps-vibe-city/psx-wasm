//! Chip cd's cases: libcd's drive layer (`bios_1`, `bios_2`, `bios_3`) and
//! the streaming ring's interrupt (D61; phase 1 task 9d). The level-2
//! sequences run libcd's public layer (btn) and libds with the ring
//! (bof4), original code on both sides, over the drive layer.
//!
//! Observation notes, beyond the module's specs:
//! - **The disc is the game's**, on both sides; reads stay among its first
//!   2,000 sectors, which every disc has. The streaming cases play the
//!   disc's first movie ([`Harness::movie`]) and fall back to plain data
//!   sectors without one.
//! - **Handlers are addresses.** `CD_initintr` returns line 2's previous
//!   handler: libcd's own on the device, the adapters' stand-in natively.
//!   Observed as set or not.
//! - **Void functions** (`StCdInterrupt`) leave whatever was in v0: not
//!   observed.
//! - **Sector data** as an FNV-1a hash and its first bytes; timing as
//!   Vcount (`VSync(-1)`) at sync points; callbacks by their word.

use super::{Case, Harness, Rng, SCRATCH, call, observe_ram};

/// A command's result buffer.
const R: u32 = SCRATCH;
/// A command's parameters.
const P: u32 = SCRATCH + 0x10;
/// `StGetNext`'s out-parameters: the frame's data, its header.
const NEXT: u32 = SCRATCH + 0x30;
/// A `CdlFILE`.
const FILE: u32 = SCRATCH + 0x40;
const NAME: u32 = SCRATCH + 0x60;
/// Sector data, up to 8 sectors.
const BUF: u32 = SCRATCH + 0x100;
/// The streaming ring: up to 16 entries and their sectors.
const RING: u32 = SCRATCH + 0x8000;

/// libcd commands.
const NOP: u32 = 1;
const SETLOC: u32 = 2;
const READN: u32 = 6;
const STANDBY: u32 = 7;
const PAUSE: u32 = 9;
const SETMODE: u32 = 14;
const GETPARAM: u32 = 15;
const GETLOCP: u32 = 17;
const GETTN: u32 = 19;
const GETTD: u32 = 20;
const SEEKL: u32 = 21;
const READS: u32 = 27;

/// Drive modes: double speed, 2340-byte sectors, XA-ADPCM.
const SPEED: u8 = 0x80;
const SIZE: u8 = 0x20;
const XA: u8 = 0x40;

pub fn cases() -> Vec<Case> {
    const LOW: &[&str] = &["CD_init", "CD_cw", "CD_sync", "VSync"];
    vec![
        Case {
            name: "cd.init",
            chip: "cd",
            level: 1,
            covers: &["CD_init", "CD_initintr", "CD_flush", "CD_sync"],
            needs: &["CD_init", "CD_initintr", "CD_flush", "CD_cw", "CD_sync", "VSync"],
            run: init,
        },
        Case { name: "cd.commands", chip: "cd", level: 1, covers: &["CD_cw", "CD_sync"], needs: LOW, run: commands },
        Case {
            name: "cd.sectors",
            chip: "cd",
            level: 1,
            covers: &["CD_ready", "CD_getsector", "CD_getsector2", "CD_datasync"],
            needs: &["CD_init", "CD_cw", "CD_sync", "CD_ready", "CD_getsector", "CD_getsector2", "CD_datasync", "VSync"],
            run: sectors,
        },
        Case {
            name: "cd.volume",
            chip: "cd",
            level: 1,
            covers: &["CD_vol", "CD_initvol"],
            needs: &["CD_init", "CD_cw", "CD_sync", "CD_vol", "CD_initvol", "VSync"],
            run: volume,
        },
        Case {
            name: "cd.ring",
            chip: "cd",
            level: 1,
            covers: &["StCdInterrupt", "StUnSetRing"],
            needs: &[
                "CD_init", "CD_cw", "CD_sync", "VSync", "StSetRing", "StSetStream", "StCdInterrupt", "StGetNext", "StFreeRing", "StUnSetRing",
                "CdDataCallback", "data_ready_callback", "CD_ready",
            ],
            run: ring,
        },
        Case {
            name: "cd.files",
            chip: "cd",
            level: 2,
            covers: &["CD_init", "CD_cw", "CD_sync", "CD_ready", "CD_getsector", "CD_initvol", "CD_flush"],
            needs: &[
                "CdInit", "CdSearchFile", "CdControl", "CdControlB", "CdControlF", "CdRead", "CdReadSync", "CdSync", "CdSyncCallback",
                "CdReadyCallback", "CdStatus", "CdMode", "CdLastCom", "VSync",
            ],
            run: files,
        },
        Case {
            name: "cd.stream",
            chip: "cd",
            level: 2,
            covers: &["CD_init", "CD_cw", "CD_sync", "CD_ready", "CD_getsector", "StCdInterrupt", "StUnSetRing"],
            needs: &["DsInit", "DsRead2", "DsControl", "StSetRing", "StSetStream", "StGetNext", "StFreeRing", "StUnSetRing", "VSync"],
            run: stream,
        },
    ]
}

fn bcd(v: u32) -> u8 {
    ((v / 10) << 4 | v % 10) as u8
}

/// A `CdlLOC` for an LBA.
fn loc(lba: u32) -> [u8; 4] {
    let a = lba + 150;
    [bcd(a / 75 / 60), bcd(a / 75 % 60), bcd(a % 75), 0]
}

fn fnv(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &x| (h ^ x as u64).wrapping_mul(0x100_0000_01b3))
}

fn word(h: &mut dyn Harness, addr: u32) -> u32 {
    let b = h.read(addr, 4);
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn observe_word(h: &mut dyn Harness, what: &str, addr: u32) {
    let v = word(h, addr);
    h.observe(what, format!("{v:#010x}"));
}

/// `len` bytes at `addr`: their hash and the first sixteen.
fn data(h: &mut dyn Harness, what: &str, addr: u32, len: usize) {
    let b = h.read(addr, len);
    let head: String = b.iter().take(16).map(|x| format!("{x:02x}")).collect();
    h.observe(what, format!("{:016x} {head}", fnv(&b)));
}

fn vcount(h: &mut dyn Harness) -> Result<(), String> {
    let v = h.call("VSync", &[u32::MAX])?;
    h.observe("Vcount", v.to_string());
    Ok(())
}

fn callback(h: &mut dyn Harness) -> Result<(u32, u32), String> {
    h.callback().ok_or_else(|| "game.toml [conform] has no callback".to_string())
}

/// `CD_cw(com, params, R, async)`: its result and the result buffer.
fn cw(h: &mut dyn Harness, com: u32, params: Option<&[u8]>, nowait: bool) -> Result<u32, String> {
    let p = match params {
        Some(b) => {
            h.write(P, b);
            P
        }
        None => 0,
    };
    h.write(R, &[0; 8]);
    let v = call(h, "CD_cw", &[com, p, R, nowait as u32])?;
    observe_ram(h, "result", R, 8);
    Ok(v)
}

/// `CD_sync` or `CD_ready` (`name`) with a result buffer.
fn wait(h: &mut dyn Harness, name: &str, mode: u32) -> Result<u32, String> {
    h.write(R, &[0; 8]);
    let v = call(h, name, &[mode, R])?;
    observe_ram(h, "result", R, 8);
    Ok(v)
}

/// Init, the handler reinstalled, flushes; a Nop still completes.
fn init(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    let before = rng.range(0, 3);
    h.vblanks(before)?;
    call(h, "CD_init", &[])?;
    wait(h, "CD_sync", 1)?;
    vcount(h)?;
    call(h, "CD_flush", &[])?;
    wait(h, "CD_sync", 1)?;
    let prev = h.call("CD_initintr", &[])?;
    h.observe("CD_initintr() is a pointer", (prev != 0).to_string());
    cw(h, NOP, None, rng.range(0, 2) == 1)?;
    wait(h, "CD_sync", 0)?;
    call(h, "CD_flush", &[])?;
    wait(h, "CD_sync", 1)?;
    vcount(h)?;
    Ok(())
}

/// Commands with and without parameters, waited for or not; their
/// answers, the drive's position and timing.
fn commands(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    call(h, "CD_init", &[])?;
    for _ in 0..5 {
        let nowait = rng.range(0, 2) == 1;
        match rng.range(0, 10) {
            0 => cw(h, NOP, None, nowait)?,
            1 => cw(h, SETLOC, Some(&loc(rng.range(16, 2000))), nowait)?,
            2 => cw(h, SETMODE, Some(&[[0, SPEED, SIZE, SPEED | SIZE][rng.range(0, 4) as usize]]), nowait)?,
            3 => cw(h, GETPARAM, None, nowait)?,
            4 => cw(h, GETTN, None, nowait)?,
            5 => cw(h, GETTD, Some(&[1]), nowait)?,
            6 => cw(h, STANDBY, None, nowait)?,
            7 => cw(h, PAUSE, None, nowait)?,
            8 => {
                cw(h, SETLOC, Some(&loc(rng.range(16, 2000))), false)?;
                cw(h, SEEKL, None, nowait)?
            }
            _ => cw(h, GETLOCP, None, nowait)?,
        };
        if rng.range(0, 2) == 1 {
            wait(h, "CD_sync", 1)?;
        }
        wait(h, "CD_sync", 0)?;
        vcount(h)?;
    }
    // Setloc without its parameter.
    call(h, "CD_cw", &[SETLOC, 0, R, 0])?;
    Ok(())
}

/// A read: data-ready interrupts taken, sectors by DMA3 (whole, or a
/// header and the rest as CdRead takes them), DMA3's state, then a pause.
fn sectors(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    call(h, "CD_init", &[])?;
    let mode = [SPEED, SPEED | SIZE, 0, SIZE][rng.range(0, 4) as usize];
    let words = if mode & SIZE != 0 { 585 } else { 512 };
    cw(h, SETMODE, Some(&[mode]), false)?;
    cw(h, SETLOC, Some(&loc(rng.range(16, 1000))), false)?;
    cw(h, READN, None, false)?;
    for _ in 0..3 {
        wait(h, "CD_ready", 0)?;
        let f = if rng.range(0, 2) == 1 { "CD_getsector2" } else { "CD_getsector" };
        call(h, f, &[BUF, words])?;
        call(h, "CD_datasync", &[rng.range(0, 2)])?;
        data(h, "sector", BUF, 4 * words as usize);
        vcount(h)?;
    }
    wait(h, "CD_ready", 0)?;
    call(h, "CD_getsector", &[BUF, 3])?;
    call(h, "CD_getsector", &[BUF + 12, words - 3])?;
    data(h, "sector in two", BUF, 4 * words as usize);
    wait(h, "CD_ready", 1)?;
    cw(h, PAUSE, None, false)?;
    wait(h, "CD_sync", 0)?;
    wait(h, "CD_ready", 1)?;
    vcount(h)?;
    Ok(())
}

/// The CD's volumes; with a movie on the disc, its XA audio through the
/// matrix (the device effect: the SPU's output).
fn volume(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    call(h, "CD_init", &[])?;
    call(h, "CD_initvol", &[])?;
    for _ in 0..3 {
        let v: Vec<u8> = (0..4).map(|_| rng.range(0, 256) as u8).collect();
        h.write(P, &v);
        call(h, "CD_vol", &[P])?;
    }
    if let Some(movie) = h.movie() {
        cw(h, SETMODE, Some(&[SPEED | XA]), false)?;
        cw(h, SETLOC, Some(&loc(movie + rng.range(0, 32))), false)?;
        cw(h, READS, None, false)?;
        h.vblanks(30)?;
        cw(h, PAUSE, None, false)?;
        wait(h, "CD_sync", 0)?;
    }
    vcount(h)?;
    let d = h.device();
    h.observe("device", d);
    Ok(())
}

/// The frames the ring holds: each one's header, then freed.
fn drain_ring(h: &mut dyn Harness, what: &str) -> Result<(), String> {
    for _ in 0..16 {
        h.write(NEXT, &[0; 8]);
        let v = h.call("StGetNext", &[NEXT, NEXT + 4])?;
        h.observe(&format!("{what}: StGetNext"), v.to_string());
        if v != 0 {
            break;
        }
        let (addr, hdr) = (word(h, NEXT), word(h, NEXT + 4));
        h.observe(&format!("{what}: frame at"), format!("{:#x} {:#x}", addr.wrapping_sub(RING), hdr.wrapping_sub(RING)));
        data(h, &format!("{what}: header"), hdr, 32);
        call(h, "StFreeRing", &[addr])?;
    }
    Ok(())
}

/// `StCdInterrupt` on a stream in progress: for each sector (its
/// data-ready interrupt taken first, or once a frame with sectors missed:
/// frames out of sequence, restarts), frames into the ring, the frame
/// callback by DMA3's interrupt; then `StUnSetRing`.
fn ring(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    let (cb, state) = callback(h)?;
    let ready = h.addr("data_ready_callback").ok_or("data_ready_callback has no address")?;
    call(h, "CD_init", &[])?;
    let size = rng.range(4, 17);
    h.write(RING, &vec![0; 16 * 2048]);
    call(h, "StSetRing", &[RING, size])?;
    // From any frame on (no start frame), to no end.
    call(h, "StSetStream", &[0, 0, u32::MAX, cb, 0])?;
    call(h, "CdDataCallback", &[ready])?;
    cw(h, SETMODE, Some(&[SPEED | SIZE]), false)?;
    let lba = h.movie().map_or(rng.range(16, 1000), |m| m + rng.range(0, 64));
    cw(h, SETLOC, Some(&loc(lba)), false)?;
    cw(h, READN, None, false)?;
    // The first sector in the buffer: with none, the original waits for
    // data that never comes.
    wait(h, "CD_ready", 0)?;
    let every = rng.range(0, 2) == 1;
    for f in 0..if every { 40 } else { 12 } {
        if every {
            wait(h, "CD_ready", 0)?;
        } else {
            h.vblanks(1)?;
        }
        h.call("StCdInterrupt", &[])?;
        drain_ring(h, &format!("sector {f}"))?;
        observe_word(h, "frame callback", state);
    }
    data(h, "ring entries", RING, 32 * size as usize);
    call(h, "StUnSetRing", &[])?;
    call(h, "CdDataCallback", &[0])?;
    cw(h, PAUSE, None, false)?;
    wait(h, "CD_sync", 0)?;
    vcount(h)?;
    Ok(())
}

/// libcd's public layer over the drive layer: init, a file found and read,
/// a read of the volume descriptors, the sync callback, the data-ready
/// interface with its callback, and the accessors that read the drive
/// layer's state.
fn files(h: &mut dyn Harness, _rng: &mut Rng) -> Result<(), String> {
    let (cb, state) = callback(h)?;
    call(h, "CdInit", &[])?;
    for f in ["CdStatus", "CdMode", "CdLastCom"] {
        call(h, f, &[])?;
    }
    h.write(NAME, b"\\SYSTEM.CNF;1\0");
    call(h, "CdSearchFile", &[FILE, NAME])?;
    observe_ram(h, "CdlFILE", FILE, 24);
    call(h, "CdControl", &[SETLOC, FILE, R])?;
    observe_ram(h, "result", R, 8);
    call(h, "CdRead", &[1, BUF, SPEED as u32])?;
    let mut polls = 0;
    while h.call("CdReadSync", &[1, R])? != 0 && polls < 600 {
        h.vblanks(1)?;
        polls += 1;
    }
    h.observe("frames to read", polls.to_string());
    call(h, "CdReadSync", &[0, R])?;
    observe_ram(h, "result", R, 8);
    data(h, "SYSTEM.CNF", BUF, 2048);
    vcount(h)?;
    // The volume descriptors, eight sectors at once.
    h.write(P, &loc(16));
    call(h, "CdControl", &[SETLOC, P, R])?;
    call(h, "CdRead", &[8, BUF, SPEED as u32])?;
    call(h, "CdReadSync", &[0, R])?;
    data(h, "LBA 16-23", BUF, 8 * 2048);
    vcount(h)?;
    // A sync callback at Pause's completion.
    call(h, "CdSyncCallback", &[cb])?;
    call(h, "CdControlB", &[PAUSE, 0, R])?;
    observe_ram(h, "result", R, 8);
    h.vblanks(1)?;
    observe_word(h, "sync callback", state);
    call(h, "CdSyncCallback", &[0])?;
    // The data-ready interface: a callback per sector.
    h.write(P, &loc(16));
    call(h, "CdControl", &[SETLOC, P, R])?;
    call(h, "CdReadyCallback", &[cb])?;
    call(h, "CdControl", &[READN, 0, R])?;
    h.vblanks(5)?;
    observe_word(h, "ready callbacks", state);
    call(h, "CdControlF", &[PAUSE, 0])?;
    call(h, "CdSync", &[0, R])?;
    observe_ram(h, "result", R, 8);
    call(h, "CdReadyCallback", &[0])?;
    for f in ["CdStatus", "CdMode", "CdLastCom"] {
        call(h, f, &[])?;
    }
    vcount(h)?;
    Ok(())
}

/// libds and the ring as a game plays a movie: `DsInit`, the ring, a
/// stream from the disc's first movie with XA audio (`DsRead2`), its
/// frames taken as they come, then the ring unset and the drive paused.
fn stream(h: &mut dyn Harness, _rng: &mut Rng) -> Result<(), String> {
    let movie = h.movie().ok_or("the disc has no movie")?;
    let (cb, state) = callback(h)?;
    call(h, "DsInit", &[])?;
    call(h, "StSetRing", &[RING, 16])?;
    call(h, "StSetStream", &[0, 1, u32::MAX, cb, 0])?;
    h.write(P, &loc(movie));
    call(h, "DsRead2", &[P, 0x100 | (SPEED | SIZE | XA) as u32])?;
    for f in 0..60 {
        h.vblanks(1)?;
        drain_ring(h, &format!("frame {f}"))?;
        if f % 10 == 9 {
            observe_word(h, "frame callbacks", state);
            vcount(h)?;
        }
    }
    call(h, "StUnSetRing", &[])?;
    call(h, "DsControl", &[PAUSE, 0, R])?;
    observe_ram(h, "result", R, 8);
    h.vblanks(2)?;
    vcount(h)?;
    let d = h.device();
    h.observe("device", d);
    Ok(())
}
