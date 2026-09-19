//! Phase 1a: memory latency by pointer chasing.
//!
//! `run_latency`: random single-cycle (Sattolo) chain over 64-byte nodes, working sets from 4 KiB
//! to 1 GiB. `run_tlb`: one node per page (random line inside the page, so cache sets stay evenly
//! used), compared with a packed chain holding the same number of nodes; the difference isolates
//! the address-translation cost. Each measurement is cross-checked with REFILL / TLB counters.

use crate::harness::{metric, run_window, Collector, Metric, Session};
use crate::kernels;
use crate::mem::Buffer;
use crate::perm::sattolo;
use crate::pmu::{Group, Pmu};
use crate::stats::{Rng, Summary};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::hint::black_box;

const LINE: usize = 64;
const MAX_BYTES: usize = 1 << 30;
const ROUNDS: usize = 3;

fn metrics() -> [Metric; 8] {
    [
        metric("ns_per_load", "ns"),
        metric("cycles_per_load", "cycles"),
        metric("l1d_refill_per_load", "events/load"),
        metric("l2d_refill_per_load", "events/load"),
        metric("l3d_refill_per_load", "events/load"),
        metric("ll_miss_rd_per_load", "events/load"),
        metric("l1d_tlb_refill_per_load", "events/load"),
        metric("dtlb_walk_per_load", "events/load"),
    ]
}

/// cpu_cycles + 6 events = 7 counters, the largest group measured not to multiplex.
fn counter_group(pmu: &Pmu) -> Result<Group> {
    let names = [
        "cpu_cycles", "l1d_cache_refill", "l2d_cache_refill", "l3d_cache_refill", "ll_cache_miss_rd",
        "l1d_tlb_refill", "dtlb_walk",
    ];
    let evs = names.iter().map(|n| pmu.event(n)).collect::<Result<Vec<_>>>()?;
    Group::open(&evs)
}

/// Working-set sizes: geometric x1.25 steps plus every power of two, 4 KiB .. 1 GiB.
pub fn sizes() -> Vec<usize> {
    let mut set = BTreeSet::new();
    let mut s = 4096f64;
    while s <= MAX_BYTES as f64 {
        set.insert((s as usize) / LINE * LINE);
        s *= 1.25;
    }
    for p in 12..=30 {
        set.insert(1usize << p);
    }
    set.into_iter().collect()
}

fn loads_for(nodes: usize) -> u64 {
    (nodes.clamp(1 << 20, 1 << 21) / 16 * 16) as u64
}

/// Node `i` lives at `base + offset(i)` and holds the address of node `next[i]`.
///
/// # Safety
/// Every `offset(i)` (i < next.len()) plus 8 bytes must lie inside the mapping at `base`.
unsafe fn write_chain(base: *mut u8, next: &[u32], offset: impl Fn(usize) -> usize) {
    for (i, &succ) in next.iter().enumerate() {
        let target = base.add(offset(succ as usize)) as *const u8;
        (base.add(offset(i)) as *mut *const u8).write(target);
    }
}

fn chase_reps(
    c: &mut Collector,
    grp: &Group,
    frq: u64,
    exp: &str,
    tags: Value,
    n: usize,
    start: *const u8,
    loads: u64,
) -> Result<(Vec<Vec<f64>>, usize)> {
    let mut p = start;
    c.reps_raw(exp, tags, n, &metrics(), || {
        let p0 = p;
        let mut pn = p0;
        let (dt_ns, v) = run_window(grp, frq, || {
            // SAFETY: p0 is a node of a fully built chain inside a live Buffer.
            pn = unsafe { kernels::chase(black_box(p0), loads) };
        })?;
        p = black_box(pn);
        let l = loads as f64;
        Ok(vec![dt_ns / l, v[0] as f64 / l, v[1] as f64 / l, v[2] as f64 / l, v[3] as f64 / l, v[4] as f64 / l, v[5] as f64 / l, v[6] as f64 / l])
    })
}

type Pooled = (Vec<Vec<f64>>, usize);

fn pool(acc: &mut BTreeMap<String, Pooled>, key: &str, part: Pooled) {
    let e = acc.entry(key.to_string()).or_insert_with(|| (vec![Vec::new(); 8], 0));
    for (dst, src) in e.0.iter_mut().zip(part.0) {
        dst.extend(src);
    }
    e.1 += part.1;
}

fn summarize(c: &mut Collector, exp: &str, p: &Pooled) -> Result<Vec<Option<Summary>>> {
    c.summarize_pooled(exp, p.1, &metrics(), &p.0)
}

fn med(s: &[Option<Summary>], i: usize) -> f64 {
    s[i].as_ref().map_or(f64::NAN, |x| x.median)
}

pub fn run_latency(sess: &Session) -> Result<()> {
    let mut c = sess.collector("mem_lat")?;
    let grp = counter_group(&sess.pmu)?;
    let buf = Buffer::new(MAX_BYTES)?;
    c.note(&json!({"kind": "buffer", "bytes": MAX_BYTES, "mlocked": buf.locked, "rounds": ROUNDS}))?;
    let sizes = sizes();
    let per_round = sess.repeat.div_ceil(ROUNDS);
    let mut acc: BTreeMap<String, Pooled> = BTreeMap::new();
    for round in 0..ROUNDS {
        let order: Vec<usize> = if round % 2 == 0 { sizes.clone() } else { sizes.iter().rev().cloned().collect() };
        for &size in &order {
            let nodes = size / LINE;
            let next = sattolo(nodes, &mut Rng::new(size as u64 ^ (round as u64 + 1) * 0x9E37_79B9));
            // SAFETY: offsets i*64 + 8 <= size <= MAX_BYTES.
            unsafe { write_chain(buf.as_mut_ptr(), &next, |i| i * LINE) };
            drop(next);
            c.warmup = if size >= 32 << 20 { 1 } else { 3 };
            let exp = format!("mem_lat/size={size}");
            let part = chase_reps(&mut c, &grp, sess.frq, &exp, json!({"round": round, "size": size}), per_round, buf.as_ptr(), loads_for(nodes))?;
            pool(&mut acc, &exp, part);
        }
        println!("latency: round {}/{ROUNDS} done", round + 1);
    }
    let mut csv = String::from("size_bytes,nodes,n_valid,ns_median,ns_mad,ns_ci95_lo,ns_ci95_hi,cycles_median,l1d_refill_per_load,l2d_refill_per_load,l3d_refill_per_load,ll_miss_rd_per_load,l1d_tlb_refill_per_load,dtlb_walk_per_load\n");
    println!("{:>12} {:>9} {:>9} {:>9} {:>8} {:>8} {:>8} {:>8}", "size", "ns/load", "cyc/load", "L1refill", "L2refill", "L3refill", "TLBrefil", "walk");
    for &size in &sizes {
        let exp = format!("mem_lat/size={size}");
        let s = summarize(&mut c, &exp, &acc[&exp])?;
        let Some(ns) = &s[0] else { continue };
        let _ = writeln!(
            csv,
            "{size},{},{},{:.4},{:.4},{:.4},{:.4},{:.3},{:.5},{:.5},{:.5},{:.5},{:.5},{:.5}",
            size / LINE, ns.n, ns.median, ns.mad, ns.ci95_lo, ns.ci95_hi, med(&s, 1), med(&s, 2), med(&s, 3), med(&s, 4), med(&s, 5), med(&s, 6), med(&s, 7)
        );
        println!(
            "{size:>12} {:>9.3} {:>9.2} {:>9.4} {:>8.4} {:>8.4} {:>8.4} {:>8.4}",
            ns.median, med(&s, 1), med(&s, 2), med(&s, 3), med(&s, 4), med(&s, 6), med(&s, 7)
        );
    }
    std::fs::write(sess.day.join("mem_latency.csv"), csv)?;
    println!("latency: {} reps, {} flagged invalid; curve in {}", c.total_reps, c.invalid_reps, sess.day.join("mem_latency.csv").display());
    Ok(())
}

pub fn run_tlb(sess: &Session) -> Result<()> {
    // SAFETY: sysconf(_SC_PAGESIZE) has no memory effects.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let mut c = sess.collector("tlb_lat")?;
    let grp = counter_group(&sess.pmu)?;
    let max_pages = MAX_BYTES / page;
    let buf = Buffer::new(max_pages * page)?;
    c.note(&json!({"kind": "buffer", "bytes": max_pages * page, "page_size": page, "mlocked": buf.locked, "rounds": ROUNDS}))?;
    let mut pages_list = BTreeSet::new();
    let mut p = 4usize;
    while p <= max_pages {
        pages_list.insert(p);
        pages_list.insert(p / 2 * 3);
        p *= 2;
    }
    // Fine steps around the two thresholds seen in a coarse pass (48..64 and 1024..1536 pages).
    pages_list.extend([40, 44, 52, 56, 60, 1152, 1280, 1344, 1408]);
    pages_list.retain(|&p| p >= 4 && p <= max_pages);
    let pages_list: Vec<usize> = pages_list.into_iter().collect();
    let per_round = sess.repeat.div_ceil(ROUNDS);
    let mut acc: BTreeMap<String, Pooled> = BTreeMap::new();
    for round in 0..ROUNDS {
        let order: Vec<usize> = if round % 2 == 0 { pages_list.clone() } else { pages_list.iter().rev().cloned().collect() };
        for &pages in &order {
            let conds: [&str; 2] = if round % 2 == 0 { ["paged", "packed"] } else { ["packed", "paged"] };
            for cond in conds {
                let mut rng = Rng::new(pages as u64 * 31 + round as u64);
                let next = sattolo(pages, &mut rng);
                let start;
                if cond == "paged" {
                    let lines_per_page = page / LINE;
                    let line_of: Vec<usize> = (0..pages).map(|_| rng.below(lines_per_page as u64) as usize).collect();
                    // SAFETY: offset i*page + line*64 + 8 <= pages*page <= buffer size.
                    unsafe { write_chain(buf.as_mut_ptr(), &next, |i| i * page + line_of[i] * LINE) };
                    start = unsafe { buf.as_ptr().add(line_of[0] * LINE) };
                } else {
                    // SAFETY: offsets i*64 + 8 <= pages*64 <= buffer size.
                    unsafe { write_chain(buf.as_mut_ptr(), &next, |i| i * LINE) };
                    start = buf.as_ptr();
                }
                let exp = format!("tlb/{cond}/pages={pages}");
                c.warmup = 2;
                let part = chase_reps(&mut c, &grp, sess.frq, &exp, json!({"round": round, "pages": pages, "cond": cond}), per_round, start, loads_for(pages))?;
                pool(&mut acc, &exp, part);
            }
        }
        println!("tlb: round {}/{ROUNDS} done", round + 1);
    }
    let mut csv = String::from("pages,span_bytes,n_valid_paged,ns_paged,ns_paged_ci95_lo,ns_paged_ci95_hi,ns_packed,ns_packed_ci95_lo,ns_packed_ci95_hi,delta_ns,l1d_tlb_refill_per_load_paged,dtlb_walk_per_load_paged,l1d_tlb_refill_per_load_packed,l1d_refill_per_load_paged,l1d_refill_per_load_packed\n");
    println!("{:>8} {:>10} {:>10} {:>9} {:>10} {:>9}", "pages", "paged ns", "packed ns", "delta", "TLB refill", "walk");
    for &pages in &pages_list {
        let sp = summarize(&mut c, &format!("tlb/paged/pages={pages}"), &acc[&format!("tlb/paged/pages={pages}")])?;
        let sk = summarize(&mut c, &format!("tlb/packed/pages={pages}"), &acc[&format!("tlb/packed/pages={pages}")])?;
        let (Some(a), Some(b)) = (&sp[0], &sk[0]) else { continue };
        let _ = writeln!(
            csv,
            "{pages},{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.5},{:.5},{:.5},{:.5},{:.5}",
            pages * page, a.n, a.median, a.ci95_lo, a.ci95_hi, b.median, b.ci95_lo, b.ci95_hi, a.median - b.median,
            med(&sp, 6), med(&sp, 7), med(&sk, 6), med(&sp, 2), med(&sk, 2)
        );
        println!("{pages:>8} {:>10.3} {:>10.3} {:>9.3} {:>10.4} {:>9.4}", a.median, b.median, a.median - b.median, med(&sp, 6), med(&sp, 7));
    }
    std::fs::write(sess.day.join("tlb_latency.csv"), csv)?;
    println!("tlb: {} reps, {} flagged invalid; curve in {}", c.total_reps, c.invalid_reps, sess.day.join("tlb_latency.csv").display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_cover_range_and_are_line_multiples() {
        let s = sizes();
        assert_eq!(s[0], 4096);
        assert_eq!(*s.last().unwrap(), MAX_BYTES);
        assert!(s.iter().all(|x| x % LINE == 0));
        assert!(s.windows(2).all(|w| w[0] < w[1]));
        for p in [1 << 16, 1 << 19, 1 << 21] {
            assert!(s.contains(&p));
        }
    }

    #[test]
    fn loads_are_multiples_of_16() {
        for n in [64, 1000, 1 << 22] {
            assert_eq!(loads_for(n) % 16, 0);
            assert!(loads_for(n) >= (1u64 << 20) - 16);
        }
    }
}
