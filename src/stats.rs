use serde::Serialize;

/// SplitMix64: small, fast, deterministic PRNG (bootstrap, permutations).
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (multiply-shift, negligible bias for our n).
    pub fn below(&mut self, n: u64) -> u64 {
        ((self.next_u64() as u128 * n as u128) >> 64) as u64
    }
}

/// Linear-interpolated percentile of an already sorted slice, `p` in `[0, 1]`.
pub fn percentile_sorted(sorted: &[f64], p: f64) -> f64 {
    assert!(!sorted.is_empty());
    let pos = p.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64)
}

fn sorted_copy(data: &[f64]) -> Vec<f64> {
    let mut v = data.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).expect("NaN in stats input"));
    v
}

pub fn median(data: &[f64]) -> f64 {
    percentile_sorted(&sorted_copy(data), 0.5)
}

/// Median absolute deviation (raw, not rescaled to sigma).
pub fn mad(data: &[f64]) -> f64 {
    let m = median(data);
    let dev: Vec<f64> = data.iter().map(|x| (x - m).abs()).collect();
    median(&dev)
}

/// Percentile bootstrap confidence interval of the median, level `1 - alpha`.
pub fn bootstrap_median_ci(data: &[f64], resamples: usize, alpha: f64, rng: &mut Rng) -> (f64, f64) {
    let n = data.len();
    let mut meds = Vec::with_capacity(resamples);
    let mut buf = vec![0.0; n];
    for _ in 0..resamples {
        for slot in buf.iter_mut() {
            *slot = data[rng.below(n as u64) as usize];
        }
        meds.push(median(&buf));
    }
    meds.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (
        percentile_sorted(&meds, alpha / 2.0),
        percentile_sorted(&meds, 1.0 - alpha / 2.0),
    )
}

/// Ordinary least squares `y = slope * x + intercept`.
pub fn linear_fit(x: &[f64], y: &[f64]) -> (f64, f64) {
    assert_eq!(x.len(), y.len());
    let n = x.len() as f64;
    let (mx, my) = (x.iter().sum::<f64>() / n, y.iter().sum::<f64>() / n);
    let sxx: f64 = x.iter().map(|a| (a - mx) * (a - mx)).sum();
    let sxy: f64 = x.iter().zip(y).map(|(a, b)| (a - mx) * (b - my)).sum();
    let slope = if sxx == 0.0 { 0.0 } else { sxy / sxx };
    (slope, my - slope * mx)
}

/// Percentile bootstrap (95 %) of the least-squares slope, resampling (x, y) pairs.
pub fn bootstrap_slope_ci(x: &[f64], y: &[f64], resamples: usize, rng: &mut Rng) -> (f64, f64) {
    let n = x.len();
    let mut slopes = Vec::with_capacity(resamples);
    let (mut bx, mut by) = (vec![0.0; n], vec![0.0; n]);
    for _ in 0..resamples {
        for i in 0..n {
            let j = rng.below(n as u64) as usize;
            bx[i] = x[j];
            by[i] = y[j];
        }
        slopes.push(linear_fit(&bx, &by).0);
    }
    slopes.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (percentile_sorted(&slopes, 0.025), percentile_sorted(&slopes, 0.975))
}

/// A discontinuity of a piecewise-linear curve between two consecutive sample points.
#[derive(Clone, Debug)]
pub struct Jump {
    pub lo: usize,
    pub hi: usize,
    pub size: f64,
}

/// Jumps of `t(n)` larger than `min_size` above the trend given by the slope of the last six points.
pub fn jumps(ns: &[usize], t: &[f64], min_size: f64) -> Vec<Jump> {
    let m = t.len();
    if m < 8 {
        return Vec::new();
    }
    let slope = (t[m - 1] - t[m - 6]) / (ns[m - 1] as f64 - ns[m - 6] as f64);
    (0..m - 1)
        .filter_map(|i| {
            let size = t[i + 1] - t[i] - slope * (ns[i + 1] - ns[i]) as f64;
            (size > min_size).then_some(Jump { lo: ns[i], hi: ns[i + 1], size })
        })
        .collect()
}

#[derive(Clone, Debug, Serialize)]
pub struct Summary {
    pub n: usize,
    pub min: f64,
    pub p5: f64,
    pub p25: f64,
    pub median: f64,
    pub p75: f64,
    pub p95: f64,
    pub max: f64,
    pub mean: f64,
    pub mad: f64,
    pub ci95_lo: f64,
    pub ci95_hi: f64,
}

impl Summary {
    /// `None` on empty input. Bootstrap seed is fixed so reports are reproducible.
    pub fn from(data: &[f64]) -> Option<Summary> {
        if data.is_empty() {
            return None;
        }
        let s = sorted_copy(data);
        let (lo, hi) = bootstrap_median_ci(data, 2000, 0.05, &mut Rng::new(0xA76));
        Some(Summary {
            n: data.len(),
            min: s[0],
            p5: percentile_sorted(&s, 0.05),
            p25: percentile_sorted(&s, 0.25),
            median: percentile_sorted(&s, 0.5),
            p75: percentile_sorted(&s, 0.75),
            p95: percentile_sorted(&s, 0.95),
            max: s[s.len() - 1],
            mean: data.iter().sum::<f64>() / data.len() as f64,
            mad: mad(data),
            ci95_lo: lo,
            ci95_hi: hi,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_odd_even() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(median(&[7.0]), 7.0);
    }

    #[test]
    fn mad_known_value() {
        // median 3, deviations [2,1,0,1,97] -> median 1
        assert_eq!(mad(&[1.0, 2.0, 3.0, 4.0, 100.0]), 1.0);
    }

    #[test]
    fn percentile_interpolates() {
        let s = [0.0, 10.0, 20.0, 30.0, 40.0];
        assert_eq!(percentile_sorted(&s, 0.0), 0.0);
        assert_eq!(percentile_sorted(&s, 1.0), 40.0);
        assert_eq!(percentile_sorted(&s, 0.125), 5.0);
    }

    #[test]
    fn rng_is_deterministic_and_bounded() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        let mut r = Rng::new(1);
        for _ in 0..10_000 {
            assert!(r.below(7) < 7);
        }
    }

    #[test]
    fn rng_below_covers_range() {
        let mut r = Rng::new(9);
        let mut seen = [false; 5];
        for _ in 0..1000 {
            seen[r.below(5) as usize] = true;
        }
        assert!(seen.iter().all(|&s| s));
    }

    #[test]
    fn bootstrap_ci_brackets_median() {
        let data: Vec<f64> = (0..50).map(|i| 100.0 + (i % 7) as f64).collect();
        let m = median(&data);
        let (lo, hi) = bootstrap_median_ci(&data, 500, 0.05, &mut Rng::new(3));
        assert!(lo <= m && m <= hi, "{lo} {m} {hi}");
        assert!(lo >= 100.0 && hi <= 106.0);
    }

    #[test]
    fn linear_fit_recovers_line_and_ci_brackets_slope() {
        let x: Vec<f64> = (0..40).map(|i| i as f64 / 10.0).collect();
        let y: Vec<f64> = x.iter().enumerate().map(|(i, v)| 3.0 * v + 2.0 + if i % 2 == 0 { 0.05 } else { -0.05 }).collect();
        let (m, b) = linear_fit(&x, &y);
        assert!((m - 3.0).abs() < 0.05 && (b - 2.0).abs() < 0.1, "{m} {b}");
        let (lo, hi) = bootstrap_slope_ci(&x, &y, 300, &mut Rng::new(4));
        assert!(lo <= m && m <= hi && hi - lo < 0.2, "{lo} {m} {hi}");
    }

    #[test]
    fn jump_detector_finds_the_step() {
        let ns: Vec<usize> = (0..40).map(|i| i * 4).collect();
        // slope 0.25 with a +20 step after n = 100
        let t: Vec<f64> = ns.iter().map(|&n| 9.0 + 0.25 * n as f64 + if n > 100 { 20.0 } else { 0.0 }).collect();
        let j = jumps(&ns, &t, 5.0);
        assert_eq!(j.len(), 1, "{j:?}");
        assert_eq!((j[0].lo, j[0].hi), (100, 104));
        assert!((j[0].size - 20.0).abs() < 1e-9);
        assert!(jumps(&ns, &ns.iter().map(|&n| n as f64 * 0.25).collect::<Vec<_>>(), 5.0).is_empty());
    }

    #[test]
    fn summary_constant_data() {
        let s = Summary::from(&[5.0; 30]).unwrap();
        assert_eq!((s.median, s.mad, s.ci95_lo, s.ci95_hi), (5.0, 0.0, 5.0, 5.0));
        assert!(Summary::from(&[]).is_none());
    }
}
