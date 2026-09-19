//! Minimal AArch64 instruction encoders and a label-resolving assembler, used to generate the
//! code of the branch-predictor and out-of-order experiments at run time. Portable and unit-tested
//! against known encodings (the generated code is also executed and checked on the target).

pub const X30: u32 = 30;
pub const XZR: u32 = 31;
pub const SP: u32 = 31;

pub const COND_EQ: u32 = 0;
pub const COND_NE: u32 = 1;

pub fn nop() -> u32 {
    0xD503_201F
}
pub fn ret() -> u32 {
    0xD65F_03C0
}
pub fn br(rn: u32) -> u32 {
    0xD61F_0000 | rn << 5
}
pub fn blr(rn: u32) -> u32 {
    0xD63F_0000 | rn << 5
}
pub fn movz(rd: u32, imm16: u32, hw: u32) -> u32 {
    0xD280_0000 | hw << 21 | (imm16 & 0xFFFF) << 5 | rd
}
pub fn movk(rd: u32, imm16: u32, hw: u32) -> u32 {
    0xF280_0000 | hw << 21 | (imm16 & 0xFFFF) << 5 | rd
}
pub fn add_imm(rd: u32, rn: u32, imm12: u32) -> u32 {
    0x9100_0000 | (imm12 & 0xFFF) << 10 | rn << 5 | rd
}
pub fn sub_imm(rd: u32, rn: u32, imm12: u32) -> u32 {
    0xD100_0000 | (imm12 & 0xFFF) << 10 | rn << 5 | rd
}
pub fn subs_imm(rd: u32, rn: u32, imm12: u32) -> u32 {
    0xF100_0000 | (imm12 & 0xFFF) << 10 | rn << 5 | rd
}
pub fn add_reg(rd: u32, rn: u32, rm: u32) -> u32 {
    0x8B00_0000 | rm << 16 | rn << 5 | rd
}
pub fn and_reg(rd: u32, rn: u32, rm: u32) -> u32 {
    0x8A00_0000 | rm << 16 | rn << 5 | rd
}
/// EOR rd, rn, rm, LSL #shift
pub fn eor_lsl(rd: u32, rn: u32, rm: u32, shift: u32) -> u32 {
    0xCA00_0000 | rm << 16 | (shift & 63) << 10 | rn << 5 | rd
}
/// EOR rd, rn, rm, LSR #shift
pub fn eor_lsr(rd: u32, rn: u32, rm: u32, shift: u32) -> u32 {
    0xCA40_0000 | rm << 16 | (shift & 63) << 10 | rn << 5 | rd
}
/// MUL rd, rn, rm (MADD with xzr accumulator)
pub fn mul(rd: u32, rn: u32, rm: u32) -> u32 {
    0x9B00_7C00 | rm << 16 | rn << 5 | rd
}
/// EOR vd.16b, vn.16b, vm.16b
pub fn eor_v(rd: u32, rn: u32, rm: u32) -> u32 {
    0x6E20_1C00 | rm << 16 | rn << 5 | rd
}
/// LDR xt, [xn, xm, LSL #3]
pub fn ldr_x_reg_lsl3(rt: u32, rn: u32, rm: u32) -> u32 {
    0xF860_7800 | rm << 16 | rn << 5 | rt
}
/// LDR xt, [xn, xm]
pub fn ldr_x_reg(rt: u32, rn: u32, rm: u32) -> u32 {
    0xF860_6800 | rm << 16 | rn << 5 | rt
}
/// LDR xt, [xn, #imm] (imm multiple of 8, < 32768)
pub fn ldr_x_uimm(rt: u32, rn: u32, imm: u32) -> u32 {
    0xF940_0000 | (imm / 8) << 10 | rn << 5 | rt
}
/// STR xt, [xn, #imm] (imm multiple of 8)
pub fn str_x_uimm(rt: u32, rn: u32, imm: u32) -> u32 {
    0xF900_0000 | (imm / 8) << 10 | rn << 5 | rt
}
/// LDRB wt, [xn], #1 (post-index)
pub fn ldrb_post1(rt: u32, rn: u32) -> u32 {
    0x3840_1400 | rn << 5 | rt
}
/// LDRH wt, [xn], #2 (post-index)
pub fn ldrh_post2(rt: u32, rn: u32) -> u32 {
    0x7840_2400 | rn << 5 | rt
}
/// LDR xt, [xn], #8 (post-index)
pub fn ldr_x_post8(rt: u32, rn: u32) -> u32 {
    0xF840_8400 | rn << 5 | rt
}
/// STP xt, xt2, [sp, #-16]!
pub fn stp_pre16(rt: u32, rt2: u32) -> u32 {
    0xA9BF_0000 | rt2 << 10 | SP << 5 | rt
}
/// LDP xt, xt2, [sp], #16
pub fn ldp_post16(rt: u32, rt2: u32) -> u32 {
    0xA8C1_0000 | rt2 << 10 | SP << 5 | rt
}

#[derive(Clone, Copy)]
enum Kind {
    B26,
    Imm19,
    Imm14,
}

struct Fixup {
    at: usize,
    label: usize,
    kind: Kind,
}

/// Assembler with forward/backward labels. Offsets are in 32-bit words.
#[derive(Default)]
pub struct Asm {
    pub words: Vec<u32>,
    labels: Vec<Option<usize>>,
    fixups: Vec<Fixup>,
}

pub type Label = usize;

impl Asm {
    pub fn new() -> Asm {
        Asm::default()
    }

    pub fn pos(&self) -> usize {
        self.words.len()
    }

    pub fn label(&mut self) -> Label {
        self.labels.push(None);
        self.labels.len() - 1
    }

    pub fn bind(&mut self, l: Label) {
        assert!(self.labels[l].is_none(), "label bound twice");
        self.labels[l] = Some(self.words.len());
    }

    pub fn emit(&mut self, w: u32) {
        self.words.push(w);
    }

    /// Pads with `nop`-free zero words (never executed) up to word index `to`.
    pub fn pad_to(&mut self, to: usize) {
        assert!(to >= self.words.len(), "pad_to going backwards");
        self.words.resize(to, 0);
    }

    fn fix(&mut self, base: u32, l: Label, kind: Kind) {
        self.fixups.push(Fixup { at: self.words.len(), label: l, kind });
        self.words.push(base);
    }

    pub fn b(&mut self, l: Label) {
        self.fix(0x1400_0000, l, Kind::B26);
    }
    pub fn bl(&mut self, l: Label) {
        self.fix(0x9400_0000, l, Kind::B26);
    }
    pub fn b_cond(&mut self, cond: u32, l: Label) {
        self.fix(0x5400_0000 | cond, l, Kind::Imm19);
    }
    pub fn cbz(&mut self, rt: u32, l: Label) {
        self.fix(0xB400_0000 | rt, l, Kind::Imm19);
    }
    pub fn cbnz(&mut self, rt: u32, l: Label) {
        self.fix(0xB500_0000 | rt, l, Kind::Imm19);
    }
    pub fn tbz(&mut self, rt: u32, bit: u32, l: Label) {
        self.fix(0x3600_0000 | (bit >> 5) << 31 | (bit & 31) << 19 | rt, l, Kind::Imm14);
    }
    pub fn tbnz(&mut self, rt: u32, bit: u32, l: Label) {
        self.fix(0x3700_0000 | (bit >> 5) << 31 | (bit & 31) << 19 | rt, l, Kind::Imm14);
    }

    /// MOVZ + up to three MOVK.
    pub fn mov_imm64(&mut self, rd: u32, v: u64) {
        self.emit(movz(rd, (v & 0xFFFF) as u32, 0));
        for hw in 1..4 {
            let part = ((v >> (16 * hw)) & 0xFFFF) as u32;
            if part != 0 {
                self.emit(movk(rd, part, hw));
            }
        }
    }

    /// Resolves every label reference; errors on unbound labels or out-of-range offsets.
    pub fn finish(mut self) -> Result<Vec<u32>, String> {
        for f in &self.fixups {
            let target = self.labels[f.label].ok_or("unbound label")? as i64;
            let off = target - f.at as i64;
            let (bits, mask) = match f.kind {
                Kind::B26 => (26, 0x03FF_FFFFu32),
                Kind::Imm19 => (19, 0x0007_FFFF),
                Kind::Imm14 => (14, 0x0000_3FFF),
            };
            let lim = 1i64 << (bits - 1);
            if off < -lim || off >= lim {
                return Err(format!("branch offset {off} words does not fit in {bits} bits"));
            }
            let field = (off as u32) & mask;
            self.words[f.at] |= match f.kind {
                Kind::B26 => field,
                Kind::Imm19 => field << 5,
                Kind::Imm14 => field << 5,
            };
        }
        Ok(self.words)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_encodings() {
        assert_eq!(nop(), 0xD503201F);
        assert_eq!(ret(), 0xD65F03C0);
        assert_eq!(br(1), 0xD61F0020);
        assert_eq!(blr(1), 0xD63F0020);
        assert_eq!(movz(0, 1, 0), 0xD2800020);
        assert_eq!(movk(1, 0x1234, 1), 0xF2A24681);
        assert_eq!(add_imm(0, 0, 1), 0x91000400);
        assert_eq!(sub_imm(0, 0, 1), 0xD1000400);
        assert_eq!(subs_imm(0, 0, 1), 0xF1000400);
        assert_eq!(add_reg(0, 1, 2), 0x8B020020);
        assert_eq!(and_reg(0, 1, 2), 0x8A020020);
        assert_eq!(mul(0, 1, 2), 0x9B027C20);
        assert_eq!(ldr_x_reg_lsl3(2, 1, 0), 0xF8607822);
        assert_eq!(ldr_x_reg(2, 1, 0), 0xF8606822);
        assert_eq!(ldr_x_uimm(2, 1, 8), 0xF9400422);
        assert_eq!(str_x_uimm(2, 1, 8), 0xF9000422);
        assert_eq!(ldrb_post1(2, 1), 0x38401422);
        assert_eq!(ldrh_post2(4, 1), 0x78402424);
        assert_eq!(ldr_x_post8(2, 1), 0xF8408422);
        assert_eq!(stp_pre16(29, 30), 0xA9BF7BFD);
        assert_eq!(ldp_post16(29, 30), 0xA8C17BFD);
        assert_eq!(eor_lsl(3, 2, 2, 13), 0xCA023443);
        assert_eq!(eor_lsr(3, 2, 2, 7), 0xCA421C43);
        assert_eq!(eor_v(0, 1, 2), 0x6E221C20);
    }

    #[test]
    fn branch_fixups() {
        let mut a = Asm::new();
        let (top, end) = (a.label(), a.label());
        a.bind(top);
        a.emit(subs_imm(0, 0, 1)); // word 0
        a.b_cond(COND_NE, top); // word 1: back by 1 word
        a.b(end); // word 2: forward by 2 words
        a.emit(nop()); // word 3
        a.bind(end);
        a.emit(ret()); // word 4
        let w = a.finish().unwrap();
        assert_eq!(w[1], 0x54000000 | ((-1i32 as u32) & 0x7FFFF) << 5 | COND_NE);
        assert_eq!(w[2], 0x14000002);
        assert_eq!(w[4], ret());
    }

    #[test]
    fn known_branch_forms() {
        let mut a = Asm::new();
        let l = a.label();
        a.cbz(0, l); // +2 words
        a.cbnz(0, l);
        a.tbz(0, 0, l);
        a.tbnz(3, 33, l);
        a.bind(l);
        a.emit(nop());
        let w = a.finish().unwrap();
        assert_eq!(w[0], 0xB4000080); // cbz x0, +16
        assert_eq!(w[1], 0xB5000060); // cbnz x0, +12
        assert_eq!(w[2], 0x36000040); // tbz w0,#0,+8
        assert_eq!(w[3], 0xB7080023); // tbnz x3,#33,+4
    }

    #[test]
    fn mov_imm64_uses_only_needed_parts() {
        let mut a = Asm::new();
        a.mov_imm64(5, 0x0000_0001_0000_0002);
        assert_eq!(a.words, vec![movz(5, 2, 0), movk(5, 1, 2)]);
    }

    #[test]
    fn errors_are_reported() {
        let mut a = Asm::new();
        let l = a.label();
        a.b(l);
        assert!(a.finish().is_err());
        let mut a = Asm::new();
        let l = a.label();
        a.tbz(0, 0, l);
        a.pad_to(1 << 15);
        a.bind(l);
        assert!(a.finish().is_err(), "tbz reaches only +-32 KiB");
    }
}
