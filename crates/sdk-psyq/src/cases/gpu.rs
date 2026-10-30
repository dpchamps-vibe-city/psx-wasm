//! Chip gpu's cases: libgpu's `sys.o` (D59, D61; phase 1 task 9b).
//!
//! Observation notes, beyond the module's specs:
//! - **The device effect** is the renderer's command stream and the
//!   displays presented (`Harness::device`, appended to every case): what
//!   GP0 drew and what GP1 set.
//! - **libgpu's state** is observed through the accessors that stay
//!   original (`GetDrawEnv`, `GetDispEnv`) and in the structs the adapters
//!   write back (the RECT an image call clamps, a DRAWENV's DR_ENV packet,
//!   a DISPENV's `pad0`, an ordering table).
//! - **Vblank alignment.** libgpu's own loops (ResetGraph's queue clear,
//!   memcpy) spend virtual time on the device side, the adapters none
//!   (`libgpu`'s module notes), so a case lets a vblank pass after heavy
//!   calls, and keeps straight-line work between vblanks well under a
//!   frame.
//! - **Not observed:** `DMACallback(2, …)`'s result while libgpu's queue
//!   pump is DMA2's callback (a stand-in address natively).
//! - Before `ResetGraph` DPCR leaves DMA2 off (as after `ResetCallback`),
//!   so a transfer waits, and `DrawSync(0)` runs into libgpu's timeout
//!   (btn's boot does): the level-2 sequence covers it.

use super::{Case, Harness, Rng, SCRATCH, call, observe_ram};

pub fn cases() -> Vec<Case> {
    vec![
        Case { name: "gpu.reset-graph", chip: "gpu", level: 1, covers: &["ResetGraph"], needs: &["ResetGraph", "DrawSync", "GetDrawEnv", "GetDispEnv"], run: reset_graph },
        Case { name: "gpu.disp-mask", chip: "gpu", level: 1, covers: &["SetDispMask"], needs: &["ResetGraph", "SetDispMask", "GetDispEnv", "PutDispEnv"], run: disp_mask },
        Case { name: "gpu.draw-sync", chip: "gpu", level: 1, covers: &["DrawSync"], needs: &["ResetGraph", "DrawSync", "DrawSyncCallback", "ClearOTagR", "DrawOTag"], run: draw_sync },
        Case { name: "gpu.clear-image", chip: "gpu", level: 1, covers: &["ClearImage"], needs: &["ResetGraph", "DrawSync", "ClearImage"], run: clear_image },
        Case { name: "gpu.load-image", chip: "gpu", level: 1, covers: &["LoadImage"], needs: &["ResetGraph", "DrawSync", "LoadImage"], run: load_image },
        Case { name: "gpu.store-image", chip: "gpu", level: 1, covers: &["StoreImage"], needs: &["ResetGraph", "DrawSync", "StoreImage"], run: store_image },
        Case {
            name: "gpu.move-image",
            chip: "gpu",
            level: 1,
            covers: &["MoveImage", "MoveImage2"],
            needs: &["ResetGraph", "DrawSync", "MoveImage", "MoveImage2"],
            run: move_image,
        },
        Case {
            name: "gpu.ordering-table",
            chip: "gpu",
            level: 1,
            covers: &["ClearOTagR", "DrawOTag"],
            needs: &["ResetGraph", "DrawSync", "ClearOTagR", "DrawOTag"],
            run: ordering_table,
        },
        Case {
            name: "gpu.draw-env",
            chip: "gpu",
            level: 1,
            covers: &["PutDrawEnv", "DrawOTagEnv"],
            needs: &["ResetGraph", "DrawSync", "PutDrawEnv", "DrawOTagEnv", "GetDrawEnv", "ClearOTagR"],
            run: draw_env,
        },
        Case {
            name: "gpu.disp-env",
            chip: "gpu",
            level: 1,
            covers: &["PutDispEnv"],
            needs: &["ResetGraph", "PutDispEnv", "GetDispEnv", "SetDispMask", "SetVideoMode"],
            run: disp_env,
        },
        Case { name: "gpu.field", chip: "gpu", level: 1, covers: &["GetODE"], needs: &["ResetGraph", "GetODE", "PutDispEnv"], run: field },
        Case {
            name: "gpu.reset-callback",
            chip: "gpu",
            level: 1,
            covers: &["_GPU_ResetCallback"],
            needs: &["ResetGraph", "DrawSync", "_GPU_ResetCallback", "ClearOTagR", "DrawOTag", "DrawSyncCallback", "MoveImage2"],
            run: reset_callback,
        },
        Case {
            name: "gpu.sequence",
            chip: "gpu",
            level: 2,
            covers: &[
                "ResetGraph", "SetDispMask", "DrawSync", "ClearImage", "LoadImage", "StoreImage", "MoveImage", "MoveImage2", "ClearOTagR",
                "DrawOTag", "PutDrawEnv", "DrawOTagEnv", "PutDispEnv", "_GPU_ResetCallback", "GetODE",
            ],
            needs: &[
                "ResetCallback", "ResetGraph", "SetDispMask", "DrawSync", "ClearImage", "LoadImage", "StoreImage", "MoveImage", "MoveImage2",
                "ClearOTag", "ClearOTagR", "DrawOTag", "PutDrawEnv", "DrawOTagEnv", "PutDispEnv", "DrawSyncCallback", "GetDispEnv",
                "GetDrawEnv", "SetGraphDebug", "GetODE",
            ],
            run: sequence,
        },
    ]
}

// Scratch layout: structs, an ordering table, prims, image data.
const RECT: u32 = SCRATCH;
const RECT2: u32 = SCRATCH + 0x10;
const ENV: u32 = SCRATCH + 0x40;
const ENV2: u32 = SCRATCH + 0xC0;
const COPY: u32 = SCRATCH + 0x140;
const OT: u32 = SCRATCH + 0x200;
const PRIMS: u32 = SCRATCH + 0x400;
const DATA: u32 = SCRATCH + 0x800;
const DATA_LEN: u32 = 0x1000;

fn write_rect(h: &mut dyn Harness, at: u32, x: i32, y: i32, w: i32, hh: i32) {
    let b: Vec<u8> = [x, y, w, hh].iter().flat_map(|v| (*v as i16).to_le_bytes()).collect();
    h.write(at, &b);
}

fn word(h: &mut dyn Harness, addr: u32) -> u32 {
    let b = h.read(addr, 4);
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn callback(h: &mut dyn Harness) -> Result<(u32, u32), String> {
    h.callback().ok_or_else(|| "game.toml [conform] has no callback".to_string())
}

/// A random rectangle in VRAM, sometimes 64-pixel aligned, sometimes past
/// its edges or negative.
fn random_rect(rng: &mut Rng) -> (i32, i32, i32, i32) {
    let aligned = rng.range(0, 3) == 0;
    let x = if aligned { 64 * rng.range(0, 16) as i32 } else { rng.range(0, 1024) as i32 };
    let y = rng.range(0, 512) as i32;
    let w = match rng.range(0, 6) {
        0 => -(rng.range(1, 16) as i32),
        1 => rng.range(1000, 1200) as i32,
        _ if aligned => 64 * rng.range(1, 6) as i32,
        _ => rng.range(1, 200) as i32,
    };
    let hh = match rng.range(0, 6) {
        0 => -(rng.range(1, 16) as i32),
        1 => rng.range(500, 700) as i32,
        _ => rng.range(1, 120) as i32,
    };
    (x, y, w, hh)
}

/// A small rectangle inside VRAM for image transfers.
fn small_rect(rng: &mut Rng) -> (i32, i32, i32, i32) {
    let (w, hh) = (rng.range(1, 48) as i32, rng.range(1, 24) as i32);
    (rng.range(0, 1024 - w as u32) as i32, rng.range(0, 512 - hh as u32) as i32, w, hh)
}

fn fill(h: &mut dyn Harness, at: u32, len: u32, rng: &mut Rng) {
    let b: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
    h.write(at, &b);
}

/// ResetGraph(0) and a vblank: the device side's queue clear takes time.
fn reset_graph0(h: &mut dyn Harness) -> Result<(), String> {
    call(h, "ResetGraph", &[0])?;
    h.vblanks(1)
}

/// A TILE at `p` with colour and place, linked into the table entry `e`.
fn add_tile(h: &mut dyn Harness, e: u32, p: u32, rng: &mut Rng) {
    let next = word(h, e) & 0xFF_FFFF;
    let rgb = rng.next() as u32 & 0xFF_FFFF;
    let (x, y, w, hh) = (rng.range(0, 320), rng.range(0, 240), rng.range(1, 64), rng.range(1, 64));
    let words = [3 << 24 | next, 0x6000_0000 | rgb, y << 16 | x, hh << 16 | w];
    let b: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    h.write(p, &b);
    h.write(e, &(p & 0xFF_FFFF).to_le_bytes());
}

fn reset_graph(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    call(h, "DrawSync", &[1])?;
    for _ in 0..3 {
        let mode = [0, 1, 2, 3, 4, 5, 6, 7, 8, 0x13][rng.range(0, 10) as usize];
        call(h, "ResetGraph", &[mode])?;
        h.vblanks(1)?;
        call(h, "DrawSync", &[1])?;
        call(h, "GetDrawEnv", &[COPY])?;
        observe_ram(h, "GetDrawEnv", COPY, 0x5C);
        call(h, "GetDispEnv", &[COPY])?;
        observe_ram(h, "GetDispEnv", COPY, 0x14);
    }
    Ok(())
}

fn disp_mask(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    reset_graph0(h)?;
    write_rect(h, ENV, 0, 0, 320, 240);
    h.write(ENV + 8, &[0; 12]);
    call(h, "PutDispEnv", &[ENV])?;
    for _ in 0..3 {
        let mask = rng.range(0, 3);
        call(h, "SetDispMask", &[mask])?;
        call(h, "GetDispEnv", &[COPY])?;
        observe_ram(h, "GetDispEnv", COPY, 0x14);
        h.vblanks(1)?;
    }
    Ok(())
}

fn draw_sync(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    let (cb, state) = callback(h)?;
    call(h, "DrawSync", &[1])?;
    reset_graph0(h)?;
    call(h, "DrawSync", &[0])?;
    call(h, "DrawSync", &[rng.range(1, 4)])?;
    // A DrawSync callback: work goes through the queue, and the callback
    // runs when it drains.
    h.write(state, &(rng.next() as u32).to_le_bytes());
    call(h, "DrawSyncCallback", &[cb])?;
    let n = rng.range(1, 16);
    call(h, "ClearOTagR", &[OT, n])?;
    call(h, "DrawOTag", &[OT + 4 * (n - 1)])?;
    observe_ram(h, "callback word", state, 4);
    call(h, "DrawSync", &[1])?;
    call(h, "DrawSync", &[0])?;
    observe_ram(h, "callback word", state, 4);
    call(h, "DrawSyncCallback", &[0])?;
    call(h, "DrawOTag", &[OT + 4 * (n - 1)])?;
    call(h, "DrawSync", &[0])?;
    observe_ram(h, "callback word", state, 4);
    Ok(())
}

fn clear_image(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    reset_graph0(h)?;
    for _ in 0..3 {
        let (x, y, w, hh) = random_rect(rng);
        write_rect(h, RECT, x, y, w, hh);
        let (r, g, b) = (rng.range(0, 0x200), rng.range(0, 0x100), rng.range(0, 0x100));
        call(h, "ClearImage", &[RECT, r, g, b])?;
        observe_ram(h, "rect", RECT, 8);
        call(h, "DrawSync", &[0])?;
    }
    Ok(())
}

fn load_image(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    reset_graph0(h)?;
    fill(h, DATA, DATA_LEN, rng);
    for _ in 0..3 {
        let (x, y, w, hh) = if rng.range(0, 4) == 0 { random_rect(rng) } else { small_rect(rng) };
        let (w, hh) = (w.min(64), hh.min(32));
        write_rect(h, RECT, x, y, w, hh);
        call(h, "LoadImage", &[RECT, DATA])?;
        observe_ram(h, "rect", RECT, 8);
        call(h, "DrawSync", &[0])?;
    }
    Ok(())
}

fn store_image(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    reset_graph0(h)?;
    for _ in 0..2 {
        let (x, y, w, hh) = small_rect(rng);
        write_rect(h, RECT, x, y, w, hh);
        h.write(DATA, &[0xAA; 0x800]);
        call(h, "StoreImage", &[RECT, DATA])?;
        call(h, "DrawSync", &[0])?;
        observe_ram(h, "rect", RECT, 8);
        // What the transfer wrote, and where it stopped.
        let words = ((w * hh + 1) / 2) as usize;
        observe_ram(h, "stored", DATA, 4 * words + 8);
    }
    Ok(())
}

fn move_image(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    reset_graph0(h)?;
    for name in ["MoveImage", "MoveImage2", "MoveImage"] {
        let (x, y, mut w, mut hh) = small_rect(rng);
        if rng.range(0, 4) == 0 {
            w = 0;
        }
        if rng.range(0, 4) == 0 {
            hh = 0;
        }
        write_rect(h, RECT, x, y, w, hh);
        call(h, name, &[RECT, rng.range(0, 1000), rng.range(0, 500)])?;
        call(h, "DrawSync", &[0])?;
    }
    Ok(())
}

fn ordering_table(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    reset_graph0(h)?;
    let n = rng.range(1, 48);
    fill(h, OT, 4 * n, rng);
    let first = call(h, "ClearOTagR", &[OT, n])?;
    observe_ram(h, "ordering table", OT, 4 * n as usize);
    // The terminator libgpu links the table to.
    let term = word(h, first) & 0xFF_FFFF;
    observe_ram(h, "terminator", 0x8000_0000 | term, 20);
    for i in 0..rng.range(0, 6) {
        let e = OT + 4 * rng.range(0, n);
        add_tile(h, e, PRIMS + 16 * i, rng);
    }
    call(h, "DrawOTag", &[OT + 4 * (n - 1)])?;
    call(h, "DrawSync", &[0])?;
    Ok(())
}

/// A random DRAWENV at `at` (its DR_ENV area garbage).
fn random_draw_env(h: &mut dyn Harness, at: u32, rng: &mut Rng) {
    fill(h, at, 0x5C, rng);
    let (x, y, w, hh) = random_rect(rng);
    write_rect(h, at, x, y, w, hh);
    let ofs = [rng.range(0, 0x1000) as i32 - 0x800, rng.range(0, 0x1000) as i32 - 0x800];
    h.write(at + 8, &[(ofs[0] as i16).to_le_bytes(), (ofs[1] as i16).to_le_bytes()].concat());
    let tw = [rng.range(0, 256), rng.range(0, 256), rng.range(0, 256), rng.range(0, 256)];
    write_rect(h, at + 12, tw[0] as i32, tw[1] as i32, tw[2] as i32, tw[3] as i32);
    let flags = [rng.range(0, 3) as u8, rng.range(0, 3) as u8, rng.range(0, 3) as u8];
    h.write(at + 22, &flags);
}

fn draw_env(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    reset_graph0(h)?;
    for _ in 0..2 {
        random_draw_env(h, ENV, rng);
        call(h, "PutDrawEnv", &[ENV])?;
        observe_ram(h, "DRAWENV", ENV, 0x5C);
        call(h, "GetDrawEnv", &[COPY])?;
        observe_ram(h, "GetDrawEnv", COPY, 0x5C);
        call(h, "DrawSync", &[0])?;
    }
    let n = rng.range(1, 8);
    call(h, "ClearOTagR", &[OT, n])?;
    add_tile(h, OT + 4 * rng.range(0, n), PRIMS, rng);
    random_draw_env(h, ENV2, rng);
    call(h, "DrawOTagEnv", &[OT + 4 * (n - 1), ENV2])?;
    observe_ram(h, "DRAWENV", ENV2, 0x5C);
    call(h, "GetDrawEnv", &[COPY])?;
    observe_ram(h, "GetDrawEnv", COPY, 0x5C);
    call(h, "DrawSync", &[0])?;
    Ok(())
}

/// A random DISPENV at `at`: widths and heights around the mode
/// boundaries.
fn random_disp_env(h: &mut dyn Harness, at: u32, rng: &mut Rng) {
    let w = [256, 280, 281, 320, 352, 353, 368, 400, 401, 512, 560, 561, 640, rng.range(0, 1024)][rng.range(0, 14) as usize] as i32;
    let hh = [240, 256, 257, 288, 289, 480, 512, rng.range(0, 600)][rng.range(0, 8) as usize] as i32;
    write_rect(h, at, rng.range(0, 1024) as i32, rng.range(0, 512) as i32, w, hh);
    let sw = if rng.range(0, 3) == 0 { 0 } else { rng.range(0, 512) as i32 };
    let sh = if rng.range(0, 3) == 0 { 0 } else { rng.range(0, 300) as i32 };
    write_rect(h, at + 8, rng.range(0, 128) as i32 - 64, rng.range(0, 64) as i32 - 32, sw, sh);
    let b = [rng.range(0, 2) as u8, rng.range(0, 2) as u8, rng.next() as u8, rng.next() as u8];
    h.write(at + 16, &b);
}

fn disp_env(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    call(h, "SetVideoMode", &[rng.range(0, 2)])?;
    reset_graph0(h)?;
    random_disp_env(h, ENV, rng);
    call(h, "PutDispEnv", &[ENV])?;
    observe_ram(h, "DISPENV", ENV, 0x14);
    h.vblanks(1)?;
    // The same again: nothing changes but the copy.
    call(h, "PutDispEnv", &[ENV])?;
    observe_ram(h, "DISPENV", ENV, 0x14);
    // Only the screen.
    let screen = [rng.range(0, 64) as i32 - 32, rng.range(0, 32) as i32 - 16, rng.range(0, 400) as i32, rng.range(0, 260) as i32];
    write_rect(h, ENV + 8, screen[0], screen[1], screen[2], screen[3]);
    call(h, "PutDispEnv", &[ENV])?;
    observe_ram(h, "DISPENV", ENV, 0x14);
    call(h, "GetDispEnv", &[COPY])?;
    observe_ram(h, "GetDispEnv", COPY, 0x14);
    h.vblanks(1)?;
    // A new one, and after SetDispMask(0) the same one in full.
    random_disp_env(h, ENV2, rng);
    call(h, "PutDispEnv", &[ENV2])?;
    observe_ram(h, "DISPENV", ENV2, 0x14);
    call(h, "SetDispMask", &[0])?;
    call(h, "PutDispEnv", &[ENV2])?;
    observe_ram(h, "DISPENV", ENV2, 0x14);
    call(h, "SetDispMask", &[1])?;
    h.vblanks(1)?;
    Ok(())
}

fn reset_callback(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    let (cb, state) = callback(h)?;
    reset_graph0(h)?;
    // DMA2's callback was none; libgpu's pump from here on.
    call(h, "_GPU_ResetCallback", &[])?;
    h.write(state, &(rng.next() as u32).to_le_bytes());
    call(h, "DrawSyncCallback", &[cb])?;
    let n = rng.range(1, 8);
    call(h, "ClearOTagR", &[OT, n])?;
    call(h, "DrawOTag", &[OT + 4 * (n - 1)])?;
    h.vblanks(1)?;
    observe_ram(h, "callback word", state, 4);
    // MoveImage2 makes it DMA2's callback; its DMA's interrupt runs it.
    write_rect(h, RECT, 0, 0, 16, 16);
    call(h, "MoveImage2", &[RECT, 64, 64])?;
    h.vblanks(1)?;
    call(h, "DrawOTag", &[OT + 4 * (n - 1)])?;
    h.vblanks(1)?;
    observe_ram(h, "callback word", state, 4);
    call(h, "DrawSyncCallback", &[0])?;
    call(h, "DrawSync", &[0])?;
    Ok(())
}

/// libgpu from boot: DMA2 off until ResetGraph (a ClearImage waits, and
/// DrawSync(0) times out), then the environments, the queue with a
/// DrawSync callback, every transfer, the display, and the resets.
fn sequence(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    let (cb, state) = callback(h)?;
    h.write(state, &0x1234_5678u32.to_le_bytes());
    // Before ResetGraph: libetc's ResetCallback leaves every DMA channel
    // off.
    call(h, "ResetCallback", &[])?;
    write_rect(h, RECT, 0, 0, 640, 480);
    call(h, "ClearImage", &[RECT, 0, 0, 0])?;
    observe_ram(h, "rect", RECT, 8);
    call(h, "DrawSync", &[1])?;
    call(h, "ClearOTag", &[OT, 4])?;
    call(h, "DrawOTag", &[OT])?;
    call(h, "DrawSync", &[1])?;
    call(h, "DrawSync", &[0])?;
    call(h, "DrawSync", &[1])?;
    h.vblanks(1)?;
    // Up.
    call(h, "ResetGraph", &[0])?;
    h.vblanks(1)?;
    call(h, "SetGraphDebug", &[0])?;
    write_rect(h, ENV, 0, 0, 320, 240);
    h.write(ENV + 8, &[0; 12]);
    call(h, "PutDispEnv", &[ENV])?;
    observe_ram(h, "DISPENV", ENV, 0x14);
    h.write(ENV2, &[0; 0x5C]);
    write_rect(h, ENV2, 0, 0, 320, 240);
    h.write(ENV2 + 22, &[1, 0, 1, 0x20, 0x40, 0x80]);
    call(h, "PutDrawEnv", &[ENV2])?;
    call(h, "SetDispMask", &[1])?;
    h.vblanks(1)?;
    // The queue, with a DrawSync callback: every call queues, the pump
    // runs it at once, and the callback when it drains.
    call(h, "DrawSyncCallback", &[cb])?;
    call(h, "ClearOTagR", &[OT, 8])?;
    for i in 0..4 {
        add_tile(h, OT + 4 * rng.range(0, 8), PRIMS + 16 * i, rng);
    }
    call(h, "DrawOTag", &[OT + 28])?;
    observe_ram(h, "callback word", state, 4);
    call(h, "DrawSync", &[0])?;
    fill(h, DATA, 0x200, rng);
    write_rect(h, RECT, 320, 0, 16, 8);
    call(h, "LoadImage", &[RECT, DATA])?;
    write_rect(h, RECT2, 320, 0, 16, 8);
    h.write(DATA + 0x400, &[0xAA; 0x110]);
    call(h, "StoreImage", &[RECT2, DATA + 0x400])?;
    call(h, "DrawSync", &[0])?;
    observe_ram(h, "stored", DATA + 0x400, 0x108);
    observe_ram(h, "callback word", state, 4);
    write_rect(h, RECT, 320, 0, 16, 8);
    call(h, "MoveImage", &[RECT, 336, 8])?;
    write_rect(h, RECT, 0, 0, 100, 50);
    call(h, "ClearImage", &[RECT, 0x10, 0x20, 0x30])?;
    call(h, "DrawSync", &[1])?;
    h.write(ENV2 + 24, &[0]);
    call(h, "DrawOTagEnv", &[OT + 28, ENV2])?;
    call(h, "GetDrawEnv", &[COPY])?;
    observe_ram(h, "GetDrawEnv", COPY, 0x5C);
    observe_ram(h, "callback word", state, 4);
    h.vblanks(2)?;
    call(h, "GetODE", &[])?;
    // Past the queue: MoveImage2 makes _GPU_ResetCallback DMA2's callback,
    // which puts the pump back.
    call(h, "DrawSyncCallback", &[0])?;
    call(h, "DrawSync", &[0])?;
    write_rect(h, RECT, 0, 0, 32, 32);
    call(h, "MoveImage2", &[RECT, 512, 256])?;
    h.vblanks(1)?;
    call(h, "DrawOTag", &[OT + 28])?;
    call(h, "DrawSync", &[0])?;
    call(h, "SetDispMask", &[0])?;
    call(h, "GetDispEnv", &[COPY])?;
    observe_ram(h, "GetDispEnv", COPY, 0x14);
    h.vblanks(1)?;
    // The lesser resets.
    for mode in [1, 3, 5] {
        call(h, "ResetGraph", &[mode])?;
        h.vblanks(1)?;
        call(h, "DrawSync", &[1])?;
    }
    call(h, "PutDispEnv", &[ENV])?;
    call(h, "SetDispMask", &[1])?;
    h.vblanks(1)?;
    observe_ram(h, "callback word at the end", state, 4);
    Ok(())
}


/// `GetODE`: the field the display reads, which each vblank flips; an
/// interlaced display, then not.
fn field(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    call(h, "GetODE", &[])?;
    reset_graph0(h)?;
    for inter in [1u8, 0] {
        write_rect(h, ENV, 0, 0, 640, 480);
        h.write(ENV + 8, &[0; 8]);
        h.write(ENV + 16, &[inter, 0, 0, 0]);
        call(h, "PutDispEnv", &[ENV])?;
        for _ in 0..3 {
            h.vblanks(rng.range(1, 3))?;
            call(h, "GetODE", &[])?;
        }
    }
    Ok(())
}
