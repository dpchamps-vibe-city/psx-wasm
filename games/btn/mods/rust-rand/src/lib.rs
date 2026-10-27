//! better-rand in Rust: xorshift32 over the game's own state word, keeping
//! Rand's scaling, plus a log line every 600 frames from the `frame` point.

#![no_std]

// The game's declarations (bindgen over mod.h's headers) and the checks of
// each implementation's type against its hook's.
include!(concat!(env!("PSXW_MOD_DIR"), "/mod.rs"));

use game::gRandomState;

static mut FRAMES: u32 = 0;

#[no_mangle]
pub extern "C" fn better_rand(limit: i32) -> u32 {
    // SAFETY: the game's one thread; gRandomState is the game's.
    unsafe {
        let mut x = gRandomState;
        if x == 0 {
            x = 0x2545_F491; // xorshift never leaves 0, and the state starts at 0
        }
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        gRandomState = x;
        ((x >> 16) * (limit as u32 & 0xFFFF)) >> 16
    }
}

#[no_mangle]
pub extern "C" fn on_frame() {
    // SAFETY: the game's one thread.
    let n = unsafe {
        FRAMES += 1;
        FRAMES
    };
    if n % 600 == 0 {
        psx_mod::logf!("rust-rand: {} frames, state {:08x}", n, unsafe { gRandomState });
    }
}
