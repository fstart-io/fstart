// SPDX-License-Identifier: MIT OR Apache-2.0
//! The WH NBIO RAS steps beyond the vendor tables ([`super::init::RootBridge::ras_init`]),
//! from Dell PR9XJ AmdNbioDxe (DEVICES.md §2.12, [D]):
//!
//! - `NbioPcieAerReportingPathExt` (`0x88c0`, from
//!   `PcieAerReportingPathExtCallBack 0xf2b0`) in
//!   [`RootBridge::aer_reporting_path`], the routing of each PCIe port's
//!   AER interrupt (`0x13B2_0114..0x012C`) for the report mechanism SMU
//!   message 9 then announces;
//! - `NbioRASControl`'s `0x133a0` in [`RootBridge::egress_poison`], the
//!   egress poison masks and severities (`0x13B2_0828..0x0834`);
//! - in [`RootBridge::sync_flood_from_pin`] the IOHC's sync-flood status
//!   (`0x13B2_0800` bit 2, "sync flood from pin"), a previous boot's error
//!   that `ras_init` clears, for the BERT (GIGABYTE AmdPlatformRasWhDxe
//!   `0x9888` [D]).
//! - the IOHC's RAS SMI of firmware-first error handling
//!   (FIRMWARE_FIRST.md §2.1, §2.2): [`RootBridge::enable_ras_smi`] as
//!   AGESA's `FchSocSmmApuRasSmiEnable` (Dell FchSmmDispatcher `0x10bc0`,
//!   GIGABYTE `0xa0ac` [D]), the status an SMI handler reads
//!   ([`RootBridge::ras_status`], `RAS_GLOBAL_STATUS_LO/HI`), the PCIe
//!   bits it clears and the re-arm after the FCH's status
//!   ([`RootBridge::rearm_ras_smi`]), and the CXL event of `SW_SMI_CNTL`.

use tock_registers::LocalRegisterCopy;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

use super::RootBridge;
use crate::smn::{SmnAccess, SmnReg};
use fstart_pci::ConfigRegionAccess;

register_bitfields![u32,
    /// IOHC `0x13B2_0800` (32-bit, W1C): sync-flood status.
    pub SyncFloodStatus [
        /// A sync flood came in on the pin.
        FROM_PIN OFFSET(2) NUMBITS(1) []
    ],
    /// IOHC `0x13B2_0114 + off + port * 0x20` (32-bit): the port's AER
    /// interrupt routing, bits 4:0.
    pub AerRoute114 [
        BIT0 OFFSET(0) NUMBITS(1) [],
        /// The redirect: 3 with `PcdAmdPcieAndCxlIntRedir`, else 0.
        REDIRECT OFFSET(1) NUMBITS(2) [],
        BITS34 OFFSET(3) NUMBITS(2) []
    ],
    /// `+0x120..0x12C`: bits 0, 3, 4 (sync flood on fatal, `0x19`) and
    /// bits 2:1 (the redirect).
    pub AerRoute [
        SYNC_FLOOD_0 OFFSET(0) NUMBITS(1) [],
        REDIRECT OFFSET(1) NUMBITS(2) [],
        SYNC_FLOOD_34 OFFSET(3) NUMBITS(2) []
    ],
    /// IOHC `0x13B2_0020` (32-bit): RAS_GLOBAL_STATUS_LO.
    pub RasGlobalStatusLo [
        /// A CXL component event (`SW_SMI_CNTL`, AMD CPM's
        /// `Cxl_1p1_2p0_CmpErrScan`).
        CXL OFFSET(9) NUMBITS(1) []
    ],
    /// IOHC `0x13B2_0024` (32-bit, W1C): RAS_GLOBAL_STATUS_HI.
    pub RasGlobalStatusHi [
        /// The PCIe cores' errors (the vendor scans every root port of the
        /// root bridge when any is set).
        PCIE OFFSET(0) NUMBITS(22) []
    ],
    /// IOHC `0x13B1_0120` (32-bit): the IOHC's SMI to the FCH.
    pub IohcSmi [
        /// Set to enable and, after each SMI, to re-arm it.
        ENABLE OFFSET(0) NUMBITS(1) []
    ],
    /// IOHC `0x13B1_010C` (32-bit): SW_SMI_CNTL; written back as read to
    /// end the event ("invert bit values & prevent SMI re-entrance").
    pub SwSmiCntl [
        /// MPIO firmware interrupt: a CXL event.
        CXL_EVENT OFFSET(1) NUMBITS(1) []
    ]
];

/// IOHC RAS status and SMI control.
pub const RAS_GLOBAL_STATUS_LO: SmnReg<RasGlobalStatusLo::Register> = SmnReg::new(0x13b2_0020);
pub const RAS_GLOBAL_STATUS_HI: SmnReg<RasGlobalStatusHi::Register> = SmnReg::new(0x13b2_0024);
pub const IOHC_SMI: SmnReg<IohcSmi::Register> = SmnReg::new(0x13b1_0120);
pub const SW_SMI_CNTL: SmnReg<SwSmiCntl::Register> = SmnReg::new(0x13b1_010c);

/// What an IOHC's RAS status says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasStatus {
    pub lo: u32,
    pub hi: u32,
}

impl RasStatus {
    /// The PCIe cores' errors.
    pub fn pcie(&self) -> u32 {
        LocalRegisterCopy::<u32, RasGlobalStatusHi::Register>::new(self.hi)
            .read(RasGlobalStatusHi::PCIE)
    }

    /// A CXL component event.
    pub fn cxl(&self) -> bool {
        LocalRegisterCopy::<u32, RasGlobalStatusLo::Register>::new(self.lo)
            .is_set(RasGlobalStatusLo::CXL)
    }

    /// Nothing to handle (an absent IOHC reads all ones: nothing either).
    pub fn idle(&self) -> bool {
        (self.pcie() == 0 && !self.cxl()) || (self.lo == u32::MAX && self.hi == u32::MAX)
    }
}

/// IOHC `0x13B2_0800`.
pub const SYNC_FLOOD_STATUS: SmnReg<SyncFloodStatus::Register> = SmnReg::new(0x13b2_0800);
/// IOHC egress poison mask low/high and severity low/high.
pub const EGRESS_POISON_MASK: [SmnReg; 2] = [SmnReg::new(0x13b2_0828), SmnReg::new(0x13b2_082c)];
pub const EGRESS_POISON_SEVERITY: [SmnReg; 2] =
    [SmnReg::new(0x13b2_0830), SmnReg::new(0x13b2_0834)];

/// The first port's AER routing registers on IOHC instance 0; port `p`
/// adds `p * 0x20`, the bonus core [`aer_core_offset`].
pub const AER_ROUTE_114: SmnReg<AerRoute114::Register> = SmnReg::new(0x13b2_0114);
pub const AER_ROUTE_120: SmnReg<AerRoute::Register> = SmnReg::new(0x13b2_0120);
pub const AER_ROUTE_124: SmnReg<AerRoute::Register> = SmnReg::new(0x13b2_0124);
pub const AER_ROUTE_128: SmnReg<AerRoute::Register> = SmnReg::new(0x13b2_0128);
pub const AER_ROUTE_12C: SmnReg<AerRoute::Register> = SmnReg::new(0x13b2_012c);

/// The offset of a core's ports' AER routing registers in its IOHC: the
/// bonus core (6) `0x100`, the others 0; on SP7 A0 the bonus core `0x200`,
/// core 0 0, the others `0x100`.
pub const fn aer_core_offset(core: u8, a0: bool) -> u32 {
    match (a0, core) {
        (false, 6) => 0x100,
        (false, _) => 0,
        (true, 6) => 0x200,
        (true, 0) => 0,
        (true, _) => 0x100,
    }
}

/// How a port's AER errors are routed (`PcdAmdPcieAerReportMechanism`,
/// `PcdAmdPcieAndCxlIntRedir`, `PcdAmdPcieSyncFloodOnFatal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AerPath {
    /// [`super::init::aer_mechanism`]: 1 OS-first, 2 firmware-first.
    pub mechanism: u8,
    /// `PcdAmdPcieAndCxlIntRedir` (Dell 0): redirect 3.
    pub int_redirect: bool,
    /// `PcdAmdPcieSyncFloodOnFatal` (Dell 0): a fatal error floods.
    pub sync_flood_on_fatal: bool,
}

impl AerPath {
    /// Dell's build values with `mechanism`.
    pub const fn dell(mechanism: u8) -> Self {
        Self {
            mechanism,
            int_redirect: false,
            sync_flood_on_fatal: false,
        }
    }
}

/// The egress poison inputs of `NbioRASControl` (Dell `0x133a0`; GIGABYTE
/// AmdNbioDxe `0xb470`, the same writes [I]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EgressPoison {
    /// `PcdAmdNbioEgressPoisonMaskLo`/`Hi`; the vendor clears bit 2 of the
    /// low and bits 17:16 of the high word.
    pub mask: (u32, u32),
    /// `PcdEnableEgressPoisonSeverity` (Dell 1) with
    /// `PcdAmdNbioEgressPoisonSeverityLo`/`Hi` (Dell 0/0).
    pub severity: Option<(u32, u32)>,
}

impl EgressPoison {
    /// Dell's: masks `0xFFFF_FFFB`/`0xFFFC_FFFC` once applied, severities 0.
    pub const DELL: Self = Self {
        mask: (0xffff_ffff, 0xffff_fffc),
        severity: Some((0, 0)),
    };

    /// GIGABYTE MD25-CG0 F01's (PCD database, ex `0x4F215`/`0x4F214` =
    /// `0xFFFF_FFFB`/`0xFFFC_FFFE`, CBS overrides not applied at first
    /// boot): masks `0xFFFF_FFFB`/`0xFFFC_FFFE` once applied, and no
    /// severity (`PcdEnableEgressPoisonSeverity` ex `0x4F20F` = 0).
    pub const GIGABYTE: Self = Self {
        mask: (0xffff_fffb, 0xfffc_fffe),
        severity: None,
    };

    /// The low and high mask as written.
    pub const fn masks(&self) -> (u32, u32) {
        (self.mask.0 & !4, self.mask.1 & 0xfffc_ffff)
    }
}

impl<P: ConfigRegionAccess + Clone> RootBridge<P> {
    /// The previous boot's "sync flood from pin" (`0x13B2_0800[2]`), read
    /// before [`super::init::RootBridge::ras_init`] clears it.
    pub fn sync_flood_from_pin(&self) -> bool {
        self.reg(SYNC_FLOOD_STATUS)
            .is_set(SyncFloodStatus::FROM_PIN)
    }

    /// `NbioRASControl`'s egress poison step (`0x133a0`), run with the
    /// `RAS_SYNC_FLOOD` table unless poison consumption is on.
    pub fn egress_poison(&self, poison: &EgressPoison) {
        let (lo, hi) = poison.masks();
        self.reg(EGRESS_POISON_MASK[0]).set(lo);
        self.reg(EGRESS_POISON_MASK[1]).set(hi);
        if let Some((lo, hi)) = poison.severity {
            self.reg(EGRESS_POISON_SEVERITY[0]).set(lo);
            self.reg(EGRESS_POISON_SEVERITY[1]).set(hi);
        }
    }

    /// Enable the IOHC's RAS SMI (`FchSocSmmApuRasSmiEnable`):
    /// `0x13B2_00F4 |= 0x14000`, `0x13B1_0120 |= 1`. The FCH side is
    /// `fch::smi::route_nbio_ras`.
    pub fn enable_ras_smi(&self) {
        use crate::dxio::wh::hotplug::{IOHC_HOTPLUG, IohcHotplug};
        self.reg(IOHC_HOTPLUG)
            .modify(IohcHotplug::HOTPLUG::SET + IohcHotplug::SFI::SET);
        self.reg(IOHC_SMI).modify(IohcSmi::ENABLE::SET);
    }

    /// `RAS_GLOBAL_STATUS_LO` and `_HI`.
    pub fn ras_status(&self) -> RasStatus {
        RasStatus {
            lo: self.reg(RAS_GLOBAL_STATUS_LO).get(),
            hi: self.reg(RAS_GLOBAL_STATUS_HI).get(),
        }
    }

    /// Clear the PCIe bits `pcie` of `RAS_GLOBAL_STATUS_HI` (W1C; the
    /// vendor writes back what it read of bits 21:0).
    pub fn clear_pcie_ras(&self, pcie: u32) {
        self.reg(RAS_GLOBAL_STATUS_HI)
            .write(RasGlobalStatusHi::PCIE.val(pcie));
    }

    /// End a CXL event: `SW_SMI_CNTL` written back as read. Whether it
    /// named one.
    pub fn take_cxl_event(&self) -> bool {
        let control = self.reg(SW_SMI_CNTL);
        let value = control.extract();
        control.set(value.get());
        value.is_set(SwSmiCntl::CXL_EVENT)
    }

    /// Re-arm the IOHC's SMI once the FCH's status is cleared, as the
    /// vendor's dispatcher does after its handlers (`0x13B1_0120 |= 1`).
    pub fn rearm_ras_smi(&self) {
        self.reg(IOHC_SMI).modify(IohcSmi::ENABLE::SET);
    }

    /// `NbioPcieAerReportingPathExt` for local port `port` of PCIe core
    /// `core` of this IOHC (DEVICES.md §2.12): mechanism 1 clears the
    /// routing (`+0x114 &= 0xFFFF_FFE6`) and sets or clears the sync flood
    /// on fatal (`+0x124` bits 0, 3, 4); any other mechanism sets the
    /// redirect `r` (3 with the interrupt redirect, else 0) in `+0x114`
    /// (bits 4:0 = `r << 1`), `+0x124` and `+0x128` (bits 2:1), clears
    /// `+0x12C` bits 2:1 and sets `+0x120` bits 2:1.
    pub fn aer_reporting_path(&self, core: u8, port: u8, a0: bool, path: &AerPath) {
        let offset = aer_core_offset(core, a0) + u32::from(port) * 0x20;
        let at = |reg: SmnReg<AerRoute::Register>| self.reg(reg.offset(offset));
        let flood = AerRoute::SYNC_FLOOD_0.val(path.sync_flood_on_fatal.into())
            + AerRoute::SYNC_FLOOD_34.val(if path.sync_flood_on_fatal { 3 } else { 0 });
        let route114 = self.reg(AER_ROUTE_114.offset(offset));
        if path.mechanism == 1 {
            route114.modify(AerRoute114::BIT0::CLEAR + AerRoute114::BITS34::CLEAR);
            at(AER_ROUTE_124).modify(flood);
            return;
        }
        let r = if path.int_redirect { 3 } else { 0 };
        route114.modify(
            AerRoute114::BIT0::CLEAR + AerRoute114::REDIRECT.val(r) + AerRoute114::BITS34::CLEAR,
        );
        at(AER_ROUTE_124).modify(flood);
        at(AER_ROUTE_124).modify(AerRoute::REDIRECT.val(r));
        at(AER_ROUTE_128).modify(AerRoute::REDIRECT.val(r));
        at(AER_ROUTE_12C).modify(AerRoute::REDIRECT::CLEAR);
        at(AER_ROUTE_120).modify(AerRoute::REDIRECT.val(3));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smn::testing::SmnSpace;

    #[test]
    fn firmware_first_routes_and_os_first_clears() {
        let space = SmnSpace::default();
        // IOHC 1: `+ 1 << 20`; bonus core port 2: `+ 0x100 + 0x40`.
        let rb = unsafe { RootBridge::new(&space, 1, 0) }.unwrap();
        let base: u32 = 0x13b2_0000 + (1 << 20) + 0x100 + 0x40;
        for off in [0x114, 0x120, 0x124, 0x128, 0x12c] {
            space.set(base + off, 0xffff_ffff);
        }
        rb.aer_reporting_path(6, 2, false, &AerPath::dell(2));
        assert_eq!(space.get(base + 0x114), 0xffff_ffe0);
        assert_eq!(space.get(base + 0x124), 0xffff_ffe0);
        assert_eq!(space.get(base + 0x128), 0xffff_fff9);
        assert_eq!(space.get(base + 0x12c), 0xffff_fff9);
        assert_eq!(space.get(base + 0x120), 0xffff_ffff);
        let os: u32 = 0x13b2_0000 + (1 << 20) + 0x20;
        space.set(os + 0x114, 0xffff_ffff);
        rb.aer_reporting_path(
            1,
            1,
            false,
            &AerPath {
                mechanism: 1,
                int_redirect: false,
                sync_flood_on_fatal: true,
            },
        );
        assert_eq!(space.get(os + 0x114), 0xffff_ffe6);
        assert_eq!(space.get(os + 0x124), 0x19);
        assert_eq!(aer_core_offset(2, true), 0x100);
    }

    /// The vendors' IOHC RAS SMI (both FchSmmDispatcher tables: IOHC n at
    /// `+ n << 20`): enable, status, the PCIe bits cleared, the re-arm.
    #[test]
    fn the_iohc_ras_smi() {
        let space = SmnSpace::default();
        let rb = unsafe { RootBridge::new(&space, 2, 0) }.unwrap();
        let base: u32 = 2 << 20;
        space.set(0x13b2_00f4 + base, 0x1);
        rb.enable_ras_smi();
        assert_eq!(space.get(0x13b2_00f4 + base), 0x14001);
        assert_eq!(space.get(0x13b1_0120 + base), 1);
        space.set(0x13b2_0020 + base, 0x200);
        space.set(0x13b2_0024 + base, 0xffc0_0005);
        let status = rb.ras_status();
        assert_eq!(
            (status.pcie(), status.cxl(), status.idle()),
            (5, true, false)
        );
        rb.clear_pcie_ras(status.pcie());
        assert_eq!(space.get(0x13b2_0024 + base), 5);
        space.set(0x13b1_010c + base, 2);
        assert!(rb.take_cxl_event());
        space.set(0x13b1_0120 + base, 0);
        rb.rearm_ras_smi();
        assert_eq!(space.get(0x13b1_0120 + base), 1);
        assert!(
            RasStatus {
                lo: 0,
                hi: 0xffc0_0000
            }
            .idle()
        );
        assert!(RasStatus { lo: !0, hi: !0 }.idle());
    }

    #[test]
    fn egress_poison_and_sync_flood_status() {
        let space = SmnSpace::default();
        let rb = unsafe { RootBridge::new(&space, 0, 0) }.unwrap();
        rb.egress_poison(&EgressPoison::DELL);
        assert_eq!(space.get(0x13b2_0828u32), 0xffff_fffb);
        assert_eq!(space.get(0x13b2_082cu32), 0xfffc_fffc);
        assert_eq!(space.get(0x13b2_0830u32), 0);
        // GIGABYTE's: its own high mask, no severity write.
        space.set(0x13b2_0830u32, 0x55);
        rb.egress_poison(&EgressPoison::GIGABYTE);
        assert_eq!(space.get(0x13b2_0828u32), 0xffff_fffb);
        assert_eq!(space.get(0x13b2_082cu32), 0xfffc_fffe);
        assert_eq!(space.get(0x13b2_0830u32), 0x55);
        assert!(!rb.sync_flood_from_pin());
        space.set(0x13b2_0800u32, 4);
        assert!(rb.sync_flood_from_pin());
    }
}
