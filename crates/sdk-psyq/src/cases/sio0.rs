//! Chip sio0's cases: libpad's direct connection (D59; phase 1 task 9c).
//!
//! Both sides start with a digital pad in each port (the conformance
//! replay: port 1's buttons scripted from vblank 20, port 2 idle);
//! [`Harness::plug_port2`] takes port 2's away and back. The games'
//! buffers are scratch RAM.
//!
//! Observation notes, beyond the module's specs:
//! - **Per vblank:** both buffers (eight bytes: status, ID, the two button
//!   bytes, and what lies past them), `PadGetState` and the terminal type
//!   (`PadInfoMode` mode 1) of each port. The detection steps, the buffer
//!   bytes and which vblank's buttons they hold are the state machine D40
//!   kept libpad original for.
//! - **Nothing before `PadInitDirect`:** the original calls through
//!   function pointers it installs, so every case starts with it.
//! - **`PadSetAct` returns nothing:** v0 is libpad's record address on the
//!   original, so its result isn't observed.
//! - **The interrupt takes time.** The original busy-waits on SIO0 and
//!   root counter 2 inside the vblank interrupt; under the safepoint model
//!   each wait is a quantum, half a frame in all with two pads. Where the
//!   game resumes in the frame is observed in hblanks (exactly: both sides
//!   step time in the same quanta).

use super::{Case, Harness, Rng, SCRATCH, call, observe_ram};

/// NTSC hblanks per frame.
const LINES_PER_FRAME: u32 = 263;

/// The games' buffers: port 1's, port 2's 40h on.
const BUF: [u32; 2] = [SCRATCH, SCRATCH + 0x40];
/// Actuator tables.
const TABLE: u32 = SCRATCH + 0x100;
/// Vblanks a digital pad's detection takes (twelve config-mode attempts),
/// with a few to spare.
const DETECT: u32 = 16;

const INIT: &[&str] = &["PadInitDirect", "PadStartCom", "PadGetState", "PadInfoMode"];

pub fn cases() -> Vec<Case> {
    vec![
        Case { name: "sio0.init-direct", chip: "sio0", level: 1, covers: &["PadInitDirect"], needs: INIT, run: init_direct },
        Case {
            name: "sio0.start-com",
            chip: "sio0",
            level: 1,
            covers: &["PadStartCom"],
            needs: &["PadInitDirect", "PadStartCom", "PadGetState", "PadInfoMode", "GetIntrMask"],
            run: start_com,
        },
        Case {
            name: "sio0.stop-com",
            chip: "sio0",
            level: 1,
            covers: &["PadStopCom"],
            needs: &["PadInitDirect", "PadStartCom", "PadStopCom", "PadGetState", "PadInfoMode", "GetIntrMask"],
            run: stop_com,
        },
        Case { name: "sio0.get-state", chip: "sio0", level: 1, covers: &["PadGetState"], needs: INIT, run: get_state },
        Case { name: "sio0.info-mode", chip: "sio0", level: 1, covers: &["PadInfoMode"], needs: INIT, run: info_mode },
        Case {
            name: "sio0.set-act",
            chip: "sio0",
            level: 1,
            covers: &["PadSetAct"],
            needs: &["PadInitDirect", "PadStartCom", "PadGetState", "PadInfoMode", "PadSetAct"],
            run: set_act,
        },
        Case {
            name: "sio0.set-act-align",
            chip: "sio0",
            level: 1,
            covers: &["PadSetActAlign"],
            needs: &["PadInitDirect", "PadStartCom", "PadGetState", "PadInfoMode", "PadSetActAlign"],
            run: set_act_align,
        },
        Case { name: "sio0.port2", chip: "sio0", level: 1, covers: &["PadGetState", "PadInfoMode"], needs: INIT, run: port2 },
        Case {
            name: "sio0.sequence",
            chip: "sio0",
            level: 2,
            covers: &["PadInitDirect", "PadStartCom", "PadStopCom", "PadGetState", "PadInfoMode", "PadSetAct", "PadSetActAlign"],
            needs: &[
                "PadInitDirect", "PadStartCom", "PadStopCom", "PadGetState", "PadInfoMode", "PadSetAct", "PadSetActAlign", "GetIntrMask",
            ],
            run: sequence,
        },
        Case {
            name: "sio0.interrupt-time",
            chip: "sio0",
            level: 2,
            covers: &["PadInitDirect", "PadStartCom"],
            needs: &["PadInitDirect", "PadStartCom", "PadGetState", "PadInfoMode", "ResetCallback", "GetRCnt"],
            run: interrupt_time,
        },
    ]
}

/// A port argument: port 1 is 00h-0Fh, port 2 anything above.
fn port_arg(rng: &mut Rng, port: usize) -> u32 {
    if port == 0 { rng.range(0, 0x10) } else { [0x10, 0x10 | rng.range(0, 0x10), rng.range(0x10, 0x100)][rng.range(0, 3) as usize] }
}

fn init(h: &mut dyn Harness, bufs: [u32; 2]) -> Result<(), String> {
    call(h, "PadInitDirect", &[bufs[0], bufs[1]])?;
    observe_ram(h, "buffers after PadInitDirect", bufs[0], 8);
    observe_ram(h, "port 2's", bufs[1], 8);
    Ok(())
}

/// What a game reads of both ports: the buffers, the state, the type.
fn look(h: &mut dyn Harness, what: &str, bufs: [u32; 2]) -> Result<(), String> {
    for (i, &b) in bufs.iter().enumerate() {
        observe_ram(h, &format!("{what}: port {} buffer", i + 1), b, 8);
        call(h, "PadGetState", &[[0x00, 0x10][i]])?;
        call(h, "PadInfoMode", &[[0x00, 0x10][i], 1, 0])?;
    }
    Ok(())
}

/// `n` vblanks, looking after each.
fn frames(h: &mut dyn Harness, what: &str, bufs: [u32; 2], n: u32) -> Result<(), String> {
    for f in 0..n {
        h.vblanks(1)?;
        look(h, &format!("{what} {f}"), bufs)?;
    }
    Ok(())
}

/// Fill the buffers with seeded bytes.
fn fill(h: &mut dyn Harness, rng: &mut Rng, bufs: [u32; 2]) {
    for b in bufs {
        let bytes: Vec<u8> = (0..0x40).map(|_| rng.range(0, 0x100) as u8).collect();
        h.write(b, &bytes);
    }
}

fn init_direct(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    fill(h, rng, BUF);
    init(h, BUF)?;
    look(h, "before PadStartCom", BUF)?;
    // Without PadStartCom nothing polls.
    h.vblanks(rng.range(1, 4))?;
    look(h, "vblanks later", BUF)?;
    // Again, the ports' buffers elsewhere.
    let other = [BUF[1] + 0x10, BUF[0] + 0x10];
    fill(h, rng, other);
    init(h, other)?;
    call(h, "PadStartCom", &[])?;
    frames(h, "started", other, 3)?;
    // Once polling: fresh records, the reply in flight lands in the new
    // buffers.
    call(h, "PadInitDirect", &[BUF[0], BUF[1]])?;
    look(h, "restarted", BUF)?;
    frames(h, "restarted", BUF, rng.range(2, DETECT))?;
    Ok(())
}

fn start_com(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    init(h, BUF)?;
    h.vblanks(rng.range(0, 3))?;
    call(h, "GetIntrMask", &[])?;
    call(h, "PadStartCom", &[])?;
    call(h, "GetIntrMask", &[])?;
    frames(h, "frame", BUF, rng.range(2, DETECT + 4))?;
    // Again: both ports are found afresh.
    call(h, "PadStartCom", &[])?;
    look(h, "started again", BUF)?;
    frames(h, "again", BUF, DETECT)?;
    Ok(())
}

fn stop_com(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    init(h, BUF)?;
    call(h, "PadStartCom", &[])?;
    h.vblanks(rng.range(1, DETECT + 2))?;
    look(h, "polling", BUF)?;
    call(h, "PadStopCom", &[])?;
    call(h, "GetIntrMask", &[])?;
    // Stopped: the buffers keep what the game writes.
    let marks = [rng.range(0, 0x100) as u8, rng.range(0, 0x100) as u8];
    h.write(BUF[0], &marks);
    h.vblanks(rng.range(1, 4))?;
    look(h, "stopped", BUF)?;
    call(h, "PadStopCom", &[])?;
    call(h, "PadStartCom", &[])?;
    frames(h, "restarted", BUF, 4)?;
    Ok(())
}

fn get_state(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    init(h, BUF)?;
    call(h, "PadStartCom", &[])?;
    for f in 0..DETECT + 2 {
        h.vblanks(1)?;
        for port in [0, 1, 0, 1] {
            let p = port_arg(rng, port);
            call(h, "PadGetState", &[p])?;
        }
        // A status byte the game writes reads as a command in flight.
        if f == DETECT / 2 || f == DETECT + 1 {
            let port = rng.range(0, 2) as usize;
            let b = [0, 1, 0xFF, rng.range(0, 0x100)][rng.range(0, 4) as usize] as u8;
            h.write(BUF[port], &[b]);
            call(h, "PadGetState", &[port_arg(rng, port)])?;
        }
    }
    Ok(())
}

fn info_modes(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    for port in [0, 1] {
        let p = port_arg(rng, port);
        let mode = [0, 1, 2, 3, 4, 4, 5, 100, rng.range(0, 0x200)][rng.range(0, 9) as usize];
        let id = [u32::MAX, 0, 1, rng.range(0, 0x100), rng.range(0x8000_0000, u32::MAX)][rng.range(0, 5) as usize];
        call(h, "PadInfoMode", &[p, mode, id])?;
        call(h, "PadInfoMode", &[p, 100, 0])?;
    }
    Ok(())
}

fn info_mode(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    init(h, BUF)?;
    info_modes(h, rng)?;
    call(h, "PadStartCom", &[])?;
    for _ in 0..DETECT + 2 {
        h.vblanks(1)?;
        info_modes(h, rng)?;
    }
    Ok(())
}

/// A seeded actuator table: `n` bytes at [`TABLE`].
fn table(h: &mut dyn Harness, rng: &mut Rng, n: usize) -> u32 {
    let bytes: Vec<u8> = (0..n).map(|_| [0x00, 0x40, 0x41, 0xFF, rng.range(0, 0x100)][rng.range(0, 5) as usize] as u8).collect();
    h.write(TABLE, &bytes);
    TABLE
}

/// `PadSetAct`, its result (the original's record address) unobserved.
fn set_act_call(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    let port = rng.range(0, 2) as usize;
    let n = rng.range(0, 8);
    let t = table(h, rng, 8);
    h.call("PadSetAct", &[port_arg(rng, port), t, n])?;
    h.observe("PadSetAct", format!("port {}, {n} bytes", port + 1));
    Ok(())
}

fn set_act(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    init(h, BUF)?;
    set_act_call(h, rng)?;
    call(h, "PadStartCom", &[])?;
    frames(h, "frame", BUF, rng.range(1, DETECT + 2))?;
    set_act_call(h, rng)?;
    set_act_call(h, rng)?;
    frames(h, "after", BUF, 4)?;
    Ok(())
}

fn set_act_align(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    init(h, BUF)?;
    let port = rng.range(0, 2) as usize;
    let t = table(h, rng, 6);
    call(h, "PadSetActAlign", &[port_arg(rng, port), t])?;
    call(h, "PadStartCom", &[])?;
    for f in 0..DETECT + 2 {
        h.vblanks(1)?;
        if f % 4 == rng.range(0, 4) {
            let port = rng.range(0, 2) as usize;
            call(h, "PadSetActAlign", &[port_arg(rng, port), t])?;
            look(h, &format!("frame {f}"), BUF)?;
        }
    }
    Ok(())
}

/// Port 2 empty from the start, then plugged in, then pulled.
fn port2(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    h.plug_port2(false)?;
    init(h, BUF)?;
    call(h, "PadStartCom", &[])?;
    frames(h, "port 2 empty", BUF, rng.range(2, DETECT + 4))?;
    call(h, "PadInfoMode", &[0x10, 100, 0])?;
    h.plug_port2(true)?;
    frames(h, "plugged", BUF, DETECT + 2)?;
    // Pulled: eleven vblanks with the last buttons, then nothing.
    h.plug_port2(false)?;
    frames(h, "pulled", BUF, 14)?;
    call(h, "PadInfoMode", &[0x10, 100, 0])?;
    Ok(())
}

/// A game's session: both pads found, port 1's buttons through the
/// scripted presses, actuator calls on the way, port 2 pulled and plugged
/// back, communication stopped, the buffers moved, started again.
fn sequence(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    init(h, BUF)?;
    call(h, "PadStartCom", &[])?;
    call(h, "GetIntrMask", &[])?;
    frames(h, "detect", BUF, 18)?;
    let t = table(h, rng, 8);
    h.call("PadSetAct", &[0, t, 2])?;
    call(h, "PadSetActAlign", &[0, t])?;
    // The presses: START from vblank 20, CROSS from 30, UP+CIRCLE from 40.
    frames(h, "presses", BUF, 26)?;
    h.plug_port2(false)?;
    frames(h, "port 2 pulled", BUF, 14)?;
    for port in [0x00, 0x10] {
        call(h, "PadInfoMode", &[port, 100, 0])?;
        call(h, "PadInfoMode", &[port, 2, 0])?;
        call(h, "PadInfoMode", &[port, 4, u32::MAX])?;
    }
    h.plug_port2(true)?;
    frames(h, "port 2 back", BUF, 16)?;
    call(h, "PadStopCom", &[])?;
    h.vblanks(2)?;
    look(h, "stopped", BUF)?;
    let moved = [BUF[0] + 0x20, BUF[1] + 0x20];
    init(h, moved)?;
    call(h, "PadStartCom", &[])?;
    frames(h, "moved", moved, 18)?;
    observe_ram(h, "old buffers", BUF[0], 0x60);
    Ok(())
}

/// Where in the frame the game resumes after each vblank's interrupt:
/// the engine's busy-waits take the time they take the original
/// (root counter 1 counting hblanks, after `ResetCallback`). Two pads
/// while they are found and once stable, then port 2 empty.
fn interrupt_time(h: &mut dyn Harness, _rng: &mut Rng) -> Result<(), String> {
    let line = |h: &mut dyn Harness, what: &str| -> Result<(), String> {
        let v = h.call("GetRCnt", &[0xF200_0001])?;
        h.observe(what, format!("hblank {}", v % LINES_PER_FRAME));
        Ok(())
    };
    // Root counter 1 counts hblanks; its result, libetc's context, isn't observed.
    h.call("ResetCallback", &[])?;
    init(h, BUF)?;
    h.vblanks(1)?;
    line(h, "before PadStartCom")?;
    call(h, "PadStartCom", &[])?;
    for f in 0..DETECT + 2 {
        h.vblanks(1)?;
        line(h, &format!("frame {f}"))?;
    }
    h.plug_port2(false)?;
    for f in 0..4 {
        h.vblanks(1)?;
        line(h, &format!("port 2 empty {f}"))?;
    }
    Ok(())
}
