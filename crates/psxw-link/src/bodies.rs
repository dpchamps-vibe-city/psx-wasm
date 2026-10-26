//! Function bodies both link modes synthesize.

use crate::link::Division;
use psxw_abi::{WINDOW_BASE, WINDOW_MASK};
use wasm_encoder::{BlockType, Function, Instruction as I, ValType};

/// abs/labs `(i32) -> i32` or fabs `(f64) -> f64` (D29).
pub fn support(name: &str) -> Function {
    let mut f = Function::new([]);
    if name == "fabs" {
        f.instruction(&I::LocalGet(0));
        f.instruction(&I::F64Abs);
    } else {
        // (x ^ (x >> 31)) - (x >> 31)
        f.instruction(&I::LocalGet(0));
        f.instruction(&I::LocalGet(0));
        f.instruction(&I::I32Const(31));
        f.instruction(&I::I32ShrS);
        f.instruction(&I::I32Xor);
        f.instruction(&I::LocalGet(0));
        f.instruction(&I::I32Const(31));
        f.instruction(&I::I32ShrS);
        f.instruction(&I::I32Sub);
    }
    f.instruction(&I::End);
    f
}

/// `(n, d) -> i32` with the R3000's results and GCC's `break` checks
/// (`brk` is `(code) -> ()`). GCC checks a variable divisor: `break` on
/// zero, and with GNU as also on INT_MIN / -1; the R3000 then yields
/// lo = n >= 0 ? -1 : 1 (signed) or ~0, hi = n.
pub fn division(op: &str, conv: Division, brk: u32) -> Function {
    let signed = op.ends_with("_s");
    let rem = op.starts_with("rem");
    let mut f = Function::new([]);
    let block = BlockType::Empty;
    f.instruction(&I::LocalGet(1));
    f.instruction(&I::I32Eqz);
    f.instruction(&I::If(block));
    f.instruction(&I::I32Const(conv.zero as i32));
    f.instruction(&I::Call(brk));
    if rem {
        f.instruction(&I::LocalGet(0));
    } else if signed {
        f.instruction(&I::I32Const(1));
        f.instruction(&I::I32Const(-1));
        f.instruction(&I::LocalGet(0));
        f.instruction(&I::I32Const(0));
        f.instruction(&I::I32LtS);
        f.instruction(&I::Select);
    } else {
        f.instruction(&I::I32Const(-1));
    }
    f.instruction(&I::Return);
    f.instruction(&I::End);
    if signed {
        // n == INT_MIN && d == -1: lo = INT_MIN, hi = 0.
        f.instruction(&I::LocalGet(0));
        f.instruction(&I::I32Const(i32::MIN));
        f.instruction(&I::I32Eq);
        f.instruction(&I::LocalGet(1));
        f.instruction(&I::I32Const(-1));
        f.instruction(&I::I32Eq);
        f.instruction(&I::I32And);
        f.instruction(&I::If(block));
        if let Some(code) = conv.overflow {
            f.instruction(&I::I32Const(code as i32));
            f.instruction(&I::Call(brk));
        }
        f.instruction(&I::I32Const(if rem { 0 } else { i32::MIN }));
        f.instruction(&I::Return);
        f.instruction(&I::End);
    }
    f.instruction(&I::LocalGet(0));
    f.instruction(&I::LocalGet(1));
    f.instruction(match op {
        "div_s" => &I::I32DivS,
        "div_u" => &I::I32DivU,
        "rem_s" => &I::I32RemS,
        _ => &I::I32RemU,
    });
    f.instruction(&I::End);
    f
}

/// `memcpy(dst, src, n) -> dst` as a window copy; `mark` is the harness's
/// `(wasm dst, n)` dirty-marking helper.
pub fn inline_copy(mark: Option<u32>) -> Function {
    let mut f = Function::new([(1, ValType::I32)]);
    for l in [0, 1] {
        f.instruction(&I::LocalGet(l));
        f.instruction(&I::I32Const(WINDOW_MASK as i32));
        f.instruction(&I::I32And);
        f.instruction(&I::I32Const(WINDOW_BASE as i32));
        f.instruction(&I::I32Add);
        if let (0, Some(m)) = (l, mark) {
            f.instruction(&I::LocalTee(3));
            f.instruction(&I::LocalGet(2));
            f.instruction(&I::Call(m));
            f.instruction(&I::LocalGet(3));
        }
    }
    f.instruction(&I::LocalGet(2));
    f.instruction(&I::MemoryCopy { src_mem: 0, dst_mem: 0 });
    f.instruction(&I::LocalGet(0));
    f.instruction(&I::End);
    f
}
