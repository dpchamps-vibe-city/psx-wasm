//! libpad (chip sio0, D59, D60): direct connection (`PadInitDirect`), its
//! vblank engine (`pdresres.o`, `pddirres.o`) and the calls games make
//! into it, as adapters.
//!
//! **What games observe is libpad's own state machine** (D40), so the
//! adapters run it, from PsyQ 4.7's objects (`psxw sdk disasm`; psyq-decomp
//! `libpad/{pddirini,pddirres,pdresres,pdmaiini,pdent2,pdent3,pdcmd1,
//! pdcmd2}`), with each port's bytes answered by a pad model instead of
//! SIO0's registers:
//! - **`PadStartCom` installs a `SysEnqIntRP` element** at priority 2, as
//!   the original installs `_padVsyncParam`. The kernel runs it in its
//!   place in the chains ([`psx_bios::Sdk::chain_element`]): when vblank is
//!   unmasked and pending (`_padIsVsync`), one exchange per port, port 1
//!   first (`_padIntPad`). The pads' buttons are latched at the vblank, as
//!   SIO0's are.
//! - **An exchange** is libpad's five handler states (select 01h, the
//!   command, the TAP byte, then the data bytes), each byte needing the
//!   pad's acknowledge for the next. Nothing connected acknowledges
//!   nothing; a digital pad acknowledges a poll (42h) through its last
//!   button byte and nothing else ([`Wire`]).
//! - **Replies are taken a port late.** A completed exchange's reply is
//!   processed (`_padFuncRecvAuto`: the buffer, the terminal type, the
//!   detection steps) while the other port sends its first data byte, or
//!   else when its own port's turn comes at the next vblank. So with two
//!   pads port 1's buffer is that vblank's and port 2's the one before;
//!   with port 2 empty, port 1's is the one before too.
//! - **Detection.** A pad's first reply sets PadGetState's state to
//!   FindPad and asks it for config mode (43h) every vblank; a digital pad
//!   never acknowledges that, and after the twelfth failure the state is
//!   FindCTP1, its stable state. Meanwhile its buffer keeps the first
//!   reply. PadGetState reports a state with a command in flight, or a
//!   buffer whose status byte isn't 0, as the step before it.
//! - **Failures** (nothing acknowledged in time) count for `PadInfoMode`'s
//!   mode 100; a port with nothing found writes `FF 00` to its buffer
//!   every vblank, one whose pad went away keeps its buffer for eleven
//!   more vblanks first.
//! - **The interrupt takes the original's time** (D60). Its busy-waits on
//!   SIO0 and root counter 2 each end at the next safepoint, a quantum
//!   later ([`Spin`]): seven for a poll, five for a refused command, three
//!   for an empty port, so half a frame with two pads. The adapters spend
//!   them in the exception, delivering nothing, and the guest resumes
//!   with its safepoint budget started over, as after the original's last
//!   wait. Games see it: bof4's `title-to-field` falls hundreds of frames
//!   behind the device without either.
//!
//! **State.** The port records, receive buffers and pending results are
//! here, in the runtime: only libpad's own code reads them (`psxw sdk
//! scan`: PadInitDirect, the one state accessor games call, is adapted
//! too). The games' buffers are guest RAM, written as the original writes
//! them; PadGetState reads the status byte back from there. `PadStartCom`
//! acknowledges and unmasks vblank in the interrupt controller, which the
//! chips share.
//!
//! **What isn't reproduced.** The pad model is a digital pad, as SIO0's
//! device is (`psx_devices::sio`): no analog or DualShock modes, so
//! libpad's extended commands (config mode, actuator alignment, the info
//! tables) are never acknowledged on either side, and `PadSetActAlign`
//! always reports the engine busy. The bytes libpad transmits after the
//! command (actuator levels, `PadSetAct`) aren't computed: a digital pad
//! ignores them. Multitap and light-gun paths aren't linked in these
//! games. `PadSetAct`'s return value, the record's address, is 0. The
//! waits' times assume the safepoint quantum exceeds the /ACK pulse (a
//! wait for /ACK to drop, which the original makes after each byte, is
//! never long under the default 20,000 cycles).

use crate::{Adapter, M, Result, reg, ret, set_reg};

pub const ADAPTERS: &[(&str, Adapter)] = &[
    ("PadInitDirect", pad_init_direct),
    ("PadStartCom", pad_start_com),
    ("PadStopCom", pad_stop_com),
    ("PadGetState", pad_get_state),
    ("PadInfoMode", pad_info_mode),
    ("PadSetAct", pad_set_act),
    ("PadSetActAlign", pad_set_act_align),
];

/// libpad's `SysEnqIntRP` element (`_padVsyncParam`). The adapters own it:
/// an address in the BIOS ROM's range, which no game code or data has.
pub const ELEMENT: u32 = 0xBFC7_0100;
/// The exception step that runs it ([`irq_step`]).
pub const STEP: u32 = 0x5000_0000;

const I_STAT: u32 = psx_devices::irq::I_STAT;
const I_MASK: u32 = psx_devices::irq::I_MASK;
/// The kernel's chain priority libpad uses.
const PRIORITY: u32 = 2;
/// Bytes of one port's receive buffer: the ID, 5Ah and up to 32 data
/// bytes.
const RX: usize = 0x23;

/// PadGetState's states.
pub const STATE_DISCON: u8 = 0;
pub const STATE_FIND_PAD: u8 = 1;
pub const STATE_FIND_CTP1: u8 = 2;
pub const STATE_FIND_CTP2: u8 = 3;
pub const STATE_REQ_INFO: u8 = 4;
pub const STATE_STABLE: u8 = 6;

/// The engine's command states: detect a pad, ask it for config mode,
/// leave config mode, done. The others take a pad that answers extended
/// commands.
const CMD_DETECT: u8 = 0;
const CMD_CONFIG: u8 = 1;
const CMD_CONFIG_OFF: u8 = 0xFE;
const CMD_DONE: u8 = 0xFF;
/// The commands: poll, config mode, the info and actuator requests.
const POLL: u8 = 0x42;
const CONFIG: u8 = 0x43;
/// Failures a command state takes before it gives up.
const RETRIES: u8 = 0xB;
/// A reply's ID in config mode.
const ID_CONFIG: u8 = 0xF3;

/// One port's record (a `PadInfo2`'s fields direct connection uses).
/// `PadInitDirect` clears it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Port {
    /// The game's buffer: status (0 or FFh), ID, then the data bytes.
    pub buf: u32,
    pub state: u8,
    pub cmd_state: u8,
    /// The command for the next exchange (0: poll), and the one the last
    /// exchange sent.
    pub request: u8,
    pub current: u8,
    /// Failures in a row in the current command state.
    pub fails: u8,
    /// The terminal type, the ID's high nibble (4: digital pad).
    pub kind: u8,
    pub ex_id: u16,
    pub id_count: u8,
    pub prev_mode: u8,
    /// Failed exchanges (`PadInfoMode` mode 100).
    pub failures: u32,
    /// `PadSetAct`'s table and size.
    pub act: u32,
    pub act_size: u8,
    /// `PadSetActAlign`'s table, while its command is the engine's.
    pub align: Option<u32>,
    /// Reply bytes the last exchange stored.
    pub rx_count: u8,
}

/// libpad's state.
#[derive(Debug, Clone)]
pub struct Pad {
    pub ports: [Port; 2],
    /// `PadInitDirect` has run: the engine has its records.
    pub direct: bool,
    /// Each port's receive buffer, from the ID on. `PadInitDirect`
    /// leaves them, as the original's live outside the records.
    pub rx: [[u8; RX]; 2],
    /// An exchange completed and its reply awaits processing
    /// (`_padFixResult` 0). `PadInitDirect` leaves these too.
    pub pending: [bool; 2],
    /// Each slot's buttons, latched at the last vblank (`None`: nothing
    /// connected).
    pub latched: [Option<[u8; 2]>; 2],
}

impl Default for Pad {
    fn default() -> Self {
        Pad { ports: Default::default(), direct: false, rx: [[0; RX]; 2], pending: [false; 2], latched: [None; 2] }
    }
}

/// `_padFuncPort2Info`: ports 00h-0Fh are port 1, any other port 2.
fn index(port: u32) -> usize {
    (port & 0xF0 != 0) as usize
}

/// A BIOS call that returns at once.
fn bios(m: &mut M, table: u32, func: u32, a: [u32; 4]) -> std::result::Result<(), String> {
    psx_bios::bios::call(m, table, func, a).map(|_| ())
}

// --- The vblank engine -----------------------------------------------------

/// Whether libpad's element is in the kernel's chain.
fn installed(m: &M) -> bool {
    m.kernel.int_handlers.get(&PRIORITY).is_some_and(|c| c.contains(&ELEMENT))
}

/// At each vblank, before the interrupt: the pads latch their buttons,
/// as SIO0's do (`psx_bios::io`), once there are records to poll for.
pub fn on_vblank(m: &mut M) {
    if !m.sdk.pad.direct || !installed(m) {
        return;
    }
    for p in 0..2 {
        let st = m.host.pad_state(p as u32);
        m.sdk.pad.latched[p] = (st.len() >= 2).then(|| [st[0], st[1]]);
    }
}

/// The element's verifier (`_padIsVsync`: vblank unmasked and pending)
/// and handler (`_padIntPad`): each port in turn, its pending reply
/// processed (`_padInitSioMode`), then its exchange. The handler's
/// busy-waits take the time they take the original ([`Spin`]): it passes
/// in the exception, delivering nothing, as at the original's safepoints.
pub fn irq_step(m: &mut M, _step: u32) {
    let vblank = |m: &mut M, r: u32| reg(m, r, 4).is_ok_and(|v| v & 1 != 0);
    if !vblank(m, I_MASK) || !vblank(m, I_STAT) || !m.sdk.pad.direct {
        return;
    }
    let mut spin = Spin { quantum: crate::libetc::quantum(m), now: 0 };
    for chan in 0..2 {
        take_pending(m, chan);
        match exchange(m, chan, &mut spin) {
            Ok(()) => m.sdk.pad.pending[chan] = true,
            Err(_) => {
                fail(m, chan);
                clear_command(&mut m.sdk.pad.ports[chan]);
            }
        }
    }
    if spin.now > 0 {
        let until = m.clock.now + spin.now;
        m.clock.advance_to(until);
        // The guest resumes with its safepoint budget as the original's
        // last safepoint left it: full. Left as it was, the game would get
        // less (or more) done before its next quantum than on the device.
        m.regs.reset_budget();
    }
}

/// SIO0 at libpad's rate (baud 88h, factor 1): cycles a byte takes, and
/// from its end to the pad's /ACK.
const BYTE: u64 = 8 * 0x88;
const ACK_DELAY: u64 = 0x40;
/// libpad's waits on root counter 2 (`_padSetRC2wait`; ticks of 8
/// cycles): before select, before the command, for an /ACK after reading
/// a reply (`_padSioRW2`, counted from the reply; `_padSioRW`, from before
/// it).
const SELECT_WAIT: u64 = 0x91 * 8;
const COMMAND_WAIT: u64 = 0x3C * 8;
const ACK_WAIT: u64 = 0x190 * 8;
const DATA_ACK_WAIT: u64 = 0x1AE * 8;

/// libpad's busy-waits as the runtime runs a guest loop: virtual time
/// only moves at its safepoints, a quantum at a time (§6), so a wait ends
/// at the first safepoint at or after what it waits for. `now` counts
/// from the handler's start.
pub struct Spin {
    quantum: u64,
    now: u64,
}

impl Spin {
    fn until(&mut self, t: u64) {
        if t > self.now {
            self.now += (t - self.now).div_ceil(self.quantum.max(1)) * self.quantum.max(1);
        }
    }
}

/// What a slot's device answers, a byte at a time: a digital pad
/// (`psx_devices::sio`'s, Beetle's `InputDevice_Gamepad`), or nothing.
pub struct Wire {
    pad: Option<[u8; 2]>,
    n: usize,
    selected: bool,
    polled: bool,
}

impl Wire {
    pub fn new(pad: Option<[u8; 2]>) -> Self {
        Wire { pad, n: 0, selected: false, polled: false }
    }

    /// The reply to `tx`, and whether the device acknowledges it (has
    /// more to send).
    pub fn byte(&mut self, tx: u8) -> (u8, bool) {
        let n = self.n;
        self.n += 1;
        let Some(b) = self.pad else { return (0xFF, false) };
        match n {
            0 => {
                self.selected = tx == 0x01;
                (0xFF, self.selected)
            }
            1 if self.selected => {
                self.polled = tx == POLL;
                (0x41, self.polled)
            }
            2 if self.polled => (0x5A, true),
            3 if self.polled => (b[0], true),
            4 if self.polled => (b[1], false),
            _ => (0xFF, false),
        }
    }
}

/// A port's exchange, as libpad's handler states run it (`pdresres.o`
/// +158Ch..+1874h, direct connection): each byte's reply is read, and its
/// acknowledge awaited, when the next byte goes out; the last one isn't
/// awaited. An error is the code `_padFuncNextPort` gets.
fn exchange(m: &mut M, chan: usize, spin: &mut Spin) -> std::result::Result<(), i32> {
    // `_padInitSioMode` starts the wait before select.
    let start = spin.now;
    let mut x = Exchange { wire: Wire::new(m.sdk.pad.latched[chan]), reply: (0, false), sent: 0, stored: 0, spin };
    // State 0: the engine's next command; select, once the wait is over.
    let last_id = m.sdk.pad.rx[chan][0];
    send_auto(&mut m.sdk.pad.ports[chan], last_id);
    m.sdk.pad.rx[chan][0] = 0;
    x.spin.until(start + SELECT_WAIT);
    x.send(0x01);
    // State 1: the command, after a wait (the reply to select isn't kept).
    x.reply_acknowledged(ACK_WAIT, -2)?;
    x.spin.until(x.spin.now + COMMAND_WAIT);
    let request = m.sdk.pad.ports[chan].request;
    x.send(if request != 0 { request } else { POLL });
    // State 2: the ID, which says how many data bytes follow; the TAP
    // byte out (no multitap).
    x.reply_acknowledged(ACK_WAIT, -2)?;
    let id = x.keep(&mut m.sdk.pad.rx[chan]);
    let mut count = match (id & 0xF) * 2 {
        0 => 32,
        c => c as u32,
    };
    x.send(0);
    // State 3: 5Ah; the first data byte out.
    x.reply_acknowledged(ACK_WAIT, -2)?;
    let r = x.keep(&mut m.sdk.pad.rx[chan]);
    x.send(0);
    if !matches!(r, 0x5A | 0) {
        return Err(-4);
    }
    // State 4: the data bytes; before each but the last two, the other
    // port's pending reply.
    while count >= 2 {
        take_pending(m, 1 - chan);
        x.data_acknowledged()?;
        x.keep(&mut m.sdk.pad.rx[chan]);
        x.send(0);
        count -= 1;
    }
    count -= 1;
    while count > 0 {
        x.data_acknowledged()?;
        x.keep(&mut m.sdk.pad.rx[chan]);
        x.send(0);
        count -= 1;
    }
    x.received();
    x.keep(&mut m.sdk.pad.rx[chan]);
    m.sdk.pad.ports[chan].rx_count = x.stored as u8;
    Ok(())
}

/// The wire side of an exchange: the byte in flight's reply and
/// acknowledge, when it went out, how many replies were kept, and the
/// handler's time.
struct Exchange<'a> {
    wire: Wire,
    reply: (u8, bool),
    sent: u64,
    stored: usize,
    spin: &'a mut Spin,
}

impl Exchange<'_> {
    fn send(&mut self, tx: u8) {
        self.reply = self.wire.byte(tx);
        self.sent = self.spin.now;
    }

    /// Spin until the byte in flight's reply is in.
    fn received(&mut self) {
        self.spin.until(self.sent + BYTE);
    }

    /// Spin until the byte in flight's /ACK, or until `wait` from `from`
    /// passes without one: the exchange ends with `code`.
    fn acknowledge(&mut self, from: u64, wait: u64, code: i32) -> std::result::Result<(), i32> {
        if self.reply.1 {
            self.spin.until(self.sent + BYTE + ACK_DELAY);
            Ok(())
        } else {
            self.spin.until(from + wait);
            Err(code)
        }
    }

    /// `_padSioRW2`: the reply, then its /ACK, the wait counted from the
    /// reply.
    fn reply_acknowledged(&mut self, wait: u64, code: i32) -> std::result::Result<(), i32> {
        self.received();
        let from = self.spin.now;
        self.acknowledge(from, wait, code)
    }

    /// `_padSioRW` (data bytes): the wait is counted from before the
    /// reply.
    fn data_acknowledged(&mut self) -> std::result::Result<(), i32> {
        let from = self.spin.now;
        self.received();
        self.acknowledge(from, DATA_ACK_WAIT, -0x14)
    }

    /// Keep the byte in flight's reply in the receive buffer.
    fn keep(&mut self, rx: &mut [u8; RX]) -> u8 {
        let r = self.reply.0;
        if let Some(b) = rx.get_mut(self.stored) {
            *b = r;
        }
        self.stored += 1;
        r
    }
}

/// A completed exchange's reply, if one is pending: processed, and its
/// command retired (`_padInitSioMode`; the data state for the other port).
fn take_pending(m: &mut M, chan: usize) {
    if std::mem::take(&mut m.sdk.pad.pending[chan]) {
        receive(m, chan);
        clear_command(&mut m.sdk.pad.ports[chan]);
    }
}

/// `_padFuncSendAuto` (direct): the command the port's command state asks
/// for. `last_id` is the last exchange's ID byte.
fn send_auto(p: &mut Port, last_id: u8) {
    if last_id == ID_CONFIG {
        if p.kind == 0 || p.cmd_state == CMD_DONE {
            p.request = CONFIG;
            return;
        }
        if p.state == STATE_FIND_CTP1 {
            clear_info(p);
        }
    }
    match p.cmd_state {
        // Into config mode, and out of it.
        CMD_CONFIG | CMD_CONFIG_OFF => p.request = CONFIG,
        // The other states' requests (the info tables, `PadSetActAlign`'s
        // 4Dh) go to a pad in config mode, which a digital pad never is.
        _ => {}
    }
}

/// `_padFuncRecvAuto` (direct): a completed exchange's reply into the
/// game's buffer, and the detection step it makes.
fn receive(m: &mut M, chan: usize) {
    let rx = m.sdk.pad.rx[chan];
    let mut p = m.sdk.pad.ports[chan];
    let buf = p.buf;
    if rx[0] & 0xF0 == 0 {
        m.mem.write(buf, &[0xFF, 0]);
        p.kind = 0;
        clear_info(&mut p);
        m.sdk.pad.ports[chan] = p;
        return;
    }
    let old = p.kind;
    p.kind = rx[0] >> 4;
    if p.kind == 0xF {
        p.kind = old;
    } else {
        m.mem.write(buf, &[0, rx[0]]);
        let n = (p.rx_count as usize).min(RX);
        if n > 2 {
            m.mem.write(buf + 2, &rx[2..n]);
        }
    }
    if (rx[1] == 0 && (p.cmd_state != CMD_CONFIG || p.align.is_some())) || p.kind != old {
        clear_info(&mut p);
    }
    p.fails = 0;
    m.sdk.pad.ports[chan] = p;
    let p = &mut m.sdk.pad.ports[chan];
    if p.cmd_state == CMD_DONE || (p.cmd_state != CMD_DETECT && p.request == 0) {
        return;
    }
    if p.cmd_state.wrapping_sub(2) < 0xFC && rx[0] != ID_CONFIG {
        clear_info(p);
        return;
    }
    match p.cmd_state {
        CMD_DETECT => {
            p.state = STATE_FIND_PAD;
            p.cmd_state = CMD_CONFIG;
        }
        CMD_CONFIG => p.cmd_state = 2,
        CMD_CONFIG_OFF => p.cmd_state = CMD_DONE,
        // Replies to the info and actuator requests: a pad in config
        // mode's, which a digital pad never is.
        _ => {}
    }
}

/// `_dirFailAuto`: an exchange failed.
fn fail(m: &mut M, chan: usize) {
    let p = &mut m.sdk.pad.ports[chan];
    p.failures = p.failures.wrapping_add(1);
    match p.cmd_state {
        CMD_DETECT => {}
        CMD_CONFIG => {
            if p.fails < RETRIES {
                p.fails += 1;
            } else {
                p.state = STATE_FIND_CTP1;
                p.cmd_state = CMD_DONE;
            }
            return;
        }
        _ => {
            if p.fails < RETRIES {
                p.fails += 1;
                return;
            }
            if p.state != STATE_DISCON {
                clear_info(p);
            }
        }
    }
    if m.sdk.pad.rx[chan][0] != ID_CONFIG {
        p.kind = 0;
        let buf = p.buf;
        m.mem.write(buf, &[0xFF, 0]);
    }
}

/// `_padFuncClrInfo` (direct): a found pad is forgotten.
fn clear_info(p: &mut Port) {
    if p.state == STATE_DISCON {
        return;
    }
    p.state = STATE_DISCON;
    p.cmd_state = CMD_DETECT;
    p.ex_id = 0;
    p.align = None;
    p.id_count = 0;
    p.prev_mode = 0;
}

/// `_padFuncClrCmdNo`: the requested command is the current one.
fn clear_command(p: &mut Port) {
    p.current = std::mem::take(&mut p.request);
}

/// `_padFuncChkEng`: whether the engine is busy for a new command: always,
/// unless the pad reported extended IDs and its info is loaded.
fn busy(p: &Port) -> bool {
    p.ex_id == 0 || p.cmd_state != CMD_DONE
}

// --- The calls ---------------------------------------------------------------

/// `PadInitDirect(port1, port2)`: fresh records for both ports, their
/// buffers `FF 00`. The original leaves 1 in v0.
fn pad_init_direct(m: &mut M, a: [u32; 4]) -> Result {
    let pad = &mut m.sdk.pad;
    pad.ports = Default::default();
    pad.direct = true;
    for (i, &buf) in a[..2].iter().enumerate() {
        pad.ports[i].buf = buf;
    }
    for &buf in &a[..2] {
        m.mem.write(buf, &[0xFF, 0]);
    }
    ret(1)
}

/// `PadStartCom()`: the element (re)queued at the chain's end, vblank
/// acknowledged and unmasked, the kernel's vblank acknowledge of root
/// counter 3 off (ChangeClearRCnt(3, 0)), the critical section left, both
/// ports forgotten. The original leaves 1 in v0.
fn pad_start_com(m: &mut M, _a: [u32; 4]) -> Result {
    bios(m, 0xC0, 0x03, [PRIORITY, ELEMENT, 0, 0])?;
    bios(m, 0xC0, 0x02, [PRIORITY, ELEMENT, 0, 0])?;
    set_reg(m, I_STAT, 4, !1)?;
    let mask = reg(m, I_MASK, 4)?;
    set_reg(m, I_MASK, 4, mask | 1)?;
    bios(m, 0xC0, 0x0A, [3, 0, 0, 0])?;
    m.crit = false;
    for p in &mut m.sdk.pad.ports {
        clear_info(p);
    }
    ret(1)
}

/// `PadStopCom()`: the kernel acknowledges root counter 3 again, the
/// element leaves the chain, the critical section is left (v0: 0, from
/// ExitCriticalSection).
fn pad_stop_com(m: &mut M, _a: [u32; 4]) -> Result {
    bios(m, 0xC0, 0x0A, [3, 1, 0, 0])?;
    bios(m, 0xC0, 0x03, [PRIORITY, ELEMENT, 0, 0])?;
    m.crit = false;
    ret(0)
}

/// `PadGetState(port)`: the port's state; with a command in flight or a
/// status byte that isn't 0 in its buffer, FindCTP1 and FindCTP2 read as
/// FindPad and Stable as ReqInfo.
fn pad_get_state(m: &mut M, a: [u32; 4]) -> Result {
    if !m.sdk.pad.direct {
        return ret(0);
    }
    let p = m.sdk.pad.ports[index(a[0])];
    let busy = p.request != 0 || p.current != 0 || m.mem.r8(p.buf) != 0;
    let s = match (busy, p.state) {
        (true, STATE_FIND_CTP1 | STATE_FIND_CTP2) => STATE_FIND_PAD,
        (true, STATE_STABLE) => STATE_REQ_INFO,
        (_, s) => s,
    };
    ret(s as u32)
}

/// `PadInfoMode(port, mode, id)`: 1 the terminal type, 2 the extended ID,
/// 3 the mode offset, 4 the ID table (`id < 0`: its size), 100 the
/// failure count, else 0.
fn pad_info_mode(m: &mut M, a: [u32; 4]) -> Result {
    let p = &m.sdk.pad.ports[index(a[0])];
    let id = a[2] as i32;
    ret(match a[1] {
        1 => p.kind as u32,
        2 => p.ex_id as u32,
        3 => p.prev_mode as u32,
        // The table's entries come from a pad in config mode; there are
        // none without one.
        4 if id < 0 => p.id_count as u32,
        100 => p.failures,
        _ => 0,
    })
}

/// `PadSetAct(port, table, size)`: the actuator table libpad sends from.
fn pad_set_act(m: &mut M, a: [u32; 4]) -> Result {
    let p = &mut m.sdk.pad.ports[index(a[0])];
    p.act = a[1];
    p.act_size = a[2] as u8;
    ret(0)
}

/// `PadSetActAlign(port, table)`: 1 when the engine takes the actuator
/// alignment command, 0 when it is busy.
fn pad_set_act_align(m: &mut M, a: [u32; 4]) -> Result {
    let p = &mut m.sdk.pad.ports[index(a[0])];
    if busy(p) {
        return ret(0);
    }
    p.cmd_state = CMD_CONFIG;
    p.align = Some(a[1]);
    ret(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{call, machine_with_bus, vblanks};

    const B0: u32 = 0x8010_0000;
    const B1: u32 = 0x8010_0040;

    fn buf(m: &M, a: u32) -> [u8; 4] {
        let mut b = [0; 4];
        m.mem.read(a, &mut b);
        b
    }

    fn started() -> M {
        let mut m = machine_with_bus();
        m.mem.write(B0, &[0xAA; 0x80]);
        assert_eq!(call(&mut m, pad_init_direct, [B0, B1, 0, 0]), 1);
        assert_eq!(buf(&m, B0), [0xFF, 0, 0xAA, 0xAA]);
        assert_eq!(call(&mut m, pad_start_com, [0; 4]), 1);
        m
    }

    fn state(m: &mut M, port: u32) -> u32 {
        call(m, pad_get_state, [port, 0, 0, 0])
    }

    #[test]
    fn wire_answers_a_poll() {
        let mut w = Wire::new(Some([0xF7, 0xFF]));
        let got: Vec<(u8, bool)> = [0x01, 0x42, 0, 0, 0].iter().map(|&b| w.byte(b)).collect();
        assert_eq!(got, [(0xFF, true), (0x41, true), (0x5A, true), (0xF7, true), (0xFF, false)]);
        let mut w = Wire::new(Some([0xFF, 0xFF]));
        assert_eq!(w.byte(0x01), (0xFF, true));
        assert_eq!(w.byte(0x43), (0x41, false), "config mode: not a digital pad's");
        assert_eq!(Wire::new(None).byte(0x01), (0xFF, false));
    }

    /// The testing host has a digital pad in port 1 (`FF EF`), nothing in
    /// port 2: port 1's reply is taken at the next vblank, detection asks
    /// for config mode twelve times, then FindCTP1.
    #[test]
    fn one_pad_detected() {
        let mut m = started();
        assert_eq!(m.io.as_ref().unwrap().irq.mask & 1, 1, "vblank unmasked");
        vblanks(&mut m, 1);
        assert_eq!(buf(&m, B0), [0xFF, 0, 0xAA, 0xAA], "the reply waits a vblank");
        assert_eq!(state(&mut m, 0), 0);
        assert_eq!(buf(&m, B1), [0xFF, 0, 0xAA, 0xAA]);
        vblanks(&mut m, 1);
        assert_eq!(buf(&m, B0), [0, 0x41, 0xFF, 0xEF]);
        assert_eq!(state(&mut m, 0), 1);
        assert_eq!(call(&mut m, pad_info_mode, [0, 1, 0, 0]), 4);
        assert_eq!(m.sdk.pad.ports[0].current, CONFIG);
        // The handler's waits take a quantum each: a refused command five,
        // nothing connected three.
        let spent = |m: &M| (m.clock.now - m.clock.video.vblank_at(m.vblanks)) / 20_000;
        assert_eq!(spent(&m), 8);
        for _ in 0..11 {
            assert_eq!(state(&mut m, 0), 1);
            vblanks(&mut m, 1);
        }
        assert_eq!(m.sdk.pad.ports[0].state, STATE_FIND_CTP1);
        vblanks(&mut m, 1);
        assert_eq!(state(&mut m, 0), 1, "the poll's reply is pending");
        // A poll: seven.
        assert_eq!(spent(&m), 10);
        vblanks(&mut m, 1);
        assert_eq!(state(&mut m, 0), 2);
        assert_eq!(call(&mut m, pad_info_mode, [0, 100, 0, 0]), 12);
        assert_eq!(call(&mut m, pad_info_mode, [0x10, 1, 0, 0]), 0);
        assert_eq!(call(&mut m, pad_info_mode, [0x10, 100, 0, 0]), 15, "port 2 fails every vblank");
        assert_eq!(state(&mut m, 0x10), 0);
        assert_eq!(call(&mut m, pad_set_act_align, [0, B1, 0, 0]), 0);
        // The status byte reads as busy.
        m.mem.w8(B0, 1);
        assert_eq!(state(&mut m, 0), 1);
    }

    #[test]
    fn stop_and_restart() {
        let mut m = started();
        vblanks(&mut m, 3);
        assert_eq!(call(&mut m, pad_stop_com, [0; 4]), 0);
        assert!(!installed(&m));
        m.mem.write(B0, &[9; 4]);
        vblanks(&mut m, 2);
        assert_eq!(buf(&m, B0), [9; 4], "nothing polls");
        call(&mut m, pad_start_com, [0; 4]);
        assert_eq!(m.sdk.pad.ports[0].state, 0, "forgotten");
        assert_eq!(m.kernel.int_handlers[&2], vec![ELEMENT]);
        // Detection again: a poll, its reply a vblank later.
        vblanks(&mut m, 1);
        assert_eq!(buf(&m, B0), [9; 4]);
        assert_eq!(state(&mut m, 0), 0);
        vblanks(&mut m, 1);
        assert_eq!(buf(&m, B0), [0, 0x41, 0xFF, 0xEF]);
        assert_eq!(state(&mut m, 0), 1);
        // A reply still pending when PadInitDirect starts over lands in
        // the new records' buffer, whose length was cleared.
        vblanks(&mut m, 12);
        assert!(m.sdk.pad.pending[0]);
        m.mem.write(B1 + 0x10, &[7; 4]);
        call(&mut m, pad_init_direct, [B1 + 0x10, 0, 0, 0]);
        vblanks(&mut m, 1);
        assert_eq!(buf(&m, B1 + 0x10), [0, 0x41, 7, 7]);
    }

    #[test]
    fn masked_vblank_polls_nothing() {
        let mut m = started();
        let mask = m.io.as_ref().unwrap().irq.mask;
        set_reg(&mut m, I_MASK, 4, mask & !1).unwrap();
        vblanks(&mut m, 3);
        assert_eq!(call(&mut m, pad_info_mode, [0x10, 100, 0, 0]), 0);
    }
}
