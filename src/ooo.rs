//! Phase 5: the out-of-order core.
//!
//! 5.1 window sizes: one independent DRAM miss per iteration followed by N filler instructions of a
//!     given kind; the time per iteration rises linearly with N (the window holds W/(N+c) misses at
//!     once) until one iteration fills the window, then saturates at the miss latency. The slope gives
//!     W, the resource that saturates first depends on the filler: `nop` (ROB), integer / vector
//!     writes (physical register files), loads (load queue), stores (store buffer).
//! 5.2 memory-level parallelism: K interleaved dependent chains, per-load time vs K.
//! 5.3 latency and reciprocal throughput of individual instructions (unrolled `asm!` loops).

use crate::a64::*;
use crate::harness::{metric, run_window, Collector, Metric, Session};
use crate::jit::JitCode;
use crate::mem::Buffer;
use crate::perm::sattolo;
use crate::pmu::{Group, Pmu};
use crate::stats::{jumps, Rng, Summary};
use anyhow::{anyhow, Result};
use core::arch::aarch64::float32x4_t;
use core::arch::asm;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::hint::black_box;

const ROUNDS: usize = 3;
const LINE: usize = 64;

type Pooled = (Vec<Vec<f64>>, usize);

fn pool_into(acc: &mut BTreeMap<String, Pooled>, key: &str, part: Pooled) {
    let e = acc.entry(key.to_string()).or_insert_with(|| (vec![Vec::new(); part.0.len()], 0));
    for (dst, src) in e.0.iter_mut().zip(part.0) {
        dst.extend(src);
    }
    e.1 += part.1;
}

fn med(s: &[Option<Summary>], i: usize) -> f64 {
    s[i].as_ref().map_or(f64::NAN, |x| x.median)
}

fn group(pmu: &Pmu, names: &[&str]) -> Result<Group> {
    Group::open(&names.iter().map(|n| pmu.event(n)).collect::<Result<Vec<_>>>()?)
}

fn core_metrics() -> [Metric; 3] {
    [metric("cycles_per_unit", "cycles"), metric("instructions_per_unit", "events"), metric("stall_backend_per_unit", "cycles")]
}

// ---------------------------------------------------------------------------------------------
// 5.1 window sizes

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Filler {
    Nop,
    Int,
    Vec,
    Load,
    Store,
}

impl Filler {
    fn name(self) -> &'static str {
        match self {
            Filler::Nop => "nop",
            Filler::Int => "int_add",
            Filler::Vec => "vec_eor",
            Filler::Load => "load_l1",
            Filler::Store => "store_l1",
        }
    }
}

/// 16 MiB: far beyond the caches (L3 = 2 MiB) yet inside the L2 TLB reach (20 MiB), so no page walks.
const REGION: usize = 16 << 20;
const SCRATCH: usize = 16 << 10;
/// Instructions per iteration other than the fillers: 3 xorshift + and + load + consumer + subs + b.ne.
const FIXED_INSTR: usize = 8;

/// x0 = iterations, x1 = buffer (scratch page first, DRAM region after it).
fn gen_window(kind: Filler, n: usize, dram: bool) -> Result<Vec<u32>> {
    let mut a = Asm::new();
    // The generator state lives in the first 8 bytes of the scratch page so that every call continues
    // the sequence: restarting from the same seed would replay the same addresses and hit in cache.
    a.emit(ldr_x_uimm(2, 1, 0));
    if dram {
        a.mov_imm64(9, ((REGION - 1) & !(LINE - 1)) as u64);
        a.mov_imm64(10, SCRATCH as u64);
        a.emit(add_reg(6, 1, 10));
    } else {
        a.mov_imm64(9, 0xFC0); // 64 lines inside the L1-resident scratch page
        a.emit(add_reg(6, 1, XZR));
    }
    a.emit(eor_v(24, 24, 24));
    a.emit(movz(7, 0, 0));
    let top = a.label();
    a.bind(top);
    a.emit(eor_lsl(2, 2, 2, 13));
    a.emit(eor_lsr(2, 2, 2, 7));
    a.emit(eor_lsl(2, 2, 2, 17));
    a.emit(and_reg(4, 2, 9));
    a.emit(ldr_x_reg(5, 6, 4));
    // Consumer of the load: it cannot retire before the data arrives, which is what blocks retirement.
    a.emit(add_reg(7, 7, 5));
    for i in 0..n {
        a.emit(match kind {
            Filler::Nop => nop(),
            Filler::Int => add_imm(10 + (i % 8) as u32, 10 + (i % 8) as u32, 1),
            Filler::Vec => eor_v(16 + (i % 8) as u32, 16 + (i % 8) as u32, 24),
            Filler::Load => ldr_x_uimm(XZR, 1, ((i % 32) * 8) as u32),
            Filler::Store => str_x_uimm(XZR, 1, (((i % 56) + 1) * 64) as u32),
        });
    }
    a.emit(subs_imm(0, 0, 1));
    a.b_cond(COND_NE, top);
    a.emit(str_x_uimm(2, 1, 0));
    a.emit(ret());
    a.finish().map_err(|e| anyhow!(e))
}

fn run_windows(c: &mut Collector, grp: &Group, sess: &Session, csv: &mut String) -> Result<()> {
    let buf = Buffer::new(REGION + SCRATCH)?;
    let ns: Vec<usize> = [0usize, 1, 2, 4, 8, 12, 16, 20].into_iter().chain((24..=144).step_by(2)).chain([160, 176, 192, 224, 256, 320, 384, 448, 512]).collect();
    let kinds = [Filler::Nop, Filler::Int, Filler::Vec, Filler::Load, Filler::Store];
    let per_round = sess.repeat.div_ceil(ROUNDS);
    let mut acc: BTreeMap<String, Pooled> = BTreeMap::new();
    let mut keys: Vec<(Filler, usize)> = kinds.iter().flat_map(|&k| ns.iter().map(move |&n| (k, n))).collect();
    let base = buf.as_ptr();
    // SAFETY: the buffer is larger than 8 bytes.
    unsafe { (buf.as_mut_ptr() as *mut u64).write(0x9E37_79B9_7F4A_7C15) };
    for round in 0..ROUNDS {
        if round % 2 == 1 {
            keys.reverse();
        }
        for &(kind, n) in &keys {
            let code = JitCode::new(&gen_window(kind, n, true)?)?;
            let iters = 1u64 << 14;
            let exp = format!("window/{}/N={n}", kind.name());
            let part = c.reps_raw(&exp, json!({"filler": kind.name(), "n": n, "round": round}), per_round, &core_metrics(), || {
                let (_, v) = run_window(grp, sess.frq, || {
                    black_box(code.call(iters, black_box(base)));
                })?;
                Ok(v.iter().map(|&x| x as f64 / iters as f64).collect())
            })?;
            pool_into(&mut acc, &exp, part);
        }
    }
    // Reference: same loop with the load hitting L1 (no miss to hide).
    let mut l1 = Vec::new();
    for &kind in &kinds {
        let mut row = Vec::new();
        for &n in &ns {
            let code = JitCode::new(&gen_window(kind, n, false)?)?;
            let iters = 1u64 << 14;
            let exp = format!("window_l1/{}/N={n}", kind.name());
            let (vals, total) = c.reps_raw(&exp, Value::Null, sess.repeat.min(6), &core_metrics(), || {
                let (_, v) = run_window(grp, sess.frq, || {
                    black_box(code.call(iters, black_box(base)));
                })?;
                Ok(v.iter().map(|&x| x as f64 / iters as f64).collect())
            })?;
            let sm = c.summarize_pooled(&exp, total, &core_metrics(), &vals)?;
            row.push((n, med(&sm, 0)));
        }
        l1.push((kind, row));
    }
    println!("\n== 5.1 window sizes: cycles per iteration (one independent DRAM miss + N fillers; {FIXED_INSTR} fixed instructions per iteration)");
    println!("   T(N) is piecewise linear; it jumps where one more iteration stops fitting in the window (W / (N + {FIXED_INSTR}) drops below an integer).");
    let _ = writeln!(csv, "window_header,filler,n,cycles_per_iter,instr_per_iter,stall_backend_per_iter,cycles_per_iter_l1_hit_reference");
    let _ = writeln!(csv, "window_jump_header,filler,n_lo,n_hi,jump_cycles,window_lo,window_hi");
    for &kind in &kinds {
        let mut t = Vec::new();
        let l1row: Vec<(usize, f64)> = l1.iter().find(|(k, _)| *k == kind).unwrap().1.clone();
        for (idx, &n) in ns.iter().enumerate() {
            let exp = format!("window/{}/N={n}", kind.name());
            let (vals, total) = &acc[&exp];
            let sm = c.summarize_pooled(&exp, *total, &core_metrics(), vals)?;
            let _ = writeln!(csv, "window,{},{n},{:.3},{:.3},{:.3},{:.3}", kind.name(), med(&sm, 0), med(&sm, 1), med(&sm, 2), l1row[idx].1);
            t.push(med(&sm, 0));
        }
        let js = jumps(&ns, &t, 2.5);
        let tail = (t[t.len() - 1] - t[t.len() - 6]) / (ns[ns.len() - 1] as f64 - ns[ns.len() - 6] as f64);
        print!("  {:<9} throughput-bound slope {tail:.3} cycles/filler; jumps:", kind.name());
        for j in &js {
            let (wl, wh) = (j.lo + FIXED_INSTR + 1, j.hi + FIXED_INSTR);
            let _ = writeln!(csv, "window_jump,{},{},{},{:.2},{wl},{wh}", kind.name(), j.lo, j.hi, j.size);
            print!(" N {}->{} (+{:.0} cycles => window {}..{})", j.lo, j.hi, j.size, wl, wh);
        }
        if js.is_empty() {
            print!(" none");
        }
        println!();
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// 5.2 memory-level parallelism

const CHAIN_REGS: [u32; 22] = [2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 19, 20, 21, 22, 23, 24];

/// K interleaved dependent pointer chases; start pointers are read from the data area.
fn gen_mlp(k: usize) -> Result<Vec<u32>> {
    assert!(k >= 1 && k <= CHAIN_REGS.len());
    let mut a = Asm::new();
    let saved: Vec<(u32, u32)> = if k > 16 { vec![(19, 20), (21, 22), (23, 24)] } else { vec![] };
    for &(r1, r2) in &saved {
        a.emit(stp_pre16(r1, r2));
    }
    for i in 0..k {
        a.emit(ldr_x_uimm(CHAIN_REGS[i], 1, 8 * i as u32));
    }
    let top = a.label();
    a.bind(top);
    for i in 0..k {
        a.emit(ldr_x_uimm(CHAIN_REGS[i], CHAIN_REGS[i], 0));
    }
    a.emit(subs_imm(0, 0, 1));
    a.b_cond(COND_NE, top);
    for &(r1, r2) in saved.iter().rev() {
        a.emit(ldp_post16(r1, r2));
    }
    a.emit(ret());
    a.finish().map_err(|e| anyhow!(e))
}

fn run_mlp(c: &mut Collector, grp: &Group, sess: &Session, csv: &mut String) -> Result<()> {
    let ks = [1usize, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 14, 16, 18, 20, 22];
    let per_round = sess.repeat.div_ceil(ROUNDS);
    let levels: [(&str, usize); 2] = [("L2-resident", 5120), ("DRAM", 1 << 18)]; // total lines over all chains (320 KiB / 16 MiB)
    let buf = Buffer::new((1 << 18) * LINE)?;
    println!("\n== 5.2 memory-level parallelism: cycles per load with K independent dependent chains");
    let _ = writeln!(csv, "mlp_header,level,k,cycles_per_load,ns_per_load");
    for &(level, size) in &levels {
        let mut acc: BTreeMap<String, Pooled> = BTreeMap::new();
        for round in 0..ROUNDS {
            let mut order = ks.to_vec();
            if round % 2 == 1 {
                order.reverse();
            }
            for &k in &order {
                let lines = size / k;
                let mut starts = vec![0u64; 24];
                for i in 0..k {
                    let base = buf.as_mut_ptr() as usize + i * lines * LINE;
                    let next = sattolo(lines, &mut Rng::new(900 + i as u64 * 7 + round as u64));
                    // SAFETY: k * lines * 64 <= buffer size for both levels.
                    unsafe {
                        for (j, &n) in next.iter().enumerate() {
                            ((base + j * LINE) as *mut usize).write(base + n as usize * LINE);
                        }
                    }
                    starts[i] = base as u64;
                }
                let code = JitCode::new(&gen_mlp(k)?)?;
                let iters = ((1u64 << 19) / k as u64).max(2);
                let exp = format!("mlp/{level}/K={k}");
                let units = (iters * k as u64) as f64;
                c.warmup = if level == "DRAM" { 1 } else { 3 };
                let part = c.reps_raw(&exp, json!({"level": level, "k": k, "round": round}), per_round, &core_metrics(), || {
                    let (_, v) = run_window(grp, sess.frq, || {
                        black_box(code.call(iters, black_box(starts.as_ptr() as *const u8)));
                    })?;
                    Ok(v.iter().map(|&x| x as f64 / units).collect())
                })?;
                pool_into(&mut acc, &exp, part);
            }
        }
        c.warmup = crate::harness::WARMUP_REPS;
        print!("  {level:<12}:");
        let mut first = f64::NAN;
        let mut last = f64::NAN;
        for &k in &ks {
            let exp = format!("mlp/{level}/K={k}");
            let (vals, total) = &acc[&exp];
            let sm = c.summarize_pooled(&exp, *total, &core_metrics(), vals)?;
            let cy = med(&sm, 0);
            if k == 1 {
                first = cy;
            }
            last = cy;
            let _ = writeln!(csv, "mlp,{level},{k},{cy:.3},{:.3}", cy / 2.4);
            print!(" K={k}:{cy:.1}");
        }
        println!("\n     latency K=1: {first:.1} cycles; K=22: {last:.1} cycles per load => effective parallelism ~ {:.1}", first / last);
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// 5.3 instruction latency and throughput

macro_rules! dep16 {
    ($s:expr) => {
        concat!($s, "\n", $s, "\n", $s, "\n", $s, "\n", $s, "\n", $s, "\n", $s, "\n", $s, "\n", $s, "\n", $s, "\n", $s, "\n", $s, "\n", $s, "\n", $s, "\n", $s, "\n", $s)
    };
}

/// 16 instructions writing 8 different destinations (each used twice): `op DEST suffix`.
macro_rules! ind16 {
    ($op:expr, $m:expr, $arr:expr, $suf:expr) => {
        concat!(
            $op, " ", "{a0", $m, "}", $arr, $suf, "\n", $op, " ", "{a1", $m, "}", $arr, $suf, "\n",
            $op, " ", "{a2", $m, "}", $arr, $suf, "\n", $op, " ", "{a3", $m, "}", $arr, $suf, "\n",
            $op, " ", "{a4", $m, "}", $arr, $suf, "\n", $op, " ", "{a5", $m, "}", $arr, $suf, "\n",
            $op, " ", "{a6", $m, "}", $arr, $suf, "\n", $op, " ", "{a7", $m, "}", $arr, $suf, "\n",
            $op, " ", "{a0", $m, "}", $arr, $suf, "\n", $op, " ", "{a1", $m, "}", $arr, $suf, "\n",
            $op, " ", "{a2", $m, "}", $arr, $suf, "\n", $op, " ", "{a3", $m, "}", $arr, $suf, "\n",
            $op, " ", "{a4", $m, "}", $arr, $suf, "\n", $op, " ", "{a5", $m, "}", $arr, $suf, "\n",
            $op, " ", "{a6", $m, "}", $arr, $suf, "\n", $op, " ", "{a7", $m, "}", $arr, $suf
        )
    };
}

type BenchFn = unsafe fn(u64, *mut u8);

macro_rules! int_bench {
    ($lat:ident, $tp:ident, $op:literal) => {
        unsafe fn $lat(n: u64, _p: *mut u8) {
            asm!("2:", dep16!(concat!($op, " {a}, {a}, {b}")), "subs {n}, {n}, #1", "b.ne 2b",
                a = inout(reg) 0x1234_5678u64 => _, b = in(reg) 3u64, n = inout(reg) n => _, options(nomem, nostack));
        }
        unsafe fn $tp(n: u64, _p: *mut u8) {
            asm!("2:", ind16!($op, "", "", ", {b}, {b}"), "subs {n}, {n}, #1", "b.ne 2b",
                a0 = out(reg) _, a1 = out(reg) _, a2 = out(reg) _, a3 = out(reg) _, a4 = out(reg) _, a5 = out(reg) _, a6 = out(reg) _, a7 = out(reg) _,
                b = in(reg) 3u64, n = inout(reg) n => _, options(nomem, nostack));
        }
    };
}

int_bench!(add_lat, add_tp, "add");
int_bench!(mul_lat, mul_tp, "mul");

unsafe fn madd_lat(n: u64, _p: *mut u8) {
    asm!("2:", dep16!("madd {a}, {a}, {b}, {c}"), "subs {n}, {n}, #1", "b.ne 2b",
        a = inout(reg) 1u64 => _, b = in(reg) 1u64, c = in(reg) 0u64, n = inout(reg) n => _, options(nomem, nostack));
}
unsafe fn madd_tp(n: u64, _p: *mut u8) {
    asm!("2:", ind16!("madd", "", "", ", {b}, {b}, {c}"), "subs {n}, {n}, #1", "b.ne 2b",
        a0 = out(reg) _, a1 = out(reg) _, a2 = out(reg) _, a3 = out(reg) _, a4 = out(reg) _, a5 = out(reg) _, a6 = out(reg) _, a7 = out(reg) _,
        b = in(reg) 3u64, c = in(reg) 0u64, n = inout(reg) n => _, options(nomem, nostack));
}
unsafe fn sdiv_lat(n: u64, _p: *mut u8) {
    // divisor 1: the dividend keeps its value; the latency of a division by 1
    asm!("2:", dep16!("sdiv {a}, {a}, {b}"), "subs {n}, {n}, #1", "b.ne 2b",
        a = inout(reg) 0x7FFF_FFFF_FFFF_FFFFu64 => _, b = in(reg) 1u64, n = inout(reg) n => _, options(nomem, nostack));
}
unsafe fn sdiv_tp(n: u64, _p: *mut u8) {
    asm!("2:", ind16!("sdiv", "", "", ", {c}, {d}"), "subs {n}, {n}, #1", "b.ne 2b",
        a0 = out(reg) _, a1 = out(reg) _, a2 = out(reg) _, a3 = out(reg) _, a4 = out(reg) _, a5 = out(reg) _, a6 = out(reg) _, a7 = out(reg) _,
        c = in(reg) 0x7FFF_FFFF_FFFF_FFFFu64, d = in(reg) 3u64, n = inout(reg) n => _, options(nomem, nostack));
}
unsafe fn fadd_lat(n: u64, _p: *mut u8) {
    asm!("2:", dep16!("fadd {a:d}, {a:d}, {b:d}"), "subs {n}, {n}, #1", "b.ne 2b",
        a = inout(vreg) 1.0f64 => _, b = in(vreg) 0.0f64, n = inout(reg) n => _, options(nomem, nostack));
}
unsafe fn fadd_tp(n: u64, _p: *mut u8) {
    asm!("2:", ind16!("fadd", ":d", "", ", {b:d}, {b:d}"), "subs {n}, {n}, #1", "b.ne 2b",
        a0 = out(vreg) _, a1 = out(vreg) _, a2 = out(vreg) _, a3 = out(vreg) _, a4 = out(vreg) _, a5 = out(vreg) _, a6 = out(vreg) _, a7 = out(vreg) _,
        b = in(vreg) 1.0f64, n = inout(reg) n => _, options(nomem, nostack));
}
unsafe fn fmul_lat(n: u64, _p: *mut u8) {
    asm!("2:", dep16!("fmul {a:d}, {a:d}, {b:d}"), "subs {n}, {n}, #1", "b.ne 2b",
        a = inout(vreg) 1.0f64 => _, b = in(vreg) 1.0f64, n = inout(reg) n => _, options(nomem, nostack));
}
unsafe fn fmul_tp(n: u64, _p: *mut u8) {
    asm!("2:", ind16!("fmul", ":d", "", ", {b:d}, {b:d}"), "subs {n}, {n}, #1", "b.ne 2b",
        a0 = out(vreg) _, a1 = out(vreg) _, a2 = out(vreg) _, a3 = out(vreg) _, a4 = out(vreg) _, a5 = out(vreg) _, a6 = out(vreg) _, a7 = out(vreg) _,
        b = in(vreg) 1.0f64, n = inout(reg) n => _, options(nomem, nostack));
}
unsafe fn fmla_lat(n: u64, _p: *mut u8) {
    let z = core::mem::zeroed::<float32x4_t>();
    asm!("2:", dep16!("fmla {a:v}.4s, {b:v}.4s, {c:v}.4s"), "subs {n}, {n}, #1", "b.ne 2b",
        a = inout(vreg) z => _, b = in(vreg) z, c = in(vreg) z, n = inout(reg) n => _, options(nomem, nostack));
}
unsafe fn fmla_tp(n: u64, _p: *mut u8) {
    let z = core::mem::zeroed::<float32x4_t>();
    asm!("2:", ind16!("fmla", ":v", ".4s", ", {b:v}.4s, {c:v}.4s"), "subs {n}, {n}, #1", "b.ne 2b",
        a0 = inout(vreg) z => _, a1 = inout(vreg) z => _, a2 = inout(vreg) z => _, a3 = inout(vreg) z => _,
        a4 = inout(vreg) z => _, a5 = inout(vreg) z => _, a6 = inout(vreg) z => _, a7 = inout(vreg) z => _,
        b = in(vreg) z, c = in(vreg) z, n = inout(reg) n => _, options(nomem, nostack));
}
unsafe fn fmla_tp16(n: u64, _p: *mut u8) {
    // sixteen independent accumulators would need 16 vector operands: use the 8 twice per pair of lines
    fmla_tp(n, _p);
}
unsafe fn sdot_lat(n: u64, _p: *mut u8) {
    let z = core::mem::zeroed::<float32x4_t>();
    asm!("2:", dep16!("sdot {a:v}.4s, {b:v}.16b, {c:v}.16b"), "subs {n}, {n}, #1", "b.ne 2b",
        a = inout(vreg) z => _, b = in(vreg) z, c = in(vreg) z, n = inout(reg) n => _, options(nomem, nostack));
}
unsafe fn sdot_tp(n: u64, _p: *mut u8) {
    let z = core::mem::zeroed::<float32x4_t>();
    asm!("2:", ind16!("sdot", ":v", ".4s", ", {b:v}.16b, {c:v}.16b"), "subs {n}, {n}, #1", "b.ne 2b",
        a0 = inout(vreg) z => _, a1 = inout(vreg) z => _, a2 = inout(vreg) z => _, a3 = inout(vreg) z => _,
        a4 = inout(vreg) z => _, a5 = inout(vreg) z => _, a6 = inout(vreg) z => _, a7 = inout(vreg) z => _,
        b = in(vreg) z, c = in(vreg) z, n = inout(reg) n => _, options(nomem, nostack));
}
unsafe fn ldr_lat(n: u64, p: *mut u8) {
    asm!("2:", dep16!("ldr {a}, [{a}]"), "subs {n}, {n}, #1", "b.ne 2b",
        a = inout(reg) p => _, n = inout(reg) n => _, options(nostack, readonly));
}
unsafe fn ldr_tp(n: u64, p: *mut u8) {
    asm!("2:", ind16!("ldr", "", "", ", [{p}]"), "subs {n}, {n}, #1", "b.ne 2b",
        a0 = out(reg) _, a1 = out(reg) _, a2 = out(reg) _, a3 = out(reg) _, a4 = out(reg) _, a5 = out(reg) _, a6 = out(reg) _, a7 = out(reg) _,
        p = in(reg) p, n = inout(reg) n => _, options(nostack, readonly));
}
unsafe fn ldp_lat(n: u64, p: *mut u8) {
    asm!("2:", dep16!("ldp {a}, {c}, [{a}]"), "subs {n}, {n}, #1", "b.ne 2b",
        a = inout(reg) p => _, c = out(reg) _, n = inout(reg) n => _, options(nostack, readonly));
}
unsafe fn ldp_tp(n: u64, p: *mut u8) {
    asm!("2:", dep16!("ldp {a0}, {a1}, [{p}]"), "subs {n}, {n}, #1", "b.ne 2b",
        a0 = out(reg) _, a1 = out(reg) _, p = in(reg) p, n = inout(reg) n => _, options(nostack, readonly));
}
unsafe fn ldpq_tp(n: u64, p: *mut u8) {
    asm!("2:", dep16!("ldp {a0:q}, {a1:q}, [{p}]"), "subs {n}, {n}, #1", "b.ne 2b",
        a0 = out(vreg) _, a1 = out(vreg) _, p = in(reg) p, n = inout(reg) n => _, options(nostack, readonly));
}
unsafe fn str_tp(n: u64, p: *mut u8) {
    let p = p.add(1024); // never overwrite the self-pointer at the start of the scratch area
    asm!("2:", dep16!("str {b}, [{p}]"), "subs {n}, {n}, #1", "b.ne 2b",
        b = in(reg) 0u64, p = in(reg) p, n = inout(reg) n => _, options(nostack));
}
unsafe fn stp_tp(n: u64, p: *mut u8) {
    let p = p.add(1024); // never overwrite the self-pointer at the start of the scratch area
    asm!("2:", dep16!("stp {b}, {b}, [{p}]"), "subs {n}, {n}, #1", "b.ne 2b",
        b = in(reg) 0u64, p = in(reg) p, n = inout(reg) n => _, options(nostack));
}
unsafe fn dmb_tp(n: u64, _p: *mut u8) {
    asm!("2:", dep16!("dmb ish"), "subs {n}, {n}, #1", "b.ne 2b", n = inout(reg) n => _, options(nostack));
}
unsafe fn dmb_st_tp(n: u64, p: *mut u8) {
    let p = p.add(1024); // never overwrite the self-pointer at the start of the scratch area
    asm!("2:", dep16!("str {b}, [{p}]\ndmb ish"), "subs {n}, {n}, #1", "b.ne 2b",
        b = in(reg) 0u64, p = in(reg) p, n = inout(reg) n => _, options(nostack));
}
unsafe fn ldar_lat(n: u64, p: *mut u8) {
    asm!("2:", dep16!("ldar {a}, [{a}]"), "subs {n}, {n}, #1", "b.ne 2b",
        a = inout(reg) p => _, n = inout(reg) n => _, options(nostack, readonly));
}
unsafe fn ldar_tp(n: u64, p: *mut u8) {
    asm!("2:", ind16!("ldar", "", "", ", [{p}]"), "subs {n}, {n}, #1", "b.ne 2b",
        a0 = out(reg) _, a1 = out(reg) _, a2 = out(reg) _, a3 = out(reg) _, a4 = out(reg) _, a5 = out(reg) _, a6 = out(reg) _, a7 = out(reg) _,
        p = in(reg) p, n = inout(reg) n => _, options(nostack, readonly));
}
unsafe fn stlr_tp(n: u64, p: *mut u8) {
    let p = p.add(1024); // never overwrite the self-pointer at the start of the scratch area
    asm!("2:", dep16!("stlr {b}, [{p}]"), "subs {n}, {n}, #1", "b.ne 2b",
        b = in(reg) 0u64, p = in(reg) p, n = inout(reg) n => _, options(nostack));
}
unsafe fn ldadd_same(n: u64, p: *mut u8) {
    let p = p.add(1024); // never overwrite the self-pointer at the start of the scratch area
    asm!("2:", dep16!("ldadd {b}, {a}, [{p}]"), "subs {n}, {n}, #1", "b.ne 2b",
        a = out(reg) _, b = in(reg) 1u64, p = in(reg) p, n = inout(reg) n => _, options(nostack));
}
unsafe fn ldadd_4lines(n: u64, p: *mut u8) {
    let p = p.add(1024); // never overwrite the self-pointer at the start of the scratch area
    let (p1, p2, p3) = (p.add(64), p.add(128), p.add(192));
    asm!("2:",
        "ldadd {b}, {a0}, [{p0}]\nldadd {b}, {a1}, [{p1}]\nldadd {b}, {a2}, [{p2}]\nldadd {b}, {a3}, [{p3}]\n\
         ldadd {b}, {a0}, [{p0}]\nldadd {b}, {a1}, [{p1}]\nldadd {b}, {a2}, [{p2}]\nldadd {b}, {a3}, [{p3}]\n\
         ldadd {b}, {a0}, [{p0}]\nldadd {b}, {a1}, [{p1}]\nldadd {b}, {a2}, [{p2}]\nldadd {b}, {a3}, [{p3}]\n\
         ldadd {b}, {a0}, [{p0}]\nldadd {b}, {a1}, [{p1}]\nldadd {b}, {a2}, [{p2}]\nldadd {b}, {a3}, [{p3}]",
        "subs {n}, {n}, #1", "b.ne 2b",
        a0 = out(reg) _, a1 = out(reg) _, a2 = out(reg) _, a3 = out(reg) _, b = in(reg) 1u64,
        p0 = in(reg) p, p1 = in(reg) p1, p2 = in(reg) p2, p3 = in(reg) p3, n = inout(reg) n => _, options(nostack));
}

struct Bench {
    name: &'static str,
    kind: &'static str,
    /// Instructions of the measured kind per loop iteration.
    per_iter: f64,
    run: BenchFn,
}

fn benches() -> Vec<Bench> {
    let b = |name, kind, per_iter, run| Bench { name, kind, per_iter, run };
    vec![
        b("add x,x,x", "latency", 16.0, add_lat as BenchFn),
        b("add x,x,x", "throughput", 16.0, add_tp),
        b("mul x,x,x", "latency", 16.0, mul_lat),
        b("mul x,x,x", "throughput", 16.0, mul_tp),
        b("madd x,x,x,x", "latency (accumulator)", 16.0, madd_lat),
        b("madd x,x,x,x", "throughput", 16.0, madd_tp),
        b("sdiv x,x,x (divide by 1)", "latency", 16.0, sdiv_lat),
        b("sdiv x,x,x (0x7fff.. / 3)", "throughput", 16.0, sdiv_tp),
        b("fadd d,d,d", "latency", 16.0, fadd_lat),
        b("fadd d,d,d", "throughput", 16.0, fadd_tp),
        b("fmul d,d,d", "latency", 16.0, fmul_lat),
        b("fmul d,d,d", "throughput", 16.0, fmul_tp),
        b("fmla v.4s (NEON)", "latency (accumulator)", 16.0, fmla_lat),
        b("fmla v.4s (NEON)", "throughput", 16.0, fmla_tp16),
        b("sdot v.4s,v.16b,v.16b", "latency (accumulator)", 16.0, sdot_lat),
        b("sdot v.4s,v.16b,v.16b", "throughput", 16.0, sdot_tp),
        b("ldr x,[x] (L1 hit)", "latency", 16.0, ldr_lat),
        b("ldr x,[x] (L1 hit)", "throughput", 16.0, ldr_tp),
        b("ldp x,x,[x]", "latency", 16.0, ldp_lat),
        b("ldp x,x,[x]", "throughput", 16.0, ldp_tp),
        b("ldp q,q,[x]", "throughput", 16.0, ldpq_tp),
        b("str x,[x]", "throughput", 16.0, str_tp),
        b("stp x,x,[x]", "throughput", 16.0, stp_tp),
        b("dmb ish", "throughput (alone)", 16.0, dmb_tp),
        b("str + dmb ish", "throughput (pairs, per pair)", 16.0, dmb_st_tp),
        b("ldar x,[x]", "latency", 16.0, ldar_lat),
        b("ldar x,[x]", "throughput", 16.0, ldar_tp),
        b("stlr x,[x]", "throughput", 16.0, stlr_tp),
        b("ldadd (LSE), same line", "throughput", 16.0, ldadd_same),
        b("ldadd (LSE), 4 lines", "throughput", 16.0, ldadd_4lines),
    ]
}

fn run_instructions(c: &mut Collector, sess: &Session, csv: &mut String) -> Result<()> {
    let grp = group(&sess.pmu, &["cpu_cycles", "inst_retired"])?;
    // A line-aligned scratch area whose first 16 bytes point to itself (pointer chasing) and are
    // otherwise zero.
    let scratch = Buffer::new(16 << 10)?;
    // SAFETY: the buffer is 16 KiB.
    unsafe { (scratch.as_mut_ptr() as *mut usize).write(scratch.as_ptr() as usize) };
    let p = scratch.as_mut_ptr();
    let list = benches();
    let iters = 1u64 << 15;
    println!("\n== 5.3 instruction latency / reciprocal throughput (cycles per instruction; instructions per loop iteration checked with INST_RETIRED)");
    let _ = writeln!(csv, "instr_header,name,kind,cycles_per_instr,retired_per_instr");
    for b in &list {
        let exp = format!("instr/{}/{}", b.name, b.kind);
        let units = iters as f64 * b.per_iter;
        let sm = c.reps(&exp, sess.repeat, &[metric("cycles_per_instr", "cycles"), metric("retired_per_instr", "events")], || {
            let (_, v) = run_window(&grp, sess.frq, || unsafe { (b.run)(black_box(iters), p) })?;
            Ok(v.iter().map(|&x| x as f64 / units).collect())
        })?;
        let (cy, ret) = (med(&sm, 0), med(&sm, 1));
        let _ = writeln!(csv, "instr,\"{}\",{},{cy:.4},{ret:.4}", b.name, b.kind);
        println!("  {:<28} {:<30} {cy:>8.3} cycles   (retired/instr {ret:.3})", b.name, b.kind);
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------

pub fn run_ooo(sess: &Session) -> Result<()> {
    let mut c = sess.collector("ooo")?;
    let grp = group(&sess.pmu, &["cpu_cycles", "inst_retired", "stall_backend"])?;
    let mut csv = String::from("kind,a,b,c,d,e,f,g,h\n");
    run_instructions(&mut c, sess, &mut csv)?;
    run_mlp(&mut c, &grp, sess, &mut csv)?;
    run_windows(&mut c, &grp, sess, &mut csv)?;
    std::fs::write(sess.day.join("ooo_experiments.csv"), csv)?;
    println!("ooo: {} reps, {} flagged invalid; data in {}", c.total_reps, c.invalid_reps, sess.day.display());
    Ok(())
}
