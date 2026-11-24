// SPDX-License-Identifier: MIT OR Apache-2.0
//! The system agent after training: PCI root, SA devices, SMRAM, display.

use super::*;
use crate::IntelNorthbridgeDriver;
use crate::skylake::{package_power, sa_graphics_pm, smram};
use fstart_arch::x86::cpu::intel::smm::SmramControl;
use fstart_pci::PciAddress;

impl IntelSkylake {
    /// Capture the canonical allocator's actual display BARs after final
    /// page tables are installed. Do not resize or relocate an IGD BAR.
    pub fn bind_scanout(&mut self, pci: &fstart_pci::PciEcam) {
        self.scanout = (|| {
            self.config.display.as_ref()?;
            if !self.config.graphics.igd_enabled || self.sa_phase != SaPhase::PostMemory {
                return None;
            }
            let address = PciAddress::new(0, 0, 2, 0);
            let mmio = pci.assigned_memory_bar(address, 0x10)?;
            let aperture = pci.assigned_memory_bar(address, 0x18)?;
            let map = self.programmed_map()?.plan();
            let device = EcamDevice::new(0, 2, 0);
            if device.memory_bar(0x10) != Some(mmio.base)
                || device.memory_bar(0x18) != Some(aperture.base)
            {
                return None;
            }
            Some(crate::igd::IgdAddresses {
                pci_bdf: address,
                gtt_mmio_base: mmio.base,
                gtt_mmio_size: u32::try_from(mmio.size).ok()?,
                gtt_pte_base: None,
                gmadr_base: Some(aperture.base),
                gmadr_size: u32::try_from(aperture.size).ok()?,
                stolen_base: map.bdsm,
                stolen_size: u32::try_from(map.tolud.checked_sub(map.bdsm)?).ok()?,
                gtt_size: u32::try_from(map.bdsm.checked_sub(map.bgsm)?).ok()?,
                gcfgc: None,
            })
        })();
    }

    /// Lend the live IGD engine and board clock policy to the PCH audio step.
    /// The borrow prevents the SA owner from being reused during that step.
    pub fn display_audio(
        &mut self,
    ) -> Result<Option<(sa_graphics_pm::Graphics<'_>, bool, bool)>, ServiceError> {
        if self.state != State::Observed || self.sa_phase != SaPhase::PostMemory {
            return Err(ServiceError::NotInitialized);
        }
        match (self.config.graphics.igd_enabled, self.config.display_audio) {
            (true, Some((mhz96, tmode2t))) => {
                let (resources, _) = self.live_resources()?;
                Ok(Some((self.graphics_binding(&resources)?, mhz96, tmode2t)))
            }
            _ => Ok(None),
        }
    }

    fn root_info(&self) -> PciRootInfo {
        PciRootInfo {
            segment: 0,
            ecam_base: self.config.ecam.base,
            bus_start: 0,
            bus_end: (self.config.ecam.buses - 1) as u8,
        }
    }
    pub(super) fn live_resources(
        &self,
    ) -> Result<(sa_peg::Resources<'_>, sa_dmi::Dmi<'_>), ServiceError> {
        let map = self.programmed_map().ok_or(ServiceError::NotInitialized)?;
        map.validate_live(self.sa.as_ref().ok_or(ServiceError::NotInitialized)?)
            .map_err(|_| ServiceError::HardwareError)?;
        let mch = unsafe { MchRegisters::at(MCHBAR_BASE) };
        let resources = unsafe {
            sa_peg::Resources::from_ecam(
                self.root_info(),
                usize::from(self.config.ecam.buses) << 20,
                mch,
            )
        }
        .map_err(|_| ServiceError::HardwareError)?;
        let dmi = unsafe { sa_dmi::Dmi::at(early_sa::DMI_BASE as usize, early_sa::DMI_BASE) }
            .map_err(|_| ServiceError::HardwareError)?;
        Ok((resources, dmi))
    }

    fn graphics_binding(
        &self,
        resources: &sa_peg::Resources<'_>,
    ) -> Result<sa_graphics_pm::Graphics<'_>, ServiceError> {
        let igd = resources
            .dev(0, 2, 0)
            .map_err(|_| ServiceError::HardwareError)?;
        let physical = igd.memory_bar(0x10).ok_or(ServiceError::HardwareError)?;
        let virtual_base = usize::try_from(physical).map_err(|_| ServiceError::NotSupported)?;
        // Actual canonical PCI allocation is mapped before device init. The
        // returned view is scoped to this owner's borrow and validates its BAR.
        unsafe {
            sa_graphics_pm::Graphics::at(igd, virtual_base, physical, sa_graphics_pm::MMIO_BYTES)
        }
        .map_err(|_| ServiceError::HardwareError)
    }

    fn program_post_memory(&self) -> Result<(), Error> {
        let (resources, dmi) = self.live_resources().map_err(Error::Service)?;
        let map = self
            .programmed_map()
            .ok_or(Error::Service(ServiceError::NotInitialized))?;
        let cpu = Cpu::from_signature(fstart_arch::x86::cpuid(1).0)
            .map_err(|_| Error::Service(ServiceError::NotSupported))?;
        let mut delay = Udelay;
        package_power::configure_interrupt_routing(resources.mch).map_err(Error::PackagePower)?;
        unsafe {
            sa_post_mem::init_links(
                &resources,
                &dmi,
                sa_post_mem::EgressRegisters::at(early_sa::EP_BASE as usize),
                map,
                cpu,
                MemoryBootMode::Cold,
                self.config.post_memory,
                &mut delay,
            )
        }
        .map_err(|_| Error::Service(ServiceError::HardwareError))?;
        unsafe {
            sa_post_mem::init_vtd(
                &resources,
                self.config.ecam,
                MemoryBootMode::Cold,
                sa_post_mem::VtdRegisters::at(early_sa::GFX_VTD_BASE as usize),
                sa_post_mem::VtdRegisters::at(early_sa::VTVC0_BASE as usize),
            )
        }
        .map_err(|_| Error::Service(ServiceError::HardwareError))?;
        sa_post_mem::configure_pavp(&resources, map, self.config.pavp_enabled)
            .map_err(|_| Error::Service(ServiceError::HardwareError))?;
        let msr = unsafe { NativeMsr::new() };
        let power = package_power::Plan::prepare(resources.mch, &msr, self.config.package_power)
            .map_err(Error::PackagePower)?;
        package_power::complete_bios_reset(resources.mch).map_err(Error::PackagePower)?;
        if self.config.graphics.igd_enabled {
            let graphics = self.graphics_binding(&resources).map_err(Error::Service)?;
            graphics
                .configure_pm(cpu, MemoryBootMode::Cold, &msr, &mut delay)
                .map_err(|_| Error::Service(ServiceError::HardwareError))?;
        }
        power
            .install(resources.mch, &msr, &mut delay)
            .map_err(Error::PackagePower)
    }

    fn smram_binding(&self) -> Result<smram::Smram<'_>, ServiceError> {
        if self.state != State::Observed
            || !matches!(self.sa_phase, SaPhase::PostMemory | SaPhase::Locked)
        {
            return Err(ServiceError::NotInitialized);
        }
        let sa = self.sa.as_ref().ok_or(ServiceError::NotInitialized)?;
        let map = self.programmed_map().ok_or(ServiceError::NotInitialized)?;
        // SAFETY: the map was read back from this programmed controller and
        // this driver owns the SA endpoints.
        unsafe { smram::Smram::bind_existing(sa, map) }.map_err(|_| ServiceError::HardwareError)
    }
}

impl IntelNorthbridgeDriver for IntelSkylake {
    fn training_identity(&self) -> Option<[u8; 32]> {
        // The SPDs are not read yet when the platform keys the journal; the
        // cached record carries its own CPU/fuse/SPD identity.
        let host = fstart_pci::EcamDevice::new(0, 0, 0);
        let spd = self.config.spd_addresses;
        Some(crate::generic::training::identity(
            b"skylake-raminit-v1",
            &[spd[0][0], spd[0][1], spd[1][0], spd[1][1]],
            0,
            0,
            crate::generic::training::HardwareIdentity {
                vendor_id: host.read16(0x00),
                device_id: host.read16(0x02),
                revision_id: host.read8(0x08),
                capabilities: [host.read32(0xe4), host.read32(0xe8)],
                cpu_signature: fstart_arch::x86::cpuid(1).0,
            },
        ))
    }
    type Config = Config;
    // Native policies remain immutable board-owned configuration. No user
    // option is allowed to bypass qualified memory/security policy.
    type Options = ();
    const KNOBS: &'static [fstart_options::Knob<()>] = &[];
    fn board_options(_: &Config) {}
    fn options(&self) {}
    fn set_options(&mut self, _: ()) {}

    fn pci_root(&self) -> Result<fstart_pci::PciRoot, fstart_pci::PciEcamError> {
        use fstart_pci::{
            PciEcamError,
            PciRole::{Forwarded, Own, Window},
        };
        #[cfg(not(target_os = "none"))]
        let (base, touud, high_end) = if self.host_baseline {
            (0x8000_0000, 1u64 << 32, 1u64 << 39)
        } else {
            let map = self.programmed_map().ok_or(PciEcamError::ConfigError)?;
            map.validate_live(&EcamDevice::new(0, 0, 0))
                .map_err(|_| PciEcamError::ConfigError)?;
            (
                map.plan().tolud,
                map.plan().touud,
                fstart_arch::x86::physical_address_limit(),
            )
        };
        #[cfg(target_os = "none")]
        let (base, touud, high_end) = {
            let map = self.programmed_map().ok_or(PciEcamError::ConfigError)?;
            map.validate_live(&EcamDevice::new(0, 0, 0))
                .map_err(|_| PciEcamError::ConfigError)?;
            (
                map.plan().tolud,
                map.plan().touud,
                fstart_arch::x86::physical_address_limit(),
            )
        };
        let end = self.config.ecam.base.min(0xfc80_0000);
        if base >= end {
            return Err(PciEcamError::ConfigError);
        }
        let high_base = touud.max(1u64 << 32);
        // Forward fixed PCI decodes too, not just the allocator's smaller
        // arena. Shared host-bridge roles supply ISA/VGA/ROM/TPM routing.
        let forwarded = crate::host_bridge::forwarded(Some(base), None)
            .into_iter()
            .map(|mut r| {
                if r.role == Window {
                    r.role = Forwarded;
                }
                r
            });
        let owned = [
            Window.io(
                u64::from(self.config.pci_io.0),
                u64::from(self.config.pci_io.1 - u32::from(self.config.pci_io.0)),
            ),
            Window.mmio(base, end - base),
            Window.mmio(high_base, high_end.saturating_sub(high_base)),
            Own.mmio(
                self.config.ecam.base,
                u64::from(self.config.ecam.buses) << 20,
            ),
            Own.mmio(MCHBAR_BASE as u64, 0xa000),
            // The PCH HECI1 BAR stays at its fixed cold-boot address, right
            // above EPBAR. The SA forwards that page to DMI; list it so the
            // fixed BAR lies inside _CRS as every PCI BAR must.
            Forwarded.mmio(u64::from(super::cse::HECI1_BASE), 0x1000),
            Own.mmio(0xfed3_0000, 0x10000),
            Own.mmio(0xfed8_0000, 0x5000),
            Own.mmio(u64::from(early_sa::GFX_VTD_BASE), 0x2000),
            Own.mmio(0xfee0_0000, 0x1000),
        ];
        fstart_pci::PciRoot::new(self.root_info(), forwarded.chain(owned))
    }
    const ACPI_PCI_ROOT_NAME: &'static str = "PCI0";
    const ACPI_IGD_NAME: &'static str = "GFX0";
    fn new_from_config(config: &'static Config) -> Result<Self, ServiceError> {
        Self::new(config).map_err(|_| ServiceError::InvalidParam)
    }
    fn config(&self) -> &'static Config {
        self.config
    }
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        // SAFETY: the BSP owns the host bridge's configuration space.
        let sa = unsafe { early_sa::bootstrap_ecam(self.config.ecam) }
            .map_err(|_| ServiceError::HardwareError)?;
        self.sa = Some(sa);
        Ok(())
    }
    fn early_init(&mut self) -> Result<(), ServiceError> {
        let sa = self.sa.as_ref().ok_or(ServiceError::NotInitialized)?;
        early_sa::validate_host(sa).map_err(|_| ServiceError::HardwareError)?;
        Cpu::from_signature(fstart_arch::x86::cpuid(1).0)
            .map_err(|_| ServiceError::NotSupported)?;
        // Destructive SA/TXT work belongs to real MemoryInit, after the actual
        // PCH producer has rejected saved warm/S3 state, not this console hook.
        early_sa::require_ecam(sa, self.config.ecam).map_err(|_| ServiceError::HardwareError)
    }
    fn early_post_dram_init(&mut self) -> Result<(), ServiceError> {
        self.memory_table().map(drop)
    }
    fn memory_detected(&mut self, _e820: &fstart_core::services::memory_detect::E820State) {
        if self.state == State::New
            && let Ok((map, _)) = self.observe()
        {
            self.map = Some(map);
            self.state = State::Observed;
        }
    }
    fn chip_init(&mut self) -> Result<(), ServiceError> {
        if self.state != State::Observed || self.sa_phase != SaPhase::Fresh {
            return Err(ServiceError::NotSupported);
        }
        self.sa_phase = SaPhase::Running;
        let result = (|| {
            let (resources, _) = self.live_resources()?;
            sa_post_mem::configure_devices(
                &resources,
                self.config.post_memory.devices,
                self.config.cio2_enabled,
            )
            .map(drop)
            .map_err(|_| ServiceError::HardwareError)
        })();
        self.sa_phase = if result.is_ok() {
            SaPhase::Devices
        } else {
            SaPhase::Failed
        };
        result
    }
    fn post_dram_init(&mut self) -> Result<(), ServiceError> {
        if self.state != State::Observed || self.sa_phase != SaPhase::Devices {
            return Err(ServiceError::NotSupported);
        }
        self.sa_phase = SaPhase::Running;
        match self.program_post_memory() {
            Ok(()) => {
                self.sa_phase = SaPhase::PostMemory;
                Ok(())
            }
            Err(error) => {
                self.failure.get_or_insert(error);
                self.sa_phase = SaPhase::Failed;
                Err(ServiceError::HardwareError)
            }
        }
    }
    fn post_verify_init(
        &mut self,
        memory: crate::igd::OpRegionMemory<'_>,
    ) -> Result<(), ServiceError> {
        if self.state != State::Observed || self.sa_phase != SaPhase::Locked {
            return Err(ServiceError::NotInitialized);
        }
        if !self.config.graphics.igd_enabled {
            return Ok(());
        }
        let device = EcamDevice::new(0, 2, 0);
        if !device.is_present() {
            return Ok(());
        }
        let vbt = crate::igd::publish_opregion(
            &device,
            crate::igd::OpRegionSci::Swsci,
            &self.config.vbt,
            memory,
        );
        let cpu = Cpu::from_signature(fstart_arch::x86::cpuid(1).0)
            .map_err(|_| ServiceError::NotSupported)?;
        let display_cpu = match cpu.family {
            super::super::cpu::Family::Skylake => crate::gma::types::Cpu::Skylake,
            super::super::cpu::Family::KabyLake => crate::gma::types::Cpu::Kabylake,
        };
        if let Some(addresses) = &self.scanout {
            self.display
                .initialize::<crate::gma::generation::skylake::Skylake>(
                    display_cpu,
                    self.config.display.as_ref(),
                    addresses,
                    vbt,
                );
        }
        Ok(())
    }

    fn framebuffer_info(&self) -> Option<fstart_core::services::FramebufferInfo> {
        self.display.framebuffer_info()
    }

    fn memory_info(&self) -> Option<fstart_core::memory_info::MemoryInfo> {
        self.memory_info
    }

    fn stage_local_init(&mut self) -> Result<(), ServiceError> {
        if self.state != State::Observed || self.sa_phase != SaPhase::PostMemory {
            return Err(ServiceError::NotSupported);
        }
        self.sa_phase = SaPhase::Running;
        let result = (|| {
            let (resources, dmi) = self.live_resources()?;
            sa_post_mem::lock(
                &resources,
                &dmi,
                self.programmed_map().ok_or(ServiceError::NotInitialized)?,
                self.config.post_memory.devices.thermal,
                self.config.dmi.pch == crate::sunrise_point::dmi::Kind::H,
            )
            .map_err(|_| ServiceError::HardwareError)
        })();
        self.sa_phase = if result.is_ok() {
            SaPhase::Locked
        } else {
            SaPhase::Failed
        };
        result
    }
}
impl SmramControl for IntelSkylake {
    fn tseg(&self) -> Option<(u64, u32)> {
        self.smram_binding().ok()?.tseg()
    }
    fn smram_open(&self) -> bool {
        self.smram_binding().is_ok_and(|owner| owner.smram_open())
    }
    fn smram_close(&self) {
        self.smram_binding()
            .expect("Skylake SMRAM owner lost")
            .smram_close();
    }
    fn smram_lock(&self) -> bool {
        self.smram_binding().is_ok_and(|owner| owner.smram_lock())
    }
}
