//! DF2 (Naples, ZP): one DF node per die, 8-bit fabric IDs
//! `socket[7] | die[6:5] | component[4:0]` (SOC_DF §3.1, DF_ZP §3.2).

use tock_registers::fields::FieldValue;
use tock_registers::interfaces::Readable;
use tock_registers::register_bitfields;
use tock_registers::registers::InMemoryRegister;

use super::{BusRange, DramRule, Reg, Version};

/// IOMS component of a ZP die (DF_ZP §3.2: component map kind 5 = (4,4)).
pub const IOMS_COMPONENT: u16 = 4;
/// ZP fabric instances of the DRAM map reads (PIE, inst 5; DF_ZP §3.8).
pub const PIE_INSTANCE: u8 = 5;
/// ZP CCM instances (DF_ZP §3.5).
pub const CCM_INSTANCES: [u8; 2] = [2, 3];
/// ZP IOMS instance.
pub const IOMS_INSTANCE: u8 = 4;
/// ZP CS (UMC) instances.
pub const CS_INSTANCES: [u8; 2] = [0, 1];

/// Fabric ID of `component` on `socket`/`die`.
pub const fn fabric_id(socket: u8, die: u8, component: u16) -> u16 {
    (socket as u16) << 7 | (die as u16) << 5 | component
}

/// DF node of `socket`/`die` (`0x18 + node` on bus 0).
pub const fn node(socket: u8, die: u8) -> u8 {
    socket * 4 + die
}

register_bitfields![u32,
    /// F1x200 SystemCfg (DF2).
    pub SYSTEM_CFG [
        DIE_PRESENT OFFSET(0) NUMBITS(4) [],
        OTHER_SOCK OFFSET(5) NUMBITS(1) [],
        MY_TYPE OFFSET(21) NUMBITS(2) [],
        DIE_ID OFFSET(24) NUMBITS(2) [],
        SOCK_ID OFFSET(27) NUMBITS(1) []
    ],
    /// F0xA0+4i CfgAddressMap (DF2).
    pub CFG_MAP [
        RE OFFSET(0) NUMBITS(1) [],
        WE OFFSET(1) NUMBITS(1) [],
        DST OFFSET(4) NUMBITS(8) [],
        BUS_BASE OFFSET(16) NUMBITS(8) [],
        BUS_LIMIT OFFSET(24) NUMBITS(8) []
    ],
    /// F0xC4+8i X86IOLimitAddress (DF2).
    pub IO_LIMIT [
        DST OFFSET(0) NUMBITS(8) [],
        LIMIT OFFSET(12) NUMBITS(13) []
    ],
    /// F0x208+16i MmioAddressControl (DF2).
    pub MMIO_CTRL [
        RE OFFSET(0) NUMBITS(1) [],
        WE OFFSET(1) NUMBITS(1) [],
        CPU_DIS OFFSET(2) NUMBITS(1) [],
        DST OFFSET(4) NUMBITS(8) [],
        NP OFFSET(12) NUMBITS(1) []
    ],
    /// F0x80 VGAEn (DF2).
    pub VGA_EN [
        EN OFFSET(0) NUMBITS(1) [],
        NP OFFSET(1) NUMBITS(1) [],
        CPU_DIS OFFSET(2) NUMBITS(1) [],
        DST OFFSET(4) NUMBITS(8) []
    ],
    /// F0x110+8i DramBaseAddress (DF2).
    pub DRAM_BASE [
        VALID OFFSET(0) NUMBITS(1) [],
        HOLE_EN OFFSET(1) NUMBITS(1) [],
        ILV_CHAN OFFSET(4) NUMBITS(4) [],
        ILV_ADDR OFFSET(8) NUMBITS(3) [],
        BASE OFFSET(12) NUMBITS(20) []
    ],
    /// F0x114+8i DramLimitAddress (DF2).
    pub DRAM_LIMIT [
        DST OFFSET(0) NUMBITS(8) [],
        ILV_SOCK OFFSET(8) NUMBITS(1) [],
        ILV_DIE OFFSET(10) NUMBITS(2) [],
        LIMIT OFFSET(12) NUMBITS(20) []
    ]
];

pub const SYSTEM_CFG: Reg<SYSTEM_CFG::Register> = Reg::new(1, super::regs::SYSTEM_CFG_OFFSET);

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
