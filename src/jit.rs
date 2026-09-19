//! Executable code buffers: mmap RW, write, clean the data cache / invalidate the instruction cache
//! to the point of unification, then `mprotect` to R+X (never W+X at the same time).
//!
//! Why run-time generation rather than `build.rs`: the experiments sweep parameters over wide ranges
//! (number of branches, spacing, history distance, call depth, filler count) that would otherwise
//! need thousands of pre-built variants.

use anyhow::{bail, Result};
use core::arch::asm;

const PAGE: usize = 16 << 10;

fn ctr_el0() -> u64 {
    let v: u64;
    // SAFETY: CTR_EL0 is readable at EL0 under Linux.
    unsafe { asm!("mrs {v}, ctr_el0", v = out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// Smallest data / instruction cache line sizes in bytes, from CTR_EL0 (DminLine / IminLine).
pub fn cache_line_sizes() -> (usize, usize) {
    let c = ctr_el0();
    (4usize << ((c >> 16) & 0xF), 4usize << (c & 0xF))
}

/// Make freshly written instructions in `[start, start + len)` visible to instruction fetch.
///
/// # Safety
/// The range must be mapped.
unsafe fn sync_icache(start: usize, len: usize) {
    let (dl, il) = cache_line_sizes();
    let end = start + len;
    let mut a = start & !(dl - 1);
    while a < end {
        asm!("dc cvau, {a}", a = in(reg) a, options(nostack, preserves_flags));
        a += dl;
    }
    asm!("dsb ish", options(nostack, preserves_flags));
    let mut a = start & !(il - 1);
    while a < end {
        asm!("ic ivau, {a}", a = in(reg) a, options(nostack, preserves_flags));
        a += il;
    }
    asm!("dsb ish", "isb", options(nostack, preserves_flags));
}

/// A finished, read+execute code region.
pub struct JitCode {
    ptr: *mut u8,
    mapped: usize,
    pub words: usize,
}

/// Entry-point signature shared by all generated functions: `(iterations, data pointer) -> value`.
pub type JitFn = unsafe extern "C" fn(u64, *const u8) -> u64;

impl JitCode {
    pub fn new(words: &[u32]) -> Result<JitCode> {
        let bytes = words.len() * 4;
        let mapped = bytes.div_ceil(PAGE).max(1) * PAGE;
        // SAFETY: anonymous private mapping; result checked.
        let ptr = unsafe {
            libc::mmap(std::ptr::null_mut(), mapped, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0)
        };
        if ptr == libc::MAP_FAILED {
            bail!("mmap {mapped} bytes for code: {}", std::io::Error::last_os_error());
        }
        let ptr = ptr as *mut u8;
        // SAFETY: the mapping is `mapped >= bytes` bytes and writable; cache maintenance covers exactly what we wrote.
        unsafe {
            std::ptr::copy_nonoverlapping(words.as_ptr() as *const u8, ptr, bytes);
            sync_icache(ptr as usize, bytes);
            if libc::mprotect(ptr as *mut libc::c_void, mapped, libc::PROT_READ | libc::PROT_EXEC) != 0 {
                let e = std::io::Error::last_os_error();
                libc::munmap(ptr as *mut libc::c_void, mapped);
                bail!("mprotect R+X: {e}");
            }
        }
        Ok(JitCode { ptr, mapped, words: words.len() })
    }

    pub fn entry(&self) -> JitFn {
        // SAFETY: the region is executable and starts with a function following the C ABI
        // (callee-saved registers preserved, ret to x30).
        unsafe { std::mem::transmute::<*mut u8, JitFn>(self.ptr) }
    }

    pub fn base(&self) -> usize {
        self.ptr as usize
    }

    pub fn call(&self, iters: u64, data: *const u8) -> u64 {
        // SAFETY: see `entry`; callers pass data valid for the generated code.
        unsafe { (self.entry())(iters, data) }
    }
}

impl Drop for JitCode {
    fn drop(&mut self) {
        // SAFETY: ptr/mapped are what mmap returned.
        unsafe { libc::munmap(self.ptr as *mut libc::c_void, self.mapped) };
    }
}
