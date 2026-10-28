//! The IOHC steps of an SP3 NBIO on the shared IOHC registers
//! ([`crate::nbio::iohc`], IOHC0 / die addresses that [`Nbio`]'s
//! [`SmnAccess`] moves to its quadrant): TOM/TOM2/TOM3, bus number, bus
//! routes and the SMU/PSP/IOAPIC BARs (NBIO_SMU §3.5, PSP_APOB §3.2).

use fstart_pci::{ConfigRegionAccess, ConfigRegister};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use super::Nbio;
use crate::nbio::iohc::{
    BarHigh, CFG_IOHC_PCI, CfgIohcPci, IOAPIC_BAR, IoapicBar, IohcRbBusRoute, NB_BUS_NUM_CNTL,
    NB_TOP_OF_DRAM_SLOT1, NbBusNumCntl, PSP_BAR, PspBar, SMU_BAR, SmuBar, TopStyle,
    iohc_rb_bus_route,
};
use crate::nbio::iohc::{DramTops, PSP_BAR_SIZE, SMU_BAR_SIZE};
use crate::smn::SmnAccess;
use crate::sp3::soc::Generation;
use crate::sp3::topology::RootBridge;

impl<P: ConfigRegionAccess + Clone> Nbio<P> {
    /// Program TOM, TOM2 and TOM3 (and Naples' MMIO base/limit) on this NBIO
    /// (`GnbSetTom{ZP,SSP,GN}`, NBIO_SMU §3.5, ../sp4r2 NBIO_BASE_ZP §3.5).
    /// AGESA does this on every NBIO without an S3 script.
    pub fn set_tom(&self, tops: DramTops) {
        // SAFETY: D0F0x90 of this NBIO's host bridge (constructor contract).
        let slot1 =
            unsafe { ConfigRegister::new(self.pci(), self.function(0, 0), NB_TOP_OF_DRAM_SLOT1) };
        let style = match self.generation {
            Generation::Naples => TopStyle::MmioWindow,
            Generation::Rome | Generation::Milan => TopStyle::Tom3,
        };
        crate::nbio::iohc::set_tom(self, &slot1, tops, style);
    }

    /// `NB_BUS_NUM_CNTL = EN | bus` (NBIO_SMU §3.5 IOAPIC step).
    pub fn set_bus_number(&self) {
        self.reg(NB_BUS_NUM_CNTL)
            .write(NbBusNumCntl::LAT_MODE::SET + NbBusNumCntl::BUS.val(self.bus() as u32));
    }

    /// The bus route table after DXIO (Rome `ffdff8c8` -> `ffdff045`,
    /// Milan and Naples the same): entry `j` is the `j`-th root bridge in
    /// physical order (the GNB handle InstanceId) with its bus range,
    /// REMOTE unless it is this NBIO. The donor writes bits 30:16 from an
    /// uninitialised stack word; they are 0 here.
    pub fn set_bus_routes(&self, root_bridges: &[RootBridge]) {
        root_bridges.iter().enumerate().for_each(|(j, rb)| {
            let remote = (rb.socket, rb.index) != (self.socket, self.index);
            self.reg(iohc_rb_bus_route(j)).write(
                IohcRbBusRoute::BASE.val(rb.bus_base as u32)
                    + IohcRbBusRoute::LIMIT.val(rb.bus_limit as u32)
                    + IohcRbBusRoute::REMOTE.val(remote as u32),
            );
        });
    }

    /// Hide the IOMMU (D0F2) when the policy disables it.
    pub fn disable_iommu(&self) {
        self.reg(CFG_IOHC_PCI)
            .modify(CfgIohcPci::D0F2_ENABLE::CLEAR);
    }

    /// The SMU BAR, if enabled.
    pub fn smu_bar(&self) -> Option<u64> {
        SMU_BAR.base(self)
    }

    /// Place the SMU BAR at `base` (1 MiB aligned, from the NBIO's non-PCI
    /// MMIO window) unless the BAR already holds an address; then enable and
    /// lock it. Returns whether it was written. Rome/Milan do this on every
    /// NBIO; Naples never does (NBIO_SMU §3.5, `nbio.smu_bar`).
    pub fn set_smu_bar(&self, base: u64) -> bool {
        debug_assert_eq!(base % SMU_BAR_SIZE, 0);
        let lo = self.reg(SMU_BAR.low);
        let hi = self.reg(SMU_BAR.high);
        if lo.get() != 0 || hi.get() != 0 {
            return false;
        }
        hi.write(BarHigh::ADDRESS.val((base >> 32) as u32));
        lo.write(SmuBar::BASE.val((base >> 20) as u32 & 0xfff));
        lo.modify(SmuBar::ENABLE::SET);
        lo.modify(SmuBar::LOCK::SET);
        true
    }

    /// The PSP BAR, if enabled.
    pub fn psp_bar(&self) -> Option<u64> {
        PSP_BAR.base(self)
    }

    /// Place the PSP BAR (1 MiB) at `base` with enable and lock, unless it
    /// is already enabled. Rome/Milan: on IOHC0 of each socket; Naples: on
    /// each die (die 0 of socket 0 in PEI, PSP_APOB §3.2). Returns whether
    /// it was written. The caller also loads `PSP_ADDR` (MSR C001_10A2) on
    /// every thread of the socket/die.
    pub fn set_psp_bar(&self, base: u64) -> bool {
        debug_assert_eq!(base % PSP_BAR_SIZE, 0);
        let lo = self.reg(PSP_BAR.low);
        if lo.is_set(PspBar::ENABLE) {
            return false;
        }
        self.reg(PSP_BAR.high)
            .write(BarHigh::ADDRESS.val((base >> 32) as u32));
        lo.write(
            PspBar::BASE.val((base >> 20) as u32 & 0xfff) + PspBar::LOCK::SET + PspBar::ENABLE::SET,
        );
        true
    }

    /// The NBIO IOAPIC base, if enabled.
    pub fn ioapic_bar(&self) -> Option<u64> {
        IOAPIC_BAR.base(self)
    }

    /// Place the NBIO IOAPIC at `base` (256-byte aligned, inside the root
    /// bridge's MMIO window): high, low, then enable — the AGESA PCD order
    /// (`gn.ioapic_bar_pcd`).
    pub fn set_ioapic_bar(&self, base: u64) {
        let lo = self.reg(IOAPIC_BAR.low);
        self.reg(IOAPIC_BAR.high)
            .write(BarHigh::ADDRESS.val((base >> 32) as u32));
        lo.write(IoapicBar::ADDRESS.val((base >> 8) as u32 & 0xff_ffff));
        lo.modify(IoapicBar::ENABLE::SET);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nbio::iohc::{
        NB_LOWER_TOP_OF_DRAM2, NB_MMIOBASE, NB_PCI_CTRL, NB_TOP_OF_DRAM3, NB_UPPER_TOP_OF_DRAM2,
    };
    use crate::smn::{SmnAddr, SmnReg};
    use crate::sp3::nbio::tests::nbio;

    fn smn<R: tock_registers::RegisterLongName>(
        model: &crate::smn::model::Model,
        bus: u8,
        reg: SmnReg<R>,
        quadrant: u32,
    ) -> u32 {
        let addr: SmnAddr = reg.address() + (quadrant << 20);
        model.0.borrow().smn.get(&(bus, addr)).copied().unwrap_or(0)
    }

    #[test]
    fn tom_sp3_below_and_above_1t() {
        let (m, n) = nbio(Generation::Milan, 1, 0x20);
        n.set_tom(DramTops {
            tom: 0x8000_0000,
            tom2: Some(0x80_5000_0000),
        });
        assert_eq!(smn(&m, 0x20, NB_LOWER_TOP_OF_DRAM2, 1), 0x5000_0001);
        assert_eq!(smn(&m, 0x20, NB_UPPER_TOP_OF_DRAM2, 1), 0x80);
        assert_eq!(smn(&m, 0x20, NB_TOP_OF_DRAM3, 1), 0);
        assert_eq!(smn(&m, 0x20, NB_PCI_CTRL, 1), 1 << 23);

        let (m, n) = nbio(Generation::Rome, 0, 0);
        m.0.borrow_mut().config.insert((0, 0, 0, 0x90), 0);
        n.set_tom(DramTops {
            tom: 1 << 32,
            tom2: Some(0x200_0000_0000),
        });
        assert_eq!(
            m.0.borrow().config[&(0, 0, 0, 0x90)],
            1,
            "4 GiB: TOM 0, bit 32"
        );
        assert_eq!(smn(&m, 0, NB_LOWER_TOP_OF_DRAM2, 0), 0xFF80_0001);
        assert_eq!(smn(&m, 0, NB_UPPER_TOP_OF_DRAM2, 0), 0x1FF);
        assert_eq!(smn(&m, 0, NB_TOP_OF_DRAM3, 0), 0x8000_0000 | 0x7_FFFF);
    }

    #[test]
    fn bus_routes() {
        // Bench order: IOMS 0..3 own buses c0, 80, 40, 00.
        let rbs: [RootBridge; 4] = core::array::from_fn(|i| RootBridge {
            socket: 0,
            index: i as u8,
            fabric_id: 0x18 + i as u16,
            bus_base: 0xC0 - 0x40 * i as u8,
            bus_limit: 0xFF - 0x40 * i as u8,
            primary: i == 3,
        });
        let (m, n) = nbio(Generation::Rome, 1, 0x80);
        n.set_bus_routes(&rbs);
        assert_eq!(smn(&m, 0x80, iohc_rb_bus_route(0), 1), 1 << 31 | 0xFFC0);
        assert_eq!(smn(&m, 0x80, iohc_rb_bus_route(1), 1), 0xBF80, "own entry");
        assert_eq!(smn(&m, 0x80, iohc_rb_bus_route(3), 1), 1 << 31 | 0x3F00);
    }

    #[test]
    fn tom_naples() {
        let (m, n) = nbio(Generation::Naples, 2, 0x30);
        m.0.borrow_mut().config.insert((0x30, 0, 0, 0x90), 0);
        n.set_tom(DramTops {
            tom: 0xC000_0000,
            tom2: Some(0x2_4000_0000),
        });
        assert_eq!(m.0.borrow().config[&(0x30, 0, 0, 0x90)], 0xC000_0000);
        assert_eq!(smn(&m, 0x30, NB_TOP_OF_DRAM3, 0), 0x83FF_FFFF);
        assert_eq!(smn(&m, 0x30, NB_MMIOBASE, 0), 0x0100_0000);
        assert_eq!(smn(&m, 0x30, NB_UPPER_TOP_OF_DRAM2, 0), 2);
    }

    #[test]
    fn bars() {
        let (m, n) = nbio(Generation::Rome, 2, 0x40);
        assert!(n.set_smu_bar(0x1_2340_0000));
        assert!(!n.set_smu_bar(0x1_2350_0000), "kept when set");
        assert_eq!(smn(&m, 0x40, SMU_BAR.low, 2), 0x2340_0003);
        assert_eq!(n.smu_bar(), Some(0x1_2340_0000));
        assert!(n.set_psp_bar(0xF100_0000));
        assert_eq!(smn(&m, 0x40, PSP_BAR.low, 2), 0xF100_0101);
        n.set_ioapic_bar(0xC000_1000);
        assert_eq!(n.ioapic_bar(), Some(0xC000_1000));
    }
}
