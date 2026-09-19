//! Offline scoring of measured replacement signatures against the software models. Portable.
//!
//! For every policy and pattern the model is started from many random initial set states
//! (deterministic policies can settle into different limit cycles); the model "reaches" the steady
//! states that occur often enough. A measured repetition is scored by its distance to the nearest
//! reachable state; a policy is scored by the mean distance over all repetitions of all patterns.

use crate::sim::{self, Kind};
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

pub struct Score {
    pub kind: Kind,
    /// Mean, over patterns, of the mean distance from each repetition to the nearest reachable state.
    pub mean_dist: f64,
    /// Mean, over patterns, of the fraction of repetitions within 0.012 of a reachable state.
    pub coverage: f64,
    pub per_pattern_dist: Vec<f64>,
    pub per_pattern_cov: Vec<f64>,
}

pub fn nearest_distance(reach: &[f64], v: f64) -> f64 {
    reach.iter().map(|r| (r - v).abs()).fold(f64::INFINITY, f64::min)
}

/// `values[i]`: measured per-repetition rates of pattern `i` of `sim::patterns(ways)`.
pub fn score_policies(ways: usize, values: &[Vec<f64>]) -> Vec<Score> {
    let pats = sim::patterns(ways);
    assert_eq!(pats.len(), values.len());
    let mut out = Vec::new();
    for &k in Kind::ALL.iter().filter(|k| **k != Kind::TreePlru || ways.is_power_of_two()) {
        let (mut dists, mut covs) = (Vec::new(), Vec::new());
        for (p, vals) in pats.iter().zip(values) {
            let reach = sim::reachable(&sim::rate_support(k, ways, &p.trace, 300), 0.01);
            let d = if vals.is_empty() {
                f64::NAN
            } else {
                vals.iter().map(|&v| nearest_distance(&reach, v)).sum::<f64>() / vals.len() as f64
            };
            dists.push(d);
            covs.push(sim::coverage(&reach, vals, 0.012));
        }
        out.push(Score {
            kind: k,
            mean_dist: dists.iter().sum::<f64>() / dists.len() as f64,
            coverage: covs.iter().sum::<f64>() / covs.len() as f64,
            per_pattern_dist: dists,
            per_pattern_cov: covs,
        });
    }
    out.sort_by(|a, b| a.mean_dist.partial_cmp(&b.mean_dist).unwrap());
    out
}

/// Only a handful of textbook policies are modelled: never better than "medium".
pub fn confidence(scores: &[Score]) -> &'static str {
    let best = scores[0].mean_dist;
    let second = scores.get(1).map_or(f64::INFINITY, |s| s.mean_dist);
    if best <= 0.02 && second >= 2.0 * best + 0.01 {
        "medium"
    } else {
        "low"
    }
}

/// Per-pattern valid repetition values of `repl/<level>/...` records in a raw JSONL file.
pub fn load_pattern_values(path: &Path, level: &str, ways: usize) -> Result<Vec<Vec<f64>>> {
    let key = if level == "L1D" { "l1d_refill_per_access" } else { "l2d_refill_per_access" };
    let prefix = format!("repl/{level}/");
    let mut by_pat: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for line in std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?.lines() {
        let v: serde_json::Value = serde_json::from_str(line)?;
        if v["kind"] != "rep" || !v["meta"]["valid"].as_bool().unwrap_or(false) {
            continue;
        }
        let Some(exp) = v["exp"].as_str().and_then(|e| e.strip_prefix(prefix.as_str())) else { continue };
        if let Some(x) = v["data"][key].as_f64() {
            by_pat.entry(exp.to_string()).or_default().push(x);
        }
    }
    let pats = sim::patterns(ways);
    let mut out = Vec::new();
    for p in &pats {
        match by_pat.remove(p.name) {
            Some(v) => out.push(v),
            None => bail!("no repetitions for pattern '{}' at {level} (wrong --ways?)", p.name),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_the_generating_policy_first() {
        let pats = sim::patterns(4);
        let values: Vec<Vec<f64>> = pats.iter().map(|p| vec![sim::expected_miss_rate(Kind::Lru, 4, &p.trace); 5]).collect();
        let s = score_policies(4, &values);
        assert_eq!(s[0].kind, Kind::Lru);
        assert!(s[0].mean_dist < 0.01, "{}", s[0].mean_dist);
        assert!(s[1].mean_dist > s[0].mean_dist);
    }

    #[test]
    fn nearest_distance_basic() {
        assert!((nearest_distance(&[0.1, 0.5], 0.45) - 0.05).abs() < 1e-12);
        assert_eq!(nearest_distance(&[0.3], 0.3), 0.0);
    }

    #[test]
    fn confidence_needs_a_margin() {
        let mk = |d: f64| Score { kind: Kind::Lru, mean_dist: d, coverage: 1.0, per_pattern_dist: vec![], per_pattern_cov: vec![] };
        assert_eq!(confidence(&[mk(0.005), mk(0.05)]), "medium");
        assert_eq!(confidence(&[mk(0.005), mk(0.012)]), "low");
        assert_eq!(confidence(&[mk(0.05), mk(0.2)]), "low");
    }
}
