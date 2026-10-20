// SPDX-License-Identifier: MIT OR Apache-2.0
//! The NBIO registers Raven/Picasso and Zeppelin share: the NB/IOHC
//! registers of the root complex and the PCIe core and port registers, all
//! in SMN. A Zeppelin die has two PCIe cores, Picasso one.
use super::registers::SmnReg;
use tock_registers::register_bitfields;

register_bitfields![u32,
    /// D0F0x90 (also SMN 0x13b00090).
    pub TopOfDram [TOM OFFSET(23) NUMBITS(9) []],
    pub NbPciCtrl [MMIO_ENABLE OFFSET(23) NUMBITS(1) []],
    pub NbBusNumCntl [
        BUS OFFSET(0) NUMBITS(8) [],
        LAT_MODE OFFSET(8) NUMBITS(1) []
    ],
    pub IohcMmioBar [ENABLE OFFSET(0) NUMBITS(1) [], ADDRESS OFFSET(8) NUMBITS(24) []],
    pub IohcFeatureCntl [
        /// Vendor PCD 0x1d (ARI support).
        ARI OFFSET(22) NUMBITS(1) []
    ],
    pub NbProgDeviceRemap [DEVFN OFFSET(0) NUMBITS(8) []],
    pub IohcBridgeCntl [
        BRIDGE_DIS OFFSET(0) NUMBITS(1) [],
        CFG_DIS OFFSET(2) NUMBITS(1) [],
        CRS_ENABLE OFFSET(18) NUMBITS(1) []
    ],
    pub IohcRbBusRoute [
        BASE OFFSET(0) NUMBITS(8) [],
        LIMIT OFFSET(8) NUMBITS(8) [],
        /// Set for the bus ranges of the other dies.
        REMOTE OFFSET(31) NUMBITS(1) []
    ],
    /// The names are the later IOHC's; the vendor code sets 0x14 on die 0
    /// and 0x34 on the others.
    pub IoapicFeatures [
        ID_EXT OFFSET(2) NUMBITS(1) [],
        FCH OFFSET(4) NUMBITS(1) [],
        SECONDARY OFFSET(5) NUMBITS(1) []
    ],
    /// The PSP and SMU BARs (low halves).
    pub Bar [
        ENABLE OFFSET(0) NUMBITS(1) [],
        LOCK OFFSET(8) NUMBITS(1) [],
        /// Address bits [31:20].
        BASE OFFSET(20) NUMBITS(12) []
    ]
];

// NB configuration space (SMN), also D0F0 of the die.
pub const NB_PCI_CTRL: SmnReg<NbPciCtrl::Register> = SmnReg::new(0x13b0_004c);
/// D0F0x90, in config space.
pub const NB_TOP_OF_DRAM_SLOT1: u16 = 0x90;
pub const NB_TOP_OF_DRAM_SLOT1_SMN: SmnReg<TopOfDram::Register> = SmnReg::new(0x13b0_0090);

// IOHC (SMN)
const IOHC_BASE: u32 = 0x13b1_0000;
pub const NB_BUS_NUM_CNTL: SmnReg<NbBusNumCntl::Register> = SmnReg::new(IOHC_BASE + 0x044);
pub const NB_MMIOBASE: SmnReg = SmnReg::new(IOHC_BASE + 0x05c);
pub const NB_MMIOLIMIT: SmnReg = SmnReg::new(IOHC_BASE + 0x060);
pub const NB_LOWER_TOP_OF_DRAM2: SmnReg = SmnReg::new(IOHC_BASE + 0x064);
pub const NB_UPPER_TOP_OF_DRAM2: SmnReg = SmnReg::new(IOHC_BASE + 0x068);
pub const fn nb_prog_device_remap(port: usize) -> SmnReg<NbProgDeviceRemap::Register> {
    SmnReg::new(IOHC_BASE + 0x0c0 + port as u32 * 4)
}
pub const IOHC_FEATURE_CNTL: SmnReg<IohcFeatureCntl::Register> =
    SmnReg::new(IOHC_BASE + 0x118);
pub const NB_TOP_OF_DRAM3: SmnReg = SmnReg::new(IOHC_BASE + 0x138);
/// Bus routing between the root bridges of the dies.
pub const fn iohc_rb_bus_route(rb: usize) -> SmnReg<IohcRbBusRoute::Register> {
    SmnReg::new(IOHC_BASE + 0x190 + rb as u32 * 0x14)
}
pub const PSP_BASE_ADDR_LO: SmnReg<Bar::Register> = SmnReg::new(IOHC_BASE + 0x2e0);
pub const PSP_BASE_ADDR_HI: SmnReg = SmnReg::new(IOHC_BASE + 0x2e4);
pub const SMU_BASE_ADDR_LO: SmnReg<Bar::Register> = SmnReg::new(IOHC_BASE + 0x2e8);
pub const SMU_BASE_ADDR_HI: SmnReg = SmnReg::new(IOHC_BASE + 0x2ec);
pub const IOAPIC_BASE_ADDR_LO: SmnReg<IohcMmioBar::Register> = SmnReg::new(IOHC_BASE + 0x2f0);
pub const IOAPIC_BASE_ADDR_HI: SmnReg = SmnReg::new(IOHC_BASE + 0x2f4);
/// The 16 PCIe bridges of a die each have a bridge control register.
pub const fn iohc_bridge_cntl(bridge: usize) -> SmnReg<IohcBridgeCntl::Register> {
    SmnReg::new(0x13b3_1004 + bridge as u32 * 0x400)
}
pub const IOAPIC_FEATURES_ENABLE: SmnReg<IoapicFeatures::Register> = SmnReg::new(0x1430_0000);

// PCIe core c (0x11180000 + c MiB) and its port p (p / 8 = c). The
// register offsets are the index register numbers * 4; the names are those
// of the NBIO 6.1 PCIe registers.
pub const PCIE_PORTS_PER_CORE: usize = 8;
const PCIE_CORE_STRIDE: u32 = 0x10_0000;

pub mod pcie {
    use super::{PCIE_CORE_STRIDE, PCIE_PORTS_PER_CORE};
    use crate::fam17h::registers::SmnReg;
    use tock_registers::register_bitfields;

    register_bitfields![u32,
        pub Cntl [
            HWINIT_WR_LOCK OFFSET(0) NUMBITS(1) [],
            RX_RCB_INVALID_SIZE_DIS OFFSET(17) NUMBITS(1) []
        ],
        pub ConfigCntl [DYN_CLK_LATENCY OFFSET(0) NUMBITS(4) []],
        pub PCntl [
            P_IGNORE_EDB_ERR OFFSET(6) NUMBITS(1) [],
            P_ELEC_IDLE_MODE OFFSET(14) NUMBITS(2) []
        ],
        /// The vendor clock gating: two bits set, four cleared.
        pub Cntl18c [GATING OFFSET(5) NUMBITS(2) []],
        pub StrapF0 [
            /// Vendor PCD 0xdf.
            PCD_DF OFFSET(13) NUMBITS(2) [],
            /// Vendor PCD 0xd1/0xd2.
            PCD_D1 OFFSET(18) NUMBITS(1) []
        ],
        /// Bit per port 1-7 of the core: not trained.
        pub LinkStatus [NOT_TRAINED OFFSET(0) NUMBITS(8) []],
        /// [2:0] function and [15:3] of 0x084: the devfn the port answers to.
        pub Port008 [FUNCTION OFFSET(0) NUMBITS(3) [], BIT15 OFFSET(15) NUMBITS(1) []],
        pub Port084 [DEVICE OFFSET(3) NUMBITS(13) []],
        pub TxCntl [TX_FLUSH_TLP_DIS OFFSET(15) NUMBITS(1) []],
        pub LcCntl [LC_L1_IMMEDIATE_ACK OFFSET(23) NUMBITS(1) []],
        pub LcLinkWidthCntl [
            LC_DUAL_END_RECONFIG_EN OFFSET(19) NUMBITS(1) [],
            LC_DYN_LANES_PWR_STATE OFFSET(21) NUMBITS(2) []
        ],
        /// Port 0 of a core only.
        pub LcPort2dc [NOT_TRAINED OFFSET(28) NUMBITS(1) []],
        pub LcCntl2 [LC_ELEC_IDLE_MODE OFFSET(14) NUMBITS(2) []],
        pub LcCntl6 [
            /// Vendor PCD 0xe0.
            LC_SPC_MODE_2P5GT OFFSET(0) NUMBITS(1) [],
            /// Vendor PCD 0xe1.
            LC_SPC_MODE_5GT OFFSET(2) NUMBITS(1) [],
            LC_SPC_MODE_8GT OFFSET(4) NUMBITS(1) [],
            /// PLL off in L1.
            LC_L1_POWERDOWN OFFSET(21) NUMBITS(1) []
        ],
        pub LcCntl7 [
            /// Vendor PCD 0xe5.
            LC_SCHEDULED_RXEQEVAL_INTERVAL OFFSET(13) NUMBITS(8) []
        ],
        pub Port304 [BITS OFFSET(1) NUMBITS(2) []],
        /// PCI Express Device Control 2, in the port's config space.
        pub DeviceControl2 [COMPLETION_TIMEOUT OFFSET(0) NUMBITS(4) []]
    ];

    const fn core(core: usize, offset: u32) -> u32 {
        0x1118_0000 + core as u32 * PCIE_CORE_STRIDE + offset
    }
    const fn port_offset(port: usize) -> u32 {
        (port / PCIE_PORTS_PER_CORE) as u32 * PCIE_CORE_STRIDE
            + (port % PCIE_PORTS_PER_CORE) as u32 * 0x1000
    }
    const fn port(port: usize, offset: u32) -> u32 {
        0x1114_0000 + port_offset(port) + offset
    }

    pub const fn cntl(c: usize) -> SmnReg<Cntl::Register> {
        SmnReg::new(core(c, 0x040))
    }
    pub const fn config_cntl(c: usize) -> SmnReg<ConfigCntl::Register> {
        SmnReg::new(core(c, 0x044))
    }
    pub const fn p_cntl(c: usize) -> SmnReg<PCntl::Register> {
        SmnReg::new(core(c, 0x100))
    }
    pub const fn cntl_18c(c: usize) -> SmnReg<Cntl18c::Register> {
        SmnReg::new(core(c, 0x18c))
    }
    pub const fn strap_f0(c: usize) -> SmnReg<StrapF0::Register> {
        SmnReg::new(core(c, 0x2c0))
    }
    pub const fn link_status(c: usize) -> SmnReg<LinkStatus::Register> {
        SmnReg::new(core(c, 0x428))
    }
    /// The clock power management control; the vendor values are raw.
    pub const fn cpm_control(c: usize) -> u32 {
        core(c, 0x460)
    }
    /// A raw register of core `c`, for vendor values.
    pub const fn core_raw(c: usize, offset: u32) -> u32 {
        core(c, offset)
    }
    /// A raw register of port `p`, for vendor values.
    pub const fn port_raw(p: usize, offset: u32) -> u32 {
        port(p, offset)
    }
    pub const fn port_084(p: usize) -> SmnReg<Port084::Register> {
        SmnReg::new(port(p, 0x084))
    }
    pub const fn port_008(p: usize) -> SmnReg<Port008::Register> {
        SmnReg::new(port(p, 0x008))
    }
    pub const fn tx_cntl(p: usize) -> SmnReg<TxCntl::Register> {
        SmnReg::new(port(p, 0x080))
    }
    pub const fn lc_cntl(p: usize) -> SmnReg<LcCntl::Register> {
        SmnReg::new(port(p, 0x280))
    }
    pub const fn lc_link_width_cntl(p: usize) -> SmnReg<LcLinkWidthCntl::Register> {
        SmnReg::new(port(p, 0x288))
    }
    pub const fn lc_cntl2(p: usize) -> SmnReg<LcCntl2::Register> {
        SmnReg::new(port(p, 0x2c4))
    }
    pub const fn lc_port_2dc(p: usize) -> SmnReg<LcPort2dc::Register> {
        SmnReg::new(port(p, 0x2dc))
    }
    pub const fn lc_cntl6(p: usize) -> SmnReg<LcCntl6::Register> {
        SmnReg::new(port(p, 0x2ec))
    }
    pub const fn lc_cntl7(p: usize) -> SmnReg<LcCntl7::Register> {
        SmnReg::new(port(p, 0x2f0))
    }
    pub const fn port_304(p: usize) -> SmnReg<Port304::Register> {
        SmnReg::new(port(p, 0x304))
    }
    /// Device Control 2 of the root port, through its SMN config alias.
    pub const fn device_control2(p: usize) -> SmnReg<DeviceControl2::Register> {
        SmnReg::new(0x1110_0000 + port_offset(p) + 0x80)
    }
    /// Indirect strap access of a core: index, data.
    pub const fn strap_index(c: usize) -> SmnReg {
        SmnReg::new(0x0004_a348 + c as u32 * 0x80)
    }
    pub const fn strap_data(c: usize) -> SmnReg {
        SmnReg::new(0x0004_a34c + c as u32 * 0x80)
    }
}

