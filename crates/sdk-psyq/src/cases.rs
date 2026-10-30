//! Adapter conformance cases (plans/unified-adaptor-layer, "Conformance";
//! D59). A case calls SDK functions by name and records what code staying
//! original could observe. `psxw conform <game> --chip C` runs every case
//! of chip C twice: on the game with the chip native (its adapters) and
//! with the chip on its device (the original library over the device
//! model), and the two records must be equal. Cases name functions, never
//! addresses, so the same case runs in every game that links them
//! (level 3).
//!
//! - **Level 1:** one adapter, with seeded random arguments within its
//!   preconditions.
//! - **Level 2:** a chip's adapters in sequence (init, use, sync,
//!   callbacks), including calls from library code that stays original.
//!
//! The observation specs: return values and out-parameters exactly; the
//! device effect at the core's API ([`Harness::device`]: the renderer's
//! commands, the audio); library state only through functions that read
//! it; callbacks by their effect on a word, to the frame.

mod gpu;
mod irq;

/// What a case runs against: one game's build, one side.
pub trait Harness {
    /// Whether the game links `name`.
    fn linked(&self, name: &str) -> bool;
    /// Call an SDK function by name, o32 arguments (from the fifth on, on
    /// the stack).
    fn call(&mut self, name: &str, args: &[u32]) -> Result<u32, String>;
    fn read(&mut self, addr: u32, len: usize) -> Vec<u8>;
    fn write(&mut self, addr: u32, bytes: &[u8]);
    fn vblanks(&mut self, n: u32) -> Result<(), String>;
    /// Record an observation.
    fn observe(&mut self, what: &str, value: String);
    /// The device effect so far, at the core's API both sides reach: the
    /// renderer's command stream, the displays presented at each vblank
    /// (what GP1 set), and the audio, hashed.
    fn device(&mut self) -> String;
    /// A guest function that counts its calls in a word, and that word
    /// (game.toml `[conform]`), for callbacks.
    fn callback(&self) -> Option<(u32, u32)>;
}

/// Free RAM for a case's buffers: below the stack, above game data.
pub const SCRATCH: u32 = 0x801C_0000;

pub struct Case {
    pub name: &'static str,
    pub chip: &'static str,
    pub level: u8,
    /// The adapters it exercises.
    pub covers: &'static [&'static str],
    /// Functions it calls; it runs only in games that link them all.
    pub needs: &'static [&'static str],
    pub run: fn(&mut dyn Harness, &mut Rng) -> Result<(), String>,
}

/// xorshift64*, for seeded arguments.
pub struct Rng(pub u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `lo..hi`.
    pub fn range(&mut self, lo: u32, hi: u32) -> u32 {
        lo + (self.next() % (hi - lo).max(1) as u64) as u32
    }
}

/// Call `name` and record its result.
pub fn call(h: &mut dyn Harness, name: &str, args: &[u32]) -> Result<u32, String> {
    let v = h.call(name, args)?;
    let shown: Vec<String> = args.iter().map(|a| format!("{a:#x}")).collect();
    h.observe(&format!("{name}({})", shown.join(", ")), format!("{v:#x}"));
    Ok(v)
}

/// Record `len` bytes at `addr`, in hex.
pub fn observe_ram(h: &mut dyn Harness, what: &str, addr: u32, len: usize) {
    let b = h.read(addr, len);
    h.observe(what, b.iter().map(|x| format!("{x:02x}")).collect());
}

/// Every case.
pub fn all() -> Vec<Case> {
    let mut v = Vec::new();
    v.extend(irq::cases());
    v.extend(gpu::cases());
    v
}

/// The chip each hardware adapter belongs to (D59). BIOS-internals
/// adapters (libapi's pad and patch members, libcard, libmcx's BIOS card
/// path) have none: they are mandatory and need no device side.
pub fn chip_of(adapter: &str) -> Option<&'static str> {
    crate::chip_adapters().into_iter().find(|(n, _)| *n == adapter).map(|(_, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Adapters whose cases their chip's task (phase 1 task 9) still owes.
    /// The gate below fails when one of these gets its cases, so the list
    /// only shrinks; phase 1 ends with it empty.
    const OWED: &[&str] = &[
        "CdInit", "CdReset", "CdControl", "CdControlF", "CdControlB", "CdSync", "CdReady", "CdGetSector", "CdGetSector2", "CdRead", "CdReadSync", "CdReadBreak", "CdReadCallback", "CdSyncCallback",
        "CdReadyCallback", "CdDataCallback", "CdFlush", "CdDataSync", "CdStatus", "CdMode", "CdLastCom", "CdSearchFile",
    ];

    /// The CI gate (phase 1 task 5): every hardware adapter has a level-1
    /// case, and its chip a level-2 sequence.
    #[test]
    fn every_adapter_has_cases() {
        let cases = all();
        let mut missing = Vec::new();
        for (name, chip) in crate::chip_adapters() {
            let owed = OWED.contains(&name);
            let has = cases.iter().any(|c| c.level == 1 && c.covers.contains(&name)) && cases.iter().any(|c| c.level == 2 && c.chip == chip);
            match (has, owed) {
                (false, false) => missing.push(format!("{name}: no level-1 case, or no level-2 sequence for chip {chip}")),
                (true, true) => missing.push(format!("{name}: has its cases now; take it off OWED")),
                _ => {}
            }
        }
        missing.sort();
        missing.dedup();
        assert!(missing.is_empty(), "adapters without conformance cases:\n  {}", missing.join("\n  "));
    }

    #[test]
    fn case_names_are_unique() {
        let cases = all();
        let mut names: Vec<&str> = cases.iter().map(|c| c.name).collect();
        names.sort();
        let n = names.len();
        names.dedup();
        assert_eq!(n, names.len());
    }
}
