//! Time sources: the generic-timer virtual counter (CNTVCT_EL0) and the PMU cycle counter.

use core::arch::asm;

/// Virtual counter read, serialised with `isb` so it cannot be hoisted above prior instructions.
#[inline(always)]
pub fn cntvct() -> u64 {
    let v: u64;
    // SAFETY: reading CNTVCT_EL0 is permitted at EL0 (Linux enables it); no memory access.
    unsafe { asm!("isb", "mrs {v}, cntvct_el0", v = out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

pub fn cntfrq() -> u64 {
    let v: u64;
    // SAFETY: CNTFRQ_EL0 is readable at EL0.
    unsafe { asm!("mrs {v}, cntfrq_el0", v = out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

pub fn ticks_to_ns(ticks: u64, freq_hz: u64) -> f64 {
    ticks as f64 * 1e9 / freq_hz as f64
}
