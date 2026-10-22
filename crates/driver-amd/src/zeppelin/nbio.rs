// SPDX-License-Identifier: MIT OR Apache-2.0
//! NBIO base initialization of the vendor AmdNbioBaseZPPei and the late
//! settings of AmdNbioBaseZPDxe/AmdNbioPcieZPDxe, per die (sp4r2 report
//! NBIO_BASE_ZP and nbio-base-contracts.json).
use super::{Revision, topology::Die, topology::NONPCI_IOAPIC};
use crate::fam17h::registers::Smn;
use fstart_pci::ConfigRegionAccess;
use tock_registers::register_bitfields;

pub use super::nbio_tables::{BASE, IOMMU};

/// One record of a vendor register table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `reg = (reg & !clear) | set`
    Update {
        clear: u32,
        set: u32,
    },
    Write(u32),
    /// `reg = SMN[from]`
    Copy {
        from: u32,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reg {
    pub address: u32,
    pub op: Op,
}
pub const fn rmw(address: u32, clear: u32, set: u32) -> Reg {
    Reg {
        address,
        op: Op::Update { clear, set },
    }
}
pub const fn write(address: u32, value: u32) -> Reg {
    Reg {
        address,
        op: Op::Write(value),
    }
}
pub const fn copy(address: u32, from: u32) -> Reg {
    Reg {
        address,
        op: Op::Copy { from },
    }
}

/// Apply a register table in order.
pub fn apply(smn: &mut impl Smn, table: &[Reg]) {
    for reg in table {
        match reg.op {
            Op::Update { clear, set } => smn.update(reg.address, clear, set),
            Op::Write(value) => smn.write(reg.address, value),
            Op::Copy { from } => {
                let value = smn.read(from);
                smn.write(reg.address, value);
            }
        }
    }
}

// NB configuration space (SMN), also D0F0 of the die.
const NBCFG_BASE: u32 = 0x13b0_0000;
const NB_PCI_CTRL: u32 = NBCFG_BASE + 0x4c;
const NB_TOP_OF_DRAM_SLOT1: u16 = 0x90;

// IOHC (SMN)
const IOHC_BASE: u32 = 0x13b1_0000;
const NB_BUS_NUM_CNTL: u32 = IOHC_BASE + 0x044;
const NB_MMIOBASE: u32 = IOHC_BASE + 0x05c;
const NB_MMIOLIMIT: u32 = IOHC_BASE + 0x060;
const NB_LOWER_TOP_OF_DRAM2: u32 = IOHC_BASE + 0x064;
const NB_UPPER_TOP_OF_DRAM2: u32 = IOHC_BASE + 0x068;
pub(super) const fn nb_prog_device_remap(port: usize) -> u32 {
    IOHC_BASE + 0x0c0 + port as u32 * 4
}
pub(super) const IOHC_FEATURE_CNTL: u32 = IOHC_BASE + 0x118;
const NB_TOP_OF_DRAM3: u32 = IOHC_BASE + 0x138;
/// Bus routing between the root bridges of the dies.
pub(super) const fn iohc_rb_bus_route(rb: usize) -> u32 {
    IOHC_BASE + 0x190 + rb as u32 * 0x14
}
const IOAPIC_BASE_ADDR_LO: u32 = IOHC_BASE + 0x2f0;
const IOAPIC_BASE_ADDR_HI: u32 = IOAPIC_BASE_ADDR_LO + 4;
/// The 16 PCIe bridges of a die each have a bridge control register.
pub(super) const fn iohc_bridge_cntl(bridge: usize) -> u32 {
    0x13b3_1004 + bridge as u32 * 0x400
}
const IOAPIC_FEATURES_ENABLE: u32 = 0x1430_0000;

register_bitfields![u32,
    pub(super) NbPciCtrl [MMIO_ENABLE OFFSET(23) NUMBITS(1) []],
    pub(super) NbBusNumCntl [
        BUS OFFSET(0) NUMBITS(8) [],
        LAT_MODE OFFSET(8) NUMBITS(1) []
    ],
    pub(super) IohcMmioBar [ENABLE OFFSET(0) NUMBITS(1) []],
    pub(super) IohcFeatureCntl [
        /// Vendor PCD 0x1d (ARI support).
        ARI OFFSET(22) NUMBITS(1) []
    ],
    pub(super) IohcBridgeCntl [
        BRIDGE_DIS OFFSET(0) NUMBITS(1) [],
        CFG_DIS OFFSET(2) NUMBITS(1) [],
        CRS_ENABLE OFFSET(18) NUMBITS(1) []
    ],
    pub(super) IohcRbBusRoute [
        BASE OFFSET(0) NUMBITS(8) [],
        LIMIT OFFSET(8) NUMBITS(8) [],
        /// Set for the bus ranges of the other dies.
        REMOTE OFFSET(31) NUMBITS(1) []
    ]
];

// PCIe core c of a die (c = 0: lanes 0-15, devices 1.x and 2.1; c = 1: lanes
// 16-31, devices 3.x and 4.1). Port p (0-15) belongs to core p / 8. The
// register offsets are the index register numbers * 4.
pub const PCIE_CORE_COUNT: usize = 2;
pub const PCIE_PORTS_PER_CORE: usize = 8;
pub const PCIE_PORT_COUNT: usize = PCIE_CORE_COUNT * PCIE_PORTS_PER_CORE;
const PCIE_CORE_STRIDE: u32 = 0x10_0000;

pub(super) mod pcie {
    pub const CNTL: u32 = 0x040;
    pub const CONFIG_CNTL: u32 = 0x044;
    pub const P_CNTL: u32 = 0x100;
    pub const CNTL_18C: u32 = 0x18c;
    pub const STRAP_F0: u32 = 0x2c0;
    /// Bit per port 1-7 of the core: not trained.
    pub const LINK_STATUS: u32 = 0x428;
    pub const CPM_CONTROL: u32 = 0x460;

    pub const PORT_008: u32 = 0x008;
    pub const TX_CNTL: u32 = 0x080;
    pub const LC_CNTL: u32 = 0x280;
    pub const LC_LINK_WIDTH_CNTL: u32 = 0x288;
    /// Bit 28: port 0 of a core not trained.
    pub const LC_PORT_2DC: u32 = 0x2dc;
    pub const LC_CNTL2: u32 = 0x2c4;
    pub const LC_CNTL6: u32 = 0x2ec;
    pub const LC_CNTL7: u32 = 0x2f0;
    pub const PORT_304: u32 = 0x304;
}

pub(super) const fn pcie_core_reg(core: usize, offset: u32) -> u32 {
    0x1118_0000 + core as u32 * PCIE_CORE_STRIDE + offset
}
const fn port_offset(port: usize) -> u32 {
    (port / PCIE_PORTS_PER_CORE) as u32 * PCIE_CORE_STRIDE
        + (port % PCIE_PORTS_PER_CORE) as u32 * 0x1000
}
pub(super) const fn pcie_port_reg(port: usize, offset: u32) -> u32 {
    0x1114_0000 + port_offset(port) + offset
}
/// The config space of the PCIe root ports is also accessible through SMN.
pub(super) const fn pcie_port_cfg(port: usize, offset: u32) -> u32 {
    0x1110_0000 + port_offset(port) + offset
}
/// Indirect PCIe strap access (SMN) of a core.
pub(super) fn strap_write(smn: &mut impl Smn, core: usize, index: u32, value: u32) {
    smn.write(0x0004_a348 + core as u32 * 0x80, index);
    smn.write(0x0004_a34c + core as u32 * 0x80, value);
}
pub(super) fn strap_read(smn: &mut impl Smn, core: usize, index: u32) -> u32 {
    smn.write(0x0004_a348 + core as u32 * 0x80, index);
    smn.read(0x0004_a34c + core as u32 * 0x80)
}

/// NBIF (SMN): three instances 0x101xxxxx, 0x102xxxxx, 0x103xxxxx.
pub(super) const NBIF_COUNT: u32 = 3;
pub(super) const fn nbif_reg(n: u32, offset: u32) -> u32 {
    0x1010_0000 + n * 0x10_0000 + offset
}
/// Bits 31:24: root bus of the die.
const NBIF_BUS_CFG: u32 = 0x31404;
const NBIF_REV_STRAP: u32 = 0x1013_460c;
const NBIF_PARITY_CTRL: u32 = 0x39004;
const NBIF_HWINIT_LOCK: u32 = 0x2358c;

/// AmdNbioBaseZPPei for one die: the register tables, bus number, IOAPIC
/// features, parity/poison handling and IOMMU RAS.
pub fn early_init<P: ConfigRegionAccess + Copy>(die: &Die<P>, revision: Revision) {
    die_init(&mut die.smn(), die.index(), die.bus(), revision);
}

fn die_init(smn: &mut impl Smn, die: u8, bus: u8, revision: Revision) {
    for n in 0..NBIF_COUNT {
        smn.update(nbif_reg(n, NBIF_BUS_CFG), 0xff00_0000, u32::from(bus) << 24);
    }

    apply(smn, BASE);
    apply(smn, IOMMU);

    // ZP A0/B0/B1 only: strap bit 20 = vendor PCD 0xb9 (0).
    if matches!(revision, Revision::A0 | Revision::B0 | Revision::B1) {
        smn.update(NBIF_REV_STRAP, 1 << 20, 0);
    }

    smn.update(
        IOAPIC_FEATURES_ENABLE,
        0,
        if die != 0 { 0x34 } else { 0x14 },
    );
    smn.write(
        NB_BUS_NUM_CNTL,
        (NbBusNumCntl::LAT_MODE::SET + NbBusNumCntl::BUS.val(u32::from(bus))).value,
    );

    // Parity and poison handling
    for n in 0..NBIF_COUNT {
        smn.update(nbif_reg(n, NBIF_PARITY_CTRL), 0, 0x77);
    }
    for address in [0x0141_f800, 0x0151_f800, 0x0151_f804] {
        smn.update(address, 0, 0x77);
    }

    // IOMMU RAS
    for (to, from) in [(0x1570_4330, 0x13f0_0080), (0x1570_401c, 0x0240_001c)] {
        let value = smn.read(from);
        smn.write(to, value);
    }
    smn.update(0x0400_0200, 0, 1 << 16);
}

/// IOHC TOM2/MMIO base registers for a TOP_MEM2 (0: disabled): (lower,
/// upper, MMIO base).
fn top_of_dram2(lower: u32, upper: u32, top_mem2: u64) -> (u32, u32, u32) {
    if top_mem2 > 1 << 40 {
        // TOM2 stops at the HyperTransport hole, the rest is above TOM3.
        (
            lower | 0xff80_0001,
            upper | 0x1ff,
            (((top_mem2 - 1) >> 16) + 1) as u32,
        )
    } else if top_mem2 != 0 {
        (
            (lower & 0x007f_fffe) | (top_mem2 as u32 & 0xff80_0000) | 1,
            (upper & !0x1ff) | ((top_mem2 >> 32) as u32 & 0xff),
            0x0100_0000,
        )
    } else {
        (lower, upper, 0x0100_0000)
    }
}

/// Tell the IOHC of a die about the DRAM map and the MMIO above 4 GiB.
/// `top_mem` is TOP_MEM (MSR C001_001A), `top_mem2` TOP_MEM2 if SYSCFG
/// enables it.
pub fn tom_init<P: ConfigRegionAccess + Copy>(die: &Die<P>, top_mem: u32, top_mem2: Option<u64>) {
    let root = fstart_pci::PciAddress::new(0, die.bus(), 0, 0);
    let pci = die.pci();
    // SAFETY: D0F0 of the die's root bus; exclusivity is the `Die` contract.
    unsafe {
        let tom = pci.read(root, NB_TOP_OF_DRAM_SLOT1);
        pci.write(
            root,
            NB_TOP_OF_DRAM_SLOT1,
            (tom & 0x007f_ffff) | (top_mem & 0xff80_0000),
        );
    }
    let smn = &mut die.smn();
    iohc_tom(smn, top_mem2.unwrap_or(0));
    fstart_log::debug!(
        "NBIO {}: TOM {} TOM2 {}",
        die.index(),
        fstart_log::Hex(top_mem.into()),
        fstart_log::Hex(top_mem2.unwrap_or(0))
    );
}

fn iohc_tom(smn: &mut impl Smn, top_mem2: u64) {
    let lower = smn.read(NB_LOWER_TOP_OF_DRAM2);
    let upper = smn.read(NB_UPPER_TOP_OF_DRAM2);
    let (lower, upper, mmio_base) = top_of_dram2(lower, upper, top_mem2);
    smn.write(NB_LOWER_TOP_OF_DRAM2, lower);
    smn.write(NB_UPPER_TOP_OF_DRAM2, upper);
    smn.write(NB_TOP_OF_DRAM3, 0x83ff_ffff);
    smn.write(NB_MMIOBASE, mmio_base);
    smn.write(NB_MMIOLIMIT, 0x7fff_ffff);
    smn.update(NB_PCI_CTRL, 0, NbPciCtrl::MMIO_ENABLE::SET.value);
}

/// Decode the NBIO IOAPIC of a die 1 MiB below the top of its non-PCI MMIO
/// pool (0xfeb00000 on die 0) and return its address. The vendor firmware
/// sets its ID to 0x81 + die; the ACPI/MADT code assigns the IDs.
pub fn ioapic_init<P: ConfigRegionAccess + Copy>(die: &Die<P>, nonpci_base: u32) -> u32 {
    let base = nonpci_base + NONPCI_IOAPIC;
    let smn = &mut die.smn();
    smn.write(IOAPIC_BASE_ADDR_HI, 0);
    smn.write(IOAPIC_BASE_ADDR_LO, base);
    smn.write(IOAPIC_BASE_ADDR_LO, base | IohcMmioBar::ENABLE::SET.value);
    base
}

/// AmdNbioBaseZPDxe/AmdNbioPcieZPDxe before PCI enumeration: PCIe core clock
/// gating, then lock the NBIF hardware init registers (straps, IDs).
pub fn pre_enumeration_init<P: ConfigRegionAccess + Copy>(die: &Die<P>) {
    let smn = &mut die.smn();
    // The per-die wrapper features of the vendor topology template enable
    // ClkGating but not LclkGating.
    for core in 0..PCIE_CORE_COUNT {
        smn.update(pcie_core_reg(core, pcie::CONFIG_CNTL), 0, 0xf);
        smn.update(pcie_core_reg(core, pcie::CPM_CONTROL), 0x3c00, 0x81e6);
        smn.update(pcie_core_reg(core, pcie::CNTL_18C), 0, 0x60);
    }
    for n in 0..NBIF_COUNT {
        smn.update(nbif_reg(n, NBIF_HWINIT_LOCK), 0, 1);
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    extern crate std;
    use std::collections::BTreeMap;

    #[derive(Default)]
    pub struct Registers(pub BTreeMap<u32, u32>);
    impl Smn for Registers {
        fn read(&mut self, address: u32) -> u32 {
            self.0.get(&address).copied().unwrap_or(0)
        }
        fn write(&mut self, address: u32, value: u32) {
            self.0.insert(address, value);
        }
    }

    #[test]
    fn tables_copy_write_and_update_in_order() {
        let mut smn = Registers::default();
        smn.0.insert(0x10, 0xf0f0);
        smn.0.insert(0x20, 0x1234);
        apply(
            &mut smn,
            &[
                rmw(0x10, 0xff, 0x01),
                copy(0x14, 0x10),
                write(0x10, 0x5),
                copy(0x18, 0x20),
            ],
        );
        assert_eq!(smn.0[&0x10], 5);
        assert_eq!(smn.0[&0x14], 0xf001);
        assert_eq!(smn.0[&0x18], 0x1234);
    }

    #[test]
    fn die_init_programs_bus_and_revision_strap() {
        let mut smn = Registers::default();
        smn.0.insert(NBIF_REV_STRAP, 1 << 20);
        smn.0.insert(nbif_reg(2, NBIF_BUS_CFG), 0x1200_0034);
        die_init(&mut smn, 1, 0x40, Revision::B1);
        assert_eq!(smn.0[&NBIF_REV_STRAP], 0);
        assert_eq!(smn.0[&nbif_reg(2, NBIF_BUS_CFG)], 0x4000_0034);
        assert_eq!(smn.0[&NB_BUS_NUM_CNTL], 0x140);
        assert_eq!(smn.0[&IOAPIC_FEATURES_ENABLE], 0x34);

        let mut smn = Registers::default();
        smn.0.insert(NBIF_REV_STRAP, 1 << 20);
        die_init(&mut smn, 0, 0, Revision::B2);
        assert_eq!(smn.0[&NBIF_REV_STRAP], 1 << 20);
        assert_eq!(smn.0[&IOAPIC_FEATURES_ENABLE], 0x14);
    }

    #[test]
    fn tom2_below_and_above_the_hypertransport_hole() {
        // 32 GiB: TOM2 in the IOHC, MMIO from 16 MiB up.
        assert_eq!(
            top_of_dram2(0x0000_0002, 0x8000_0000, 0x8_0000_0000),
            (0x0000_0003, 0x8000_0008, 0x0100_0000)
        );
        // TOM2 above 1 TiB: the IOHC stops at the hole, MMIO base above TOM2.
        assert_eq!(
            top_of_dram2(0, 0, 0x101_0000_0000),
            (0xff80_0001, 0x1ff, 0x0101_0000)
        );
        assert_eq!(top_of_dram2(7, 9, 0), (7, 9, 0x0100_0000));
        let mut smn = Registers::default();
        iohc_tom(&mut smn, 0);
        assert_eq!(smn.0[&NB_PCI_CTRL], 1 << 23);
        assert_eq!(smn.0[&NB_MMIOBASE], 0x0100_0000);
    }
}
