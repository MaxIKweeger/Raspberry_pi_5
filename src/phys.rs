//! Physical addresses of our own pages via /proc/self/pagemap (needs root: unprivileged reads
//! return a zero PFN). Read-only; only our own mapping is inspected.

use crate::lineset::{PoolPage, PAGE_SHIFT};
use crate::mem::Buffer;
use anyhow::{bail, Context, Result};
use std::fs::File;
use std::os::unix::fs::FileExt;

pub struct PhysPool {
    pub buf: Buffer,
    pub pages: Vec<PoolPage>,
}

const PM_PRESENT: u64 = 1 << 63;
const PM_SWAPPED: u64 = 1 << 62;
const PM_PFN_MASK: u64 = (1 << 55) - 1;

pub fn read_pfns(vbase: usize, npages: usize) -> Result<Vec<u64>> {
    let f = File::open("/proc/self/pagemap").context("open /proc/self/pagemap")?;
    let mut raw = vec![0u8; npages * 8];
    let off = (vbase >> PAGE_SHIFT) as u64 * 8;
    f.read_exact_at(&mut raw, off).context("read pagemap")?;
    let mut out = Vec::with_capacity(npages);
    for (i, ch) in raw.chunks_exact(8).enumerate() {
        let e = u64::from_le_bytes(ch.try_into().unwrap());
        if e & PM_PRESENT == 0 || e & PM_SWAPPED != 0 {
            bail!("page {i} not resident");
        }
        let pfn = e & PM_PFN_MASK;
        if pfn == 0 {
            bail!("pagemap returned PFN 0: run as root (sudo) to read physical frame numbers");
        }
        out.push(pfn);
    }
    Ok(out)
}

/// `System RAM` ranges from /proc/iomem (inclusive start, inclusive end). Needs root for real values.
pub fn system_ram_ranges() -> Result<Vec<(u64, u64)>> {
    let text = std::fs::read_to_string("/proc/iomem").context("read /proc/iomem")?;
    let mut v = Vec::new();
    for l in text.lines() {
        if let Some((range, name)) = l.trim().split_once(" : ") {
            if name.trim() == "System RAM" {
                if let Some((a, b)) = range.split_once('-') {
                    v.push((u64::from_str_radix(a, 16)?, u64::from_str_radix(b, 16)?));
                }
            }
        }
    }
    Ok(v)
}

impl PhysPool {
    pub fn new(bytes: usize) -> Result<PhysPool> {
        let buf = Buffer::new(bytes)?;
        let npages = bytes >> PAGE_SHIFT;
        let vbase = buf.as_ptr() as usize;
        let pfns = read_pfns(vbase, npages)?;
        let pages = pfns
            .iter()
            .enumerate()
            .map(|(i, &pfn)| PoolPage { vbase: vbase + (i << PAGE_SHIFT), pbase: pfn << PAGE_SHIFT })
            .collect();
        Ok(PhysPool { buf, pages })
    }

    /// Number of pages whose physical address changed since allocation (should be 0).
    pub fn moved_pages(&self) -> Result<usize> {
        let now = read_pfns(self.pages[0].vbase, self.pages.len())?;
        Ok(now.iter().zip(&self.pages).filter(|(&pfn, p)| pfn << PAGE_SHIFT != p.pbase).count())
    }

    /// Pages lying outside every `System RAM` range of /proc/iomem (0 confirms the PFN unit).
    pub fn pages_outside_ram(&self) -> Result<usize> {
        let ranges = system_ram_ranges()?;
        if ranges.is_empty() {
            bail!("no System RAM range found in /proc/iomem");
        }
        let size = 1u64 << PAGE_SHIFT;
        Ok(self
            .pages
            .iter()
            .filter(|p| !ranges.iter().any(|&(a, b)| p.pbase >= a && p.pbase + size - 1 <= b))
            .count())
    }
}
