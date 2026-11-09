//! The recompiled game. Everything under `gen/` is written by `recomp` and is not checked in:
//!
//!   cargo run -p recomp --release -- disc/files/sb04gc_nm.elf config/hle.txt crates/game/src/gen

#![cfg_attr(not(feature = "std"), no_std)]
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

mod gen;
pub use gen::*;

