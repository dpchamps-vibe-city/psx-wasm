//! libgpu (M2 stubs until the GPU front end, M4). Drawing and transfers
//! complete immediately in virtual time and leave VRAM untouched;
//! `StoreImage` reads back zeros. `DrawSync` and its callback behave as on
//! hardware once the queue drains.

use crate::{Adapter, M, Result, block, cond, ev, ret};
use psx_bios::{Event, Flow, GuestCall, Wait};

pub const ADAPTERS: &[(&str, Adapter)] = &[
    ("ResetGraph", reset_graph),
    ("GetGraphType", zero),
    ("SetGraphDebug", set_graph_debug),
    ("GetGraphDebug", get_graph_debug),
    ("SetGraphQueue", zero),
    ("DrawSync", draw_sync),
    ("DrawSyncCallback", draw_sync_callback),
    ("DrawOTag", start),
    ("DrawOTag2", start),
    ("DrawOTagEnv", start),
    ("DrawPrim", start),
    ("LoadImage", start),
    ("LoadImage2", start),
    ("MoveImage", start),
    ("MoveImage2", start),
    ("ClearImage", start),
    ("ClearImage2", start),
    ("StoreImage", store_image),
    ("StoreImage2", store_image),
    ("PutDrawEnv", identity),
    ("PutDispEnv", identity),
    ("SetDispMask", zero),
    ("GetODE", get_ode),
    ("_GPU_ResetCallback", zero),
];

#[derive(Debug, Default)]
pub struct Gpu {
    pub busy: bool,
    pub sync_cb: u32,
    pub debug: u32,
    pub commands: u64,
}

pub fn on_idle(m: &mut M) {
    m.sdk.gpu.busy = false;
    let f = m.sdk.gpu.sync_cb;
    if f != 0 {
        m.calls.push_back(GuestCall { addr: f, args: [0; 4] });
    }
}

/// Queue work: the GPU is busy for a moment of virtual time.
fn start(m: &mut M, _a: [u32; 4]) -> Result {
    m.sdk.gpu.commands += 1;
    m.sdk.gpu.busy = true;
    m.clock.cancel(|e| *e == Event::Sdk(ev::GPU_IDLE, 0));
    let t = m.clock.now + 64;
    m.clock.schedule(t, Event::Sdk(ev::GPU_IDLE, 0));
    ret(0)
}

fn store_image(m: &mut M, a: [u32; 4]) -> Result {
    let (w, h) = (m.mem.r16(a[0] + 4) as usize, m.mem.r16(a[0] + 6) as usize);
    m.mem.write(a[1], &vec![0; w * h * 2]);
    start(m, a)
}

fn reset_graph(m: &mut M, _a: [u32; 4]) -> Result {
    m.sdk.gpu.busy = false;
    ret(0)
}

fn draw_sync(m: &mut M, a: [u32; 4]) -> Result {
    if a[0] == 0 {
        return block(Wait::Sdk(cond::GPU_IDLE), |_m: &mut M| Flow::ret(0));
    }
    ret(m.sdk.gpu.busy as u32)
}

fn draw_sync_callback(m: &mut M, a: [u32; 4]) -> Result {
    ret(std::mem::replace(&mut m.sdk.gpu.sync_cb, a[0]))
}

fn set_graph_debug(m: &mut M, a: [u32; 4]) -> Result {
    ret(std::mem::replace(&mut m.sdk.gpu.debug, a[0]))
}

fn get_graph_debug(m: &mut M, _a: [u32; 4]) -> Result {
    ret(m.sdk.gpu.debug)
}

/// Interlaced field: alternates each vblank.
fn get_ode(m: &mut M, _a: [u32; 4]) -> Result {
    ret((m.vblanks & 1) as u32)
}

fn identity(_m: &mut M, a: [u32; 4]) -> Result {
    ret(a[0])
}

fn zero(_m: &mut M, _a: [u32; 4]) -> Result {
    ret(0)
}
