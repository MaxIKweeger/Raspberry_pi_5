pub mod guard;
pub mod output;
pub mod perm;
pub mod stats;

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub mod affinity;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub mod env;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub mod harness;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub mod kernels;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub mod mem;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub mod mem_bw;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub mod mem_lat;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub mod pmu;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub mod selftest;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub mod timing;
