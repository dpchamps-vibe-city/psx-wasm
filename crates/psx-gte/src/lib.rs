//! Geometry Transformation Engine as a fixed-point math library with the
//! hardware's register-file API: saturating IR writes, the SXY/SZ FIFOs,
//! LZCS/LZCR, IRGB/ORGB conversion and an exact FLAG register.
//!
//! One implementation serves three callers: the `gte_*` macro shims in C,
//! lifted COP2 instructions, and Rust libgte replacements. The register file
//! lives in the guest window's runtime-private page so all three agree.
//! Conformance: Amidog's `psxtest_gte`.

#![no_std]

/// 32 data + 32 control registers, laid out as the hardware numbers them.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct GteRegs {
    pub data: [u32; 32],
    pub ctrl: [u32; 32],
}
