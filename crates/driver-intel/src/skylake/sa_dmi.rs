// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native **DMI-only** cold pre-memory services, not SA pre-memory init.
//!
//! Port of sa_pre_mem_native.c:245–356,441–686,1350–1371. SHA256:
//! 27e7566c9724ee95db1b69db040a64b5211a8460eb789b1758f7ebd2cf06a372.
//! Opaque AFE/PH3 values below are source configuration, not measurements.
//! PEG/graphics/BCLK/CSE/VT-d and the full SA wrapper are intentionally absent.
//! Calls require exclusive CPU SA_MSRIO, host bridge, DMIBAR and PCH ownership.
//! Failure stops the pipeline; there is no rollback or memory-readiness claim.
use super::{
    cpu::{Cpu, Package},
    dmi_registers::*,
    early_sa,
    registers::MCHBAR_BASE,
};
use crate::sunrise_point::{
    dmi::{self as pch, Kind},
    pcr::Pcr,
    pmc::MemoryBootMode,
};
use embedded_hal::delay::DelayNs;
use fstart_arch::x86::cpu::msr::{Msr, MsrAccess};
use fstart_pci::ecam::EcamDevice;
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{ReadWriteable, Readable, Writeable},
};

const SA_MSRIO: Msr<SaMsrio::Register> = Msr::new(0x791);
const DMI_AFE_PH3_CFG0_BND0: u64 = 0xa00;
const BUNDLE_STEP: u64 = 0x20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    UnsupportedCpu,
    RetainedMemory,
    BadPolicy,
    InvalidMapping,
    Host(early_sa::Error),
    DecodeMismatch,
    NoDmiLink,
    InvalidCapability,
    LinkTimeout,
    LinkStatus,
    VcTimeout,
}

/// Native HSIO's existing Gen1-only decision must be supplied, not inferred
/// from PCI enumeration. `vc1` is native platform policy (M900 source: true).
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub pch: Kind,
    pub ecam: early_sa::EcamConfig,
    pub hsio_gen1_only: bool,
    pub vc1: bool,
    /// Number of one-ms waits per retrain/VC phase. Zero rejects before I/O.
    /// Native uses 100; callers must budget every mandatory phase separately.
    pub poll_budget_ms: u16,
}

/// Caller-owned operational DMIBAR. No resource allocation or enabling here.
pub struct Dmi<'a> {
    pub(crate) registers: &'a DmiRegisters,
    physical: u32,
}
impl<'a> Dmi<'a> {
    /// # Safety
    /// `virtual_base` maps the whole 4-KiB UC DMIBAR at `physical`, exclusively
    /// owned for `'a`. The supplied ECAM/PCR mappings must likewise be live,
    /// owned and refer to this same silicon. Mapping is not verified by reads.
    pub unsafe fn at(virtual_base: usize, physical: u32) -> Result<Self, Error> {
        if virtual_base == 0
            || virtual_base & 0xfff != 0
            || virtual_base.checked_add(0xfff).is_none()
            || physical != early_sa::DMI_BASE
        {
            return Err(Error::InvalidMapping);
        }
        Ok(Self {
            registers: unsafe { &*(virtual_base as *const DmiRegisters) },
            physical,
        })
    }

    pub(crate) fn preflight(
        &self,
        sa: &EcamDevice,
        cpu: Cpu,
        mode: MemoryBootMode,
        policy: Policy,
    ) -> Result<(), Error> {
        if !matches!(mode, MemoryBootMode::Cold | MemoryBootMode::S3) {
            return Err(Error::RetainedMemory);
        }
        if policy.poll_budget_ms == 0 {
            return Err(Error::BadPolicy);
        }
        // Public Cpu fields cannot authorize a forged package/stepping tuple.
        if Cpu::from_signature(cpu.signature).map_err(|_| Error::UnsupportedCpu)? != cpu {
            return Err(Error::UnsupportedCpu);
        }
        policy.ecam.encoded_pciexbar().map_err(Error::Host)?;
        early_sa::validate_host(sa).map_err(Error::Host)?;
        early_sa::require_ecam(sa, policy.ecam).map_err(Error::Host)?;
        // Reuse exact accepted IDs, then cross-check package against their source
        // OPI selector, also used by sa_pre_mem_native.c:1681–1683.
        let opi = sa.read16(2) & 6 == 4;
        if (opi && policy.pch != Kind::Lp)
            || (!opi && policy.pch != Kind::H)
            || (cpu.package == Package::UltUlx && !opi)
        {
            return Err(Error::BadPolicy);
        }
        // Native warned and continued; this port fails before destructive writes.
        for (offset, expected) in [(0x48, MCHBAR_BASE as u32), (0x68, self.physical)] {
            if unsafe { sa.register::<u32, ()>(offset).get() } != expected | 1
                || unsafe { sa.register::<u32, ()>(offset + 4).get() } != 0
            {
                return Err(Error::DecodeMismatch);
            }
        }
        Ok(())
    }

    /// Source SI 3.7.9.0 recipe tag, not a claim of an Intel binary release.
    pub(crate) fn report_version(&self) {
        self.registers.si_version[0].set(0x03070900);
        self.registers.si_version[1].set(0x03070900);
    }

    pub(crate) fn gate_peg_spine_clock(&self) {
        let r = &self.registers.peg_clock;
        r.set(r.get() & !(7 << 27));
    }

    /// Actual kabylake_native_dmi_init closure. OPI has no native DMI link
    /// recipe; rejecting it here does not pretend the missing full wrapper ran.
    pub fn init_link(
        &self,
        sa: &EcamDevice,
        pcr: &Pcr,
        cpu: Cpu,
        mode: MemoryBootMode,
        policy: Policy,
        msrs: &impl MsrAccess,
        delay: &mut impl DelayNs,
    ) -> Result<u8, Error> {
        self.preflight(sa, cpu, mode, policy)?;
        if cpu.package != Package::DesktopHalo || sa.device_id() & 6 == 4 {
            return Err(Error::NoDmiLink);
        }
        let cap = pch::max_speed(pcr);
        if !(1..=3).contains(&cap) {
            return Err(Error::InvalidCapability);
        }
        let a = unsafe { sa.register::<u32, CapidA::Register>(0xe4) };
        let b = unsafe { sa.register::<u32, CapidB::Register>(0xe8) };
        let speed = if cap == 1 || a.is_set(CapidA::GEN2_DISABLED) || policy.hsio_gen1_only {
            1
        } else if cap <= 2 || b.is_set(CapidB::GEN3_DISABLED) {
            2
        } else {
            3
        };
        self.program_afe(cpu, msrs);
        let r = self.registers;
        r.nfts.modify(Nfts::COUNTS.val(0x2020));
        self.configure_deemphasis();
        self.configure_speed(pcr, speed, policy, delay)?;
        Ok(speed)
    }

    fn configure_deemphasis(&self) {
        // Native deemphasis uses a dword; TLS below uses a word at the same
        // address. Preserve those actual transaction widths without a raw
        // accessor suite or writing back the adjacent word on TLS updates.
        let r = unsafe {
            fstart_core::mmio::MmioRegister::<u32, Deemphasis::Register>::new(core::ptr::addr_of!(
                self.registers.link_control2
            )
                as usize)
        };
        r.modify(Deemphasis::SELECTABLE::SET);
    }

    fn program_afe(&self, cpu: Cpu, msrs: &impl MsrAccess) {
        let r = self.registers;
        for b in &r.bundle {
            b.cfg1.modify(
                Bundle1::IGACQ.val(0)
                    + Bundle1::DFEIDACPD.val(3)
                    + Bundle1::PGACQ.val(10)
                    + Bundle1::OFFCORGAIN.val(3),
            );
            b.cfg2.modify(
                Bundle2::RXVCMDSSEL.val(1)
                    + Bundle2::G3BYPCOEFF.val(1)
                    + Bundle2::TXEQCUR.val(20)
                    + Bundle2::TXEQPOSTCUR.val(2)
                    + Bundle2::TXEQPRECUR.val(2),
            );
            b.cfg3
                .modify(Bundle3::RXRTBIN.val(3) + Bundle3::TXRTBIN.val(4) + Bundle3::PGTRK.val(10));
            b.cfg4.modify(Bundle4::G2RXCTLEPEAK.val(2));
            b.cfg4.modify(Bundle4::SPARE.val(0x40));
        }
        r.common1
            .modify(Common1::IBIAS.val(1) + Common1::PIBIASICTRLGEN3.val(4));
        r.common2
            .modify(Common2::VREFRXDET.val(0x17) + Common2::RXINCMSEL.val(4));
        for lane in 0..4 {
            r.lane[lane].cfg0.modify(Lane0::G23RXVREF.val(10));
            r.lane[lane]
                .cfg1
                .modify(Lane1::CDRPDDATMODE::SET + Lane1::G1RXVREFSEL.val(12));
            r.tx[lane].control.modify(
                LaneTx::OVERRIDE::SET + LaneTx::PI_RESSEL.val(16) + LaneTx::SWING.val(0x88),
            );
            r.io[lane].control.modify(
                LaneIo::GEN3_GAIN.val(0) + LaneIo::GEN2_GAIN.val(1) + LaneIo::GEN1_GAIN.val(2),
            );
        }
        for b in &r.bundle {
            b.spare.modify(
                BundleSpare::SPARE30.val(0)
                    + BundleSpare::GEN3DFEIDACPD.val(3)
                    + BundleSpare::GEN2DFEIDACPD.val(3)
                    + BundleSpare::GEN3DFELSBSEL.val(0)
                    + BundleSpare::GEN2DFELSBSEL.val(1)
                    + BundleSpare::GEN3DFESUM_MFC.val(0)
                    + BundleSpare::GEN3DFESUMADDMFC.val(0)
                    + BundleSpare::GEN2DFESUMADDMFC.val(0),
            );
        }
        r.config5.modify(Config5::UPCFGSLVTMR.val(4));
        r.eq_config.modify(EqConfig::EXTEIEOS::SET);
        r.eq_config.modify(EqConfig::BYPADFSM::SET);
        for lane in 0..4 {
            r.eq_phase3.write(
                EqPhase3::COEFFICIENT.val(0x160c7)
                    + EqPhase3::LANE.val(lane)
                    + EqPhase3::LATCH::SET,
            );
            r.eq_phase3
                .modify(EqPhase3::LANE.val(0) + EqPhase3::LATCH::CLEAR);
        }
        if cpu.package == Package::DesktopHalo {
            let endpoint = msrs.reg(SA_MSRIO);
            // All CFG0, then all CFG1, then all CFG2, across both bundles.
            for (offset, data) in [(0, 0x02010000), (0x10, 0x01805576), (4, 0x00040002)] {
                for bundle in 0..2 {
                    endpoint.write(
                        SaMsrio::ADDRESS.val(DMI_AFE_PH3_CFG0_BND0 + offset + bundle * BUNDLE_STEP)
                            + SaMsrio::DATA.val(data),
                    );
                }
            }
        }
        for b in &r.bundle {
            b.cfg4.modify(Bundle4::G3RXCTLEPEAK.val(3));
        }
    }

    fn train(&self, budget: u16, delay: &mut impl DelayNs) -> Result<(), Error> {
        self.registers
            .link_control
            .modify(LinkControl::RETRAIN::SET);
        for elapsed in 0..=budget {
            // Also observe the self-clearing retrain request: an idle LSTS
            // sampled before hardware accepts the request is not completion.
            if !self.registers.link_status.is_set(LinkStatus::TRAINING)
                && !self.registers.link_control.is_set(LinkControl::RETRAIN)
            {
                return Ok(());
            }
            if elapsed < budget {
                delay.delay_us(1000);
            }
        }
        // Deliberate correction: source logs timeout and continues; fail closed.
        Err(Error::LinkTimeout)
    }

    fn configure_speed(
        &self,
        pcr: &Pcr,
        speed: u8,
        policy: Policy,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let r = self.registers;
        r.link_cap.set(r.link_cap.get());
        if speed >= 3 {
            for eq in &r.eq_preset {
                eq.modify(
                    EqPreset::RP0.val(4)
                        + EqPreset::EP0.val(7)
                        + EqPreset::HINT0.val(2)
                        + EqPreset::RP1.val(4)
                        + EqPreset::EP1.val(7)
                        + EqPreset::HINT1.val(2),
                );
            }
        }
        if speed >= 2 && policy.pch == Kind::H {
            pch::before_speed_change(pcr, speed);
        }
        if speed > 1 {
            r.link_control2
                .modify(LinkControl2::SPEED.val(u16::from(speed)));
        }
        if speed >= 3 {
            r.eq_config.modify(EqConfig::PHASE2_BYPASS::SET);
        }
        self.train(policy.poll_budget_ms, delay)?;
        if r.link_status.read(LinkStatus::SPEED) < u16::from(speed)
            || pch::link_status(pcr) & 15 < u16::from(speed)
        {
            self.train(policy.poll_budget_ms, delay)?;
        }
        if speed >= 3 {
            r.eq_config.modify(EqConfig::PHASE2_BYPASS::CLEAR);
        }
        let sa_status = r.link_status.extract();
        let pch_status = LocalRegisterCopy::<u16, LinkStatus::Register>::new(pch::link_status(pcr));
        // Source only logs final status. Require both ends at target, no ongoing
        // training, and the native four lanes; a partial link is not completion.
        for status in [sa_status, pch_status] {
            if status.read(LinkStatus::SPEED) != u16::from(speed)
                || status.read(LinkStatus::WIDTH) != 4
                || status.is_set(LinkStatus::TRAINING)
            {
                return Err(Error::LinkStatus);
            }
        }
        r.power_timer.modify(PowerTimer::VALUE.val(match speed {
            3 => 16,
            2 => 8,
            _ => 4,
        }));
        Ok(())
    }

    /// dmi_init_max_payload_size, called before the SA wrapper's mandatory
    /// PEG work. Separate from configure_vcs to preserve that source ordering.
    pub fn configure_max_payload(
        &self,
        sa: &EcamDevice,
        cpu: Cpu,
        mode: MemoryBootMode,
        policy: Policy,
    ) -> Result<(), Error> {
        self.preflight(sa, cpu, mode, policy)?;
        let r = self.registers;
        let cap = r.device_cap.extract();
        r.device_cap.set(cap.get());
        if cap.read(DeviceCap::MPS) == 1 {
            r.device_control.modify(DeviceControl::MPS.val(1));
        }
        Ok(())
    }

    /// dmi_init_vc_tc_mapping, called after the SA wrapper's PEG work. This
    /// service does not implement or stand in for the intervening calls.
    pub fn configure_vcs(
        &self,
        sa: &EcamDevice,
        pcr: &Pcr,
        cpu: Cpu,
        mode: MemoryBootMode,
        policy: Policy,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.preflight(sa, cpu, mode, policy)?;
        let r = self.registers;
        if policy.pch == Kind::Lp {
            r.opi.modify(Opi::INIT::SET);
        }
        let tvm0 = if policy.vc1 { 1 } else { 3 };
        r.vc0.modify(
            VcControl::TC0::SET
                + VcControl::MAP.val(tvm0 >> 1)
                + VcControl::ID.val(0)
                + VcControl::ENABLE::SET,
        );
        if policy.vc1 {
            r.vc1
                .modify(VcControl::MAP.val(1) + VcControl::ID.val(1) + VcControl::ENABLE::SET);
        }
        r.vcm
            .modify(VcControl::MAP.val(2) + VcControl::ID.val(7) + VcControl::ENABLE::SET);
        pch::configure_vcs(pcr, policy.vc1);
        for elapsed in 0..=policy.poll_budget_ms {
            if !pch::vcs_pending(pcr, policy.vc1)
                && !r.vc0_status.is_set(VcStatus::PENDING)
                && (!policy.vc1 || !r.vc1_status.is_set(VcStatus::PENDING))
                && !r.vcm_status.is_set(VcStatus::PENDING)
            {
                return Ok(());
            }
            if elapsed < policy.poll_budget_ms {
                delay.delay_us(1000);
            }
        }
        Err(Error::VcTimeout)
    }
}
