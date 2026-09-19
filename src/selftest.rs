//! `a76probe selftest`: validates the measurement chain itself before any experiment relies on it.
//!
//! 1. cost / resolution / jitter of both time sources (CNTVCT_EL0, PMU cycle counter via read()),
//! 2. PMU sanity: loops with a known instruction count must give INST_RETIRED ~ N, and cycles
//!    coherent with frequency x time,
//! 3. per-event validation on workloads whose event counts can be derived from the code,
//! 4. how many events fit in one group without multiplexing.

use crate::env::Env;
use crate::harness::{metric, metric_exp, run_window, Collector, Metric};
use crate::output::{self, Jsonl};
use crate::pmu::{Group, Pmu};
use crate::stats::percentile_sorted;
use crate::timing::{cntfrq, cntvct, ticks_to_ns};
use crate::{affinity, guard, kernels, mem::Buffer};
use anyhow::Result;
use serde_json::json;
use std::hint::black_box;
use std::path::PathBuf;

pub struct Opts {
    pub core: usize,
    pub repeat: usize,
    pub out_dir: PathBuf,
}

#[derive(Clone, Copy)]
enum Wl {
    L1,
    Stream,
    Branch,
}

const L1_BYTES: usize = 32 * 1024;
const L1_PASSES: u64 = 2000;
const STREAM_BYTES: usize = 64 << 20;
const STREAM_PASSES: u64 = 2;
const BRANCH_ITERS: u64 = 1_000_000;

fn run_wl(k: Wl, l1: &Buffer, big: &Buffer) {
    match k {
        Wl::L1 => {
            for _ in 0..L1_PASSES {
                // SAFETY: the buffer holds L1_BYTES bytes = 512 lines.
                unsafe { kernels::load_lines(black_box(l1.as_ptr()), (L1_BYTES / 64) as u64) };
            }
        }
        Wl::Stream => {
            for _ in 0..STREAM_PASSES {
                // SAFETY: the buffer holds STREAM_BYTES bytes.
                unsafe { kernels::load_lines(black_box(big.as_ptr()), (STREAM_BYTES / 64) as u64) };
            }
        }
        Wl::Branch => kernels::alu_indep(black_box(BRANCH_ITERS)),
    }
}

/// Event count derived from the code of the workload (loads, loop branches, instructions).
fn expectation(event: &str, k: Wl) -> Metric {
    let loads = match k {
        Wl::L1 => (L1_BYTES as u64 / 64 * L1_PASSES) as f64,
        Wl::Stream => (STREAM_BYTES as u64 / 64 * STREAM_PASSES) as f64,
        Wl::Branch => BRANCH_ITERS as f64,
    };
    let inst = match k {
        Wl::Branch => (kernels::ALU_INDEP_INSTR * BRANCH_ITERS) as f64,
        _ => (kernels::LOAD_LINES_INSTR as f64) * loads,
    };
    let is_load_wl = !matches!(k, Wl::Branch);
    match event {
        "inst_retired" => metric_exp("count", "events", inst, 0.05, 0.0),
        "l1d_cache" | "mem_access" if is_load_wl => metric_exp("count", "events", loads, 0.05, 0.0),
        "br_retired" => metric_exp("count", "events", loads, 0.05, 0.0),
        "br_mis_pred" | "br_mis_pred_retired" => metric_exp("count", "events", 0.0, 0.0, 5000.0),
        "l1d_cache_refill" => match k {
            Wl::L1 => metric_exp("count", "events", 0.0, 0.0, 5000.0),
            Wl::Stream => metric_exp("count", "events", loads, 0.10, 0.0),
            Wl::Branch => metric("count", "events"),
        },
        _ => metric("count", "events"),
    }
}

pub fn run(o: &Opts) -> Result<()> {
    affinity::pin_to(o.core)?;
    let cpu = o.core;
    let n = o.repeat;
    let pmu = Pmu::discover()?;
    let max_khz = guard::max_freq_khz(cpu)?;
    let day = o.out_dir.join(output::today_utc());
    let env = Env::collect();
    std::fs::create_dir_all(&day)?;
    std::fs::write(day.join("env.json"), serde_json::to_string_pretty(&env)?)?;
    let mut c = Collector::new(Jsonl::create(&day.join("raw").join("selftest.jsonl"))?, cpu, max_khz);
    c.note(&json!({"kind": "env", "env": env}))?;
    let frq = cntfrq();
    println!("core {cpu}, expected freq {max_khz} kHz, CNTFRQ_EL0 {frq} Hz, PMU {} (type {}), {n} reps", pmu.device, pmu.type_);

    // Ramp the governor to its top frequency before recording anything.
    kernels::alu_indep(black_box(150_000_000));
    kernels::alu_indep(black_box(150_000_000));

    // --- 1. time sources -------------------------------------------------------------------
    c.reps("clock/cntvct/call_cost", n, &[metric("ns_per_call", "ns")], || {
        let calls = 200_000u64;
        let t0 = cntvct();
        for _ in 0..calls {
            black_box(cntvct());
        }
        let t1 = cntvct();
        Ok(vec![ticks_to_ns(t1 - t0, frq) / calls as f64])
    })?;
    c.reps(
        "clock/cntvct/back_to_back_delta",
        n,
        &[metric("frac_zero", "ratio"), metric("min_nonzero", "ticks"), metric("p99", "ticks"), metric("max", "ticks")],
        || {
            let mut d = Vec::with_capacity(10_000);
            let mut prev = cntvct();
            for _ in 0..10_000 {
                let t = cntvct();
                d.push((t - prev) as f64);
                prev = t;
            }
            let zeros = d.iter().filter(|&&x| x == 0.0).count() as f64 / d.len() as f64;
            let min_nz = d.iter().cloned().filter(|&x| x > 0.0).fold(f64::INFINITY, f64::min);
            d.sort_by(|a, b| a.partial_cmp(b).unwrap());
            Ok(vec![zeros, min_nz, percentile_sorted(&d, 0.99), *d.last().unwrap()])
        },
    )?;

    let cyc = Group::open(&[pmu.event("cpu_cycles")?])?;
    cyc.reset_enable()?;
    c.reps("clock/pmu_cycles/read_syscall_cost", n, &[metric("ns_per_read", "ns")], || {
        let calls = 2000u64;
        let t0 = cntvct();
        for _ in 0..calls {
            black_box(cyc.read_leader());
        }
        let t1 = cntvct();
        Ok(vec![ticks_to_ns(t1 - t0, frq) / calls as f64])
    })?;
    c.reps(
        "clock/pmu_cycles/read_syscall_jitter",
        n,
        &[metric("p50", "ns"), metric("p99", "ns"), metric("max", "ns")],
        || {
            let mut d = Vec::with_capacity(1000);
            for _ in 0..1000 {
                let t0 = cntvct();
                black_box(cyc.read_leader());
                let t1 = cntvct();
                d.push(ticks_to_ns(t1 - t0, frq));
            }
            d.sort_by(|a, b| a.partial_cmp(b).unwrap());
            Ok(vec![percentile_sorted(&d, 0.5), percentile_sorted(&d, 0.99), *d.last().unwrap()])
        },
    )?;
    c.reps("clock/pmu_cycles/reset_enable_disable_cost", n, &[metric("ns_per_window", "ns")], || {
        let calls = 500u64;
        let t0 = cntvct();
        for _ in 0..calls {
            cyc.reset_enable()?;
            cyc.disable()?;
        }
        let t1 = cntvct();
        Ok(vec![ticks_to_ns(t1 - t0, frq) / calls as f64])
    })?;
    drop(cyc);

    // --- 2. PMU sanity on loops with a known instruction count -------------------------------
    let grp = Group::open(&[pmu.event("cpu_cycles")?, pmu.event("inst_retired")?])?;
    let iters = 5_000_000u64;
    let kernels_under_test: [(&str, u64, fn(u64)); 2] = [
        ("alu_indep", kernels::ALU_INDEP_INSTR, kernels::alu_indep),
        ("alu_dep", kernels::ALU_DEP_INSTR, kernels::alu_dep),
    ];
    for (name, per_iter, kernel) in kernels_under_test {
        let expected_inst = (per_iter * iters) as f64;
        c.reps(
            &format!("pmu_selftest/{name}"),
            n,
            &[
                metric_exp("inst_retired_over_expected", "ratio", 1.0, 0.01, 0.0),
                metric_exp("cycles_over_freq_x_time", "ratio", 1.0, 0.02, 0.0),
                metric("cycles_per_iter", "cycles"),
                metric("ipc", "inst/cycle"),
                metric("effective_freq", "GHz"),
            ],
            || {
                let (dt_ns, v) = run_window(&grp, frq, || kernel(black_box(iters)))?;
                let (cycles, inst) = (v[0] as f64, v[1] as f64);
                let freq_hz = max_khz as f64 * 1e3;
                Ok(vec![
                    inst / expected_inst,
                    cycles / (dt_ns * 1e-9 * freq_hz),
                    cycles / iters as f64,
                    inst / cycles,
                    cycles / dt_ns,
                ])
            },
        )?;
    }
    drop(grp);

    // --- 3. per-event validation --------------------------------------------------------------
    let l1 = Buffer::new(L1_BYTES)?;
    let big = Buffer::new(STREAM_BYTES)?;
    c.note(&json!({"kind": "buffers", "l1_locked": l1.locked, "stream_locked": big.locked}))?;
    let events = [
        "inst_retired", "inst_spec", "l1d_cache", "l1d_cache_refill", "l1d_cache_wb", "l1d_tlb",
        "l1d_tlb_refill", "dtlb_walk", "l2d_cache", "l2d_cache_refill", "l2d_cache_wb", "l2d_tlb",
        "l2d_tlb_refill", "l3d_cache", "l3d_cache_refill", "ll_cache_rd", "ll_cache_miss_rd",
        "mem_access", "bus_access", "br_retired", "br_pred", "br_mis_pred", "br_mis_pred_retired",
        "stall_frontend", "stall_backend",
    ];
    for ev in events {
        let group = pmu.event("cpu_cycles").and_then(|cy| pmu.event(ev).map(|e| [cy, e])).and_then(|evs| Group::open(&evs));
        let group = match group {
            Ok(g) => g,
            Err(e) => {
                c.note(&json!({"kind": "event_unavailable", "event": ev, "error": format!("{e:#}")}))?;
                c.rows.push(format!("event_validation/*/{ev:<30} UNAVAILABLE: {e:#}"));
                continue;
            }
        };
        for (wl_name, wl) in [("l1_resident_loads", Wl::L1), ("dram_stream_loads", Wl::Stream), ("branch_loop", Wl::Branch)] {
            let m = expectation(ev, wl);
            c.reps(&format!("event_validation/{wl_name}/{ev}"), n, &[m], || {
                let (_, v) = run_window(&group, frq, || run_wl(wl, &l1, &big))?;
                Ok(vec![v[1] as f64])
            })?;
        }
    }

    // --- 4. group capacity without multiplexing ------------------------------------------------
    let cap_names = [
        "cpu_cycles", "inst_retired", "l1d_cache", "l1d_cache_refill", "l2d_cache", "l2d_cache_refill",
        "br_retired", "br_mis_pred", "mem_access", "inst_spec", "l1d_tlb", "l2d_cache_wb",
    ];
    let avail: Vec<_> = cap_names.iter().filter_map(|nm| pmu.event(nm).ok()).collect();
    let mut max_ok = 0;
    for k in 1..=avail.len() {
        match Group::open(&avail[..k]) {
            Err(e) => {
                c.note(&json!({"kind": "group_capacity", "k": k, "opened": false, "error": format!("{e:#}")}))?;
                break;
            }
            Ok(g) => {
                let r = run_window(&g, frq, || kernels::alu_indep(black_box(2_000_000)));
                let ok = r.is_ok();
                c.note(&json!({"kind": "group_capacity", "k": k, "opened": true, "no_multiplexing": ok,
                    "events": avail[..k].iter().map(|e| e.name.clone()).collect::<Vec<_>>()}))?;
                if ok {
                    max_ok = k;
                } else {
                    break;
                }
            }
        }
    }
    c.rows.push(format!("group_capacity: largest group without multiplexing = {max_ok} events (incl. cpu_cycles)"));

    for r in &c.rows {
        println!("{r}");
    }
    println!(
        "repetitions: {} total, {} flagged invalid (temperature/frequency); data in {}",
        c.total_reps,
        c.invalid_reps,
        day.display()
    );
    Ok(())
}
