// SPDX-License-Identifier: MIT OR Apache-2.0
//! Modern SA compatible-SMRAM control (PCI0:0.0/88h), not a CPU/SMM driver.
//! Enable step: memory_map_native.c:368–377; control semantics: Intel
//!332688-003EN pp75–76. TSEG CPU access/protection separately requires the
//! actual MP/SMM/SMBASE/SMRR owner; SMRAMC is NOT a TSEG/SMRR security proof.
use super::{early_sa, memory_map::ProgrammedMap, sa_registers::*};
use crate::sunrise_point::pmc::MemoryBootMode;
use fstart_arch::x86::cpu::intel::smm::SmramControl as SmramContract;
use fstart_pci::EcamDevice;
use tock_registers::LocalRegisterCopy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    RetainedMemory,
    Host(early_sa::Error),
    DisabledTseg,
    MapChanged(u16),
    InvalidControl(u8),
    Locked,
    Disabled,
    Readback,
}

/// Bind the existing generic Intel SMM contract to the actual modern SA.
/// Contains borrowed real endpoints/map, no invented ECAM/access wrapper trait.
pub struct Smram<'a> {
    sa: &'a EcamDevice,
    map: &'a ProgrammedMap,
}
impl<'a> Smram<'a> {
    /// Native cold G_SMRAME enable, leaving D_LCK to the later real installer.
    /// Full preflight precedes the one8-bit write. No DRAM/SMM-ready token.
    ///
    /// # Safety
    /// Caller owns this actual mapped SA and its actual returned ProgrammedMap,
    /// after successful real cold training, normal activation, MC-done/map
    /// locks and CPU-visible validation. It must exclude competing config users
    /// and retain that ownership for this binding's lifetime. The eventual SMM
    /// owner must supply actual CPU/save-state/SMRR/AP/SMBASE/handler contracts;
    /// this binding does not install SMM, open TSEG through SMRR, or prove CPU
    /// access to TSEG. Never use legacy GMCH control or package PRMRR as SMRR.
    pub unsafe fn enable_cold(
        sa: &'a EcamDevice,
        map: &'a ProgrammedMap,
        mode: MemoryBootMode,
    ) -> Result<Self, Error> {
        if mode != MemoryBootMode::Cold {
            return Err(Error::RetainedMemory);
        }
        let owner = Self { sa, map };
        owner.geometry()?;
        let mut control = owner.control()?;
        if control.is_set(SmramControl::D_LCK) {
            return Err(Error::Locked);
        }
        // Exact native update: clear LOCK/G_SMRAME, then set G_SMRAME;
        // preserve the independent compatible-SMRAM open/close/RO fields.
        control.modify(SmramControl::D_LCK::CLEAR + SmramControl::G_SMRAME::SET);
        owner.publish(control.get())?;
        Ok(owner)
    }
    /// Non-writing binding to the compatible SMRAM already enabled by the
    /// actual controller. No training or SMM-installation proof is created.
    /// # Safety
    /// Retain the same mapped SA/map ownership as enable_cold, with either its
    /// real completion or the validated authenticated cold predecessor chain.
    pub(crate) unsafe fn bind_existing(
        sa: &'a EcamDevice,
        map: &'a ProgrammedMap,
    ) -> Result<Self, Error> {
        map.validate_live(sa).map_err(|_| Error::MapChanged(TSEG))?;
        let owner = Self { sa, map };
        owner.enabled()?;
        Ok(owner)
    }

    fn geometry(&self) -> Result<(u64, u32), Error> {
        early_sa::validate_host(self.sa).map_err(Error::Host)?;
        let p = self.map.plan();
        let size = p
            .bgsm
            .checked_sub(p.tseg)
            .filter(|n| *n != 0)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or(Error::DisabledTseg)?;
        if !size.is_power_of_two() || p.tseg & (u64::from(size) - 1) != 0 {
            return Err(Error::DisabledTseg);
        }
        // Native MC-done locks BGSM/TOLUD, NOT TSEGMB. Preserve unchanged
        // TSEG, allowing only its documented bit0 lock to vary. The later
        // smram_lock contract below locks TSEGMB before compatible D_LCK.
        if u64::from(self.sa.read32(TSEG)) & !1 != p.tseg {
            return Err(Error::MapChanged(TSEG));
        }
        for (offset, expected) in [(BGSM, p.bgsm), (TOLUD, p.tolud)] {
            if u64::from(self.sa.read32(offset))
                != expected | u64::from(LowMemoryBoundary::LOCK::SET.value)
            {
                return Err(Error::MapChanged(offset));
            }
        }
        Ok((p.tseg, size))
    }
    fn control(&self) -> Result<LocalRegisterCopy<u8, SmramControl::Register>, Error> {
        let value = self.sa.read8(SMRAMC);
        let c = LocalRegisterCopy::<u8, SmramControl::Register>::new(value);
        if value & 0x80 != 0
            || c.read(SmramControl::C_BASE_SEG) != 2
            || c.is_set(SmramControl::D_OPEN) && c.is_set(SmramControl::D_CLS)
            || c.is_set(SmramControl::D_OPEN) && c.is_set(SmramControl::D_LCK)
        {
            return Err(Error::InvalidControl(value));
        }
        Ok(c)
    }
    fn publish(&self, value: u8) -> Result<(), Error> {
        self.sa.write8(SMRAMC, value);
        if self.sa.read8(SMRAMC) != value {
            return Err(Error::Readback);
        }
        Ok(())
    }
    fn enabled(&self) -> Result<LocalRegisterCopy<u8, SmramControl::Register>, Error> {
        self.geometry()?;
        let c = self.control()?;
        if !c.is_set(SmramControl::G_SMRAME) {
            return Err(Error::Disabled);
        }
        Ok(c)
    }
    /// Clear OPEN and CLS: CLS=1 blocks SMM *data* accesses, not a normal
    /// closed state usable by a C SMM handler. Existing locked/closed is okay.
    /// Failure is fatal partial configuration, not a rollback certificate.
    pub fn close(&self) -> Result<(), Error> {
        let mut c = self.enabled()?;
        if c.is_set(SmramControl::D_LCK) {
            return if !c.is_set(SmramControl::D_CLS) {
                Ok(())
            } else {
                Err(Error::Locked)
            };
        }
        c.modify(SmramControl::D_OPEN::CLEAR + SmramControl::D_CLS::CLEAR);
        self.publish(c.get())
    }
}
impl SmramContract for Smram<'_> {
    fn tseg(&self) -> Option<(u64, u32)> {
        self.geometry().ok()
    }
    fn smram_open(&self) -> bool {
        let Ok(mut c) = self.enabled() else {
            return false;
        };
        if c.is_set(SmramControl::D_LCK) {
            return false;
        }
        c.modify(SmramControl::D_OPEN::SET + SmramControl::D_CLS::CLEAR);
        self.publish(c.get()).is_ok()
    }
    fn smram_close(&self) {
        // The existing shared contract cannot return a close error. Fail STOP
        // rather than silently continue with possibly visible SMRAM.
        assert!(self.close().is_ok(), "Skylake SMRAM close failed");
    }
    fn smram_lock(&self) -> bool {
        // Never set LOCK while OPEN is set. Close/readback is a separate write
        // BEFORE irreversible LOCK; D_CLS must remain clear for SMM data.
        if self.close().is_err() {
            return false;
        }
        let Ok(mut c) = self.enabled() else {
            return false;
        };
        // Intel332688-003EN p80: TSEGMB bit0 RW_KL locks all writable
        // settings. This is LATER SMM finalization, not native MC-done.
        // CPU SMRR/WB/AP/save-state protection remains the installer's duty;
        // generic SMM's optional legacy SMRR policy does not establish it.
        let lock = LowMemoryBoundary::LOCK::SET.value;
        let tseg = self.sa.read32(TSEG);
        if tseg & lock == 0 {
            self.sa.write32(TSEG, tseg | lock);
        }
        if self.sa.read32(TSEG) != tseg | lock || self.geometry().is_err() {
            return false;
        }
        if c.is_set(SmramControl::D_LCK) {
            return true;
        }
        c.modify(SmramControl::D_LCK::SET);
        #[cfg(target_os = "none")]
        {
            // Skylake D_LCK requires CF8/CFC (native cpu.c:smm_lock).
            // SAFETY: enabled() verified the root bridge and the SMM owner
            // serializes PCI config access through installation.
            unsafe {
                fstart_pci::cf8::write8(0, 0, 0, SMRAMC as u8, c.get());
                fstart_pci::cf8::read8(0, 0, 0, SMRAMC as u8) == c.get()
                    && self.sa.read8(SMRAMC) == c.get()
            }
        }
        #[cfg(not(target_os = "none"))]
        {
            // Host fixtures model PCI config in memory, not CF8/CFC.
            self.publish(c.get()).is_ok()
        }
    }
}

#[cfg(test)]
#[path = "smram_tests.rs"]
mod tests;
