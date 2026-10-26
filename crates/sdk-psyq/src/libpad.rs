//! libpad (M2: the host's pad state written into the `PadInitDirect`
//! buffers at each vblank; actuators ignored).
//!
//! Buffer layout: status (0x00 ok, 0xFF disconnected), ID (0x41 digital,
//! 0x73 analog), then the controller's data bytes (buttons active low).

use crate::{Adapter, M, Result, ret};

pub const ADAPTERS: &[(&str, Adapter)] = &[
    ("PadInitDirect", pad_init_direct),
    ("PadStartCom", pad_start_com),
    ("PadStopCom", pad_stop_com),
    ("PadGetState", pad_get_state),
    ("PadInfoMode", pad_info_mode),
    ("PadSetAct", one),
    ("PadSetActAlign", one),
];

/// PadGetState results.
pub const STATE_DISCON: u32 = 0;
pub const STATE_STABLE: u32 = 6;

#[derive(Debug, Default)]
pub struct Pad {
    pub bufs: [u32; 2],
    pub started: bool,
    pub connected: [bool; 2],
}

pub fn on_vblank(m: &mut M) {
    if !m.sdk.pad.started {
        return;
    }
    for port in 0..2 {
        let buf = m.sdk.pad.bufs[port];
        if buf == 0 {
            continue;
        }
        let state = m.host.pad_state(port as u32);
        m.sdk.pad.connected[port] = !state.is_empty();
        if state.is_empty() {
            m.mem.w8(buf, 0xFF);
            continue;
        }
        let id = if state.len() > 2 { 0x73 } else { 0x41 };
        let mut out = vec![0x00, id];
        out.extend(&state);
        m.mem.write(buf, &out);
    }
}

fn pad_init_direct(m: &mut M, a: [u32; 4]) -> Result {
    m.sdk.pad.bufs = [a[0], a[1]];
    for b in [a[0], a[1]] {
        if b != 0 {
            m.mem.w8(b, 0xFF);
        }
    }
    ret(1)
}

fn pad_start_com(m: &mut M, _a: [u32; 4]) -> Result {
    m.sdk.pad.started = true;
    ret(1)
}

fn pad_stop_com(m: &mut M, _a: [u32; 4]) -> Result {
    m.sdk.pad.started = false;
    ret(1)
}

fn port(a: u32) -> usize {
    ((a >> 4) & 1) as usize
}

fn pad_get_state(m: &mut M, a: [u32; 4]) -> Result {
    ret(if m.sdk.pad.connected[port(a[0])] { STATE_STABLE } else { STATE_DISCON })
}

/// InfoModeCurID (1): the digital pad's ID 4; nothing else is reported.
fn pad_info_mode(m: &mut M, a: [u32; 4]) -> Result {
    if !m.sdk.pad.connected[port(a[0])] {
        return ret(0);
    }
    ret(if a[1] == 1 { 4 } else { 0 })
}

fn one(_m: &mut M, _a: [u32; 4]) -> Result {
    ret(1)
}
