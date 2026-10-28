//! The DF2 (Naples) layout of the resource programmers; the registers are
//! [`crate::df::v2`].

use tock_registers::fields::FieldValue;
use tock_registers::interfaces::Readable;
use tock_registers::registers::InMemoryRegister;

use super::{BusRange, DramRule, Version};
use crate::df::v2::{CFG_MAP, DRAM_BASE, DRAM_LIMIT, IO_LIMIT, MMIO_CTRL, VGA_EN};

/// The DF2 layout.
pub enum Df2 {}

impl Version for Df2 {
    type MmioCtrl = MMIO_CTRL::Register;
    type IoLimit = IO_LIMIT::Register;
    type CfgMap = CFG_MAP::Register;
    type VgaEn = VGA_EN::Register;

    fn mmio_ctrl(dst: u16) -> FieldValue<u32, MMIO_CTRL::Register> {
        MMIO_CTRL::RE::SET + MMIO_CTRL::WE::SET + MMIO_CTRL::DST.val(u32::from(dst))
    }

    fn io_limit(limit: u32, dst: u16) -> FieldValue<u32, IO_LIMIT::Register> {
        IO_LIMIT::LIMIT.val(limit >> 12) + IO_LIMIT::DST.val(u32::from(dst))
    }

    fn cfg_map(bus: BusRange, dst: u16) -> FieldValue<u32, CFG_MAP::Register> {
        CFG_MAP::RE::SET
            + CFG_MAP::WE::SET
            + CFG_MAP::DST.val(u32::from(dst))
            + CFG_MAP::BUS_BASE.val(u32::from(bus.base))
            + CFG_MAP::BUS_LIMIT.val(u32::from(bus.limit))
    }

    fn decode_cfg_map(value: u32) -> Option<(BusRange, u16)> {
        let r = InMemoryRegister::<u32, CFG_MAP::Register>::new(value);
        (r.is_set(CFG_MAP::RE) && r.is_set(CFG_MAP::WE)).then(|| {
            let bus = BusRange {
                base: r.read(CFG_MAP::BUS_BASE) as u8,
                limit: r.read(CFG_MAP::BUS_LIMIT) as u8,
            };
            (bus, r.read(CFG_MAP::DST) as u16)
        })
    }

    fn decode_dram(base: u32, limit: u32) -> Option<DramRule> {
        let b = InMemoryRegister::<u32, DRAM_BASE::Register>::new(base);
        let l = InMemoryRegister::<u32, DRAM_LIMIT::Register>::new(limit);
        b.is_set(DRAM_BASE::VALID).then(|| DramRule {
            base: u64::from(b.read(DRAM_BASE::BASE)) << 28,
            end: (u64::from(l.read(DRAM_LIMIT::LIMIT)) + 1) << 28,
            dst: l.read(DRAM_LIMIT::DST) as u16,
            hole: b.is_set(DRAM_BASE::HOLE_EN),
            socket_interleave: l.is_set(DRAM_LIMIT::ILV_SOCK),
            die_interleave: l.read(DRAM_LIMIT::ILV_DIE) as u8,
            channel_interleave: b.read(DRAM_BASE::ILV_CHAN) as u8,
        })
    }

    fn vga(dst: u16) -> FieldValue<u32, VGA_EN::Register> {
        VGA_EN::EN::SET + VGA_EN::CPU_DIS::CLEAR + VGA_EN::DST.val(u32::from(dst))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::df::v2::{IOMS_COMPONENT, fabric_id, node};

    #[test]
    fn ids_and_layout() {
        assert_eq!(fabric_id(1, 2, IOMS_COMPONENT), 0xC4);
        assert_eq!(node(1, 3), 7);
        // DF_ZP §3.10: control = 3 | dst << 4.
        assert_eq!(Df2::mmio_ctrl(fabric_id(0, 1, 4)).value, 0x243);
        let map = Df2::cfg_map(
            BusRange {
                base: 0x40,
                limit: 0x7F,
            },
            0x24,
        )
        .value;
        assert_eq!(map, 0x7F40_0243);
        assert_eq!(
            Df2::decode_cfg_map(map),
            Some((
                BusRange {
                    base: 0x40,
                    limit: 0x7F
                },
                0x24
            ))
        );
        let rule = Df2::decode_dram(0x0000_0003, 0x0000_1024).unwrap();
        assert_eq!(
            (rule.base, rule.end, rule.dst, rule.hole),
            (0, 0x2000_0000, 0x24, true)
        );
    }
}
