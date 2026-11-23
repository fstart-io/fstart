// SPDX-License-Identifier: MIT OR Apache-2.0
//! DDR4 MAD decoder programming from native/address_decode.c.
//! This is MC channel/rank decoding only, not TOM/TOLUD/TSEG/e820 setup.

use super::{
    Error,
    registers::{ChannelHash, MadDimm, MadInter, MadIntra, MchRegisters, MrParams},
    spd::{Dimm, Dimms},
};
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::Writeable;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressDecode {
    inter: u32,
    intra: [u32; 2],
    dimm: [u32; 2],
    mr_params: [u32; 2],
    populated: [bool; 2],
    ecc_io: bool,
}

impl AddressDecode {
    pub fn for_dimms(
        dimms: &Dimms<'_>,
        rank_interleave: bool,
        enhanced_interleave: bool,
        ecc_io: bool,
    ) -> Result<Self, Error> {
        let size = |channel: usize| {
            dimms[channel]
                .iter()
                .flatten()
                .map(|d| d.size_mib)
                .sum::<u32>()
        };
        let sizes = [size(0), size(1)];
        if sizes == [0, 0] {
            return Err(Error::NoMemory);
        }
        if ecc_io && dimms.iter().flatten().flatten().any(|d| !d.ecc_capable) {
            return Err(Error::UnsupportedDimm);
        }
        let channel_one_is_large = sizes[1] > sizes[0];
        let mut result = Self {
            inter: (MadInter::DDR_TYPE::Ddr4
                + MadInter::CH_L_MAP.val(channel_one_is_large as u32)
                + MadInter::CH_S_SIZE.val(sizes[0].min(sizes[1]) / 1024))
            .value,
            intra: [0; 2],
            dimm: [0; 2],
            mr_params: [0; 2],
            populated: [sizes[0] != 0, sizes[1] != 0],
            ecc_io,
        };
        for (ch, channel) in dimms.iter().enumerate() {
            if !result.populated[ch] {
                continue;
            }
            let cap = |d: Option<Dimm<'_>>| d.map_or(0, |d| d.size_mib);
            let slot_one_is_large = cap(channel[1]) > cap(channel[0]);
            let (large, small) = if slot_one_is_large {
                (channel[1], channel[0])
            } else {
                (channel[0], channel[1])
            };
            let fields = |d: Option<Dimm<'_>>| -> Result<(u32, u32, u32), Error> {
                let Some(d) = d else {
                    return Ok((0, 0, 0));
                };
                let width = match d.device_width {
                    8 => 0,
                    16 => 1,
                    _ => return Err(Error::UnsupportedDimm),
                };
                if !(1..=2).contains(&d.ranks)
                    || !d.size_mib.is_multiple_of(1024)
                    || d.size_mib / 1024 > 63
                {
                    return Err(Error::UnsupportedDimm);
                }
                Ok((d.size_mib / 1024, width, u32::from(d.ranks - 1)))
            };
            let (ls, lw, lr) = fields(large)?;
            let (ss, sw, sr) = fields(small)?;
            result.dimm[ch] = (MadDimm::L_SIZE.val(ls)
                + MadDimm::L_WIDTH.val(lw)
                + MadDimm::L_RANKS.val(lr)
                + MadDimm::S_SIZE.val(ss)
                + MadDimm::S_WIDTH.val(sw)
                + MadDimm::S_RANKS.val(sr))
            .value;
            result.intra[ch] = (MadIntra::DIMM_L_MAP.val(slot_one_is_large as u32)
                + MadIntra::RANK_INTERLEAVE.val(rank_interleave as u32)
                + MadIntra::ENHANCED_INTERLEAVE.val(enhanced_interleave as u32)
                + MadIntra::ECC.val(ecc_io as u32))
            .value;
            let widths = if slot_one_is_large {
                sw | (sw << 2) | (lw << 4) | (lw << 6)
            } else {
                lw | (lw << 2) | (sw << 4) | (sw << 6)
            };
            result.mr_params[ch] = (MrParams::RANK_WIDTHS.val(widths)
                + MrParams::MR4_PERIOD.val(0x200d)
                + MrParams::DDR4_TS_READOUT::SET)
                .value;
        }
        Ok(result)
    }

    /// Call after PLL initialization and initial RCOMP completion, before the
    /// post-MAD forced compensation. Does not enable normal MC operation.
    pub fn program(&self, registers: &MchRegisters, delay: &mut impl DelayNs) {
        registers.channel_hash.write(
            ChannelHash::MASK.val(0x30c8) + ChannelHash::LSB.val(2) + ChannelHash::ENABLE::SET,
        );
        registers.mad_inter.set(self.inter);
        for ch in 0..2 {
            if !self.populated[ch] {
                continue;
            }
            if self.ecc_io {
                delay.delay_us(4);
            }
            registers.mad_dimm[ch].set(self.dimm[ch]);
            registers.mad_intra[ch].set(self.intra[ch]);
            registers.channel[ch].mr_params.set(self.mr_params[ch]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        registers::testing::image,
        spd::{
            Dimm,
            testing::{seal, sodimm},
        },
    };
    use super::*;
    use tock_registers::interfaces::Readable;
    struct Delay(u32);
    impl DelayNs for Delay {
        fn delay_ns(&mut self, ns: u32) {
            self.0 += ns;
        }
    }
    #[test]
    fn address_decode_symmetric_ddr4_and_absent_channels() {
        let raw = sodimm();
        let d = Dimm::decode(&raw).unwrap();
        let map = AddressDecode::for_dimms(&[[Some(d), None], [Some(d), None]], true, true, false)
            .unwrap();
        let image = image();
        map.program(image.registers(), &mut Delay(0));
        let r = image.registers();
        assert_eq!(r.mad_inter.get(), 8 << 12);
        assert_eq!(r.channel_hash.get(), 0x120c3200);
        assert_eq!(r.mad_dimm[0].get(), 8);
        assert_eq!(r.mad_intra[0].get(), 0x110);
        assert_eq!(r.channel[1].mr_params.get(), 0x01200d00);
        let empty_ch =
            AddressDecode::for_dimms(&[[None, None], [Some(d), None]], false, false, false)
                .unwrap();
        assert_eq!(empty_ch.inter, 1 << 4);
        assert_eq!(empty_ch.populated, [false, true]);
    }
    #[test]
    fn address_decode_larger_slot_and_channel_mapping_and_rank_count() {
        let small = sodimm();
        let mut large = small;
        large[12] = 9;
        seal(&mut large);
        let a = Dimm::decode(&small).unwrap();
        let b = Dimm::decode(&large).unwrap();
        let map =
            AddressDecode::for_dimms(&[[Some(a), None], [Some(a), Some(b)]], true, true, false)
                .unwrap();
        assert_eq!(map.inter, (8 << 12) | (1 << 4));
        assert_eq!(map.dimm[1], 16 | (1 << 10) | (8 << 16));
        assert_eq!(map.intra[1], 0x111);
    }
    #[test]
    fn address_decode_ecc_io_requires_ecc_dimms_and_delays() {
        let mut raw = sodimm();
        let d = Dimm::decode(&raw).unwrap();
        assert_eq!(
            AddressDecode::for_dimms(&[[Some(d), None], [None, None]], true, true, true),
            Err(Error::UnsupportedDimm)
        );
        raw[13] |= 8;
        seal(&mut raw);
        let d = Dimm::decode(&raw).unwrap();
        let map =
            AddressDecode::for_dimms(&[[Some(d), None], [None, None]], true, true, true).unwrap();
        let image = image();
        let mut delay = Delay(0);
        map.program(image.registers(), &mut delay);
        assert_eq!(delay.0, 4000);
        assert_eq!(image.registers().mad_intra[0].read(MadIntra::ECC), 1);
        assert_eq!(
            AddressDecode::for_dimms(&[[None; 2]; 2], true, true, false),
            Err(Error::NoMemory)
        );
    }
}
