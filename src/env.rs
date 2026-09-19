//! Snapshot of everything about the machine that can influence (or explain) a measurement.

use crate::{guard, pmu, timing};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::process::Command;

fn rd(path: &str) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

#[derive(Serialize)]
pub struct CacheInfo {
    pub cpu: usize,
    pub index: usize,
    pub level: Option<String>,
    pub kind: Option<String>,
    pub size: Option<String>,
    pub ways: Option<String>,
    pub sets: Option<String>,
    pub line_size: Option<String>,
    pub shared_cpu_list: Option<String>,
}

#[derive(Serialize)]
pub struct CpuFreq {
    pub cpu: usize,
    pub governor: Option<String>,
    pub cur_khz: Option<String>,
    pub min_khz: Option<String>,
    pub max_khz: Option<String>,
    pub available_governors: Option<String>,
    pub available_frequencies: Option<String>,
}

#[derive(Serialize)]
pub struct PmuInfo {
    pub device: String,
    pub type_id: u32,
    pub cpus: String,
    pub events: BTreeMap<String, u64>,
}

#[derive(Serialize)]
pub struct Env {
    pub timestamp_utc: String,
    pub tool_version: &'static str,
    pub rustc: &'static str,
    pub board_model: Option<String>,
    pub kernel_release: Option<String>,
    pub kernel_version: Option<String>,
    pub kernel_cmdline: Option<String>,
    pub page_size: i64,
    pub online_cpus: Option<String>,
    pub cpu_part_cpuinfo: Option<String>,
    pub cpu_features: Option<String>,
    pub midr_el1_sysfs: Option<String>,
    pub midr_el1_mrs: String,
    pub cntfrq_hz: u64,
    pub caches: Vec<CacheInfo>,
    pub cpufreq: Vec<CpuFreq>,
    pub temperature_c: Option<f64>,
    pub vcgencmd_get_throttled: Option<String>,
    pub perf_event_paranoid: Option<String>,
    pub perf_user_access: Option<String>,
    pub thp_enabled: Option<String>,
    pub sysfs_hugepages_present: bool,
    pub meminfo_huge: Vec<String>,
    pub memlock_limit_bytes: Option<u64>,
    pub pmu: Option<PmuInfo>,
}

/// Results are published: hide hardware identifiers (MAC addresses) from the kernel command line.
fn redact_cmdline(cmdline: &str) -> String {
    cmdline
        .split_whitespace()
        .map(|tok| match tok.split_once('=') {
            Some((k, _)) if k.to_ascii_lowercase().contains("macaddr") => format!("{k}=<redacted>"),
            _ => tok.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn field_from_cpuinfo(key: &str) -> Option<String> {
    let text = fs::read_to_string("/proc/cpuinfo").ok()?;
    text.lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()))
}

fn memlock_limit() -> Option<u64> {
    let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: lim is a valid out-pointer.
    (unsafe { libc::getrlimit(libc::RLIMIT_MEMLOCK, &mut lim) } == 0).then_some(lim.rlim_cur)
}

impl Env {
    pub fn collect() -> Env {
        let cache_cpus = 0..4usize;
        let mut caches = Vec::new();
        for cpu in cache_cpus.clone() {
            for index in 0..8 {
                let d = format!("/sys/devices/system/cpu/cpu{cpu}/cache/index{index}");
                if fs::metadata(&d).is_err() {
                    break;
                }
                caches.push(CacheInfo {
                    cpu,
                    index,
                    level: rd(&format!("{d}/level")),
                    kind: rd(&format!("{d}/type")),
                    size: rd(&format!("{d}/size")),
                    ways: rd(&format!("{d}/ways_of_associativity")),
                    sets: rd(&format!("{d}/number_of_sets")),
                    line_size: rd(&format!("{d}/coherency_line_size")),
                    shared_cpu_list: rd(&format!("{d}/shared_cpu_list")),
                });
            }
        }
        let cpufreq = cache_cpus
            .map(|cpu| {
                let d = format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq");
                CpuFreq {
                    cpu,
                    governor: rd(&format!("{d}/scaling_governor")),
                    cur_khz: rd(&format!("{d}/scaling_cur_freq")),
                    min_khz: rd(&format!("{d}/scaling_min_freq")),
                    max_khz: rd(&format!("{d}/scaling_max_freq")),
                    available_governors: rd(&format!("{d}/scaling_available_governors")),
                    available_frequencies: rd(&format!("{d}/scaling_available_frequencies")),
                }
            })
            .collect();
        let midr: u64;
        // SAFETY: MIDR_EL1 reads are trapped and emulated by Linux for EL0 (ID register emulation).
        unsafe { core::arch::asm!("mrs {v}, midr_el1", v = out(reg) midr, options(nomem, nostack, preserves_flags)) };
        let throttled = Command::new("vcgencmd")
            .arg("get_throttled")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        Env {
            timestamp_utc: crate::output::today_utc(),
            tool_version: env!("CARGO_PKG_VERSION"),
            rustc: env!("A76_RUSTC"),
            board_model: fs::read("/proc/device-tree/model")
                .ok()
                .map(|b| String::from_utf8_lossy(&b).trim_end_matches('\0').to_string()),
            kernel_release: rd("/proc/sys/kernel/osrelease"),
            kernel_version: rd("/proc/sys/kernel/version"),
            kernel_cmdline: rd("/proc/cmdline").map(|c| redact_cmdline(&c)),
            // SAFETY: sysconf with a valid name has no memory effects.
            page_size: unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as i64,
            online_cpus: rd("/sys/devices/system/cpu/online"),
            cpu_part_cpuinfo: field_from_cpuinfo("CPU part"),
            cpu_features: field_from_cpuinfo("Features"),
            midr_el1_sysfs: rd("/sys/devices/system/cpu/cpu0/regs/identification/midr_el1"),
            midr_el1_mrs: format!("{midr:#018x}"),
            cntfrq_hz: timing::cntfrq(),
            caches,
            cpufreq,
            temperature_c: guard::temp_c().ok(),
            vcgencmd_get_throttled: throttled,
            perf_event_paranoid: rd("/proc/sys/kernel/perf_event_paranoid"),
            perf_user_access: rd("/proc/sys/kernel/perf_user_access"),
            thp_enabled: rd("/sys/kernel/mm/transparent_hugepage/enabled"),
            sysfs_hugepages_present: fs::metadata("/sys/kernel/mm/hugepages").is_ok(),
            meminfo_huge: fs::read_to_string("/proc/meminfo")
                .unwrap_or_default()
                .lines()
                .filter(|l| l.to_ascii_lowercase().contains("huge"))
                .map(str::to_string)
                .collect(),
            memlock_limit_bytes: memlock_limit(),
            pmu: pmu::Pmu::discover().ok().map(|p| PmuInfo {
                device: p.device,
                type_id: p.type_,
                cpus: p.cpus,
                events: p.events,
            }),
        }
    }
}
