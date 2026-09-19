//! Phase 3: hardware prefetchers, characterised from user space.
//!
//! A. stride sweep: dependent chase through lines `k` apart (k = +/-1..4096) versus a random cycle,
//!    on an L2-resident, an L3-resident and a DRAM-sized set (the level that hides the latency tells
//!    which prefetcher acts);
//! B. number of interleaved streams that keep being followed;
//! C. distance/degree: cold streams of n lines, counting the lines actually read on the bus;
//! D. behaviour at 4 KiB / 16 KiB / 64 KiB / 2 MiB boundaries;
//! E. stores versus loads.

use crate::harness::{metric, run_window, Collector, Metric, Session};
use crate::kernels;
use crate::mem::Buffer;
use crate::perm::sattolo;
use crate::pmu::{Group, Pmu};
use crate::stats::{Rng, Summary};
use crate::timing::cntvct;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::hint::black_box;

const ROUNDS: usize = 3;
const LINE: usize = 64;
const M_STREAMS: usize = 64;

type Pooled = (Vec<Vec<f64>>, usize);

fn pool_into(acc: &mut BTreeMap<String, Pooled>, key: &str, part: Pooled) {
    let e = acc.entry(key.to_string()).or_insert_with(|| (vec![Vec::new(); part.0.len()], 0));
    for (dst, src) in e.0.iter_mut().zip(part.0) {
        dst.extend(src);
    }
    e.1 += part.1;
}

fn med(s: &Option<Summary>) -> f64 {
    s.as_ref().map_or(f64::NAN, |x| x.median)
}

fn group(pmu: &Pmu, names: &[&str]) -> Result<Group> {
    Group::open(&names.iter().map(|n| pmu.event(n)).collect::<Result<Vec<_>>>()?)
}

fn chase_metrics() -> [Metric; 8] {
    [
        metric("ns_per_access", "ns"),
        metric("cycles_per_access", "cycles"),
        metric("l1d_refill_per_access", "events"),
        metric("l2d_refill_per_access", "events"),
        metric("l3d_refill_per_access", "events"),
        metric("ll_miss_rd_per_access", "events"),
        metric("bus_access_per_access", "events"),
        metric("dtlb_walk_per_access", "events"),
    ]
}

fn chase_group(pmu: &Pmu) -> Result<Group> {
    group(pmu, &["cpu_cycles", "l1d_cache_refill", "l2d_cache_refill", "l3d_cache_refill", "ll_cache_miss_rd", "bus_access", "dtlb_walk"])
}

#[allow(clippy::too_many_arguments)]
fn chase_reps(c: &mut Collector, grp: &Group, frq: u64, exp: &str, tags: Value, n: usize, start: *const u8, loads: u64) -> Result<Pooled> {
    let mut p = start;
    c.reps_raw(exp, tags, n, &chase_metrics(), || {
        let p0 = p;
        let mut pn = p0;
        let (dt, v) = run_window(grp, frq, || {
            // SAFETY: p0 is a node of a chain fully written in a live buffer.
            pn = unsafe { kernels::chase(black_box(p0), loads) };
        })?;
        p = black_box(pn);
        let l = loads as f64;
        Ok(vec![dt / l, v[0] as f64 / l, v[1] as f64 / l, v[2] as f64 / l, v[3] as f64 / l, v[4] as f64 / l, v[5] as f64 / l, v[6] as f64 / l])
    })
}

/// Chain visiting line `(j * k) mod n_lines` at step j; `n_lines` must be prime (single cycle).
///
/// # Safety
/// `base .. base + 64 * n_lines` must be writable.
unsafe fn build_stride_chain(base: *mut u8, n_lines: usize, k: isize) -> *const u8 {
    let kk = k.rem_euclid(n_lines as isize) as usize;
    let mut cur = 0usize;
    for _ in 0..n_lines {
        let nxt = (cur + kk) % n_lines;
        (base.add(cur * LINE) as *mut usize).write(base.add(nxt * LINE) as usize);
        cur = nxt;
    }
    base
}

/// # Safety
/// `base .. base + 64 * n_lines` must be writable.
unsafe fn build_random_chain(base: *mut u8, n_lines: usize, seed: u64) -> *const u8 {
    let next = sattolo(n_lines, &mut Rng::new(seed));
    for (i, &n) in next.iter().enumerate() {
        (base.add(i * LINE) as *mut usize).write(base.add(n as usize * LINE) as usize);
    }
    base
}

// ---------------------------------------------------------------------------------------------
// A. stride sweep

/// Strides of at least N/16 lines wrap around the small sets within a few steps and stop being a
/// clean stride, so they are skipped for those sets.
fn allowed(s: Option<isize>, n_lines: usize) -> bool {
    s.map_or(true, |k| k.unsigned_abs() * 16 <= n_lines)
}

fn run_stride_sweep(c: &mut Collector, sess: &Session, csv: &mut String) -> Result<BTreeMap<&'static str, f64>> {
    let grp = chase_group(&sess.pmu)?;
    let buf = Buffer::new(64 << 20)?;
    let regions: [(&str, usize); 3] = [("L2-resident", 4093), ("L3-resident", 16381), ("DRAM", 1_048_573)];
    let mags: [isize; 25] = [1, 2, 3, 4, 5, 6, 8, 10, 12, 16, 18, 20, 21, 22, 23, 24, 32, 48, 64, 96, 128, 256, 512, 1024, 4096];
    let mut strides: Vec<Option<isize>> = mags.iter().map(|&m| Some(m)).collect();
    strides.extend([-1isize, -2, -4, -8, -16, -64].iter().map(|&m| Some(m)));
    strides.push(None);
    let per_round = sess.repeat.div_ceil(ROUNDS);
    let mut acc: BTreeMap<String, Pooled> = BTreeMap::new();
    for round in 0..ROUNDS {
        for &(rname, n) in &regions {
            let mut order: Vec<Option<isize>> = strides.iter().cloned().filter(|s| allowed(*s, n)).collect();
            if round % 2 == 1 {
                order.reverse();
            }
            for s in order {
                // SAFETY: the buffer holds 64 MiB >= 64 * n bytes.
                let start = unsafe {
                    match s {
                        Some(k) => build_stride_chain(buf.as_mut_ptr(), n, k),
                        None => build_random_chain(buf.as_mut_ptr(), n, 99 + round as u64),
                    }
                };
                let tag = s.map_or("random".to_string(), |k| k.to_string());
                let exp = format!("stride/{rname}/{tag}");
                c.warmup = if n > 100_000 { 1 } else { 3 };
                let part = chase_reps(c, &grp, sess.frq, &exp, json!({"round": round, "region": rname, "stride": tag}), per_round, start, 1 << 20)?;
                pool_into(&mut acc, &exp, part);
            }
        }
        println!("stride sweep: round {}/{ROUNDS} done", round + 1);
    }
    let mut random_ns = BTreeMap::new();
    let _ = writeln!(csv, "stride_header,region,stride_lines,ns,cycles,ratio_vs_random,l1r,l2r,l3r,llmiss,bus,walk");
    for &(rname, n) in &regions {
        let rexp = format!("stride/{rname}/random");
        let rs = c.summarize_pooled(&rexp, acc[&rexp].1, &chase_metrics(), &acc[&rexp].0)?;
        let rnd = med(&rs[0]);
        random_ns.insert(rname, rnd);
        println!("\n== stride sweep, {rname} ({} KiB): random reference {rnd:.3} ns ({:.1} cycles)", n * LINE / 1024, med(&rs[1]));
        println!("  {:>7} {:>9} {:>8} {:>7} {:>7} {:>7} {:>7} {:>7}", "stride", "ns", "ratio", "L1r", "L2r", "L3r/ll", "bus", "walk");
        for s in strides.iter().filter(|s| allowed(**s, n)) {
            let tag = s.map_or("random".to_string(), |k| k.to_string());
            let exp = format!("stride/{rname}/{tag}");
            let sm = c.summarize_pooled(&exp, acc[&exp].1, &chase_metrics(), &acc[&exp].0)?;
            let (ns, cy) = (med(&sm[0]), med(&sm[1]));
            let ratio = ns / rnd;
            let _ = writeln!(csv, "stride,{rname},{tag},{ns:.4},{cy:.3},{ratio:.4},{:.5},{:.5},{:.5},{:.5},{:.4},{:.5}", med(&sm[2]), med(&sm[3]), med(&sm[4]), med(&sm[5]), med(&sm[6]), med(&sm[7]));
            println!("  {tag:>7} {ns:>9.3} {ratio:>8.3} {:>7.3} {:>7.3} {:>7.3} {:>7.2} {:>7.3}", med(&sm[2]), med(&sm[3]), med(&sm[5]), med(&sm[6]), med(&sm[7]));
        }
    }
    Ok(random_ns)
}

// ---------------------------------------------------------------------------------------------
// B. interleaved streams

fn run_streams(c: &mut Collector, sess: &Session, random_dram_ns: f64, csv: &mut String) -> Result<()> {
    let grp = chase_group(&sess.pmu)?;
    let l_lines = 131_072usize; // 8 MiB per stream
    let pad = 257usize;
    let s_list = [1usize, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 14, 16, 17, 18, 19, 20, 24, 32];
    let buf = Buffer::new(288 << 20)?;
    let per_round = sess.repeat.div_ceil(ROUNDS);
    let mut acc: BTreeMap<String, Pooled> = BTreeMap::new();
    for round in 0..ROUNDS {
        let order: Vec<usize> = if round % 2 == 0 { s_list.to_vec() } else { s_list.iter().rev().cloned().collect() };
        for &s in &order {
            let total = s * l_lines;
            let addr = |t: usize| -> usize { buf.as_ptr() as usize + ((t % s) * (l_lines + pad) + t / s) * LINE };
            // SAFETY: every address lies inside the 288 MiB buffer (s <= 32).
            unsafe {
                for t in 0..total {
                    (addr(t) as *mut usize).write(addr((t + 1) % total));
                }
            }
            let exp = format!("streams/S={s}");
            c.warmup = 1;
            let part = chase_reps(c, &grp, sess.frq, &exp, json!({"round": round, "streams": s}), per_round, addr(0) as *const u8, 1 << 20)?;
            pool_into(&mut acc, &exp, part);
        }
    }
    println!("\n== interleaved +1-line streams (DRAM-sized, 8 MiB each), random DRAM reference {random_dram_ns:.1} ns");
    let _ = writeln!(csv, "streams_header,S,ns,ratio_vs_random,bus,l2r,ll");
    for &s in &s_list {
        let exp = format!("streams/S={s}");
        let sm = c.summarize_pooled(&exp, acc[&exp].1, &chase_metrics(), &acc[&exp].0)?;
        let ns = med(&sm[0]);
        let _ = writeln!(csv, "streams,{s},{ns:.4},{:.4},{:.4},{:.5},{:.5}", ns / random_dram_ns, med(&sm[6]), med(&sm[3]), med(&sm[5]));
        println!("  S={s:>2}: {ns:>8.3} ns/access ({:.2} of random), bus {:.2}/access, L2 refill {:.3}", ns / random_dram_ns, med(&sm[6]), med(&sm[3]));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Physically contiguous memory from the CMA DMA heap (group `video`), for the boundary tests

struct CmaBuf {
    ptr: *mut u8,
    len: usize,
    fd: i32,
}

impl CmaBuf {
    fn alloc(len: usize) -> Result<CmaBuf> {
        #[repr(C)]
        struct AllocData {
            len: u64,
            fd: u32,
            fd_flags: u32,
            heap_flags: u64,
        }
        const DMA_HEAP_IOCTL_ALLOC: u64 = 0xC018_4800; // _IOWR('H', 0, struct dma_heap_allocation_data)
        // SAFETY: plain open/ioctl/mmap calls with checked results; the struct matches the kernel ABI.
        unsafe {
            let heap = libc::open(b"/dev/dma_heap/linux,cma\0".as_ptr() as *const libc::c_char, libc::O_RDWR | libc::O_CLOEXEC);
            if heap < 0 {
                return Err(anyhow!("open /dev/dma_heap/linux,cma: {}", std::io::Error::last_os_error()));
            }
            let mut d = AllocData { len: len as u64, fd: 0, fd_flags: (libc::O_RDWR | libc::O_CLOEXEC) as u32, heap_flags: 0 };
            let r = libc::ioctl(heap, DMA_HEAP_IOCTL_ALLOC as _, &mut d as *mut AllocData);
            libc::close(heap);
            if r < 0 {
                return Err(anyhow!("DMA_HEAP_IOCTL_ALLOC {len} bytes: {}", std::io::Error::last_os_error()));
            }
            let p = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, d.fd as i32, 0);
            if p == libc::MAP_FAILED {
                let e = std::io::Error::last_os_error();
                libc::close(d.fd as i32);
                return Err(anyhow!("mmap CMA buffer: {e}"));
            }
            std::ptr::write_bytes(p as *mut u8, 0, len); // lines must read as zero
            Ok(CmaBuf { ptr: p as *mut u8, len, fd: d.fd as i32 })
        }
    }
}

impl Drop for CmaBuf {
    fn drop(&mut self) {
        // SAFETY: ptr/len/fd were returned by mmap/ioctl in `alloc`.
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.len);
            libc::close(self.fd);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// C/D/E helpers: cold streams

struct ColdPool {
    buf: Buffer,
    slots: Vec<usize>,
    next: usize,
}

const SLOT_BYTES: usize = 512 << 10;
const POOL_BYTES: usize = 512 << 20;

impl ColdPool {
    fn new() -> Result<ColdPool> {
        let buf = Buffer::new_zeroed(POOL_BYTES)?;
        let mut slots: Vec<usize> = (0..POOL_BYTES / SLOT_BYTES).collect();
        let mut rng = Rng::new(4242);
        for i in (1..slots.len()).rev() {
            slots.swap(i, rng.below(i as u64 + 1) as usize);
        }
        Ok(ColdPool { buf, slots, next: 0 })
    }

    fn slot_base(&mut self) -> usize {
        let s = self.slots[self.next % self.slots.len()];
        self.next += 1;
        self.buf.as_ptr() as usize + s * SLOT_BYTES
    }

    /// Evict the L2/L3 contents so following streams start cold.
    fn flush(&self, scratch: &Buffer) {
        // SAFETY: scratch is 8 MiB, a multiple of 128 bytes.
        unsafe { kernels::bw_read(scratch.as_ptr(), scratch.len()) };
    }
}

fn cold_metrics() -> [Metric; 4] {
    [
        metric("bus_access_per_stream", "events"),
        metric("ll_miss_rd_per_stream", "events"),
        metric("l2d_refill_per_stream", "events"),
        metric("l3d_refill_per_stream", "events"),
    ]
}

fn cold_group(pmu: &Pmu) -> Result<Group> {
    group(pmu, &["bus_access", "ll_cache_miss_rd", "l2d_cache_refill", "l3d_cache_refill"])
}

fn spin_us(frq: u64, us: u64) {
    let t = cntvct();
    while cntvct() - t < frq * us / 1_000_000 {
        std::hint::spin_loop();
    }
}

#[allow(clippy::too_many_arguments)]
fn cold_reps(
    c: &mut Collector,
    grp: &Group,
    frq: u64,
    exp: &str,
    tags: Value,
    n: usize,
    flush: Option<(&ColdPool, &Buffer)>,
    m_streams: usize,
    mut body: impl FnMut(),
) -> Result<Pooled> {
    c.warmup = 2;
    c.reps_raw(exp, tags, n, &cold_metrics(), || {
        if let Some((pool, scratch)) = flush {
            pool.flush(scratch);
        }
        let (_, v) = run_window(grp, frq, || {
            body();
            spin_us(frq, 20);
        })?;
        let m = m_streams as f64;
        Ok(v.iter().map(|&x| x as f64 / m).collect())
    })
}

fn stream_once(start: usize, stride_lines: isize, n: usize, store: bool) {
    assert!(n * stride_lines.unsigned_abs() < 5000, "stream would leave its slot");
    // SAFETY: callers pass addresses inside the zero-filled cold pool with room for n lines.
    unsafe {
        if store {
            kernels::stream_store(start as *mut u8, stride_lines * LINE as isize, n as u64);
        } else {
            kernels::stream_load(start as *const u8, stride_lines * LINE as isize, n as u64);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// C. distance / degree

fn run_overshoot(c: &mut Collector, sess: &Session, pool: &mut ColdPool, csv: &mut String) -> Result<f64> {
    let grp = cold_group(&sess.pmu)?;
    let per_round = sess.repeat.div_ceil(ROUNDS);
    let ns_list = [1usize, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256];
    let cfgs: [(isize, bool); 9] = [(1, false), (2, false), (3, false), (4, false), (8, false), (16, false), (-1, false), (-2, false), (1, true)];
    let mut acc: BTreeMap<String, Pooled> = BTreeMap::new();
    for round in 0..ROUNDS {
        for &(stride, store) in &cfgs {
            let mut order = ns_list.to_vec();
            if round % 2 == 1 {
                order.reverse();
            }
            for n in order {
                let exp = format!("cold/{}/stride={stride}/n={n}", if store { "store" } else { "load" });
                let part = cold_reps(c, &grp, sess.frq, &exp, json!({"round": round}), per_round, None, M_STREAMS, || {
                    for _ in 0..M_STREAMS {
                        let base = pool.slot_base();
                        let start = if stride > 0 { base + 1024 * LINE } else { base + 6144 * LINE };
                        stream_once(start, stride, n, store);
                    }
                })?;
                pool_into(&mut acc, &exp, part);
            }
        }
    }
    let _ = writeln!(csv, "cold_header,kind,stride,n,bus_per_stream,bus_lines_extra_vs_n1,llmiss_per_stream,l2r,l3r");
    let mut baseline_bus = f64::NAN;
    println!("\n== cold streams: lines read on the bus beyond the n demanded (baseline = the n = 1 stream)");
    for &(stride, store) in &cfgs {
        let kind = if store { "store" } else { "load" };
        let get = |n: usize, c: &mut Collector| -> Result<Vec<Option<Summary>>> {
            let exp = format!("cold/{kind}/stride={stride}/n={n}");
            c.summarize_pooled(&exp, acc[&exp].1, &cold_metrics(), &acc[&exp].0)
        };
        let base = get(1, c)?;
        let (b1, ll1) = (med(&base[0]), med(&base[1]));
        if stride == 1 && !store {
            baseline_bus = b1;
        }
        print!("  {kind:<5} stride {stride:>2}: baseline(n=1) bus {b1:.1} ll {ll1:.2} | extra lines (bus/8, ll):");
        for &n in &ns_list {
            let sm = get(n, c)?;
            let (bus, ll) = (med(&sm[0]), med(&sm[1]));
            let extra_bus = (bus - b1) / 8.0 - (n as f64 - 1.0);
            let extra_ll = ll - ll1 - (n as f64 - 1.0);
            let _ = writeln!(csv, "cold,{kind},{stride},{n},{bus:.3},{extra_bus:.3},{ll:.3},{:.3},{:.3}", med(&sm[2]), med(&sm[3]));
            print!(" n={n}:{extra_bus:.1}/{extra_ll:.1}");
        }
        println!();
    }
    Ok(baseline_bus)
}

// ---------------------------------------------------------------------------------------------
// D. boundaries

/// Boundary addresses of exactly size `b`, optionally split by whether the two pages on either side
/// are physically contiguous (needs PFNs, i.e. root). `want`: None = all, Some(true/false) = filter.
fn boundary_candidates(pool: &ColdPool, b: usize, pfns: Option<&[u64]>, want: Option<bool>) -> Vec<usize> {
    let lo = pool.buf.as_ptr() as usize + (2 << 20);
    let hi = pool.buf.as_ptr() as usize + POOL_BYTES - (2 << 20);
    let mut a = lo.div_ceil(b) * b;
    let mut v = Vec::new();
    while a < hi {
        // exactly a boundary of size b (not also one of the next larger level)
        if b == 2 << 20 || a % (4 * b) != 0 {
            let ok = match (want, pfns) {
                (Some(w), Some(p)) if b >= 16384 => {
                    let i = (a - pool.buf.as_ptr() as usize) >> 14;
                    (p[i] == p[i - 1] + 1) == w
                }
                _ => true,
            };
            if ok {
                v.push(a);
            }
        }
        a += b;
    }
    v
}

fn run_boundaries(c: &mut Collector, sess: &Session, pool: &mut ColdPool, scratch: &Buffer, base_bus: f64, csv: &mut String) -> Result<()> {
    let grp = cold_group(&sess.pmu)?;
    let per_round = sess.repeat.div_ceil(ROUNDS);
    let bs = [4096usize, 16384, 65536, 2 << 20];
    let n = 16usize;
    let kinds = ["ends_at_boundary", "crosses_boundary", "mid_block"];
    let pfns = crate::phys::read_pfns(pool.buf.as_ptr() as usize, POOL_BYTES >> 14).ok();
    println!("\nphysical frame numbers {}", if pfns.is_some() { "available (root)" } else { "unavailable (not root): physical contiguity unknown" });
    // (label, boundary size, candidate boundary addresses)
    let mut variants: Vec<(String, usize, Vec<usize>)> = vec![("pool/B=4096".to_string(), 4096, boundary_candidates(pool, 4096, None, None))];
    for &b in &bs[1..] {
        match pfns.as_deref() {
            Some(p) => {
                let nc = boundary_candidates(pool, b, Some(p), Some(false));
                let ct = boundary_candidates(pool, b, Some(p), Some(true));
                variants.push((format!("pool/B={b}/noncontig"), b, nc));
                variants.push((format!("pool/B={b}/contig"), b, ct));
            }
            None => variants.push((format!("pool/B={b}/unknown"), b, boundary_candidates(pool, b, None, None))),
        }
    }
    // Physically contiguous CMA buffer: boundaries aligned in *physical* address space.
    let cma = match CmaBuf::alloc(32 << 20) {
        Ok(c) => Some(c),
        Err(e) => {
            println!("  CMA buffer unavailable ({e:#}): skipping the physically contiguous variants");
            None
        }
    };
    // The physical placement of a dma-buf mapping cannot be read from pagemap here, but a CMA
    // allocation is physically contiguous by construction and its start is page (16 KiB) aligned:
    // 4 KiB and 16 KiB boundaries are exact; the 64 KiB physical phase is unknown, so all four
    // phases are tried (the one matching a physical 64 KiB boundary is the one that would stand
    // out). 2 MiB physical alignment is unknowable: not tested here.
    if let Some(cb) = &cma {
        let vbase = cb.ptr as usize;
        let cand = |keep: &dyn Fn(usize) -> bool| -> Vec<usize> {
            (64usize << 10..cb.len - (64 << 10)).step_by(4096).filter(|&off| keep(off)).map(|off| vbase + off).collect()
        };
        variants.push(("cma/B=4096".to_string(), 4096, cand(&|o| o % 16384 != 0)));
        variants.push(("cma/B=16384".to_string(), 16384, cand(&|o| o % 16384 == 0)));
        for r in 0..4usize {
            variants.push((format!("cma/B=65536/phase={}K", r * 16), 65536, cand(&|o| o % 65536 == r * 16384)));
        }
        println!("  CMA buffer: 32 MiB physically contiguous by construction (physical alignment unknown; 2 MiB not tested)");
        c.note(&json!({"kind": "cma_buffer", "len": cb.len}))?;
    }
    let mut acc: BTreeMap<String, Pooled> = BTreeMap::new();
    for round in 0..ROUNDS {
        for (label, b, cl) in variants.iter() {
            let b = *b;
            if cl.len() < 4 {
                continue;
            }
            let m = cl.len().min(M_STREAMS);
            for dir in ["asc", "desc"] {
                for kind in kinds {
                    let exp = format!("boundary/{label}/{dir}/{kind}");
                    let mut rng = Rng::new(round as u64 * 7919 + b as u64);
                    let pool_ref: &ColdPool = pool;
                    let part = cold_reps(c, &grp, sess.frq, &exp, json!({"round": round}), per_round, Some((pool_ref, scratch)), m, || {
                        let mut used: Vec<usize> = Vec::with_capacity(m);
                        while used.len() < m {
                            let cand = cl[rng.below(cl.len() as u64) as usize];
                            if !used.contains(&cand) {
                                used.push(cand);
                            }
                        }
                        for &bd in &used {
                            let last_asc = match kind {
                                "ends_at_boundary" => bd - LINE,
                                "crosses_boundary" => bd + 7 * LINE,
                                _ => bd - LINE - b / 2 - if b == 4096 { 0 } else { 1920 },
                            };
                            let first_asc = if kind == "crosses_boundary" { bd - 8 * LINE } else { last_asc - (n - 1) * LINE };
                            if dir == "asc" {
                                stream_once(first_asc, 1, n, false);
                            } else {
                                // mirror around the boundary: line a -> 2*bd - 64 - a, visited downwards
                                stream_once(2 * bd - LINE - first_asc, -1, n, false);
                            }
                        }
                    })?;
                    pool_into(&mut acc, &exp, part);
                }
            }
        }
    }
    println!("\n== boundaries: 16-line cold streams; extra lines read on the bus = (bus - baseline(n=1))/8 - 15 demanded");
    let _ = writeln!(csv, "boundary_header,variant,dir,kind,bus_per_stream,extra_lines,ll_per_stream,candidates");
    for (label, _, cl) in variants.iter() {
        if cl.len() < 4 {
            println!("  {label}: only {} candidate boundaries, skipped", cl.len());
            continue;
        }
        for dir in ["asc", "desc"] {
            print!("  {label:<18} {dir:<4}:");
            for kind in kinds {
                let exp = format!("boundary/{label}/{dir}/{kind}");
                let sm = c.summarize_pooled(&exp, acc[&exp].1, &cold_metrics(), &acc[&exp].0)?;
                let extra = (med(&sm[0]) - base_bus) / 8.0 - (n as f64 - 1.0);
                let _ = writeln!(csv, "boundary,{label},{dir},{kind},{:.3},{extra:.3},{:.3},{}", med(&sm[0]), med(&sm[1]), cl.len());
                print!("  {kind}: {extra:.1}");
            }
            println!("  ({} candidates)", cl.len());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// E. stores vs loads (table-driven throughput)

fn run_stores(c: &mut Collector, sess: &Session, csv: &mut String) -> Result<()> {
    let grp = chase_group(&sess.pmu)?;
    let buf = Buffer::new(64 << 20)?;
    let n_lines = 1_048_573usize;
    let entries = 1usize << 19;
    let strides: [Option<isize>; 9] = [Some(1), Some(2), Some(4), Some(8), Some(16), Some(32), Some(64), Some(-1), None];
    let per_round = sess.repeat.div_ceil(ROUNDS);
    let mut acc: BTreeMap<String, Pooled> = BTreeMap::new();
    for round in 0..ROUNDS {
        let mut order = strides.to_vec();
        if round % 2 == 1 {
            order.reverse();
        }
        for s in order {
            let mut table: Vec<usize> = Vec::with_capacity(entries);
            match s {
                Some(k) => {
                    let kk = k.rem_euclid(n_lines as isize) as usize;
                    let mut cur = 0usize;
                    for _ in 0..entries {
                        table.push(buf.as_ptr() as usize + cur * LINE);
                        cur = (cur + kk) % n_lines;
                    }
                }
                None => {
                    let perm = sattolo(n_lines, &mut Rng::new(31 + round as u64));
                    let mut cur = 0usize;
                    for _ in 0..entries {
                        table.push(buf.as_ptr() as usize + cur * LINE);
                        cur = perm[cur] as usize;
                    }
                }
            }
            let tag = s.map_or("random".to_string(), |k| k.to_string());
            let exp = format!("store/stride={tag}");
            c.warmup = 1;
            let part = c.reps_raw(&exp, json!({"round": round}), per_round, &chase_metrics(), || {
                let (dt, v) = run_window(&grp, sess.frq, || {
                    // SAFETY: every table entry is a line inside the 64 MiB buffer.
                    unsafe { kernels::store_table(black_box(table.as_ptr()), entries as u64) };
                })?;
                let l = entries as f64;
                Ok(vec![dt / l, v[0] as f64 / l, v[1] as f64 / l, v[2] as f64 / l, v[3] as f64 / l, v[4] as f64 / l, v[5] as f64 / l, v[6] as f64 / l])
            })?;
            pool_into(&mut acc, &exp, part);
        }
    }
    println!("\n== stores (independent, table-driven, DRAM-sized): ns per store");
    let _ = writeln!(csv, "store_header,stride,ns,ratio_vs_random,bus,l2r,ll");
    let rexp = "store/stride=random".to_string();
    let rs = c.summarize_pooled(&rexp, acc[&rexp].1, &chase_metrics(), &acc[&rexp].0)?;
    let rnd = med(&rs[0]);
    for s in strides {
        let tag = s.map_or("random".to_string(), |k| k.to_string());
        let exp = format!("store/stride={tag}");
        let sm = c.summarize_pooled(&exp, acc[&exp].1, &chase_metrics(), &acc[&exp].0)?;
        let ns = med(&sm[0]);
        let _ = writeln!(csv, "store,{tag},{ns:.4},{:.4},{:.4},{:.5},{:.5}", ns / rnd, med(&sm[6]), med(&sm[3]), med(&sm[5]));
        println!("  stride {tag:>6}: {ns:>7.3} ns/store ({:.2} of random), bus {:.2}/store, L2 refill {:.3}", ns / rnd, med(&sm[6]), med(&sm[3]));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------

/// Cold-stream baseline (n = 1) alone, for running the boundary experiment on its own.
fn baseline_only(c: &mut Collector, sess: &Session, pool: &mut ColdPool) -> Result<f64> {
    let grp = cold_group(&sess.pmu)?;
    let part = cold_reps(c, &grp, sess.frq, "cold/load/stride=1/n=1", Value::Null, sess.repeat, None, M_STREAMS, || {
        for _ in 0..M_STREAMS {
            let base = pool.slot_base();
            stream_once(base + 1024 * LINE, 1, 1, false);
        }
    })?;
    let sm = c.summarize_pooled("cold/load/stride=1/n=1", part.1, &cold_metrics(), &part.0)?;
    Ok(med(&sm[0]))
}

pub fn run_boundary_only(sess: &Session) -> Result<()> {
    let mut c = sess.collector("prefetch_boundary")?;
    let mut csv = String::from("kind,a,b,c,d,e,f,g,h,i,j,k\n");
    let mut pool = ColdPool::new()?;
    let scratch = Buffer::new(8 << 20)?;
    let base_bus = baseline_only(&mut c, sess, &mut pool)?;
    println!("baseline (n = 1 cold stream): {base_bus:.1} bus accesses");
    run_boundaries(&mut c, sess, &mut pool, &scratch, base_bus, &mut csv)?;
    std::fs::write(sess.day.join("prefetch_boundary_experiments.csv"), csv)?;
    println!("boundary: {} reps, {} flagged invalid", c.total_reps, c.invalid_reps);
    Ok(())
}

pub fn run_prefetch(sess: &Session) -> Result<()> {
    let mut c = sess.collector("prefetch")?;
    let mut csv = String::from("kind,a,b,c,d,e,f,g,h,i,j,k\n");
    let random = run_stride_sweep(&mut c, sess, &mut csv)?;
    let dram_random = *random.get("DRAM").ok_or_else(|| anyhow!("no DRAM reference"))?;
    run_streams(&mut c, sess, dram_random, &mut csv)?;
    let mut pool = ColdPool::new()?;
    let scratch = Buffer::new(8 << 20)?;
    let base_bus = run_overshoot(&mut c, sess, &mut pool, &mut csv)?;
    run_boundaries(&mut c, sess, &mut pool, &scratch, base_bus, &mut csv)?;
    run_stores(&mut c, sess, &mut csv)?;
    std::fs::write(sess.day.join("prefetch_experiments.csv"), csv)?;
    println!("prefetch: {} reps, {} flagged invalid; data in {}", c.total_reps, c.invalid_reps, sess.day.display());
    Ok(())
}
