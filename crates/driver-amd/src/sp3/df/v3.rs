//! DF3 (Rome SSP, Milan GN): one DF node per socket (the IOD), 10-bit
//! fabric IDs `(socket << 5) + component` (SOC_DF §3.1), and the fabric
//! init steps Rome and Milan share (SOC_DF §3.6).

use fstart_pci::ConfigRegionAccess;
use tock_registers::fields::FieldValue;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;
use tock_registers::registers::InMemoryRegister;

use super::{BusRange, Df, DfReg, DramRule, Version, or_bits};
use crate::df::regs::{self, GLOBAL_CTRL};

/// IOMS fabric components 0..3 (component type 5, SOC_DF §3.1).
pub const IOMS: [u8; 4] = [0x18, 0x19, 0x1A, 0x1B];
/// The IOMS with the FCH on socket 0 (`ffe21d74`: first record with flag 0).
pub const PRIMARY_IOMS: u8 = 0x1B;
/// CCM0: the instance whose CfgAddressMap copy AGESA reads (SOC_DF §3.1).
pub const CCM0: u8 = 0x10;
/// CCM instances.
pub const CCMS: core::ops::RangeInclusive<u8> = 0x10..=0x17;
/// The component (type 4) whose DRAM map and hole AGESA reads (§3.6).
pub const PIE: u8 = 0x1E;
/// First UMC (CS) instance; channel `c` is instance `c` (Milan SoC table
/// `ffd06ab4`: CS base 0).
pub const UMC0: u8 = 0;
/// The CS instance with the extra F2x48 bit (§3.6, data PPI byte 4 = 1).
pub const CS_F2X48_INSTANCE: u8 = 0x0A;

/// SP3 DF3 component table (Rome `ffe21f8c`): `(type, instances)` in the
/// order AGESA walks it for the PIE RAS status clear (§3.6). Types 9/10
/// re-list CS instances.
pub const COMPONENTS: [(u8, &[u8]); 8] = [
    (6, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]),
    (0, &[0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17]),
    (5, &IOMS),
    (4, &[0x1E]),
    (8, &[0x1F, 0x20, 0x21, 0x22, 0x23, 0x24]),
    (
        7,
        &[
            0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F, 0x30,
        ],
    ),
    (9, &[0, 1, 2, 3, 4, 5, 6, 7]),
    (10, &[8, 9, 10, 11]),
];

/// F0x3F8 instances of the fabric data PPI `86d521b8` (Rome `ffe21b2c`,
/// Milan `ffd41cf8`: identical; OB).
pub const GLOBAL_CTRL_INSTANCES: [u8; 31] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x10, 0x11, 0x12, 0x13,
    0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24,
];

/// Scrub-rate thresholds of the data PPI (same 16 values on ZP, §3.5).
pub const SCRUB_THRESHOLDS: [u64; 16] = [
    640, 1280, 2560, 5120, 10200, 20500, 41000, 81900, 163_800, 327_700, 655_400, 1_310_000,
    2_620_000, 5_240_000, 10_490_000, 20_970_000,
];

/// Fabric ID of `component` on `socket`.
pub const fn fabric_id(socket: u8, component: u8) -> u16 {
    ((socket as u16) << 5) + component as u16
}

register_bitfields![u32,
    /// F1x200 SystemCfg (DF3).
    pub SYSTEM_CFG [
        DIE_PRESENT OFFSET(0) NUMBITS(8) [],
        DIE_TYPE OFFSET(11) NUMBITS(8) [],
        MY_TYPE OFFSET(23) NUMBITS(2) [],
        OTHER_TYPE OFFSET(25) NUMBITS(2) [],
        OTHER_SOCK OFFSET(27) NUMBITS(1) [],
        NODE_ID OFFSET(28) NUMBITS(3) []
    ],
    /// F0xA0+4i CfgAddressMap (DF3).
    pub CFG_MAP [
        RE OFFSET(0) NUMBITS(1) [],
        WE OFFSET(1) NUMBITS(1) [],
        DST OFFSET(4) NUMBITS(10) [],
        BUS_BASE OFFSET(16) NUMBITS(8) [],
        BUS_LIMIT OFFSET(24) NUMBITS(8) []
    ],
    /// F0xC4+8i X86IOLimitAddress (DF3).
    pub IO_LIMIT [
        DST OFFSET(0) NUMBITS(10) [],
        LIMIT OFFSET(12) NUMBITS(13) []
    ],
    /// F0x208+16i MmioAddressControl (DF3).
    pub MMIO_CTRL [
        RE OFFSET(0) NUMBITS(1) [],
        WE OFFSET(1) NUMBITS(1) [],
        CPU_DIS OFFSET(2) NUMBITS(1) [],
        DST OFFSET(4) NUMBITS(10) [],
        NP OFFSET(16) NUMBITS(1) []
    ],
    /// F0x80 VGAEn (DF3).
    pub VGA_EN [
        EN OFFSET(0) NUMBITS(1) [],
        NP OFFSET(1) NUMBITS(1) [],
        CPU_DIS OFFSET(2) NUMBITS(1) [],
        DST OFFSET(4) NUMBITS(10) []
    ],
    /// F0x110+8i DramBaseAddress (DF3).
    pub DRAM_BASE [
        VALID OFFSET(0) NUMBITS(1) [],
        HOLE_EN OFFSET(1) NUMBITS(1) [],
        ILV_CHAN OFFSET(2) NUMBITS(4) [],
        ILV_DIE OFFSET(6) NUMBITS(2) [],
        ILV_SOCK OFFSET(8) NUMBITS(1) [],
        ILV_ADDR OFFSET(9) NUMBITS(3) [],
        BASE OFFSET(12) NUMBITS(20) []
    ],
    /// F0x114+8i DramLimitAddress (DF3).
    pub DRAM_LIMIT [
        DST OFFSET(0) NUMBITS(10) [],
        BUS_BREAK OFFSET(10) NUMBITS(1) [],
        LIMIT OFFSET(12) NUMBITS(20) []
    ],
    /// F6x40+16k: scrubber region base (address\[47:20\]). Bit 0 is set on
    /// region 0 only and bit 1 on every used region (Rome `|= 3`, Milan
    /// `ffd02e40`); the names are inferred.
    pub SCRUB_BASE [
        EN OFFSET(0) NUMBITS(1) [],
        REGION_EN OFFSET(1) NUMBITS(1) [],
        BASE OFFSET(4) NUMBITS(28) []
    ],
    /// F6x44+16k: scrub rate and region limit (address\[47:20\], inclusive).
    pub SCRUB_LIMIT [
        RATE OFFSET(0) NUMBITS(4) [],
        LIMIT OFFSET(4) NUMBITS(28) []
    ],
    /// F6x48+16k (Milan): low half of the region's scrub address.
    pub SCRUB_ADDR_LO [
        ADDR OFFSET(6) NUMBITS(26) []
    ],
    /// F6x4C+16k (Milan): high half of the region's scrub address; region 0
    /// also carries the sequential-scrub controls (names from the AGESA
    /// debug strings, `ffd072bc`).
    pub SCRUB_ADDR_HI [
        ADDR OFFSET(0) NUMBITS(16) [],
        SEQ_SCRUB_MODE OFFSET(24) NUMBITS(2) [],
        NUM_SEQ_SCRUBS_CSTATE_EXIT OFFSET(28) NUMBITS(4) []
    ],
    /// F6xB8 \[UMC\]: per-channel scrub controls written from three CBS
    /// options; their meaning is not public (SOC_DF §8).
    pub SCRUB_CHANNEL [
        CTL0 OFFSET(0) NUMBITS(1) [],
        CTL1 OFFSET(1) NUMBITS(1) [],
        MODE OFFSET(2) NUMBITS(2) []
    ],
    /// F2x228 \[UMC\] (SSP-B0, GN): value pattern only (SOC_DF §3.6).
    pub CS_F2X228 [
        EN OFFSET(24) NUMBITS(1) [],
        RATE OFFSET(25) NUMBITS(5) []
    ]
];

pub const SYSTEM_CFG: DfReg<SYSTEM_CFG::Register> = DfReg::new(1, regs::SYSTEM_CFG_OFFSET);
pub const SCRUB_BASE: DfReg<SCRUB_BASE::Register> = DfReg::new(6, 0x40);
pub const SCRUB_LIMIT: DfReg<SCRUB_LIMIT::Register> = DfReg::new(6, 0x44);
pub const SCRUB_ADDR_LO: DfReg<SCRUB_ADDR_LO::Register> = DfReg::new(6, 0x48);
pub const SCRUB_ADDR_HI: DfReg<SCRUB_ADDR_HI::Register> = DfReg::new(6, 0x4C);
pub const SCRUB_REGION_STRIDE: u16 = 0x10;
pub const SCRUB_CHANNEL: DfReg<SCRUB_CHANNEL::Register> = DfReg::new(6, 0xB8);
pub const CS_F2X228: DfReg<CS_F2X228::Register> = DfReg::new(2, 0x228);
/// CS init registers (values only, §3.6).
pub const CS_F2X44: DfReg = DfReg::new(2, 0x44);
pub const CS_F2X48: DfReg = DfReg::new(2, 0x48);
pub const CS_F2X6C: DfReg = DfReg::new(2, 0x6C);
/// CCM F4xC8 (Rome; Milan skips it).
pub const CCM_F4XC8: DfReg = DfReg::new(4, 0xC8);

/// The DF3 layout.
pub enum Df3 {}

impl Version for Df3 {
    type MmioCtrl = MMIO_CTRL::Register;
    type IoLimit = IO_LIMIT::Register;
    type CfgMap = CFG_MAP::Register;
    type VgaEn = VGA_EN::Register;

    fn mmio_ctrl(dst: u16) -> FieldValue<u32, MMIO_CTRL::Register> {
        // The SoC writer stores the full control: NP clear (§3.4.4).
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
            socket_interleave: b.is_set(DRAM_BASE::ILV_SOCK),
            die_interleave: b.read(DRAM_BASE::ILV_DIE) as u8,
            channel_interleave: b.read(DRAM_BASE::ILV_CHAN) as u8,
        })
    }

    fn vga(dst: u16) -> FieldValue<u32, VGA_EN::Register> {
        // AmdFabricSspDxe 0x700c: (v & 0xFFFFC00B) | dst << 4 | 1.
        VGA_EN::EN::SET + VGA_EN::CPU_DIS::CLEAR + VGA_EN::DST.val(u32::from(dst))
    }
}

/// The F0x3F8 PIE RAS fields: `None` keeps the hardware value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasFields {
    /// \[5:4\] (Rome PCD 0xC3, Milan 0x17A; default skip).
    pub a: Option<u8>,
    /// \[10:8\], \[14:12\], \[18:16\] (Rome PCD 0xC4, Milan 0x17B; AGESA maps
    /// the default 0xFF to 6).
    pub b: Option<u8>,
    /// Bit 3 (cleared on Rome; Milan `!PCD 0x41`, default clear).
    pub bit3: bool,
}

impl Default for RasFields {
    fn default() -> Self {
        Self {
            a: None,
            b: Some(6),
            bit3: false,
        }
    }
}

/// Clock gating: F0x3F0 = `value` on every node (§3.6, IC).
pub fn clock_gating<P: ConfigRegionAccess + Clone>(df: &Df<P>, sockets: u8, value: u32) {
    for s in 0..sockets {
        // SAFETY: node `s` exists (topology), F0x3F0 is a node register.
        unsafe { df.node(s, regs::CLOCK_GATING) }.set(value);
    }
}

/// CS init (§3.6, IC; Rome `ffdca302`, Milan `ffd03497`). The F2x48 value
/// for CS 0x0A is the broadcast value with bit 17 added, as AGESA writes it.
pub fn cs_init<P: ConfigRegionAccess + Clone>(df: &Df<P>, sockets: u8) {
    for s in 0..sockets {
        // SAFETY: node registers of an existing node.
        unsafe {
            df.node(s, CS_F2X6C).set(0x1004_0611);
            or_bits(&df.node(s, CS_F2X44), 0x4000);
            let f2x48 = df.node(s, CS_F2X48);
            let v = f2x48.get() | 0x8680_00F0;
            f2x48.set(v);
            df.inst(s, CS_F2X48_INSTANCE, CS_F2X48).set(v | 0x2_0000);
        }
    }
}

/// PIE RAS (§3.6): optionally clear the F6x1F0/1F4 status of the listed
/// components (Rome; Milan only logs them), then program F0x3F8 on the data
/// PPI's instances.
pub fn pie_ras<P: ConfigRegionAccess + Clone>(
    df: &Df<P>,
    sockets: u8,
    ras: RasFields,
    clear_status: bool,
) {
    for s in 0..sockets {
        if clear_status {
            // SAFETY: node registers of an existing node.
            let count = unsafe { df.node(s, regs::FBICNT) }.read(regs::FBICNT::COUNT) as usize;
            // AGESA walks whole table entries until it has visited `count`
            // instances (`ffdc9c96`).
            let mut seen = 0;
            for (_, insts) in COMPONENTS.iter() {
                if seen >= count {
                    break;
                }
                for &i in insts.iter() {
                    for reg in [regs::PIE_RAS_STATUS_LO, regs::PIE_RAS_STATUS_HI] {
                        // SAFETY: a listed instance of an existing node.
                        let r = unsafe { df.inst(s, i, reg) };
                        if r.get() != 0 {
                            r.set(0);
                        }
                    }
                }
                seen += insts.len();
            }
        }
        for &i in GLOBAL_CTRL_INSTANCES.iter() {
            // SAFETY: a data-PPI instance of an existing node.
            let r = unsafe { df.inst(s, i, regs::GLOBAL_CTRL) };
            let mut v = r.extract();
            if let Some(a) = ras.a.filter(|&a| a < 4) {
                v.modify(GLOBAL_CTRL::RAS_A.val(u32::from(a)));
            }
            if let Some(b) = ras.b.filter(|&b| b < 8) {
                let b = u32::from(b);
                v.modify(
                    GLOBAL_CTRL::RAS_B0.val(b)
                        + GLOBAL_CTRL::RAS_B1.val(b)
                        + GLOBAL_CTRL::RAS_B2.val(b),
                );
            }
            v.modify(GLOBAL_CTRL::BIT3.val(u32::from(ras.bit3)));
            r.set(v.get());
        }
    }
}

/// Scrub rate index: `q = hours · 3.96e12 / (bytes / 64)`, the last
/// threshold `<= q` (0 if none). `shift` is 6 plus Milan's SeqScrubMode.
pub fn scrub_rate(hours: u8, bytes: u64, shift: u32) -> u8 {
    let lines = bytes >> shift;
    if lines == 0 {
        return 0;
    }
    let q = u64::from(hours) * 3_960_000_000_000 / lines;
    SCRUB_THRESHOLDS.iter().rposition(|&t| t <= q).unwrap_or(0) as u8
}

/// A socket's memory channels as the scrubber sees them (AGESA: the memory
/// info PPI, from the APOB).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SocketMemory {
    /// UMC channels of the socket (8 on SP3).
    pub channels: u8,
    /// Bit `c`: channel `c` has DIMMs.
    pub present: u16,
}

/// Per-channel scrub controls (F6xB8 on each UMC instance, §3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelCtl {
    pub ctl0: bool,
    pub ctl1: bool,
    /// Already mapped field value (AGESA map {0→3, 1→1, 2→2, 3→0}, other → 0).
    pub mode: u8,
}

impl ChannelCtl {
    /// AGESA's map of the mode CBS option (Rome PCD 0xC2, Milan 0x16E).
    pub const fn mode_from_option(option: u8) -> u8 {
        match option {
            0 => 3,
            1 => 1,
            2 => 2,
            _ => 0,
        }
    }
}

/// Program F6xB8 of channels `0..channels`; populated channels get the
/// controls, the others have bits 1:0 cleared (Rome `ffdc9eec`).
pub fn scrub_channels<P: ConfigRegionAccess + Clone>(
    df: &Df<P>,
    socket: u8,
    channels: u8,
    present: u16,
    ctl: ChannelCtl,
) {
    for ch in 0..channels {
        let p = present & (1 << ch) != 0;
        // SAFETY: a UMC instance of an existing node.
        let r = unsafe { df.inst(socket, UMC0 + ch, SCRUB_CHANNEL) };
        let mut v = r.extract();
        v.modify(
            SCRUB_CHANNEL::CTL0.val(u32::from(ctl.ctl0 && p))
                + SCRUB_CHANNEL::CTL1.val(u32::from(ctl.ctl1 && p)),
        );
        if p {
            v.modify(SCRUB_CHANNEL::MODE.val(u32::from(ctl.mode)));
        }
        r.set(v.get());
    }
}

/// F2x228 of channels `0..channels` (§3.6): bit 24 set, \[29:25\] =
/// `capped ? 0 : 8 − min(rate, 8)`.
pub fn cs_f2x228<P: ConfigRegionAccess + Clone>(
    df: &Df<P>,
    socket: u8,
    channels: u8,
    rate: u8,
    capped: bool,
) {
    let field = if capped {
        0
    } else {
        8 - u32::from(rate.min(8))
    };
    for ch in 0..channels {
        // SAFETY: a UMC instance of an existing node.
        unsafe { df.inst(socket, UMC0 + ch, CS_F2X228) }
            .modify(CS_F2X228::EN::SET + CS_F2X228::RATE.val(field));
    }
}

/// DRAM behind `socket`'s scrubber (Rome `ffdca236`): the F6x40/44 range
/// minus the MMIO hole when one of DRAM rules 0/1 (PIE copy) targets this
/// socket with the hole enabled.
pub fn rome_scrub_bytes<P: ConfigRegionAccess + Clone>(df: &Df<P>, socket: u8) -> u64 {
    // SAFETY: node and PIE registers of an existing node.
    unsafe {
        let base = df.node(socket, SCRUB_BASE).read(SCRUB_BASE::BASE);
        let limit_reg = df.node(socket, SCRUB_LIMIT);
        if limit_reg.get() == 0 {
            return 0;
        }
        let limit = limit_reg.read(SCRUB_LIMIT::LIMIT);
        let size = u64::from(limit.wrapping_sub(base).wrapping_add(1)) << 20;
        let hole = df.inst(socket, PIE, regs::DRAM_HOLE);
        (0..2u8)
            .filter(|&i| {
                let b = df
                    .inst(
                        socket,
                        PIE,
                        DfReg::<DRAM_BASE::Register>::new(0, regs::DRAM_BASE_OFFSET)
                            .at(i, regs::DRAM_STRIDE),
                    )
                    .extract();
                let l = df
                    .inst(
                        socket,
                        PIE,
                        DfReg::<DRAM_LIMIT::Register>::new(0, regs::DRAM_LIMIT_OFFSET)
                            .at(i, regs::DRAM_STRIDE),
                    )
                    .get();
                b.is_set(DRAM_BASE::VALID)
                    && (l & 0x20) == u32::from(socket) << 5
                    && b.is_set(DRAM_BASE::HOLE_EN)
                    && hole.is_set(regs::DRAM_HOLE::VALID)
            })
            .fold(size, |size, _| {
                size.wrapping_sub((1 << 32) - (u64::from(hole.read(regs::DRAM_HOLE::BASE)) << 24))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::DfModel;
    use super::*;

    #[test]
    fn layout() {
        assert_eq!(fabric_id(1, PRIMARY_IOMS), 0x3B);
        // §3.4.4: (FabricId & 0x3FF) << 4 | 3.
        assert_eq!(Df3::mmio_ctrl(0x3B).value, 0x3B3);
        assert_eq!(Df3::io_limit(0x7FFF, 0x1B).value, 0x7000 | 0x1B);
        let map = Df3::cfg_map(
            BusRange {
                base: 0,
                limit: 0x3F,
            },
            0x1B,
        )
        .value;
        assert_eq!(map, 0x3F00_01B3);
        assert_eq!(Df3::decode_cfg_map(map).unwrap().1, 0x1B);
        assert_eq!(Df3::decode_cfg_map(map & !2), None);
    }

    #[test]
    fn rate() {
        // 256 GiB at 24 h: q = 24 * 3.96e12 / 2^32 = 22128 -> threshold 20500 (5).
        assert_eq!(scrub_rate(24, 256 << 30, 6), 5);
        assert_eq!(scrub_rate(24, 0, 6), 0);
        assert_eq!(scrub_rate(1, 1 << 50, 6), 0);
    }

    #[test]
    fn cs_and_ras() {
        let m = DfModel::default();
        let df = Df::new(m.clone());
        m.set(0, None, 2, 0x48, 0x1);
        m.set(0, None, 0, 0x40, 2);
        m.set(0, Some(1), 6, 0x1F0, 0x55);
        m.set(0, Some(0x18), 0, 0x3F8, 0xFF_FFFF);
        cs_init(&df, 1);
        assert_eq!(m.get(0, None, 2, 0x6C), 0x1004_0611);
        assert_eq!(m.get(0, None, 2, 0x44), 0x4000);
        assert_eq!(m.get(0, Some(0x0A), 2, 0x48), 0x8682_00F1);
        pie_ras(&df, 1, RasFields::default(), true);
        assert_eq!(m.get(0, Some(1), 6, 0x1F0), 0);
        // Fields B = 6, bit 3 clear, rest kept.
        assert_eq!(m.get(0, Some(0x18), 0, 0x3F8), 0xFE_EEF7);
    }
}
