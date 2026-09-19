//! Phase 1b: NEON streaming bandwidth (read / write / copy), 1 to 4 cores, from L1-resident to DRAM.
//!
//! Every thread owns a private working set of `size` bytes (copy: `size/2` source + `size/2`
//! destination). Reported bandwidth is traffic (bytes read + bytes written) per second, the STREAM
//! convention, in decimal GB/s. With one thread the PMU cross-checks the timing (cycles, refills,
//! bus accesses); with several threads the shared CNTVCT counter times the run from the first
//! thread's start to the last thread's end.

use crate::harness::{metric, run_window, Metric, Session};
use crate::mem::Buffer;
use crate::pmu::{Group, Pmu};
use crate::stats::Summary;
use crate::timing::{cntvct, ticks_to_ns};
use crate::{affinity, kernels};
use anyhow::{anyhow, Result};
use serde_json::json;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::hint::black_box;
use std::sync::Barrier;

const ROUNDS: usize = 3;
const TARGET_TRAFFIC: usize = 128 << 20;
const OPS: [Op; 3] = [Op::Read, Op::Write, Op::Copy];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Op {
    Read,
    Write,
    Copy,
}

impl Op {
    fn name(self) -> &'static str {
        match self {
            Op::Read => "read",
            Op::Write => "write",
            Op::Copy => "copy",
        }
    }
}

pub fn sizes() -> Vec<usize> {
    let k = 1usize << 10;
    let m = 1usize << 20;
    vec![
        16 * k, 32 * k, 64 * k, 128 * k, 256 * k, 384 * k, 512 * k, m, 3 * m / 2, 2 * m, 4 * m, 8 * m,
        32 * m, 128 * m, 256 * m,
    ]
}

/// Raw addresses of one thread's buffers (usize so they can cross thread boundaries).
#[derive(Clone, Copy)]
struct Work {
    a: usize,
    b: usize,
}

/// # Safety
/// `w` must point to live mappings sized for `op`/`size` (see `alloc`).
unsafe fn run_op(op: Op, w: Work, size: usize, passes: usize) {
    for _ in 0..passes {
        match op {
            Op::Read => kernels::bw_read(black_box(w.a as *const u8), size),
            Op::Write => kernels::bw_write(black_box(w.a as *mut u8), size),
            Op::Copy => kernels::bw_copy(black_box(w.a as *const u8), black_box(w.b as *mut u8), size / 2),
        }
    }
}

fn alloc(op: Op, size: usize) -> Result<(Vec<Buffer>, Work)> {
    match op {
        Op::Copy => {
            let (s, d) = (Buffer::new(size / 2)?, Buffer::new(size / 2)?);
            let w = Work { a: s.as_ptr() as usize, b: d.as_ptr() as usize };
            Ok((vec![s, d], w))
        }
        _ => {
            let b = Buffer::new(size)?;
            let w = Work { a: b.as_ptr() as usize, b: 0 };
            Ok((vec![b], w))
        }
    }
}

/// One timed multi-thread run; returns elapsed ns from the earliest start to the latest end.
fn run_threads(cores: &[usize], op: Op, size: usize, works: &[Work], passes: usize, frq: u64) -> Result<f64> {
    let barrier = Barrier::new(cores.len());
    let times: Vec<Result<(u64, u64)>> = std::thread::scope(|s| {
        let handles: Vec<_> = cores
            .iter()
            .zip(works)
            .map(|(&core, &w)| {
                let barrier = &barrier;
                s.spawn(move || -> Result<(u64, u64)> {
                    affinity::pin_to(core)?;
                    barrier.wait();
                    let t0 = cntvct();
                    // SAFETY: `w` was produced by `alloc` for this op/size and outlives the scope.
                    unsafe { run_op(op, w, size, passes) };
                    let t1 = cntvct();
                    Ok((t0, t1))
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().map_err(|_| anyhow!("worker panicked"))?).collect()
    });
    let mut lo = u64::MAX;
    let mut hi = 0;
    for t in times {
        let (a, b) = t?;
        lo = lo.min(a);
        hi = hi.max(b);
    }
    Ok(ticks_to_ns(hi - lo, frq))
}

fn counter_group(pmu: &Pmu) -> Result<Group> {
    let names = ["cpu_cycles", "l1d_cache_refill", "l2d_cache_refill", "l3d_cache_refill", "bus_access"];
    let evs = names.iter().map(|n| pmu.event(n)).collect::<Result<Vec<_>>>()?;
    Group::open(&evs)
}

fn metrics_single() -> Vec<Metric> {
    vec![
        metric("gbps", "GB/s"),
        metric("bytes_per_cycle", "B/cycle"),
        metric("l1d_refill_per_kib", "events/KiB"),
        metric("l2d_refill_per_kib", "events/KiB"),
        metric("l3d_refill_per_kib", "events/KiB"),
        metric("bus_access_per_kib", "events/KiB"),
    ]
}

fn metrics_multi() -> Vec<Metric> {
    vec![metric("gbps", "GB/s"), metric("gbps_per_thread", "GB/s")]
}

type Pooled = (Vec<Vec<f64>>, usize);

pub fn run_bw(sess: &Session) -> Result<()> {
    let mut c = sess.collector("mem_bw")?;
    let grp = counter_group(&sess.pmu)?;
    let cores: Vec<usize> = (0..4).map(|i| (sess.cpu + i) % 4).collect();
    let per_round = sess.repeat.div_ceil(ROUNDS);
    let sizes = sizes();
    let mut acc: BTreeMap<(usize, &'static str, usize), Pooled> = BTreeMap::new();
    for round in 0..ROUNDS {
        let order: Vec<usize> = if round % 2 == 0 { sizes.clone() } else { sizes.iter().rev().cloned().collect() };
        for nthreads in 1..=4usize {
            let ops: Vec<Op> = (0..3).map(|i| OPS[(i + round) % 3]).collect();
            for &op in &ops {
                for &size in &order {
                    let passes = (TARGET_TRAFFIC / size).max(1);
                    let traffic = (size * passes) as f64;
                    let mut keep = Vec::new();
                    let mut works = Vec::new();
                    for _ in 0..nthreads {
                        let (b, w) = alloc(op, size)?;
                        keep.push(b);
                        works.push(w);
                    }
                    let exp = format!("mem_bw/{}/threads={nthreads}/size={size}", op.name());
                    let tags = json!({"round": round, "op": op.name(), "threads": nthreads, "size_per_thread": size, "passes": passes});
                    c.warmup = if size >= 128 << 20 { 1 } else { 3 };
                    let part = if nthreads == 1 {
                        c.reps_raw(&exp, tags, per_round, &metrics_single(), || {
                            let (dt_ns, v) = run_window(&grp, sess.frq, || {
                                // SAFETY: works[0] comes from alloc(op, size).
                                unsafe { run_op(op, works[0], size, passes) };
                            })?;
                            let kib = traffic / 1024.0;
                            Ok(vec![traffic / dt_ns, traffic / v[0] as f64, v[1] as f64 / kib, v[2] as f64 / kib, v[3] as f64 / kib, v[4] as f64 / kib])
                        })?
                    } else {
                        c.reps_raw(&exp, tags, per_round, &metrics_multi(), || {
                            let dt_ns = run_threads(&cores[..nthreads], op, size, &works, passes, sess.frq)?;
                            let total = traffic * nthreads as f64;
                            Ok(vec![total / dt_ns, total / dt_ns / nthreads as f64])
                        })?
                    };
                    let e = acc.entry((nthreads, op.name(), size)).or_insert_with(|| (vec![Vec::new(); part.0.len()], 0));
                    for (dst, src) in e.0.iter_mut().zip(part.0) {
                        dst.extend(src);
                    }
                    e.1 += part.1;
                }
            }
        }
        println!("bandwidth: round {}/{ROUNDS} done", round + 1);
    }
    let mut csv = String::from("op,threads,size_per_thread_bytes,total_bytes,n_valid,gbps_median,gbps_mad,gbps_ci95_lo,gbps_ci95_hi,gbps_per_thread,bytes_per_cycle,l1d_refill_per_kib,l2d_refill_per_kib,l3d_refill_per_kib,bus_access_per_kib\n");
    println!("{:<6} {:>3} {:>11} {:>9} {:>10} {:>9}", "op", "thr", "size/thr", "GB/s", "MAD", "B/cycle");
    for nthreads in 1..=4usize {
        for op in OPS {
            for &size in &sizes {
                let (vals, total) = &acc[&(nthreads, op.name(), size)];
                let metrics = if nthreads == 1 { metrics_single() } else { metrics_multi() };
                let exp = format!("mem_bw/{}/threads={nthreads}/size={size}", op.name());
                let s: Vec<Option<Summary>> = c.summarize_pooled(&exp, *total, &metrics, vals)?;
                let Some(g) = &s[0] else { continue };
                let m = |i: usize| s.get(i).and_then(|x| x.as_ref()).map_or(String::new(), |x| format!("{:.5}", x.median));
                let _ = writeln!(
                    csv,
                    "{},{nthreads},{size},{},{},{:.4},{:.4},{:.4},{:.4},{},{},{},{},{},{}",
                    op.name(), size * nthreads, g.n, g.median, g.mad, g.ci95_lo, g.ci95_hi,
                    if nthreads == 1 { format!("{:.4}", g.median) } else { m(1) },
                    if nthreads == 1 { m(1) } else { String::new() },
                    if nthreads == 1 { m(2) } else { String::new() },
                    if nthreads == 1 { m(3) } else { String::new() },
                    if nthreads == 1 { m(4) } else { String::new() },
                    if nthreads == 1 { m(5) } else { String::new() },
                );
                println!(
                    "{:<6} {nthreads:>3} {size:>11} {:>9.3} {:>10.4} {:>9}",
                    op.name(), g.median, g.mad, if nthreads == 1 { m(1) } else { String::new() }
                );
            }
        }
    }
    std::fs::write(sess.day.join("mem_bandwidth.csv"), csv)?;
    println!("bandwidth: {} reps, {} flagged invalid; table in {}", c.total_reps, c.invalid_reps, sess.day.join("mem_bandwidth.csv").display());
    Ok(())
}
