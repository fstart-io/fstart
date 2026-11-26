// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SPI flash read linearly through SMN, as the vendor's x86 code reads
//! it on Qilian (`FchSpiRomReadEx`, Venice AmdPspPeiV2Wh `0xe57ba0` [I]):
//! flash offset `o` is the SMN dword at `0x4400_0000 + o`, or at
//! `0x5A00_0000 + o` when SMN `0x02D0_000C[0]` selects the 2×64 MiB mode
//! (`0xe577c0`), read dword by dword through the IOHC's SMN index/data
//! pair; no ROM2 page XOR, no ROM3. The flash size (`0xe57820`) is
//! `64 MiB >> SPIx60[2:0]` (SMN alias `0x02DC_4060`), plus
//! `64 MiB >> SPIx7C[2:0]` for the second part when both `SPIxF8[0]` and
//! the 2×64 mode are set.
//!
//! Unlike ROM2 ([`crate::fch::rom3`]) this reaches every byte of the flash
//! whatever page the boot ROM left ROM2 on.

use tock_registers::interfaces::Readable;
use tock_registers::register_bitfields;

use crate::fch::rom3::Rom3Low;
use crate::smn::{Smn, SmnAddr, SmnReg};
use fstart_pci::ConfigRegionAccess;

/// The flash window in SMN.
pub const WINDOW: SmnAddr = 0x4400_0000;
/// The flash window in SMN in the 2×64 MiB mode.
pub const WINDOW_2X64: SmnAddr = 0x5A00_0000;
/// SMN `0x02D0_000C`: bit 0 selects the 2×64 MiB mode.
pub const ROM_MODE: SmnAddr = 0x02d0_000c;
/// The SPI controller's registers in SMN (`SPIxNN` at `+NN`).
pub const SPI: SmnAddr = 0x02dc_4000;
/// SPIx60 in SMN: ROM3's low base bits and the first part's size.
pub const ROM3_LOW: SmnReg<Rom3Low::Register> = SmnReg::new(SPI + crate::fch::rom3::ROM3_LOW);
/// SPIx7C in SMN: the second part's size.
pub const SECOND_ROM: SmnReg<SecondRom::Register> = SmnReg::new(SPI + 0x7c);
/// SPIxF8 in SMN: a second part is fitted.
pub const DUAL_ROM: SmnReg<DualRom::Register> = SmnReg::new(SPI + 0xf8);
/// SMN `0x02D0_000C`, typed.
pub const ROM_MODE_REG: SmnReg<RomMode::Register> = SmnReg::new(ROM_MODE);

register_bitfields![u32,
    /// SMN 0x02D0_000C (32-bit): the flash's SMN mode.
    pub RomMode [
        DUAL_64 OFFSET(0) NUMBITS(1) []
    ],
    /// SPIx7C (32-bit; SMN 0x02DC_407C): the second part's size code.
    pub SecondRom [
        ROM_SIZE OFFSET(0) NUMBITS(3) []
    ],
    /// SPIxF8 (32-bit; SMN 0x02DC_40F8): a second part is fitted.
    pub DualRom [
        ENABLE OFFSET(0) NUMBITS(1) []
    ]
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The range is not inside the flash.
    Range,
}

/// The flash of one FCH through the SMN of its IOD.
pub struct SmnFlash<P> {
    smn: Smn<P>,
    window: SmnAddr,
    size: u32,
}

impl<P: ConfigRegionAccess> SmnFlash<P> {
    /// The flash behind `smn`, `None` if the controller reads all-ones.
    pub fn new(smn: Smn<P>) -> Option<Self> {
        let mode = smn.reg(ROM_MODE_REG).extract();
        let first = smn.reg(ROM3_LOW).extract();
        if mode.get() == u32::MAX || first.get() == u32::MAX {
            return None;
        }
        let dual_64 = mode.is_set(RomMode::DUAL_64);
        let part = |code: u32| (64u32 << 20) >> code;
        let mut size = part(first.read(Rom3Low::ROM_SIZE));
        let dual = smn.reg(DUAL_ROM).extract();
        if dual_64 && dual.get() != u32::MAX && dual.is_set(DualRom::ENABLE) {
            size += part(smn.reg(SECOND_ROM).read(SecondRom::ROM_SIZE));
        }
        Some(Self {
            smn,
            window: if dual_64 { WINDOW_2X64 } else { WINDOW },
            size,
        })
    }

    /// The bytes of flash the window reaches.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Read `buf.len()` bytes of flash from `offset`.
    pub fn read(&self, offset: u32, buf: &mut [u8]) -> Result<(), Error> {
        let len = u32::try_from(buf.len()).map_err(|_| Error::Range)?;
        let end = offset
            .checked_add(len)
            .filter(|&e| e <= self.size)
            .ok_or(Error::Range)?;
        let mut at = offset & !3;
        let mut out = buf.iter_mut();
        while at < end {
            let word = self.smn.read32(self.window + at).to_le_bytes();
            let skip = offset.saturating_sub(at) as usize;
            let take = (end - at).min(4) as usize;
            for (byte, dst) in word[skip..take].iter().zip(out.by_ref()) {
                *dst = *byte;
            }
            at += 4;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smn::testing::SmnSpace;

    fn flash(space: &SmnSpace, window: u32, len: u32) {
        (0..len).step_by(4).for_each(|o| {
            let bytes: [u8; 4] = core::array::from_fn(|i| (o as usize + i) as u8 ^ 0x5a);
            space.set(window + o, u32::from_le_bytes(bytes));
        });
    }

    #[test]
    fn reads_any_span_linearly() {
        let space = SmnSpace::default();
        flash(&space, WINDOW, 0x40);
        let f = SmnFlash::new(space.smn()).unwrap();
        assert_eq!(f.size(), 64 << 20);
        for (offset, len) in [(0, 16), (1, 1), (3, 6), (5, 0x30), (0x3c, 4), (2, 0)] {
            let mut buf = vec![0; len];
            f.read(offset, &mut buf).unwrap();
            let want: Vec<u8> = (offset..offset + len as u32)
                .map(|o| o as u8 ^ 0x5a)
                .collect();
            assert_eq!(buf, want, "{offset:#x}+{len}");
        }
        let mut buf = [0; 8];
        assert_eq!(f.read((64 << 20) - 4, &mut buf), Err(Error::Range));
        assert_eq!(f.read(u32::MAX, &mut buf[..1]), Err(Error::Range));
    }

    #[test]
    fn size_and_window_follow_the_controller() {
        let space = SmnSpace::default();
        // 32 MiB part.
        space.set(SPI + 0x60, 1);
        assert_eq!(SmnFlash::new(space.smn()).unwrap().size(), 32 << 20);
        // A second part only counts in the 2x64 mode, which moves the window.
        space.set(SPI + 0xf8, 1);
        space.set(SPI + 0x7c, 0);
        assert_eq!(SmnFlash::new(space.smn()).unwrap().size(), 32 << 20);
        space.set(ROM_MODE, 1);
        flash(&space, WINDOW_2X64, 8);
        let f = SmnFlash::new(space.smn()).unwrap();
        assert_eq!(f.size(), 96 << 20);
        let mut buf = [0; 4];
        f.read(4, &mut buf).unwrap();
        assert_eq!(buf, [4 ^ 0x5a, 5 ^ 0x5a, 6 ^ 0x5a, 7 ^ 0x5a]);
        space.set(ROM_MODE, u32::MAX);
        assert!(SmnFlash::new(space.smn()).is_none());
    }
}
