//! Chip memcard's cases: libmcx, the PocketStation library (D59, D65;
//! phase 1 task 9g).
//!
//! Both sides have the BIOS's cards: slot 1 a card (formatted; its FLAG
//! says new until a frame is written), slot 2 empty. There is no
//! PocketStation, so every command ends in a timeout at the vblank after
//! it starts: McxErrInvalid with a card in the slot (it acknowledged the
//! address), McxErrNoCard without. `McxExecFlag` sends a card's read
//! command, which the card does answer. The command functions and
//! `McxSync` are original code on both sides, over libmcx's state in
//! guest RAM; the adapters are `McxStartCom` and `_mcxDsrEntry`, which the
//! vblank handler calls for every transfer.
//!
//! Observation notes, beyond the module's specs:
//! - **Per vblank:** `McxSync(1, …)` and the command number and result it
//!   writes (sentinels before), as a game polls; where the guest resumes
//!   in the frame (root counter 1, in hblanks), which the wait before the
//!   address byte moves a quantum (D60); the interrupt mask.
//! - **The commands' buffers** (scratch RAM, seeded) after the transfer:
//!   a callback that wrote one would show.

use super::{Case, Harness, Rng, SCRATCH, call, observe_ram};

/// NTSC hblanks per frame.
const LINES_PER_FRAME: u32 = 263;

/// The commands' buffers, and `McxSync`'s out-parameters.
const BUF: u32 = SCRATCH;
const PARAM: u32 = SCRATCH + 0x100;
const DATA: u32 = SCRATCH + 0x200;
const CMD: u32 = SCRATCH + 0x300;
const RES: u32 = SCRATCH + 0x304;

/// A libmcx command: its name and seeded arguments after the port.
struct Command {
    name: &'static str,
    args: fn(&mut Rng) -> Vec<u32>,
}

const COMMANDS: &[Command] = &[
    Command { name: "McxCardType", args: |_| vec![] },
    Command { name: "McxGetApl", args: |_| vec![BUF] },
    Command { name: "McxExecApl", args: |r| vec![r.range(0, 0x12), r.next() as u32] },
    Command { name: "McxGetTime", args: |_| vec![BUF] },
    Command { name: "McxGetMem", args: |r| vec![BUF, r.range(0, 0x10) * 0x1000 + r.range(0, 0x100), r.range(1, 0x81)] },
    Command { name: "McxSetMem", args: |r| vec![BUF, r.range(0, 0x10) * 0x1000 + r.range(0, 0x100), r.range(1, 0x81)] },
    Command { name: "McxShowTrans", args: |r| vec![r.range(0, 3), r.range(0, 0x100)] },
    Command { name: "McxHideTrans", args: |_| vec![] },
    Command { name: "McxCurrCtrl", args: |r| vec![r.range(0, 4), r.range(0, 3), r.range(0, 0x100)] },
    Command { name: "McxSetLED", args: |r| vec![r.range(0, 3)] },
    Command { name: "McxGetSerial", args: |_| vec![BUF] },
    Command { name: "McxExecFlag", args: |r| vec![r.range(0, 0x11), r.range(0, 2)] },
    Command { name: "McxAllInfo", args: |_| vec![BUF] },
    Command { name: "McxFlashAcs", args: |r| vec![r.range(0, 3)] },
    Command { name: "McxReadDev", args: |r| vec![r.range(0, 4), PARAM, DATA] },
    Command { name: "McxWriteDev", args: |r| vec![r.range(0, 4), PARAM, DATA] },
];

/// Every function the cases call.
const ALL: &[&str] = &[
    "McxStartCom", "McxStopCom", "McxSync", "McxCardType", "McxGetApl", "McxExecApl", "McxGetTime", "McxGetMem", "McxSetMem", "McxShowTrans",
    "McxHideTrans", "McxCurrCtrl", "McxSetLED", "McxGetSerial", "McxExecFlag", "McxAllInfo", "McxFlashAcs", "McxReadDev", "McxWriteDev",
    "ResetCallback", "GetIntrMask", "GetRCnt", "VSync",
];

pub fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "memcard.start-com",
            chip: "memcard",
            level: 1,
            covers: &["McxStartCom", "_mcxDsrEntry"],
            needs: &["McxStartCom", "McxSync", "McxCardType", "ResetCallback", "SetIntrMask", "GetIntrMask", "GetRCnt"],
            run: start_com,
        },
        Case { name: "memcard.command", chip: "memcard", level: 1, covers: &["McxStartCom", "_mcxDsrEntry"], needs: ALL, run: command },
        Case { name: "memcard.session", chip: "memcard", level: 2, covers: &["McxStartCom", "_mcxDsrEntry"], needs: ALL, run: session },
    ]
}

/// A port argument: slot 1 (00h), slot 2 (10h), a sub-address on either
/// (a multitap's; nothing answers it), or anything up to FFh.
fn port_arg(rng: &mut Rng) -> u32 {
    match rng.range(0, 5) {
        0 => 0x00,
        1 => 0x10,
        2 => rng.range(1, 0x10),
        3 => 0x10 | rng.range(1, 0x10),
        _ => rng.range(0x20, 0x100),
    }
}

/// Where the guest resumes in the frame, and the interrupt mask.
fn place(h: &mut dyn Harness, what: &str) -> Result<(), String> {
    let v = h.call("GetRCnt", &[0xF200_0001])?;
    h.observe(&format!("{what}: hblank"), format!("{}", v % LINES_PER_FRAME));
    call(h, "GetIntrMask", &[])?;
    Ok(())
}

/// `McxSync(mode, …)` and what it wrote over the sentinels.
fn sync(h: &mut dyn Harness, what: &str, mode: u32) -> Result<u32, String> {
    h.write(CMD, &0xAAAA_AAAAu32.to_le_bytes());
    h.write(RES, &0x5555_5555u32.to_le_bytes());
    let v = h.call("McxSync", &[mode, CMD, RES])?;
    h.observe(&format!("{what}: McxSync({mode})"), format!("{v:#x}"));
    observe_ram(h, &format!("{what}: cmd, result"), CMD, 8);
    Ok(v)
}

/// Poll `McxSync(1)` once per vblank for `n` vblanks, as a game does.
fn poll(h: &mut dyn Harness, what: &str, n: u32) -> Result<(), String> {
    for f in 0..n {
        sync(h, &format!("{what} {f}"), 1)?;
        h.vblanks(1)?;
        place(h, &format!("{what} {f}"))?;
    }
    sync(h, &format!("{what} end"), 1)?;
    Ok(())
}

/// Seeded bytes in the commands' buffers.
fn fill(h: &mut dyn Harness, rng: &mut Rng) {
    for b in [BUF, PARAM, DATA] {
        let bytes: Vec<u8> = (0..0x100).map(|_| rng.range(0, 0x100) as u8).collect();
        h.write(b, &bytes);
    }
}

fn buffers(h: &mut dyn Harness, what: &str) {
    for (b, name) in [(BUF, "buffer"), (PARAM, "param"), (DATA, "data")] {
        observe_ram(h, &format!("{what}: {name}"), b, 0x90);
    }
}

fn start(h: &mut dyn Harness) -> Result<(), String> {
    h.call("ResetCallback", &[])?;
    call(h, "McxStartCom", &[])?;
    place(h, "after McxStartCom")
}

/// `McxStartCom`, once or twice, with vblank masked first or not; then a
/// card-type query to a seeded slot, polled.
fn start_com(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    h.call("ResetCallback", &[])?;
    if rng.range(0, 2) == 1 {
        h.call("SetIntrMask", &[0])?;
    }
    for i in 0..rng.range(1, 3) {
        call(h, "McxStartCom", &[])?;
        place(h, &format!("after McxStartCom {i}"))?;
        h.vblanks(rng.range(0, 2))?;
    }
    let port = [0x00, 0x10][rng.range(0, 2) as usize];
    sync(h, "nothing reserved", rng.range(0, 2))?;
    call(h, "McxCardType", &[port])?;
    poll(h, "McxCardType", 4)
}

/// One seeded command to a seeded port, polled to its end, its buffers;
/// another while it runs, refused; then `McxSync(0)` with nothing left.
fn command(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    fill(h, rng);
    start(h)?;
    h.vblanks(rng.range(0, 2))?;
    let c = &COMMANDS[rng.range(0, COMMANDS.len() as u32) as usize];
    let mut args = vec![port_arg(rng)];
    args.extend((c.args)(rng));
    call(h, c.name, &args)?;
    let other = &COMMANDS[rng.range(0, COMMANDS.len() as u32) as usize];
    let mut more = vec![0x00];
    more.extend((other.args)(rng));
    call(h, other.name, &more)?;
    poll(h, c.name, 5)?;
    buffers(h, c.name);
    sync(h, "after", 0)?;
    Ok(())
}

/// A session: nothing reserved; the card type of both slots, waited for
/// with `McxSync(0)` (the vblanks it took); every command to slot 1 and
/// slot 2 in turn; `McxStopCom` with a transfer running, which then never
/// ends, and `McxStartCom` again, which times it out; `McxExecFlag` on
/// the card, whose read the card answers.
fn session(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    fill(h, rng);
    start(h)?;
    sync(h, "nothing reserved", 1)?;
    sync(h, "nothing reserved", 0)?;
    for port in [0x00, 0x10] {
        call(h, "VSync", &[0xFFFF_FFFF])?;
        call(h, "McxCardType", &[port])?;
        sync(h, &format!("McxCardType({port:#x})"), 0)?;
        call(h, "VSync", &[0xFFFF_FFFF])?;
        place(h, "after McxSync(0)")?;
    }
    for c in COMMANDS {
        for port in [0x00, 0x10] {
            let mut args = vec![port];
            args.extend((c.args)(rng));
            call(h, c.name, &args)?;
            poll(h, &format!("{}({port:#x})", c.name), 3)?;
        }
    }
    buffers(h, "after every command");
    // McxStopCom with the address sent: the vblank element leaves the
    // chain, the /ACK element stays; nothing times the transfer out.
    call(h, "McxGetApl", &[0x00, BUF])?;
    h.vblanks(1)?;
    call(h, "McxStopCom", &[])?;
    place(h, "after McxStopCom")?;
    poll(h, "stopped", 3)?;
    call(h, "McxStartCom", &[])?;
    poll(h, "started again", 3)?;
    for block in [1, 15] {
        call(h, "McxExecFlag", &[0x00, block, 1])?;
        poll(h, &format!("McxExecFlag(0, {block}, 1)"), 4)?;
    }
    Ok(())
}
