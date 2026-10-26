//! Running BIOS calls and adapters to completion (docs/design.md §6).
//!
//! A call returns a value or a wait; while it waits, virtual time advances
//! event by event, and the callbacks events queue run as guest calls. The
//! machine is borrowed only between guest calls, never across one, so a
//! callback can call back into the runtime.

use psx_bios::{Flow, GuestCall, Machine, Sdk, Wait};
use std::cell::RefCell;

/// Calls into the guest (the dispatch table).
pub trait Invoke {
    fn invoke(&mut self, call: GuestCall) -> u64;
}

/// Run queued callbacks, unless one is already running (no nesting:
/// interrupts are disabled in a handler, as on hardware).
pub fn drain<S: Sdk>(rt: &RefCell<Machine<S>>, inv: &mut dyn Invoke) {
    loop {
        let call = {
            let mut m = rt.borrow_mut();
            if m.in_irq || m.crit {
                return;
            }
            let Some(c) = m.calls.pop_front() else { return };
            m.in_irq = true;
            c
        };
        inv.invoke(call);
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

    /// A guest whose callback re-enters the runtime (VSync(-1)).
    struct Guest<'a> {
        rt: &'a RefCell<Machine<Psyq>>,
        seen: Vec<(u32, u64)>,
    }
    impl Invoke for Guest<'_> {
        fn invoke(&mut self, call: GuestCall) -> u64 {
            let vsync = sdk_psyq::adapters().into_iter().find(|(n, _)| *n == "VSync").unwrap().1;
            let flow = vsync(&mut self.rt.borrow_mut(), [u32::MAX, 0, 0, 0]).unwrap();
            let v = run(self.rt, self, flow);
            self.seen.push((call.addr, v));
            0
        }
    }

    #[test]
    fn callbacks_reenter_during_waits() {
        let m = Machine::new(Box::new(VecMem::default()), Box::new(Host), Box::new(Regs), Default::default(), Psyq::default());
        let rt = RefCell::new(m);
        let adapters = sdk_psyq::adapters();
        let get = |n: &str| adapters.iter().find(|(x, _)| *x == n).unwrap().1;
        let mut g = Guest { rt: &rt, seen: vec![] };
        let f = get("ResetCallback")(&mut rt.borrow_mut(), [0; 4]).unwrap();
        run(&rt, &mut g, f);
        let f = get("VSyncCallback")(&mut rt.borrow_mut(), [0x8001_0000, 0, 0, 0]).unwrap();
        run(&rt, &mut g, f);
        let f = get("VSync")(&mut rt.borrow_mut(), [3, 0, 0, 0]).unwrap();
        run(&rt, &mut g, f);
        assert_eq!(rt.borrow().vblanks, 3);
        assert_eq!(g.seen, vec![(0x8001_0000, 1), (0x8001_0000, 2), (0x8001_0000, 3)]);
    }
}
