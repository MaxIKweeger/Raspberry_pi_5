//! Phase 2: cache geometry and replacement policy, using physical-address-controlled lines.
//!
//! 1. line size: strided chains, refills per load = stride / line size;
//! 2. associativity: K lines mapping to one set, cyclic chase, refill rate vs K (jump at W+1);
//! 3. index bits: for a group that overflows one set, flip a single physical address bit in half
//!    of the lines; if the overflow disappears, that bit takes part in set selection;
//! 4. replacement: W+1..W+3 lines in one set, several access patterns, refill rate compared with
//!    software models (LRU, tree-PLRU, FIFO, random, SRRIP, BRRIP) replaying the same trace;
//! 5. L1/L2 inclusion: a hot line that never reaches L2 while its L2 set is thrashed.

use crate::analysis;
use crate::harness::{metric, run_window, Collector, Metric, Session};
use crate::lineset::{self, Choice, PAGE_SHIFT};
use crate::mem::Buffer;
use crate::perm::sattolo;
use crate::phys::PhysPool;
use crate::pmu::{Group, Pmu};
use crate::sim::{self, Kind};
use crate::stats::{Rng, Summary};
use crate::kernels;
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::hint::black_box;

const ROUNDS: usize = 3;
const POOL_BYTES: usize = 2 << 30;
/// In-page offset of every target line: L1 set 192, clear of the trace page (offsets 0..8191).
const OFFSET: usize = 12288;
const LINE: usize = 64;
const N_EVICT: usize = 12;
const PASSES: usize = 16;

struct LevelCfg {
    name: &'static str,
    /// Index of this level's refill counter among the values of `chain_group` (after cpu_cycles).
    slot: usize,
    k_max: usize,
    loads: u64,
    sysfs_ways: usize,
    sysfs_bytes: usize,
}

const LEVELS: [LevelCfg; 3] = [
    LevelCfg { name: "L1D", slot: 1, k_max: 10, loads: 1 << 20, sysfs_ways: 4, sysfs_bytes: 64 << 10 },
    LevelCfg { name: "L2", slot: 2, k_max: 16, loads: 1 << 20, sysfs_ways: 8, sysfs_bytes: 512 << 10 },
    LevelCfg { name: "L3", slot: 3, k_max: 44, loads: 1 << 19, sysfs_ways: 16, sysfs_bytes: 2 << 20 },
];

fn chain_group(pmu: &Pmu) -> Result<Group> {
    let names = ["cpu_cycles", "l1d_cache_refill", "l2d_cache_refill", "l3d_cache_refill", "ll_cache_miss_rd"];
    Group::open(&names.iter().map(|n| pmu.event(n)).collect::<Result<Vec<_>>>()?)
}

fn chain_metrics() -> [Metric; 6] {
    [
        metric("ns_per_load", "ns"),
        metric("cycles_per_load", "cycles"),
        metric("l1d_refill_per_load", "events/load"),
        metric("l2d_refill_per_load", "events/load"),
        metric("l3d_refill_per_load", "events/load"),
        metric("ll_miss_rd_per_load", "events/load"),
    ]
}

type Pooled = (Vec<Vec<f64>>, usize);

fn pool_into(acc: &mut BTreeMap<String, Pooled>, key: &str, part: Pooled) {
    let e = acc.entry(key.to_string()).or_insert_with(|| (vec![Vec::new(); part.0.len()], 0));
    for (dst, src) in e.0.iter_mut().zip(part.0) {
        dst.extend(src);
    }
    e.1 += part.1;
}

/// Writes a random single-cycle chain through `lines` (each line's first 8 bytes = next address).
///
/// # Safety
/// Every address in `lines` must be writable for 8 bytes.
unsafe fn write_line_chain(lines: &[usize], seed: u64) -> *const u8 {
    let next = sattolo(lines.len(), &mut Rng::new(seed));
    for (i, &n) in next.iter().enumerate() {
        (lines[i] as *mut usize).write_volatile(lines[n as usize]);
    }
    lines[0] as *const u8
}

/// # Safety
/// Every address in `lines` must be writable for 8 bytes.
unsafe fn zero_lines(lines: &[usize]) {
    for &l in lines {
        (l as *mut u64).write_volatile(0);
    }
}

#[allow(clippy::too_many_arguments)]
fn chase_reps(
    c: &mut Collector,
    grp: &Group,
    frq: u64,
    exp: &str,
    tags: Value,
    n: usize,
    start: *const u8,
    loads: u64,
) -> Result<Pooled> {
    let mut p = start;
    c.reps_raw(exp, tags, n, &chain_metrics(), || {
        let p0 = p;
        let mut pn = p0;
        let (dt, v) = run_window(grp, frq, || {
            // SAFETY: p0 is a node of a chain written by write_line_chain over live pool memory.
            pn = unsafe { kernels::chase(black_box(p0), loads) };
        })?;
        p = black_box(pn);
        let l = loads as f64;
        Ok(vec![dt / l, v[0] as f64 / l, v[1] as f64 / l, v[2] as f64 / l, v[3] as f64 / l, v[4] as f64 / l])
    })
}

fn summarize(c: &mut Collector, exp: &str, p: &Pooled) -> Result<Vec<Option<Summary>>> {
    c.summarize_pooled(exp, p.1, &chain_metrics(), &p.0)
}

fn med(s: &Option<Summary>) -> f64 {
    s.as_ref().map_or(f64::NAN, |x| x.median)
}

// ---------------------------------------------------------------------------------------------
// 1. line size

fn run_line_size(c: &mut Collector, sess: &Session, csv: &mut String) -> Result<()> {
    let grp = chain_group(&sess.pmu)?;
    let per_round = sess.repeat.div_ceil(ROUNDS);
    println!("\n== line size: refills per load on strided chains (line order random, elements ascending inside a line)");
    for (lvl, bytes) in [(0usize, 256usize << 10), (1, 1 << 20)] {
        let buf = Buffer::new(bytes)?;
        let slot = LEVELS[lvl].slot;
        let mut acc: BTreeMap<String, Pooled> = BTreeMap::new();
        let strides = [8usize, 16, 32, 64, 128];
        for round in 0..ROUNDS {
            let order: Vec<usize> = if round % 2 == 0 { strides.to_vec() } else { strides.iter().rev().cloned().collect() };
            for &s in &order {
                let lines = bytes / LINE;
                let lperm = sattolo(lines, &mut Rng::new(s as u64 + round as u64));
                // Visit lines in the order of the cycle, elements ascending inside each line.
                let mut order_addr: Vec<usize> = Vec::new();
                let mut cur = 0usize;
                for _ in 0..lines {
                    let base = buf.as_ptr() as usize + cur * LINE;
                    let per_line = if s < LINE { LINE / s } else { 1 };
                    for e in 0..per_line {
                        order_addr.push(base + e * s);
                    }
                    cur = lperm[cur] as usize;
                }
                // SAFETY: all addresses lie inside `buf`.
                let start = unsafe {
                    for (i, &a) in order_addr.iter().enumerate() {
                        (a as *mut usize).write_volatile(order_addr[(i + 1) % order_addr.len()]);
                    }
                    order_addr[0] as *const u8
                };
                let exp = format!("linesize/{}/stride={s}", LEVELS[lvl].name);
                let part = chase_reps(c, &grp, sess.frq, &exp, json!({"round": round, "stride": s}), per_round, start, 1 << 20)?;
                pool_into(&mut acc, &exp, part);
            }
        }
        let mut rates = Vec::new();
        for &s in &strides {
            let exp = format!("linesize/{}/stride={s}", LEVELS[lvl].name);
            let sm = summarize(c, &exp, &acc[&exp])?;
            rates.push(med(&sm[slot + 1]));
        }
        let plateau = *rates.last().unwrap();
        for (&s, &r) in strides.iter().zip(&rates) {
            let expect = if s < LINE { s as f64 / LINE as f64 } else { 1.0 };
            println!("  {:<3} stride {s:>3} B: {r:.4} refills/load, {:.4} of the stride-128 plateau (line = 64 B predicts {expect:.4})", LEVELS[lvl].name, r / plateau);
            let _ = writeln!(csv, "linesize,{},{s},{r:.5},{expect:.5},{:.5}", LEVELS[lvl].name, r / plateau);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// 2 + 3. associativity and index bits

#[derive(Debug, Clone)]
struct Geo {
    level: &'static str,
    ways: Option<usize>,
    onset: Option<usize>,
    index_bits: Vec<u32>,
    tested_hi: u32,
}

impl Geo {
    fn sets(&self) -> usize {
        1usize << self.index_bits.len()
    }
    fn contiguous(&self) -> bool {
        self.index_bits.windows(2).all(|w| w[1] == w[0] + 1)
    }
}

fn run_assoc(c: &mut Collector, sess: &Session, grp: &Group, pool: &PhysPool, class: &[usize], lvl: &LevelCfg, csv: &mut String) -> Result<(Option<usize>, Option<usize>, Vec<f64>)> {
    let per_round = sess.repeat.div_ceil(ROUNDS);
    let mut acc: BTreeMap<String, Pooled> = BTreeMap::new();
    let ks: Vec<usize> = (1..=lvl.k_max).collect();
    for round in 0..ROUNDS {
        let order: Vec<usize> = if round % 2 == 0 { ks.clone() } else { ks.iter().rev().cloned().collect() };
        for &k in &order {
            let lines = lineset::same_set_lines(&pool.pages, class, OFFSET, k);
            // SAFETY: lines lie in the pool buffer.
            let start = unsafe { write_line_chain(&lines, k as u64 * 77 + round as u64) };
            let exp = format!("assoc/{}/k={k}", lvl.name);
            let part = chase_reps(c, grp, sess.frq, &exp, json!({"round": round, "k": k}), per_round, start, lvl.loads)?;
            pool_into(&mut acc, &exp, part);
        }
    }
    let mut rates = Vec::new();
    let (mut ways, mut onset, mut k50): (Option<usize>, Option<usize>, Option<usize>) = (None, None, None);
    println!("\n== associativity {}: {} refills per load vs K lines in one set", lvl.name, ["L1D", "L2D", "L3D"][lvl.slot - 1]);
    for &k in &ks {
        let exp = format!("assoc/{}/k={k}", lvl.name);
        let sm = summarize(c, &exp, &acc[&exp])?;
        let r = med(&sm[lvl.slot + 1]);
        let (lo, hi) = sm[lvl.slot + 1].as_ref().map_or((f64::NAN, f64::NAN), |s| (s.ci95_lo, s.ci95_hi));
        rates.push(r);
        let _ = writeln!(csv, "assoc,{},{k},{r:.5},{lo:.5},{hi:.5},{:.3}", lvl.name, med(&sm[1]));
        println!("  K={k:>2}  {r:.4}  [{lo:.4}, {hi:.4}]  {:.1} cycles/load", med(&sm[1]));
        if onset.is_none() && r >= 0.05 {
            onset = Some(k);
            ways = Some(k - 1);
        }
        if k50.is_none() && r >= 0.5 {
            k50 = Some(k);
        }
    }
    println!("  conflict capacity = onset - 1 = {ways:?} lines (first K with >= 5 % refills: {onset:?}; first K with >= 50 %: {k50:?})");
    c.note(&json!({"kind": "assoc_summary", "level": lvl.name, "capacity": ways, "onset_k": onset, "k50": k50}))?;
    Ok((ways, onset, rates))
}

fn run_flip(c: &mut Collector, sess: &Session, grp: &Group, pool: &PhysPool, choice: &Choice, lvl: &LevelCfg, ways: usize, csv: &mut String) -> Result<Vec<u32>> {
    let per_round = sess.repeat.div_ceil(ROUNDS);
    let k = ways + ways.div_ceil(2);
    let bits: Vec<u32> = (6..=choice.hi).collect();
    let mut acc: BTreeMap<String, Pooled> = BTreeMap::new();
    let mut variants: Vec<Option<u32>> = vec![None];
    variants.extend(bits.iter().map(|&b| Some(b)));
    for round in 0..ROUNDS {
        let order: Vec<Option<u32>> = if round % 2 == 0 { variants.clone() } else { variants.iter().rev().cloned().collect() };
        for &v in &order {
            let lines = match v {
                None => lineset::same_set_lines(&pool.pages, &choice.base, OFFSET, k),
                Some(b) if b < PAGE_SHIFT => lineset::flipped_lines(&pool.pages, &choice.base, &choice.base, OFFSET, k, b),
                Some(b) => {
                    let other = &choice.flipped.iter().find(|(bb, _)| *bb == b).ok_or_else(|| anyhow!("no class for bit {b}"))?.1;
                    lineset::flipped_lines(&pool.pages, &choice.base, other, OFFSET, k, b)
                }
            };
            // SAFETY: lines lie in the pool buffer.
            let start = unsafe { write_line_chain(&lines, k as u64 * 131 + round as u64) };
            let tag = v.map_or("none".to_string(), |b| b.to_string());
            let exp = format!("flip/{}/bit={tag}", lvl.name);
            let part = chase_reps(c, grp, sess.frq, &exp, json!({"round": round, "k": k, "bit": tag}), per_round, start, lvl.loads)?;
            pool_into(&mut acc, &exp, part);
        }
    }
    let base_exp = format!("flip/{}/bit=none", lvl.name);
    let base = summarize(c, &base_exp, &acc[&base_exp])?;
    let base_rate = med(&base[lvl.slot + 1]);
    println!("\n== index bits {}: K = {k} lines (W* = {ways}); baseline (all bits equal) {base_rate:.3} refills/load", lvl.name);
    let mut index_bits = Vec::new();
    for &b in &bits {
        let exp = format!("flip/{}/bit={b}", lvl.name);
        let sm = summarize(c, &exp, &acc[&exp])?;
        let r = med(&sm[lvl.slot + 1]);
        let is_index = r < 0.5 * base_rate;
        if is_index {
            index_bits.push(b);
        }
        let _ = writeln!(csv, "flip,{},{b},{r:.5},{base_rate:.5},{}", lvl.name, is_index as u8);
        println!("  flip bit {b:>2}: {r:.4}  {}", if is_index { "<- set-index bit (overflow disappears)" } else { "" });
    }
    Ok(index_bits)
}

// ---------------------------------------------------------------------------------------------
// 4 + 5. replacement policy and inclusion

struct TraceRig {
    page: Buffer,
    table_ptr: *const usize,
    evict_ptr: *const usize,
}

impl TraceRig {
    /// Layout of the 16 KiB page: trace 0..8191, line table at 8192, evictor table at 8448.
    fn new(targets: &[usize], evictors: &[usize]) -> Result<TraceRig> {
        let page = Buffer::new(16 << 10)?;
        let base = page.as_mut_ptr();
        // SAFETY: table regions are inside the 16 KiB page (8192 + 8*targets, 8448 + 8*evictors).
        unsafe {
            let table = base.add(8192) as *mut usize;
            for (i, &t) in targets.iter().enumerate() {
                table.add(i).write(t);
            }
            let ev = base.add(8448) as *mut usize;
            for (i, &e) in evictors.iter().enumerate() {
                ev.add(i).write(e);
            }
            zero_lines(targets);
            zero_lines(evictors);
        }
        Ok(TraceRig { page, table_ptr: unsafe { base.add(8192) as *const usize }, evict_ptr: unsafe { base.add(8448) as *const usize } })
    }

    fn load_trace(&self, trace: &[u16]) {
        assert_eq!(trace.len(), sim::TRACE_LEN);
        // SAFETY: the trace area is 8192 bytes = TRACE_LEN u16 at the start of the page.
        unsafe { std::ptr::copy_nonoverlapping(trace.as_ptr(), self.page.as_mut_ptr() as *mut u16, trace.len()) };
    }

    fn trace_ptr(&self) -> *const u16 {
        self.page.as_ptr() as *const u16
    }
}

fn trace_metrics() -> [Metric; 3] {
    [metric("l1d_refill_per_access", "events"), metric("l2d_refill_per_access", "events"), metric("cycles_per_access", "cycles")]
}

fn measure_traces(
    c: &mut Collector,
    sess: &Session,
    grp: &Group,
    rig: &TraceRig,
    exp: &str,
    tags: Value,
    trace: &[u16],
    evict: bool,
) -> Result<Pooled> {
    rig.load_trace(trace);
    let accesses = (PASSES * sim::TRACE_LEN) as f64;
    let (tp, tab, ev) = (rig.trace_ptr(), rig.table_ptr, rig.evict_ptr);
    let n = sess.repeat.div_ceil(ROUNDS);
    c.reps_raw(exp, tags, n, &trace_metrics(), || {
        let (_, v) = run_window(grp, sess.frq, || {
            for _ in 0..PASSES {
                // SAFETY: rig tables/trace are live and target lines are zero-filled.
                unsafe {
                    if evict {
                        kernels::trace_replay_evict(black_box(tp), sim::TRACE_LEN as u64, tab, ev, 2 * N_EVICT as u64);
                    } else {
                        kernels::trace_replay(black_box(tp), sim::TRACE_LEN as u64, tab);
                    }
                }
            }
        })?;
        Ok(vec![v[1] as f64 / accesses, v[2] as f64 / accesses, v[0] as f64 / accesses])
    })
}

/// `n` lines at in-page offset OFFSET (same L1 set as the targets) from pages whose L2-index bits
/// above the page offset (`mask`) differ from the target's, spread evenly over the other colors,
/// so that they land in other L2 sets with at most ceil(n / other colors) lines each.
fn evictor_lines(pool: &PhysPool, mask: u64, target_color: u64, n: usize) -> Result<Vec<usize>> {
    let ncolors = 1usize << mask.count_ones();
    let per_color = n.div_ceil(ncolors - 1);
    let mut per: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    for p in &pool.pages {
        let col = p.pbase & mask;
        if col == target_color {
            continue;
        }
        let e = per.entry(col).or_default();
        if e.len() < per_color {
            e.push(p.vbase + OFFSET);
        }
        if per.len() == ncolors - 1 && per.values().all(|v| v.len() == per_color) {
            break;
        }
    }
    let lines: Vec<usize> = per.into_values().flatten().take(n).collect();
    if lines.len() < n {
        bail!("only {} evictor lines found, {n} needed", lines.len());
    }
    Ok(lines)
}

pub fn print_scores(level: &str, pats: &[sim::Pattern], values: &[Vec<f64>], scores: &[analysis::Score]) {
    println!("  {level}: per-repetition steady states (0.01 bins) of each pattern:");
    for (p, vals) in pats.iter().zip(values) {
        let mut hist: BTreeMap<i64, usize> = BTreeMap::new();
        for v in vals {
            *hist.entry((v * 100.0).round() as i64).or_default() += 1;
        }
        let h: Vec<String> = hist.iter().map(|(b, c)| format!("{:.2}x{c}", *b as f64 / 100.0)).collect();
        println!("    {:<20} [{}]", p.name, h.join(" "));
    }
    println!("  mean distance to the nearest state reachable by each model (lower is better; coverage = share of reps within 0.012):");
    for s in scores {
        let worst = s.per_pattern_dist.iter().cloned().enumerate().fold((0, 0.0), |a, (i, d)| if d > a.1 { (i, d) } else { a });
        println!("    {:<9} distance {:.4}  coverage {:.2}  worst pattern: {} ({:.3})", s.kind.name(), s.mean_dist, s.coverage, pats[worst.0].name, worst.1);
    }
    println!("  -> closest: {} (confidence {})", scores[0].kind.name(), analysis::confidence(scores));
}

struct Policy {
    level: &'static str,
    ranking: Vec<(Kind, f64)>,
    confidence: &'static str,
}

fn run_replacement(
    c: &mut Collector,
    sess: &Session,
    pool: &PhysPool,
    choice: &Choice,
    level_idx: usize,
    ways: usize,
    l2_color_mask: u64,
    csv: &mut String,
) -> Result<Option<Policy>> {
    let lvl = &LEVELS[level_idx];
    let pats = sim::patterns(ways);
    let max_lines = pats.iter().map(|p| p.lines).max().unwrap();
    if choice.base.len() < max_lines {
        bail!("class too small for {max_lines} lines");
    }
    let targets = lineset::same_set_lines(&pool.pages, &choice.base, OFFSET, max_lines);
    let mut evictors: Vec<usize> = Vec::new();
    if level_idx == 1 {
        if l2_color_mask == 0 {
            bail!("no L2 index bit >= {PAGE_SHIFT} found: cannot build evictor lines in other L2 sets");
        }
        let target_color = pool.pages[choice.base[0]].pbase & l2_color_mask;
        let lines = evictor_lines(pool, l2_color_mask, target_color, N_EVICT)?;
        evictors = lines.iter().chain(lines.iter()).cloned().collect();
    }
    let rig = TraceRig::new(&targets, &evictors)?;
    let grp = Group::open(&["cpu_cycles", "l1d_cache_refill", "l2d_cache_refill"].iter().map(|n| sess.pmu.event(n)).collect::<Result<Vec<_>>>()?)?;
    let use_evict = level_idx == 1;
    let rate_idx = if level_idx == 0 { 0 } else { 1 };
    let mut all: Vec<(String, Vec<u16>)> = pats.iter().map(|p| (p.name.to_string(), p.trace.clone())).collect();
    all.push((format!("control: {ways} lines cyclic (fits)"), (0..sim::TRACE_LEN).map(|i| (i % ways) as u16).collect()));
    let mut acc: BTreeMap<String, Pooled> = BTreeMap::new();
    for round in 0..ROUNDS {
        let order: Vec<usize> = if round % 2 == 0 { (0..all.len()).collect() } else { (0..all.len()).rev().collect() };
        for &pi in &order {
            let exp = format!("repl/{}/{}", lvl.name, all[pi].0);
            let part = measure_traces(c, sess, &grp, &rig, &exp, json!({"round": round, "pattern": all[pi].0}), &all[pi].1, use_evict)?;
            pool_into(&mut acc, &exp, part);
        }
    }
    println!("\n== replacement {}: W = {ways}, evictors {}, refill rate per access at this level", lvl.name, use_evict);
    let mut measured = Vec::new();
    for (name, _) in &all {
        let exp = format!("repl/{}/{name}", lvl.name);
        let (vals, total) = &acc[&exp];
        let sm = c.summarize_pooled(&exp, *total, &trace_metrics(), vals)?;
        let m = med(&sm[rate_idx]);
        let (lo, hi) = sm[rate_idx].as_ref().map_or((f64::NAN, f64::NAN), |s| (s.ci95_lo, s.ci95_hi));
        measured.push((name.clone(), m, lo, hi));
    }
    let kinds: Vec<Kind> = Kind::ALL.iter().cloned().filter(|k| *k != Kind::TreePlru || ways.is_power_of_two()).collect();
    let sims: Vec<(Kind, Vec<f64>)> = kinds.iter().map(|&k| (k, pats.iter().map(|p| sim::expected_miss_rate(k, ways, &p.trace)).collect())).collect();
    let _ = write!(csv, "repl_header,{},pattern,measured,ci_lo,ci_hi", lvl.name);
    for (k, _) in &sims {
        let _ = write!(csv, ",{}", k.name());
    }
    csv.push('\n');
    for (i, (name, m, lo, hi)) in measured.iter().enumerate() {
        let _ = write!(csv, "repl,{},{name},{m:.5},{lo:.5},{hi:.5}", lvl.name);
        print!("  {name:<34} measured {m:.4}  ");
        for (k, s) in &sims {
            if i < pats.len() {
                let _ = write!(csv, ",{:.5}", s[i]);
                print!("{} {:.3}  ", k.name(), s[i]);
            } else {
                let _ = write!(csv, ",");
            }
        }
        csv.push('\n');
        println!();
    }
    // Score with the shared offline analysis (random initial states, nearest reachable state).
    let per_pat_vals: Vec<Vec<f64>> = pats.iter().map(|p| acc[&format!("repl/{}/{}", lvl.name, p.name)].0[rate_idx].clone()).collect();
    let scores = analysis::score_policies(ways, &per_pat_vals);
    let confidence = analysis::confidence(&scores);
    print_scores(lvl.name, &pats, &per_pat_vals, &scores);
    for s in &scores {
        let _ = writeln!(csv, "repl_score,{},{},{:.5},{:.4}", lvl.name, s.kind.name(), s.mean_dist, s.coverage);
    }
    let ranking: Vec<(Kind, f64)> = scores.iter().map(|s| (s.kind, s.mean_dist)).collect();
    let control = measured.last().unwrap().1;
    println!("  control (fits in the set): {control:.4} refills/access");
    Ok(Some(Policy { level: lvl.name, ranking, confidence }))
}

fn run_inclusion(c: &mut Collector, sess: &Session, pool: &PhysPool, choice: &Choice, w2: usize, csv: &mut String) -> Result<String> {
    let n = w2 + 1;
    let targets = lineset::same_set_lines(&pool.pages, &choice.base, OFFSET, n);
    let rig = TraceRig::new(&targets, &[])?;
    let grp = Group::open(&["cpu_cycles", "l1d_cache_refill", "l2d_cache_refill"].iter().map(|n| sess.pmu.event(n)).collect::<Result<Vec<_>>>()?)?;
    let pats = sim::patterns(w2);
    let hot = pats.iter().find(|p| p.name == "hot line + cycle").unwrap().trace.clone();
    let thrash_only: Vec<u16> = (0..sim::TRACE_LEN).map(|i| (1 + i % w2) as u16).collect();
    let cases = [("hot line X + W thrash lines (X never reaches L2 while hot)", hot), ("control: the W thrash lines only", thrash_only)];
    let mut acc: BTreeMap<String, Pooled> = BTreeMap::new();
    for round in 0..ROUNDS {
        let order: Vec<usize> = if round % 2 == 0 { vec![0, 1] } else { vec![1, 0] };
        for &i in &order {
            let exp = format!("inclusion/{}", cases[i].0);
            let part = measure_traces(c, sess, &grp, &rig, &exp, json!({"round": round}), &cases[i].1, false)?;
            pool_into(&mut acc, &exp, part);
        }
    }
    println!("\n== L1/L2 inclusion (L2 set thrashed with W = {w2} lines while line X stays hot in L1)");
    let mut rates = Vec::new();
    for (name, _) in &cases {
        let exp = format!("inclusion/{name}");
        let (vals, total) = &acc[&exp];
        let sm = c.summarize_pooled(&exp, *total, &trace_metrics(), vals)?;
        let (l1, l2) = (med(&sm[0]), med(&sm[1]));
        println!("  {name}: L1 refills {l1:.4}, L2 refills {l2:.4} per access");
        let _ = writeln!(csv, "inclusion,{name},{l1:.5},{l2:.5}");
        rates.push(l2);
    }
    let verdict = if rates[0] < 0.05 {
        "no back-invalidation observed: the hot line stayed in L1 although its L2 copy became the eviction candidate (L2 not strictly inclusive of L1)"
    } else if rates[0] > 0.3 {
        "L2 refills as if hot line X were evicted from L1 together with its L2 copy: back-invalidation (L2 inclusive of L1) is compatible with the data"
    } else {
        "intermediate: neither clean signature"
    };
    println!("  -> {verdict}");
    Ok(verdict.to_string())
}

// ---------------------------------------------------------------------------------------------

pub fn run_cache(sess: &Session) -> Result<()> {
    let mut c = sess.collector("cache")?;
    let mut csv = String::from("kind,level_or_case,x,value,a,b,c\n");
    println!("allocating and mapping a {} MiB pool (mlock as root, PFNs from pagemap)...", POOL_BYTES >> 20);
    let pool = PhysPool::new(POOL_BYTES)?;
    let outside = pool.pages_outside_ram()?;
    let ram = crate::phys::system_ram_ranges()?;
    if outside != 0 {
        bail!("{outside} pool pages fall outside System RAM: PFN unit assumption is wrong");
    }
    let (pmin, pmax) = (pool.pages.iter().map(|p| p.pbase).min().unwrap(), pool.pages.iter().map(|p| p.pbase).max().unwrap());
    println!("pool: {} pages, physical span {:#x}..{:#x}, locked: {}, System RAM ranges: {ram:x?}", pool.pages.len(), pmin, pmax, pool.buf.locked);
    c.note(&json!({"kind": "pool", "pages": pool.pages.len(), "pmin": pmin, "pmax": pmax, "locked": pool.buf.locked,
        "pages_outside_system_ram": outside, "system_ram": ram.iter().map(|r| [r.0, r.1]).collect::<Vec<_>>()}))?;

    run_line_size(&mut c, sess, &mut csv)?;

    let flip_bits: Vec<u32> = (PAGE_SHIFT..=26).collect();
    let need = LEVELS[2].k_max + 2;
    let choice = lineset::choose(&pool.pages, 26, need, &flip_bits).ok_or_else(|| anyhow!("pool too small to build a {need}-line class"))?;
    println!("\nconstrained physical bits {PAGE_SHIFT}..={} of every line group (bits above are uncontrolled), class of {} pages", choice.hi, choice.base.len());
    c.note(&json!({"kind": "class", "hi": choice.hi, "pages_in_class": choice.base.len()}))?;

    let grp = chain_group(&sess.pmu)?;
    let mut geos: Vec<Geo> = Vec::new();
    for lvl in &LEVELS {
        let (ways, onset, _) = run_assoc(&mut c, sess, &grp, &pool, &choice.base, lvl, &mut csv)?;
        let mut geo = Geo { level: lvl.name, ways, onset, index_bits: Vec::new(), tested_hi: choice.hi };
        match ways {
            Some(w) if w >= 1 => geo.index_bits = run_flip(&mut c, sess, &grp, &pool, &choice, lvl, w, &mut csv)?,
            _ => println!("  {}: no refill onset within K <= {}: cannot run the index-bit test", lvl.name, lvl.k_max),
        }
        println!("  => {}: ways*(conflict capacity) = {:?}, onset K = {:?}, index bits {:?} ({} sets{})", lvl.name, geo.ways, geo.onset, geo.index_bits, geo.sets(), if geo.contiguous() { ", contiguous" } else { ", NOT contiguous" });
        c.note(&json!({"kind": "geometry", "level": geo.level, "ways": geo.ways, "onset_k": geo.onset, "index_bits": geo.index_bits, "sets": geo.sets(), "tested_hi": geo.tested_hi}))?;
        geos.push(geo);
    }

    let mut policies: Vec<Policy> = Vec::new();
    let mut inclusion = String::from("not run");
    let l2_mask: u64 = geos[1].index_bits.iter().filter(|&&b| b >= PAGE_SHIFT).fold(0u64, |m, &b| m | (1u64 << b));
    for (i, g) in geos.iter().enumerate().take(2) {
        let Some(w) = g.ways else { continue };
        match run_replacement(&mut c, sess, &pool, &choice, i, w, l2_mask, &mut csv) {
            Ok(Some(p)) => policies.push(p),
            Ok(None) => {}
            Err(e) => println!("  replacement {}: skipped ({e:#})", g.level),
        }
    }
    if let Some(w2) = geos[1].ways {
        inclusion = run_inclusion(&mut c, sess, &pool, &choice, w2, &mut csv)?;
    }

    let moved = pool.moved_pages()?;
    println!("\npages whose physical address changed during the run: {moved}");
    c.note(&json!({"kind": "pool_check", "moved_pages": moved}))?;
    if moved != 0 {
        println!("WARNING: {moved} pages moved: the run is not trustworthy");
    }

    // Summary table.
    let mut summary = String::from("level,sysfs_size_bytes,sysfs_ways,measured_conflict_capacity,measured_index_bits,sets_from_bits,line_bytes,size_from_bits_bytes,policy_candidate,policy_confidence\n");
    println!("\n== summary");
    for (g, l) in geos.iter().zip(LEVELS.iter()) {
        let pol = policies.iter().find(|p| p.level == g.level);
        let (pname, pconf) = pol.map_or(("n/a".to_string(), "n/a"), |p| (format!("compatible with {}", p.ranking[0].0.name()), p.confidence));
        let size = g.ways.map_or(0, |w| w * g.sets() * LINE);
        let _ = writeln!(summary, "{},{},{},{},\"{:?}\",{},{},{},{},{}", g.level, l.sysfs_bytes, l.sysfs_ways, g.ways.map_or(-1, |w| w as i64), g.index_bits, g.sets(), LINE, size, pname, pconf);
        println!("  {}: conflict capacity {:?} (sysfs ways {}), {} sets from index bits {:?}, => {} KiB ({} KiB in sysfs); policy: {pname} [{pconf}]", g.level, g.ways, l.sysfs_ways, g.sets(), g.index_bits, size / 1024, l.sysfs_bytes / 1024);
    }
    println!("  L1/L2 inclusion: {inclusion}");
    std::fs::write(sess.day.join("cache_experiments.csv"), csv)?;
    std::fs::write(sess.day.join("cache_summary.csv"), summary)?;
    println!("cache: {} reps, {} flagged invalid; data in {}", c.total_reps, c.invalid_reps, sess.day.display());
    Ok(())
}
