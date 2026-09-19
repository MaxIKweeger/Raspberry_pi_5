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

/// Instructions per iteration of [`chase`]: 16 dependent `ldr` + `subs` + `b.ne`.
pub const CHASE_LOADS_PER_ITER: u64 = 16;

/// Dependent pointer chase: `loads` must be a non-zero multiple of 16. Each node's first 8 bytes
/// hold the address of its successor; the final pointer is returned so the chain can continue.
///
/// # Safety
/// `p` must be a node of a valid chain (every reachable node readable).
#[inline(never)]
pub unsafe fn chase(p: *const u8, loads: u64) -> *const u8 {
    debug_assert!(loads > 0 && loads % CHASE_LOADS_PER_ITER == 0);
    let mut cur = p;
    asm!(
        "2:",
        "ldr {p}, [{p}]", "ldr {p}, [{p}]", "ldr {p}, [{p}]", "ldr {p}, [{p}]",
        "ldr {p}, [{p}]", "ldr {p}, [{p}]", "ldr {p}, [{p}]", "ldr {p}, [{p}]",
        "ldr {p}, [{p}]", "ldr {p}, [{p}]", "ldr {p}, [{p}]", "ldr {p}, [{p}]",
        "ldr {p}, [{p}]", "ldr {p}, [{p}]", "ldr {p}, [{p}]", "ldr {p}, [{p}]",
        "subs {n}, {n}, #1",
        "b.ne 2b",
        p = inout(reg) cur,
        n = inout(reg) loads / CHASE_LOADS_PER_ITER => _,
        options(nostack, readonly)
    );
    cur
}

/// Bytes moved per iteration of the NEON bandwidth kernels (8 x 16-byte registers).
pub const BW_BYTES_PER_ITER: usize = 128;

/// Streaming read with `ldp q`; `bytes` must be a non-zero multiple of 128.
///
/// # Safety
/// `p .. p + bytes` must be readable.
#[inline(never)]
pub unsafe fn bw_read(p: *const u8, bytes: usize) {
    asm!(
        "2:",
        "ldp q0, q1, [{p}]",
        "ldp q2, q3, [{p}, #32]",
        "ldp q4, q5, [{p}, #64]",
        "ldp q6, q7, [{p}, #96]",
        "add {p}, {p}, #128",
        "subs {n}, {n}, #1",
        "b.ne 2b",
        p = inout(reg) p => _,
        n = inout(reg) bytes / BW_BYTES_PER_ITER => _,
        out("v0") _, out("v1") _, out("v2") _, out("v3") _,
        out("v4") _, out("v5") _, out("v6") _, out("v7") _,
        options(nostack, readonly)
    );
}

/// Streaming write with `stp q`; `bytes` must be a non-zero multiple of 128.
///
/// # Safety
/// `p .. p + bytes` must be writable.
#[inline(never)]
pub unsafe fn bw_write(p: *mut u8, bytes: usize) {
    asm!(
        "movi v0.16b, #0x5a",
        "movi v1.16b, #0x5a",
        "2:",
        "stp q0, q1, [{p}]",
        "stp q0, q1, [{p}, #32]",
        "stp q0, q1, [{p}, #64]",
        "stp q0, q1, [{p}, #96]",
        "add {p}, {p}, #128",
        "subs {n}, {n}, #1",
        "b.ne 2b",
        p = inout(reg) p => _,
        n = inout(reg) bytes / BW_BYTES_PER_ITER => _,
        out("v0") _, out("v1") _,
        options(nostack)
    );
}

/// Copy with `ldp q` / `stp q`; `bytes` must be a non-zero multiple of 128.
///
/// # Safety
/// `src .. src + bytes` readable, `dst .. dst + bytes` writable, regions must not overlap.
#[inline(never)]
pub unsafe fn bw_copy(src: *const u8, dst: *mut u8, bytes: usize) {
    asm!(
        "2:",
        "ldp q0, q1, [{s}]",
        "ldp q2, q3, [{s}, #32]",
        "ldp q4, q5, [{s}, #64]",
        "ldp q6, q7, [{s}, #96]",
        "stp q0, q1, [{d}]",
        "stp q2, q3, [{d}, #32]",
        "stp q4, q5, [{d}, #64]",
        "stp q6, q7, [{d}, #96]",
        "add {s}, {s}, #128",
        "add {d}, {d}, #128",
        "subs {n}, {n}, #1",
        "b.ne 2b",
        s = inout(reg) src => _,
        d = inout(reg) dst => _,
        n = inout(reg) bytes / BW_BYTES_PER_ITER => _,
        out("v0") _, out("v1") _, out("v2") _, out("v3") _,
        out("v4") _, out("v5") _, out("v6") _, out("v7") _,
        options(nostack)
    );
}
