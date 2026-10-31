//! The kernel's C library (A0h; psx-spx "BIOS String Functions", "BIOS
//! Number/String/Character Conversion", "BIOS Memory Allocation", "BIOS
//! Misc Functions"): Sony's behaviour, its documented bugs included. The
//! functions that take a callback call the guest through [`Flow::Call`].

use crate::bios::MAX_LEN;
use crate::machine::{Flow, GuestCall, Machine, Sdk};
use psx_devices::mem::Mem;

/// Where `strtok` copies its string: a 100h-byte buffer in the kernel's
/// variables (psx-spx); the address is ours.
pub const STRTOK_BUF: u32 = 0x8000_7100;

/// `todigit`: 0-9 for digits, 10-35 for letters, 9999999 for anything
/// else (psx-spx; for 80h-FFh, "garbage", 9999999 here too).
pub fn todigit(c: u32) -> u32 {
    match c as u8 {
        b @ b'0'..=b'9' => (b - b'0') as u32,
        b @ (b'A'..=b'Z' | b'a'..=b'z') => (b.to_ascii_lowercase() - b'a') as u32 + 10,
        _ => 9_999_999,
    }
}

/// How `strtol`, `strtoul` and `atoi` read the number at `src`: blanks
/// (09h-0Dh, 20h); a `-` when `minus`; a prefix that overrides `base`
/// whatever it is (psx-spx's bug): "0x", "0b", and "o" for octal, or for
/// `atoi` a leading "0"; then digits below the base, without overflow
/// checks. A base outside 2-36 is 10. The value and where reading stopped.
pub fn strtol(mem: &dyn Mem, src: u32, base: u32, minus: bool, atoi: bool) -> (u32, u32) {
    let at = |p: u32| mem.r8(p);
    let mut p = src;
    while matches!(at(p), 0x09..=0x0D | b' ') && p.wrapping_sub(src) < MAX_LEN {
        p = p.wrapping_add(1);
    }
    let neg = minus && at(p) == b'-';
    p = p.wrapping_add(neg as u32);
    let mut base = if (2..=36).contains(&base) { base } else { 10 };
    let prefix = match (at(p).to_ascii_lowercase(), at(p.wrapping_add(1)).to_ascii_lowercase()) {
        (b'0', b'x') => Some((16, 2)),
        (b'0', b'b') => Some((2, 2)),
        (b'0', _) if atoi => Some((8, 1)),
        (b'o', _) if !atoi => Some((8, 1)),
        _ => None,
    };
    if let Some((b, n)) = prefix {
        base = b;
        p = p.wrapping_add(n);
    }
    let mut v = 0u32;
    while p.wrapping_sub(src) < MAX_LEN {
        let d = todigit(at(p) as u32);
        if d >= base {
            break;
        }
        v = v.wrapping_mul(base).wrapping_add(d);
        p = p.wrapping_add(1);
    }
    (if neg { v.wrapping_neg() } else { v }, p)
}

/// `strpbrk`: the first character of `src` that is in `list`; psx-spx's
/// bug: with none, `src` itself unless `src` is empty.
pub fn strpbrk(mem: &dyn Mem, src: u32, list: u32) -> u32 {
    let (s, l) = (mem.cstr(src, MAX_LEN as usize), mem.cstr(list, MAX_LEN as usize));
    match s.iter().position(|c| l.contains(c)) {
        Some(i) => src + i as u32,
        None if s.is_empty() => 0,
        None => src,
    }
}

/// `strspn` (`member`) and `strcspn`: the index of the first character of
/// `src` that is (`strspn`) or isn't (`strcspn`) in `list`, else `src`'s
/// length. psx-spx has them so, the reverse of C's.
pub fn span(mem: &dyn Mem, src: u32, list: u32, member: bool) -> u32 {
    let (s, l) = (mem.cstr(src, MAX_LEN as usize), mem.cstr(list, MAX_LEN as usize));
    s.iter().position(|c| l.contains(c) == member).unwrap_or(s.len()) as u32
}

/// `strtok(src, list)`: a first call (`src` nonzero) copies the string to
/// [`STRTOK_BUF`]. Each call cuts the fragment there at its first
/// separator and returns it, or 0 once the string is used up. psx-spx's
/// bug: the separators after the cut are skipped only while each equals
/// every character of `list`, so not at all when `list` has two different
/// characters.
pub fn strtok<S: Sdk>(m: &mut Machine<S>, src: u32, list: u32) -> u32 {
    if src != 0 {
        let mut s = m.mem.cstr(src, MAX_LEN as usize);
        s.push(0);
        m.mem.write(STRTOK_BUF, &s);
        m.kernel.strtok = STRTOK_BUF;
    }
    let cur = m.kernel.strtok;
    if cur == 0 || m.mem.r8(cur) == 0 {
        return 0;
    }
    let list = m.mem.cstr(list, MAX_LEN as usize);
    let s = m.mem.cstr(cur, MAX_LEN as usize);
    m.kernel.strtok = match s.iter().position(|c| list.contains(c)) {
        Some(i) => {
            m.mem.w8(cur + i as u32, 0);
            let mut p = cur + i as u32 + 1;
            while m.mem.r8(p) != 0 && list.iter().all(|&c| c == m.mem.r8(p)) {
                p += 1;
            }
            p
        }
        None => cur + s.len() as u32,
    };
    cur
}

/// `strstr`, with psx-spx's bug: after a partial match the search goes on
/// after the mismatching character, not one past where the match began
/// (so "aab" isn't found in "aaab").
pub fn strstr(mem: &dyn Mem, s: u32, sub: u32) -> u32 {
    let sub = mem.cstr(sub, MAX_LEN as usize);
    let mut p = s;
    while mem.r8(p) != 0 && p.wrapping_sub(s) < MAX_LEN {
        let mut q = p;
        let mut found = true;
        for &c in &sub {
            let b = mem.r8(q);
            q = q.wrapping_add(1);
            if b != c {
                if b == 0 {
                    return 0;
                }
                found = false;
                break;
            }
        }
        if found {
            return p;
        }
        p = q;
    }
    0
}

/// Copy `n` bytes forward a byte at a time, as the BIOS's copies run: a
/// destination overlapping above the source repeats its start.
pub fn copy(mem: &mut dyn Mem, dst: u32, src: u32, n: u32) {
    if dst <= src || dst >= src.wrapping_add(n) {
        let b = mem.bytes(src, n as usize);
        mem.write(dst, &b);
        return;
    }
    for i in 0..n {
        let b = mem.r8(src + i);
        mem.w8(dst + i, b);
    }
}

// --- The heap ----------------------------------------------------------------

/// Set up a heap over `size` bytes at `addr` (`InitHeap`, and the kernel's
/// own): one free block. A block is a header word, the size of its data
/// with bit 0 set while it is free (psx-spx's `free` sets [buf-4] bit 0),
/// then the data; blocks are words. Returns the heap, (base, end), or
/// (0, 0) when there is no room for a block.
pub fn heap_init(mem: &mut dyn Mem, addr: u32, size: u32) -> (u32, u32) {
    let base = addr.wrapping_add(3) & !3;
    let end = addr.saturating_add(size) & !3;
    if end < base || end - base < 4 {
        return (0, 0);
    }
    mem.w32(base, (end - base - 4) | 1);
    (base, end)
}

/// `malloc`: the first free block that fits, with the free blocks after it
/// merged into it as the search passes them, split when the rest can hold
/// a header. 0 when nothing fits or there is no heap. The search order is
/// ours; psx-spx gives only the block's alignment and rounding (4 bytes).
pub fn malloc(mem: &mut dyn Mem, (base, end): (u32, u32), size: u32) -> u32 {
    let Some(n) = size.checked_add(3).map(|s| s & !3) else { return 0 };
    // The block after the one at `p` with `len` bytes of data, if it fits.
    let next = |p: u32, len: u32| p.checked_add(4).and_then(|q| q.checked_add(len)).filter(|&q| q <= end);
    let mut p = base;
    while end.saturating_sub(p) >= 4 {
        let h = mem.r32(p);
        let mut len = h & !3;
        if h & 1 != 0 {
            while let Some(q) = next(p, len).filter(|&q| end - q >= 4) {
                let hq = mem.r32(q);
                if hq & 1 == 0 || next(q, hq & !3).is_none() {
                    break;
                }
                len += 4 + (hq & !3);
            }
            if len >= n {
                if len - n >= 4 {
                    mem.w32(p, n);
                    mem.w32(p + 4 + n, (len - n - 4) | 1);
                } else {
                    mem.w32(p, len);
                }
                return p + 4;
            }
            mem.w32(p, len | 1);
        }
        match next(p, len) {
            Some(q) => p = q,
            None => break,
        }
    }
    0
}

/// `free`: [buf-4] bit 0 set, nothing checked (psx-spx). `buf` 0 is a bus
/// error on the console (a write to FFFFFFFCh): an error here.
pub fn free(mem: &mut dyn Mem, buf: u32) -> Result<(), String> {
    if buf == 0 {
        return Err("free(0) writes FFFFFFFCh, a bus error".into());
    }
    let h = buf.wrapping_sub(4);
    let v = mem.r32(h);
    mem.w32(h, v | 1);
    Ok(())
}

/// `calloc(x, y)`: `malloc(x*y)`, zero-filled; the address, or 0.
pub fn calloc(mem: &mut dyn Mem, heap: (u32, u32), x: u32, y: u32) -> u32 {
    let n = x.wrapping_mul(y);
    let buf = malloc(mem, heap, n);
    if buf != 0 && (n as i32) > 0 {
        mem.write(buf, &vec![0; n as usize]);
    }
    buf
}

/// `realloc(old, size)`: `malloc(size)` with `old` 0; `free(old)` with
/// `size` 0 (0 here; psx-spx: garbage); else `malloc(size)`, then `size`
/// bytes copied from `old` whatever its size (psx-spx's bug), then
/// `free(old)`. When the `malloc` fails, 0 and `old` kept.
pub fn realloc(mem: &mut dyn Mem, heap: (u32, u32), old: u32, size: u32) -> Result<u32, String> {
    if old == 0 {
        return Ok(malloc(mem, heap, size));
    }
    if size == 0 {
        free(mem, old)?;
        return Ok(0);
    }
    let buf = malloc(mem, heap, size);
    if buf != 0 {
        if (size as i32) > 0 {
            copy(mem, buf, old, size);
        }
        free(mem, old)?;
    }
    Ok(buf)
}

// --- Sorting and searching -----------------------------------------------------

/// An array and the guest's comparison function.
#[derive(Clone, Copy)]
struct Array {
    base: u32,
    width: u32,
    cmp: u32,
}

impl Array {
    fn at(&self, i: u32) -> u32 {
        self.base.wrapping_add(i.wrapping_mul(self.width))
    }

    /// Call `cmp(a, b)`, then go on with its result.
    fn compare<S: Sdk>(&self, a: u32, b: u32, then: impl FnOnce(&mut Machine<S>, i32) -> Flow<S> + 'static) -> Flow<S> {
        Flow::Call { call: GuestCall { addr: self.cmp, args: [a, b, 0, 0] }, then: Box::new(move |m, v| then(m, v as u32 as i32)) }
    }
}

/// `qsort(base, nel, width, callback)`: a quicksort, calling the guest's
/// `callback(a, b)`. It swaps elements through a `width`-byte block from
/// the heap (psx-spx), so with no room there nothing is sorted. The
/// partitioning (Lomuto's, last element as pivot) is ours: psx-spx doesn't
/// give Sony's.
pub fn qsort<S: Sdk>(m: &mut Machine<S>, a: [u32; 4]) -> Flow<S> {
    let (nel, width) = (a[1], a[2]);
    if (nel as i32) < 2 || (width as i32) <= 0 {
        return Flow::ret(0);
    }
    let tmp = malloc(&mut *m.mem, m.kernel.heap, width);
    if tmp == 0 {
        return Flow::ret(0);
    }
    sort(m, Sort { arr: Array { base: a[0], width, cmp: a[3] }, tmp, ranges: vec![(0, nel - 1)], part: None })
}

/// [`qsort`]'s state: the ranges left to sort, and the partition under way
/// (its range, the next place for a smaller element, the element next
/// compared with the pivot).
struct Sort {
    arr: Array,
    tmp: u32,
    ranges: Vec<(u32, u32)>,
    part: Option<(u32, u32, u32, u32)>,
}

impl Sort {
    fn swap(&self, mem: &mut dyn Mem, i: u32, j: u32) {
        if i != j {
            let (a, b, w) = (self.arr.at(i), self.arr.at(j), self.arr.width);
            copy(mem, self.tmp, a, w);
            copy(mem, a, b, w);
            copy(mem, b, self.tmp, w);
        }
    }
}

fn sort<S: Sdk>(m: &mut Machine<S>, mut s: Sort) -> Flow<S> {
    loop {
        if let Some((lo, hi, i, j)) = s.part {
            if j < hi {
                let arr = s.arr;
                return arr.compare(arr.at(j), arr.at(hi), move |m: &mut Machine<S>, c| {
                    let i = if c < 0 {
                        s.swap(&mut *m.mem, i, j);
                        i + 1
                    } else {
                        i
                    };
                    s.part = Some((lo, hi, i, j + 1));
                    sort(m, s)
                });
            }
            s.swap(&mut *m.mem, i, hi);
            if i > lo + 1 {
                s.ranges.push((lo, i - 1));
            }
            if i + 1 < hi {
                s.ranges.push((i + 1, hi));
            }
            s.part = None;
            continue;
        }
        match s.ranges.pop() {
            Some((lo, hi)) => s.part = Some((lo, hi, lo, lo)),
            None => {
                let _ = free(&mut *m.mem, s.tmp);
                return Flow::ret(0);
            }
        }
    }
}

/// `lsearch(key, base, nel, width, callback)`: each element in turn,
/// `callback(key, element)`; the first that compares equal, or 0.
pub fn lsearch<S: Sdk>(m: &Machine<S>, a: [u32; 4]) -> Flow<S> {
    let arr = Array { base: a[1], width: a[3], cmp: m.arg(&a, 4) };
    linear(arr, a[0], 0, (a[2] as i32).max(0) as u32)
}

fn linear<S: Sdk>(arr: Array, key: u32, i: u32, n: u32) -> Flow<S> {
    if i >= n {
        return Flow::ret(0);
    }
    let at = arr.at(i);
    arr.compare(key, at, move |_: &mut Machine<S>, c| if c == 0 { Flow::ret(at) } else { linear(arr, key, i + 1, n) })
}

/// `bsearch(key, base, nel, width, callback)`: a binary search of a sorted
/// array, `callback(key, element)`; the element that compares equal, or 0.
pub fn bsearch<S: Sdk>(m: &Machine<S>, a: [u32; 4]) -> Flow<S> {
    let arr = Array { base: a[1], width: a[3], cmp: m.arg(&a, 4) };
    binary(arr, a[0], 0, (a[2] as i32).max(0) as u32)
}

fn binary<S: Sdk>(arr: Array, key: u32, lo: u32, hi: u32) -> Flow<S> {
    if lo >= hi {
        return Flow::ret(0);
    }
    let mid = lo + (hi - lo) / 2;
    let at = arr.at(mid);
    arr.compare(key, at, move |_: &mut Machine<S>, c| match c {
        0 => Flow::ret(at),
        c if c < 0 => binary(arr, key, lo, mid),
        _ => binary(arr, key, mid + 1, hi),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{NoSdk, machine};

    /// Run `f` as the driver does, the guest's comparison being a signed
    /// compare of the words its arguments point at.
    fn run(m: &mut Machine<NoSdk>, mut f: Flow<NoSdk>) -> (u32, u32) {
        let mut calls = 0;
        loop {
            match f {
                Flow::Return(v) => return (v as u32, calls),
                Flow::Call { call, then } => {
                    calls += 1;
                    let (x, y) = (m.mem.r32(call.args[0]) as i32, m.mem.r32(call.args[1]) as i32);
                    f = then(m, (x - y) as u32 as u64);
                }
                Flow::Block { .. } => panic!("blocked"),
            }
        }
    }

    #[test]
    fn sort_and_search_call_the_guest() {
        let mut m = machine();
        let (base, key, cmp) = (0x8001_0000, 0x8001_0100, 0x8002_0000);
        let words = [5i32, -3, 9, 0, 5, 12, -7, 1];
        for (i, w) in words.iter().enumerate() {
            m.mem.w32(base + 4 * i as u32, *w as u32);
        }
        // No heap: nothing sorted.
        let f = qsort(&mut m, [base, 8, 4, cmp]);
        assert_eq!(run(&mut m, f), (0, 0));
        m.kernel.heap = heap_init(&mut *m.mem, 0x8003_0000, 0x100);
        let f = qsort(&mut m, [base, 8, 4, cmp]);
        run(&mut m, f);
        let got: Vec<i32> = (0..8).map(|i| m.mem.r32(base + 4 * i) as i32).collect();
        assert_eq!(got, [-7, -3, 0, 1, 5, 5, 9, 12]);
        // The swap block went back to the heap.
        assert_eq!(m.mem.r32(0x8003_0000), 4 | 1);
        // lsearch and bsearch take the callback from the stack.
        let sp = m.regs.sp();
        m.mem.w32(sp + 0x10, cmp);
        let find = |m: &mut Machine<NoSdk>, k: i32, f: fn(&Machine<NoSdk>, [u32; 4]) -> Flow<NoSdk>| {
            m.mem.w32(key, k as u32);
            let flow = f(m, [key, base, 8, 4]);
            run(m, flow)
        };
        assert_eq!(find(&mut m, 9, lsearch), (base + 24, 7));
        assert_eq!(find(&mut m, 9, bsearch).0, base + 24);
        assert_eq!(find(&mut m, 2, lsearch), (0, 8));
        assert_eq!(find(&mut m, 2, bsearch).0, 0);
    }

    #[test]
    fn heap_blocks() {
        let mut m = machine();
        let heap = heap_init(&mut *m.mem, 0x8001_0002, 0x42);
        assert_eq!(heap, (0x8001_0004, 0x8001_0044));
        let a = malloc(&mut *m.mem, heap, 5);
        assert_eq!(a, 0x8001_0008);
        assert_eq!(m.mem.r32(a - 4), 8);
        let b = calloc(&mut *m.mem, heap, 4, 4);
        assert_eq!(b, a + 12);
        assert_eq!(malloc(&mut *m.mem, heap, 0x30), 0, "too big");
        free(&mut *m.mem, a).unwrap();
        assert_eq!(m.mem.r32(a - 4), 9);
        // realloc copies the new size from the old block, then frees it.
        m.mem.write(b, b"0123456789ABCDEF");
        let c = realloc(&mut *m.mem, heap, b, 20).unwrap();
        assert_eq!(c, b + 20);
        assert_eq!(m.mem.bytes(c, 16), b"0123456789ABCDEF");
        assert_eq!(m.mem.r32(b - 4) & 1, 1);
        // The freed blocks merge for the next search.
        assert_eq!(malloc(&mut *m.mem, heap, 0x18), a);
        assert!(free(&mut *m.mem, 0).is_err());
    }
}
