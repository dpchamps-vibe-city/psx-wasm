//! Runs one harness module under wasmtime: both sides of every pair, for
//! every seed, from the same initial memory, with every call-out logged
//! and answered from the same seeded sequence.
//!
//! Stores in harness builds mark their 4 KiB page in a byte map, so a run
//! only saves, compares and restores the pages it touched.

use psxw_link::callout;
use std::collections::{BTreeMap, BTreeSet};
use wasmtime::{Caller, Config, Engine, Instance, Linker, Memory, Module, Store, TypedFunc};

pub const WINDOW_BASE: usize = psxw_abi::WINDOW_BASE as usize;
/// Dirty-page map (one byte per window page), below the window.
pub const DIRTY_MAP: u32 = 0x0010_0000;
pub const PAGE: usize = 1 << psxw_link::rewrite::DIRTY_PAGE_SHIFT;
pub const PAGES: usize = 0x0100_0000 / PAGE;
/// Guest stack top at entry; the caller's outgoing arguments sit above it.
pub const STACK_TOP: u32 = 0x801F_F000;
/// Compared memory excludes the callee's frames and the caller's argument
/// area: `[STACK_TOP - STACK_EXCLUDE, STACK_TOP + 64)`.
pub const STACK_EXCLUDE: u32 = 0x1_0000;
/// All-ones bytes just below that area. A function given a random pointer
/// may scan memory for an all-ones terminator (`0xFFFF`, `-1`) and run up
/// into the stack frames, where C and the original lay out spills
/// differently; the guard ends such a scan first, on both sides alike.
pub const STACK_GUARD: u32 = 0x100;
/// Seeded scratch memory that pointer arguments point into.
pub const SCRATCH: u32 = 0x8070_0000;
pub const SCRATCH_LEN: u32 = 0x8000;
/// Snapshot extent: RAM, AUX and the scratchpad.
const SNAPSHOT_LEN: usize = 0x0080_0400;
const MAX_CALLOUTS: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Callout {
    pub kind: u32,
    pub target: u32,
    /// Argument words the caller declared (C side) or 4 (translated side).
    pub nwords: u32,
    pub args: [u32; 4],
    /// The first stack argument words, `sp + 16 ..`.
    pub stack: [u32; 8],
    /// What the harness answered.
    pub ret: i64,
}

#[derive(Debug, Clone, Default)]
pub struct State {
    pub seed: u64,
    pub log: Vec<Callout>,
    /// Return kinds of direct callees, by address: (bits, signed); 0 = 64,
    /// 1 = a pointer.
    pub ret_ext: BTreeMap<u32, (u32, bool)>,
}

pub fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// The value a call-out returns: the same for both sides at the same index.
fn answer(st: &State, index: usize, kind: u32, target: u32) -> i64 {
    let r = mix(st.seed ^ ((index as u64 + 1) << 40));
    let v: u64 = match r & 3 {
        0 => (r >> 8) % 4,
        1 => ((r >> 8) as i64 % 3 - 1) as u64,
        _ => r >> 8,
    };
    let (bits, signed) = if kind == callout::CALL { st.ret_ext.get(&target).copied().unwrap_or((32, false)) } else { (32, false) };
    if bits == 1 {
        // A pointer: NULL or somewhere in the scratch arena, never garbage.
        let p = if (r >> 8) % 4 == 0 { 0 } else { SCRATCH + ((r >> 16) as u32 % (SCRATCH_LEN - 64) & !3) };
        return (p as u64 | (r << 32 & 0xFFFF_FFFF_0000_0000)) as i64;
    }
    let v = match (bits, signed) {
        (8, true) => v as u8 as i8 as i32 as u32 as u64,
        (8, false) => v as u8 as u64,
        (16, true) => v as u16 as i16 as i32 as u32 as u64,
        (16, false) => v as u16 as u64,
        (0, _) => v,
        _ => v & 0xFFFF_FFFF | (r << 32 & 0xFFFF_FFFF_0000_0000),
    };
    v as i64
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Returned(u64),
    Trap(String),
    Fuel,
}

pub struct Harness {
    store: Store<State>,
    instance: Instance,
    memory: Memory,
    snapshot: Vec<u8>,
    sp: wasmtime::Global,
    fuel: u64,
}

pub struct Run {
    pub outcome: Outcome,
    pub log: Vec<Callout>,
    /// Pages written, plus the setup's pages.
    pub dirty: BTreeSet<usize>,
}

impl Harness {
    pub fn new(wasm: &[u8], images: &[(u32, &[u8])], ret_ext: BTreeMap<u32, (u32, bool)>, fuel: u64) -> anyhow::Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true);
        let engine = Engine::new(&config)?;
        let module = Module::new(&engine, wasm)?;
        let mut linker: Linker<State> = Linker::new(&engine);
        linker.func_wrap(
            "h",
            "callout",
            |mut caller: Caller<'_, State>,
             kind: i32,
             target: i32,
             nwords: i32,
             a0: i32,
             a1: i32,
             a2: i32,
             a3: i32,
             sp: i32|
             -> wasmtime::Result<i64> {
                let mem = caller.get_export("memory").and_then(|e| e.into_memory()).expect("memory");
                let data = mem.data(&caller);
                let mut stack = [0u32; 8];
                for (k, w) in stack.iter_mut().enumerate() {
                    let a = (((sp as u32).wrapping_add(16 + 4 * k as u32)) & 0x00FF_FFFF) as usize + WINDOW_BASE;
                    *w = u32::from_le_bytes([data[a], data[a + 1], data[a + 2], data[a + 3]]);
                }
                let (kind, target) = (kind as u32, target as u32);
                let st = caller.data_mut();
                if st.log.len() >= MAX_CALLOUTS {
                    return Err(wasmtime::Error::msg("call-out limit"));
                }
                st.log.push(Callout {
                    kind,
                    target,
                    nwords: nwords as u32,
                    args: [a0 as u32, a1 as u32, a2 as u32, a3 as u32],
                    stack,
                    ret: 0,
                });
                match kind {
                    callout::TRAP_OVERFLOW => return Err(wasmtime::Error::msg(format!("overflow at {target:08X}"))),
                    callout::BAD_JUMP => {
                        return Err(wasmtime::Error::msg(format!("bad jump at {target:08X} to {:08X}", a0 as u32)));
                    }
                    callout::BREAK => return Ok(0),
                    _ => {}
                }
                let i = st.log.len() - 1;
                let v = answer(st, i, kind, target);
                st.log[i].ret = v;
                Ok(v)
            },
        )?;
        let mut store = Store::new(&engine, State { ret_ext, ..Default::default() });
        store.set_fuel(u64::MAX)?;
        let instance = linker.instantiate(&mut store, &module)?;
        let memory = instance.get_memory(&mut store, "memory").ok_or_else(|| anyhow::anyhow!("no memory export"))?;
        let sp = instance.get_global(&mut store, "__guest_sp").ok_or_else(|| anyhow::anyhow!("no __guest_sp"))?;
        for (addr, bytes) in images {
            let at = (*addr & 0x00FF_FFFF) as usize + WINDOW_BASE;
            memory.data_mut(&mut store)[at..at + bytes.len()].copy_from_slice(bytes);
        }
        let guard = ((STACK_TOP - STACK_EXCLUDE - STACK_GUARD) & 0x00FF_FFFF) as usize + WINDOW_BASE;
        memory.data_mut(&mut store)[guard..guard + STACK_GUARD as usize].fill(0xFF);
        let snapshot = memory.data(&store)[WINDOW_BASE..WINDOW_BASE + SNAPSHOT_LEN].to_vec();
        Ok(Self { store, instance, memory, snapshot, sp, fuel })
    }

    pub fn func(&mut self, name: &str) -> anyhow::Result<TypedFunc<(i32, i32, i32, i32), i64>> {
        Ok(self.instance.get_typed_func(&mut self.store, name)?)
    }

    /// Guest memory at a guest address.
    pub fn read(&self, addr: u32, len: usize) -> Vec<u8> {
        let at = (addr & 0x00FF_FFFF) as usize + WINDOW_BASE;
        self.memory.data(&self.store)[at..at + len].to_vec()
    }

    pub fn snapshot_page(&self, p: usize) -> &[u8] {
        let start = p * PAGE;
        let end = (start + PAGE).min(self.snapshot.len());
        if start >= self.snapshot.len() { &[] } else { &self.snapshot[start..end] }
    }

    pub fn page(&self, p: usize) -> &[u8] {
        let start = WINDOW_BASE + p * PAGE;
        &self.memory.data(&self.store)[start..start + PAGE]
    }

    /// Put pages back to the snapshot.
    pub fn restore(&mut self, pages: &BTreeSet<usize>) {
        for &p in pages {
            let start = p * PAGE;
            if start >= self.snapshot.len() {
                // Outside the snapshot: zero it.
                let at = WINDOW_BASE + start;
                self.memory.data_mut(&mut self.store)[at..at + PAGE].fill(0);
                continue;
            }
            let end = (start + PAGE).min(self.snapshot.len());
            let (snap, mem) = (&self.snapshot[start..end], WINDOW_BASE + start);
            self.memory.data_mut(&mut self.store)[mem..mem + (end - start)].copy_from_slice(snap);
        }
    }

    /// Run one side: seed the scratch arena and stack arguments, call `f`.
    /// Run `f` with `fuel` scaled by `fuel_scale` (translated code spends
    /// more wasm instructions per guest instruction than compiled C).
    pub fn run(
        &mut self,
        f: &TypedFunc<(i32, i32, i32, i32), i64>,
        seed: u64,
        args: [u32; 4],
        scratch: &[u8],
        stack_args: &[u32; 8],
        fuel_scale: u64,
    ) -> Run {
        let data = self.memory.data_mut(&mut self.store);
        let map = DIRTY_MAP as usize;
        data[map..map + PAGES].fill(0);
        let s = (SCRATCH & 0x00FF_FFFF) as usize + WINDOW_BASE;
        data[s..s + scratch.len()].copy_from_slice(scratch);
        let a = ((STACK_TOP + 16) & 0x00FF_FFFF) as usize + WINDOW_BASE;
        for (k, w) in stack_args.iter().enumerate() {
            data[a + 4 * k..a + 4 * k + 4].copy_from_slice(&w.to_le_bytes());
        }
        let st = self.store.data_mut();
        st.seed = seed;
        st.log.clear();
        let _ = self.sp.set(&mut self.store, wasmtime::Val::I32(STACK_TOP as i32));
        let _ = self.store.set_fuel(self.fuel * fuel_scale);
        let r = f.call(&mut self.store, (args[0] as i32, args[1] as i32, args[2] as i32, args[3] as i32));
        let outcome = match r {
            Ok(v) => Outcome::Returned(v as u64),
            Err(e) => {
                if self.store.get_fuel().is_ok_and(|f| f == 0) {
                    Outcome::Fuel
                } else if let Some(t) = e.downcast_ref::<wasmtime::Trap>() {
                    Outcome::Trap(t.to_string())
                } else {
                    Outcome::Trap(e.root_cause().to_string())
                }
            }
        };
        let data = self.memory.data(&self.store);
        let mut dirty: BTreeSet<usize> = (0..PAGES).filter(|&p| data[map + p] != 0).collect();
        let sp = (SCRATCH & 0x00FF_FFFF) as usize / PAGE;
        dirty.extend(sp..sp + SCRATCH_LEN as usize / PAGE);
        // The whole stack area: adapters write outgoing arguments there
        // without marking, and each side must start from the same stack.
        let lo = ((STACK_TOP - STACK_EXCLUDE) & 0x00FF_FFFF) as usize / PAGE;
        dirty.extend(lo..=((STACK_TOP + 64) & 0x00FF_FFFF) as usize / PAGE);
        // A store may straddle into the next page; past the window's last
        // page that is the guard page, which is tracked like the others.
        let next: Vec<usize> = dirty.iter().map(|p| p + 1).filter(|&p| p <= PAGES).collect();
        dirty.extend(next);
        Run { outcome, log: std::mem::take(&mut self.store.data_mut().log), dirty }
    }
}
