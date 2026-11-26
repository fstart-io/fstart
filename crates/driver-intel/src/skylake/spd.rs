// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native DDR4 intake from romstage_native.c:478-757 and common/block/smbus/
//! smbuslib.c:get_spd. Reuses the JEDEC inventory decoder and SMBus service.

use super::Error;
use fstart_core::services::{ServiceError, SmBus};
use fstart_driver_spd::{crc16, ddr4};
use tock_registers::{LocalRegisterCopy, register_bitfields};
use zerocopy::{
    FromBytes, Immutable, KnownLayout, Unaligned,
    byteorder::{big_endian, little_endian},
};

pub const SPD_BYTES: usize = 512;
const PAGE_ZERO: u8 = 0x36;
const PAGE_ONE: u8 = 0x37;

/// JEDEC DDR4 SPD (512-byte) wire layout for the fields consumed by Skylake/Kaby Lake raminit.
#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, KnownLayout, Unaligned)]
pub(crate) struct Ddr4Spd {
    pub bytes_used_total: u8,
    pub revision: u8,
    pub dram_device_type: u8,
    pub module_type: u8,
    pub density_banks: u8,
    pub addressing: u8,
    pub package_type: u8,
    _rsvd7: [u8; 5],
    pub organization: u8,
    _rsvd13: [u8; 5],
    pub tck_min_mtb: u8,
    pub tck_max_mtb: u8,
    pub cas_latencies: little_endian::U32,
    pub taa_min_mtb: u8,
    pub trcd_min_mtb: u8,
    pub trp_min_mtb: u8,
    pub tras_trc_upper: u8,
    pub tras_min_lsb: u8,
    pub trc_min_lsb: u8,
    pub trfc1_min_mtb: little_endian::U16,
    pub trfc2_min_mtb: little_endian::U16,
    pub trfc4_min_mtb: little_endian::U16,
    pub tfaw_upper: u8,
    pub tfaw_min_lsb: u8,
    pub trrd_s_min_mtb: u8,
    pub trrd_l_min_mtb: u8,
    _rsvd40: [u8; 78],
    pub trrd_l_min_ftb: u8,
    pub trrd_s_min_ftb: u8,
    pub trc_min_ftb: u8,
    pub trp_min_ftb: u8,
    pub trcd_min_ftb: u8,
    pub taa_min_ftb: u8,
    pub tck_max_ftb: u8,
    pub tck_min_ftb: u8,
    pub base_crc: little_endian::U16,
    _rsvd128: [u8; 2],
    pub reference_raw_card: u8,
    pub address_mapping: u8,
    _rsvd132: [u8; 122],
    pub module_crc: little_endian::U16,
    _rsvd256: [u8; 64],
    pub module_manufacturer: little_endian::U16,
    pub manufacturing_location: u8,
    pub manufacturing_date: big_endian::U16,
    pub serial_number: [u8; 4],
    _part_prefix: [u8; 15],
    pub part_byte_344: u8,
    _part_suffix: [u8; 5],
    pub dram_manufacturer: little_endian::U16,
    _rsvd352: [u8; 160],
}

const _: () = {
    assert!(core::mem::size_of::<Ddr4Spd>() == SPD_BYTES);
    assert!(core::mem::offset_of!(Ddr4Spd, tck_min_mtb) == 18);
    assert!(core::mem::offset_of!(Ddr4Spd, trrd_l_min_ftb) == 118);
    assert!(core::mem::offset_of!(Ddr4Spd, reference_raw_card) == 130);
    assert!(core::mem::offset_of!(Ddr4Spd, module_crc) == 254);
    assert!(core::mem::offset_of!(Ddr4Spd, module_manufacturer) == 320);
    assert!(core::mem::offset_of!(Ddr4Spd, part_byte_344) == 344);
    assert!(core::mem::offset_of!(Ddr4Spd, dram_manufacturer) == 350);
};

register_bitfields![u8,
    Density [
        CAPACITY OFFSET(0) NUMBITS(4) [],
        BANK_ADDRESS OFFSET(4) NUMBITS(2) [],
        BANK_GROUP OFFSET(6) NUMBITS(2) []
    ],
    Addressing [
        COLUMN OFFSET(0) NUMBITS(3) [],
        ROW OFFSET(3) NUMBITS(3) []
    ],
    Organization [
        WIDTH OFFSET(0) NUMBITS(3) [],
        RANKS OFFSET(3) NUMBITS(3) []
    ],
    RawCard [
        CARD OFFSET(0) NUMBITS(5) [],
        EXTENSION OFFSET(7) NUMBITS(1) []
    ]
];

/// DDR4 SPD page selection is bus-global. Callers must serialize the whole
/// operation with every other user of the same bus. Always attempt to restore
/// page zero, including on a failed read; never return a partial SPD as valid.
/// Do not set the PCH SPD write-disable bit before calling this routine.
pub fn read(
    smbus: &mut dyn SmBus,
    address: u8,
    output: &mut [u8; SPD_BYTES],
) -> Result<(), ServiceError> {
    if !(0x50..=0x57).contains(&address) {
        return Err(ServiceError::InvalidParam);
    }
    output.fill(0);
    let result = (|| {
        smbus.write_byte(PAGE_ZERO, 0, 0)?;
        read_page(smbus, address, &mut output[..256])?;
        if output[2] != 0x0c {
            return Err(ServiceError::InvalidParam);
        }
        smbus.write_byte(PAGE_ONE, 0, 0)?;
        read_page(smbus, address, &mut output[256..])
    })();
    let restore = smbus.write_byte(PAGE_ZERO, 0, 0);
    let result = result.and(restore);
    if result.is_err() {
        output.fill(0);
    }
    result
}

fn read_page(smbus: &mut dyn SmBus, address: u8, output: &mut [u8]) -> Result<(), ServiceError> {
    if smbus.i2c_eeprom_read(address, 0, output).is_ok() {
        return Ok(());
    }
    // A failed block read may have filled a prefix. Overwrite every byte.
    for (offset, byte) in output.iter_mut().enumerate() {
        *byte = smbus.read_byte(address, offset as u8)?;
    }
    Ok(())
}

/// Validated unbuffered DDR4 geometry for the SKL/KBL decoder, not a general
/// inventory representation. Raw SPD remains owned by the caller through all
/// training and training-cache identity calculations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dimm<'a> {
    pub raw: &'a [u8; SPD_BYTES],
    pub size_mib: u32,
    pub ranks: u8,
    pub device_width: u8,
    pub banks: u8,
    pub bank_groups: u8,
    pub column_bits: u8,
    pub row_bits: u8,
    pub density_index: u8,
    pub ecc_capable: bool,
    pub rank_one_mirrored: bool,
    pub reference_raw_card: u8,
    pub manufacturing_crc: u16,
    pub dram_manufacturer: u16,
    pub tcr_sensitive_hynix: bool,
    pub tcr_sensitive_micron: bool,
}

impl<'a> Dimm<'a> {
    pub(crate) fn spd(&self) -> &'a Ddr4Spd {
        zerocopy::transmute_ref!(self.raw)
    }

    pub fn decode(raw: &'a [u8; SPD_BYTES]) -> Result<Self, Error> {
        let inventory = ddr4::inventory(raw, 0, 0, 0)?;
        let spd: &Ddr4Spd = zerocopy::transmute_ref!(raw);
        // SKL/KBL client MC does not support registered/LRDIMM/3DS, x4,
        // asymmetric or non-64-bit organizations. Do not silently truncate
        // ranks into the one-bit MAD rank-count fields.
        if !matches!(spd.module_type & 0xf, 2 | 3)
            || spd.package_type != 0
            || spd.bytes_used_total & 0xf < 3
        {
            return Err(Error::UnsupportedDimm);
        }
        if crc16(&raw[128..254]) != spd.module_crc.get() {
            return Err(ddr4::Error::Crc.into());
        }
        let organization = LocalRegisterCopy::<u8, Organization::Register>::new(spd.organization);
        let density = LocalRegisterCopy::<u8, Density::Register>::new(spd.density_banks);
        let addressing = LocalRegisterCopy::<u8, Addressing::Register>::new(spd.addressing);
        let raw_card = LocalRegisterCopy::<u8, RawCard::Register>::new(spd.reference_raw_card);
        let device_width = 4 << organization.read(Organization::WIDTH);
        let banks = 1u8
            .checked_shl((2 + density.read(Density::BANK_ADDRESS)).into())
            .ok_or(Error::UnsupportedDimm)?;
        let bank_groups = 1u8 << density.read(Density::BANK_GROUP);
        let column_bits = 9 + addressing.read(Addressing::COLUMN);
        let row_bits = 12 + addressing.read(Addressing::ROW);
        let density_index = density.read(Density::CAPACITY);
        let size_mib = inventory.size_mib.get() as u32;
        if !(1024..=32768).contains(&size_mib)
            || !size_mib.is_multiple_of(1024)
            || inventory.data_width.get() != 64
            || !(1..=2).contains(&inventory.ranks)
            || !matches!(device_width, 8 | 16)
            || banks != 4
            || !matches!(bank_groups, 2 | 4)
            || !(3..=6).contains(&density_index)
            || column_bits != 10
            || !(14..=18).contains(&row_bits)
        {
            return Err(Error::UnsupportedDimm);
        }
        // Reject corrupt geometry that disagrees with density-derived capacity.
        let geometry_mib = (1u64 << (column_bits + row_bits))
            * u64::from(banks)
            * u64::from(bank_groups)
            * 8
            * u64::from(inventory.ranks)
            / (1024 * 1024);
        if geometry_mib != u64::from(size_mib) {
            return Err(Error::UnsupportedDimm);
        }
        let module_manufacturer = spd.module_manufacturer.get();
        let dram_manufacturer = spd.dram_manufacturer.get();
        let date_code = spd.manufacturing_date.get();
        let hynix = dram_manufacturer == 0xad80 || module_manufacturer == 0xad80;
        let micron = dram_manufacturer == 0x2c80 || module_manufacturer == 0x2c80;
        Ok(Self {
            raw,
            size_mib,
            ranks: inventory.ranks,
            device_width,
            banks,
            bank_groups,
            column_bits,
            row_bits,
            density_index,
            ecc_capable: inventory.total_width.get() == 72,
            rank_one_mirrored: spd.address_mapping & 1 != 0,
            reference_raw_card: raw_card.read(RawCard::CARD)
                | (raw_card.read(RawCard::EXTENSION) << 5),
            manufacturing_crc: crc16(&raw[320..329]),
            dram_manufacturer,
            tcr_sensitive_hynix: hynix && date_code < 0x1512,
            tcr_sensitive_micron: !hynix
                && micron
                && density_index == 4
                && device_width == 8
                && spd.part_byte_344 == b'A',
        })
    }
}

/// Channel-major, slot-major order. Unlike coreboot's temporary native-index
/// remapping, the index here always means `[channel][slot]`.
pub type Dimms<'a> = [[Option<Dimm<'a>>; 2]; 2];

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    pub fn seal(raw: &mut [u8; 512]) {
        let base = crc16(&raw[..126]);
        raw[126..128].copy_from_slice(&base.to_le_bytes());
        let module = crc16(&raw[128..254]);
        raw[254..256].copy_from_slice(&module.to_le_bytes());
    }
    // Synthetic DDR4-2133 8 GiB 1Rx8 SO-DIMM; not a captured M900 SPD.
    pub fn sodimm() -> [u8; 512] {
        let mut raw = [0; 512];
        raw[0] = 0x23;
        raw[1] = 0x11;
        raw[2] = 0x0c;
        raw[3] = 3;
        raw[4] = 0x85;
        raw[5] = 0x21;
        raw[11] = 3;
        raw[12] = 1;
        raw[13] = 3;
        raw[18] = 8;
        raw[125] = (-62i8) as u8;
        raw[19] = 20;
        raw[20..24].copy_from_slice(&0x3ff80u32.to_le_bytes());
        raw[24] = 112;
        raw[25] = 112;
        raw[26] = 112;
        raw[27] = 0x11;
        raw[28] = 8;
        raw[29] = 120;
        raw[30..32].copy_from_slice(&2800u16.to_le_bytes());
        raw[32..34].copy_from_slice(&2080u16.to_le_bytes());
        raw[34..36].copy_from_slice(&1280u16.to_le_bytes());
        raw[37] = 240;
        raw[38] = 32;
        raw[39] = 48;
        raw[130] = 0x81;
        raw[131] = 1;
        raw[320..322].copy_from_slice(&0xad80u16.to_le_bytes());
        raw[323..325].copy_from_slice(&0x1511u16.to_be_bytes());
        seal(&mut raw);
        raw
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;
    use testing::*;
    #[test]
    fn native_spd_geometry_and_manufacturing() {
        let raw = sodimm();
        let dimm = Dimm::decode(&raw).unwrap();
        assert_eq!((dimm.size_mib, dimm.ranks, dimm.device_width), (8192, 1, 8));
        assert_eq!(
            (
                dimm.banks,
                dimm.bank_groups,
                dimm.row_bits,
                dimm.column_bits
            ),
            (4, 4, 16, 10)
        );
        assert_eq!(dimm.reference_raw_card, 33);
        assert!(dimm.rank_one_mirrored && dimm.tcr_sensitive_hynix);
        assert_eq!(dimm.manufacturing_crc, crc16(&raw[320..329]));
    }
    #[test]
    fn rejects_unsupported_or_inconsistent_controller_geometry() {
        for (offset, value) in [
            (3, 1),
            (6, 0x91),
            (12, 0),
            (12, 0x19),
            (5, 0x19),
            (4, 0xc5),
            (13, 2),
        ] {
            let mut raw = sodimm();
            raw[offset] = value;
            seal(&mut raw);
            assert_eq!(
                Dimm::decode(&raw),
                Err(Error::UnsupportedDimm),
                "offset {offset}"
            );
        }
        let mut raw = sodimm();
        raw[131] ^= 1;
        assert_eq!(Dimm::decode(&raw), Err(Error::Spd(ddr4::Error::Crc)));
    }
    struct Bus {
        raw: [u8; 512],
        page: usize,
        trace: Vec<(u8, u8)>,
        block_fail: bool,
        fail_at: Option<(usize, u8)>,
        fail_restore: bool,
    }
    impl SmBus for Bus {
        fn read_byte(&mut self, addr: u8, cmd: u8) -> Result<u8, ServiceError> {
            assert_eq!(addr, 0x52);
            if self.fail_at == Some((self.page, cmd)) {
                return Err(ServiceError::HardwareError);
            }
            Ok(self.raw[self.page * 256 + usize::from(cmd)])
        }
        fn write_byte(&mut self, addr: u8, cmd: u8, value: u8) -> Result<(), ServiceError> {
            assert_eq!((cmd, value), (0, 0));
            self.trace.push((addr, value));
            if addr == PAGE_ZERO && self.page == 1 && self.fail_restore {
                return Err(ServiceError::HardwareError);
            }
            self.page = match addr {
                PAGE_ZERO => 0,
                PAGE_ONE => 1,
                _ => panic!(),
            };
            Ok(())
        }
        fn i2c_eeprom_read(
            &mut self,
            addr: u8,
            offset: u8,
            buf: &mut [u8],
        ) -> Result<(), ServiceError> {
            assert_eq!((addr, offset), (0x52, 0));
            if self.block_fail {
                buf.fill(0xff);
                return Err(ServiceError::HardwareError);
            }
            for (i, byte) in buf.iter_mut().enumerate() {
                *byte = self.read_byte(addr, i as u8)?;
            }
            Ok(())
        }
    }
    fn bus() -> Bus {
        Bus {
            raw: sodimm(),
            page: 1,
            trace: Vec::new(),
            block_fail: false,
            fail_at: None,
            fail_restore: false,
        }
    }
    #[test]
    fn paged_spd_reads_block_and_byte_fallback_and_restores_page_zero() {
        for fallback in [false, true] {
            let mut bus = bus();
            bus.block_fail = fallback;
            let mut output = [0; 512];
            read(&mut bus, 0x52, &mut output).unwrap();
            assert_eq!(output, bus.raw);
            assert_eq!(bus.page, 0);
            assert_eq!(bus.trace, [(0x36, 0), (0x37, 0), (0x36, 0)]);
        }
    }
    #[test]
    fn paged_spd_failure_erases_partial_output_and_attempts_restore() {
        for failure in [Some((0, 20)), Some((1, 200)), None] {
            let mut bus = bus();
            bus.fail_at = failure;
            bus.fail_restore = failure.is_none();
            let mut output = [0xff; 512];
            assert!(read(&mut bus, 0x52, &mut output).is_err());
            assert_eq!(output, [0; 512]);
            assert_eq!(bus.trace.last(), Some(&(0x36, 0)));
        }
        let mut bus = bus();
        let mut output = [0; 512];
        assert_eq!(
            read(&mut bus, 0x80, &mut output),
            Err(ServiceError::InvalidParam)
        );
        assert!(bus.trace.is_empty());
    }
}
