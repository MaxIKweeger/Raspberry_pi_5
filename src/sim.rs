//! Software models of one cache set under several replacement policies, and the access patterns
//! used to tell them apart. Hardware miss rates measured on a real set are compared with the
//! steady-state miss rates these models produce on the very same trace.

use crate::stats::Rng;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Lru,
    TreePlru,
    Fifo,
    Random,
    Srrip,
    Brrip,
    /// One reference bit per way, victim = lowest way without the bit (bits reset when all set).
    Nru,
    /// 2-bit SRRIP with frequency-priority promotion (a hit lowers RRPV by one instead of to 0).
    SrripFp,
}

impl Kind {
    pub const ALL: [Kind; 8] =
        [Kind::Lru, Kind::TreePlru, Kind::Fifo, Kind::Random, Kind::Srrip, Kind::Brrip, Kind::Nru, Kind::SrripFp];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Lru => "LRU",
            Kind::TreePlru => "tree-PLRU",
            Kind::Fifo => "FIFO",
            Kind::Random => "random",
            Kind::Srrip => "SRRIP",
            Kind::Brrip => "BRRIP",
            Kind::Nru => "NRU",
            Kind::SrripFp => "SRRIP-FP",
        }
    }

    pub fn is_stochastic(self) -> bool {
        matches!(self, Kind::Random | Kind::Brrip)
    }
}

/// One set of `ways` lines. Empty ways are always filled first (lowest index), as in hardware
/// that fills invalid ways before choosing a victim.
pub struct CacheSet {
    kind: Kind,
    ways: usize,
    tags: Vec<Option<u16>>,
    stamp: Vec<u64>,
    rrpv: Vec<u8>,
    ref_bit: Vec<bool>,
    plru: Vec<bool>,
    clock: u64,
    rng: Rng,
}

impl CacheSet {
    pub fn new(kind: Kind, ways: usize, seed: u64) -> CacheSet {
        assert!(ways >= 1);
        if kind == Kind::TreePlru {
            assert!(ways.is_power_of_two(), "tree-PLRU needs a power-of-two way count");
        }
        CacheSet {
            kind,
            ways,
            tags: vec![None; ways],
            stamp: vec![0; ways],
            rrpv: vec![3; ways],
            ref_bit: vec![false; ways],
            plru: vec![false; ways.saturating_sub(1)],
            clock: 0,
            rng: Rng::new(seed),
        }
    }

    /// A set already full of unrelated lines (ids >= 10000) with random replacement state, the way a
    /// hardware set looks before an experiment starts: which limit cycle a deterministic policy
    /// settles into can depend on this state.
    pub fn randomized(kind: Kind, ways: usize, seed: u64) -> CacheSet {
        let mut s = CacheSet::new(kind, ways, seed);
        let mut r = Rng::new(seed ^ 0xD1CE);
        for w in 0..ways {
            // About half of the ways start invalid: they are filled first, which fixes the way
            // layout (and hence the limit cycle) of deterministic pseudo-LRU policies.
            s.tags[w] = if r.below(2) == 0 { None } else { Some(10_000 + w as u16) };
            s.rrpv[w] = r.below(4) as u8;
            s.ref_bit[w] = r.below(2) == 1;
            s.stamp[w] = r.below(1 << 20);
        }
        if s.ref_bit.iter().all(|&b| b) {
            s.ref_bit[r.below(ways as u64) as usize] = false;
        }
        for b in s.plru.iter_mut() {
            *b = r.below(2) == 1;
        }
        s.clock = 1 << 21;
        s
    }

    fn plru_touch(&mut self, way: usize) {
        let (mut idx, mut lo, mut hi) = (0usize, 0usize, self.ways);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if way < mid {
                self.plru[idx] = true; // next victim is in the right half
                idx = 2 * idx + 1;
                hi = mid;
            } else {
                self.plru[idx] = false;
                idx = 2 * idx + 2;
                lo = mid;
            }
        }
    }

    fn plru_victim(&self) -> usize {
        let (mut idx, mut lo, mut hi) = (0usize, 0usize, self.ways);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if self.plru[idx] {
                idx = 2 * idx + 2;
                lo = mid;
            } else {
                idx = 2 * idx + 1;
                hi = mid;
            }
        }
        lo
    }

    fn victim(&mut self) -> usize {
        if let Some(w) = self.tags.iter().position(|t| t.is_none()) {
            return w;
        }
        match self.kind {
            Kind::Lru | Kind::Fifo => (0..self.ways).min_by_key(|&w| self.stamp[w]).unwrap(),
            Kind::Random => self.rng.below(self.ways as u64) as usize,
            Kind::TreePlru => self.plru_victim(),
            Kind::Nru => loop {
                if let Some(w) = self.ref_bit.iter().position(|&b| !b) {
                    return w;
                }
                self.ref_bit.iter_mut().for_each(|b| *b = false);
            },
            Kind::Srrip | Kind::Brrip | Kind::SrripFp => loop {
                if let Some(w) = self.rrpv.iter().position(|&r| r == 3) {
                    return w;
                }
                self.rrpv.iter_mut().for_each(|r| *r += 1);
            },
        }
    }

    /// Returns true on a hit.
    pub fn access(&mut self, line: u16) -> bool {
        self.clock += 1;
        if let Some(w) = self.tags.iter().position(|t| *t == Some(line)) {
            match self.kind {
                Kind::Lru => self.stamp[w] = self.clock,
                Kind::TreePlru => self.plru_touch(w),
                Kind::Srrip | Kind::Brrip => self.rrpv[w] = 0,
                Kind::SrripFp => self.rrpv[w] = self.rrpv[w].saturating_sub(1),
                Kind::Nru => self.ref_bit[w] = true,
                Kind::Fifo | Kind::Random => {}
            }
            return true;
        }
        let w = self.victim();
        self.tags[w] = Some(line);
        match self.kind {
            Kind::Lru | Kind::Fifo => self.stamp[w] = self.clock,
            Kind::TreePlru => self.plru_touch(w),
            Kind::Srrip | Kind::SrripFp => self.rrpv[w] = 2,
            Kind::Nru => self.ref_bit[w] = true,
            Kind::Brrip => self.rrpv[w] = if self.rng.below(32) == 0 { 2 } else { 3 },
            Kind::Random => {}
        }
        false
    }
}

/// Steady-state miss rate of `trace` (replayed `warm` times unmeasured, then `measure` times).
pub fn miss_rate(kind: Kind, ways: usize, trace: &[u16], warm: usize, measure: usize, seed: u64) -> f64 {
    let mut set = CacheSet::new(kind, ways, seed);
    for _ in 0..warm {
        for &l in trace {
            set.access(l);
        }
    }
    let mut misses = 0usize;
    for _ in 0..measure {
        for &l in trace {
            if !set.access(l) {
                misses += 1;
            }
        }
    }
    misses as f64 / (measure * trace.len()) as f64
}

/// Steady-state miss rate starting from a random full-set state (see [`CacheSet::randomized`]).
pub fn miss_rate_random_start(kind: Kind, ways: usize, trace: &[u16], seed: u64) -> f64 {
    let mut set = CacheSet::randomized(kind, ways, seed);
    for _ in 0..8 {
        for &l in trace {
            set.access(l);
        }
    }
    let mut misses = 0usize;
    for _ in 0..24 {
        for &l in trace {
            if !set.access(l) {
                misses += 1;
            }
        }
    }
    misses as f64 / (24 * trace.len()) as f64
}

/// Steady-state rates reached from `n` random initial states, sorted.
pub fn rate_support(kind: Kind, ways: usize, trace: &[u16], n: usize) -> Vec<f64> {
    let mut v: Vec<f64> = (0..n as u64).map(|s| miss_rate_random_start(kind, ways, trace, 500 + s)).collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v
}

/// Distinct steady-state rates (0.01 bins) that the model reaches with probability >= `min_mass`.
pub fn reachable(support: &[f64], min_mass: f64) -> Vec<f64> {
    let mut bins: std::collections::BTreeMap<i64, usize> = Default::default();
    for x in support {
        *bins.entry((x * 100.0).round() as i64).or_default() += 1;
    }
    bins.into_iter().filter(|(_, c)| *c as f64 / support.len() as f64 >= min_mass).map(|(b, _)| b as f64 / 100.0).collect()
}

/// Fraction of `values` lying within `tol` of some element of `reach`.
pub fn coverage(reach: &[f64], values: &[f64], tol: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().filter(|&&v| reach.iter().any(|&r| (r - v).abs() <= tol)).count() as f64 / values.len() as f64
}

/// Fraction of `support` lying within `tol` of `x`: how likely the model is to produce `x`.
pub fn mass_near(support: &[f64], x: f64, tol: f64) -> f64 {
    support.iter().filter(|&&s| (s - x).abs() <= tol).count() as f64 / support.len() as f64
}

/// Same, averaged over several seeds for the stochastic policies.
pub fn expected_miss_rate(kind: Kind, ways: usize, trace: &[u16]) -> f64 {
    let seeds = if kind.is_stochastic() { 24 } else { 1 };
    (0..seeds).map(|s| miss_rate(kind, ways, trace, 8, 24, 1000 + s)).sum::<f64>() / seeds as f64
}

pub const TRACE_LEN: usize = 4096;

pub struct Pattern {
    pub name: &'static str,
    pub lines: usize,
    pub trace: Vec<u16>,
}

fn fill(mut next: impl FnMut() -> u16) -> Vec<u16> {
    (0..TRACE_LEN).map(|_| next()).collect()
}

/// Access patterns over lines that all map to one set of a `ways`-way cache.
pub fn patterns(ways: usize) -> Vec<Pattern> {
    let n = ways + 1;
    let cyc = |m: usize| {
        let mut i = 0usize;
        fill(move || {
            let v = (i % m) as u16;
            i += 1;
            v
        })
    };
    let sawtooth = {
        let period: Vec<u16> = (0..n).chain((1..n - 1).rev()).map(|x| x as u16).collect();
        let mut i = 0usize;
        fill(move || {
            let v = period[i % period.len()];
            i += 1;
            v
        })
    };
    let random = |m: usize, seed: u64| {
        let mut r = Rng::new(seed);
        fill(move || r.below(m as u64) as u16)
    };
    let hot1 = {
        // 0, 1, 0, 2, 0, 3, ...: line 0 is re-referenced constantly, the others cycle.
        let mut i = 0usize;
        fill(move || {
            let step = i / 2;
            let v = if i % 2 == 0 { 0 } else { (1 + step % (n - 1)) as u16 };
            i += 1;
            v
        })
    };
    let hot_cold = {
        // W-1 hot lines swept twice, then two cold lines touched once each.
        let hot = n - 2;
        let period: Vec<u16> = (0..hot).chain(0..hot).chain([hot, hot + 1]).map(|x| x as u16).collect();
        let mut i = 0usize;
        fill(move || {
            let v = period[i % period.len()];
            i += 1;
            v
        })
    };
    let double = {
        let mut i = 0usize;
        fill(move || {
            let v = ((i / 2) % n) as u16;
            i += 1;
            v
        })
    };
    vec![
        Pattern { name: "cyclic W+1", lines: n, trace: cyc(n) },
        Pattern { name: "cyclic W+2", lines: n + 1, trace: cyc(n + 1) },
        Pattern { name: "sawtooth W+1", lines: n, trace: sawtooth },
        Pattern { name: "random W+1", lines: n, trace: random(n, 11) },
        Pattern { name: "random W+3", lines: n + 2, trace: random(n + 2, 12) },
        Pattern { name: "hot line + cycle", lines: n, trace: hot1 },
        Pattern { name: "hot set + 2 cold", lines: n, trace: hot_cold },
        Pattern { name: "each line twice", lines: n, trace: double },
    ]
}

/// Root-mean-square difference between two miss-rate signatures.
pub fn rms(a: &[f64], b: &[f64]) -> f64 {
    assert_eq!(a.len(), b.len());
    (a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum::<f64>() / a.len() as f64).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cyc(m: usize) -> Vec<u16> {
        (0..TRACE_LEN).map(|i| (i % m) as u16).collect()
    }

    #[test]
    fn lru_cyclic_w_plus_one_always_misses_and_w_never_misses() {
        // The 4096-entry trace wraps once per pass, which yields at most one hit per pass.
        assert!(miss_rate(Kind::Lru, 8, &cyc(9), 4, 8, 1) > 0.999);
        assert_eq!(miss_rate(Kind::Lru, 8, &cyc(8), 4, 8, 1), 0.0);
        assert!(miss_rate(Kind::Lru, 4, &cyc(5), 4, 8, 1) > 0.999);
    }

    #[test]
    fn fifo_cyclic_w_plus_one_always_misses() {
        assert!(miss_rate(Kind::Fifo, 8, &cyc(9), 4, 8, 1) > 0.999);
        assert_eq!(miss_rate(Kind::Fifo, 8, &cyc(8), 4, 8, 1), 0.0);
    }

    #[test]
    fn every_policy_holds_w_lines_without_misses() {
        for k in Kind::ALL {
            assert_eq!(miss_rate(k, 8, &cyc(8), 8, 8, 5), 0.0, "{}", k.name());
        }
    }

    #[test]
    fn random_is_between_extremes_and_seed_deterministic() {
        let r = miss_rate(Kind::Random, 8, &cyc(9), 8, 24, 3);
        assert!(r > 0.05 && r < 0.95, "{r}");
        assert_eq!(r, miss_rate(Kind::Random, 8, &cyc(9), 8, 24, 3));
    }

    #[test]
    fn plru_on_cyclic_w_plus_one_is_not_always_a_miss_for_4_ways() {
        // Known property: tree-PLRU keeps some lines of a cyclic W+1 loop, unlike true LRU.
        let plru = miss_rate(Kind::TreePlru, 4, &cyc(5), 8, 16, 1);
        assert!(plru < 1.0, "{plru}");
    }

    #[test]
    fn plru_hand_traced_fill_and_evict() {
        // 4 ways, fill 0..3 then access 4: tree-PLRU must evict line 0 (the first one touched).
        let mut s = CacheSet::new(Kind::TreePlru, 4, 1);
        for l in 0..4 {
            assert!(!s.access(l));
        }
        assert!(!s.access(4));
        assert!(!s.access(0), "line 0 should have been the victim");
    }

    #[test]
    fn srrip_promotes_hits_and_ages_on_miss() {
        let mut s = CacheSet::new(Kind::Srrip, 2, 1);
        assert!(!s.access(0));
        assert!(!s.access(1));
        assert!(s.access(0)); // 0 -> rrpv 0
        assert!(!s.access(2)); // ages both, evicts 1 (rrpv reaches 3 first)
        assert!(s.access(0), "hot line survives");
        assert!(!s.access(1), "line 1 was evicted");
    }

    #[test]
    fn nru_and_srrip_fp_hand_traced() {
        // NRU, 2 ways: 0,1 fill (both bits set); access 2 -> all set, reset, evict way 0 (line 0).
        let mut s = CacheSet::new(Kind::Nru, 2, 1);
        assert!(!s.access(0));
        assert!(!s.access(1));
        assert!(!s.access(2));
        assert!(s.access(1), "line 1 kept its slot");
        // SRRIP-FP: a single hit only lowers RRPV from 2 to 1, so line 0 still ages out first.
        let mut f = CacheSet::new(Kind::SrripFp, 2, 1);
        assert!(!f.access(0));
        assert!(!f.access(1));
        assert!(f.access(0));
        assert!(!f.access(2));
        assert!(f.access(0), "hot line 0 (rrpv 1) survives; line 1 (rrpv 3 after aging) is the victim");
    }

    #[test]
    fn lru_and_fifo_differ_on_a_hot_line() {
        let ps = patterns(8);
        let hot = ps.iter().find(|p| p.name == "hot line + cycle").unwrap();
        let lru = expected_miss_rate(Kind::Lru, 8, &hot.trace);
        let fifo = expected_miss_rate(Kind::Fifo, 8, &hot.trace);
        assert!(fifo > lru, "fifo {fifo} lru {lru}");
    }

    #[test]
    fn patterns_stay_within_their_line_count() {
        for w in [4usize, 8] {
            for p in patterns(w) {
                assert_eq!(p.trace.len(), TRACE_LEN);
                assert!(p.trace.iter().all(|&l| (l as usize) < p.lines), "{}", p.name);
                assert!((0..p.lines).all(|l| p.trace.contains(&(l as u16))), "{} misses a line", p.name);
            }
        }
    }

    #[test]
    fn policies_have_distinct_signatures_for_w8() {
        let ps = patterns(8);
        let sig = |k: Kind| ps.iter().map(|p| expected_miss_rate(k, 8, &p.trace)).collect::<Vec<_>>();
        let all: Vec<_> = Kind::ALL.iter().map(|&k| (k, sig(k))).collect();
        for i in 0..all.len() {
            for j in i + 1..all.len() {
                assert!(rms(&all[i].1, &all[j].1) > 0.01, "{} vs {}", all[i].0.name(), all[j].0.name());
            }
        }
    }

    #[test]
    fn random_start_reaches_several_limit_cycles_for_plru() {
        let ps = patterns(8);
        let hot = ps.iter().find(|p| p.name == "hot line + cycle").unwrap();
        let sup = rate_support(Kind::TreePlru, 8, &hot.trace, 200);
        let mut distinct: Vec<i64> = sup.iter().map(|x| (x * 1000.0).round() as i64).collect();
        distinct.dedup();
        assert!(distinct.len() >= 2, "{distinct:?}");
        assert!(mass_near(&sup, sup[0], 0.005) > 0.0);
    }

    #[test]
    fn lru_has_a_single_steady_state_from_any_start() {
        let ps = patterns(8);
        for p in &ps {
            if p.name.starts_with("random") {
                continue;
            }
            let sup = rate_support(Kind::Lru, 8, &p.trace, 50);
            assert!(sup[sup.len() - 1] - sup[0] < 0.01, "{}: {} .. {}", p.name, sup[0], sup[sup.len() - 1]);
        }
    }

    #[test]
    #[ignore]
    fn explore_signature_supports() {
        let ps = patterns(8);
        for k in Kind::ALL {
            println!("== {}", k.name());
            for p in &ps {
                let sup = rate_support(k, 8, &p.trace, 300);
                let mut hist: std::collections::BTreeMap<i64, usize> = Default::default();
                for x in &sup {
                    *hist.entry((x * 200.0).round() as i64).or_default() += 1;
                }
                let h: Vec<String> = hist.iter().map(|(b, c)| format!("{:.3}:{}", *b as f64 / 200.0, c)).collect();
                println!("  {:<18} {}", p.name, h.join(" "));
            }
        }
    }

    #[test]
    fn coverage_and_reachable() {
        let sup = [0.5, 0.5, 0.5, 0.31, 0.5];
        assert_eq!(reachable(&sup, 0.1), vec![0.31, 0.5]);
        assert_eq!(reachable(&sup, 0.5), vec![0.5]);
        assert_eq!(coverage(&[0.31, 0.5], &[0.312, 0.5, 0.9], 0.012), 2.0 / 3.0);
        assert_eq!(coverage(&[], &[0.1], 0.01), 0.0);
    }

    #[test]
    fn rms_basic() {
        assert_eq!(rms(&[0.0, 0.0], &[0.0, 0.0]), 0.0);
        assert!((rms(&[1.0, 0.0], &[0.0, 0.0]) - 0.5f64.sqrt()).abs() < 1e-12);
    }
}
