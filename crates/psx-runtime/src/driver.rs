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
}

/// Run queued callbacks, unless one is already running (no nesting:
/// interrupts are disabled in a handler, as on hardware).
pub fn drain<S: Sdk>(rt: &RefCell<Machine<S>>, inv: &mut dyn Invoke) {
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
                    None => return,
                },
            };
            if step.0 == IrqStep::Exit && m.kernel.custom_exit == 0 {
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
}

/// Run a flow to completion.
pub fn run<S: Sdk>(rt: &RefCell<Machine<S>>, inv: &mut dyn Invoke, mut flow: Flow<S>) -> u64 {
    loop {
        match flow {
            Flow::Return(v) => {
                drain(rt, inv);
                return v;
            }
            Flow::Block { until, then } => {
                let start = rt.borrow().clock.now;
                loop {
                    drain(rt, inv);
                    let mut m = rt.borrow_mut();
                    if m.holds(until) {
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
    struct Regs;
    impl psx_bios::Regs for Regs {
        fn sp(&self) -> u32 {
            0x801F_F000
        }
        fn set_sp(&mut self, _: u32) {}
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
            self.seen.push((call.addr, call.args));
            if self.rfe.contains(&call.addr) {
                self.rt.borrow_mut().rfe = true;
            }
            1
        }
    }

    fn machine() -> RefCell<Machine<Psyq>> {
        RefCell::new(Machine::new(Box::new(VecMem::default()), Box::new(Host), Box::new(Regs), Default::default(), Psyq::default()))
    }

    /// A `SysEnqIntRP` element at `at` (verifier, handler) in a chain.
    fn element(rt: &RefCell<Machine<Psyq>>, prio: u32, at: u32, verifier: u32, handler: u32) {
        let mut m = rt.borrow_mut();
        m.mem.w32(at + 4, handler);
        m.mem.w32(at + 8, verifier);
        m.kernel.int_handlers.entry(prio).or_default().push(at);
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
