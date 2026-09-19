//! Critical loops written in inline asm so the compiler cannot reshape them.
//! Each kernel documents its exact instruction count per iteration; the disassembly is archived
//! in docs/asm/.

use core::arch::asm;

/// Instructions retired per iteration of [`alu_indep`]: 8 `add` + `subs` + `b.ne`.
pub const ALU_INDEP_INSTR: u64 = 10;
/// Instructions retired per iteration of [`alu_dep`]: 10 chained `add` + `subs` + `b.ne`.
pub const ALU_DEP_INSTR: u64 = 12;
/// Instructions per iteration of [`load_lines`]: `ldr` + `subs` + `b.ne`.
pub const LOAD_LINES_INSTR: u64 = 3;

/// Eight independent add chains; `iters` must be > 0.
#[inline(never)]
pub fn alu_indep(iters: u64) {
    // SAFETY: register-only asm, no memory access, no stack use.
    unsafe {
        asm!(
            "2:",
            "add {a0}, {a0}, #1",
            "add {a1}, {a1}, #1",
            "add {a2}, {a2}, #1",
            "add {a3}, {a3}, #1",
            "add {a4}, {a4}, #1",
            "add {a5}, {a5}, #1",
            "add {a6}, {a6}, #1",
            "add {a7}, {a7}, #1",
            "subs {n}, {n}, #1",
            "b.ne 2b",
            n = inout(reg) iters => _,
            a0 = inout(reg) 0u64 => _,
            a1 = inout(reg) 0u64 => _,
            a2 = inout(reg) 0u64 => _,
            a3 = inout(reg) 0u64 => _,
            a4 = inout(reg) 0u64 => _,
            a5 = inout(reg) 0u64 => _,
            a6 = inout(reg) 0u64 => _,
            a7 = inout(reg) 0u64 => _,
            options(nomem, nostack)
        );
    }
}

/// One serial add chain of 10 links per iteration; `iters` must be > 0.
#[inline(never)]
pub fn alu_dep(iters: u64) {
    // SAFETY: register-only asm, no memory access, no stack use.
    unsafe {
        asm!(
            "2:",
            "add {a}, {a}, #1",
            "add {a}, {a}, #1",
            "add {a}, {a}, #1",
            "add {a}, {a}, #1",
            "add {a}, {a}, #1",
            "add {a}, {a}, #1",
            "add {a}, {a}, #1",
            "add {a}, {a}, #1",
            "add {a}, {a}, #1",
            "add {a}, {a}, #1",
            "subs {n}, {n}, #1",
            "b.ne 2b",
            n = inout(reg) iters => _,
            a = inout(reg) 0u64 => _,
            options(nomem, nostack)
        );
    }
}

/// One 8-byte load per 64-byte line, `lines` loads; `lines` must be > 0.
///
/// # Safety
/// `p .. p + 64 * lines` must be readable memory.
#[inline(never)]
pub unsafe fn load_lines(p: *const u8, lines: u64) {
    asm!(
        "2:",
        "ldr {t}, [{p}], #64",
        "subs {n}, {n}, #1",
        "b.ne 2b",
        p = inout(reg) p => _,
        n = inout(reg) lines => _,
        t = out(reg) _,
        options(nostack, readonly)
    );
}
