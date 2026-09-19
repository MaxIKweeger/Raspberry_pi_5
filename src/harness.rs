//! Measurement harness: warm-up, guarded repetitions, validity flags, per-rep JSONL records and
//! summary statistics computed on valid repetitions only.

use crate::guard::{self, Snapshot};
use crate::output::{self, Jsonl};
use crate::pmu::{Group, Pmu};
use crate::stats::Summary;
use crate::timing::{cntfrq, cntvct, ticks_to_ns};
use crate::{affinity, kernels};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
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

/// Runs `f` inside a PMU window bracketed by CNTVCT reads; returns (elapsed ns, counter values).
/// Fails if the group was multiplexed (counts would be scaled estimates, not measurements).
pub fn run_window(g: &Group, frq: u64, f: impl FnOnce()) -> Result<(f64, Vec<u64>)> {
    g.reset_enable()?;
    let t0 = cntvct();
    f();
    let t1 = cntvct();
    g.disable()?;
    let r = g.read()?;
    if r.multiplexed() {
        bail!("group was multiplexed ({}/{} ns running)", r.time_running, r.time_enabled);
    }
    Ok((ticks_to_ns(t1 - t0, frq), r.values))
}

/// Everything an experiment run needs: pinned core, PMU description, clock, output directory.
pub struct Session {
    pub day: PathBuf,
    pub cpu: usize,
    pub repeat: usize,
    pub pmu: Pmu,
    pub frq: u64,
    pub max_khz: u64,
}

impl Session {
    pub fn start(cpu: usize, repeat: usize, out_dir: &Path) -> Result<Session> {
        affinity::pin_to(cpu)?;
        let pmu = Pmu::discover()?;
        let max_khz = guard::max_freq_khz(cpu)?;
        let day = out_dir.join(output::today_utc());
        std::fs::create_dir_all(&day)?;
        let env = crate::env::Env::collect();
        std::fs::write(day.join("env.json"), serde_json::to_string_pretty(&env)?)?;
        // Ramp the governor to its top frequency before recording anything.
        kernels::alu_indep(std::hint::black_box(150_000_000));
        kernels::alu_indep(std::hint::black_box(150_000_000));
        Ok(Session { day, cpu, repeat, pmu, frq: cntfrq(), max_khz })
    }

    pub fn collector(&self, name: &str) -> Result<Collector> {
        let w = Jsonl::create(&self.day.join("raw").join(format!("{name}.jsonl")))?;
        Ok(Collector::new(w, self.cpu, self.max_khz))
    }
}

pub struct Collector {
    pub w: Jsonl,
    pub cpu: usize,
    pub expected_khz: u64,
    pub warmup: usize,
    pub rows: Vec<String>,
    pub invalid_reps: usize,
    pub total_reps: usize,
}

impl Collector {
    pub fn new(w: Jsonl, cpu: usize, expected_khz: u64) -> Collector {
        Collector { w, cpu, expected_khz, warmup: WARMUP_REPS, rows: Vec::new(), invalid_reps: 0, total_reps: 0 }
    }

    /// Runs `f` `warmup` times unrecorded, then `n` guarded repetitions, writing one JSONL record
    /// per repetition (with `tags` attached) but no summary. Returns the values of the valid
    /// repetitions per metric, and the number of repetitions run (valid or not).
    pub fn reps_raw(
        &mut self,
        exp: &str,
        tags: Value,
        n: usize,
        metrics: &[Metric],
        mut f: impl FnMut() -> Result<Vec<f64>>,
    ) -> Result<(Vec<Vec<f64>>, usize)> {
        for _ in 0..self.warmup {
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
                "kind": "rep", "exp": exp, "rep": rep, "core": self.cpu, "tags": tags,
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
        Ok((valid_vals, n))
    }

    /// Warm-up, `n` guarded repetitions, then one summary per metric.
    pub fn reps(
        &mut self,
        exp: &str,
        n: usize,
        metrics: &[Metric],
        f: impl FnMut() -> Result<Vec<f64>>,
    ) -> Result<Vec<Option<Summary>>> {
        let (vals, total) = self.reps_raw(exp, Value::Null, n, metrics, f)?;
        self.summarize_pooled(exp, total, metrics, &vals)
    }

    /// Summaries over values pooled from several `reps_raw` calls (e.g. alternating-order rounds).
    pub fn summarize_pooled(
        &mut self,
        exp: &str,
        n_total: usize,
        metrics: &[Metric],
        vals: &[Vec<f64>],
    ) -> Result<Vec<Option<Summary>>> {
        metrics.iter().zip(vals).map(|(m, v)| self.summarize(exp, n_total, m, v)).collect()
    }

    fn summarize(&mut self, exp: &str, n_total: usize, m: &Metric, vals: &[f64]) -> Result<Option<Summary>> {
        let Some(s) = Summary::from(vals) else {
            self.rows.push(format!("{exp:<46} {:<22} NO VALID REPETITION (0/{n_total})", m.name));
            self.w.write(&json!({"kind": "summary", "exp": exp, "metric": m.name, "unit": m.unit,
                "n_total": n_total, "n_valid": 0}))?;
            return Ok(None);
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
        }))?;
        Ok(Some(s))
    }

    pub fn note(&mut self, v: &Value) -> Result<()> {
        self.w.write(v)
    }
}
