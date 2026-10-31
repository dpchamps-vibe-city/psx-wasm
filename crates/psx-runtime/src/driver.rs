//! Running BIOS calls and adapters to completion (docs/design.md §6).
//!
//! A call returns a value or a wait; while it waits, virtual time advances
//! event by event, and the callbacks events queue run as guest calls. The
//! machine is borrowed only between guest calls, never across one, so a
//! callback can call back into the runtime.

use psx_bios::{Flow, GuestCall, IrqStep, Machine, Sdk, Wait};
use std::cell::RefCell;

/// Calls into the guest (the dispatch table).
pub trait Invoke {
    fn invoke(&mut self, call: GuestCall) -> u64;
    /// The same in exception context: a ReturnFromException in the call
    /// unwinds to here, and `Machine::rfe` says so.
    fn invoke_irq(&mut self, call: GuestCall) -> u64 {
        self.invoke(call)
    }
    /// The disc wrote `len` bytes at `addr` (mods' overlay data patches).
    fn loaded(&mut self, _addr: u32, _len: u32) {}
    /// Service a host-requested call, with no machine borrow held and
    /// outside interrupt handlers and critical sections.
    fn host_calls(&mut self) {}
}

/// Run queued callbacks, unless one is already running (no nesting:
/// interrupts are disabled in a handler, as on hardware). Then service a
/// pending host call, with the machine unborrowed.
pub fn drain<S: Sdk>(rt: &RefCell<Machine<S>>, inv: &mut dyn Invoke) {
    // What the disc wrote first, so no guest code sees an overlay before
    // its mods' patches (even in a handler).
    let loaded = std::mem::take(&mut rt.borrow_mut().loaded);
    for (addr, len) in loaded {
        inv.loaded(addr, len);
    }
    loop {
        let (step, exception) = {
            let mut m = rt.borrow_mut();
            if m.in_irq || m.crit {
                return;
            }
            // The exception handler's steps first (it runs before anything
            // the kernel queues).
            let step = match m.irq_calls.pop_front() {
                Some(s) => (s, true),
                None => match m.calls.pop_front() {
                    Some(c) => (IrqStep::Chain(c, None), false),
                    None => break,
                },
            };
            if step.0 == IrqStep::Exit && m.kernel.custom_exit == 0 {
                continue;
            }
            // The adapters' dispatcher: Rust, no guest call; it queues the
            // callbacks it runs at the front.
            if let IrqStep::Sdk(s) = step.0 {
                S::irq_step(&mut m, s);
                continue;
            }
            m.in_irq = true;
            m.rfe = false;
            step
        };
        let rfe = || std::mem::take(&mut rt.borrow_mut().rfe);
        match step {
            IrqStep::Chain(call, then) => {
                let v = inv.invoke_irq(call) as u32;
                let mut returned = rfe();
                if let (Some(handler), true, false) = (then, v != 0, returned) {
                    inv.invoke_irq(GuestCall { addr: handler, args: [v, 0, 0, 0] });
                    returned = rfe();
                }
                // Returning from the exception skips the rest of it.
                if returned && exception {
                    let mut m = rt.borrow_mut();
                    while let Some(s) = m.irq_calls.pop_front() {
                        if s == IrqStep::Exit {
                            break;
                        }
                    }
                }
            }
            IrqStep::Sdk(_) => unreachable!("run above"),
            IrqStep::Exit => {
                let buf = rt.borrow().kernel.custom_exit;
                let resume = rt.borrow().mem.r32(buf);
                inv.invoke_irq(GuestCall { addr: resume, args: [buf, 1, 0, 0] });
                if !rfe() {
                    panic!("the custom exception exit at {resume:#010x} returned instead of calling ReturnFromException");
                }
            }
        }
        rt.borrow_mut().in_irq = false;
    }
    inv.host_calls();
}

/// Run a flow to completion.
pub fn run<S: Sdk>(rt: &RefCell<Machine<S>>, inv: &mut dyn Invoke, mut flow: Flow<S>) -> u64 {
    loop {
        match flow {
            Flow::Return(v) => {
                // An adapter whose original makes no runtime call here
                // returns without a safepoint (`Machine::quiet`, D63).
                if !std::mem::take(&mut rt.borrow_mut().quiet) {
                    drain(rt, inv);
                }
                return v;
            }
            Flow::Block { until, then } => {
                let start = rt.borrow().clock.now;
                loop {
                    drain(rt, inv);
                    let mut m = rt.borrow_mut();
                    if m.holds(&until) {
                        break;
                    }
                    // With interrupts disabled nothing can satisfy an event
                    // wait; hardware would hang (or time out, like VSync).
                    if (m.in_irq || m.crit) && m.clock.now - start > 2 * psx_devices::clock::CPU_HZ {
                        panic!("blocked for 2 s of virtual time with interrupts disabled, waiting for {until:?}");
                    }
                    let now = m.clock.now;
                    let limit = match until {
                        Wait::Cycle(t) => t,
                        _ => m.clock.next_time().unwrap_or(now + psx_devices::clock::CPU_HZ),
                    };
                    m.step(limit.max(now));
                }
                let mut m = rt.borrow_mut();
                flow = then(&mut m);
            }
            Flow::Call { call, then } => {
                let v = inv.invoke(call);
                let mut m = rt.borrow_mut();
                flow = then(&mut m, v);
            }
        }
    }
}

/// A safepoint: advance virtual time by a quantum, deliver what fell due,
/// run its callbacks.
pub fn safepoint<S: Sdk>(rt: &RefCell<Machine<S>>, inv: &mut dyn Invoke, quantum: u64) {
    {
        let mut m = rt.borrow_mut();
        let until = m.clock.now + quantum;
        m.run_until(until);
    }
    drain(rt, inv);
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_devices::mem::VecMem;
    use sdk_psyq::Psyq;

    struct Host;
    impl psx_bios::Host for Host {
        fn log(&mut self, _: u32, _: &str) {}
        fn wait_vblank(&mut self, _: u64) {}
        fn disc_read(&mut self, _: u32, _: &mut [u8; 2352]) -> bool {
            false
        }
        fn has_disc(&mut self) -> bool {
            false
        }
        fn pad_state(&mut self, _: u32) -> Vec<u8> {
            vec![]
        }
        fn storage_read(&mut self, _: &str) -> Option<Vec<u8>> {
            None
        }
        fn storage_write(&mut self, _: &str, _: &[u8]) -> bool {
            false
        }
        fn report(&mut self, _: &str) {}
    }
    struct Regs(u32);
    impl psx_bios::Regs for Regs {
        fn sp(&self) -> u32 {
            self.0
        }
        fn set_sp(&mut self, sp: u32) { self.0 = sp; }
        fn gp(&self) -> u32 {
            0
        }
        fn set_gp(&mut self, _: u32) {}
    }

    /// A guest that records its calls; addresses in `rfe` return from the
    /// exception (as `psx.return_from_exception` does before unwinding).
    struct Guest<'a> {
        rt: &'a RefCell<Machine<Psyq>>,
        rfe: Vec<u32>,
        seen: Vec<(u32, [u32; 4])>,
    }
    impl Invoke for Guest<'_> {
        fn invoke(&mut self, call: GuestCall) -> u64 {
            let flow = self.rt.borrow_mut().kernel_call(call.addr, call.args);
            if let Some(flow) = flow {
                return run(self.rt, self, flow);
            }
            if call.addr == psxw_abi::BIOS_RETURN_FROM_EXCEPTION {
                self.rt.borrow_mut().rfe = true;
                return 0;
            }
            self.seen.push((call.addr, call.args));
            if self.rfe.contains(&call.addr) {
                self.rt.borrow_mut().rfe = true;
            }
            1
        }
    }

    fn machine() -> RefCell<Machine<Psyq>> {
        RefCell::new(Machine::new(Box::new(VecMem::default()), Box::new(Host), Box::new(Regs(0x801F_F000)), Default::default(), Psyq::default()))
    }

    /// A `SysEnqIntRP` element at `at` (verifier, handler) in a chain.
    fn element(rt: &RefCell<Machine<Psyq>>, prio: u32, at: u32, verifier: u32, handler: u32) {
        let mut m = rt.borrow_mut();
        m.mem.w32(at + 4, handler);
        m.mem.w32(at + 8, verifier);
        m.enq_int_rp(prio, at);
    }

    /// A queued guest callback delivers ERR, cancels CdGetStatus, or writes
    /// the patched command's local status byte before delivering DNE. The
    /// ordinary driver resumes immediately with the frame intact throughout.
    #[test]
    fn cd_event_wait_wakes_from_a_guest_callback_without_advancing_time() {
        use psx_bios::{bios, cdrom};
        const DELIVER: u32 = 0x8001_1000;
        const ERROR: u32 = 0x8001_2000;
        const STATUS: u32 = 0x8001_3000;
        const SP: u32 = 0x801F_F000;
        struct CdGuest<'a> {
            rt: &'a RefCell<Machine<Psyq>>,
            seen: Vec<u32>,
            cancel: bool,
            status: Option<u32>,
        }
        impl Invoke for CdGuest<'_> {
            fn invoke(&mut self, call: GuestCall) -> u64 {
                self.seen.push(call.addr);
                assert_eq!(self.rt.borrow().regs.sp(), SP - 24);
                if call.addr == STATUS {
                    let mut m = self.rt.borrow_mut();
                    assert!(!m.in_irq);
                    assert_eq!(call.args, [SP - 8, 0, 0, 0]);
                    m.mem.write(SP - 24, &[0xCC; 16]); // o32 argument homes
                    m.mem.w8(call.args[0], 0x42);
                    self.status = Some(call.args[0]);
                    return 1;
                }
                if call.addr == DELIVER {
                    assert!(self.rt.borrow().in_irq);
                    let flow = if let Some(status) = self.status {
                        let mut m = self.rt.borrow_mut();
                        assert_eq!(m.mem.r8(status), 0x42);
                        m.mem.w8(status, 0xA5);
                        bios::dispatch(&mut m, 0xB0, 0x07, [cdrom::EV_CD, cdrom::DNE, 0, 0]).unwrap()
                    } else if self.cancel {
                        bios::dispatch(&mut self.rt.borrow_mut(), 0xA0, 0x95, [0; 4]).unwrap()
                    } else {
                        bios::dispatch(&mut self.rt.borrow_mut(), 0xB0, 0x07,
                            [cdrom::EV_CD, cdrom::ERR, 0, 0]).unwrap()
                    };
                    run(self.rt, self, flow)
                } else {
                    assert_eq!(call.addr, ERROR);
                    assert_eq!(call.args, [0x44, 0x20, 0, 0]);
                    let m = self.rt.borrow();
                    assert!(!m.in_irq);
                    assert!(!m.event_ready(m.kernel.cd.events[4]), "ERR consumed before SystemError");
                    123
                }
            }
        }
        for (cancel, software) in [(false, false), (true, false), (false, true)] {
            let rt = machine();
            let (flow, now, due) = {
                let mut m = rt.borrow_mut();
                m.cd_init();
                m.mem.w32(psx_bios::tables::entry(0xA0, 0xA1).unwrap(), ERROR);
                if software { m.mem.w32(psx_bios::tables::entry(0xA0, 0x7C).unwrap(), STATUS); }
                let flow = bios::dispatch(&mut m, 0xA0, 0xA6, [0; 4]).unwrap();
                m.calls.push_back(GuestCall { addr: DELIVER, args: [0; 4] });
                (flow, m.clock.now, m.cd_due())
            };
            assert!(due > now);
            let mut guest = CdGuest { rt: &rt, seen: vec![], cancel, status: None };
            assert_eq!(run(&rt, &mut guest, flow), if software { 0xA5 } else { u32::MAX as u64 });
            assert_eq!(guest.seen, if software { [STATUS, DELIVER] } else { [DELIVER, ERROR] });
            assert_eq!(rt.borrow().regs.sp(), SP);
            assert_eq!(rt.borrow().clock.now, now);
            assert_eq!(rt.borrow().clock.pending(|e| *e == psx_bios::Event::Cd),
                if cancel || software { None } else { Some(due) });
        }
    }

    /// A callback can patch TestEvent while a native CD wait is asleep.
    /// Synthetic replies need no flags/time; a wrapper around the original
    /// must let scheduled work progress even when the wrapper has no loop.
    #[test]
    fn cd_testevent_patch_wakes_and_yields_through_the_driver() {
        use psx_bios::{bios, tables};
        use std::{cell::Cell, rc::Rc};
        const INSTALL: u32 = 0x8001_1000;
        const POLL: u32 = 0x8001_2000;
        const SP: u32 = 0x801F_F000;
        struct PollRegs {
            sp: u32,
            budget: Rc<Cell<u32>>,
            resets: Rc<Cell<u32>>,
        }
        impl psx_bios::Regs for PollRegs {
            fn sp(&self) -> u32 { self.sp }
            fn set_sp(&mut self, sp: u32) { self.sp = sp; }
            fn gp(&self) -> u32 { 0 }
            fn set_gp(&mut self, _: u32) {}
            fn budget(&self) -> u32 { self.budget.get() }
            fn reset_budget(&mut self) {
                self.budget.set(psx_bios::machine::LOOP_BUDGET as u32);
                self.resets.set(self.resets.get() + 1);
            }
        }
        struct PollGuest<'a> {
            rt: &'a RefCell<Machine<Psyq>>,
            wrap: bool,
            saved: u32,
            polls: usize,
            expected: usize,
        }
        impl Invoke for PollGuest<'_> {
            fn invoke(&mut self, call: GuestCall) -> u64 {
                let mut m = self.rt.borrow_mut();
                assert_eq!(m.regs.sp(), SP - 24);
                if call.addr == INSTALL {
                    assert!(m.in_irq);
                    assert!(!m.event_ready(m.kernel.cd.events[1]));
                    m.mem.w8(SP - 8, 0xA5);
                    m.mem.w32(tables::entry(0xB0, 0x0B).unwrap(), POLL);
                    return 0;
                }
                assert_eq!(call.addr, POLL);
                assert!(!m.in_irq, "TestEvent is an ordinary call, not a callback");
                assert_eq!(m.regs.budget(), if m.clock.now == 0 { 7 } else { psx_bios::machine::LOOP_BUDGET as u32 });
                let slot = if self.polls % 2 == 0 { 1 } else { 4 };
                assert_eq!(call.args, [m.kernel.cd.events[slot], 0, 0, 0]);
                self.polls += 1;
                if !self.wrap {
                    assert_eq!(m.clock.now, 0, "success on the second round needs no yield");
                    return if self.polls == 3 { u32::MAX as u64 } else { 1 << 32 };
                }
                // No loop or safepoint in this guest wrapper. The outer BIOS
                // polling loop, not this callback, must yield to the drive.
                assert!(self.polls <= self.expected);
                let (t, f) = tables::stand_in_fn(self.saved).unwrap();
                match bios::call(&mut m, t, f, call.args).unwrap() {
                    Flow::Return(v) => v,
                    _ => panic!("native TestEvent is nonblocking"),
                }
            }
        }
        for wrap in [false, true] {
            let rt = machine();
            let budget = Rc::new(Cell::new(7));
            let resets = Rc::new(Cell::new(0));
            let (flow, saved, due) = {
                let mut m = rt.borrow_mut();
                m.regs = Box::new(PollRegs { sp: SP, budget: budget.clone(), resets: resets.clone() });
                m.cd_init();
                let saved = m.mem.r32(tables::entry(0xB0, 0x0B).unwrap());
                let flow = bios::dispatch(&mut m, 0xA0, 0xA6, [0; 4]).unwrap();
                let Flow::Block { ref until, .. } = flow else { panic!("native CD wait") };
                assert!(!m.holds(until));
                m.calls.push_back(GuestCall { addr: INSTALL, args: [0; 4] });
                (flow, saved, m.cd_due())
            };
            let yields = due.div_ceil(rt.borrow().quantum());
            let expected = (2 * yields * psx_bios::machine::LOOP_BUDGET + 1) as usize;
            let mut guest = PollGuest { rt: &rt, wrap, saved, polls: 0, expected };
            assert_eq!(run(&rt, &mut guest, flow), if wrap { 2 } else { 0xA5 });
            let m = rt.borrow();
            assert_eq!(m.regs.sp(), SP);
            assert!(!m.event_ready(m.kernel.cd.events[1]));
            if wrap {
                assert!(m.clock.now >= due);
                assert!(m.clock.now < due + m.quantum());
                assert_eq!(guest.polls, expected);
                assert_eq!(resets.get() as u64, yields);
                assert_eq!(budget.get() as u64, psx_bios::machine::LOOP_BUDGET);
                assert!(m.clock.pending(|e| *e == psx_bios::Event::Cd).is_none());
            } else {
                assert_eq!(guest.polls, 3);
                assert_eq!(resets.get(), 0);
                assert_eq!(budget.get(), 7);
                assert_eq!(m.clock.now, 0);
                assert_eq!(m.clock.pending(|e| *e == psx_bios::Event::Cd), Some(due));
            }
        }
    }

    /// The ABI 10 host: `vblank` at every delivered vblank, `wait_vblank`
    /// through the runtime's hold.
    struct Paced {
        seen: std::rc::Rc<RefCell<Vec<String>>>,
        hold: std::rc::Rc<crate::hold::Hold>,
    }
    impl psx_bios::Host for Paced {
        fn log(&mut self, _: u32, _: &str) {}
        fn vblank(&mut self, f: u64) {
            self.seen.borrow_mut().push(format!("vblank {f}"));
        }
        fn wait_vblank(&mut self, f: u64) {
            let seen = self.seen.clone();
            self.hold.wait(f, move |f| seen.borrow_mut().push(format!("wait {f}")));
        }
        fn disc_read(&mut self, _: u32, _: &mut [u8; 2352]) -> bool {
            false
        }
        fn has_disc(&mut self) -> bool {
            false
        }
        fn pad_state(&mut self, _: u32) -> Vec<u8> {
            vec![]
        }
        fn storage_read(&mut self, _: &str) -> Option<Vec<u8>> {
            None
        }
        fn storage_write(&mut self, _: &str, _: &[u8]) -> bool {
            false
        }
        fn report(&mut self, _: &str) {}
    }

    #[test]
    fn vblank_waits_are_held_while_js_is_on_the_stack() {
        let seen = std::rc::Rc::new(RefCell::new(Vec::new()));
        let hold = std::rc::Rc::new(crate::hold::Hold::new());
        let host = Paced { seen: seen.clone(), hold: hold.clone() };
        let rt = RefCell::new(Machine::new(Box::new(VecMem::default()), Box::new(host), Box::new(Regs(0x801F_F000)), Default::default(), Psyq::default()));
        let mut g = Guest { rt: &rt, rfe: vec![], seen: vec![] };
        let vblank = psx_devices::clock::CPU_HZ / 60;
        let run_vblanks = |g: &mut Guest, n: u64| {
            let until = rt.borrow().vblanks + n;
            while rt.borrow().vblanks < until {
                safepoint(&rt, g, vblank / 8);
            }
        };
        run_vblanks(&mut g, 1);
        assert_eq!(*seen.borrow(), ["vblank 1", "wait 1"]);
        // A hook's JS calls back into the game, which reaches two vblanks.
        hold.enter();
        run_vblanks(&mut g, 2);
        assert_eq!(*seen.borrow(), ["vblank 1", "wait 1", "vblank 2", "vblank 3"]);
        let wait = |f| seen.borrow_mut().push(format!("wait {f}"));
        hold.leave(wait);
        assert_eq!(*seen.borrow(), ["vblank 1", "wait 1", "vblank 2", "vblank 3", "wait 2", "wait 3"]);
        assert_eq!(hold.total(), 2);
        run_vblanks(&mut g, 1);
        assert_eq!(seen.borrow().last().map(String::as_str), Some("wait 4"));
    }

    #[test]
    fn host_calls_run_unborrowed_after_callbacks() {
        struct HostCall<'a>(&'a RefCell<Machine<Psyq>>, usize);
        impl Invoke for HostCall<'_> {
            fn invoke(&mut self, _: GuestCall) -> u64 {
                assert_eq!(self.1, 0);
                self.0.borrow_mut().count("callback");
                0
            }
            fn host_calls(&mut self) {
                let mut m = self.0.borrow_mut();
                assert!(!m.in_irq && !m.crit);
                assert!(m.calls.is_empty());
                assert_eq!(m.counts["callback"], 1);
                self.1 += 1;
                m.mem.w32(0x8000_1000, 42);
            }
        }
        let rt = machine();
        rt.borrow_mut().calls.push_back(GuestCall { addr: 0x8001_0000, args: [0; 4] });
        let mut g = HostCall(&rt, 0);
        for (crit, in_irq) in [(true, false), (false, true)] {
            rt.borrow_mut().crit = crit;
            rt.borrow_mut().in_irq = in_irq;
            drain(&rt, &mut g);
            assert_eq!(g.1, 0);
        }
        rt.borrow_mut().in_irq = false;
        drain(&rt, &mut g);
        assert_eq!(g.1, 1);
        assert_eq!(rt.borrow().mem.r32(0x8000_1000), 42);
    }

    #[test]
    fn chains_then_the_custom_exit() {
        let rt = machine();
        element(&rt, 0, 0x8000_1000, 0x8001_0000, 0x8001_0100);
        element(&rt, 2, 0x8000_1010, 0x8001_0200, 0);
        // HookEntryInt's buffer: resume address first (setjmp's layout).
        rt.borrow_mut().mem.w32(0x8000_2000, 0x8003_0000);
        rt.borrow_mut().kernel.custom_exit = 0x8000_2000;
        let mut g = Guest { rt: &rt, rfe: vec![0x8003_0000], seen: vec![] };
        rt.borrow_mut().queue_irq_chains();
        drain(&rt, &mut g);
        assert_eq!(
            g.seen,
            vec![
                (0x8001_0000, [0; 4]),
                (0x8001_0100, [1, 0, 0, 0]),
                (0x8001_0200, [0; 4]),
                (0x8003_0000, [0x8000_2000, 1, 0, 0]),
            ]
        );
        assert!(!rt.borrow().in_irq && !rt.borrow().rfe);
    }

    #[test]
    fn return_from_exception_skips_the_rest() {
        let rt = machine();
        element(&rt, 0, 0x8000_1000, 0x8001_0000, 0x8001_0100);
        element(&rt, 1, 0x8000_1010, 0x8001_0200, 0);
        rt.borrow_mut().mem.w32(0x8000_2000, 0x8003_0000);
        rt.borrow_mut().kernel.custom_exit = 0x8000_2000;
        let mut g = Guest { rt: &rt, rfe: vec![0x8001_0000], seen: vec![] };
        rt.borrow_mut().queue_irq_chains();
        // A second exception queued behind the first: the skip ends at the
        // first one's exit, so the second runs (and returns early too).
        rt.borrow_mut().queue_irq_chains();
        drain(&rt, &mut g);
        let addrs: Vec<u32> = g.seen.iter().map(|c| c.0).collect();
        assert_eq!(addrs, vec![0x8001_0000, 0x8001_0000]);
        assert!(rt.borrow().irq_calls.is_empty());
    }

    #[test]
    fn native_kernel_callbacks_and_patched_chain_functions() {
        use psx_bios::kernel::{self, EV_ENABLED, EV_MODE_INTR};
        use psx_devices::irq::VBLANK;
        const EVENT: u32 = 0x8002_0000;
        const DISABLED: u32 = EVENT + 4;
        const NESTED: u32 = EVENT + 8;
        const HANDLER: u32 = EVENT + 12;
        const VERIFIER: u32 = EVENT + 16;
        const LATER: u32 = EVENT + 20;
        struct Patched<'a> {
            rt: &'a RefCell<Machine<Psyq>>,
            disable: u32,
            seen: Vec<u32>,
        }
        impl Invoke for Patched<'_> {
            fn invoke(&mut self, call: GuestCall) -> u64 {
                let flow = self.rt.borrow_mut().kernel_call(call.addr, call.args);
                if let Some(flow) = flow { return run(self.rt, self, flow); }
                let mut m = self.rt.borrow_mut();
                assert!(m.in_irq);
                if call.addr == psxw_abi::BIOS_RETURN_FROM_EXCEPTION {
                    m.rfe = true;
                    return 0;
                }
                self.seen.push(call.addr);
                assert_ne!(m.io.as_ref().unwrap().irq.stat & (1 << VBLANK), 0, "callback precedes ack");
                match call.addr {
                    EVENT => {
                        m.kernel.clear_rcnt[3] = 0;
                        m.set_ev_status(self.disable, kernel::EV_DISABLED);
                        let f = psx_bios::bios::call(&mut m, 0xB0, 7, [0xF000_0042, 2, 0, 0]).unwrap();
                        drop(m);
                        run(self.rt, self, f)
                    }
                    HANDLER => { assert_eq!(call.args[0], 1); 0 }
                    VERIFIER => 7,
                    NESTED | LATER => 0,
                    _ => panic!("unexpected callback {:#x}", call.addr),
                }
            }
        }
        let rt = machine();
        let e = kernel::kernel_element(3);
        let disable;
        let saved_handler;
        {
            let mut m = rt.borrow_mut();
            m.io = Some(Default::default());
            m.io.as_mut().unwrap().irq.mask = 1 << VBLANK;
            let event = m.open_event(0xF200_0003, 2, EV_MODE_INTR, EVENT);
            disable = m.open_event(0xF200_0003, 2, EV_MODE_INTR, DISABLED);
            let nested = m.open_event(0xF000_0042, 2, EV_MODE_INTR, NESTED);
            for h in [event, disable, nested] { m.set_ev_status(h, EV_ENABLED); }
            saved_handler = m.mem.r32(e + 4);
            m.mem.w32(e + 4, HANDLER);
        }
        element(&rt, 2, 0x8000_1000, LATER, 0);
        let mut guest = Patched { rt: &rt, disable, seen: vec![] };
        rt.borrow_mut().raise_irq(VBLANK);
        drain(&rt, &mut guest);
        assert_eq!(guest.seen, [EVENT, NESTED, HANDLER, LATER]);
        assert!(rt.borrow().calls.is_empty(), "nested event callbacks were not deferred");
        // Patch only the verifier; the saved native handler is still callable
        // and returns from the exception, skipping the lower-priority element.
        {
            let mut m = rt.borrow_mut();
            m.mem.w32(e + 8, VERIFIER);
            m.mem.w32(e + 4, saved_handler);
            m.kernel.clear_rcnt[3] = 1;
            m.queue_irq_chains();
        }
        drain(&rt, &mut guest);
        assert_eq!(guest.seen, [EVENT, NESTED, HANDLER, LATER, VERIFIER]);
        assert_eq!(rt.borrow().io.as_ref().unwrap().irq.stat & (1 << VBLANK), 0);
        assert!(!rt.borrow().in_irq && !rt.borrow().rfe);
    }

    #[test]
    #[should_panic(expected = "returned instead of calling ReturnFromException")]
    fn custom_exit_must_not_return() {
        let rt = machine();
        rt.borrow_mut().mem.w32(0x8000_2000, 0x8003_0000);
        rt.borrow_mut().kernel.custom_exit = 0x8000_2000;
        let mut g = Guest { rt: &rt, rfe: vec![], seen: vec![] };
        rt.borrow_mut().queue_irq_chains();
        drain(&rt, &mut g);
    }
}
