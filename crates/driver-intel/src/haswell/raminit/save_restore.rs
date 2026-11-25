//! The hardware-register portion of coreboot's `register_save_frame`.
//! This is not by itself a valid training-cache record: CPU, SPD/topology,
//! controller clock, and board policy must match before replay.

use super::MchBar;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

const PER_BYTE: &[(u32, u32)] = &[(0, 0x3c), (0x48, 0x4c), (0x5c, 0x78)];
const PER_CHANNEL: &[(u32, u32)] = &[
    (0x1204, 0x1208),
    (0x1214, 0x121c),
    (0x1404, 0x140c),
    (0x1808, 0x1810),
    (0x1a04, 0x1a0c),
    (0x1c14, 0x1c1c),
];
const COMMON: &[(u32, u32)] = &[(0x2000, 0x2008), (0x3a14, 0x3a1c), (0x3a24, 0x3a24)];
const MCMAIN: &[(u32, u32)] = &[
    (0x4000, 0x4014),
    (0x4024, 0x4028),
    (0x40d0, 0x40d0),
    (0x4220, 0x4224),
    (0x4294, 0x4294),
    (0x429c, 0x42a0),
    (0x42ec, 0x42fc),
    (0x4328, 0x4328),
    (0x438c, 0x4390),
];
const MISC: &[(u32, u32)] = &[
    (0x5884, 0x5888),
    (0x5890, 0x589c),
    (0x58a4, 0x58a4),
    (0x58d0, 0x58e4),
    (0x5880, 0x5880),
    (0x5000, 0x50dc),
    (0x59b8, 0x59b8),
];
pub(super) const WORDS: usize = 2 * 9 * 26 + 2 * 17 + 7 + 2 * 22 + 71;

#[cfg(test)]
pub(super) fn offsets() -> [u32; WORDS] {
    let mut offsets = [0; WORDS];
    visit(|i, off| offsets[i] = off);
    offsets
}

fn visit(mut f: impl FnMut(usize, u32)) {
    let mut i = 0;
    let mut group = |base: u32, ranges: &[(u32, u32)]| {
        for &(first, last) in ranges {
            for off in (first..=last).step_by(4) {
                f(i, base + off);
                i += 1;
            }
        }
    };
    // Both channels and all nine byte lanes, even if ECC is disabled.
    for ch in 0..2 {
        for byte in 0..9 {
            group(0x100 * ch + 0x200 * byte, PER_BYTE);
        }
    }
    for ch in 0..2 {
        group(0x100 * ch, PER_CHANNEL);
    }
    group(0, COMMON);
    for ch in 0..2 {
        group(0x400 * ch, MCMAIN);
    }
    group(0, MISC);
    debug_assert_eq!(i, WORDS);
}

#[repr(transparent)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable)]
pub struct TrainingRegisters([u32; WORDS]);
impl TrainingRegisters {
    pub const EMPTY: Self = Self([0; WORDS]);
    pub fn capture(&mut self, mch: MchBar) {
        visit(|i, off| self.0[i] = mch.read32(off));
    }
    /// # Safety
    /// Only replay validated cache data from the same CPU, memory and board
    /// wiring, at the restore-training point of the fast/warm/S3 schedule.
    pub unsafe fn restore(&self, mch: MchBar) {
        visit(|i, off| mch.write32(off, self.0[i]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layout() {
        let mut offsets = [0; WORDS];
        visit(|i, off| offsets[i] = off);
        assert_eq!(offsets[26], 0x200);
        assert_eq!(offsets[9 * 26], 0x100);
        assert_eq!(offsets[468], 0x1204);
        assert_eq!(offsets[509], 0x4000);
        assert_eq!(offsets[623], 0x59b8);
    }
}
