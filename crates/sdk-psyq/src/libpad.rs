//! libpad (chip sio0, D59, D60): direct connection (`PadInitDirect`), its
//! vblank engine (`pdresres.o`, `pddirres.o`) and the calls games make
//! into it, as adapters.
//!
//! **What games observe is libpad's own state machine** (D40), so the
//! adapters run it, from PsyQ 4.7's objects (`psxw sdk disasm`; psyq-decomp
//! `libpad/{pddirini,pddirres,pdresres,pdmaiini,pdent2,pdent3,pdcmd1,
//! pdcmd2}`), with each port's bytes answered by a pad model instead of
//! SIO0's registers:
//! - **`PadStartCom` queues libpad's `SysEnqIntRP` element**,
//!   `_padVsyncParam`, at priority 2. The kernel runs it as the adapters'
//!   step in its place in the chains ([`psx_bios::Sdk::chain_element`]):
//!   when vblank is unmasked and pending (`_padIsVsync`), one exchange per
//!   port, port 1 first (`_padIntPad`). The pads' buttons are latched at
//!   the vblank, as SIO0's are.
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
//! - **The interrupt takes the original's time** (D60, D66). Its
//!   busy-waits on SIO0 and root counter 2 poll once a loop-header pass,
//!   and both see the cycle each pass is made at, so a wait ends at the
//!   first pass at or after what it waits for ([`Spin`]): about 640
//!   passes (12,800 cycles) for a poll and an empty port. The adapters
//!   spend the passes from the guest's budget in the exception, a quantum
//!   passing (nothing delivered) each time it runs out, as the original's
//!   loops do. Games see it: bof4's `title-to-field` falls hundreds of
//!   frames behind the device without the time; and when each wait took a
//!   quantum (half a frame with two pads), btn's CD outran its CPU.
//!
//! **State stays where libpad keeps it** (D67), found through the
//! objects' code ([`Lib`]). Each call's code reaches one of the engine's
//! variables in `pdresres.o`'s `.data` (`_padIntExec`, `_padVsyncParam`,
//! `_padFuncPort2Info`); the element there names `_padIntPad`, whose code
//! names `_padRestPeriod`. `PadInitDirect` calls `_padInitDirPort` (guest
//! code), which clears `pddirres.o`'s two port records ([`rec`]), points
//! them at their receive and transmit buffers, installs the engine's
//! functions and sets `_padInfoDir`, where the adapters find the records.
//! A port's pending reply is its `_padFixResult`. The games' buffers are
//! guest RAM, written as the original writes them; PadGetState reads the
//! status byte back from there. `PadStartCom` acknowledges and unmasks
//! vblank in the interrupt controller, which the chips share. Only the
//! pad model's latched buttons, SIO0's side of the wire, are the
//! runtime's.
//!
//! **What isn't reproduced.** The pad model is a digital pad, as SIO0's
//! device is (`psx_devices::sio`): no analog or DualShock modes, so
//! libpad's extended commands (config mode, actuator alignment, the info
//! tables) are never acknowledged on either side, and `PadSetActAlign`
//! always reports the engine busy. The bytes libpad transmits after the
//! command (actuator levels, `PadSetAct`) aren't computed: a digital pad
//! ignores them, so the engine's current budget (`_padTotalCurr`) and the
//! records' actuator gates keep what they had. Nor are the engine's own
//! bookkeeping words written (`_padSioChan`, `_padSioState`, the data
//! count). Multitap and light-gun paths aren't linked in these games. The
//! waits' times leave out the original's wait for /ACK to fall after each
//! byte (the pulse lasts 32 cycles, a pass or two).

use crate::{Adapter, M, Result, reg, ret, set_reg};
use psx_bios::Flow;

pub const ADAPTERS: &[(&str, Adapter)] = &[
    ("PadInitDirect", pad_init_direct),
    ("PadStartCom", pad_start_com),
    ("PadStopCom", pad_stop_com),
    ("PadGetState", pad_get_state),
    ("PadInfoMode", pad_info_mode),
    ("PadSetAct", pad_set_act),
    ("PadSetActAlign", pad_set_act_align),
];

/// The exception step that runs libpad's element ([`irq_step`]).
pub const STEP: u32 = 0x5000_0000;

const I_STAT: u32 = psx_devices::irq::I_STAT;
const I_MASK: u32 = psx_devices::irq::I_MASK;
/// The kernel's chain priority libpad uses.
const PRIORITY: u32 = 2;
/// Bytes of one port's receive buffer: the ID, 5Ah and up to 32 data
/// bytes.
const RX: u32 = 0x23;

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

/// A port record's fields direct connection uses (`PadInfo2`, psyq-decomp
/// `libpad-internal.h`): two records in `pddirres.o`'s `.bss`.
pub mod rec {
    pub const SIZE: u32 = 0xF0;
    /// The ID, actuator-alignment and combination tables' pointers.
    pub const TABLES: u32 = 0x00;
    pub const CHILD: u32 = 0x0C;
    pub const ROOT: u32 = 0x10;
    /// `PadSetActAlign`'s handlers for the info states, and its table.
    pub const PREPARE: u32 = 0x14;
    pub const APPLY: u32 = 0x18;
    pub const ALIGN: u32 = 0x20;
    /// `_padCmdParaMode`'s parameter byte; the command's bytes, and how
    /// many.
    pub const PARAM: u32 = 0x24;
    pub const CMD_DATA: u32 = 0x2C;
    pub const CMD_LEN: u32 = 0x36;
    /// `PadSetAct`'s table and size.
    pub const ACT: u32 = 0x28;
    pub const ACT_SIZE: u32 = 0x34;
    /// The game's buffer: status (0 or FFh), ID, then the data bytes; and
    /// how many the last reply put there.
    pub const BUF: u32 = 0x30;
    pub const BUF_LEN: u32 = 0x35;
    /// The command for the next exchange (0: poll), the one the last
    /// exchange sent, and the one before.
    pub const REQUEST: u32 = 0x37;
    pub const CURRENT: u32 = 0x38;
    pub const PREVIOUS: u32 = 0x39;
    /// The receive and transmit buffers, and their counts (the receive
    /// count is FFh while the reply to select is in flight).
    pub const RX: u32 = 0x3C;
    pub const TX: u32 = 0x40;
    pub const RX_COUNT: u32 = 0x44;
    pub const TX_COUNT: u32 = 0x45;
    pub const CMD_STATE: u32 = 0x46;
    /// The info states' table index.
    pub const INFO_INDEX: u32 = 0x47;
    pub const STATE: u32 = 0x49;
    /// Failures in a row in the current command state.
    pub const FAILS: u32 = 0x4A;
    /// Failed exchanges (`PadInfoMode` mode 100).
    pub const FAILURES: u32 = 0x4C;
    /// The actuator alignment's six bytes, FFh while unknown.
    pub const ALIGN_REPLY: u32 = 0x5D;
    pub const ID_COUNT: u32 = 0xE3;
    pub const PREV_MODE: u32 = 0xE4;
    pub const EX_ID: u32 = 0xE6;
    /// The terminal type, the ID's high nibble (4: digital pad).
    pub const KIND: u32 = 0xE8;
    pub const ACT_COUNT: u32 = 0xE9;
    pub const COMB_COUNT: u32 = 0xEA;
}

/// `pdresres.o`'s `.data`: the engine's variables.
pub mod data {
    pub const PORT2INFO: u32 = 0x1C;
    /// The port records (`_padInfoDir`).
    pub const INFO_DIR: u32 = 0x3C;
    pub const INT_EXEC: u32 = 0x40;
    pub const MODE_MTAP: u32 = 0x54;
    /// The first and last port the engine polls (`PadEnableCom`).
    pub const CHAN_START: u32 = 0x58;
    pub const CHAN_STOP: u32 = 0x5C;
    /// Each port's `_padFixResult`: 0, a reply awaits processing; -1, none.
    pub const FIX_RESULT: u32 = 0x60;
    /// `_padVsyncParam`: next, handler (`_padIntPad`), verifier
    /// (`_padIsVsync`).
    pub const VSYNC_PARAM: u32 = 0x68;
    /// `_padCalledIntPad` (`PadChkVsync`).
    pub const CALLED: u32 = 0x78;
}

/// Code operands, from the start of the function that has them.
mod at {
    /// `_padFuncPort2Info`'s pair, in `PadGetState`, `PadInfoMode`,
    /// `PadSetAct` and `PadSetActAlign`.
    pub const PORT2INFO: (u32, u32) = (0x00, 0x04);
    /// `PadInitDirect`: `_padIntExec`'s pair, the `jal _padInitDirPort`.
    pub const INIT_EXEC: (u32, u32) = (0x14, 0x18);
    pub const JAL_INIT_PORT: u32 = 0x24;
    /// `PadStartCom`'s and `PadStopCom`'s `_padVsyncParam` pair.
    pub const ELEMENT: (u32, u32) = (0x1C, 0x20);
    /// `PadSetActAlign`'s handlers' pairs.
    pub const PREPARE: (u32, u32) = (0x4C, 0x50);
    pub const APPLY: (u32, u32) = (0x58, 0x5C);
    /// `_padIntPad` (`pdresres.o` +68h, `_padIsVsync` at +0): its
    /// `_padRestPeriod` pair.
    pub const INT_PAD: u32 = 0x68;
    pub const REST: (u32, u32) = (0x44, 0x48);
}

/// Where libpad keeps its state (D67): `pdresres.o`'s `.data`, and
/// `_padRestPeriod` (a common).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lib {
    pub data: u32,
    pub rest: u32,
}

impl Lib {
    /// `_padVsyncParam`, the element `PadStartCom` queues.
    pub fn element(&self) -> u32 {
        self.data + data::VSYNC_PARAM
    }

    fn word(&self, m: &M, var: u32) -> u32 {
        m.mem.r32(self.data + var)
    }

    /// The port records `_padInitDirPort` set out (`_padInfoDir`).
    pub fn records(&self, m: &M) -> Option<u32> {
        Some(self.word(m, data::INFO_DIR)).filter(|&r| r != 0)
    }

    /// `port`'s record (`_padFuncPort2Info`: ports 00h-0Fh are port 1, any
    /// other port 2).
    fn port(&self, m: &M, port: u32) -> Option<u32> {
        self.records(m).map(|r| r + rec::SIZE * (port & 0xF0 != 0) as u32)
    }

    fn fix(&self, chan: u32) -> u32 {
        self.data + data::FIX_RESULT + 4 * chan
    }
}

/// libpad's adapter state: where libpad is, and the pad model's buttons.
#[derive(Debug, Default)]
pub struct Pad {
    pub lib: Option<Lib>,
    /// Each slot's buttons, latched at the last vblank (`None`: nothing
    /// connected).
    pub latched: [Option<[u8; 2]>; 2],
}

/// libpad from the running adapter: the engine variable `var` (in
/// `pdresres.o`'s `.data`) its code reaches with the pair at `hi`, `lo`.
fn lib(m: &mut M, (hi, lo): (u32, u32), var: u32) -> std::result::Result<Lib, String> {
    let f = m.sdk.at;
    let data = crate::libspu::pair(m, f + hi, f + lo)
        .ok_or_else(|| format!("libpad at {f:#010x}: no engine variable where PsyQ 4.7's object has it"))?
        .wrapping_sub(var);
    if let Some(l) = m.sdk.pad.lib.filter(|l| l.data == data) {
        return Ok(l);
    }
    let err = |what: &str| format!("libpad/pdresres's .data at {data:#010x}: no {what} where PsyQ 4.7's object has it");
    let elem = data + data::VSYNC_PARAM;
    let int_pad = m.mem.r32(elem + 4);
    if m.mem.r32(elem + 8) != int_pad.wrapping_sub(at::INT_PAD) {
        return Err(err("_padVsyncParam"));
    }
    let rest = crate::libspu::pair(m, int_pad + at::REST.0, int_pad + at::REST.1).ok_or_else(|| err("_padRestPeriod"))?;
    let l = Lib { data, rest };
    m.sdk.pad.lib = Some(l);
    Ok(l)
}

/// The kernel's step for `elem`, when it is libpad's element.
pub fn chain_element(m: &M, elem: u32) -> Option<u32> {
    m.sdk.pad.lib.filter(|l| l.element() == elem).map(|_| STEP)
}

// --- The vblank engine -----------------------------------------------------

/// At each vblank, before the interrupt: the pads latch their buttons,
/// as SIO0's do (`psx_bios::io`), once libpad's adapters have run.
pub fn on_vblank(m: &mut M) {
    if m.sdk.pad.lib.is_none() {
        return;
    }
    for p in 0..2 {
        let st = m.host.pad_state(p as u32);
        m.sdk.pad.latched[p] = (st.len() >= 2).then(|| [st[0], st[1]]);
    }
}

/// The element's verifier (`_padIsVsync`: vblank unmasked and pending)
/// and handler (`_padIntPad`): each port the engine polls in turn, its
/// pending reply processed (`_padInitSioMode`), then its exchange. The
/// handler's busy-waits take the time they take the original ([`Spin`]):
/// it passes in the exception, delivering nothing, as at the original's
/// safepoints.
pub fn irq_step(m: &mut M, _step: u32) {
    let vblank = |m: &mut M, r: u32| reg(m, r, 4).is_ok_and(|v| v & 1 != 0);
    let Some(l) = m.sdk.pad.lib else { return };
    if !vblank(m, I_MASK) || !vblank(m, I_STAT) {
        return;
    }
    m.mem.w32(l.data + data::CALLED, 1);
    let (start, stop) = (l.word(m, data::CHAN_START) as i32, l.word(m, data::CHAN_STOP) as i32);
    // A port the engine doesn't poll counts its rest, up to 150 vblanks.
    for (rest, off) in [(l.rest, start != 0), (l.rest + 4, stop == 0)] {
        let n = m.mem.r32(rest) as i32;
        if off && n < 150 {
            m.mem.w32(rest, (n + 1) as u32);
        }
    }
    let Some(recs) = l.records(m) else { return };
    if l.word(m, data::INT_EXEC) == 0 || start > stop {
        return;
    }
    let budget = crate::libetc::LOOP_BUDGET;
    let mut spin = Spin { pass: (m.quantum() / budget).max(1), now: 0, passes: 0 };
    for chan in start.max(0)..=stop.min(1) {
        let chan = chan as u32;
        let p = recs + rec::SIZE * chan;
        take_pending(m, l, recs, chan);
        match exchange(m, l, recs, chan, &mut spin) {
            Ok(()) => m.mem.w32(l.fix(chan), 0),
            Err(_) => {
                fail(m, p);
                clear_command(m, p);
            }
        }
    }
    spend_in_exception(m, spin.passes);
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

/// libpad's busy-waits as the runtime runs a guest loop: each poll is a
/// loop-header pass, and SIO0 and root counter 2 see the cycle it is made
/// at (D66), so a wait ends at the first pass at or after what it waits
/// for. `now` counts from the handler's start.
pub struct Spin {
    /// Cycles a pass takes.
    pass: u64,
    now: u64,
    /// Passes made.
    passes: u64,
}

impl Spin {
    fn until(&mut self, t: u64) {
        if t > self.now {
            let k = (t - self.now).div_ceil(self.pass);
            self.now += k * self.pass;
            self.passes += k;
        }
    }
}

/// `n` loop-header passes of code running in the exception: the guest's
/// budget spent as the original's loops spend it, a quantum passing
/// (nothing delivered) each time it runs out.
pub(crate) fn spend_in_exception(m: &mut M, n: u64) {
    let budget = crate::libetc::LOOP_BUDGET;
    let left = m.regs.budget() as u64;
    if left == 0 {
        let t = m.clock.now + n * m.quantum() / budget;
        m.clock.advance_to(t);
        return;
    }
    if n < left {
        m.regs.set_budget((left - n) as u32);
        return;
    }
    let r = n - left;
    let t = m.clock.now + (1 + r / budget) * m.quantum();
    m.clock.advance_to(t);
    m.regs.set_budget((budget - r % budget) as u32);
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
fn exchange(m: &mut M, l: Lib, recs: u32, chan: u32, spin: &mut Spin) -> std::result::Result<(), i32> {
    let p = recs + rec::SIZE * chan;
    // `_padInitSioMode` starts the wait before select.
    let start = spin.now;
    let mut x = Exchange { wire: Wire::new(m.sdk.pad.latched[chan as usize]), rec: p, reply: (0, false), sent: 0, spin };
    // State 0: the engine's next command; select, once the wait is over.
    send_auto(m, p);
    let rx = m.mem.r32(p + rec::RX);
    m.mem.w8(rx, 0);
    x.spin.until(start + SELECT_WAIT);
    x.select(m);
    // State 1: the command, after a wait (the reply to select isn't kept).
    x.reply_acknowledged(ACK_WAIT, -2)?;
    x.spin.until(x.spin.now + COMMAND_WAIT);
    let request = m.mem.r8(p + rec::REQUEST);
    x.next(m, if request != 0 { request } else { POLL });
    // State 2: the ID, which says how many data bytes follow; the TAP
    // byte out (no multitap).
    x.reply_acknowledged(ACK_WAIT, -2)?;
    let id = x.next(m, 0);
    let mut count = match (id & 0xF) * 2 {
        0 => 32,
        c => c as u32,
    };
    // State 3: 5Ah; the first data byte out.
    x.reply_acknowledged(ACK_WAIT, -2)?;
    let r = x.next(m, 0);
    if !matches!(r, 0x5A | 0) {
        return Err(-4);
    }
    // State 4: the data bytes; before each but the last two, the other
    // port's pending reply.
    while count >= 2 {
        take_pending(m, l, recs, 1 - chan);
        x.data_acknowledged()?;
        x.next(m, 0);
        count -= 1;
    }
    count -= 1;
    while count > 0 {
        x.data_acknowledged()?;
        x.next(m, 0);
        count -= 1;
    }
    x.received();
    x.keep(m);
    Ok(())
}

/// The wire side of an exchange: the byte in flight's reply and
/// acknowledge, when it went out, the port's record, and the handler's
/// time.
struct Exchange<'a> {
    wire: Wire,
    rec: u32,
    reply: (u8, bool),
    sent: u64,
    spin: &'a mut Spin,
}

impl Exchange<'_> {
    fn send(&mut self, tx: u8) {
        self.reply = self.wire.byte(tx);
        self.sent = self.spin.now;
    }

    /// `_padSioRW(-2)`: select goes out, its reply not to be kept.
    fn select(&mut self, m: &mut M) {
        m.mem.w8(self.rec + rec::RX_COUNT, 0xFF);
        m.mem.w8(self.rec + rec::TX_COUNT, 1);
        let tx = m.mem.r32(self.rec + rec::TX);
        m.mem.w8(tx, 0x01);
        self.send(0x01);
    }

    /// The byte in flight's reply into the receive buffer at the receive
    /// count, unless that is FFh, and the count on.
    fn keep(&mut self, m: &mut M) -> u8 {
        let r = self.reply.0;
        let n = m.mem.r8(self.rec + rec::RX_COUNT);
        if (n as u32) < RX {
            let rx = m.mem.r32(self.rec + rec::RX);
            m.mem.w8(rx + n as u32, r);
        }
        m.mem.w8(self.rec + rec::RX_COUNT, n.wrapping_add(1));
        r
    }

    /// The end of `_padSioRW2` and `_padSioRW`: the reply kept, `tx` out.
    fn next(&mut self, m: &mut M, tx: u8) -> u8 {
        let n = m.mem.r8(self.rec + rec::TX_COUNT);
        m.mem.w8(self.rec + rec::TX_COUNT, n.wrapping_add(1));
        let r = self.keep(m);
        self.send(tx);
        r
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
}

/// A completed exchange's reply, if one is pending: processed, and its
/// command retired (`_padInitSioMode`; the data state for the other port).
fn take_pending(m: &mut M, l: Lib, recs: u32, chan: u32) {
    if m.mem.r32(l.fix(chan)) == 0 {
        m.mem.w32(l.fix(chan), u32::MAX);
        let p = recs + rec::SIZE * chan;
        receive(m, p);
        clear_command(m, p);
    }
}

/// `_padFuncSendAuto` (direct): the command the port's command state asks
/// for, after the last exchange's ID byte.
fn send_auto(m: &mut M, p: u32) {
    let last_id = m.mem.r8(m.mem.r32(p + rec::RX));
    if last_id == ID_CONFIG {
        if m.mem.r8(p + rec::KIND) == 0 || m.mem.r8(p + rec::CMD_STATE) == CMD_DONE {
            para_mode(m, p, 0);
            return;
        }
        if m.mem.r8(p + rec::STATE) == STATE_FIND_CTP1 {
            clear_info(m, p);
        }
    }
    match m.mem.r8(p + rec::CMD_STATE) {
        // Into config mode, and out of it.
        CMD_CONFIG => para_mode(m, p, 1),
        CMD_CONFIG_OFF => para_mode(m, p, 0),
        // The other states' requests (the info tables, `PadSetActAlign`'s
        // 4Dh) go to a pad in config mode, which a digital pad never is.
        _ => {}
    }
}

/// `_padCmdParaMode`: config mode (43h) next, its one byte `mode` (1: in,
/// 0: out).
fn para_mode(m: &mut M, p: u32, mode: u8) {
    m.mem.w8(p + rec::REQUEST, CONFIG);
    m.mem.w32(p + rec::CMD_DATA, p + rec::PARAM);
    m.mem.w8(p + rec::PARAM, mode);
    m.mem.w8(p + rec::CMD_LEN, 1);
}

/// `_padFuncRecvAuto` (direct): a completed exchange's reply into the
/// game's buffer, and the detection step it makes.
fn receive(m: &mut M, p: u32) {
    let (rx, buf) = (m.mem.r32(p + rec::RX), m.mem.r32(p + rec::BUF));
    let id = m.mem.r8(rx);
    if id & 0xF0 == 0 {
        m.mem.write(buf, &[0xFF, 0]);
        m.mem.w8(p + rec::KIND, 0);
        m.mem.w8(p + rec::BUF_LEN, 0);
        clear_info(m, p);
        return;
    }
    let old = m.mem.r8(p + rec::KIND);
    let kind = if id >> 4 == 0xF { old } else { id >> 4 };
    m.mem.w8(p + rec::KIND, kind);
    if id >> 4 != 0xF {
        m.mem.write(buf, &[0, id]);
        let n = m.mem.r8(p + rec::RX_COUNT);
        m.mem.w8(p + rec::BUF_LEN, n);
        let n = (n as u32).min(RX);
        if n > 2 {
            let mut data = vec![0; (n - 2) as usize];
            m.mem.read(rx + 2, &mut data);
            m.mem.write(buf + 2, &data);
        }
    }
    let config = m.mem.r8(p + rec::CMD_STATE) == CMD_CONFIG;
    if (m.mem.r8(rx + 1) == 0 && (!config || m.mem.r32(p + rec::PREPARE) != 0)) || kind != old {
        clear_info(m, p);
    }
    m.mem.w8(p + rec::FAILS, 0);
    let cmd = m.mem.r8(p + rec::CMD_STATE);
    if cmd == CMD_DONE || (cmd != CMD_DETECT && m.mem.r8(p + rec::REQUEST) == 0) {
        return;
    }
    if cmd.wrapping_sub(2) < 0xFC && id != ID_CONFIG {
        clear_info(m, p);
        return;
    }
    match cmd {
        CMD_DETECT => {
            m.mem.w8(p + rec::STATE, STATE_FIND_PAD);
            m.mem.w8(p + rec::CMD_STATE, CMD_CONFIG);
        }
        CMD_CONFIG => {
            m.mem.w8(p + rec::INFO_INDEX, 0);
            m.mem.w8(p + rec::CMD_STATE, 2);
        }
        CMD_CONFIG_OFF => m.mem.w8(p + rec::CMD_STATE, CMD_DONE),
        // Replies to the info and actuator requests: a pad in config
        // mode's, which a digital pad never is.
        _ => {}
    }
}

/// `_dirFailAuto`: an exchange failed.
fn fail(m: &mut M, p: u32) {
    let n = m.mem.r32(p + rec::FAILURES);
    m.mem.w32(p + rec::FAILURES, n.wrapping_add(1));
    let fails = m.mem.r8(p + rec::FAILS);
    match m.mem.r8(p + rec::CMD_STATE) {
        CMD_DETECT => {}
        CMD_CONFIG => {
            if fails < RETRIES {
                m.mem.w8(p + rec::FAILS, fails + 1);
            } else {
                m.mem.w8(p + rec::STATE, STATE_FIND_CTP1);
                m.mem.w8(p + rec::CMD_STATE, CMD_DONE);
            }
            return;
        }
        _ => {
            if fails < RETRIES {
                m.mem.w8(p + rec::FAILS, fails + 1);
                return;
            }
            clear_info(m, p);
        }
    }
    if m.mem.r8(m.mem.r32(p + rec::RX)) != ID_CONFIG {
        let buf = m.mem.r32(p + rec::BUF);
        m.mem.write(buf, &[0xFF, 0]);
        m.mem.w8(p + rec::KIND, 0);
        m.mem.w8(p + rec::BUF_LEN, 0);
    }
}

/// `_padFuncClrInfo` (direct): a found pad is forgotten.
fn clear_info(m: &mut M, p: u32) {
    if m.mem.r8(p + rec::STATE) == STATE_DISCON {
        return;
    }
    for f in [rec::STATE, rec::CMD_STATE, rec::ID_COUNT, rec::PREV_MODE, rec::ACT_COUNT, rec::COMB_COUNT] {
        m.mem.w8(p + f, 0);
    }
    m.mem.w16(p + rec::EX_ID, 0);
    m.mem.w32(p + rec::PREPARE, 0);
    m.mem.w32(p + rec::APPLY, 0);
    m.mem.write(p + rec::TABLES, &[0; 12]);
    m.mem.write(p + rec::ALIGN_REPLY, &[0xFF; 6]);
}

/// `_padFuncClrCmdNo`: the requested command is the current one.
fn clear_command(m: &mut M, p: u32) {
    let r = m.mem.r8(p + rec::REQUEST);
    m.mem.w8(p + rec::REQUEST, 0);
    m.mem.w8(p + rec::CURRENT, r);
}

/// `_padFuncChkEng`: whether the engine is busy for a new command: always,
/// unless the pad reported extended IDs and its info is loaded.
fn busy(m: &M, p: u32) -> bool {
    m.mem.r16(p + rec::EX_ID) == 0 || m.mem.r8(p + rec::CMD_STATE) != CMD_DONE
}

// --- The calls ---------------------------------------------------------------

/// `PadInitDirect(port1, port2)`: `_padInitDirPort` sets out fresh
/// records for both ports, then their buffers `FF 00`. The original
/// leaves 1 in v0.
fn pad_init_direct(m: &mut M, a: [u32; 4]) -> Result {
    let l = lib(m, at::INIT_EXEC, data::INT_EXEC)?;
    let f = m.sdk.at;
    let init_port = crate::libspu::jal(m, f + at::JAL_INIT_PORT)
        .ok_or_else(|| format!("libpad/pddirini at {f:#010x}: no _padInitDirPort call where PsyQ 4.7's object has it"))?;
    m.mem.w32(l.data + data::INT_EXEC, 0);
    m.mem.w32(l.data + data::MODE_MTAP, 0);
    Ok(Flow::Call {
        call: psx_bios::GuestCall { addr: init_port, args: [a[0], a[1], 0, 0] },
        then: Box::new(move |m, _| {
            let recs = l.word(m, data::INFO_DIR);
            for (i, &buf) in a[..2].iter().enumerate() {
                let p = recs + rec::SIZE * i as u32;
                m.mem.w32(p + rec::BUF, buf);
                m.mem.w32(p + rec::CHILD, 0);
                m.mem.w32(p + rec::ROOT, p);
                m.mem.write(buf, &[0xFF, 0]);
                m.mem.write(p + rec::ALIGN_REPLY, &[0xFF; 6]);
            }
            m.mem.w32(l.data + data::INT_EXEC, 1);
            Flow::ret(1)
        }),
    })
}

/// `PadStartCom()`: the element (re)queued at the chain's end, vblank
/// acknowledged and unmasked, the kernel's vblank acknowledge of root
/// counter 3 off (ChangeClearRCnt(3, 0)), the critical section left, both
/// ports forgotten, their rests over. The original leaves 1 in v0.
///
/// Its BIOS calls are the entry's own, so a patched table runs the guest's
/// function there and the rest waits for it (`psx_bios::machine::seq`).
/// [`lib`] is read here, before those calls: a callback that enters another
/// adapter moves `sdk.at` (D61).
fn pad_start_com(m: &mut M, _a: [u32; 4]) -> Result {
    let l = lib(m, at::ELEMENT, data::VSYNC_PARAM)?;
    m.mem.w32(l.data + data::INT_EXEC, 0);
    psx_bios::bios::syscall(m, [1, 0, 0, 0])?;
    let flow = psx_bios::bios::dispatch(m, 0xC0, 0x03, [PRIORITY, l.element(), 0, 0])?;
    Ok(psx_bios::machine::seq(m, flow, move |m, _| {
        let flow = psx_bios::bios::dispatch(m, 0xC0, 0x02, [PRIORITY, l.element(), 0, 0])
            .unwrap_or_else(|e| panic!("PadStartCom: {e}"));
        psx_bios::machine::seq(m, flow, move |m, _| {
            set_reg(m, I_STAT, 4, !1).unwrap_or_else(|e| panic!("PadStartCom: {e}"));
            let mask = reg(m, I_MASK, 4).unwrap_or_else(|e| panic!("PadStartCom: {e}"));
            set_reg(m, I_MASK, 4, mask | 1).unwrap_or_else(|e| panic!("PadStartCom: {e}"));
            let flow = psx_bios::bios::dispatch(m, 0xC0, 0x0A, [3, 0, 0, 0]).unwrap_or_else(|e| panic!("PadStartCom: {e}"));
            psx_bios::machine::seq(m, flow, move |m, _| {
                psx_bios::bios::syscall(m, [2, 0, 0, 0]).unwrap_or_else(|e| panic!("PadStartCom: {e}"));
                if let Some(recs) = l.records(m) {
                    clear_info(m, recs);
                    clear_info(m, recs + rec::SIZE);
                }
                m.mem.write(l.rest, &[0; 8]);
                m.mem.w32(l.data + data::INT_EXEC, 1);
                Flow::ret(1)
            })
        })
    }))
}

/// `PadStopCom()`: the kernel acknowledges root counter 3 again, the
/// element leaves the chain, the critical section is left (v0: 0, from
/// ExitCriticalSection). Its BIOS calls are the entry's own, as
/// [`pad_start_com`]'s are.
fn pad_stop_com(m: &mut M, _a: [u32; 4]) -> Result {
    let l = lib(m, at::ELEMENT, data::VSYNC_PARAM)?;
    psx_bios::bios::syscall(m, [1, 0, 0, 0])?;
    let flow = psx_bios::bios::dispatch(m, 0xC0, 0x0A, [3, 1, 0, 0])?;
    Ok(psx_bios::machine::seq(m, flow, move |m, _| {
        let flow = psx_bios::bios::dispatch(m, 0xC0, 0x03, [PRIORITY, l.element(), 0, 0])
            .unwrap_or_else(|e| panic!("PadStopCom: {e}"));
        psx_bios::machine::seq(m, flow, move |m, _| {
            psx_bios::bios::syscall(m, [2, 0, 0, 0]).unwrap_or_else(|e| panic!("PadStopCom: {e}"));
            Flow::ret(0)
        })
    }))
}

/// `PadGetState(port)`: the port's state; with a command in flight or a
/// status byte that isn't 0 in its buffer, FindCTP1 and FindCTP2 read as
/// FindPad and Stable as ReqInfo.
fn pad_get_state(m: &mut M, a: [u32; 4]) -> Result {
    let l = lib(m, at::PORT2INFO, data::PORT2INFO)?;
    let Some(p) = l.port(m, a[0]) else { return ret(0) };
    let b = |f: u32| m.mem.r8(p + f) != 0;
    // A multitap's port also waits for the command before.
    let child = m.mem.r32(p + rec::ROOT) != p && b(rec::PREVIOUS);
    let busy = b(rec::REQUEST) || b(rec::CURRENT) || child || m.mem.r8(m.mem.r32(p + rec::BUF)) != 0;
    let s = match (busy, m.mem.r8(p + rec::STATE)) {
        (true, STATE_FIND_CTP1 | STATE_FIND_CTP2) => STATE_FIND_PAD,
        (true, STATE_STABLE) => STATE_REQ_INFO,
        (_, s) => s,
    };
    ret(s as u32)
}

/// `PadInfoMode(port, mode, id)`: 1 the terminal type, 2 the extended ID,
/// 3 the mode offset, 4 the ID table's entry `id` (`id < 0`: its size),
/// 100 the failure count, else 0.
fn pad_info_mode(m: &mut M, a: [u32; 4]) -> Result {
    let l = lib(m, at::PORT2INFO, data::PORT2INFO)?;
    let Some(p) = l.port(m, a[0]) else { return ret(0) };
    let (id, ids) = (a[2] as i32, m.mem.r8(p + rec::ID_COUNT) as i32);
    ret(match a[1] {
        1 => m.mem.r8(p + rec::KIND) as u32,
        2 => m.mem.r16(p + rec::EX_ID) as u32,
        3 => m.mem.r8(p + rec::PREV_MODE) as u32,
        4 if id < 0 => ids as u32,
        // The table's entries come from a pad in config mode; there are
        // none without one.
        4 if id < ids => m.mem.r16(m.mem.r32(p + rec::TABLES) + 2 * id as u32) as u32,
        100 => m.mem.r32(p + rec::FAILURES),
        _ => 0,
    })
}

/// `PadSetAct(port, table, size)`: the actuator table libpad sends from.
/// The original leaves the port's record in v0.
fn pad_set_act(m: &mut M, a: [u32; 4]) -> Result {
    let l = lib(m, at::PORT2INFO, data::PORT2INFO)?;
    let Some(p) = l.port(m, a[0]) else { return ret(0) };
    m.mem.w32(p + rec::ACT, a[1]);
    m.mem.w8(p + rec::ACT_SIZE, a[2] as u8);
    ret(p)
}

/// `PadSetActAlign(port, table)`: 1 when the engine takes the actuator
/// alignment command (its handlers, `pdcmd2.o`'s, for the info states),
/// 0 when it is busy.
fn pad_set_act_align(m: &mut M, a: [u32; 4]) -> Result {
    let l = lib(m, at::PORT2INFO, data::PORT2INFO)?;
    let Some(p) = l.port(m, a[0]) else { return ret(0) };
    if busy(m, p) {
        return ret(0);
    }
    let f = m.sdk.at;
    let handler = |m: &M, (hi, lo): (u32, u32)| {
        crate::libspu::pair(m, f + hi, f + lo).ok_or_else(|| format!("libpad/pdcmd2 at {f:#010x}: no handler where PsyQ 4.7's object has it"))
    };
    let (prepare, apply) = (handler(m, at::PREPARE)?, handler(m, at::APPLY)?);
    m.mem.w8(p + rec::CMD_STATE, CMD_CONFIG);
    m.mem.w32(p + rec::PREPARE, prepare);
    m.mem.w32(p + rec::ALIGN, a[1]);
    m.mem.w32(p + rec::APPLY, apply);
    ret(1)
}

/// btn's libpad as its code addresses it, for the tests: the calls'
/// operands, `pdresres.o`'s `.data` as the EXE loads it, and a double of
/// `_padInitDirPort` (guest code in a game).
#[cfg(test)]
pub(crate) mod fixture {
    use super::*;
    use crate::libetc::fixture::{jal_to, pair_at};

    const CALLS: &[(&str, u32)] = &[
        ("PadSetAct", 0x8004_5D24),
        ("PadSetActAlign", 0x8004_5D64),
        ("PadGetState", 0x8004_5ED4),
        ("PadInfoMode", 0x8004_5FA4),
        ("PadStartCom", 0x8004_60A4),
        ("PadStopCom", 0x8004_6170),
        ("PadInitDirect", 0x8004_61B4),
    ];
    const INIT_DIR_PORT: u32 = 0x8004_6264;
    const IS_VSYNC: u32 = 0x8004_6CA4;
    pub const DATA: u32 = 0x8005_E1C4;
    pub const REST: u32 = 0x800A_7E18;
    /// `pddirres.o`'s `.bss`: the records, then the receive and transmit
    /// buffers.
    pub const RECORDS: u32 = 0x800A_7B98;

    fn call_at(name: &str) -> u32 {
        CALLS.iter().find(|(n, _)| *n == name).unwrap().1
    }

    pub fn install(m: &mut M) {
        m.mem.w32(DATA + data::CHAN_STOP, 1);
        m.mem.write(DATA + data::FIX_RESULT, &[0xFF; 8]);
        m.mem.w32(DATA + data::VSYNC_PARAM + 4, IS_VSYNC + at::INT_PAD);
        m.mem.w32(DATA + data::VSYNC_PARAM + 8, IS_VSYNC);
        pair_at(m, IS_VSYNC + at::INT_PAD + at::REST.0, REST);
        let init = call_at("PadInitDirect");
        pair_at(m, init + at::INIT_EXEC.0, DATA + data::INT_EXEC);
        jal_to(m, init + at::JAL_INIT_PORT, INIT_DIR_PORT);
        for f in ["PadStartCom", "PadStopCom"] {
            pair_at(m, call_at(f) + at::ELEMENT.0, DATA + data::VSYNC_PARAM);
        }
        for f in ["PadGetState", "PadInfoMode", "PadSetAct", "PadSetActAlign"] {
            pair_at(m, call_at(f), DATA + data::PORT2INFO);
        }
        let align = call_at("PadSetActAlign");
        pair_at(m, align + at::PREPARE.0, align + 0x7C);
        pair_at(m, align + at::APPLY.0, align + 0xA4);
        crate::testing::double(INIT_DIR_PORT, init_dir_port);
    }

    /// The call `name`, at its place in btn.
    pub fn run(m: &mut M, name: &str, a: [u32; 4]) -> u32 {
        let f = ADAPTERS.iter().find(|(n, _)| *n == name).unwrap().1;
        m.sdk.at = call_at(name);
        crate::testing::call(m, f, a)
    }

    /// `_padInitDirPort`: the records cleared and pointed at their
    /// buffers, `_padInfoDir` set.
    fn init_dir_port(m: &mut M) {
        m.mem.write(RECORDS, &[0; 2 * rec::SIZE as usize]);
        for i in 0..2 {
            let p = RECORDS + rec::SIZE * i;
            m.mem.w32(p + rec::RX, RECORDS + 2 * rec::SIZE + RX * i);
            m.mem.w32(p + rec::TX, RECORDS + 2 * rec::SIZE + 0x48 + RX * i);
        }
        m.mem.w32(DATA + data::INFO_DIR, RECORDS);
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{DATA, RECORDS, REST, install, run};
    use super::*;
    use crate::testing::{machine_with_bus, vblanks};

    const B0: u32 = 0x8010_0000;
    const B1: u32 = 0x8010_0040;
    const ELEMENT: u32 = DATA + data::VSYNC_PARAM;

    fn buf(m: &M, a: u32) -> [u8; 4] {
        let mut b = [0; 4];
        m.mem.read(a, &mut b);
        b
    }

    fn started() -> M {
        let mut m = machine_with_bus();
        install(&mut m);
        m.mem.write(B0, &[0xAA; 0x80]);
        assert_eq!(run(&mut m, "PadInitDirect", [B0, B1, 0, 0]), 1);
        assert_eq!(buf(&m, B0), [0xFF, 0, 0xAA, 0xAA]);
        assert_eq!(run(&mut m, "PadStartCom", [0; 4]), 1);
        m
    }

    fn state(m: &mut M, port: u32) -> u32 {
        run(m, "PadGetState", [port, 0, 0, 0])
    }

    fn info(m: &mut M, port: u32, mode: u32) -> u32 {
        run(m, "PadInfoMode", [port, mode, 0, 0])
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
        assert_eq!(m.mem.r32(DATA + data::FIX_RESULT), 0, "pending, in _padFixResult");
        vblanks(&mut m, 1);
        assert_eq!(buf(&m, B0), [0, 0x41, 0xFF, 0xEF]);
        assert_eq!(state(&mut m, 0), 1);
        assert_eq!(info(&mut m, 0, 1), 4);
        assert_eq!(m.mem.r8(RECORDS + rec::CURRENT), CONFIG, "in port 1's record");
        assert_eq!(m.mem.r32(DATA + data::CALLED), 1);
        // The handler's waits end at the pass that sees what they wait for
        // (D66): a refused command and nothing connected take 628 passes.
        // This machine has no guest budget, so the passes are its time.
        let spent = |m: &M| m.clock.now - m.clock.video.vblank_at(m.vblanks);
        assert_eq!(spent(&m), 12_560);
        for _ in 0..11 {
            assert_eq!(state(&mut m, 0), 1);
            vblanks(&mut m, 1);
        }
        assert_eq!(m.mem.r8(RECORDS + rec::STATE), STATE_FIND_CTP1);
        vblanks(&mut m, 1);
        assert_eq!(state(&mut m, 0), 1, "the poll's reply is pending");
        // A poll and nothing connected: 642 passes.
        assert_eq!(spent(&m), 12_840);
        vblanks(&mut m, 1);
        assert_eq!(state(&mut m, 0), 2);
        assert_eq!(info(&mut m, 0, 100), 12);
        assert_eq!(info(&mut m, 0x10, 1), 0);
        assert_eq!(info(&mut m, 0x10, 100), 15, "port 2 fails every vblank");
        assert_eq!(state(&mut m, 0x10), 0);
        assert_eq!(run(&mut m, "PadSetActAlign", [0, B1, 0, 0]), 0);
        assert_eq!(run(&mut m, "PadSetAct", [0x10, B1, 2, 0]), RECORDS + rec::SIZE, "port 2's record");
        // The status byte reads as busy.
        m.mem.w8(B0, 1);
        assert_eq!(state(&mut m, 0), 1);
    }

    #[test]
    fn stop_and_restart() {
        let mut m = started();
        vblanks(&mut m, 3);
        assert_eq!(run(&mut m, "PadStopCom", [0; 4]), 0);
        assert!(!m.chain(2).contains(&ELEMENT));
        m.mem.write(B0, &[9; 4]);
        vblanks(&mut m, 2);
        assert_eq!(buf(&m, B0), [9; 4], "nothing polls");
        run(&mut m, "PadStartCom", [0; 4]);
        assert_eq!(m.mem.r8(RECORDS + rec::STATE), 0, "forgotten");
        assert_eq!(m.chain(2), vec![ELEMENT], "libpad's own element");
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
        assert_eq!(m.mem.r32(DATA + data::FIX_RESULT), 0);
        m.mem.write(B1 + 0x10, &[7; 4]);
        run(&mut m, "PadInitDirect", [B1 + 0x10, 0, 0, 0]);
        vblanks(&mut m, 1);
        assert_eq!(buf(&m, B1 + 0x10), [0, 0x41, 7, 7]);
    }

    #[test]
    fn masked_vblank_polls_nothing() {
        let mut m = started();
        let mask = m.io.as_ref().unwrap().irq.mask;
        set_reg(&mut m, I_MASK, 4, mask & !1).unwrap();
        vblanks(&mut m, 3);
        assert_eq!(info(&mut m, 0x10, 100), 0);
    }

    /// Where the guest's patched functions note their turn.
    const LOG: u32 = 0x8010_0200;
    const START_DEQUEUE: u32 = 0x8010_0100;
    const START_ENQUEUE: u32 = 0x8010_0104;
    const START_CHANGE_CLEAR: u32 = 0x8010_0108;
    const STOP_CHANGE_CLEAR: u32 = 0x8010_010C;
    const STOP_DEQUEUE: u32 = 0x8010_0110;

    fn note(m: &mut M, v: u32) {
        let n = m.mem.r32(LOG);
        m.mem.w32(LOG + 4 + 4 * n, v);
        m.mem.w32(LOG, n + 1);
    }

    fn notes(m: &M) -> Vec<u32> {
        let n = m.mem.r32(LOG);
        (0..n).map(|i| m.mem.r32(LOG + 4 + 4 * i)).collect()
    }

    /// The arguments the guest call being served got.
    fn args() -> [u32; 4] {
        *crate::testing::irq_args().last().expect("a guest call's arguments")
    }

    fn unmasked(m: &M) -> u32 {
        m.io.as_ref().unwrap().irq.mask & 1
    }

    fn patch(m: &mut M, func: u32, addr: u32) {
        m.mem.w32(psx_bios::tables::entry(0xC0, func).expect("a C0h table entry"), addr);
    }

    /// A patch of `PadStartCom`'s `SysDeqIntRP`: nothing after the call has
    /// run yet.
    fn start_dequeue(m: &mut M) {
        note(m, 0x03);
        assert_eq!(args(), [PRIORITY, ELEMENT, 0, 0]);
        assert!(m.crit, "PadStartCom entered its critical section");
        assert_eq!(m.mem.r32(DATA + data::INT_EXEC), 0, "_padIntExec cleared before the call");
        assert_eq!(m.mem.r8(RECORDS + rec::STATE), 7, "the ports are not forgotten yet");
    }

    /// A patch of `PadStartCom`'s `SysEnqIntRP`: guest code that enters
    /// another adapter (the runtime points `sdk.at` at it) and leaves it
    /// there.
    fn start_enqueue(m: &mut M) {
        note(m, 0x02);
        assert_eq!(args(), [PRIORITY, ELEMENT, 0, 0]);
        assert_eq!(unmasked(m), 0, "vblank is unmasked only after the enqueue");
        assert_eq!(m.mem.r32(DATA + data::INT_EXEC), 0);
        run(m, "PadGetState", [0, 0, 0, 0]);
    }

    /// A patch of `PadStartCom`'s `ChangeClearRCnt(3, 0)`: vblank already
    /// acknowledged and unmasked (the original writes `I_MASK` in this
    /// call's delay slot), the ports and the engine's flag not yet touched.
    fn start_change_clear(m: &mut M) {
        note(m, 0x0A);
        assert_eq!(args(), [3, 0, 0, 0]);
        assert_eq!(unmasked(m), 1);
        assert_eq!(m.io.as_ref().unwrap().irq.stat & 1, 0, "vblank acknowledged");
        assert_eq!(m.mem.r32(DATA + data::INT_EXEC), 0, "the engine restarts after the call");
        assert_eq!(m.mem.r8(RECORDS + rec::STATE), 7);
        assert!(m.crit);
    }

    /// `PadStopCom`'s calls: `ChangeClearRCnt(3, 1)`, then the dequeue.
    fn stop_change_clear(m: &mut M) {
        note(m, 0x0A);
        assert_eq!(args(), [3, 1, 0, 0]);
        assert_eq!(unmasked(m), 1);
        assert!(m.crit);
    }

    fn stop_dequeue(m: &mut M) {
        note(m, 0x03);
        assert_eq!(args(), [PRIORITY, ELEMENT, 0, 0]);
        assert!(m.crit, "the critical section is left after the dequeue");
    }

    /// The patched entries run where the original calls them: each guest
    /// function finishes before the state that follows it, a callback that
    /// enters another adapter does not move the engine's addresses, and the
    /// native enqueue/dequeue do not also run.
    #[test]
    fn patched_start_and_stop_finish_their_calls_first() {
        let mut m = machine_with_bus();
        install(&mut m);
        m.mem.write(B0, &[0xAA; 0x80]);
        assert_eq!(run(&mut m, "PadInitDirect", [B0, B1, 0, 0]), 1);
        let chain = m.chain(2);
        m.mem.w8(RECORDS + rec::STATE, 7);
        m.mem.write(REST, &[9; 8]);
        crate::testing::double(START_DEQUEUE, start_dequeue);
        crate::testing::double(START_ENQUEUE, start_enqueue);
        crate::testing::double(START_CHANGE_CLEAR, start_change_clear);
        patch(&mut m, 0x03, START_DEQUEUE);
        patch(&mut m, 0x02, START_ENQUEUE);
        patch(&mut m, 0x0A, START_CHANGE_CLEAR);
        assert!(!m.crit, "the adapter, not the caller, must enter critical");
        assert_eq!(run(&mut m, "PadStartCom", [0; 4]), 1);
        assert_eq!(notes(&m), [0x03, 0x02, 0x0A], "the original's order");
        assert!(!m.crit, "the critical section is left after the last call");
        assert_eq!(m.mem.r32(DATA + data::INT_EXEC), 1, "the engine restarts at the end");
        assert_eq!(m.mem.r8(RECORDS + rec::STATE), 0, "the ports forgotten at the end");
        assert_eq!(m.mem.bytes(REST, 8), [0; 8], "the rests over at the end");
        assert_eq!(m.chain(2), chain, "the guest's enqueue runs, not the native service");

        // The stop side: ChangeClearRCnt(3, 1) then the dequeue, the
        // critical section left after both.
        m.mem.w32(LOG, 0);
        crate::testing::double(STOP_CHANGE_CLEAR, stop_change_clear);
        crate::testing::double(STOP_DEQUEUE, stop_dequeue);
        patch(&mut m, 0x0A, STOP_CHANGE_CLEAR);
        patch(&mut m, 0x03, STOP_DEQUEUE);
        assert!(!m.crit, "PadStopCom must enter critical too");
        assert_eq!(run(&mut m, "PadStopCom", [0; 4]), 0);
        assert_eq!(notes(&m), [0x0A, 0x03]);
        assert!(!m.crit, "ExitCriticalSection's 0, after the dequeue");
        assert_eq!(m.chain(2), chain, "the guest's dequeue runs, not the native service");
    }
}
