//! Phase 4: branch predictors, measured with code generated at run time (see `jit.rs`).
//!
//! 4.1 BTB: a ring of N direct taken branches (spacing S bytes); cycles per branch vs N.
//! 4.2 conditional predictor, learning capacity: a random pattern of period P.
//! 4.3 conditional predictor, history length: a branch correlated with one K branches earlier.
//! 4.4 number of distinct conditional branches that stay predicted.
//! 4.5 indirect predictor: `br xN` with T targets (round robin, or a random sequence of period P).
//! 4.6 return stack depth: nested call chains of depth D.
//! 4.7 misprediction penalty: cycles per misprediction from a slope over mixed outcome data.

use crate::a64::*;
use crate::harness::{metric, run_window, Collector, Metric, Session};
use crate::jit::JitCode;
use crate::perm::sattolo;
use crate::pmu::{Group, Pmu};
use crate::stats::{bootstrap_slope_ci, linear_fit, Rng, Summary};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::hint::black_box;

const ROUNDS: usize = 3;

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

fn metrics() -> [Metric; 7] {
    [
        metric("cycles_per_unit", "cycles"),
        metric("mispredicts_per_unit", "events"),
        metric("branches_retired_per_unit", "events"),
        metric("instructions_per_unit", "events"),
        metric("stall_frontend_per_unit", "cycles"),
        metric("l1i_refill_per_unit", "events"),
        metric("itlb_walk_per_unit", "events"),
    ]
}

fn group(pmu: &Pmu) -> Result<Group> {
    let names = ["cpu_cycles", "br_mis_pred", "br_retired", "inst_retired", "stall_frontend", "l1i_cache_refill", "itlb_walk"];
    Group::open(&names.iter().map(|n| pmu.event(n)).collect::<Result<Vec<_>>>()?)
}

/// 8-byte aligned data area, optionally regenerated before each repetition.
struct Data {
    v: Vec<u64>,
}

impl Data {
    fn new(bytes: usize) -> Data {
        Data { v: vec![0u64; bytes.div_ceil(8) + 2] }
    }
    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: a Vec<u64> is valid as a byte slice of 8x the length.
        unsafe { std::slice::from_raw_parts_mut(self.v.as_mut_ptr() as *mut u8, self.v.len() * 8) }
    }
    fn ptr(&self) -> *const u8 {
        self.v.as_ptr() as *const u8
    }
}

struct Case {
    exp: String,
    tags: Value,
    code: JitCode,
    iters: u64,
    units: f64,
    data: Data,
    refill: Option<Box<dyn FnMut(&mut [u8])>>,
    /// Memory that generated code refers to by address (must stay alive and unmoved).
    keep: Vec<u64>,
}

fn run_case(c: &mut Collector, grp: &Group, frq: u64, n: usize, case: &mut Case) -> Result<Pooled> {
    let (code, iters, units) = (&case.code, case.iters, case.units);
    let data = &mut case.data;
    let refill = &mut case.refill;
    let _keep = &case.keep;
    c.reps_raw(&case.exp, case.tags.clone(), n, &metrics(), || {
        if let Some(f) = refill.as_mut() {
            f(data.bytes_mut());
        }
        let p = data.ptr();
        let (_, v) = run_window(grp, frq, || {
            black_box(code.call(iters, black_box(p)));
        })?;
        Ok(v.iter().map(|&x| x as f64 / units).collect())
    })
}

/// Runs `build` for every key in three rounds (ascending, descending, ascending) and returns the
/// pooled summaries by experiment name.
fn sweep<K: Clone>(
    c: &mut Collector,
    grp: &Group,
    sess: &Session,
    keys: &[K],
    mut build: impl FnMut(&K, usize) -> Result<Option<Case>>,
) -> Result<BTreeMap<String, (Vec<Option<Summary>>, Pooled)>> {
    let per_round = sess.repeat.div_ceil(ROUNDS);
    let mut acc: BTreeMap<String, Pooled> = BTreeMap::new();
    for round in 0..ROUNDS {
        let order: Vec<K> = if round % 2 == 0 { keys.to_vec() } else { keys.iter().rev().cloned().collect() };
        for k in &order {
            let Some(mut case) = build(k, round)? else { continue };
            let part = run_case(c, grp, sess.frq, per_round, &mut case)?;
            pool_into(&mut acc, &case.exp, part);
        }
    }
    let mut out = BTreeMap::new();
    for (exp, p) in acc {
        let s = c.summarize_pooled(&exp, p.1, &metrics(), &p.0)?;
        out.insert(exp, (s, p));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// code generators (x0 = iterations, x1 = data pointer)

/// Ring of `n` slots, each a single `b` to the next slot of a random cycle; the tail decrements the
/// iteration count. Slots are `spacing_words` apart. `n + 2` taken branches per iteration (N ring
/// branches, the tail's `b.ne`, and the `b` back to the first slot).
fn gen_btb(n: usize, spacing_words: usize, seed: u64) -> Result<Vec<u32>> {
    let order = sattolo(n, &mut Rng::new(seed));
    let mut path = Vec::with_capacity(n);
    let mut cur = 0usize;
    for _ in 0..n {
        path.push(cur);
        cur = order[cur] as usize;
    }
    let mut next_of = vec![usize::MAX; n];
    for k in 0..n.saturating_sub(1) {
        next_of[path[k]] = path[k + 1];
    }
    let mut a = Asm::new();
    let slots: Vec<Label> = (0..n).map(|_| a.label()).collect();
    let tail = a.label();
    a.b(slots[path[0]]);
    for i in 0..n {
        a.pad_to(16 + i * spacing_words);
        a.bind(slots[i]);
        if next_of[i] == usize::MAX {
            a.b(tail);
        } else {
            a.b(slots[next_of[i]]);
        }
    }
    a.pad_to(16 + n * spacing_words);
    a.bind(tail);
    a.emit(subs_imm(0, 0, 1));
    let back = a.label();
    a.b_cond(COND_NE, back);
    a.emit(ret());
    a.bind(back);
    a.b(slots[path[0]]); // b.cond only reaches +-1 MiB, b reaches +-128 MiB
    a.finish().map_err(|e| anyhow!(e))
}

/// One data-dependent branch per iteration: byte `data[i] & 1` decides; `late_muls` dependent
/// multiplies (which preserve bit 0) delay the resolution of the branch.
fn gen_pattern(late_muls: usize) -> Result<Vec<u32>> {
    let mut a = Asm::new();
    let (top, skip) = (a.label(), a.label());
    a.emit(movz(5, 0, 0));
    a.bind(top);
    a.emit(ldrb_post1(2, 1));
    for _ in 0..late_muls {
        a.emit(mul(2, 2, 2));
    }
    a.tbz(2, 0, skip);
    a.emit(add_imm(5, 5, 1));
    a.bind(skip);
    a.emit(subs_imm(0, 0, 1));
    a.b_cond(COND_NE, top);
    a.emit(add_reg(0, 5, XZR));
    a.emit(ret());
    a.finish().map_err(|e| anyhow!(e))
}

/// Branch 0 tests a random bit; K-1 always-taken (or never-taken) filler branches, 16 bytes apart, follow; the final
/// branch tests the same bit again. Mispredictions per iteration = 0.5 (branch 0 is random) plus the
/// final branch's rate, which depends on whether the predictor still sees branch 0 in its history.
fn gen_corr(k: usize, taken_fillers: bool, control: bool) -> Result<Vec<u32>> {
    let mut a = Asm::new();
    let top = a.label();
    a.emit(movz(5, 0, 0));
    a.emit(movz(6, 0, 0));
    a.bind(top);
    a.emit(ldr_x_post8(2, 1));
    let s0 = a.label();
    a.tbz(2, 0, s0);
    a.emit(add_imm(5, 5, 1));
    a.bind(s0);
    for _ in 0..k.saturating_sub(1) {
        let next = a.label();
        if taken_fillers {
            a.cbz(XZR, next);
            a.pad_to(a.pos() + 3); // skipped by the taken branch: fillers sit 16 bytes apart
        } else {
            a.cbnz(XZR, next);
            for _ in 0..3 {
                a.emit(nop());
            }
        }
        a.bind(next);
    }
    let s1 = a.label();
    // Control variant: the final branch tests an independent random bit, so it cannot be predicted.
    a.tbz(2, if control { 1 } else { 0 }, s1);
    a.emit(add_imm(6, 6, 1));
    a.bind(s1);
    a.emit(subs_imm(0, 0, 1));
    a.b_cond(COND_NE, top);
    a.emit(add_reg(0, 5, 6));
    a.emit(ret());
    a.finish().map_err(|e| anyhow!(e))
}

/// `n` static conditional branches per iteration; branch i tests bit (i mod 3) of an iteration
/// counter, so each has a period of 2, 4 or 8 iterations. Both outcomes continue at the next
/// instruction, or after `pad_nops` nops (skipped when taken) to space the branches out.
fn gen_nbranches(n: usize, pad_nops: usize) -> Result<Vec<u32>> {
    let mut a = Asm::new();
    let top = a.label();
    a.emit(movz(3, 0, 0));
    a.bind(top);
    a.emit(add_imm(3, 3, 1));
    for i in 0..n {
        let next = a.label();
        a.tbz(3, (i % 3) as u32, next);
        for _ in 0..pad_nops {
            a.emit(nop());
        }
        a.bind(next);
    }
    a.emit(subs_imm(0, 0, 1));
    a.b_cond(COND_NE, top);
    a.emit(add_reg(0, 3, XZR));
    a.emit(ret());
    a.finish().map_err(|e| anyhow!(e))
}

/// Indirect jump through a table of `t` targets, selected by a stream of u16 indices. Data layout:
/// `[u64 pointer to the target table][u16 indices ...]`. Returns the code and the word offsets of
/// the targets.
fn gen_indirect(t: usize) -> Result<(Vec<u32>, Vec<usize>)> {
    let mut a = Asm::new();
    let (top, tail) = (a.label(), a.label());
    a.emit(movz(5, 0, 0));
    a.emit(ldr_x_post8(7, 1));
    a.bind(top);
    a.emit(ldrh_post2(4, 1));
    a.emit(ldr_x_reg_lsl3(2, 7, 4));
    a.emit(br(2));
    let start = a.pos().div_ceil(16) * 16;
    a.pad_to(start);
    let mut offs = Vec::with_capacity(t);
    for _ in 0..t {
        offs.push(a.pos());
        a.emit(add_imm(5, 5, 1));
        a.b(tail);
        a.pad_to(a.pos() + 2);
    }
    a.bind(tail);
    a.emit(subs_imm(0, 0, 1));
    a.b_cond(COND_NE, top);
    a.emit(add_reg(0, 5, XZR));
    a.emit(ret());
    Ok((a.finish().map_err(|e| anyhow!(e))?, offs))
}

/// Chain of `d` nested calls per iteration: f1 calls f2 ... f_d returns immediately. With
/// `random_sites`, every level calls the next one from one of two call sites chosen by a random bit,
/// so the return address of each level is unpredictable for anything but a return-address stack.
fn gen_ras(d: usize, random_sites: bool) -> Result<Vec<u32>> {
    let mut a = Asm::new();
    let top = a.label();
    let fl: Vec<Label> = (0..d).map(|_| a.label()).collect();
    a.emit(stp_pre16(29, 30));
    a.bind(top);
    if random_sites {
        a.emit(ldr_x_post8(2, 1));
    }
    a.bl(fl[0]);
    a.emit(subs_imm(0, 0, 1));
    a.b_cond(COND_NE, top);
    a.emit(ldp_post16(29, 30));
    a.emit(ret());
    for i in 0..d {
        a.bind(fl[i]);
        if i + 1 < d {
            a.emit(stp_pre16(29, 30));
            if random_sites {
                if i > 0 && i % 64 == 0 {
                    a.emit(ldr_x_post8(2, 1));
                }
                let (site_b, done) = (a.label(), a.label());
                a.tbz(2, (i % 64) as u32, site_b);
                a.bl(fl[i + 1]);
                a.b(done);
                a.bind(site_b);
                a.bl(fl[i + 1]);
                a.bind(done);
            } else {
                a.bl(fl[i + 1]);
            }
            a.emit(ldp_post16(29, 30));
        }
        a.emit(ret());
    }
    a.finish().map_err(|e| anyhow!(e))
}

/// Indirect-experiment case: `sequence` is the target index stream (repeated to `len` entries).
fn indirect_case(exp: String, tags: Value, t: usize, sequence: &[u16], len: usize) -> Result<Case> {
    let (words, offs) = gen_indirect(t)?;
    let code = JitCode::new(&words)?;
    let keep: Vec<u64> = offs.iter().map(|o| (code.base() + 4 * o) as u64).collect();
    let mut data = Data::new(8 + 2 * len);
    {
        let b = data.bytes_mut();
        b[..8].copy_from_slice(&(keep.as_ptr() as u64).to_le_bytes());
        for i in 0..len {
            let v = sequence[i % sequence.len()];
            b[8 + 2 * i..10 + 2 * i].copy_from_slice(&v.to_le_bytes());
        }
    }
    Ok(Case { exp, tags, code, iters: len as u64, units: len as f64, data, refill: None, keep })
}

// ---------------------------------------------------------------------------------------------

fn selftest_jit() -> Result<()> {
    let code = JitCode::new(&gen_pattern(0)?)?;
    let mut d = Data::new(4096);
    d.bytes_mut()[..1000].fill(1);
    let r = code.call(1000, d.ptr());
    if r != 1000 {
        bail!("JIT self-test: pattern all-ones returned {r}, expected 1000");
    }
    d.bytes_mut()[..1000].fill(2);
    if code.call(1000, d.ptr()) != 0 {
        bail!("JIT self-test: pattern with bit 0 clear must count 0");
    }
    let code = JitCode::new(&gen_pattern(4)?)?;
    for (i, b) in d.bytes_mut()[..1000].iter_mut().enumerate() {
        *b = (i % 3 == 0) as u8;
    }
    let expect = (0..1000).filter(|i| i % 3 == 0).count() as u64;
    if code.call(1000, d.ptr()) != expect {
        bail!("JIT self-test: the multiply chain must preserve bit 0");
    }
    for taken in [true, false] {
        let code = JitCode::new(&gen_corr(5, taken, false)?)?;
        let mut d = Data::new(800);
        d.bytes_mut()[..800].fill(0xFF);
        let r = code.call(100, d.ptr());
        if r != 200 {
            bail!("JIT self-test: corr(taken fillers = {taken}) returned {r}, expected 200");
        }
    }
    for pad in [0usize, 3] {
        if JitCode::new(&gen_nbranches(7, pad)?)?.call(50, std::ptr::null()) != 50 {
            bail!("JIT self-test: nbranches(pad {pad})");
        }
    }
    for (n, s) in [(1usize, 1usize), (5, 1), (33, 4)] {
        JitCode::new(&gen_btb(n, s, 3)?)?.call(10, std::ptr::null());
    }
    for d in [1usize, 2, 5, 40] {
        JitCode::new(&gen_ras(d, false)?)?.call(10, std::ptr::null());
    }
    for d in [1usize, 2, 5, 70] {
        let code = JitCode::new(&gen_ras(d, true)?)?;
        let mut dd = Data::new(8 * 64);
        dd.bytes_mut().iter_mut().for_each(|b| *b = 0xA5);
        code.call(10, dd.ptr());
    }
    let mut case = indirect_case("selftest".into(), Value::Null, 3, &[0, 1, 2], 30)?;
    let r = case.code.call(30, case.data.ptr());
    if r != 30 {
        bail!("JIT self-test: indirect jumps returned {r}, expected 30");
    }
    let _ = &mut case;
    Ok(())
}

// ---------------------------------------------------------------------------------------------

pub fn run_branch(sess: &Session) -> Result<()> {
    selftest_jit()?;
    println!("JIT self-test passed: generated code (branches, calls, indirect jumps) executes correctly");
    let mut c = sess.collector("branch")?;
    let grp = group(&sess.pmu)?;
    let mut csv = String::from("kind,a,b,c,d,e,f,g,h,i\n");
    e41_btb(&mut c, &grp, sess, &mut csv)?;
    e42_pattern(&mut c, &grp, sess, &mut csv)?;
    e43_corr(&mut c, &grp, sess, &mut csv)?;
    e44_nbranches(&mut c, &grp, sess, &mut csv)?;
    e45_indirect(&mut c, &grp, sess, &mut csv)?;
    e46_ras(&mut c, &grp, sess, &mut csv)?;
    e47_penalty(&mut c, &grp, sess, &mut csv)?;
    std::fs::write(sess.day.join("branch_experiments.csv"), csv)?;
    println!("branch: {} reps, {} flagged invalid; data in {}", c.total_reps, c.invalid_reps, sess.day.display());
    Ok(())
}

fn e41_btb(c: &mut Collector, grp: &Group, sess: &Session, csv: &mut String) -> Result<()> {
    let spacings = [4usize, 8, 16, 32, 64, 256];
    let ns = [1usize, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536, 2048, 3072, 4096, 6144, 8192, 12288, 16384, 24576, 32768, 49152, 65536];
    let keys: Vec<(usize, usize)> = spacings.iter().flat_map(|&s| ns.iter().map(move |&n| (s, n))).collect();
    let res = sweep(c, grp, sess, &keys, |&(s, n), round| {
        if n * s > 8 << 20 {
            return Ok(None);
        }
        let code = JitCode::new(&gen_btb(n, s / 4, 100 + round as u64)?)?;
        let iters = ((1u64 << 20) / (n as u64 + 2)).max(2);
        Ok(Some(Case {
            exp: format!("btb/S={s}/N={n}"),
            tags: json!({"spacing": s, "n": n, "round": round}),
            code,
            iters,
            units: (iters * (n as u64 + 2)) as f64,
            data: Data::new(16),
            refill: None,
            keep: Vec::new(),
        }))
    })?;
    println!("\n== 4.1 BTB: cycles per taken direct branch (ring of N branches, spacing S bytes)");
    let _ = writeln!(csv, "btb_header,spacing,n,cycles_per_branch,mis_per_branch,l1i_refill_per_branch,stall_frontend_per_branch,itlb_walk_per_branch");
    for &s in &spacings {
        print!("  S={s:>3}:");
        for &n in &ns {
            if let Some((sm, _)) = res.get(&format!("btb/S={s}/N={n}")) {
                let _ = writeln!(csv, "btb,{s},{n},{:.4},{:.5},{:.5},{:.4},{:.5}", med(sm, 0), med(sm, 1), med(sm, 5), med(sm, 4), med(sm, 6));
                if [1, 4, 16, 64, 256, 1024, 2048, 4096, 8192, 16384, 32768, 65536].contains(&n) {
                    print!(" N={n}:{:.2}", med(sm, 0));
                }
            }
        }
        println!();
    }
    Ok(())
}

fn e42_pattern(c: &mut Collector, grp: &Group, sess: &Session, csv: &mut String) -> Result<()> {
    let ps = [1usize, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536, 2048, 3072, 4096, 6144, 8192, 12288, 16384, 32768, 65536];
    let len = 1usize << 18;
    let res = sweep(c, grp, sess, &ps, |&p, round| {
        let code = JitCode::new(&gen_pattern(0)?)?;
        let mut rng = Rng::new(p as u64 * 977 + round as u64);
        let base: Vec<u8> = (0..p).map(|_| (rng.next_u64() & 1) as u8).collect();
        let mut data = Data::new(len);
        for (i, b) in data.bytes_mut()[..len].iter_mut().enumerate() {
            *b = base[i % p];
        }
        Ok(Some(Case { exp: format!("pattern/P={p}"), tags: json!({"period": p, "round": round}), code, iters: len as u64, units: len as f64, data, refill: None, keep: Vec::new() }))
    })?;
    println!("\n== 4.2 conditional predictor: mispredictions per branch vs pattern period P (random pattern, repeated)");
    let _ = writeln!(csv, "pattern_header,period,mis_per_branch,cycles_per_iter");
    for &p in &ps {
        let (sm, _) = &res[&format!("pattern/P={p}")];
        let _ = writeln!(csv, "pattern,{p},{:.5},{:.4}", med(sm, 1), med(sm, 0));
        print!("  P={p}:{:.3}", med(sm, 1));
    }
    println!();
    Ok(())
}

fn e43_corr(c: &mut Collector, grp: &Group, sess: &Session, csv: &mut String) -> Result<()> {
    let ks = [1usize, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536, 2048, 2304, 2560, 2816, 3072, 4096, 6144];
    // 0 = taken fillers, 1 = not-taken fillers, 2 = control (final branch independent, taken fillers)
    let variants = [("taken", true, false), ("nottaken", false, false), ("control", true, true)];
    let keys: Vec<(usize, usize)> = (0..3).flat_map(|v| ks.iter().map(move |&k| (v, k))).collect();
    let iters = 1usize << 15;
    let res = sweep(c, grp, sess, &keys, |&(v, k), round| {
        let (name, taken, control) = variants[v];
        let code = JitCode::new(&gen_corr(k, taken, control)?)?;
        let mut rng = Rng::new(k as u64 * 31 + round as u64 + v as u64 * 7);
        let refill: Box<dyn FnMut(&mut [u8])> = Box::new(move |b: &mut [u8]| {
            for ch in b.chunks_exact_mut(8) {
                ch.copy_from_slice(&rng.next_u64().to_le_bytes());
            }
        });
        Ok(Some(Case {
            exp: format!("corr/{name}/K={k}"),
            tags: json!({"k": k, "variant": name, "round": round}),
            code,
            iters: iters as u64,
            units: iters as f64,
            data: Data::new(8 * iters),
            refill: Some(refill),
            keep: Vec::new(),
        }))
    })?;
    println!("\n== 4.3 history length: final-branch misprediction rate (= mispredictions per iteration - 0.5) vs distance K to the correlated branch");
    let _ = writeln!(csv, "corr_header,variant,k,mis_per_iter,final_branch_rate,l1i_refill_per_iter,stall_frontend_per_iter");
    for (name, _, _) in variants {
        print!("  {name:<8}:");
        for &k in &ks {
            let (sm, _) = &res[&format!("corr/{name}/K={k}")];
            let m = med(sm, 1);
            let _ = writeln!(csv, "corr,{name},{k},{m:.5},{:.5},{:.5},{:.3}", m - 0.5, med(sm, 5), med(sm, 4));
            print!(" K={k}:{:.2}", m - 0.5);
        }
        println!();
    }
    Ok(())
}

fn e44_nbranches(c: &mut Collector, grp: &Group, sess: &Session, csv: &mut String) -> Result<()> {
    let ns = [1usize, 2, 4, 8, 16, 32, 64, 128, 256, 512, 768, 1024, 1536, 2048, 3072, 4096, 6144, 8192, 12288, 16384];
    let pads = [0usize, 3];
    let keys: Vec<(usize, usize)> = pads.iter().flat_map(|&p| ns.iter().map(move |&n| (p, n))).collect();
    let res = sweep(c, grp, sess, &keys, |&(pad, n), round| {
        let code = JitCode::new(&gen_nbranches(n, pad)?)?;
        let iters = ((1u64 << 20) / n as u64).max(2);
        Ok(Some(Case { exp: format!("nbr/spacing={}/N={n}", 4 + 4 * pad), tags: json!({"n": n, "spacing": 4 + 4 * pad, "round": round}), code, iters, units: (iters * n as u64) as f64, data: Data::new(16), refill: None, keep: Vec::new() }))
    })?;
    println!("\n== 4.4 number of static conditional branches (periodic outcomes): mispredictions per branch");
    let _ = writeln!(csv, "nbr_header,spacing,n,mis_per_branch,cycles_per_branch,l1i_refill_per_branch");
    for &pad in &pads {
        let sp = 4 + 4 * pad;
        print!("  spacing {sp:>2} B:");
        for &n in &ns {
            let (sm, _) = &res[&format!("nbr/spacing={sp}/N={n}")];
            let _ = writeln!(csv, "nbr,{sp},{n},{:.5},{:.4},{:.5}", med(sm, 1), med(sm, 0), med(sm, 5));
            print!(" N={n}:{:.3}", med(sm, 1));
        }
        println!();
    }
    Ok(())
}

fn e45_indirect(c: &mut Collector, grp: &Group, sess: &Session, csv: &mut String) -> Result<()> {
    let ts = [1usize, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 2048];
    let len = 1usize << 16;
    let res = sweep(c, grp, sess, &ts, |&t, round| {
        let seq: Vec<u16> = (0..t).map(|i| i as u16).collect();
        Ok(Some(indirect_case(format!("ind_rr/T={t}"), json!({"t": t, "round": round}), t, &seq, len)?))
    })?;
    println!("\n== 4.5 indirect predictor: mispredictions per indirect branch, T targets visited round robin");
    let _ = writeln!(csv, "ind_rr_header,t,mis_per_branch,cycles_per_branch");
    for &t in &ts {
        let (sm, _) = &res[&format!("ind_rr/T={t}")];
        let _ = writeln!(csv, "ind_rr,{t},{:.5},{:.4}", med(sm, 1), med(sm, 0));
        print!("  T={t}:{:.3}", med(sm, 1));
    }
    println!();
    let ps = [16usize, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768];
    let res = sweep(c, grp, sess, &ps, |&p, round| {
        let mut rng = Rng::new(p as u64 * 13 + round as u64);
        let seq: Vec<u16> = (0..p).map(|_| rng.below(16) as u16).collect();
        Ok(Some(indirect_case(format!("ind_rand/P={p}"), json!({"p": p, "round": round}), 16, &seq, len)?))
    })?;
    println!("  16 targets, random sequence of period P:");
    let _ = writeln!(csv, "ind_rand_header,p,mis_per_branch,cycles_per_branch");
    for &p in &ps {
        let (sm, _) = &res[&format!("ind_rand/P={p}")];
        let _ = writeln!(csv, "ind_rand,{p},{:.5},{:.4}", med(sm, 1), med(sm, 0));
        print!("  P={p}:{:.3}", med(sm, 1));
    }
    println!();
    Ok(())
}

fn e46_ras(c: &mut Collector, grp: &Group, sess: &Session, csv: &mut String) -> Result<()> {
    let ds: Vec<usize> = (1..=40).chain([44, 48, 56, 64, 80, 96, 128]).collect();
    let keys: Vec<(bool, usize)> = [false, true].iter().flat_map(|&r| ds.iter().map(move |&d| (r, d))).collect();
    let res = sweep(c, grp, sess, &keys, |&(random, d), round| {
        let code = JitCode::new(&gen_ras(d, random)?)?;
        let iters = 1u64 << 14;
        let mut data = Data::new(8 * (iters as usize + 1) * (d / 64 + 1));
        let mut rng = Rng::new(d as u64 * 5 + round as u64);
        for ch in data.bytes_mut().chunks_exact_mut(8) {
            ch.copy_from_slice(&rng.next_u64().to_le_bytes());
        }
        Ok(Some(Case { exp: format!("ras/{}/D={d}", if random { "random_sites" } else { "chain" }), tags: json!({"depth": d, "random_sites": random, "round": round}), code, iters, units: iters as f64, data, refill: None, keep: Vec::new() }))
    })?;
    println!("\n== 4.6 return stack: excess mispredictions per iteration over the 0.5*(D-1) of the random call-site branches / cycles per iteration");
    let _ = writeln!(csv, "ras_header,variant,depth,mis_per_iter,cycles_per_iter,cycles_per_call_return,excess_mis_over_random_sites");
    for variant in ["chain", "random_sites"] {
        print!("  {variant:<12}:");
        for &d in &ds {
            let (sm, _) = &res[&format!("ras/{variant}/D={d}")];
            // Each random call-site branch mispredicts half the time: 0.5 * (D - 1) per iteration.
            let excess = if variant == "random_sites" { med(sm, 1) - 0.5 * (d as f64 - 1.0) } else { med(sm, 1) };
            let _ = writeln!(csv, "ras,{variant},{d},{:.4},{:.3},{:.3},{excess:.4}", med(sm, 1), med(sm, 0), med(sm, 0) / d as f64);
            if d <= 24 || d % 8 == 0 {
                print!(" D={d}:{excess:+.2}/{:.0}c", med(sm, 0));
            }
        }
        println!();
    }
    Ok(())
}

fn e47_penalty(c: &mut Collector, grp: &Group, sess: &Session, csv: &mut String) -> Result<()> {
    let muls = [0usize, 4, 8];
    let ps = [0usize, 1, 2, 4, 8, 16]; // taken probability = p / 32
    let keys: Vec<(usize, usize)> = muls.iter().flat_map(|&m| ps.iter().map(move |&p| (m, p))).collect();
    let len = 1usize << 18;
    let res = sweep(c, grp, sess, &keys, |&(m, p), round| {
        let code = JitCode::new(&gen_pattern(m)?)?;
        let mut rng = Rng::new(77 + m as u64 * 1000 + p as u64 + round as u64 * 5);
        let mut data = Data::new(len);
        for b in data.bytes_mut()[..len].iter_mut() {
            *b = (rng.below(32) < p as u64) as u8 | 0;
        }
        Ok(Some(Case { exp: format!("penalty/muls={m}/p={p}"), tags: json!({"muls": m, "p32": p, "round": round}), code, iters: len as u64, units: len as f64, data, refill: None, keep: Vec::new() }))
    })?;
    println!("\n== 4.7 misprediction penalty: slope of cycles per iteration against mispredictions per iteration");
    let _ = writeln!(csv, "penalty_header,muls,p32,mis_per_iter,cycles_per_iter");
    let mut rng = Rng::new(5);
    for &m in &muls {
        let (mut xs, mut ys) = (Vec::new(), Vec::new());
        for &p in &ps {
            let (sm, pooled) = &res[&format!("penalty/muls={m}/p={p}")];
            let _ = writeln!(csv, "penalty,{m},{p},{:.5},{:.4}", med(sm, 1), med(sm, 0));
            xs.extend_from_slice(&pooled.0[1]);
            ys.extend_from_slice(&pooled.0[0]);
        }
        let (slope, icpt) = linear_fit(&xs, &ys);
        let (lo, hi) = bootstrap_slope_ci(&xs, &ys, 1000, &mut rng);
        let _ = writeln!(csv, "penalty_fit,{m},{slope:.4},{icpt:.4},{lo:.4},{hi:.4}");
        println!("  {m} dependent multiplies before the branch: penalty {slope:.2} cycles per misprediction [{lo:.2}, {hi:.2}], {icpt:.2} cycles/iteration without mispredictions ({} points)", xs.len());
    }
    Ok(())
}
