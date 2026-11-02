//! Running a fixture's guest module under wasmtime. The `psx.*` imports
//! are the fixture contract, not the runtime: FlushCache is the one BIOS
//! call original code makes; the port's diagnostics (`untranslated`,
//! `bad_jump`) are typed rejections; anything else fails the checkpoint.
//!
//! Port-only seams the fixtures use, each with a fixed contract:
//! - a BIOS stand-in pointer (`psxw-abi BIOS_STAND_INS`) returns
//!   [`STAND_IN_RESULT`] plus its offset;
//! - the first kernel stand-in calls guest `a0` with `a1` (a native flow
//!   calling back into the guest) and returns its result;
//! - ChangeThread (B0h:10h) suspends the call through Asyncify, to be
//!   resumed by a later step;
//! - `binding_fault` (a binding rule rejecting a publication, or a native
//!   call's invalid handle) is a typed rejection named by its cause.

use super::fixture::{Layout, Observation, Outcome, Step, Variant, decode_log};
use anyhow::{Context as _, Result, bail};
use psxw_abi::{WINDOW_BASE, WINDOW_MASK, guest};
use wasmtime::{Caller, Engine, Instance, Linker, Module, Store, Trap};

/// Fuel per checkpoint: far more than any fixture needs.
const FUEL: u64 = 50_000_000;
pub const STAND_IN_RESULT: u32 = 0x5100_0000;
/// Asyncify's buffer, in wasm memory above the guest window.
const ASYNC_BUF: u32 = 0x0300_0000;
const ASYNC_BYTES: u32 = 0x0001_0000;

/// A port diagnostic: an explicit rejection.
#[derive(Debug)]
struct Rejected(String);
impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Rejected {}

#[derive(Default)]
struct State {
    /// A ChangeThread suspended the current call.
    suspended: bool,
    /// The next ChangeThread is the rewind returning to it.
    rewinding: bool,
}

pub fn engine() -> Result<Engine> {
    let mut config = wasmtime::Config::new();
    config.wasm_exceptions(true);
    config.consume_fuel(true);
    Ok(Engine::new(&config)?)
}

fn reject<T>(why: String) -> wasmtime::Result<T> {
    Err(wasmtime::Error::new(Rejected(why)))
}

fn host_fail<T>(why: String) -> wasmtime::Result<T> {
    Err(wasmtime::Error::msg(why))
}

fn export<P: wasmtime::WasmParams, R: wasmtime::WasmResults>(
    c: &mut Caller<'_, State>,
    name: &str,
) -> wasmtime::Result<wasmtime::TypedFunc<P, R>> {
    c.get_export(name).and_then(|e| e.into_func()).ok_or_else(|| wasmtime::Error::msg(format!("no export {name}")))?.typed::<P, R>(&*c)
}

fn linker(engine: &Engine) -> Result<Linker<State>> {
    let mut l = Linker::new(engine);
    l.func_wrap("psx", "safepoint", || {})?;
    l.func_wrap("psx", "probe", |_: u32| {})?;
    l.func_wrap("psx", "mod_log", |_: u32, _: u32| {})?;
    l.func_wrap("psx", "untranslated", |addr: u32| -> wasmtime::Result<u64> { reject(format!("untranslated 0x{addr:08X}")) })?;
    l.func_wrap("psx", "bad_jump", |pc: u32, to: u32| -> wasmtime::Result<()> { reject(format!("bad_jump 0x{pc:08X} -> 0x{to:08X}")) })?;
    l.func_wrap("psx", "binding_fault", |rule: u32, cause: u32, value: u32| -> wasmtime::Result<()> {
        if cause == psxw_abi::binding_cause::NATIVE_HANDLE {
            return reject(format!("binding.native_handle 0x{value:08X}"));
        }
        reject(format!("{} #{rule} 0x{value:08X}", psxw_abi::binding_cause::name(cause)))
    })?;
    l.func_wrap("psx", "brk", |code: u32| -> wasmtime::Result<()> { host_fail(format!("break {code:#x}")) })?;
    l.func_wrap("psx", "overflow", |pc: u32| -> wasmtime::Result<()> { host_fail(format!("overflow at 0x{pc:08X}")) })?;
    l.func_wrap(
        "psx",
        "bios",
        |mut c: Caller<'_, State>, table: u32, func: u32, _a0: u32, _a1: u32, _a2: u32, _a3: u32| -> wasmtime::Result<u64> {
            if psxw_abi::BIOS_STAND_INS.contains(&table) {
                return Ok((STAND_IN_RESULT + (table - psxw_abi::BIOS_STAND_INS.start)) as u64);
            }
            match (table, func) {
                // FlushCache: what makes code the guest wrote executable.
                (0xA0, 0x44) => Ok(0),
                (0xB0, 0x10) => {
                    if std::mem::take(&mut c.data_mut().rewinding) {
                        export::<(), ()>(&mut c, guest::ASYNCIFY_STOP_REWIND)?.call(&mut c, ())?;
                        return Ok(1);
                    }
                    let mem = c.get_export(guest::MEMORY).and_then(|e| e.into_memory()).ok_or_else(|| wasmtime::Error::msg("no memory"))?;
                    let hdr = [(ASYNC_BUF + 8).to_le_bytes(), (ASYNC_BUF + ASYNC_BYTES).to_le_bytes()].concat();
                    mem.write(&mut c, ASYNC_BUF as usize, &hdr)?;
                    export::<u32, ()>(&mut c, guest::ASYNCIFY_START_UNWIND)?.call(&mut c, ASYNC_BUF)?;
                    c.data_mut().suspended = true;
                    Ok(0)
                }
                _ => host_fail(format!("BIOS {table:02X}h:{func:02X}h is outside the fixture contract")),
            }
        },
    )?;
    l.func_wrap(
        "psx",
        "kernel_call",
        |mut c: Caller<'_, State>, addr: u32, a0: u32, a1: u32, _a2: u32, _a3: u32| -> wasmtime::Result<u64> {
            if addr != psxw_abi::KERNEL_STAND_INS.start {
                return host_fail(format!("kernel call 0x{addr:08X} is outside the fixture contract"));
            }
            export::<(u32, u32, u32, u32, u32), u64>(&mut c, guest::INVOKE)?.call(&mut c, (a0, a1, 0, 0, 0))
        },
    )?;
    Ok(l)
}

/// The log as the guest left it.
fn read_log(store: &mut Store<State>, instance: &Instance, layout: &Layout) -> Result<Vec<u32>> {
    let mem = instance.get_memory(&mut *store, guest::MEMORY).context("no memory")?;
    let r = layout.log_range();
    let mut bytes = vec![0u8; r.len()];
    mem.read(&*store, (WINDOW_BASE + (r.start & WINDOW_MASK)) as usize, &mut bytes)?;
    Ok(decode_log(layout, &bytes))
}

fn write(store: &mut Store<State>, instance: &Instance, at: u32, bytes: &[u8]) -> Result<()> {
    let mem = instance.get_memory(&mut *store, guest::MEMORY).context("no memory")?;
    mem.write(&mut *store, (WINDOW_BASE + (at & WINDOW_MASK)) as usize, bytes)?;
    Ok(())
}

fn outcome(r: wasmtime::Result<u64>) -> Outcome {
    match r {
        Ok(v) => Outcome::Returned { v0: v as u32, v1: (v >> 32) as u32 },
        Err(e) => {
            if let Some(Rejected(why)) = e.downcast_ref::<Rejected>() {
                Outcome::Rejected { diag: why.clone() }
            } else if e.downcast_ref::<Trap>() == Some(&Trap::OutOfFuel) {
                Outcome::Failed { why: "fuel exhausted".into() }
            } else {
                Outcome::Failed { why: format!("{e:#}").lines().next().unwrap_or_default().to_string() }
            }
        }
    }
}

/// Run a variant's steps on a module: one observation per checkpoint,
/// stopping after the first that doesn't return (or suspend).
pub fn run(engine: &Engine, wasm: &[u8], v: &Variant) -> Result<Vec<Observation>> {
    let module = Module::new(engine, wasm)?;
    let mut store = Store::new(engine, State::default());
    let mut linker = linker(engine)?;
    linker.define_unknown_imports_as_traps(&module)?;
    let instance = linker.instantiate(&mut store, &module)?;
    let invoke = instance.get_typed_func::<(u32, u32, u32, u32, u32), u64>(&mut store, guest::INVOKE)?;
    let sp_set = instance.get_typed_func::<u32, ()>(&mut store, "guest_sp_set")?;
    for (at, bytes) in v.boot() {
        write(&mut store, &instance, at, &bytes)?;
    }
    let mut out = Vec::new();
    let mut pending: Option<(u32, [u32; 4])> = None;
    for step in &v.steps {
        match step {
            Step::Write { at, bytes } => write(&mut store, &instance, *at, bytes)?,
            Step::Call { .. } | Step::Resume { .. } => {
                store.set_fuel(FUEL)?;
                let (entry, args) = match step {
                    Step::Call { entry, args, .. } => {
                        sp_set.call(&mut store, v.layout.stack)?;
                        (*entry, *args)
                    }
                    _ => {
                        let call = pending.take().context("a resume with nothing suspended")?;
                        store.data_mut().rewinding = true;
                        instance.get_typed_func::<u32, ()>(&mut store, guest::ASYNCIFY_START_REWIND)?.call(&mut store, ASYNC_BUF)?;
                        call
                    }
                };
                let r = invoke.call(&mut store, (entry, args[0], args[1], args[2], args[3]));
                let mut o = outcome(r);
                if std::mem::take(&mut store.data_mut().suspended) {
                    instance.get_typed_func::<(), ()>(&mut store, guest::ASYNCIFY_STOP_UNWIND)?.call(&mut store, ())?;
                    pending = Some((entry, args));
                    o = Outcome::Suspended;
                }
                let log = read_log(&mut store, &instance, &v.layout)?;
                let stop = !matches!(o, Outcome::Returned { .. } | Outcome::Suspended);
                out.push(Observation { outcome: o, log });
                if stop {
                    break;
                }
            }
        }
    }
    if pending.is_some() {
        bail!("{}: a call is still suspended at the end", v.id());
    }
    Ok(out)
}
