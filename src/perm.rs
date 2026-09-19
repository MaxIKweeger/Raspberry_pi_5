use crate::stats::Rng;

/// Sattolo's algorithm: a uniformly random permutation made of a single cycle of length `n`.
/// `next[i]` is the successor of node `i`; following it from any node visits all `n` nodes.
pub fn sattolo(n: usize, rng: &mut Rng) -> Vec<u32> {
    assert!(n >= 1 && n <= u32::MAX as usize);
    let mut p: Vec<u32> = (0..n as u32).collect();
    for i in (1..n).rev() {
        let j = rng.below(i as u64) as usize; // j in 0..i, strictly below i: guarantees one cycle
        p.swap(i, j);
    }
    p
}

/// True if following `next` from node 0 visits every node exactly once before returning.
pub fn is_single_cycle(next: &[u32]) -> bool {
    let n = next.len();
    let mut cur = 0usize;
    for step in 1..=n {
        cur = next[cur] as usize;
        if cur == 0 {
            return step == n;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_cycle_for_many_sizes() {
        for n in [1usize, 2, 3, 4, 7, 64, 1000, 4097] {
            for seed in 0..5 {
                let p = sattolo(n, &mut Rng::new(seed));
                assert!(is_single_cycle(&p), "n={n} seed={seed}");
            }
        }
    }

    #[test]
    fn is_permutation_without_fixed_points() {
        let n = 500;
        let p = sattolo(n, &mut Rng::new(11));
        let mut seen = vec![false; n];
        for (i, &v) in p.iter().enumerate() {
            assert!(!seen[v as usize]);
            seen[v as usize] = true;
            assert_ne!(v as usize, i, "fixed point at {i}");
        }
    }

    #[test]
    fn detects_multi_cycle() {
        assert!(!is_single_cycle(&[1, 0, 3, 2]));
        assert!(is_single_cycle(&[1, 2, 3, 0]));
    }

    #[test]
    fn deterministic_per_seed_and_varied_across_seeds() {
        let a = sattolo(100, &mut Rng::new(5));
        assert_eq!(a, sattolo(100, &mut Rng::new(5)));
        assert_ne!(a, sattolo(100, &mut Rng::new(6)));
    }
}
