//! Phase 6: inter-core transfers and memory-access corner cases.
//!
//! 6.1 4x4 matrix of one-way cache-line transfer latency: two pinned threads ping-pong on one
//!     line, either with store-release / load-acquire or with an atomic read-modify-write.
//! 6.2 store-to-load forwarding: latency of a store followed by a dependent load, for matching and
//!     failing (partial, straddling, misaligned, line- and page-crossing) combinations.
//! 6.3 cost of unaligned accesses (line, 4 KiB and page crossings), cross-checked with PMU counts.

use crate::harness::{metric, run_window, Collector, Metric, Session};
use crate::mem::Buffer;
use crate::pmu::{Group, Pmu};
use crate::stats::Summary;
use crate::timing::{cntvct, ticks_to_ns};
use crate::affinity;
use anyhow::{anyhow, Result};
use core::arch::aarch64::float32x4_t;
use core::arch::asm;
use serde_json::json;
use std::fmt::Write as _;
use std::hint::black_box;
use std::sync::Barrier;

fn med(s: &[Option<Summary>], i: usize) -> f64 {
    s[i].as_ref().map_or(f64::NAN, |x| x.median)
}

// ---------------------------------------------------------------------------------------------
// 6.1 ping-pong

/// Initiator: writes odd values, waits for the even value written back by the responder.
unsafe fn pp_init_store(n: u64, p: *mut u64) {
    asm!(
        "mov {c}, #0",
        "2:",
        "add {c}, {c}, #1",
        "stlr {c}, [{p}]",
        "add {c}, {c}, #1",
        "3:",
        "ldar {t}, [{p}]",
        "cmp {t}, {c}",
        "b.ne 3b",
        "subs {n}, {n}, #1",
        "b.ne 2b",
        c = out(reg) _, t = out(reg) _, p = in(reg) p, n = inout(reg) n => _, options(nostack)
    );
}

unsafe fn pp_resp_store(n: u64, p: *mut u64) {
    asm!(
        "mov {c}, #0",
        "2:",
        "add {c}, {c}, #1",
        "3:",
        "ldar {t}, [{p}]",
        "cmp {t}, {c}",
        "b.ne 3b",
        "add {c}, {c}, #1",
        "stlr {c}, [{p}]",
        "subs {n}, {n}, #1",
        "b.ne 2b",
        c = out(reg) _, t = out(reg) _, p = in(reg) p, n = inout(reg) n => _, options(nostack)
    );
}

/// Same protocol, but each side advances the value with an atomic add (LSE `ldaddal`).
unsafe fn pp_init_rmw(n: u64, p: *mut u64) {
    asm!(
        "mov {c}, #0",
        "2:",
        "add {c}, {c}, #1",
        "ldaddal {one}, {t}, [{p}]",
        "add {c}, {c}, #1",
        "3:",
        "ldar {t}, [{p}]",
        "cmp {t}, {c}",
        "b.ne 3b",
        "subs {n}, {n}, #1",
        "b.ne 2b",
        c = out(reg) _, t = out(reg) _, one = in(reg) 1u64, p = in(reg) p, n = inout(reg) n => _, options(nostack)
    );
}

unsafe fn pp_resp_rmw(n: u64, p: *mut u64) {
    asm!(
        "mov {c}, #0",
        "2:",
        "add {c}, {c}, #1",
        "3:",
        "ldar {t}, [{p}]",
        "cmp {t}, {c}",
        "b.ne 3b",
        "ldaddal {one}, {t}, [{p}]",
        "add {c}, {c}, #1",
        "subs {n}, {n}, #1",
        "b.ne 2b",
        c = out(reg) _, t = out(reg) _, one = in(reg) 1u64, p = in(reg) p, n = inout(reg) n => _, options(nostack)
    );
}

#[derive(Clone, Copy)]
enum Variant {
    Store,
    Rmw,
}

impl Variant {
    fn name(self) -> &'static str {
        match self {
            Variant::Store => "stlr_ldar",
            Variant::Rmw => "ldaddal_ldar",
        }
    }
}

/// One measured ping-pong between cores `a` (initiator) and `b`; returns one-way (ns, cycles).
fn pair_run(a: usize, b: usize, n: u64, variant: Variant, pmu: &Pmu, frq: u64, line: &Buffer) -> Result<(f64, f64)> {
    let p = line.as_mut_ptr() as usize;
    // SAFETY: the line buffer is live for the whole call and only these two threads touch it.
    unsafe { (p as *mut u64).write_volatile(0) };
    let barrier = Barrier::new(2);
    std::thread::scope(|s| {
        let resp = s.spawn(|| -> Result<()> {
            affinity::pin_to(b)?;
            barrier.wait();
            // SAFETY: p points to the shared, 8-byte-aligned line.
            unsafe {
                match variant {
                    Variant::Store => pp_resp_store(n, p as *mut u64),
                    Variant::Rmw => pp_resp_rmw(n, p as *mut u64),
                }
            }
            Ok(())
        });
        let init = s.spawn(|| -> Result<(f64, f64)> {
            affinity::pin_to(a)?;
            let g = Group::open(&[pmu.event("cpu_cycles")?])?;
            barrier.wait();
            g.reset_enable()?;
            let t0 = cntvct();
            // SAFETY: as above.
            unsafe {
                match variant {
                    Variant::Store => pp_init_store(n, p as *mut u64),
                    Variant::Rmw => pp_init_rmw(n, p as *mut u64),
                }
            }
            let t1 = cntvct();
            g.disable()?;
            let cycles = g.read()?.values[0] as f64;
            Ok((ticks_to_ns(t1 - t0, frq) / (2.0 * n as f64), cycles / (2.0 * n as f64)))
        });
        resp.join().map_err(|_| anyhow!("responder panicked"))??;
        init.join().map_err(|_| anyhow!("initiator panicked"))?
    })
}

fn run_matrix(c: &mut Collector, sess: &Session, csv: &mut String) -> Result<()> {
    let line = Buffer::new(16 << 10)?;
    let cores = [0usize, 1, 2, 3];
    let pairs: Vec<(usize, usize)> = cores.iter().flat_map(|&a| cores.iter().filter(move |&&b| b != a).map(move |&b| (a, b))).collect();
    let n = 200_000u64;
    let per_round = sess.repeat.div_ceil(3);
    println!("\n== 6.1 one-way cache-line transfer latency between cores (ns; cycles at the measured 2.4 GHz)");
    let _ = writeln!(csv, "pp_header,variant,initiator,responder,ns_one_way,cycles_one_way,ci95_lo,ci95_hi");
    for variant in [Variant::Store, Variant::Rmw] {
        let mut acc: std::collections::BTreeMap<(usize, usize), (Vec<Vec<f64>>, usize)> = Default::default();
        for round in 0..3 {
            let mut order = pairs.clone();
            if round % 2 == 1 {
                order.reverse();
            }
            for &(a, b) in &order {
                let exp = format!("pingpong/{}/{a}->{b}", variant.name());
                c.warmup = 2;
                let part = c.reps_raw(&exp, json!({"round": round}), per_round, &[metric("ns_one_way", "ns"), metric("cycles_one_way", "cycles")], || {
                    let (ns, cy) = pair_run(a, b, n, variant, &sess.pmu, sess.frq, &line)?;
                    Ok(vec![ns, cy])
                })?;
                let e = acc.entry((a, b)).or_insert_with(|| (vec![Vec::new(); 2], 0));
                for (dst, src) in e.0.iter_mut().zip(part.0) {
                    dst.extend(src);
                }
                e.1 += part.1;
            }
        }
        println!("  {} (rows: initiator, columns: responder)", variant.name());
        print!("        ");
        for b in cores {
            print!("   core {b}      ");
        }
        println!();
        for a in cores {
            print!("  core {a}");
            for b in cores {
                if a == b {
                    print!("      -         ");
                    continue;
                }
                let (vals, total) = &acc[&(a, b)];
                let sm = c.summarize_pooled(&format!("pingpong/{}/{a}->{b}", variant.name()), *total, &[metric("ns_one_way", "ns"), metric("cycles_one_way", "cycles")], vals)?;
                let (lo, hi) = sm[0].as_ref().map_or((f64::NAN, f64::NAN), |s| (s.ci95_lo, s.ci95_hi));
                let _ = writeln!(csv, "pp,{},{a},{b},{:.3},{:.2},{lo:.3},{hi:.3}", variant.name(), med(&sm, 0), med(&sm, 1));
                print!("  {:>6.1} ns/{:>4.0}c", med(&sm, 0), med(&sm, 1));
            }
            println!();
        }
    }
    c.warmup = crate::harness::WARMUP_REPS;
    Ok(())
}

/// The same matrix on several different lines (distinct pages, hence distinct physical addresses):
/// a latency that depends on the line rather than on the pair of cores shows up as different maps.
fn run_matrix_lines(c: &mut Collector, sess: &Session, csv: &mut String) -> Result<()> {
    let lines: Vec<Buffer> = (0..4).map(|_| Buffer::new(16 << 10)).collect::<Result<_>>()?;
    let cores = [0usize, 1, 2, 3];
    let pairs: Vec<(usize, usize)> = cores.iter().flat_map(|&a| cores.iter().filter(move |&&b| b != a).map(move |&b| (a, b))).collect();
    let n = 200_000u64;
    let per_round = sess.repeat.div_ceil(3);
    let ms = [metric("ns_one_way", "ns"), metric("cycles_one_way", "cycles")];
    println!("\n== 6.1b the same matrix (stlr/ldar) on four different lines: one-way ns, rows = initiator, columns = responder");
    let _ = writeln!(csv, "ppl_header,line,initiator,responder,ns_one_way,cycles_one_way,ci95_lo,ci95_hi");
    let mut acc: std::collections::BTreeMap<(usize, usize, usize), (Vec<Vec<f64>>, usize)> = Default::default();
    for round in 0..3 {
        for (li, line) in lines.iter().enumerate() {
            let mut order = pairs.clone();
            if (round + li) % 2 == 1 {
                order.reverse();
            }
            for &(a, b) in &order {
                let exp = format!("pingpong_lines/L{li}/{a}->{b}");
                c.warmup = 2;
                let part = c.reps_raw(&exp, json!({"round": round, "line": li, "addr": line.as_ptr() as usize}), per_round, &ms, || {
                    let (ns, cy) = pair_run(a, b, n, Variant::Store, &sess.pmu, sess.frq, line)?;
                    Ok(vec![ns, cy])
                })?;
                let e = acc.entry((li, a, b)).or_insert_with(|| (vec![Vec::new(); 2], 0));
                for (dst, src) in e.0.iter_mut().zip(part.0) {
                    dst.extend(src);
                }
                e.1 += part.1;
            }
        }
    }
    for li in 0..4 {
        println!("  line {li}:");
        for a in cores {
            print!("   ");
            for b in cores {
                if a == b {
                    print!("      -      ");
                    continue;
                }
                let (vals, total) = &acc[&(li, a, b)];
                let sm = c.summarize_pooled(&format!("pingpong_lines/L{li}/{a}->{b}"), *total, &ms, vals)?;
                let (lo, hi) = sm[0].as_ref().map_or((f64::NAN, f64::NAN), |x| (x.ci95_lo, x.ci95_hi));
                let _ = writeln!(csv, "ppl,{li},{a},{b},{:.3},{:.2},{lo:.3},{hi:.3}", med(&sm, 0), med(&sm, 1));
                print!("  {:>6.1} ns  ", med(&sm, 0));
            }
            println!();
        }
    }
    c.warmup = crate::harness::WARMUP_REPS;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// 6.2 store-to-load forwarding

macro_rules! dep8 {
    ($s:expr) => {
        concat!($s, "\n", $s, "\n", $s, "\n", $s, "\n", $s, "\n", $s, "\n", $s, "\n", $s)
    };
}

type SfFn = unsafe fn(u64, *mut u8);

macro_rules! sf_int {
    ($name:ident, $body:expr) => {
        unsafe fn $name(n: u64, p: *mut u8) {
            asm!("2:", dep8!($body), "subs {n}, {n}, #1", "b.ne 2b",
                p = in(reg) p, n = inout(reg) n => _, inout("x0") 0u64 => _, options(nostack));
        }
    };
}

macro_rules! sf_vec {
    ($name:ident, $body:expr) => {
        unsafe fn $name(n: u64, p: *mut u8) {
            let z = core::mem::zeroed::<float32x4_t>();
            asm!("2:", dep8!($body), "subs {n}, {n}, #1", "b.ne 2b",
                p = in(reg) p, n = inout(reg) n => _, inout("v0") z => _, options(nostack));
        }
    };
}

sf_int!(sf_none, "str x0, [{p}]
ldr x0, [{p}, #64]");
sf_int!(sf_x_x, "str x0, [{p}]\nldr x0, [{p}]");
sf_int!(sf_x_w_low, "str x0, [{p}]\nldr w0, [{p}]");
sf_int!(sf_x_w_high, "str x0, [{p}]\nldr w0, [{p}, #4]");
sf_int!(sf_x_b_low, "str x0, [{p}]\nldrb w0, [{p}]");
sf_int!(sf_x_b_high, "str x0, [{p}]\nldrb w0, [{p}, #7]");
sf_int!(sf_x_h_mid, "str x0, [{p}]\nldrh w0, [{p}, #2]");
sf_int!(sf_w_w, "str w0, [{p}]\nldr w0, [{p}]");
sf_int!(sf_w_x, "str w0, [{p}]\nldr x0, [{p}]");
sf_int!(sf_b_x, "strb w0, [{p}]\nldr x0, [{p}]");
sf_int!(sf_x_x_straddle, "str x0, [{p}]\nldur x0, [{p}, #4]");
sf_int!(sf_x_x_misaligned4, "stur x0, [{p}, #4]\nldur x0, [{p}, #4]");
sf_int!(sf_x_x_line_cross, "stur x0, [{p}, #60]\nldur x0, [{p}, #60]");
sf_int!(sf_x_x_page_cross, "stur x0, [{p}]\nldur x0, [{p}]"); // caller passes p = page end - 4
sf_int!(sf_stp_x_low, "stp x0, x0, [{p}]\nldr x0, [{p}]");
sf_int!(sf_stp_x_high, "stp x0, x0, [{p}]\nldr x0, [{p}, #8]");
sf_int!(sf_two_w_one_x, "str w0, [{p}]\nstr w0, [{p}, #4]\nldr x0, [{p}]");
sf_vec!(sf_q_q, "str q0, [{p}]\nldr q0, [{p}]");
sf_vec!(sf_q_d_low, "str q0, [{p}]\nldr d0, [{p}]");
sf_vec!(sf_q_d_high, "str q0, [{p}]\nldr d0, [{p}, #8]");
sf_vec!(sf_d_q, "str d0, [{p}]\nldr q0, [{p}]");
sf_vec!(sf_q_q_misaligned8, "stur q0, [{p}, #8]\nldur q0, [{p}, #8]");
sf_vec!(sf_q_q_line_cross, "stur q0, [{p}, #56]\nldur q0, [{p}, #56]");

struct SfCase {
    name: &'static str,
    f: SfFn,
    /// Byte offset added to the page-aligned base pointer.
    bias: usize,
}

fn sf_cases() -> Vec<SfCase> {
    let c = |name, f: SfFn, bias| SfCase { name, f, bias };
    vec![
        c("baseline: str x / ldr x on another line (no overlap, no forwarding)", sf_none, 0),
        c("str x / ldr x (same address)", sf_x_x, 0),
        c("str x / ldr w (low half)", sf_x_w_low, 0),
        c("str x / ldr w (high half, +4)", sf_x_w_high, 0),
        c("str x / ldrb (byte 0)", sf_x_b_low, 0),
        c("str x / ldrb (byte 7)", sf_x_b_high, 0),
        c("str x / ldrh (bytes 2-3)", sf_x_h_mid, 0),
        c("str w / ldr w (same address)", sf_w_w, 0),
        c("str w / ldr x (load larger than store)", sf_w_x, 0),
        c("strb / ldr x (load larger than store)", sf_b_x, 0),
        c("str x [p] / ldr x [p+4] (straddles the store)", sf_x_x_straddle, 0),
        c("stp x,x / ldr x (first half)", sf_stp_x_low, 0),
        c("stp x,x / ldr x (second half)", sf_stp_x_high, 0),
        c("str w [p], str w [p+4] / ldr x [p] (two stores, one load)", sf_two_w_one_x, 0),
        c("str x / ldr x at +4 (misaligned within a line)", sf_x_x_misaligned4, 0),
        c("str x / ldr x at +60 (crosses a 64-byte line)", sf_x_x_line_cross, 0),
        c("str x / ldr x crossing a 16 KiB page", sf_x_x_page_cross, 16384 - 4),
        c("str q / ldr q (same address)", sf_q_q, 0),
        c("str q / ldr d (low half)", sf_q_d_low, 0),
        c("str q / ldr d (high half)", sf_q_d_high, 0),
        c("str d / ldr q (load larger than store)", sf_d_q, 0),
        c("str q / ldr q at +8 (misaligned 16 B)", sf_q_q_misaligned8, 0),
        c("str q / ldr q at +56 (crosses a 64-byte line)", sf_q_q_line_cross, 0),
    ]
}

fn run_forwarding(c: &mut Collector, sess: &Session, csv: &mut String) -> Result<()> {
    let buf = Buffer::new(64 << 10)?;
    let grp = Group::open(&["cpu_cycles", "inst_retired"].iter().map(|n| sess.pmu.event(n)).collect::<Result<Vec<_>>>()?)?;
    let iters = 1u64 << 14;
    println!("\n== 6.2 store-to-load forwarding: cycles per dependent store + load pair (the loaded value feeds the next store)");
    let _ = writeln!(csv, "sf_header,case,cycles_per_pair,retired_per_pair");
    for case in sf_cases() {
        let p = unsafe { buf.as_mut_ptr().add(case.bias) };
        let units = (iters * 8) as f64;
        let sm = c.reps(&format!("forwarding/{}", case.name), sess.repeat, &[metric("cycles_per_pair", "cycles"), metric("retired_per_pair", "events")], || {
            let (_, v) = run_window(&grp, sess.frq, || unsafe { (case.f)(black_box(iters), p) })?;
            Ok(v.iter().map(|&x| x as f64 / units).collect())
        })?;
        let _ = writeln!(csv, "sf,\"{}\",{:.3},{:.3}", case.name, med(&sm, 0), med(&sm, 1));
        println!("  {:<62} {:>7.2} cycles", case.name, med(&sm, 0));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// 6.3 unaligned accesses

unsafe fn ld8_tp(n: u64, q: &[usize; 8]) {
    asm!("2:",
        "ldr {t0}, [{p0}]\nldr {t1}, [{p1}]\nldr {t2}, [{p2}]\nldr {t3}, [{p3}]\nldr {t4}, [{p4}]\nldr {t5}, [{p5}]\nldr {t6}, [{p6}]\nldr {t7}, [{p7}]\n\
         ldr {t0}, [{p0}]\nldr {t1}, [{p1}]\nldr {t2}, [{p2}]\nldr {t3}, [{p3}]\nldr {t4}, [{p4}]\nldr {t5}, [{p5}]\nldr {t6}, [{p6}]\nldr {t7}, [{p7}]",
        "subs {n}, {n}, #1", "b.ne 2b",
        t0 = out(reg) _, t1 = out(reg) _, t2 = out(reg) _, t3 = out(reg) _, t4 = out(reg) _, t5 = out(reg) _, t6 = out(reg) _, t7 = out(reg) _,
        p0 = in(reg) q[0], p1 = in(reg) q[1], p2 = in(reg) q[2], p3 = in(reg) q[3], p4 = in(reg) q[4], p5 = in(reg) q[5], p6 = in(reg) q[6], p7 = in(reg) q[7],
        n = inout(reg) n => _, options(nostack, readonly));
}

unsafe fn st8_tp(n: u64, q: &[usize; 8]) {
    asm!("2:",
        "str {v}, [{p0}]\nstr {v}, [{p1}]\nstr {v}, [{p2}]\nstr {v}, [{p3}]\nstr {v}, [{p4}]\nstr {v}, [{p5}]\nstr {v}, [{p6}]\nstr {v}, [{p7}]\n\
         str {v}, [{p0}]\nstr {v}, [{p1}]\nstr {v}, [{p2}]\nstr {v}, [{p3}]\nstr {v}, [{p4}]\nstr {v}, [{p5}]\nstr {v}, [{p6}]\nstr {v}, [{p7}]",
        "subs {n}, {n}, #1", "b.ne 2b",
        v = in(reg) 0u64,
        p0 = in(reg) q[0], p1 = in(reg) q[1], p2 = in(reg) q[2], p3 = in(reg) q[3], p4 = in(reg) q[4], p5 = in(reg) q[5], p6 = in(reg) q[6], p7 = in(reg) q[7],
        n = inout(reg) n => _, options(nostack));
}

unsafe fn ldq_tp(n: u64, q: &[usize; 8]) {
    asm!("2:",
        "ldr {t0:q}, [{p0}]\nldr {t1:q}, [{p1}]\nldr {t2:q}, [{p2}]\nldr {t3:q}, [{p3}]\nldr {t4:q}, [{p4}]\nldr {t5:q}, [{p5}]\nldr {t6:q}, [{p6}]\nldr {t7:q}, [{p7}]\n\
         ldr {t0:q}, [{p0}]\nldr {t1:q}, [{p1}]\nldr {t2:q}, [{p2}]\nldr {t3:q}, [{p3}]\nldr {t4:q}, [{p4}]\nldr {t5:q}, [{p5}]\nldr {t6:q}, [{p6}]\nldr {t7:q}, [{p7}]",
        "subs {n}, {n}, #1", "b.ne 2b",
        t0 = out(vreg) _, t1 = out(vreg) _, t2 = out(vreg) _, t3 = out(vreg) _, t4 = out(vreg) _, t5 = out(vreg) _, t6 = out(vreg) _, t7 = out(vreg) _,
        p0 = in(reg) q[0], p1 = in(reg) q[1], p2 = in(reg) q[2], p3 = in(reg) q[3], p4 = in(reg) q[4], p5 = in(reg) q[5], p6 = in(reg) q[6], p7 = in(reg) q[7],
        n = inout(reg) n => _, options(nostack, readonly));
}

unsafe fn stq_tp(n: u64, q: &[usize; 8]) {
    let z = core::mem::zeroed::<float32x4_t>();
    asm!("2:",
        "str {v:q}, [{p0}]\nstr {v:q}, [{p1}]\nstr {v:q}, [{p2}]\nstr {v:q}, [{p3}]\nstr {v:q}, [{p4}]\nstr {v:q}, [{p5}]\nstr {v:q}, [{p6}]\nstr {v:q}, [{p7}]\n\
         str {v:q}, [{p0}]\nstr {v:q}, [{p1}]\nstr {v:q}, [{p2}]\nstr {v:q}, [{p3}]\nstr {v:q}, [{p4}]\nstr {v:q}, [{p5}]\nstr {v:q}, [{p6}]\nstr {v:q}, [{p7}]",
        "subs {n}, {n}, #1", "b.ne 2b",
        v = in(vreg) z,
        p0 = in(reg) q[0], p1 = in(reg) q[1], p2 = in(reg) q[2], p3 = in(reg) q[3], p4 = in(reg) q[4], p5 = in(reg) q[5], p6 = in(reg) q[6], p7 = in(reg) q[7],
        n = inout(reg) n => _, options(nostack));
}

type TpFn = unsafe fn(u64, &[usize; 8]);

fn run_unaligned(c: &mut Collector, sess: &Session, csv: &mut String) -> Result<()> {
    let buf = Buffer::new(256 << 10)?;
    let base = buf.as_ptr() as usize;
    let grp = Group::open(&["cpu_cycles", "l1d_cache", "l1d_tlb", "mem_access"].iter().map(|n| sess.pmu.event(n)).collect::<Result<Vec<_>>>()?)?;
    let iters = 1u64 << 14;
    let metrics = [
        metric("cycles_per_access", "cycles"),
        metric("l1d_cache_per_access", "events"),
        metric("l1d_tlb_per_access", "events"),
        metric("mem_access_per_access", "events"),
    ];
    println!("\n== 6.3 unaligned accesses: cycles per access (independent accesses, L1-resident) and L1D accesses / L1 TLB lookups per access");
    let _ = writeln!(csv, "unal_header,kind,size,layout,offset,cycles_per_access,l1d_cache_per_access,l1d_tlb_per_access,mem_access_per_access");
    // layout: ("line", o) -> 8 accesses at base + i*64 + o; ("4k", d) -> at base + i*16384 + 4096 - d; ("page", d) -> base + i*16384 + 16384 - d
    let kinds: [(&str, usize, TpFn); 4] = [("load", 8, ld8_tp), ("store", 8, st8_tp), ("load", 16, ldq_tp), ("store", 16, stq_tp)];
    for (kind, size, f) in kinds {
        let mut cases: Vec<(String, &str, usize, [usize; 8])> = Vec::new();
        let offs: &[usize] = if size == 8 { &[0, 1, 4, 7, 8, 9, 12, 15, 16, 24, 28, 31, 32, 48, 56, 57, 60, 63] } else { &[0, 1, 4, 8, 12, 15, 16, 24, 32, 40, 48, 49, 56, 60, 63] };
        for &o in offs {
            let mut a = [0usize; 8];
            for (i, slot) in a.iter_mut().enumerate() {
                *slot = base + i * 64 + o;
            }
            cases.push((format!("line offset {o}"), "line", o, a));
        }
        let d = size / 2;
        // Two distinct boundaries per test, used alternately: eight boundaries 16 KiB apart would all
        // map to the same L1 sets (way size = 16 KiB) and turn the test into a conflict-miss test.
        for (name, layout, edges) in [
            ("crosses a 4 KiB boundary inside a 16 KiB page", "4k", [4096usize, 8192]),
            ("crosses a 16 KiB page boundary", "page", [16384, 32768]),
        ] {
            let mut a = [0usize; 8];
            for (i, slot) in a.iter_mut().enumerate() {
                *slot = base + edges[i % 2] - d;
            }
            cases.push((name.to_string(), layout, d, a));
        }
        println!("  {kind} {size} B:");
        for (name, layout, o, ptrs) in cases {
            let units = (iters * 16) as f64;
            let sm = c.reps(&format!("unaligned/{kind}{size}/{name}"), sess.repeat, &metrics, || {
                let (_, v) = run_window(&grp, sess.frq, || unsafe { f(black_box(iters), &ptrs) })?;
                Ok(v.iter().map(|&x| x as f64 / units).collect())
            })?;
            let _ = writeln!(csv, "unal,{kind},{size},{layout},{o},{:.4},{:.4},{:.4},{:.4}", med(&sm, 0), med(&sm, 1), med(&sm, 2), med(&sm, 3));
            println!("    {name:<48} {:>6.3} cycles  L1D {:.2}  TLB {:.2}  mem {:.2}", med(&sm, 0), med(&sm, 1), med(&sm, 2), med(&sm, 3));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------

pub fn run_multicore(sess: &Session) -> Result<()> {
    let mut c = sess.collector("multicore")?;
    let mut csv = String::from("kind,a,b,c,d,e,f,g,h\n");
    run_matrix(&mut c, sess, &mut csv)?;
    run_matrix_lines(&mut c, sess, &mut csv)?;
    run_forwarding(&mut c, sess, &mut csv)?;
    run_unaligned(&mut c, sess, &mut csv)?;
    std::fs::write(sess.day.join("multicore_experiments.csv"), csv)?;
    let _: Option<Metric> = None;
    println!("multicore: {} reps, {} flagged invalid; data in {}", c.total_reps, c.invalid_reps, sess.day.display());
    Ok(())
}
