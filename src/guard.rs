//! Thermal / frequency guard. A measurement is only valid if temperature stays below the abort
//! threshold and the CPU frequency is pinned at the expected value before and after.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::fs;
use std::time::{Duration, Instant};

pub const WAIT_ABOVE_C: f64 = 75.0;
pub const ABORT_ABOVE_C: f64 = 80.0;
const THERMAL: &str = "/sys/class/thermal/thermal_zone0/temp";

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Snapshot {
    pub temp_c: f64,
    pub freq_khz: u64,
}

fn read_num(path: &str) -> Result<f64> {
    fs::read_to_string(path)
        .with_context(|| format!("read {path}"))?
        .trim()
        .parse::<f64>()
        .with_context(|| format!("parse {path}"))
}

pub fn temp_c() -> Result<f64> {
    Ok(read_num(THERMAL)? / 1000.0)
}

pub fn freq_khz(cpu: usize) -> Result<u64> {
    Ok(read_num(&format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_cur_freq"))? as u64)
}

pub fn max_freq_khz(cpu: usize) -> Result<u64> {
    Ok(read_num(&format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_max_freq"))? as u64)
}

pub fn snapshot(cpu: usize) -> Result<Snapshot> {
    Ok(Snapshot { temp_c: temp_c()?, freq_khz: freq_khz(cpu)? })
}

/// Blocks while temperature is above `WAIT_ABOVE_C`; errors above `ABORT_ABOVE_C` or on timeout.
pub fn wait_until_cool(max_wait: Duration) -> Result<f64> {
    let start = Instant::now();
    loop {
        let t = temp_c()?;
        if t > ABORT_ABOVE_C {
            bail!("temperature {t:.1} C above abort threshold {ABORT_ABOVE_C} C");
        }
        if t <= WAIT_ABOVE_C {
            return Ok(t);
        }
        if start.elapsed() > max_wait {
            bail!("timeout waiting for temperature <= {WAIT_ABOVE_C} C (now {t:.1} C)");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Reasons why a measurement bracketed by `before`/`after` is invalid (empty = valid).
pub fn evaluate(before: &Snapshot, after: &Snapshot, expected_khz: Option<u64>) -> Vec<String> {
    let mut reasons = Vec::new();
    if before.temp_c > ABORT_ABOVE_C || after.temp_c > ABORT_ABOVE_C {
        reasons.push(format!("temperature above {ABORT_ABOVE_C} C"));
    }
    if before.freq_khz != after.freq_khz {
        reasons.push(format!("frequency changed {} -> {} kHz", before.freq_khz, after.freq_khz));
    }
    if let Some(e) = expected_khz {
        if before.freq_khz != e || after.freq_khz != e {
            reasons.push(format!(
                "frequency {}/{} kHz differs from expected {} kHz",
                before.freq_khz, after.freq_khz, e
            ));
        }
    }
    reasons
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: f64, f: u64) -> Snapshot {
        Snapshot { temp_c: t, freq_khz: f }
    }

    #[test]
    fn valid_when_stable() {
        assert!(evaluate(&s(60.0, 2_400_000), &s(62.0, 2_400_000), Some(2_400_000)).is_empty());
    }

    #[test]
    fn flags_freq_drift_and_heat() {
        assert_eq!(evaluate(&s(60.0, 2_400_000), &s(60.0, 1_500_000), None).len(), 1);
        assert!(!evaluate(&s(60.0, 2_400_000), &s(81.0, 2_400_000), None).is_empty());
        assert!(!evaluate(&s(60.0, 1_500_000), &s(60.0, 1_500_000), Some(2_400_000)).is_empty());
    }
}
