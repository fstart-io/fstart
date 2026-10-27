// SPDX-License-Identifier: MIT OR Apache-2.0
//! The PCIe core and port registers of the NBIO, all in SMN. A Zeppelin die
//! has two PCIe cores, Picasso one.
use crate::smn::SmnReg;
use tock_registers::register_bitfields;

// PCIe core c (0x11180000 + c MiB) and its port p (p / 8 = c). The
// register offsets are the index register numbers * 4; the names are those
// of the NBIO 6.1 PCIe registers.
pub const PCIE_PORTS_PER_CORE: usize = 8;
const PCIE_CORE_STRIDE: u32 = 0x10_0000;

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
