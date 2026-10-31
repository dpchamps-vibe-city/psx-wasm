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
//! - **Everything takes the original's time** (D60, D66, D71). The calls
//!   and the interrupt spend their originals' blocks on the paths they
//!   take, the verifier on every interrupt the kernel walks the chains
//!   for. In the interrupt ([`Int`]) each block is spent before what it
//!   does, so its busy-waits' polls of SIO0 and root counter 2 see the
//!   cycle after the block that reads them, and a wait ends at the pass
//!   that sees what it waits for: about 14,000 cycles for a poll and an
//!   empty port. A quantum passes (nothing delivered) only at one of its
//!   loop headers once the budget has run out, as at the original's
//!   safepoints. Games see it: bof4's `title-to-field` falls hundreds of
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
//! command (actuator levels, `PadSetAct`) are computed, as the paths that
//! compute them take time, but a digital pad ignores them. The engine's
//! own bookkeeping words aren't written (`_padSioChan`, `_padSioState`,
//! the data count), nor SIO0's registers: the port is the pad model's.
//! Multitap and light-gun paths aren't linked in these games; neither are
//! the info states' requests and replies of a pad in config mode, which
//! spend nothing past their dispatch.

use crate::{Adapter, M, Result, bios, reg, ret, set_reg};
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
    /// A flag direct connection never sets: with it, a pending request
    /// skips the exchange, and a reply without data keeps the pad.
    pub const HOLD: u32 = 0x50;
    /// The actuators' gates, six bytes `_padFuncCurrLimit` opens.
    pub const GATES: u32 = 0x57;
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
    /// `_padFuncCurrLimit`.
    pub const FUNC_CURR_LIMIT: u32 = 0x14;
    pub const PORT2INFO: u32 = 0x1C;
    /// The port records (`_padInfoDir`).
    pub const INFO_DIR: u32 = 0x3C;
    pub const INT_EXEC: u32 = 0x40;
    /// `_padTotalCurr`: the actuators' budget spent this vblank.
    pub const TOTAL_CURR: u32 = 0x50;
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

/// The element's verifier (`_padIsVsync`, `pdresres.o` +0h), on every
/// interrupt the kernel walks the chains for: vblank unmasked and pending.
/// Then its handler as the original runs it ([`Int`]).
pub fn irq_step(m: &mut M, _step: u32) {
    let Some(l) = m.sdk.pad.lib else { return };
    let vblank = |m: &mut M, r: u32| reg(m, r, 4).is_ok_and(|v| v & 1 != 0);
    crate::irq_path(m, &[(0x0, 0x20)]);
    let yes = vblank(m, I_MASK) && {
        crate::irq_path(m, &[(0x24, 0x34)]);
        vblank(m, I_STAT)
    };
    // `_padFuncIntGun`, a light gun's, is null in direct connection.
    if yes {
        crate::irq_path(m, &[(0x38, 0x48), (0x54, 0x54)]);
    }
    crate::irq_path(m, &[(0x58, 0x64)]);
    if yes {
        Int::new(m, l).int_pad();
    }
}

/// SIO0 at libpad's rate (baud 88h, factor 1): cycles a byte takes; from
/// its end to a pad's /ACK, and how long /ACK stays low
/// (`psx_devices::sio`).
const BYTE: u64 = 8 * 0x88;
const ACK_DELAY: u64 = 0x40;
const ACK_LEN: u64 = 32;
/// Root counter 2's counter; its mode and target follow, 4 and 8 on.
const RC2: u32 = 0x1F80_1120;

/// libpad's interrupt as the original runs it (PsyQ 4.7 `pdresres.o`, and
/// `pddirres.o` where a method says so; direct connection): each of its
/// blocks spent before what the block does (D71, offsets in the object's
/// text), so a register the block reads is read at the time after it
/// (`Machine::guest_now`, D66), and a quantum passes only at one of its loop
/// headers ([`crate::irq_header`]). Root counter 2 is read as the original
/// reads it; SIO0 is the pad model's wire, its replies, received bytes and
/// /ACKs at the cycles SIO0's device gives them.
struct Int<'a> {
    m: &'a mut M,
    l: Lib,
    recs: u32,
    /// `_padSioChan`, `_padSioState`, and the data bytes left (`.data+84h`).
    chan: u32,
    state: u32,
    left: i32,
    /// `_padSetRC2wait`'s counter and ticks.
    wait: (i32, i32),
    /// The selected port's device, and when it was selected.
    wire: Wire,
    selected: u64,
    /// The byte in flight: when it went out, its reply, and when that is
    /// in; JOY_DATA.
    sent: Option<u64>,
    reply: u8,
    ready: Option<u64>,
    data: u8,
    /// The /ACKs, and when IRQ7 was last acknowledged in I_STAT.
    acks: Vec<u64>,
    cleared: u64,
}

impl<'a> Int<'a> {
    fn new(m: &'a mut M, l: Lib) -> Self {
        let recs = l.records(m).unwrap_or(0);
        Int {
            m,
            l,
            recs,
            chan: 0,
            state: 0,
            left: 0,
            wait: (0, 0),
            wire: Wire::new(None),
            selected: 0,
            sent: None,
            reply: 0,
            ready: None,
            data: 0,
            acks: Vec::new(),
            cleared: 0,
        }
    }

    /// The original's block at `a`..=`b`.
    fn at(&mut self, a: u32, b: u32) {
        crate::irq_path(self.m, &[(a, b)]);
    }

    fn header(&mut self) {
        crate::irq_header(self.m);
    }

    fn now(&self) -> u64 {
        self.m.guest_now()
    }

    fn r8(&self, a: u32) -> u8 {
        self.m.mem.r8(a)
    }

    fn w8(&mut self, a: u32, v: u8) {
        self.m.mem.w8(a, v)
    }

    fn rec(&self, chan: u32) -> u32 {
        self.recs + rec::SIZE * chan
    }

    /// The first byte of `p`'s receive buffer: the last reply's ID.
    fn id(&self, p: u32) -> u8 {
        self.r8(self.m.mem.r32(p + rec::RX))
    }

    /// A root counter 2 register (0: counter, 4: mode, 8: target).
    fn counter(&mut self, r: u32) -> i32 {
        reg(self.m, RC2 + r, 2).unwrap_or(0) as i32
    }

    // --- The wire ------------------------------------------------------------

    /// SIO0 reset and port `chan` selected: its device starts over.
    fn select(&mut self) {
        self.wire = Wire::new(self.m.sdk.pad.latched.get(self.chan as usize).copied().flatten());
        self.selected = self.now();
        (self.sent, self.ready, self.data) = (None, None, 0);
    }

    /// JOY_CTRL 0: an /ACK still to come never does.
    fn deselect(&mut self) {
        let t = self.now();
        self.acks.retain(|&a| a <= t);
    }

    /// JOY_DATA written: `b` out; the reply in a byte's time, and an /ACK
    /// after it if the device acknowledges.
    fn send(&mut self, b: u8) {
        let t = self.now();
        let (reply, ack) = self.wire.byte(b);
        (self.sent, self.reply, self.ready) = (Some(t), reply, Some(t + BYTE));
        if ack {
            self.acks.push(t + BYTE + ACK_DELAY);
        }
    }

    /// JOY_DATA read: the last byte received.
    fn receive(&mut self) -> u8 {
        if self.ready.take().is_some_and(|r| self.now() >= r) {
            self.data = self.reply;
        }
        self.data
    }

    /// JOY_STAT: transmit ready, a byte received, /ACK low.
    fn tx_ready(&self) -> bool {
        self.sent.is_none_or(|s| self.now() >= s + BYTE)
    }

    fn rx_ready(&self) -> bool {
        self.ready.is_some_and(|r| self.now() >= r)
    }

    fn ack_low(&self) -> bool {
        let t = self.now();
        self.acks.iter().any(|&a| a >= self.selected && a <= t && t < a + ACK_LEN)
    }

    /// I_STAT's IRQ7: an /ACK since it was last acknowledged.
    fn irq7(&self) -> bool {
        let t = self.now();
        self.acks.iter().any(|&a| a > self.cleared && a <= t)
    }

    fn ack_irq7(&mut self) {
        self.cleared = self.now();
    }

    // --- Root counter 2 --------------------------------------------------------

    /// `_padSetRC2wait` (+14CCh): a wait of `ticks` from now.
    fn set_wait(&mut self, ticks: i32) {
        self.at(0x14CC, 0x14E8);
        self.wait = (self.counter(0), ticks);
    }

    /// The counter read in the block before, past its wrap as
    /// `_padChkRC2wait` takes it: plus the target, or 10000h with none
    /// (`test`'s block reads it, then `target`'s again or `none`'s).
    fn unwrap(&mut self, now: i32, [test, target, none]: [(u32, u32); 3]) -> i32 {
        if now >= self.wait.0 {
            return now;
        }
        self.at(test.0, test.1);
        if self.counter(8) == 0 {
            self.at(none.0, none.1);
            return now + 0x10000;
        }
        self.at(target.0, target.1);
        now + self.counter(8)
    }

    /// `_padChkRC2wait` (+14ECh): whether the wait is over (the counter
    /// counts a cycle in 8, or every cycle, its mode's bit 9).
    fn waited(&mut self) -> bool {
        self.at(0x14EC, 0x150C);
        let now = self.counter(0);
        let now = self.unwrap(now, [(0x1510, 0x1520), (0x1524, 0x152C), (0x1530, 0x1530)]);
        self.at(0x1534, 0x154C);
        let d = now - self.wait.0;
        let d = if self.counter(4) & 0x200 != 0 {
            self.at(0x156C, 0x157C);
            d
        } else {
            self.at(0x1550, 0x1568);
            d >> 3
        };
        self.at(0x1580, 0x1588);
        d >= self.wait.1
    }

    /// The wait for /ACK to rise again after IRQ7 is acknowledged, a loop
    /// of `call` (`_padChkRC2wait`), `test` and `read` (JOY_STAT): false if
    /// the counter's wait runs out first.
    fn ack_rises(&mut self, [call, test, read]: [(u32, u32); 3]) -> bool {
        while self.ack_low() {
            self.header();
            self.at(call.0, call.1);
            let out = self.waited();
            self.at(test.0, test.1);
            if out {
                return false;
            }
            self.at(read.0, read.1);
        }
        true
    }

    // --- pdresres.o ----------------------------------------------------------------

    /// `_padIntPad` (+68h): the rests counted; with the engine on, the first
    /// port selected (`_padInitSioMode`), then the handler states until
    /// every port has had its exchange (`_padFuncNextPort` selects the
    /// next). JOY_CTRL's DTR is off: nothing else holds the port at the
    /// vblank.
    fn int_pad(&mut self) {
        let l = self.l;
        self.at(0x68, 0x88);
        self.at(0x94, 0xA8);
        self.m.mem.w32(l.data + data::CALLED, 1);
        let (start, stop) = (self.l.word(self.m, data::CHAN_START) as i32, self.l.word(self.m, data::CHAN_STOP) as i32);
        // A port the engine doesn't poll counts its rest, up to 150 vblanks.
        let rest = |x: &mut Self, r: u32, (a, b): (u32, u32), inc: u32| {
            x.at(a, b);
            let n = x.m.mem.r32(r) as i32;
            if n < 150 {
                x.at(inc, inc);
                x.m.mem.w32(r, (n + 1) as u32);
            }
        };
        if start != 0 {
            rest(self, l.rest, (0xAC, 0xC4), 0xC8);
        }
        self.at(0xCC, 0xDC);
        if stop == 0 {
            rest(self, l.rest + 4, (0xE0, 0xF8), 0xFC);
        }
        self.at(0x100, 0x110);
        let on = self.l.word(self.m, data::INT_EXEC) != 0 && {
            self.at(0x114, 0x130);
            start <= stop
        };
        if on && self.recs != 0 {
            self.at(0x134, 0x158);
            self.chan = start as u32;
            let p = self.rec(self.chan);
            let go = self.init_sio_mode(p);
            self.at(0x15C, 0x160);
            if !go {
                self.at(0x164, 0x174);
                self.next_port(0xFFFF);
            }
            self.at(0x178, 0x198);
            self.m.mem.w32(l.data + data::TOTAL_CURR, 0);
            while self.chan as i32 <= stop {
                self.header();
                self.at(0x19C, 0x1B4);
                let p = self.rec(self.chan);
                self.step(p);
                self.at(0x1B8, 0x1D4);
            }
            self.at(0x1D8, 0x1E4);
        }
        self.at(0x1E8, 0x1F4);
    }

    /// `_padInitSioMode` (+1F8h): SIO0 reset, a wait on root counter 2
    /// started, `p`'s port selected, its pending reply processed; whether
    /// its exchange goes ahead. (A multitap's children's replies, +290h,
    /// and an interrupt pending at the port, +374h, aren't direct
    /// connection's: the reset has just cleared the latter.)
    fn init_sio_mode(&mut self, p: u32) -> bool {
        self.at(0x1F8, 0x23C);
        let tap = self.r8(p + rec::KIND) == 8;
        if tap {
            self.at(0x240, 0x240);
        }
        self.at(0x244, 0x248);
        self.set_wait(if tap { 0x50 } else { 0x91 });
        self.at(0x24C, 0x260);
        if self.chan != 0 {
            self.at(0x264, 0x264);
        }
        self.at(0x268, 0x284);
        self.select();
        let fix = self.l.fix(self.chan);
        if self.m.mem.r32(fix) as i32 >= 0 {
            self.at(0x288, 0x28C);
            self.at(0x300, 0x324);
            if self.m.mem.r32(fix) == 0 {
                self.at(0x328, 0x33C);
                self.m.mem.w32(fix, u32::MAX);
                self.recv_auto(p);
                self.at(0x340, 0x350);
                self.clr_cmd_no(p);
            }
        }
        self.at(0x354, 0x370);
        self.at(0x4F4, 0x500);
        let go = self.r8(p + rec::HOLD) == 0 || {
            self.at(0x504, 0x510);
            self.r8(p + rec::REQUEST) == 0 && {
                self.at(0x514, 0x514);
                true
            }
        };
        self.at(0x518, 0x528);
        go
    }

    /// The handler state the engine is in, for `p` (+52Ch); then, after a
    /// byte has gone out, IRQ7 acknowledged and /ACK seen to rise
    /// (`_padClrIntSio0`), and an error's port given up.
    fn step(&mut self, p: u32) {
        self.at(0x52C, 0x564);
        let s = self.state;
        self.state += 1;
        let r = match s {
            0 => self.select_byte(p),
            1 => self.command(p),
            2 => self.tap(p),
            3 => self.first_data(p),
            _ => self.data_bytes(p),
        };
        self.at(0x568, 0x570);
        if r < 0 {
            self.at(0x5F8, 0x608);
            self.next_port(r);
            self.at(0x60C, 0x618);
            return;
        }
        self.at(0x574, 0x584);
        if self.state != 0 {
            self.at(0x588, 0x58C);
            let tap = self.state == 3 && {
                self.at(0x590, 0x5A4);
                self.id(p) == 0x80
            };
            if !tap {
                self.at(0x5A8, 0x5AC);
                self.set_wait(0x3C);
                self.at(0x5B0, 0x5B4);
                let ok = self.clr_int();
                self.at(0x5B8, 0x5BC);
                if !ok {
                    self.at(0x5C0, 0x5D0);
                    self.next_port(-3);
                }
            }
            self.at(0x5D4, 0x5DC);
        }
        self.at(0x5E0, 0x5E8);
        if self.state >= 5 {
            self.at(0x5EC, 0x5F4);
            self.state -= 1;
        }
        self.at(0x60C, 0x618);
    }

    /// State 0 (+158Ch): the next command chosen (`_padFuncSendAuto`, which
    /// in direct connection names no multitap children), the receive buffer's
    /// ID cleared, select (01h) out.
    fn select_byte(&mut self, p: u32) -> i32 {
        self.at(0x158C, 0x15A4);
        self.send_auto(p);
        self.at(0x15A8, 0x15C0);
        let rx = self.m.mem.r32(p + rec::RX);
        self.w8(rx, 0);
        let r = self.sio_rw(p, -2);
        self.at(0x15C4, 0x15D0);
        r
    }

    /// State 1 (+15D4h): the command out, a poll unless one is requested.
    /// (`_padGunExec`'s light gun isn't linked in direct connection.)
    fn command(&mut self, p: u32) -> i32 {
        self.at(0x15D4, 0x15F4);
        if self.chan == self.l.word(self.m, data::CHAN_START) {
            self.at(0x15F8, 0x1608);
        }
        self.at(0x1634, 0x1644);
        self.at(0x1678, 0x1684);
        let request = self.r8(p + rec::REQUEST);
        if request == 0 {
            self.at(0x1688, 0x168C);
        } else {
            self.at(0x1690, 0x1690);
        }
        self.at(0x1694, 0x1698);
        let r = self.sio_rw2(p, if request != 0 { request } else { POLL });
        self.at(0x169C, 0x16A8);
        r
    }

    /// State 2 (+16ACh): the ID in, which says how many data bytes follow;
    /// the TAP byte out.
    fn tap(&mut self, p: u32) -> i32 {
        self.at(0x16AC, 0x16C4);
        self.at(0x16F8, 0x1704);
        let tx = if self.r8(p + rec::REQUEST) != 0 {
            0
        } else {
            self.at(0x1708, 0x170C);
            self.l.word(self.m, data::MODE_MTAP) as u8
        };
        self.at(0x1710, 0x1714);
        let id = self.sio_rw2(p, tx);
        self.at(0x1718, 0x171C);
        if id >= 0 {
            self.at(0x1720, 0x1734);
            self.left = (id & 0xF) << 1;
            if self.left == 0 {
                self.at(0x1738, 0x1744);
                self.left = 32;
            }
        }
        self.at(0x1748, 0x1754);
        id.min(0)
    }

    /// State 3 (+1758h): 5Ah in; the first data byte out, its actuator
    /// levels set first for a poll (`_padFuncCurrLimit`).
    fn first_data(&mut self, p: u32) -> i32 {
        self.at(0x1758, 0x1774);
        if self.l.word(self.m, data::MODE_MTAP) != 0 {
            self.at(0x1778, 0x1790);
            // A multitap's ID (80h), which this wire never answers.
            if self.id(p) >> 4 == 8 {
                self.at(0x1794, 0x179C);
            }
        }
        self.at(0x17A0, 0x17A8);
        self.at(0x17AC, 0x17B8);
        if self.r8(p + rec::REQUEST) == 0 {
            self.at(0x17BC, 0x17C8);
            if self.r8(p + rec::CURRENT) == 0 {
                self.at(0x17CC, 0x17D8);
                let root = self.m.mem.r32(p + rec::ROOT) == p || {
                    self.at(0x17DC, 0x17E8);
                    self.r8(p + rec::PREVIOUS) == 0
                };
                if root {
                    self.at(0x17EC, 0x1800);
                    let buf = self.m.mem.r32(p + rec::BUF);
                    if self.r8(buf) == 0 {
                        self.at(0x1804, 0x1814);
                        self.curr_limit(p);
                    }
                }
            }
        }
        self.at(0x1818, 0x1830);
        let tx = self.get_txd(p);
        self.at(0x1834, 0x183C);
        let r = self.sio_rw2(p, tx);
        self.at(0x1840, 0x184C);
        let r = match r {
            0x5A => r,
            _ => {
                self.at(0x1850, 0x1854);
                match r {
                    0 => 0,
                    _ => {
                        self.at(0x1858, 0x185C);
                        if r >= 0 {
                            -4
                        } else {
                            self.at(0x1860, 0x1860);
                            r
                        }
                    }
                }
            }
        };
        self.at(0x1864, 0x1870);
        r
    }

    /// State 4 (+1874h), direct connection: the data bytes, each sent once
    /// the last one's /ACK is in, and its /ACK seen to rise again; the
    /// other port's pending reply processed alongside, while it has one;
    /// the last byte read in, then the next port (`_padFuncNextPort`).
    fn data_bytes(&mut self, p: u32) -> i32 {
        self.at(0x1874, 0x18A4);
        self.at(0x1914, 0x1924);
        self.at(0x1A9C, 0x1ABC);
        let other = (self.chan == 0) as u32;
        if self.left >= 2 {
            self.at(0x1AC0, 0x1AE0);
            loop {
                self.header();
                self.at(0x1AE4, 0x1AF0);
                if (self.m.mem.r32(self.l.fix(other)) as i32) < 0 {
                    break;
                }
                // Direct connection's result is 0: the root's reply.
                self.at(0x1AF4, 0x1AF8);
                self.at(0x1B30, 0x1B3C);
                self.at(0x1B40, 0x1B48);
                self.at(0x1B4C, 0x1B50);
                self.at(0x1B54, 0x1B58);
                self.at(0x1B5C, 0x1B60);
                self.at(0x1B94, 0x1BB4);
                let q = self.rec(other);
                self.recv_auto(q);
                self.at(0x1BB8, 0x1BC8);
                self.clr_cmd_no(q);
                self.at(0x1BCC, 0x1BD4);
                self.m.mem.w32(self.l.fix(other), u32::MAX);
                self.at(0x1BD8, 0x1BE8);
                let tx = self.get_txd(p);
                self.at(0x1BEC, 0x1BF4);
                let r = self.sio_rw(p, tx as i32);
                self.at(0x1BF8, 0x1BFC);
                if r < 0 {
                    self.at(0x1E40, 0x1E64);
                    return r;
                }
                self.at(0x1C00, 0x1C40);
                self.wait = (self.counter(0), 60);
                self.ack_irq7();
                if !self.ack_rises([(0x1C44, 0x1C48), (0x1C4C, 0x1C50), (0x1C54, 0x1C70)]) {
                    self.at(0x1C90, 0x1C94);
                    self.at(0x1E40, 0x1E64);
                    return -3;
                }
                self.at(0x1C74, 0x1C8C);
                self.at(0x1C90, 0x1C94);
                self.at(0x1C98, 0x1CB8);
                self.left -= 1;
                if self.left < 2 {
                    break;
                }
            }
        }
        self.at(0x1CBC, 0x1CD8);
        self.left -= 1;
        if self.left > 0 {
            self.at(0x1CDC, 0x1CE8);
            loop {
                self.header();
                self.at(0x1CEC, 0x1CFC);
                let tx = self.get_txd(p);
                self.at(0x1D00, 0x1D08);
                let r = self.sio_rw(p, tx as i32);
                self.at(0x1D0C, 0x1D10);
                if r < 0 {
                    self.at(0x1E40, 0x1E64);
                    return r;
                }
                // The baud is 88h: the /ACK's rise is waited for.
                self.at(0x1D14, 0x1D2C);
                self.at(0x1D30, 0x1D60);
                self.wait = (self.counter(0), 60);
                self.ack_irq7();
                let ok = self.ack_rises([(0x1D64, 0x1D68), (0x1D6C, 0x1D70), (0x1D74, 0x1D90)]);
                if ok {
                    self.at(0x1D94, 0x1DAC);
                }
                self.at(0x1DB0, 0x1DB4);
                if !ok {
                    self.at(0x1DB8, 0x1DBC);
                    self.at(0x1E40, 0x1E64);
                    return -3;
                }
                self.at(0x1DC0, 0x1DDC);
                self.left -= 1;
                if self.left <= 0 {
                    break;
                }
            }
        }
        self.at(0x1DE0, 0x1DE8);
        loop {
            self.header();
            self.at(0x1DEC, 0x1DFC);
            if self.rx_ready() {
                break;
            }
        }
        self.at(0x1E00, 0x1E38);
        let b = self.receive();
        let n = self.r8(p + rec::RX_COUNT);
        self.w8(p + rec::RX_COUNT, n.wrapping_add(1));
        let rx = self.m.mem.r32(p + rec::RX);
        self.w8(rx + n as u32, b);
        self.next_port(0);
        self.at(0x1E3C, 0x1E3C);
        self.at(0x1E40, 0x1E64);
        0
    }

    /// `_padSioRW(p, data)` (+61Ch). `data < 0`: select (its complement)
    /// out once the counter's wait is over, the reply not to be kept.
    /// Otherwise: the last byte's reply in, `data` out once its /ACK is
    /// (-14h if root counter 2's 1AEh ticks pass first), the reply kept.
    fn sio_rw(&mut self, p: u32, data: i32) -> i32 {
        self.at(0x61C, 0x63C);
        if data < 0 {
            self.at(0x640, 0x684);
            let r = self.receive();
            self.w8(p + rec::RX_COUNT, 0xFF);
            self.w8(p + rec::TX_COUNT, 1);
            let tx = self.m.mem.r32(p + rec::TX);
            self.w8(tx, !data as u8);
            while !self.tx_ready() {
                self.header();
                self.at(0x688, 0x698);
            }
            loop {
                self.header();
                self.at(0x69C, 0x6A0);
                let over = self.waited();
                self.at(0x6A4, 0x6A8);
                if over {
                    break;
                }
            }
            self.at(0x6AC, 0x6C0);
            self.send(!data as u8);
            self.at(0x824, 0x83C);
            return r as i32;
        }
        self.at(0x6C4, 0x6DC);
        // A multitap's ID (80h), which this wire never answers, would
        // switch the baud.
        if self.id(p) >> 4 == 8 {
            self.at(0x6E0, 0x6F0);
            if self.r8(p + rec::RX_COUNT) >= 9 {
                self.at(0x6F4, 0x6F8);
            }
        } else {
            self.at(0x6F8, 0x6F8);
        }
        self.at(0x6FC, 0x740);
        self.wait = (self.counter(0), 0x1AE);
        self.counter(4);
        while !self.rx_ready() {
            self.header();
            self.at(0x744, 0x754);
        }
        self.at(0x758, 0x780);
        let r = self.receive();
        if !self.irq7() {
            loop {
                self.header();
                self.at(0x784, 0x788);
                let over = self.waited();
                self.at(0x78C, 0x790);
                if over {
                    self.at(0x824, 0x83C);
                    return -0x14;
                }
                self.at(0x794, 0x7B0);
                if self.irq7() {
                    break;
                }
            }
        }
        self.at(0x7B4, 0x7CC);
        self.send(data as u8);
        self.at(0x7F4, 0x820);
        let n = self.r8(p + rec::TX_COUNT);
        self.w8(p + rec::TX_COUNT, n.wrapping_add(1));
        let n = self.r8(p + rec::RX_COUNT);
        let rx = self.m.mem.r32(p + rec::RX);
        self.w8(rx + n as u32, r);
        self.w8(p + rec::RX_COUNT, n.wrapping_add(1));
        self.at(0x824, 0x83C);
        r as i32
    }

    /// `_padSioRW2(p, data)` (+840h): the last byte's reply in, then its
    /// /ACK (-2 if root counter 2's 190h ticks pass first); after the
    /// command's, 3Ch ticks more; `data` out, the reply kept.
    fn sio_rw2(&mut self, p: u32, data: u8) -> i32 {
        self.at(0x840, 0x878);
        if self.id(p) >> 4 == 8 {
            self.at(0x87C, 0x88C);
            if self.r8(p + rec::RX_COUNT) >= 9 {
                self.at(0x890, 0x890);
            }
        }
        self.at(0x894, 0x89C);
        loop {
            self.header();
            self.at(0x8A0, 0x8B0);
            if self.rx_ready() {
                break;
            }
        }
        self.at(0x8B4, 0x8B8);
        self.set_wait(0x190);
        self.at(0x8BC, 0x8D8);
        let r = self.receive();
        if self.r8(p + rec::RX_COUNT) == 0 {
            self.at(0x8DC, 0x8E8);
        }
        if self.r8(p + rec::RX_COUNT) == 0 && r >> 4 == 8 {
            self.at(0x8F8, 0x8F8);
        } else {
            self.at(0x8EC, 0x8F4);
        }
        self.at(0x8FC, 0x918);
        if !self.irq7() {
            self.at(0x91C, 0x948);
            loop {
                self.header();
                self.at(0x94C, 0x964);
                self.counter(4);
                let now = self.counter(0);
                let now = self.unwrap(now, [(0x968, 0x974), (0x978, 0x980), (0x984, 0x984)]);
                self.at(0x988, 0x998);
                let d = now - self.wait.0;
                let out = if self.counter(4) & 0x200 != 0 {
                    self.at(0x99C, 0x9A4);
                    d >= self.wait.1 && {
                        self.at(0x9A8, 0x9AC);
                        true
                    }
                } else {
                    self.at(0x9B0, 0x9BC);
                    d >> 3 >= self.wait.1
                };
                if out {
                    self.at(0xAA0, 0xAB8);
                    return -2;
                }
                self.at(0x9C0, 0x9D0);
                if self.irq7() {
                    break;
                }
            }
        }
        self.at(0x9D4, 0x9E0);
        if self.r8(p + rec::KIND) != 8 {
            self.at(0x9E4, 0x9F4);
            if self.state == 2 {
                self.at(0x9F8, 0x9FC);
                self.set_wait(0x3C);
                loop {
                    self.header();
                    self.at(0xA00, 0xA04);
                    let over = self.waited();
                    self.at(0xA08, 0xA0C);
                    if over {
                        break;
                    }
                }
            }
        }
        self.at(0xA10, 0xA30);
        self.send(data);
        if self.state == 3 {
            self.at(0xA34, 0xA38);
            if r == 0x80 {
                self.at(0xA3C, 0xA5C);
                self.ack_irq7();
            }
        }
        self.at(0xA60, 0xA78);
        let n = self.r8(p + rec::TX_COUNT);
        self.w8(p + rec::TX_COUNT, n.wrapping_add(1));
        let n = self.r8(p + rec::RX_COUNT);
        if n != 0xFF {
            self.at(0xA7C, 0xA8C);
            let rx = self.m.mem.r32(p + rec::RX);
            self.w8(rx + n as u32, r);
        }
        self.at(0xA90, 0xA9C);
        self.w8(p + rec::RX_COUNT, n.wrapping_add(1));
        self.at(0xAA0, 0xAB8);
        r as i32
    }

    /// `_padClrIntSio0` (+ABCh): IRQ7 acknowledged, and /ACK seen to rise;
    /// false if the counter's wait runs out first.
    fn clr_int(&mut self) -> bool {
        self.at(0xABC, 0xAEC);
        self.ack_irq7();
        let ok = self.ack_rises([(0xAF0, 0xAF4), (0xAF8, 0xAFC), (0xB00, 0xB1C)]);
        if ok {
            self.at(0xB20, 0xB38);
        }
        self.at(0xB3C, 0xB48);
        ok
    }

    /// `_padCmdParaMode` (+1424h).
    fn para_mode(&mut self, p: u32, mode: u8) {
        self.at(0x1424, 0x1440);
        para_mode(self.m, p, mode);
    }

    // --- pddirres.o ------------------------------------------------------------------

    /// `_padFuncNextPort(code)` (`pddirres.o` +158h): the exchange's end
    /// (0: its reply pending in `_padFixResult`; else a failure), the port
    /// deselected; the next selected, and given up too if its exchange
    /// can't go ahead.
    fn next_port(&mut self, code: i32) {
        self.at(0x158, 0x184);
        let mut code = code;
        loop {
            self.header();
            self.at(0x188, 0x1A4);
            let p = self.rec(self.chan);
            if code != -9 {
                self.at(0x1A8, 0x1AC);
                if code == 0 {
                    self.at(0x1B0, 0x1B8);
                    self.m.mem.w32(self.l.fix(self.chan), 0);
                } else {
                    self.at(0x1BC, 0x1C0);
                    self.fail_auto(p);
                    self.at(0x1C4, 0x1C8);
                    self.clr_cmd_no(p);
                }
            }
            self.at(0x1CC, 0x204);
            self.deselect();
            (self.state, self.chan) = (0, self.chan + 1);
            let go = if self.chan as i32 <= self.l.word(self.m, data::CHAN_STOP) as i32 {
                self.at(0x208, 0x218);
                let go = self.init_sio_mode(self.rec(self.chan));
                self.at(0x21C, 0x220);
                go
            } else {
                self.at(0x224, 0x224);
                true
            };
            code = 0xFFFF;
            self.at(0x228, 0x22C);
            if go {
                break;
            }
        }
        self.at(0x230, 0x248);
    }

    /// `_dirFailAuto` (+930h): an exchange failed. A command state other than
    /// detection gives up after its eleventh failure in a row; config mode
    /// leaves the pad at FindCTP1, the others forget it. A port with
    /// nothing found has its buffer `FF 00`.
    fn fail_auto(&mut self, p: u32) {
        self.at(0x930, 0x950);
        let n = self.m.mem.r32(p + rec::FAILURES);
        self.m.mem.w32(p + rec::FAILURES, n.wrapping_add(1));
        let cmd = self.r8(p + rec::CMD_STATE);
        if cmd != CMD_DETECT {
            self.at(0x954, 0x95C);
            let fails = self.r8(p + rec::FAILS);
            if cmd == CMD_CONFIG {
                self.at(0x960, 0x970);
            } else {
                self.at(0x988, 0x998);
            }
            if fails < RETRIES {
                self.at(0x99C, 0x9A0);
                self.w8(p + rec::FAILS, fails + 1);
                self.at(0xA00, 0xA0C);
                return;
            }
            if cmd == CMD_CONFIG {
                self.at(0x974, 0x984);
                self.w8(p + rec::STATE, STATE_FIND_CTP1);
                self.w8(p + rec::CMD_STATE, CMD_DONE);
                self.at(0xA00, 0xA0C);
                return;
            }
            self.at(0x9A4, 0x9B0);
            if self.r8(p + rec::STATE) != STATE_DISCON {
                self.at(0x9B4, 0x9C4);
                self.clr_info(p);
            }
        }
        self.at(0x9C8, 0x9DC);
        if self.id(p) != ID_CONFIG {
            self.at(0x9E0, 0x9FC);
            let buf = self.m.mem.r32(p + rec::BUF);
            self.m.mem.write(buf, &[0xFF, 0]);
            self.w8(p + rec::KIND, 0);
            self.w8(p + rec::BUF_LEN, 0);
        }
        self.at(0xA00, 0xA0C);
    }

    /// `_padFuncClrInfo` (+F0h).
    fn clr_info(&mut self, p: u32) {
        self.at(0xF0, 0xFC);
        if self.r8(p + rec::STATE) != STATE_DISCON {
            self.at(0x100, 0x13C);
            for _ in 0..6 {
                self.header();
                self.at(0x140, 0x14C);
            }
        }
        self.at(0x150, 0x154);
        clear_info(self.m, p);
    }

    /// `_padFuncClrCmdNo` (+24Ch).
    fn clr_cmd_no(&mut self, p: u32) {
        self.at(0x24C, 0x258);
        clear_command(self.m, p);
    }

    /// `_padFuncRecvAuto` (+6C4h): a completed exchange's reply into the
    /// game's buffer, and the detection step it makes. (Replies to the info
    /// and actuator requests, +8E8h, are a pad in config mode's, which a
    /// digital pad never is.)
    fn recv_auto(&mut self, p: u32) {
        self.at(0x6C4, 0x6EC);
        let (rx, buf) = (self.m.mem.r32(p + rec::RX), self.m.mem.r32(p + rec::BUF));
        let id = self.r8(rx);
        if id & 0xF0 == 0 {
            self.at(0x6F0, 0x720);
            self.m.mem.write(buf, &[0xFF, 0]);
            self.w8(p + rec::KIND, 0);
            self.w8(p + rec::BUF_LEN, 0);
            self.clr_info(p);
            self.at(0x724, 0x728);
            self.at(0x920, 0x92C);
            return;
        }
        self.at(0x72C, 0x740);
        let old = self.r8(p + rec::KIND);
        if id >> 4 == 0xF {
            self.at(0x744, 0x748);
        } else {
            self.w8(p + rec::KIND, id >> 4);
            self.at(0x74C, 0x77C);
            self.m.mem.write(buf, &[0, id]);
            let n = self.r8(p + rec::RX_COUNT);
            self.w8(p + rec::BUF_LEN, n);
            for i in 2..n as u32 {
                self.header();
                self.at(0x780, 0x7A8);
                let b = self.r8(rx + i);
                self.w8(buf + i, b);
            }
        }
        self.at(0x7AC, 0x7C0);
        let mut forget = false;
        if self.r8(rx + 1) == 0 {
            self.at(0x7C4, 0x7D0);
            let held = self.r8(p + rec::CMD_STATE) != CMD_CONFIG || {
                self.at(0x7D4, 0x7E0);
                self.m.mem.r32(p + rec::PREPARE) != 0
            };
            if held {
                self.at(0x7E4, 0x7F0);
                forget = self.r8(p + rec::HOLD) == 0;
            }
        }
        if !forget {
            self.at(0x7F4, 0x800);
            forget = self.r8(p + rec::KIND) != old;
        }
        if forget {
            self.at(0x804, 0x814);
            self.clr_info(p);
        }
        self.at(0x818, 0x828);
        self.w8(p + rec::FAILS, 0);
        let cmd = self.r8(p + rec::CMD_STATE);
        let on = cmd != CMD_DONE && {
            self.at(0x82C, 0x830);
            cmd == CMD_DETECT || {
                self.at(0x834, 0x840);
                self.r8(p + rec::REQUEST) != 0
            }
        };
        if on {
            self.at(0x844, 0x850);
            if cmd.wrapping_sub(2) < 0xFC {
                self.at(0x854, 0x868);
            }
            if cmd.wrapping_sub(2) < 0xFC && id != ID_CONFIG {
                self.at(0x86C, 0x87C);
                self.clr_info(p);
                self.at(0x880, 0x884);
            } else {
                self.at(0x888, 0x894);
                match cmd {
                    CMD_CONFIG => {
                        self.at(0x8CC, 0x8DC);
                        self.w8(p + rec::INFO_INDEX, 0);
                        self.w8(p + rec::CMD_STATE, 2);
                    }
                    CMD_DETECT => {
                        self.at(0x898, 0x8A4);
                        self.at(0x8C0, 0x8C8);
                        self.at(0x8D4, 0x8DC);
                        self.w8(p + rec::STATE, STATE_FIND_PAD);
                        self.w8(p + rec::CMD_STATE, CMD_CONFIG);
                    }
                    CMD_CONFIG_OFF => {
                        self.at(0x898, 0x89C);
                        self.at(0x8B0, 0x8B4);
                        self.at(0x8E0, 0x8E4);
                        self.w8(p + rec::CMD_STATE, CMD_DONE);
                    }
                    // The info states' replies: `APPLY` or
                    // `_padRecvAtLoadInfo` next, not reproduced.
                    _ => {
                        self.at(0x898, 0x89C);
                        self.at(0x8B0, 0x8BC);
                        self.at(0x8E8, 0x8F4);
                    }
                }
            }
        }
        self.at(0x920, 0x92C);
    }

    /// `_padFuncSendAuto` (+5B4h): the command the port's command state asks
    /// for, after the last exchange's ID byte. (The info states' requests,
    /// +688h, go to a pad in config mode, which a digital pad never is.)
    fn send_auto(&mut self, p: u32) {
        self.at(0x5B4, 0x5D8);
        if self.id(p) == ID_CONFIG {
            self.at(0x5DC, 0x5E8);
            if self.r8(p + rec::KIND) == 0 {
                return self.send_auto_off(p, false);
            }
            self.at(0x5EC, 0x5F8);
            if self.r8(p + rec::CMD_STATE) == CMD_DONE {
                return self.send_auto_off(p, true);
            }
            self.at(0x5FC, 0x608);
            if self.r8(p + rec::STATE) == STATE_FIND_CTP1 {
                self.at(0x60C, 0x61C);
                self.clr_info(p);
            }
        }
        self.at(0x620, 0x62C);
        match self.r8(p + rec::CMD_STATE) {
            CMD_CONFIG => {
                self.at(0x660, 0x668);
                self.para_mode(p, 1);
                self.at(0x66C, 0x670);
            }
            CMD_DETECT => self.at(0x630, 0x63C),
            CMD_CONFIG_OFF => {
                self.at(0x630, 0x634);
                self.at(0x648, 0x64C);
                return self.send_auto_off(p, true);
            }
            CMD_DONE => {
                self.at(0x630, 0x634);
                self.at(0x648, 0x654);
            }
            _ => {
                self.at(0x630, 0x634);
                self.at(0x648, 0x65C);
                self.at(0x688, 0x694);
            }
        }
        self.at(0x6B4, 0x6C0);
    }

    /// Out of config mode next (+674h, or +678h when `whole` is false).
    fn send_auto_off(&mut self, p: u32, whole: bool) {
        self.at(if whole { 0x674 } else { 0x678 }, 0x67C);
        self.para_mode(p, 0);
        self.at(0x680, 0x684);
        self.at(0x6B4, 0x6C0);
    }

    /// `_padFuncCurrLimit` (+318h), for a pad with no extended ID: the
    /// actuator gates cleared (the game's `bzero`), then opened as the
    /// pad's type and its actuator table allow, within `_padTotalCurr`'s
    /// budget of 60. (An extended pad's, +344h, takes config mode.)
    fn curr_limit(&mut self, p: u32) {
        self.at(0x318, 0x330);
        self.bzero();
        self.m.mem.write(p + rec::GATES, &[0; 6]);
        self.at(0x334, 0x340);
        let kind = self.r8(p + rec::KIND);
        let mut all = true;
        if self.m.mem.r16(p + rec::EX_ID) == 0 {
            self.at(0x490, 0x4A4);
            let small = kind.wrapping_sub(4) < 2 || {
                self.at(0x4A8, 0x4AC);
                kind == 7
            };
            if small {
                self.at(0x4B0, 0x4BC);
                self.at(0x4C0, 0x4D0);
                if self.r8(p + rec::ACT_SIZE) >= 2 {
                    all = false;
                    self.at(0x4D4, 0x4EC);
                    let act = self.m.mem.r32(p + rec::ACT);
                    if self.r8(act) & 0xC0 == 0x40 {
                        self.at(0x4F0, 0x500);
                        if self.r8(act + 1) & 1 != 0 {
                            self.at(0x504, 0x51C);
                            let total = self.m.mem.r32(self.l.data + data::TOTAL_CURR) as i32 + 10;
                            if total < 61 {
                                self.at(0x520, 0x540);
                                self.m.mem.write(p + rec::GATES, &[1, 1]);
                                self.m.mem.w32(self.l.data + data::TOTAL_CURR, total as u32);
                            }
                        }
                    }
                }
            }
            if all {
                self.at(0x544, 0x550);
                if kind == 3 {
                    self.at(0x554, 0x558);
                    self.w8(p + rec::GATES, 1);
                } else {
                    self.at(0x55C, 0x568);
                    self.at(0x56C, 0x570);
                    for i in (0..6).rev() {
                        self.header();
                        self.at(0x574, 0x580);
                        self.w8(p + rec::GATES + i, 1);
                    }
                }
            }
        }
        self.at(0x584, 0x590);
    }

    /// The game's `bzero` of the gates: libapi's A0h:28h vector (three
    /// instructions to the BIOS), or libc2's loop.
    fn bzero(&mut self) {
        let f = self.l.word(self.m, data::FUNC_CURR_LIMIT);
        let Some(z) = crate::libspu::jal(self.m, f + 0x14) else { return };
        if [z, z + 4, z + 8].map(|a| self.m.mem.r32(a)) == [0x240A_00A0, 0x0140_0008, 0x2409_0028] {
            return crate::irq_path(self.m, &[(0, 8)]);
        }
        crate::irq_path(self.m, &[(0x0, 0x4), (0x8, 0xC)]);
        for _ in 0..6 {
            crate::irq_header(self.m);
            crate::irq_path(self.m, &[(0x18, 0x24)]);
        }
        crate::irq_path(self.m, &[(0x28, 0x2C)]);
    }

    /// `_padFuncGetTxd` (+25Ch): the byte to send after the TAP byte: for a
    /// poll, the actuator level its gate lets through; else the command's.
    fn get_txd(&mut self, p: u32) -> u8 {
        self.at(0x25C, 0x26C);
        let i = self.r8(p + rec::TX_COUNT) as i32 - 3;
        let request = self.r8(p + rec::REQUEST);
        let v = if request == 0 {
            self.at(0x284, 0x28C);
            let shut = i < 6 && {
                self.at(0x290, 0x29C);
                self.r8((p + rec::GATES).wrapping_add(i as u32)) == 0
            };
            if shut {
                0
            } else {
                self.at(0x2A0, 0x2B0);
                if i < self.r8(p + rec::ACT_SIZE) as i32 {
                    self.at(0x2B4, 0x2C8);
                    let act = self.m.mem.r32(p + rec::ACT);
                    self.r8(act.wrapping_add(i as u32))
                } else {
                    0
                }
            }
        } else {
            self.at(0x270, 0x278);
            let len = self.r8(p + rec::CMD_LEN) as i32;
            let cmd = self.m.mem.r32(p + rec::CMD_DATA);
            if request == 0x4D {
                self.at(0x2CC, 0x2DC);
                if i < len {
                    self.at(0x2E0, 0x2E8);
                    self.at(0x2C0, 0x2C8);
                    self.r8(cmd.wrapping_add(i as u32))
                } else {
                    0xFF
                }
            } else {
                self.at(0x27C, 0x280);
                self.at(0x2EC, 0x2FC);
                if i < len {
                    self.at(0x300, 0x30C);
                    self.r8(cmd.wrapping_add(i as u32))
                } else {
                    0
                }
            }
        };
        self.at(0x310, 0x314);
        v
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

/// `_padCmdParaMode`: config mode (43h) next, its one byte `mode` (1: in,
/// 0: out).
fn para_mode(m: &mut M, p: u32, mode: u8) {
    m.mem.w8(p + rec::REQUEST, CONFIG);
    m.mem.w32(p + rec::CMD_DATA, p + rec::PARAM);
    m.mem.w8(p + rec::PARAM, mode);
    m.mem.w8(p + rec::CMD_LEN, 1);
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
//
// Each spends its original's blocks on its path (D71; offsets in its own
// object's text, `pddirres.o`'s for the engine's functions it calls
// through `.data`), and libapi's stubs for the BIOS calls it makes.

/// `_padFuncPort2Info(port)` (`pddirres.o` +594h): ports 00h-0Fh are port
/// 1's record.
fn port_time(m: &mut M, port: u32) {
    crate::path(m, &[(0x594, 0x5A4)]);
    if port & 0xF0 != 0 {
        crate::path(m, &[(0x5A8, 0x5A8)]);
    }
    crate::path(m, &[(0x5AC, 0x5B0)]);
}

/// `_padFuncClrInfo(p)` (`pddirres.o` +F0h), `p` forgotten with its time,
/// a pass of its loop a byte of the alignment reply; then `k`.
fn clr_info_flow(m: &mut M, p: Option<u32>, k: crate::libspu::K) -> Flow<crate::Psyq> {
    crate::path(m, &[(0xF0, 0xFC)]);
    let Some(p) = p.filter(|&p| m.mem.r8(p + rec::STATE) != STATE_DISCON) else {
        crate::path(m, &[(0x150, 0x154)]);
        return k(m);
    };
    clear_info(m, p);
    crate::path(m, &[(0x100, 0x13C)]);
    let tail: crate::libspu::K = Box::new(move |m| {
        crate::path(m, &[(0x150, 0x154)]);
        k(m)
    });
    crate::libspu::each(m, 0, 6, 4, std::rc::Rc::new(|_, _| {}), tail)
}

/// `PadInitDirect(port1, port2)`: `_padInitDirPort` sets out fresh
/// records for both ports, then their buffers `FF 00`, a pass of the loop
/// (+4Ch) a record, of the inner one (+6Ch) an alignment byte. The original
/// leaves 1 in v0.
fn pad_init_direct(m: &mut M, a: [u32; 4]) -> Result {
    let l = lib(m, at::INIT_EXEC, data::INT_EXEC)?;
    let f = m.sdk.at;
    let init_port = crate::libspu::jal(m, f + at::JAL_INIT_PORT)
        .ok_or_else(|| format!("libpad/pddirini at {f:#010x}: no _padInitDirPort call where PsyQ 4.7's object has it"))?;
    m.mem.w32(l.data + data::INT_EXEC, 0);
    m.mem.w32(l.data + data::MODE_MTAP, 0);
    crate::path(m, &[(0x0, 0x28)]);
    Ok(crate::guest_call(m, init_port, &a[..2], move |m| {
        crate::path(m, &[(0x2C, 0x48)]);
        let recs = l.word(m, data::INFO_DIR);
        for (i, &buf) in a[..2].iter().enumerate() {
            let p = recs + rec::SIZE * i as u32;
            m.mem.w32(p + rec::BUF, buf);
            m.mem.w32(p + rec::CHILD, 0);
            m.mem.w32(p + rec::ROOT, p);
            m.mem.write(buf, &[0xFF, 0]);
            m.mem.write(p + rec::ALIGN_REPLY, &[0xFF; 6]);
        }
        init_records(
            m,
            0,
            Box::new(move |m| {
                crate::path(m, &[(0x90, 0xAC)]);
                m.mem.w32(l.data + data::INT_EXEC, 1);
                Flow::ret(1)
            }),
        )
    }))
}

/// `PadInitDirect`'s passes over the records from `i`.
fn init_records(m: &mut M, i: u32, k: crate::libspu::K) -> Flow<crate::Psyq> {
    if i == 2 {
        return k(m);
    }
    crate::libspu::header(
        m,
        Box::new(move |m| {
            crate::path(m, &[(0x4C, 0x68)]);
            let next: crate::libspu::K = Box::new(move |m| {
                crate::path(m, &[(0x7C, 0x8C)]);
                init_records(m, i + 1, k)
            });
            crate::libspu::each(m, 0, 6, 4, std::rc::Rc::new(|_, _| {}), next)
        }),
    )
}

/// `PadStartCom()`: the element (re)queued at the chain's end, vblank
/// acknowledged and unmasked, the kernel's vblank acknowledge of root
/// counter 3 off (ChangeClearRCnt(3, 0)), the critical section left, both
/// ports forgotten, their rests over. The original leaves 1 in v0.
fn pad_start_com(m: &mut M, _a: [u32; 4]) -> Result {
    let l = lib(m, at::ELEMENT, data::VSYNC_PARAM)?;
    m.mem.w32(l.data + data::INT_EXEC, 0);
    crate::path(m, &[(0x0, 0x14)]);
    crate::take(m, crate::SYSCALL_STUB);
    crate::path(m, &[(0x18, 0x28)]);
    crate::take(m, crate::VECTOR_STUB);
    bios(m, 0xC0, 0x03, [PRIORITY, l.element(), 0, 0])?;
    crate::path(m, &[(0x2C, 0x34)]);
    crate::take(m, crate::VECTOR_STUB);
    bios(m, 0xC0, 0x02, [PRIORITY, l.element(), 0, 0])?;
    crate::path(m, &[(0x38, 0x5C)]);
    set_reg(m, I_STAT, 4, !1)?;
    let mask = reg(m, I_MASK, 4)?;
    set_reg(m, I_MASK, 4, mask | 1)?;
    crate::take(m, crate::VECTOR_STUB);
    bios(m, 0xC0, 0x0A, [3, 0, 0, 0])?;
    crate::path(m, &[(0x60, 0x64)]);
    crate::take(m, crate::SYSCALL_STUB);
    psx_bios::bios::syscall(m, [2, 0, 0, 0])?;
    crate::path(m, &[(0x68, 0x80)]);
    let recs = l.records(m);
    Ok(clr_info_flow(
        m,
        recs,
        Box::new(move |m| {
            crate::path(m, &[(0x84, 0x9C)]);
            let tail: crate::libspu::K = Box::new(move |m| {
                crate::path(m, &[(0xA0, 0xC8)]);
                m.mem.write(l.rest, &[0; 8]);
                m.mem.w32(l.data + data::INT_EXEC, 1);
                Flow::ret(1)
            });
            clr_info_flow(m, recs.map(|r| r + rec::SIZE), tail)
        }),
    ))
}

/// `PadStopCom()`: the kernel acknowledges root counter 3 again, the
/// element leaves the chain, the critical section is left (v0: 0, from
/// ExitCriticalSection).
fn pad_stop_com(m: &mut M, _a: [u32; 4]) -> Result {
    let l = lib(m, at::ELEMENT, data::VSYNC_PARAM)?;
    crate::path(m, &[(0xCC, 0xD8)]);
    crate::take(m, crate::SYSCALL_STUB);
    crate::path(m, &[(0xDC, 0xE4)]);
    crate::take(m, crate::VECTOR_STUB);
    bios(m, 0xC0, 0x0A, [3, 1, 0, 0])?;
    crate::path(m, &[(0xE8, 0xF4)]);
    crate::take(m, crate::VECTOR_STUB);
    bios(m, 0xC0, 0x03, [PRIORITY, l.element(), 0, 0])?;
    crate::path(m, &[(0xF8, 0xFC)]);
    crate::take(m, crate::SYSCALL_STUB);
    psx_bios::bios::syscall(m, [2, 0, 0, 0])?;
    crate::path(m, &[(0x100, 0x10C)]);
    ret(0)
}

/// Return `v` as an original that makes no runtime call does: quietly
/// (`Machine::quiet`, D63).
fn back(m: &mut M, v: u32) -> Result {
    m.quiet = true;
    ret(v)
}

/// `PadGetState(port)`: the port's state; with a command in flight or a
/// status byte that isn't 0 in its buffer, FindCTP1 and FindCTP2 read as
/// FindPad and Stable as ReqInfo.
fn pad_get_state(m: &mut M, a: [u32; 4]) -> Result {
    let l = lib(m, at::PORT2INFO, data::PORT2INFO)?;
    crate::path(m, &[(0x0, 0x14)]);
    port_time(m, a[0]);
    let Some(p) = l.port(m, a[0]) else { return back(m, 0) };
    let b = |m: &M, f: u32| m.mem.r8(p + f) != 0;
    crate::path(m, &[(0x18, 0x28)]);
    let mut busy = b(m, rec::REQUEST);
    if !busy {
        crate::path(m, &[(0x2C, 0x38)]);
        busy = b(m, rec::CURRENT);
    }
    // A multitap's port also waits for the command before.
    if !busy {
        crate::path(m, &[(0x3C, 0x48)]);
        if m.mem.r32(p + rec::ROOT) != p {
            crate::path(m, &[(0x4C, 0x58)]);
            busy = b(m, rec::PREVIOUS);
        }
    }
    if !busy {
        crate::path(m, &[(0x5C, 0x70)]);
        busy = m.mem.r8(m.mem.r32(p + rec::BUF)) != 0;
    }
    let s = m.mem.r8(p + rec::STATE);
    let shown = match (busy, s) {
        (false, _) => {
            crate::path(m, &[(0xB8, 0xB8)]);
            s
        }
        (true, STATE_FIND_CTP2) => {
            crate::path(m, &[(0x74, 0x80), (0xB0, 0xB4)]);
            STATE_FIND_PAD
        }
        (true, STATE_FIND_CTP1) => {
            crate::path(m, &[(0x74, 0x90)]);
            STATE_FIND_PAD
        }
        (true, STATE_STABLE) => {
            crate::path(m, &[(0x74, 0x88), (0x9C, 0xA4)]);
            STATE_REQ_INFO
        }
        (true, s) if s < STATE_FIND_CTP2 => {
            crate::path(m, &[(0x74, 0x98), (0xB8, 0xB8)]);
            s
        }
        (true, s) => {
            crate::path(m, &[(0x74, 0x88), (0x9C, 0xAC), (0xB8, 0xB8)]);
            s
        }
    };
    crate::path(m, &[(0xBC, 0xC8)]);
    back(m, shown as u32)
}

/// `PadInfoMode(port, mode, id)`: 1 the terminal type, 2 the extended ID,
/// 3 the mode offset, 4 the ID table's entry `id` (`id < 0`: its size),
/// 100 the failure count, else 0.
fn pad_info_mode(m: &mut M, a: [u32; 4]) -> Result {
    let l = lib(m, at::PORT2INFO, data::PORT2INFO)?;
    crate::path(m, &[(0x0, 0x20)]);
    port_time(m, a[0]);
    let Some(p) = l.port(m, a[0]) else { return back(m, 0) };
    crate::path(m, &[(0x24, 0x30)]);
    let (mode, id, ids) = (a[1] as i32, a[2] as i32, m.mem.r8(p + rec::ID_COUNT) as i32);
    let (v, path): (u32, &[(u32, u32)]) = match mode {
        3 => (m.mem.r8(p + rec::PREV_MODE) as u32, &[(0x88, 0x90)]),
        1 => (m.mem.r8(p + rec::KIND) as u32, &[(0x34, 0x40), (0x70, 0x78)]),
        2 => (m.mem.r16(p + rec::EX_ID) as u32, &[(0x34, 0x48), (0x7C, 0x84)]),
        ..4 => (0, &[(0x34, 0x50)]),
        4 if id < 0 => (ids as u32, &[(0x34, 0x38), (0x54, 0x5C), (0x94, 0xA4)]),
        // The table's entries come from a pad in config mode; there are
        // none without one.
        4 if id < ids => (m.mem.r16(m.mem.r32(p + rec::TABLES) + 2 * id as u32) as u32, &[(0x34, 0x38), (0x54, 0x5C), (0x94, 0x98), (0xA8, 0xD0)]),
        4 => (0, &[(0x34, 0x38), (0x54, 0x5C), (0x94, 0x98), (0xA8, 0xB8), (0xE0, 0xE0)]),
        100 => (m.mem.r32(p + rec::FAILURES), &[(0x34, 0x38), (0x54, 0x64), (0xD4, 0xDC)]),
        _ => (0, &[(0x34, 0x38), (0x54, 0x6C)]),
    };
    crate::path(m, path);
    crate::path(m, &[(0xE4, 0xF4)]);
    back(m, v)
}

/// `PadSetAct(port, table, size)`: the actuator table libpad sends from.
/// The original leaves the port's record in v0.
fn pad_set_act(m: &mut M, a: [u32; 4]) -> Result {
    let l = lib(m, at::PORT2INFO, data::PORT2INFO)?;
    crate::path(m, &[(0x0, 0x20)]);
    port_time(m, a[0]);
    let Some(p) = l.port(m, a[0]) else { return back(m, 0) };
    crate::path(m, &[(0x24, 0x3C)]);
    m.mem.w32(p + rec::ACT, a[1]);
    m.mem.w8(p + rec::ACT_SIZE, a[2] as u8);
    back(m, p)
}

/// `PadSetActAlign(port, table)`: 1 when the engine takes the actuator
/// alignment command (its handlers, `pdcmd2.o`'s, for the info states),
/// 0 when it is busy (`_padFuncChkEng`, `pddirres.o` +A10h).
fn pad_set_act_align(m: &mut M, a: [u32; 4]) -> Result {
    let l = lib(m, at::PORT2INFO, data::PORT2INFO)?;
    crate::path(m, &[(0x0, 0x1C)]);
    port_time(m, a[0]);
    let Some(p) = l.port(m, a[0]) else { return back(m, 0) };
    crate::path(m, &[(0x20, 0x34), (0xA10, 0xA1C)]);
    if m.mem.r16(p + rec::EX_ID) != 0 {
        crate::path(m, &[(0xA20, 0xA2C)]);
    }
    let busy = busy(m, p);
    if busy {
        crate::path(m, &[(0xA30, 0xA30)]);
    }
    crate::path(m, &[(0xA34, 0xA38), (0x38, 0x3C)]);
    if busy {
        crate::path(m, &[(0x68, 0x78)]);
        return back(m, 0);
    }
    let f = m.sdk.at;
    let handler = |m: &M, (hi, lo): (u32, u32)| {
        crate::libspu::pair(m, f + hi, f + lo).ok_or_else(|| format!("libpad/pdcmd2 at {f:#010x}: no handler where PsyQ 4.7's object has it"))
    };
    let (prepare, apply) = (handler(m, at::PREPARE)?, handler(m, at::APPLY)?);
    crate::path(m, &[(0x40, 0x78)]);
    m.mem.w8(p + rec::CMD_STATE, CMD_CONFIG);
    m.mem.w32(p + rec::PREPARE, prepare);
    m.mem.w32(p + rec::ALIGN, a[1]);
    m.mem.w32(p + rec::APPLY, apply);
    back(m, 1)
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
    /// buffers (23h bytes each), `_padInfoDir` set.
    fn init_dir_port(m: &mut M) {
        m.mem.write(RECORDS, &[0; 2 * rec::SIZE as usize]);
        for i in 0..2 {
            let p = RECORDS + rec::SIZE * i;
            m.mem.w32(p + rec::RX, RECORDS + 2 * rec::SIZE + 0x23 * i);
            m.mem.w32(p + rec::TX, RECORDS + 2 * rec::SIZE + 0x48 + 0x23 * i);
        }
        m.mem.w32(DATA + data::INFO_DIR, RECORDS);
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{DATA, RECORDS, install, run};
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
        // (D66, D71): a refused command and nothing connected take 13,656
        // cycles. This machine has no guest budget, so the passes are its
        // time.
        let spent = |m: &M| m.clock.now - m.clock.video.vblank_at(m.vblanks);
        assert_eq!(spent(&m), 13_656);
        for _ in 0..11 {
            assert_eq!(state(&mut m, 0), 1);
            vblanks(&mut m, 1);
        }
        assert_eq!(m.mem.r8(RECORDS + rec::STATE), STATE_FIND_CTP1);
        vblanks(&mut m, 1);
        assert_eq!(state(&mut m, 0), 1, "the poll's reply is pending");
        // A poll and nothing connected: 13,956 cycles.
        assert_eq!(spent(&m), 13_956);
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
}
