//! The mod library for Rust mods (plans/completed/M11-modkit.md, phase 2): what
//! `psxmod.h` is to C. A mod is a `no_std` `cdylib` for
//! `wasm32-unknown-unknown`, linked with `--emit-relocs` and
//! `--allow-undefined`; psxw places it in the mod area, so its statics,
//! its stack and this crate's heap live there, never in the game's RAM.
//!
//! - [`log`]: a line in the run's log (`mod: …`), as `psxmod_log`.
//! - feature `alloc`: [`Heap`], a bump allocator over a static array (in
//!   the mod's own data), as the `#[global_allocator]` the mod declares.
//!   A mod module can't grow memory: the game's is the only memory.
//! - On wasm32, a panic handler that logs the panic and traps.

#![no_std]

unsafe extern "C" {
    /// The runtime's printf-style log (`psx.mod_log`).
    fn psxmod_log(fmt: *const u8, ...);
}

/// Log `msg` (up to 255 bytes) as one line: `mod: <msg>`.
pub fn log(msg: &str) {
    let mut buf = [0u8; 256];
    let n = msg.len().min(255);
    buf[..n].copy_from_slice(&msg.as_bytes()[..n]);
    // SAFETY: both strings are NUL-terminated.
    unsafe { psxmod_log(c"%s".as_ptr().cast(), buf.as_ptr()) };
}

/// A fixed-size writer for formatting into [`log`] without allocating.
pub struct Line {
    buf: [u8; 255],
    len: usize,
}

impl Line {
    pub const fn new() -> Self {
        Self { buf: [0; 255], len: 0 }
    }

    /// Log what was written.
    pub fn log(&self) {
        log(core::str::from_utf8(&self.buf[..self.len]).unwrap_or("(not UTF-8)"));
    }
}

impl Default for Line {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// `log` with `format!`-style arguments: `psx_mod::logf!("{} calls", n)`.
#[macro_export]
macro_rules! logf {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let mut l = $crate::Line::new();
        let _ = write!(l, $($arg)*);
        l.log();
    }};
}

#[cfg(feature = "alloc")]
mod heap {
    use core::alloc::{GlobalAlloc, Layout};
    use core::cell::UnsafeCell;
    use core::sync::atomic::{AtomicUsize, Ordering};

    /// A bump allocator over `N` bytes of the mod's own data: memory is
    /// never freed (mods allocate at setup). Declare one:
    /// `#[global_allocator] static HEAP: psx_mod::Heap<65536> = psx_mod::Heap::new();`
    pub struct Heap<const N: usize> {
        bytes: UnsafeCell<[u8; N]>,
        next: AtomicUsize,
    }

    // SAFETY: the game runs one thread.
    unsafe impl<const N: usize> Sync for Heap<N> {}

    impl<const N: usize> Heap<N> {
        pub const fn new() -> Self {
            Self { bytes: UnsafeCell::new([0; N]), next: AtomicUsize::new(0) }
        }
    }

    impl<const N: usize> Default for Heap<N> {
        fn default() -> Self {
            Self::new()
        }
    }

    unsafe impl<const N: usize> GlobalAlloc for Heap<N> {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let base = self.bytes.get() as usize;
            let start = (base + self.next.load(Ordering::Relaxed)).next_multiple_of(l.align());
            let end = start + l.size();
            if end > base + N {
                return core::ptr::null_mut();
            }
            self.next.store(end - base, Ordering::Relaxed);
            start as *mut u8
        }

        unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}
    }
}

#[cfg(feature = "alloc")]
pub use heap::Heap;

#[cfg(all(target_arch = "wasm32", not(test)))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    logf!("panic: {}", info);
    core::arch::wasm32::unreachable()
}
