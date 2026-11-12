//! The recompiled game. Everything under `gen/` is written by `recomp` and is not checked in:
//!
//!   cargo run -p recomp --release -- disc/files/sb04gc_nm.elf config/hle.txt crates/game/src/gen

#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(not(feature = "std"), feature(core_intrinsics))]
#![cfg_attr(not(feature = "std"), allow(internal_features))]
#![allow(non_snake_case, unused_variables, unused_mut, unused_assignments, unreachable_code, unused_parens, clippy::all)]

pub trait FloatExt {
    fn sqrt(self) -> Self;
}

impl FloatExt for f64 {
    #[inline(always)]
    fn sqrt(self) -> f64 {
        #[cfg(feature = "std")]
        {
            self.sqrt()
        }
        #[cfg(not(feature = "std"))]
        {
            let res: f64;
            unsafe {
                core::arch::asm!("fsqrt {:d}, {:d}", out(vreg) res, in(vreg) self);
            }
            res
        }
    }
}

pub use FloatExt as _;

/// Single-precision fused multiply-add, as the guest's fmadds / ps_madd compute it (one
/// rounding). Operands are guest FPR values; see tools/f32_ops.py.
#[inline(always)]
pub fn fmas(a: f64, b: f64, c: f64) -> f64 {
    let (a, b, c) = (a as f32, b as f32, c as f32);
    #[cfg(feature = "std")]
    {
        a.mul_add(b, c) as f64
    }
    #[cfg(not(feature = "std"))]
    {
        core::intrinsics::fmaf32(a, b, c) as f64
    }
}

mod gen;
pub use gen::*;

