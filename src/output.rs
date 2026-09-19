use anyhow::{Context, Result};
use serde::Serialize;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;

/// Append-only JSON Lines writer; every record is flushed so a crash keeps prior data.
pub struct Jsonl {
    w: BufWriter<File>,
}

impl Jsonl {
    pub fn create(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
        }
        let f = File::create(path).with_context(|| format!("create {}", path.display()))?;
        Ok(Jsonl { w: BufWriter::new(f) })
    }

    pub fn write<T: Serialize>(&mut self, record: &T) -> Result<()> {
        serde_json::to_writer(&mut self.w, record)?;
        self.w.write_all(b"\n")?;
        self.w.flush()?;
        Ok(())
    }
}

/// Days since 1970-01-01 -> (year, month, day), proleptic Gregorian (H. Hinnant's algorithm).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

pub fn date_string(unix_secs: u64) -> String {
    let (y, m, d) = civil_from_days((unix_secs / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    date_string(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dates() {
        assert_eq!(date_string(0), "1970-01-01");
        assert_eq!(date_string(951_782_400), "2000-02-29");
        assert_eq!(date_string(1_000_000_000), "2001-09-09");
    }

    #[test]
    fn jsonl_roundtrip() {
        let path = std::env::temp_dir().join("a76probe_jsonl_test").join("t.jsonl");
        let mut w = Jsonl::create(&path).unwrap();
        w.write(&serde_json::json!({"a": 1})).unwrap();
        w.write(&serde_json::json!({"b": [1, 2]})).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert_eq!(text.lines().next().unwrap(), r#"{"a":1}"#);
    }
}
