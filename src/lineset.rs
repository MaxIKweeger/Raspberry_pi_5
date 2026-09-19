//! Selection of cache lines with prescribed physical-address bits, from a pool of pages whose
//! physical addresses are known. Portable (no OS access) so it can be unit-tested on the host.
//!
//! A line at in-page offset `o` of a page with physical base `pbase` has physical address
//! `pbase + o`. Bits 6..PAGE_SHIFT of that address come from `o`, bits from PAGE_SHIFT up from `pbase`.

use std::collections::HashMap;

pub const PAGE_SHIFT: u32 = 14; // 16 KiB pages

#[derive(Clone, Copy, Debug)]
pub struct PoolPage {
    pub vbase: usize,
    pub pbase: u64,
}

/// Key made of the physical bits `PAGE_SHIFT..=hi` of a page.
pub fn key_of(pbase: u64, hi: u32) -> u64 {
    (pbase >> PAGE_SHIFT) & ((1u64 << (hi - PAGE_SHIFT + 1)) - 1)
}

/// Pages grouped by the physical bits `PAGE_SHIFT..=hi`.
pub fn classes(pages: &[PoolPage], hi: u32) -> HashMap<u64, Vec<usize>> {
    let mut m: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, p) in pages.iter().enumerate() {
        m.entry(key_of(p.pbase, hi)).or_default().push(i);
    }
    m
}

/// A group of pages sharing bits `PAGE_SHIFT..=hi`, plus for each requested flip bit (a bit
/// position in `PAGE_SHIFT..=hi`) the group whose key differs from ours in that bit only.
pub struct Choice {
    pub hi: u32,
    pub key: u64,
    pub base: Vec<usize>,
    pub flipped: Vec<(u32, Vec<usize>)>,
}

/// Finds the highest `hi <= max_hi` for which some class has at least `need` pages and every
/// flip class (for bits in `flip_bits` that are <= hi) has at least `need / 2 + 1` pages.
/// Higher `hi` means more physical bits are held constant, i.e. tighter control of the set.
pub fn choose(pages: &[PoolPage], max_hi: u32, need: usize, flip_bits: &[u32]) -> Option<Choice> {
    for hi in (PAGE_SHIFT..=max_hi).rev() {
        let cl = classes(pages, hi);
        let mut keys: Vec<&u64> = cl.keys().collect();
        keys.sort();
        let mut best: Option<Choice> = None;
        for &k in keys {
            let base = &cl[&k];
            if base.len() < need {
                continue;
            }
            let mut flipped = Vec::new();
            let mut ok = true;
            for &b in flip_bits.iter().filter(|&&b| b >= PAGE_SHIFT && b <= hi) {
                let fk = k ^ (1u64 << (b - PAGE_SHIFT));
                match cl.get(&fk) {
                    Some(v) if v.len() > need / 2 => flipped.push((b, v.clone())),
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                best = Some(Choice { hi, key: k, base: base.clone(), flipped });
                break;
            }
        }
        if best.is_some() {
            return best;
        }
    }
    None
}

/// `k` lines, all at in-page offset `offset`, from distinct pages of `class`.
pub fn same_set_lines(pages: &[PoolPage], class: &[usize], offset: usize, k: usize) -> Vec<usize> {
    class[..k].iter().map(|&i| pages[i].vbase + offset).collect()
}

/// Half of the `k` lines differ from the other half in exactly one physical bit `bit`.
/// `bit >= PAGE_SHIFT`: even lines come from `class_a`, odd lines from `class_b` (same offset).
/// `bit < PAGE_SHIFT`: all lines come from `class_a`; odd lines use offset `offset ^ (1 << bit)`.
pub fn flipped_lines(
    pages: &[PoolPage],
    class_a: &[usize],
    class_b: &[usize],
    offset: usize,
    k: usize,
    bit: u32,
) -> Vec<usize> {
    (0..k)
        .map(|i| {
            if bit >= PAGE_SHIFT {
                let src = if i % 2 == 0 { class_a } else { class_b };
                pages[src[i / 2]].vbase + offset
            } else {
                let off = if i % 2 == 0 { offset } else { offset ^ (1usize << bit) };
                pages[class_a[i / 2]].vbase + off
            }
        })
        .collect()
}

/// Physical address of a line given the pool (linear search by page base; for tests and checks).
pub fn paddr_of(pages: &[PoolPage], vaddr: usize) -> Option<u64> {
    let page = 1usize << PAGE_SHIFT;
    pages.iter().find(|p| vaddr >= p.vbase && vaddr < p.vbase + page).map(|p| p.pbase + (vaddr - p.vbase) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic pool: page i at virtual i*16K, physical pages shuffled inside a 256 MiB window.
    fn pool(n: usize) -> Vec<PoolPage> {
        let mut rng = crate::stats::Rng::new(7);
        let window = 1usize << 14; // pages in 256 MiB
        let mut frames: Vec<u64> = (0..window as u64).collect();
        for i in (1..frames.len()).rev() {
            let j = rng.below(i as u64 + 1) as usize;
            frames.swap(i, j);
        }
        (0..n)
            .map(|i| PoolPage { vbase: 0x1000_0000 + i * (1 << PAGE_SHIFT), pbase: frames[i] << PAGE_SHIFT })
            .collect()
    }

    #[test]
    fn key_extracts_the_right_bits() {
        assert_eq!(key_of(0b1011 << 14, 15), 0b11);
        assert_eq!(key_of(0b1011 << 14, 16), 0b011);
        assert_eq!(key_of(0b1011 << 14, 17), 0b1011);
        assert_eq!(key_of(0x3fff, 20), 0);
    }

    #[test]
    fn same_set_lines_share_all_constrained_physical_bits() {
        let pages = pool(8192);
        let c = choose(&pages, 20, 24, &[]).expect("class");
        let lines = same_set_lines(&pages, &c.base, 12288, 24);
        let first = paddr_of(&pages, lines[0]).unwrap();
        let mask = ((1u64 << (c.hi + 1)) - 1) & !0x3f;
        for l in &lines {
            let p = paddr_of(&pages, *l).unwrap();
            assert_eq!(p & mask, first & mask);
        }
        let distinct: std::collections::HashSet<_> = lines.iter().collect();
        assert_eq!(distinct.len(), 24);
    }

    #[test]
    fn flip_above_page_changes_exactly_that_bit() {
        let pages = pool(8192);
        let c = choose(&pages, 20, 16, &[15, 17]).expect("class");
        for (bit, other) in &c.flipped {
            let lines = flipped_lines(&pages, &c.base, other, 12288, 12, *bit);
            let pa: Vec<u64> = lines.iter().map(|&l| paddr_of(&pages, l).unwrap()).collect();
            let mask = ((1u64 << (c.hi + 1)) - 1) & !0x3f;
            for (i, p) in pa.iter().enumerate() {
                let diff = (p ^ pa[0]) & mask;
                assert_eq!(diff, if i % 2 == 0 { 0 } else { 1u64 << bit }, "line {i} bit {bit}");
            }
        }
    }

    #[test]
    fn flip_inside_page_uses_offset_bit() {
        let pages = pool(4096);
        let c = choose(&pages, 18, 12, &[]).unwrap();
        let lines = flipped_lines(&pages, &c.base, &c.base, 12288, 12, 9);
        let pa: Vec<u64> = lines.iter().map(|&l| paddr_of(&pages, l).unwrap()).collect();
        let mask = ((1u64 << (c.hi + 1)) - 1) & !0x3f;
        for (i, p) in pa.iter().enumerate() {
            let diff = (p ^ pa[0]) & mask;
            assert_eq!(diff, if i % 2 == 0 { 0 } else { 1 << 9 }, "line {i}");
        }
    }

    #[test]
    fn choose_prefers_higher_hi_and_falls_back() {
        let pages = pool(2048);
        let c = choose(&pages, 26, 8, &[]).unwrap();
        assert!(c.hi >= 16);
        assert!(choose(&pages, 26, 5000, &[]).is_none());
    }
}
