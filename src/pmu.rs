//! Thin wrapper over perf_event_open for the ARMv8 PMU, user-space only (`exclude_kernel`).

use anyhow::{anyhow, bail, Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::os::fd::RawFd;
use std::path::PathBuf;

const SYS_PERF_EVENT_OPEN: libc::c_long = 241; // aarch64 syscall number
const PERF_FLAG_FD_CLOEXEC: libc::c_ulong = 8;
const IOC_ENABLE: u64 = 0x2400;
const IOC_DISABLE: u64 = 0x2401;
const IOC_RESET: u64 = 0x2403;
const IOC_FLAG_GROUP: libc::c_ulong = 1;
// read_format: TOTAL_TIME_ENABLED | TOTAL_TIME_RUNNING | ID | GROUP
const READ_FORMAT: u64 = 1 | 2 | 4 | 8;
const ATTR_SIZE_VER8: u32 = 136;
const FLAG_DISABLED: u64 = 1 << 0;
const FLAG_EXCLUDE_KERNEL: u64 = 1 << 5;
const FLAG_EXCLUDE_HV: u64 = 1 << 6;
pub const MAX_GROUP: usize = 16;

/// Mirror of `struct perf_event_attr` up to PERF_ATTR_SIZE_VER8 (136 bytes).
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct PerfEventAttr {
    pub type_: u32,
    pub size: u32,
    pub config: u64,
    pub sample_period: u64,
    pub sample_type: u64,
    pub read_format: u64,
    pub flags: u64,
    pub wakeup_events: u32,
    pub bp_type: u32,
    pub config1: u64,
    pub config2: u64,
    pub branch_sample_type: u64,
    pub sample_regs_user: u64,
    pub sample_stack_user: u32,
    pub clockid: i32,
    pub sample_regs_intr: u64,
    pub aux_watermark: u32,
    pub sample_max_stack: u16,
    pub reserved_2: u16,
    pub aux_sample_size: u32,
    pub reserved_3: u32,
    pub sig_data: u64,
    pub config3: u64,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub name: String,
    pub type_: u32,
    pub config: u64,
}

/// Parse `event=0x11` (optionally followed by `,key=val` pairs) from a sysfs events file.
pub fn parse_event_cfg(s: &str) -> Option<u64> {
    let field = s.trim().split(',').find_map(|kv| kv.trim().strip_prefix("event="))?;
    match field.strip_prefix("0x") {
        Some(h) => u64::from_str_radix(h, 16).ok(),
        None => field.parse().ok(),
    }
}

#[derive(Debug)]
pub struct Pmu {
    pub device: String,
    pub type_: u32,
    pub cpus: String,
    pub events: BTreeMap<String, u64>,
}

impl Pmu {
    /// Find the ARMv8 PMU device (`armv8_pmuv3_*` or `armv8_cortex_*`) and list its events.
    pub fn discover() -> Result<Pmu> {
        let base = PathBuf::from("/sys/bus/event_source/devices");
        let mut found = None;
        for e in fs::read_dir(&base).context("read event_source/devices")? {
            let e = e?;
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("armv8_") {
                found = Some((name, e.path()));
                break;
            }
        }
        let (device, dir) = found.ok_or_else(|| anyhow!("no armv8_* PMU device in sysfs"))?;
        let type_: u32 = fs::read_to_string(dir.join("type"))?.trim().parse()?;
        let cpus = fs::read_to_string(dir.join("cpus")).unwrap_or_default().trim().to_string();
        let mut events = BTreeMap::new();
        for e in fs::read_dir(dir.join("events"))? {
            let e = e?;
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(cfg) = fs::read_to_string(e.path()).ok().as_deref().and_then(parse_event_cfg) {
                events.insert(name, cfg);
            }
        }
        Ok(Pmu { device, type_, cpus, events })
    }

    pub fn event(&self, name: &str) -> Result<Event> {
        let config = *self.events.get(name).ok_or_else(|| anyhow!("PMU event '{name}' not exposed by sysfs"))?;
        Ok(Event { name: name.to_string(), type_: self.type_, config })
    }
}

#[derive(Clone, Debug)]
pub struct Reading {
    pub time_enabled: u64,
    pub time_running: u64,
    pub values: Vec<u64>,
}

impl Reading {
    pub fn multiplexed(&self) -> bool {
        self.time_running < self.time_enabled
    }
}

/// A group of events scheduled atomically on the PMU; the first event is the leader.
pub struct Group {
    fds: Vec<RawFd>,
}

fn open_one(attr: &PerfEventAttr, group_fd: RawFd) -> Result<RawFd> {
    // SAFETY: attr points to a valid, fully initialised struct of `attr.size` bytes.
    let fd = unsafe {
        libc::syscall(SYS_PERF_EVENT_OPEN, attr as *const PerfEventAttr, 0 as libc::pid_t, -1 as libc::c_int, group_fd, PERF_FLAG_FD_CLOEXEC)
    };
    if fd < 0 {
        let err = std::io::Error::last_os_error();
        let hint = match err.raw_os_error() {
            Some(libc::EACCES) | Some(libc::EPERM) => " (check /proc/sys/kernel/perf_event_paranoid)",
            _ => "",
        };
        bail!("perf_event_open(type={}, config={:#x}): {err}{hint}", attr.type_, attr.config);
    }
    Ok(fd as RawFd)
}

impl Group {
    pub fn open(events: &[Event]) -> Result<Group> {
        if events.is_empty() || events.len() > MAX_GROUP {
            bail!("group size must be 1..={MAX_GROUP}");
        }
        let mut fds: Vec<RawFd> = Vec::new();
        for (i, ev) in events.iter().enumerate() {
            let attr = PerfEventAttr {
                type_: ev.type_,
                size: ATTR_SIZE_VER8,
                config: ev.config,
                read_format: READ_FORMAT,
                flags: FLAG_EXCLUDE_KERNEL | FLAG_EXCLUDE_HV | if i == 0 { FLAG_DISABLED } else { 0 },
                ..Default::default()
            };
            let leader = if i == 0 { -1 } else { fds[0] };
            match open_one(&attr, leader) {
                Ok(fd) => fds.push(fd),
                Err(e) => {
                    for fd in fds {
                        // SAFETY: fds were returned by perf_event_open and are owned here.
                        unsafe { libc::close(fd) };
                    }
                    return Err(e.context(format!("event '{}'", ev.name)));
                }
            }
        }
        Ok(Group { fds })
    }

    fn ioctl(&self, req: u64) -> Result<()> {
        // SAFETY: leader fd is valid for the lifetime of self; requests take a flags argument.
        let r = unsafe { libc::ioctl(self.fds[0], req as _, IOC_FLAG_GROUP) };
        if r != 0 {
            bail!("perf ioctl {req:#x}: {}", std::io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn reset_enable(&self) -> Result<()> {
        self.ioctl(IOC_RESET)?;
        self.ioctl(IOC_ENABLE)
    }

    pub fn disable(&self) -> Result<()> {
        self.ioctl(IOC_DISABLE)
    }

    /// Non-allocating read of the leader's value only (used to time the read syscall itself).
    pub fn read_leader(&self) -> u64 {
        let mut buf = [0u64; 3 + 2 * MAX_GROUP];
        // SAFETY: buf is large enough for the group read_format of up to MAX_GROUP events.
        unsafe { libc::read(self.fds[0], buf.as_mut_ptr() as *mut libc::c_void, std::mem::size_of_val(&buf)) };
        buf[3]
    }

    pub fn read(&self) -> Result<Reading> {
        let mut buf = [0u64; 3 + 2 * MAX_GROUP];
        // SAFETY: as above.
        let n = unsafe { libc::read(self.fds[0], buf.as_mut_ptr() as *mut libc::c_void, std::mem::size_of_val(&buf)) };
        if n < 0 {
            bail!("perf read: {}", std::io::Error::last_os_error());
        }
        let nr = buf[0] as usize;
        if nr != self.fds.len() {
            bail!("group read returned {nr} values, expected {}", self.fds.len());
        }
        Ok(Reading {
            time_enabled: buf[1],
            time_running: buf[2],
            values: (0..nr).map(|i| buf[3 + 2 * i]).collect(),
        })
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        for &fd in &self.fds {
            // SAFETY: fd is owned by this group and closed exactly once.
            unsafe { libc::close(fd) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attr_layout_is_ver8() {
        assert_eq!(std::mem::size_of::<PerfEventAttr>(), ATTR_SIZE_VER8 as usize);
    }

    #[test]
    fn parses_event_files() {
        assert_eq!(parse_event_cfg("event=0x11\n"), Some(0x11));
        assert_eq!(parse_event_cfg("event=0x0008,foo=1"), Some(8));
        assert_eq!(parse_event_cfg("event=17"), Some(17));
        assert_eq!(parse_event_cfg("garbage"), None);
    }
}
