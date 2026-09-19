use anyhow::{bail, Result};

/// Pin the calling thread to one CPU and verify the scheduler honoured it.
pub fn pin_to(cpu: usize) -> Result<()> {
    // SAFETY: cpu_set_t is plain data; zeroed is a valid empty set. sched_setaffinity(0, ..)
    // only reads `set` for the given size.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(cpu, &mut set);
        if libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) != 0 {
            bail!("sched_setaffinity({cpu}): {}", std::io::Error::last_os_error());
        }
    }
    let now = current_cpu();
    if now != cpu as i32 {
        bail!("pinned to cpu {cpu} but running on cpu {now}");
    }
    Ok(())
}

pub fn current_cpu() -> i32 {
    // SAFETY: no arguments, no memory access.
    unsafe { libc::sched_getcpu() }
}
