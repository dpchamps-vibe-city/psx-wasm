//! Holding the host's vblank wait while JS is on the stack (D54).
//!
//! JSPI can't suspend through a JS frame, and `host.wait_vblank` may
//! suspend. A script mod's shim brackets each call into JS with
//! `psxw_js_enter`/`psxw_js_leave`; while any is open, the runtime holds
//! the waits, and when the last one closes it makes them, in order, with
//! only wasm frames left on the stack. Everything the guest can see
//! happens in `host.vblank`, which is never held.

use std::cell::{Cell, RefCell};

#[derive(Debug, Default)]
pub struct Hold {
    depth: Cell<u32>,
    held: RefCell<Vec<u64>>,
    /// Waits held over the run (the run report's `held_waits`).
    total: Cell<u64>,
}

impl Hold {
    pub const fn new() -> Self {
        Self { depth: Cell::new(0), held: RefCell::new(Vec::new()), total: Cell::new(0) }
    }

    /// The host's wait after vblank `frame`: made now, or held.
    pub fn wait(&self, frame: u64, wait: impl FnOnce(u64)) {
        if self.depth.get() > 0 {
            self.held.borrow_mut().push(frame);
            self.total.set(self.total.get() + 1);
        } else {
            wait(frame);
        }
    }

    pub fn enter(&self) {
        self.depth.set(self.depth.get() + 1);
    }

    /// One call into JS has returned; after the last, the held waits.
    pub fn leave(&self, mut wait: impl FnMut(u64)) {
        let d = self.depth.get().saturating_sub(1);
        self.depth.set(d);
        if d == 0 {
            let held = std::mem::take(&mut *self.held.borrow_mut());
            for f in held {
                wait(f);
            }
        }
    }

    pub fn depth(&self) -> u32 {
        self.depth.get()
    }

    pub fn total(&self) -> u64 {
        self.total.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_are_held_while_js_is_on_the_stack() {
        let h = Hold::new();
        let made = RefCell::new(Vec::new());
        let wait = |f| made.borrow_mut().push(f);
        h.wait(1, wait);
        assert_eq!(*made.borrow(), [1]);
        h.enter();
        h.wait(2, wait);
        h.enter();
        h.wait(3, wait);
        h.leave(wait);
        // Still inside the outer call: nothing made.
        h.wait(4, wait);
        assert_eq!(*made.borrow(), [1]);
        h.leave(wait);
        assert_eq!(*made.borrow(), [1, 2, 3, 4]);
        assert_eq!(h.total(), 3);
        assert_eq!(h.depth(), 0);
        h.wait(5, wait);
        assert_eq!(*made.borrow(), [1, 2, 3, 4, 5]);
    }

    #[test]
    fn an_unbalanced_leave_makes_nothing_twice() {
        let h = Hold::new();
        let made = RefCell::new(Vec::new());
        h.enter();
        h.wait(7, |f| made.borrow_mut().push(f));
        h.leave(|f| made.borrow_mut().push(f));
        h.leave(|f| made.borrow_mut().push(f));
        assert_eq!(*made.borrow(), [7]);
    }
}
