//! Measurement harness: warm-up, guarded repetitions, validity flags, per-rep JSONL records and
//! summary statistics computed on valid repetitions only.

use crate::guard::{self, Snapshot};
use crate::output::Jsonl;
use crate::stats::Summary;
use anyhow::Result;
use serde_json::{json, Value};
use std::time::Duration;

pub const WARMUP_REPS: usize = 3;

/// Expectation derived from the code under test (not from documentation).
#[derive(Clone, Copy)]
pub struct Exp {
    pub value: f64,
    pub tol_rel: f64,
    pub tol_abs: f64,
}

#[derive(Clone, Copy)]
pub struct Metric {
    pub name: &'static str,
    pub unit: &'static str,
    pub expected: Option<Exp>,
}

pub fn metric(name: &'static str, unit: &'static str) -> Metric {
    Metric { name, unit, expected: None }
}

pub fn metric_exp(name: &'static str, unit: &'static str, value: f64, tol_rel: f64, tol_abs: f64) -> Metric {
    Metric { name, unit, expected: Some(Exp { value, tol_rel, tol_abs }) }
}

pub struct Collector {
    pub w: Jsonl,
    pub cpu: usize,
    pub expected_khz: u64,
    pub rows: Vec<String>,
    pub invalid_reps: usize,
    pub total_reps: usize,
}

impl Collector {
    pub fn new(w: Jsonl, cpu: usize, expected_khz: u64) -> Collector {
        Collector { w, cpu, expected_khz, rows: Vec::new(), invalid_reps: 0, total_reps: 0 }
    }

    /// Runs `f` WARMUP_REPS times unrecorded, then `n` guarded repetitions. `f` returns one value
    /// per entry of `metrics`, in order.
    pub fn reps(
        &mut self,
        exp: &str,
        n: usize,
        metrics: &[Metric],
        mut f: impl FnMut() -> Result<Vec<f64>>,
    ) -> Result<()> {
        for _ in 0..WARMUP_REPS {
            f()?;
        }
        let mut valid_vals: Vec<Vec<f64>> = vec![Vec::new(); metrics.len()];
        for rep in 0..n {
            guard::wait_until_cool(Duration::from_secs(300))?;
            let before: Snapshot = guard::snapshot(self.cpu)?;
            let vals = f()?;
            let after: Snapshot = guard::snapshot(self.cpu)?;
            assert_eq!(vals.len(), metrics.len(), "metric count mismatch in {exp}");
            let reasons = guard::evaluate(&before, &after, Some(self.expected_khz));
            let valid = reasons.is_empty();
            self.total_reps += 1;
            if !valid {
                self.invalid_reps += 1;
            }
            let data: serde_json::Map<String, Value> =
                metrics.iter().zip(&vals).map(|(m, v)| (m.name.to_string(), json!(v))).collect();
            self.w.write(&json!({
                "kind": "rep", "exp": exp, "rep": rep, "core": self.cpu,
                "meta": {
                    "temp_before_c": before.temp_c, "temp_after_c": after.temp_c,
                    "freq_before_khz": before.freq_khz, "freq_after_khz": after.freq_khz,
                    "valid": valid, "invalid_reasons": reasons,
                },
                "data": data,
            }))?;
            if valid {
                for (dst, v) in valid_vals.iter_mut().zip(&vals) {
                    dst.push(*v);
                }
            }
        }
        for (m, vals) in metrics.iter().zip(&valid_vals) {
            self.summarize(exp, n, m, vals)?;
        }
        Ok(())
    }

    fn summarize(&mut self, exp: &str, n_total: usize, m: &Metric, vals: &[f64]) -> Result<()> {
        let Some(s) = Summary::from(vals) else {
            self.rows.push(format!("{exp:<46} {:<22} NO VALID REPETITION (0/{n_total})", m.name));
            self.w.write(&json!({"kind": "summary", "exp": exp, "metric": m.name, "unit": m.unit,
                "n_total": n_total, "n_valid": 0}))?;
            return Ok(());
        };
        let (expected, verdict) = match m.expected {
            Some(e) => {
                let ok = (s.median - e.value).abs() <= (e.tol_rel * e.value.abs()).max(e.tol_abs);
                (Some(e.value), Some(if ok { "within tolerance" } else { "OUTSIDE tolerance" }))
            }
            None => (None, None),
        };
        self.rows.push(format!(
            "{exp:<46} {:<22} n={}/{} med={:<14.6} mad={:<12.6} ci95=[{:.6}, {:.6}] {}{}",
            m.name,
            s.n,
            n_total,
            s.median,
            s.mad,
            s.ci95_lo,
            s.ci95_hi,
            m.unit,
            match (expected, verdict) {
                (Some(e), Some(v)) => format!("  expected(code)={e} -> {v}"),
                _ => String::new(),
            }
        ));
        self.w.write(&json!({
            "kind": "summary", "exp": exp, "metric": m.name, "unit": m.unit,
            "n_total": n_total, "n_valid": s.n, "summary": s,
            "expected_from_code": expected, "verdict": verdict,
        }))
    }

    pub fn note(&mut self, v: &Value) -> Result<()> {
        self.w.write(v)
    }
}
