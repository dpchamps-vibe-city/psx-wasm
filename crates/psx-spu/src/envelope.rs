//! The envelope stepper shared by the ADSR generator and the volume sweeps
//! (psx-spx "Envelope Operation depending on Shift/Step/Mode/Direction").

/// One envelope phase's parameters.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rate {
    /// 0..1Fh, fast..slow.
    pub shift: u8,
    /// 0..3: "+7,+6,+5,+4", or "-8,-7,-6,-5" when decreasing.
    pub step: u8,
    pub exponential: bool,
    pub decreasing: bool,
    /// Sweep phase bit (volume sweeps only).
    pub negative: bool,
}

impl Rate {
    /// Whether this rate never steps (and never saturates): step and shift
    /// all ones.
    pub fn never(&self) -> bool {
        (self.step | self.shift << 2) == 0x7F
    }
}

/// Advance `level` by one 44.1 kHz tick. `counter` is the envelope's step
/// counter; a step happens when its bit 15 becomes set. Returns the new
/// level and whether it stepped.
pub fn step(level: i16, counter: &mut u16, r: Rate) -> (i16, bool) {
    let shift = r.shift as i32;
    let mut step: i32 = 7 - r.step as i32;
    if r.decreasing ^ r.negative {
        step = !step;
    }
    step <<= (11 - shift).max(0);
    let mut inc: u32 = 0x8000 >> (shift - 11).max(0).min(16);
    let lvl = level as i32;
    // oracle: the slower exponential increase starts at 6000h itself
    // (psx-spx: "AdsrLevel>6000h").
    if r.exponential && !r.decreasing && lvl >= 0x6000 {
        if shift < 10 {
            step >>= 2;
        } else if shift >= 11 {
            inc >>= 2;
        } else {
            step >>= 1;
            inc >>= 1;
        }
    } else if r.exponential && r.decreasing {
        step = (step * lvl) >> 15;
    }
    if !r.never() {
        inc = inc.max(1);
    }
    let c = *counter as u32 + inc;
    if c & 0x8000 == 0 {
        *counter = c as u16;
        return (level, false);
    }
    *counter = 0;
    let next = lvl + step;
    let next = if !r.decreasing {
        next.clamp(-0x8000, 0x7FFF)
    } else if r.negative {
        next.clamp(-0x8000, 0)
    } else {
        next.max(0)
    };
    (next as i16, true)
}

/// Advance a volume register's sweep by one tick (psx-spx "Sweep Volume
/// Mode"), as the device oracle showed it behaves:
/// - oracle: the level saturates on every tick, not only on steps;
/// - oracle: exponential decrease ignores the phase bit for its step (it
///   always moves the level towards zero), and in negative phase it does
///   not clamp at zero (a negative level decays towards zero from below);
/// - oracle: exponential increase slows down when the low 15 bits of the
///   level (inverted in negative phase) reach 6000h, so with positive
///   phase a level slows down from -2000h upwards as well as from 6000h.
///
/// The step counter is not reset by writing the register; it starts at
/// zero after a step.
pub fn sweep(level: i16, counter: &mut u16, raw: u16) -> i16 {
    let shift = ((raw >> 2) & 0x1F) as i32;
    let stepv = (raw & 3) as i32;
    let exponential = raw & 0x4000 != 0;
    let decreasing = raw & 0x2000 != 0;
    let negative = raw & 0x1000 != 0;
    let lvl = level as i32;
    let mut step: i32 = 7 - stepv;
    if decreasing ^ (negative && !(exponential && decreasing)) {
        step = !step;
    }
    step <<= (11 - shift).max(0);
    let mut inc: u32 = 0x8000 >> (shift - 11).max(0).min(16);
    if exponential && !decreasing {
        let x = if negative { !lvl } else { lvl } & 0x7FFF;
        if x >= 0x6000 {
            if shift < 10 {
                step >>= 2;
            } else if shift >= 11 {
                inc >>= 2;
            } else {
                step >>= 1;
                inc >>= 1;
            }
        }
    } else if exponential && decreasing {
        step = (step * lvl) >> 15;
    }
    if (stepv | shift << 2) != 0x7F {
        inc = inc.max(1);
    }
    let c = *counter as u32 + inc;
    let mut next = lvl;
    if c & 0x8000 != 0 {
        *counter = 0;
        next += step;
    } else {
        *counter = c as u16;
    }
    let next = if !decreasing || (exponential && negative) {
        next.clamp(-0x8000, 0x7FFF)
    } else if negative {
        next.clamp(-0x8000, 0)
    } else {
        next.clamp(0, 0x7FFF)
    };
    next as i16
}
