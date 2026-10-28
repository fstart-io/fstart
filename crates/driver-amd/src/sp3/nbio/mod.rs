//! NBIO: IOHC, NBIF, IOAPIC, IOMMU and the per-generation register tables
//! (NBIO_SMU).
//!
//! An [`Nbio`] is one ZP die's NBIO (Naples, up to 8) or one NBIO quadrant
//! of an IOD (Rome, Milan: 4 per socket, up to 8). Every NBIO is reached
//! through the SMN pair of its own root bus; on Rome and Milan quadrant `n`
//! adds `n << 20` to the IOHC, IOAPIC, IOMMU, NBIF, IOAGR and SST addresses
//! (NBIO_SMU §3.1). Milan additionally skips every SMN access to a fused-off
//! aperture ([`Fuses`]).
//!
//! Shared IP lives in [`iohc`], [`nbif`], [`ioapic`], [`iommu`] and
//! [`table`]; the fixed per-generation flows in [`naples`] and [`sp3`]
//! (Rome and Milan).

use core::marker::PhantomData;

use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::RegisterLongName;
use tock_registers::interfaces::{Readable, Writeable};

use crate::smn::{Smn, SmnAddr, SmnRegister};
use crate::sp3::soc::Generation;
use crate::sp3::topology::RootBridge;

pub mod fuse;
pub mod ioapic;
pub mod iohc;
pub mod iommu;
pub mod nbif;
pub mod table;

#[cfg(feature = "naples")]
pub mod naples;
#[cfg(any(feature = "rome", feature = "milan"))]
pub mod sp3;

pub use fuse::Fuses;
use table::Knob;

/// One NBIO: a ZP die (Naples) or an IOD quadrant (Rome, Milan).
#[derive(Clone)]
pub struct Nbio<P> {
    pci: P,
    smn: Smn<P>,
    fuses: Fuses,
    pub generation: Generation,
    pub socket: u8,
    /// Die (Naples) or quadrant (Rome, Milan) within the socket.
    pub index: u8,
    /// The NBIO with the FCH (SB_LOCATION source, IOAPIC FCH bit).
    pub fch: bool,
}

impl<P: ConfigRegionAccess + Clone> Nbio<P> {
    /// The NBIO behind root bridge `rb`. Milan passes the socket's fuse
    /// words ([`Fuses::read`]); Naples and Rome pass [`Fuses::NONE`].
    ///
    /// # Safety
    /// `rb` is a live root bridge of `generation`, its bus is routed, and
    /// the caller owns the SMN index/data pair of that bus while this value
    /// is used.
    pub unsafe fn new(pci: P, generation: Generation, rb: &RootBridge, fuses: Fuses) -> Self {
        Self {
            // SAFETY: the constructor's contract.
            smn: unsafe { Smn::on_bus(pci.clone(), rb.bus_base) },
            pci,
            fuses,
            generation,
            socket: rb.socket,
            index: rb.index,
            fch: rb.primary,
        }
    }

    /// The root bus.
    pub fn bus(&self) -> u8 {
        self.smn.bus()
    }

    /// SMN access through this NBIO's root bus, unfiltered (SMU mailbox).
    pub fn smn(&self) -> Smn<P> {
        self.smn.clone()
    }

    /// The config-space accessor.
    pub fn pci(&self) -> P {
        self.pci.clone()
    }

    /// A function on this NBIO's root bus.
    pub fn function(&self, device: u8, function: u8) -> PciAddress {
        PciAddress::new(0, self.bus(), device, function)
    }

    /// The quadrant offset: `index << 20` on Rome/Milan, 0 on Naples (each
    /// die has its own SMN at the same addresses).
    pub fn offset(&self) -> u32 {
        match self.generation {
            Generation::Naples => 0,
            Generation::Rome | Generation::Milan => (self.index as u32) << 20,
        }
    }

    /// `addr` (an NBIO0 / die address) moved to this NBIO.
    pub fn at(&self, addr: SmnAddr) -> SmnAddr {
        addr + self.offset()
    }

    /// The fuse words this handle filters with.
    pub fn fuses(&self) -> Fuses {
        self.fuses
    }

    /// A typed register at the absolute SMN `addr`; reads return 0 and
    /// writes are dropped when the aperture is fused off.
    pub fn reg_at<R: RegisterLongName>(&self, addr: SmnAddr) -> NbioReg<'_, P, R> {
        NbioReg {
            // Filtered like AGESA.
            inner: self
                .fuses
                .allows(addr)
                .then(|| self.smn.reg(crate::smn::SmnReg::new(addr))),
            _reg: PhantomData,
        }
    }

    /// Read `addr`, `None` when fused off.
    pub fn read32(&self, addr: SmnAddr) -> Option<u32> {
        self.fuses.allows(addr).then(|| self.smn.read32(addr))
    }

    /// Write `addr` unless fused off.
    pub fn write32(&self, addr: SmnAddr, value: u32) {
        if self.fuses.allows(addr) {
            self.smn.write32(addr, value)
        }
    }

    /// `(old & !clear) | set` unless fused off.
    pub fn modify32(&self, addr: SmnAddr, clear: u32, set: u32) {
        if self.fuses.allows(addr) {
            self.smn.modify32(addr, clear, set)
        }
    }
}

/// The registers of this NBIO: the shared IP block definitions (IOHC0 /
/// die addresses) moved to its quadrant and fuse filtered.
impl<P: ConfigRegionAccess + Clone> crate::smn::SmnAccess for Nbio<P> {
    type Reg<'a, R: RegisterLongName>
        = NbioReg<'a, P, R>
    where
        Self: 'a;
    fn reg<R: RegisterLongName>(&self, reg: crate::smn::SmnReg<R>) -> Self::Reg<'_, R> {
        self.reg_at(self.at(reg.address()))
    }
}

/// An SMN register of an [`Nbio`], fuse filtered.
pub struct NbioReg<'a, P: ConfigRegionAccess, R: RegisterLongName> {
    inner: Option<SmnRegister<'a, P, R>>,
    _reg: PhantomData<R>,
}

impl<P: ConfigRegionAccess, R: RegisterLongName> Readable for NbioReg<'_, P, R> {
    type T = u32;
    type R = R;

    fn get(&self) -> u32 {
        self.inner.as_ref().map_or(0, Readable::get)
    }
}

impl<P: ConfigRegionAccess, R: RegisterLongName> Writeable for NbioReg<'_, P, R> {
    type T = u32;
    type R = R;

    fn set(&self, value: u32) {
        if let Some(r) = &self.inner {
            r.set(value)
        }
    }
}

/// Milan `IOHC_FCTL` P2P mode (PCD 0x194; Oxide uses drop-non-matching,
/// which AGESA does not offer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum P2pMode {
    ForwardNonMatching = 1,
    ForwardAll = 2,
}

/// NBIO policy. `Default` is the donor SP3 PCD defaults (NBIO_SMU §6); the
/// field docs give the Naples / Rome / Milan PCD tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NbioPolicy {
    /// 0x88 / 0xFD / 0xA0.
    pub iohc_clock_gating: bool,
    /// 0x8B / 0x102 / 0x8E.
    pub sst_clock_gating: bool,
    /// – / 0x103 / 0x85.
    pub nbif_clock_gating: bool,
    /// – / 0x104 / 0x8F.
    pub syshub_clock_gating: bool,
    /// 0x86 / 0xF9 / 0xA1.
    pub iommu_l1_clock_gating: bool,
    /// 0x87 / 0xFA / 0xA2.
    pub iommu_l2_clock_gating: bool,
    /// – / 0xFB / 0x137.
    pub iommu_l2_tw_bit16: bool,
    /// 0x02 / 0x32 / 0x1B0.
    pub iommu: bool,
    /// IOMMU EFR shadow XTSup/GASup (D0F2x80 |= 0x84). `None` keeps the
    /// donor default: off on Rome (PCD 0xE = 0), on on Milan (PCD 0x2C =
    /// 0xFF).
    pub iommu_xt_ga: Option<bool>,
    /// – / 0x106 / 0xB5: PTDMA functions (NBIF0/1 F2 of every NBIO).
    pub ptdma: bool,
    /// – / 0x105 / 0x1AD: HD audio (NBIF1 F4 of NBIO2 per the table
    /// addresses; the report table says NBIO3).
    pub hd_audio: bool,
    pub naples: NaplesPolicy,
    pub milan: MilanPolicy,
}

impl Default for NbioPolicy {
    fn default() -> Self {
        Self {
            iohc_clock_gating: true,
            sst_clock_gating: true,
            nbif_clock_gating: true,
            syshub_clock_gating: true,
            iommu_l1_clock_gating: true,
            iommu_l2_clock_gating: true,
            iommu_l2_tw_bit16: true,
            iommu: true,
            iommu_xt_ga: None,
            ptdma: true,
            hd_audio: false,
            naples: NaplesPolicy::default(),
            milan: MilanPolicy::default(),
        }
    }
}

/// Naples-only NBIO policy (NBIO_SMU §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NaplesPolicy {
    /// PCD 0x14: RAS/poison (property 0x40000000). 0 on SP3.
    pub ras_poison: bool,
    /// PCD 0x15: NBIF parity workaround.
    pub nbif_parity_workaround: bool,
    /// PCD 0x8E: SMN `0x04000200` bit 16.
    pub smn_04000200_bit16: bool,
    /// PCD 0x8F: NBIF0 `0x1013460C` bit 20 on ZP-A0..B1.
    pub nbif0_460c_bit20: bool,
}

impl Default for NaplesPolicy {
    fn default() -> Self {
        Self {
            ras_poison: false,
            nbif_parity_workaround: true,
            smn_04000200_bit16: true,
            nbif0_460c_bit20: false,
        }
    }
}

/// Milan-only NBIO policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MilanPolicy {
    /// PCD 0x194.
    pub p2p: P2pMode,
    /// PCD 0x110.
    pub pcie_core_feature_a: bool,
    /// PCD 0x111.
    pub pcie_core_feature_b: bool,
    /// PCD 0x136: TPH straps on NBIF functions.
    pub nbif_tph: bool,
    /// PCD 0x70 (meaning not named; gates SMN `0x04000200` bit 16, the
    /// second NBIF2 HW-init lock and SMU NBIF2 power gating).
    pub pcd_0x70: bool,
    /// A PCD pair (0x7D..0x80, 0x79..0x7C) is (1, 1): NBIF2 dev0 keeps no
    /// bus number (INF: NTB).
    pub nbif2_ntb: bool,
    /// PCD 0xAD: NBIF `0x3A21C`[9:2] when non-zero.
    pub nbif_mgcg: Option<u8>,
    /// PCD 0x135: NBIF ALT `0x01410020/01810020/01403BA8/01803BA8`[9:2].
    pub nbif_alt_mgcg: Option<u8>,
    /// PCD 0x1C2: clear the IOMMU SNP-style EFR bits.
    pub iommu_snp_clear: bool,
    /// PCD 0x66: D0F2x80[23:21] (GAMSup, INF).
    pub iommu_gam: u8,
    /// PCD 0xAE: `L2_CP_CONTROL_1` bit 2 (AGESA derives it from the OPN
    /// fuse word; open item).
    pub iommu_cp_control1_bit2: bool,
    /// PCDs 0x67..0x6E: per socket/NBIO IOMMU enable.
    pub iommu_per_nbio: [[bool; 4]; 2],
}

impl Default for MilanPolicy {
    fn default() -> Self {
        Self {
            p2p: P2pMode::ForwardNonMatching,
            pcie_core_feature_a: true,
            pcie_core_feature_b: true,
            nbif_tph: true,
            pcd_0x70: false,
            nbif2_ntb: false,
            nbif_mgcg: None,
            nbif_alt_mgcg: None,
            iommu_snp_clear: false,
            iommu_gam: 0,
            iommu_cp_control1_bit2: false,
            iommu_per_nbio: [[true; 4]; 2],
        }
    }
}

impl NbioPolicy {
    /// The value the PCD behind `knob` holds on `generation`.
    pub fn knob(&self, generation: Generation, knob: Knob) -> u32 {
        let b = |v: bool| v as u32;
        match knob {
            Knob::IohcClockGating => b(self.iohc_clock_gating),
            Knob::SstClockGating => b(self.sst_clock_gating),
            Knob::NbifClockGating => b(self.nbif_clock_gating),
            Knob::SyshubClockGating => b(self.syshub_clock_gating),
            Knob::HdAudio => b(self.hd_audio),
            Knob::Ptdma => b(self.ptdma),
            Knob::IommuEnable => b(self.iommu),
            Knob::IommuL1ClockGating => b(self.iommu_l1_clock_gating),
            Knob::IommuL2ClockGating => b(self.iommu_l2_clock_gating),
            Knob::IommuL2TwBit16 => b(self.iommu_l2_tw_bit16),
            Knob::PcieCoreFeatureA => b(self.milan.pcie_core_feature_a),
            Knob::PcieCoreFeatureB => b(self.milan.pcie_core_feature_b),
            Knob::NbifTph => b(self.milan.nbif_tph),
            Knob::IohcP2p => self.milan.p2p as u32,
            // Rome PCD 0xE enables with 1; Milan PCD 0x2C enables with
            // anything but 1 (default 0xFF).
            Knob::IommuXtGa => match (generation, self.iommu_xt_ga) {
                (Generation::Milan, Some(false)) => 1,
                (Generation::Milan, _) => 0xFF,
                (_, Some(true)) => 1,
                _ => 0,
            },
        }
    }

    /// Whether the IOMMU EFR shadow bits are set on `generation`.
    pub fn iommu_xt_ga(&self, generation: Generation) -> bool {
        self.iommu_xt_ga.unwrap_or(generation == Generation::Milan)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::smn::model::Model;

    /// An NBIO of socket 0 on `bus`; index 0 has the FCH.
    pub fn nbio(generation: Generation, index: u8, bus: u8) -> (Model, Nbio<Model>) {
        let model = Model::default();
        let rb = RootBridge {
            socket: 0,
            index,
            fabric_id: 0,
            bus_base: bus,
            bus_limit: bus,
            primary: index == 0,
        };
        // SAFETY: model.
        let n = unsafe { Nbio::new(model.clone(), generation, &rb, Fuses::NONE) };
        (model, n)
    }

    #[test]
    fn quadrant_offsets() {
        let (_, n) = nbio(Generation::Rome, 3, 0);
        assert_eq!(n.at(0x13b1_0044), 0x13e1_0044);
        let (_, z) = nbio(Generation::Naples, 3, 0);
        assert_eq!(z.at(0x13b1_0044), 0x13b1_0044);
    }

    #[test]
    fn xt_ga_knob_per_generation() {
        let p = NbioPolicy::default();
        assert_eq!(p.knob(Generation::Rome, Knob::IommuXtGa), 0);
        assert_eq!(p.knob(Generation::Milan, Knob::IommuXtGa), 0xFF);
        assert!(!p.iommu_xt_ga(Generation::Rome) && p.iommu_xt_ga(Generation::Milan));
    }
}
