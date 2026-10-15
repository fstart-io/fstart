//! Intel GM45 (Cantiga) northbridge.
//!
//! This module implements early host-bridge decode and cold-boot DDR3 initialization.
//! Sparse destructive probes gate RAM availability; they are not exhaustive qualification.

mod igd;
mod pm;
mod post_dram;
mod raminit;
mod ramtest;
mod reset;

use fstart_core::mmio::{MmioReadOnly, MmioReadWrite};
use fstart_core::services::memory_detect::{
    E820Entry, E820Kind, MemoryDetector, build_pc_compatible_e820,
};
use fstart_core::services::{MemoryController, ServiceError};
use fstart_pci::ecam;
use fstart_pci::pci_type0_config;
use fstart_pci::{
    PciRootError, PciRootInfo, PciRootProvider, PciRootWindows, PciWindow, PciWindowKind,
};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{register_bitfields, register_structs};

pub use igd::{BacklightPwm, Gm45IgdConfig};

register_bitfields![u8, TpmAccess [VALID OFFSET(7) NUMBITS(1) []]];

const DEVEN_HOST: u32 = 1;
const DEVEN_PEG: u32 = 1 << 1;
const DEVEN_IGD: u32 = 1 << 3;
const DEVEN_IGD_SECONDARY: u32 = 1 << 4;
const PCI_PIO_BASE: u64 = 0x1000;
const PCI_PIO_SIZE: u64 = 0xf000;

pci_type0_config! {
    /// GM45 host bridge register layout (PCI 00:00.0).
    pub struct Gm45HostBridgePciConfig {
        (0x40 => pub epbar_lo: MmioReadWrite<u32>),
        (0x44 => pub epbar_hi: MmioReadWrite<u32>),
        (0x48 => pub mchbar_lo: MmioReadWrite<u32>),
        (0x4c => pub mchbar_hi: MmioReadWrite<u32>),
        (0x50 => _reserved_hb0),
        (0x52 => pub ggc: MmioReadWrite<u16>),
        (0x54 => pub deven: MmioReadWrite<u32>),
        (0x58 => _reserved_hb1),
        (0x68 => pub dmibar_lo: MmioReadWrite<u32>),
        (0x6c => pub dmibar_hi: MmioReadWrite<u32>),
        (0x70 => _reserved_hb2),
        (0x78 => pub pmbase: MmioReadWrite<u16>),
        (0x7a => _reserved_hb3),
        (0x90 => pub pam: [MmioReadWrite<u8>; 7]),
        (0x97 => _reserved_hb4),
        (0x98 => pub remap_base: MmioReadWrite<u16>),
        (0x9a => pub remap_limit: MmioReadWrite<u16>),
        (0x9c => _reserved_hb4a),
        (0x9d => pub smram: MmioReadWrite<u8>),
        (0x9e => pub esmramc: MmioReadWrite<u8>),
        (0x9f => _reserved_hb5),
        (0xa0 => pub tom: MmioReadWrite<u16>),
        (0xa2 => pub touud: MmioReadWrite<u16>),
        (0xa4 => _reserved_hb6),
        (0xb0 => pub tolud: MmioReadWrite<u16>),
        (0xb2 => _reserved_hb7),
        (0xe0 => pub capid0: MmioReadWrite<u32>),
        (0xe4 => pub capid_high: MmioReadWrite<u32>),
        (0xe8 => _reserved_hb8: [u8; 8]),
        (0xf0 => jedec_control: MmioReadWrite<u8>),
        (0xf1 => @END),
    }
}

/// Graphics functions requested by board policy (not a board identity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gm45GraphicsPolicy {
    Integrated,
    Discrete,
    Hybrid,
}

impl Gm45GraphicsPolicy {
    const fn deven_bits(self) -> u32 {
        match self {
            Self::Integrated => DEVEN_IGD | DEVEN_IGD_SECONDARY,
            Self::Discrete => DEVEN_PEG,
            Self::Hybrid => DEVEN_PEG | DEVEN_IGD | DEVEN_IGD_SECONDARY,
        }
    }
}

/// Typed, immutable chipset configuration. A board may override graphics
/// selection at runtime after probing its GPIO/EC using `set_graphics_policy`.
#[derive(Debug, Clone, Copy)]
pub struct IntelGm45Config {
    pub mchbar: u64,
    pub dmibar: u64,
    pub epbar: u64,
    pub ecam_base: u64,
    /// 64, 128 or 256 buses.
    pub ecam_buses: u16,
    pub pmbase: u16,
    /// Default graphics policy; the board selects the actual runtime policy.
    pub graphics: Gm45GraphicsPolicy,
    /// SPD slot order: channel 0 slot 0/1, channel 1 slot 0/1.
    pub spd_addresses: [u8; 4],
    /// ICH9 I801 SMBus I/O base used to read DIMM SPD before RAM is online.
    pub smbus_base: u16,
    /// ICH RCBA, reserved with the host bridge's MMIO (coreboot gm45.asl).
    pub rcba: u64,
    /// Board-requested Super-LFM; enabled only on capable CPUs.
    pub slfm: bool,
    /// Board IGD policy: panel sequencing, backlight and display bring-up.
    pub igd: Gm45IgdConfig,
    /// Board `_CST` tables (coreboot mainboard `cstates.c`).
    pub cstates: crate::cpu::Core2Cstates,
    /// ME functions 03.0-03.3 the board disables (bit n hides 03.n).
    pub disabled_me_functions: u8,
    pub acpi_name: Option<&'static str>,
}

impl IntelGm45Config {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            mchbar: 0xfed1_4000,
            dmibar: 0xfed1_8000,
            epbar: 0xfed1_9000,
            ecam_base: 0xe000_0000,
            ecam_buses: 64,
            pmbase: 0x500,
            graphics: Gm45GraphicsPolicy::Integrated,
            spd_addresses: [0x50, 0, 0x51, 0],
            smbus_base: crate::ich9::ich9::DEFAULT_SMBUS_BASE,
            rcba: 0xfed1_c000,
            slfm: false,
            igd: Gm45IgdConfig::new(),
            cstates: crate::cpu::CORE2_MOBILE_CSTATES,
            disabled_me_functions: 0,
            acpi_name: Some("PCI0"),
        }
    }
}

impl Default for IntelGm45Config {
    fn default() -> Self {
        Self::new()
    }
}

register_bitfields![u16,
    /// GM45 DMIBAR link de-emphasis and equalization.
    DmiLinkCtl2 [DEEMPH_EQ OFFSET(10) NUMBITS(2) []]
];
register_structs! {
    /// Sparse view of GM45 early DMIBAR registers.
    pub Gm45DmiBarRegs {
        (0x0000 => _pad0: [u8; 0x204]),
        (0x0204 => link_ctl2: MmioReadWrite<u16, DmiLinkCtl2::Register>),
        (0x0206 => @END),
    }
}

register_structs! {
    /// GM45 memory/FSB clock strap; MCHBAR must already be enabled.
    Gm45MchClockRegs {
        (0x0000 => _pad: [u8; 0xc00]),
        (0x0c00 => clkcfg: MmioReadWrite<u32>),
        (0x0c04 => _pad1: [u8; 0x0b]),
        (0x0c0f => hpll_vco: MmioReadWrite<u8>),
        (0x0c10 => @END),
    }
}

/// GM45 driver: early decode and (once trained) memory-map readback.
pub struct IntelGm45 {
    config: &'static IntelGm45Config,
    graphics: Gm45GraphicsPolicy,
    mmio32_window: Option<(u64, u64)>,
    bootstrap_windows: Option<[(u32, u32); 2]>,
    dram_ready: bool,
    boot_path: crate::BootPath,
    trained_mode: Option<(u8, u32, u32)>,
    igd_bars: Option<crate::igd::IgdBars>,
    /// Backlight PWM frequency of the panel table entry; 0 if unknown.
    panel_pwm_hz: u16,
    display: crate::igd::IgdDisplay,
}

impl IntelGm45 {
    /// Set graphics routing after board detection and before `early_init`.
    /// The board owns discrete GPU detection, panel muxing and power rails.
    pub fn set_graphics_policy(&mut self, policy: Gm45GraphicsPolicy) {
        self.graphics = policy;
    }

    pub fn graphics_policy(&self) -> Gm45GraphicsPolicy {
        self.graphics
    }

    fn mch_clock(&self) -> &'static Gm45MchClockRegs {
        // SAFETY: setup_early_decode enabled MCHBAR at this validated base.
        unsafe { &*(self.config.mchbar as *const Gm45MchClockRegs) }
    }

    fn mch_vco(&self) -> u8 {
        self.mch_clock().hpll_vco.get()
    }

    fn hostbridge(&self) -> &'static Gm45HostBridgePciConfig {
        // SAFETY: ECAM was installed in pre_console_init before hostbridge access.
        unsafe { ecam::EcamDevice::new(0, 0, 0).regs::<Gm45HostBridgePciConfig>() }
    }

    fn enable_ecam(&self) {
        #[cfg(target_arch = "x86_64")]
        {
            let length = match self.config.ecam_buses {
                256 => 0,
                128 => 1,
                64 => 2,
                _ => unreachable!(),
            };
            // SAFETY: the only legacy config accesses, needed to enable ECAM.
            unsafe {
                fstart_core::pio::pci_cfg_write32(0, 0, 0, 0x64, 0);
                fstart_core::pio::pci_cfg_write32(
                    0,
                    0,
                    0,
                    0x60,
                    (self.config.ecam_base as u32) | (length << 1) | 1,
                );
            }
        }
        ecam::init(self.config.ecam_base as usize);
    }

    fn setup_early_decode(&self) {
        let hb = self.hostbridge();
        hb.mchbar_lo.set(self.config.mchbar as u32 | 1);
        hb.mchbar_hi.set(0);
        hb.dmibar_lo.set(self.config.dmibar as u32 | 1);
        hb.dmibar_hi.set(0);
        hb.epbar_lo.set(self.config.epbar as u32 | 1);
        hb.epbar_hi.set(0);
        hb.pmbase.set(self.config.pmbase | 1);
        hb.pam[0].set(0x30);
        for pam in &hb.pam[1..] {
            pam.set(0x33);
        }
        // Leave ME/other host functions intact; only change board-selected
        // graphics and PEG functions, unlike the historical all-bit rewrite.
        hb.deven.set(
            (hb.deven.get() & !(DEVEN_PEG | DEVEN_IGD | DEVEN_IGD_SECONDARY))
                | DEVEN_HOST
                | self.graphics.deven_bits(),
        );
        // romstage.c clears DMILCTL2 deemphasis/EQ bits before RAM training.
        // SAFETY: DMIBAR was enabled above; the overlay covers its link register.
        let dmi = unsafe { &*(self.config.dmibar as *const Gm45DmiBarRegs) };
        dmi.link_ctl2.modify(DmiLinkCtl2::DEEMPH_EQ.val(0));
    }

    fn tolud(&self) -> u32 {
        u32::from(self.hostbridge().tolud.get() & 0xfff0) << 16
    }
    fn tom(&self) -> u64 {
        u64::from(self.hostbridge().tom.get() & 0x1ff) << 27
    }
    fn touud(&self) -> u64 {
        u64::from(self.hostbridge().touud.get()) << 20
    }

    fn tseg_size_from_esmramc(esmramc: u8) -> Option<u32> {
        if esmramc & 1 == 0 {
            return Some(0);
        }
        match (esmramc >> 1) & 3 {
            0 => Some(1 << 20),
            1 => Some(2 << 20),
            2 => Some(8 << 20),
            _ => None,
        }
    }

    // coreboot gm45/memmap.c: graphics stolen and GTT follow TOLUD, then TSEG.
    fn reserved_memory(gcc: u16, esmramc: u8) -> Option<u32> {
        const UMA_MIB: [u16; 14] = [0, 1, 4, 8, 16, 32, 48, 64, 128, 256, 96, 160, 224, 352];
        let mut reserved = Self::tseg_size_from_esmramc(esmramc)?;
        if gcc & 2 == 0 {
            let gms = usize::from((gcc >> 4) & 0xf);
            let uma = u32::from(*UMA_MIB.get(gms)?) << 20;
            let gtt = match (gcc >> 8) & 0xf {
                0 => 0,
                1 => 1 << 20,
                3 | 9 => 2 << 20,
                10 => 3 << 20,
                11 => 4 << 20,
                _ => return None,
            };
            reserved = reserved.checked_add(uma)?.checked_add(gtt)?;
        }
        Some(reserved)
    }

    fn wait_for_txt_clear(&self) -> Result<(), ServiceError> {
        if self.hostbridge().capid_high.get() & (1 << 5) != 0 {
            return Ok(()); // TXT fused off
        }
        // coreboot romstage waits for TPM_ACCESS_VALID after TXT clears RAM.
        // SAFETY: fixed TPM locality-zero MMIO, read-only access.
        let access =
            unsafe { &*(0xfed4_0000usize as *const MmioReadOnly<u8, TpmAccess::Register>) };
        for _ in 0..1000 {
            if access.is_set(TpmAccess::VALID) {
                return Ok(());
            }
            fstart_arch::udelay(1000);
        }
        Err(ServiceError::Timeout)
    }

    fn checked_memory_map(
        tom: u64,
        tolud: u32,
        raw_touud: u64,
        ggc: u16,
        esmramc: u8,
    ) -> Result<(u32, u32, u64), ServiceError> {
        let reserved = Self::reserved_memory(ggc, esmramc).ok_or(ServiceError::HardwareError)?;
        let usable = tolud
            .checked_sub(reserved)
            .ok_or(ServiceError::HardwareError)?;
        let max_reclaim = 0x1_0000_0000u64.saturating_sub(u64::from(tolud));
        let touud = if raw_touud > 0x1_0000_0000 && raw_touud <= tom.saturating_add(max_reclaim) {
            raw_touud
        } else {
            tom
        };
        if usable <= 0x10_0000 || tolud <= 0x10_0000 || tom <= 0x10_0000 || u64::from(tolud) > tom {
            return Err(ServiceError::HardwareError);
        }
        Ok((usable, tolud, touud))
    }

    fn memory_map(&self) -> Result<(u32, u32, u64), ServiceError> {
        let hb = self.hostbridge();
        Self::checked_memory_map(
            self.tom(),
            self.tolud(),
            self.touud(),
            hb.ggc.get(),
            hb.esmramc.get(),
        )
    }

    fn probe_low_memory(&self) -> Result<(), ServiceError> {
        let (usable, _, _) = self.memory_map()?;
        ramtest::sample(
            usable,
            self.bootstrap_windows.ok_or(ServiceError::NotInitialized)?,
        )
    }
}

impl crate::IntelEcamConfig for IntelGm45Config {
    fn ecam_base(&self) -> u64 {
        self.ecam_base
    }
}

impl crate::IntelNorthbridgeDriver for IntelGm45 {
    type Config = IntelGm45Config;
    fn new_from_config(config: &'static Self::Config) -> Result<Self, ServiceError> {
        let buses = config.ecam_buses;
        if !matches!(buses, 64 | 128 | 256)
            || config.ecam_base == 0
            || config.ecam_base & (u64::from(buses) * 1024 * 1024 - 1) != 0
            || config
                .ecam_base
                .checked_add(u64::from(buses) << 20)
                .is_none_or(|end| end > 0x1_0000_0000)
            || [config.mchbar, config.dmibar, config.epbar]
                .iter()
                .any(|base| *base == 0 || *base > u32::MAX as u64 || (*base & 0xfff) != 0)
            || config.pmbase == 0
            || config.pmbase & 0xff != 0
            || config.smbus_base == 0
            || config.smbus_base & 0x1f != 0
        {
            return Err(ServiceError::HardwareError);
        }
        Ok(Self {
            config,
            graphics: config.graphics,
            mmio32_window: None,
            bootstrap_windows: None,
            dram_ready: false,
            boot_path: crate::BootPath::Normal,
            trained_mode: None,
            igd_bars: None,
            panel_pwm_hz: 0,
            display: crate::igd::IgdDisplay::new(),
        })
    }
    fn config(&self) -> &'static Self::Config {
        self.config
    }
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        self.enable_ecam();
        Ok(())
    }
    fn early_init(&mut self) -> Result<(), ServiceError> {
        self.enable_ecam();
        self.setup_early_decode();
        // The mainstage repeats early_init after the bootblock trained RAM.
        // Recover the published decode there; dram_init clears this again.
        self.dram_ready = self.raminit_completed() && self.memory_map().is_ok();
        Ok(())
    }
    fn stage_local_init(&mut self) -> Result<(), ServiceError> {
        self.enable_ecam();
        // Mainstage is authenticated and reached only after bootblock's RAM
        // probes. Recover decode from hardware, never retrain or test live RAM.
        self.memory_map()?;
        self.dram_ready = true;
        // Coreboot gma_func0_init() runs at device init, after allocation.
        self.igd_func0_init();
        Ok(())
    }
    fn chip_init(&mut self) -> Result<(), ServiceError> {
        // Coreboot gm45_init(): hide the ME functions disabled in devicetree
        // (DEVEN bit 6 + n), then D4F0 once no ME function remains. Only the
        // mainstage runs this, after raminit sized the ME memory from 03.0.
        let hb = self.hostbridge();
        let mut deven = hb.deven.get() & !(u32::from(self.config.disabled_me_functions & 0xf) << 6);
        if deven & (0xf << 6) == 0 {
            deven &= !(1 << 14);
        }
        hb.deven.set(deven);
        Ok(())
    }
    fn post_verify_init(
        &mut self,
        opregion: crate::igd::OpRegionMemory<'_>,
    ) -> Result<(), ServiceError> {
        // The OpRegion embeds the VBT out of verified boot media.
        self.igd_publish_and_modeset(opregion);
        Ok(())
    }
    fn resume_init(&mut self, opregion: Option<u32>) -> Result<(), ServiceError> {
        self.enable_ecam();
        // Coreboot gma_func0_init() on resume: everything but the modeset.
        self.igd_func0_init();
        self.igd_restore_opregion(opregion);
        Ok(())
    }
    fn framebuffer_info(&self) -> Option<fstart_core::services::FramebufferInfo> {
        self.display.framebuffer_info()
    }
    fn prepare_resume_reset(&self) {
        self.prepare_reset_geometry();
    }
    fn supports_s3_replay(&self) -> bool {
        // Implemented after coreboot's resume raminit; not hardware-validated.
        true
    }
    fn training_identity(&self) -> Option<[u8; 32]> {
        // Straps, fuses and board policy only: the bootblock computes this
        // before raminit and the mainstage again after it.
        let hb = self.hostbridge();
        Some(crate::generic::training::identity(
            b"gm45-raminit-v1",
            &self.config.spd_addresses,
            self.graphics as u16,
            (self.mch_clock().clkcfg.get() & 7) as u8,
            crate::generic::training::HardwareIdentity {
                vendor_id: hb.vendor_id.get(),
                device_id: hb.device_id.get(),
                revision_id: hb.revision_id.get(),
                capabilities: [hb.capid0.get(), hb.capid_high.get()],
                cpu_signature: fstart_arch::x86::cpuid(1).0,
            },
        ))
    }
    fn dram_init_cached(
        &mut self,
        _smbus: Option<&mut dyn fstart_core::services::SmBus>,
        cached: Option<&[u8]>,
        output: &mut [u8],
    ) -> Result<Option<usize>, ServiceError> {
        use zerocopy::IntoBytes;
        let record = self.raminit(cached)?;
        if self.boot_path == crate::BootPath::S3Resume {
            return Ok(None);
        }
        // Published only after the cold-boot probe passed inside raminit.
        let bytes = record.as_bytes();
        output
            .get_mut(..bytes.len())
            .ok_or(ServiceError::InvalidParam)?
            .copy_from_slice(bytes);
        Ok(Some(bytes.len()))
    }
    fn early_post_dram_init(&mut self) -> Result<(), ServiceError> {
        let (revision, _, _) = self.trained_mode.ok_or(ServiceError::NotInitialized)?;
        if self.graphics != Gm45GraphicsPolicy::Integrated {
            return Err(ServiceError::InvalidParam);
        }
        self.post_dram_pcie(revision)
    }
    fn finish_early_post_dram_init(&mut self) -> Result<(), ServiceError> {
        let (revision, _, _) = self.trained_mode.ok_or(ServiceError::NotInitialized)?;
        self.mark_post_dram();
        self.post_dram_iommu(revision);
        Ok(())
    }
    fn set_boot_path(&mut self, boot_path: crate::BootPath) {
        self.boot_path = boot_path;
    }
    fn set_bootstrap_windows(
        &mut self,
        postcar: (u64, u64),
        mainstage: (u64, u64),
    ) -> Result<(), ServiceError> {
        self.bootstrap_windows = None;
        let windows = [postcar, mainstage].map(|(base, end)| -> Result<_, ServiceError> {
            Ok((
                u32::try_from(base).map_err(|_| ServiceError::HardwareError)?,
                u32::try_from(end).map_err(|_| ServiceError::HardwareError)?,
            ))
        });
        let windows = [windows[0]?, windows[1]?];
        ramtest::validate_windows(windows)?;
        self.bootstrap_windows = Some(windows);
        Ok(())
    }
    fn memory_detected(&mut self, state: &fstart_core::services::memory_detect::E820State) {
        if state.count() == 0 {
            self.mmio32_window = None;
            return;
        }
        let low_ram_top = state
            .entries()
            .iter()
            .filter(|e| e.kind == E820Kind::Ram as u32 && e.addr < 0x1_0000_0000)
            .map(|e| e.addr.saturating_add(e.size).min(0x1_0000_0000))
            .max()
            .unwrap_or(0x10_0000);
        // The E820 reserved interval above RAM includes stolen graphics and
        // TSEG; PCI BARs must start after those reservations, not after RAM.
        let reserved_top = state
            .entries()
            .iter()
            .filter(|e| {
                e.kind != E820Kind::Ram as u32 && e.addr >= low_ram_top && e.addr < 0x1_0000_0000
            })
            .map(|e| e.addr.saturating_add(e.size).min(0x1_0000_0000))
            .max()
            .unwrap_or(low_ram_top);
        let base = (reserved_top.max(low_ram_top).saturating_add(0xfffff)) & !0xfffff;
        let limit = self.config.ecam_base.min(0x1_0000_0000);
        self.mmio32_window = (base < limit).then_some((base, limit.saturating_sub(base)));
    }
}

impl IntelGm45 {
    /// Coreboot gm45 `raminit(s3resume)`. The southbridge clears SLP_TYP
    /// while detecting S3, so the boot path comes from the shared flow.
    /// `cached` is the journaled [`raminit::TrainingRecord`]; only a resume
    /// replays it, cold boots always train like coreboot.
    fn raminit(&mut self, cached: Option<&[u8]>) -> Result<raminit::TrainingRecord, ServiceError> {
        self.dram_ready = false;
        self.trained_mode = None;
        let resume = self.boot_path == crate::BootPath::S3Resume;
        if self.bootstrap_windows.is_none() {
            return Err(ServiceError::NotInitialized);
        }
        // Coreboot romstage/raminit order: soft-reset recovery, then TXT.
        self.recover_early_reset();
        self.wait_for_txt_clear()?;
        // Coreboot gm45/get_gmch_info accepts B1/B2/B3 and conversion A1.
        // No supported stepping may skip its electrical calibration.
        let revision = ecam::EcamDevice::new(0, 0, 0).read8(0x08);
        if !matches!(revision, 5 | 6 | 7 | 9) {
            return Err(ServiceError::HardwareError);
        }
        let clkcfg = self.mch_clock().clkcfg.get();
        let fsb_mhz = match clkcfg & 7 {
            6 => 1067,
            2 => 800,
            3 => 667,
            _ => return Err(ServiceError::HardwareError),
        };
        let capid0 = self.hostbridge().capid0.get();
        let max_ddr3_mt = match (capid0 >> 30) & 3 {
            0 => 1067,
            1 => 800,
            _ => return Err(ServiceError::HardwareError),
        };
        let max_fsb_mhz = match (capid0 >> 28) & 3 {
            1 => 1067,
            2 => 800,
            3 => 667,
            _ => return Err(ServiceError::HardwareError),
        };
        if fsb_mhz > max_fsb_mhz {
            return Err(ServiceError::HardwareError);
        }
        let mut smbus = crate::southbridge::smbus::I801SmBus::enable_on_i801(
            0,
            0x1f,
            3,
            self.config.smbus_base,
        );
        let channels = raminit::probe_ddr3(&mut smbus, &self.config.spd_addresses)?;
        let timing = raminit::select_ddr3_timing(&channels, fsb_mhz, max_ddr3_mt)?;
        let capid_high = self.hostbridge().capid_high.get();
        fstart_log::info!(
            "gm45: DDR3 SPD timing selected: {} MHz CL{}; RAM not yet usable",
            timing.clock.mhz(),
            timing.cas
        );
        let replay = if resume {
            // Retained RAM must never be retrained or tested: fail before
            // touching the controller unless this boot's modules and timing
            // match the record (coreboot keeps the same values in CMOS).
            let record = cached
                .and_then(|bytes| raminit::TrainingRecord::parse(bytes, &channels, &timing))
                .ok_or_else(|| {
                    fstart_log::error!("gm45: S3 resume without matching training record");
                    ServiceError::HardwareError
                })?;
            Some(record)
        } else {
            None
        };
        self.enter_raminit();
        let record = raminit::prepare_ddr3(
            self.config.mchbar,
            self.config.epbar,
            revision,
            fsb_mhz,
            capid_high,
            self.graphics,
            &channels,
            &timing,
            replay.as_ref(),
        )?;
        // The final map and all post-map programming must complete before
        // touching DRAM. Failure keeps every public RAM API fail-closed.
        // The probe is destructive: never on memory the suspended OS owns.
        if !resume {
            self.probe_low_memory()?;
        }
        // Coreboot romstage: hide D4F0 and finish PM before ICH9 DMI setup.
        let hb = self.hostbridge();
        hb.deven.set(hb.deven.get() & !0x4000);
        self.post_dram_pm(revision, fsb_mhz, timing.clock.mhz())?;
        self.trained_mode = Some((revision, fsb_mhz, timing.clock.mhz()));
        self.dram_ready = true;
        self.finish_raminit();
        Ok(record)
    }
}

impl MemoryController for IntelGm45 {
    fn dram_init(&mut self) -> Result<(), ServiceError> {
        // Without a training record retained RAM cannot be resumed.
        if self.boot_path == crate::BootPath::S3Resume {
            self.dram_ready = false;
            return Err(ServiceError::NotInitialized);
        }
        self.raminit(None).map(|_| ())
    }
    fn detected_size_bytes(&self) -> u64 {
        if self.dram_ready { self.tom() } else { 0 }
    }
    fn memory_test(&self) -> Result<(), ServiceError> {
        // Cold-boot probes already ran before setting dram_ready. Repeating
        // them after postcar/mainstage loads would overwrite live stage code.
        if self.dram_ready {
            Ok(())
        } else {
            Err(ServiceError::NotInitialized)
        }
    }
}

impl MemoryDetector for IntelGm45 {
    fn detect_memory(&self, entries: &mut [E820Entry]) -> Result<usize, ServiceError> {
        if !self.dram_ready {
            return Err(ServiceError::NotInitialized);
        }
        let (usable, tolud, touud) = self.memory_map()?;
        build_pc_compatible_e820(entries, usable, touud, tolud)
    }
    fn total_ram_bytes(&self) -> Result<u64, ServiceError> {
        if !self.dram_ready {
            return Err(ServiceError::NotInitialized);
        }
        Ok(self.tom())
    }
}

impl PciRootProvider for IntelGm45 {
    fn root_info(&self) -> PciRootInfo {
        PciRootInfo {
            segment: 0,
            ecam_base: self.config.ecam_base,
            bus_start: 0,
            bus_end: (self.config.ecam_buses - 1) as u8,
        }
    }
    fn resource_windows(&self) -> Result<PciRootWindows, PciRootError> {
        let mut windows = PciRootWindows::new();
        if let Some((base, size)) = self.mmio32_window {
            windows
                .push(PciWindow {
                    kind: PciWindowKind::Mmio,
                    base,
                    size,
                    prefetchable: false,
                })
                .map_err(|_| PciRootError::TooManyWindows)?;
        }
        windows
            .push(PciWindow {
                kind: PciWindowKind::Io,
                base: PCI_PIO_BASE,
                size: PCI_PIO_SIZE,
                prefetchable: false,
            })
            .map_err(|_| PciRootError::TooManyWindows)?;
        Ok(windows)
    }
}

impl fstart_arch::x86::cpu::intel::smm::SmramControl for IntelGm45 {
    fn tseg(&self) -> Option<(u64, u32)> {
        if !self.dram_ready {
            return None;
        }
        let size = Self::tseg_size_from_esmramc(self.hostbridge().esmramc.get())?;
        let (base, _, _) = self.memory_map().ok()?;
        (size != 0).then_some((u64::from(base), size))
    }
    fn smram_open(&self) -> bool {
        self.hostbridge().smram.set(crate::gmch::smram::open());
        crate::gmch::smram::is_open(self.hostbridge().smram.get())
    }
    fn smram_close(&self) {
        self.hostbridge().smram.set(crate::gmch::smram::closed());
    }
    fn smram_lock(&self) -> bool {
        self.hostbridge().smram.set(crate::gmch::smram::locked());
        crate::gmch::smram::is_locked(self.hostbridge().smram.get()) && self.tseg().is_some()
    }
}

#[cfg(feature = "acpi")]
mod acpi_impl {
    use super::*;
    use alloc::vec::Vec;
    use fstart_acpi::device::AcpiDevice;
    use fstart_acpi_macros::acpi_dsl;

    impl AcpiDevice for IntelGm45 {
        type Config = IntelGm45Config;
        /// Coreboot `gm45.asl`/`hostbridge.asl` plus the `_OSC` from
        /// `i82801ix/acpi/ich9.asl`: the host bridge's producer windows, the
        /// chipset-reserved MMIO, and the IGD node.
        fn dsdt_aml(&self, config: &Self::Config) -> Vec<u8> {
            let mchbar = config.mchbar;
            let dmibar = config.dmibar;
            let epbar = config.epbar;
            let rcba = config.rcba;
            let ecam_base = config.ecam_base;
            let ecam_size = u64::from(config.ecam_buses) << 20;
            let bus_max = config.ecam_buses - 1;
            // PCI MMIO runs from TOLUD to the I/O APIC; above 4 GiB from
            // TOUUD to the CPU's physical address limit (coreboot A4GB/A4GS).
            let (tolud, touud) = self
                .dram_ready
                .then(|| self.memory_map().ok())
                .flatten()
                .map_or((0x8000_0000, 1 << 32), |(_, tolud, touud)| (tolud, touud));
            let high_base = touud.max(1 << 32);
            let high_limit = (1u64 << phys_address_bits()) - 1;
            let gttmmio = self.igd_bars.map_or(0, |bars| bars.gtt_mmio);
            let mut aml: Vec<u8> = acpi_dsl! {
                Device("PCI0") {
                    Name("_HID", EisaId("PNP0A08"));
                    Name("_CID", EisaId("PNP0A03"));
                    Name("_SEG", 0u32);
                    Name("_BBN", 0u32);
                    Name("_UID", 0u32);
                    Device("MCHC") { Name("_ADR", 0u32); }
                    Device("PDRC") {
                        Name("_HID", EisaId("PNP0C02"));
                        Name("_UID", 1u32);
                        Name("_CRS", ResourceTemplate {
                            Memory32Fixed(ReadWrite, #{dword rcba}, 0x4000u32);
                            Memory32Fixed(ReadWrite, #{dword mchbar}, 0x4000u32);
                            Memory32Fixed(ReadWrite, #{dword dmibar}, 0x1000u32);
                            Memory32Fixed(ReadWrite, #{dword epbar}, 0x1000u32);
                            Memory32Fixed(ReadWrite, #{dword ecam_base}, #{dword ecam_size});
                            Memory32Fixed(ReadWrite, 0xFED20000u32, 0x00020000u32);
                            Memory32Fixed(ReadWrite, 0xFED40000u32, 0x00005000u32);
                            Memory32Fixed(ReadWrite, 0xFED45000u32, 0x0004B000u32);
                        });
                    }
                    Name("MCRS", ResourceTemplate {
                        WordBusNumber(0x0000u16, #{word bus_max});
                        DWordIO(0x0000u32, 0x0CF7u32);
                        IO(0x0CF8u16, 0x0CF8u16, 0x01u8, 0x08u8);
                        DWordIO(0x0D00u32, 0xFFFFu32);
                        DWordMemory(Cacheable, ReadWrite, 0x000A0000u32, 0x000BFFFFu32);
                        DWordMemory(Cacheable, ReadWrite, 0x000C0000u32, 0x000FFFFFu32);
                        DWordMemory(Cacheable, ReadWrite, #{dword tolud}, 0xFEBFFFFFu32);
                        QWordMemory(Cacheable, ReadWrite, #{qword high_base}, #{qword high_limit});
                        DWordMemory(Cacheable, ReadWrite, 0xFED40000u32, 0xFED44FFFu32);
                    });
                    Method("_CRS", 0, Serialized) {
                        Return(MCRS);
                    }
                    Method("_OSC", 4, NotSerialized) {
                        If (Arg0 == ToUUID("33DB4D5B-1FF7-401C-9657-7441C03DD766")) {
                            // Let the OS control every PCIe feature.
                            Return(Arg3);
                        }
                        CreateDwordField(Arg3, 0u32, "CDW1");
                        CDW1 = CDW1 | 4u32;
                        Return(Arg3);
                    }
                }
            }
            .into();
            aml.extend(
                fstart_acpi::aml_linker::scope_vec(
                    "\\_SB_.PCI0",
                    &crate::igd::gfx0_acpi_node(gttmmio),
                )
                .expect("GM45 GFX0 emission failed"),
            );
            // Root PPKG is consumed by the ThinkPad thermal zones. Keep the
            // CPU devices in the same root scope as the other Core 2 boards.
            #[cfg(target_os = "none")]
            let count = usize::from(fstart_arch::x86::mp::online_cpus());
            #[cfg(not(target_os = "none"))]
            let count = 2;
            let cpus = crate::cpu::core2_aml::cpu_devices_aml(count, &config.cstates);
            aml.extend(
                fstart_acpi::aml_linker::scope_vec("\\", &cpus).expect("required CPU AML scope"),
            );
            aml
        }

        /// MCFG for the PCIEXBAR window and coreboot `gm45/acpi.c`'s DMAR, matching
        /// the remapping bases `post_dram_iommu()` programmed.
        fn extra_tables(&self, config: &Self::Config) -> Vec<Vec<u8>> {
            let mut mcfg = fstart_acpi::mcfg::MCFG::new(
                fstart_acpi::OEM_ID,
                fstart_acpi::OEM_TABLE_ID,
                fstart_acpi::OEM_REVISION,
            );
            mcfg.add_ecam(config.ecam_base, 0, 0, (config.ecam_buses - 1) as u8);
            let mut mcfg_bytes = Vec::new();
            fstart_acpi::Aml::to_aml_bytes(&mcfg, &mut mcfg_bytes);
            if !self.dram_ready {
                return alloc::vec![mcfg_bytes];
            }
            use fstart_acpi::dmar::{Drhd, dmar};
            let revision = ecam::EcamDevice::new(0, 0, 0).read8(0x08);
            let me_active = self.hostbridge().deven.get() & (1 << 6) != 0;
            let units = [
                Some(Drhd {
                    include_pci_all: false,
                    segment: 0,
                    base: 0xfed9_0000,
                    endpoints: &[(0, 0x1b, 0)],
                }),
                // B2 leaves the graphics unit disabled (post_dram_iommu).
                (revision != 6 && self.igd_enabled()).then_some(Drhd {
                    include_pci_all: false,
                    segment: 0,
                    base: 0xfed9_1000,
                    endpoints: &[(0, 2, 0), (0, 2, 1)],
                }),
                me_active.then_some(Drhd {
                    include_pci_all: false,
                    segment: 0,
                    base: 0xfed9_2000,
                    endpoints: &[(0, 3, 0), (0, 3, 1), (0, 3, 2), (0, 3, 3)],
                }),
                Some(Drhd {
                    include_pci_all: true,
                    segment: 0,
                    base: 0xfed9_3000,
                    endpoints: &[],
                }),
            ];
            let units: Vec<Drhd<'_>> = units.into_iter().flatten().collect();
            let width = (phys_address_bits() - 1) as u8;
            alloc::vec![mcfg_bytes, dmar(width, &units)]
        }
    }

    /// CPUID physical address width; 36 bits (Penryn) on hosts.
    fn phys_address_bits() -> u32 {
        #[cfg(target_os = "none")]
        {
            let (max, _, _, _) = fstart_arch::x86::cpuid(0x8000_0000);
            if max >= 0x8000_0008 {
                return fstart_arch::x86::cpuid(0x8000_0008).0 & 0xff;
            }
        }
        36
    }
}

#[cfg(all(test, feature = "acpi", feature = "ich9"))]
mod acpi_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gm45_reserved_top_tracks_gms_ggms_and_tseg() {
        // 32 MiB UMA + 2 MiB GTT + 8 MiB TSEG.
        assert_eq!(
            IntelGm45::reserved_memory((5 << 4) | (3 << 8), 5),
            Some(42 << 20)
        );
        assert_eq!(IntelGm45::reserved_memory(2, 3), Some(2 << 20));
        assert_eq!(IntelGm45::reserved_memory(15 << 4, 1), None);
        assert_eq!(IntelGm45::reserved_memory(2, 7), None);
    }
    #[test]
    fn gm45_map_boundaries_fail_closed() {
        let mib = 1 << 20;
        // 128 MiB TOM, 128 MiB TOLUD, 2 MiB TSEG: usable DRAM ends at 126 MiB.
        assert_eq!(
            IntelGm45::checked_memory_map(128 * mib as u64, 128 * mib, 128 * mib as u64, 2, 3),
            Ok((126 * mib, 128 * mib, 128 * mib as u64))
        );
        assert!(
            IntelGm45::checked_memory_map(64 * mib as u64, 128 * mib, 64 * mib as u64, 2, 3)
                .is_err()
        );
        assert!(
            IntelGm45::checked_memory_map(128 * mib as u64, mib, 128 * mib as u64, 2, 3).is_err()
        );
        assert!(
            IntelGm45::checked_memory_map(128 * mib as u64, 2 * mib, 128 * mib as u64, 2, 3)
                .is_err()
        );
        assert!(
            IntelGm45::checked_memory_map(
                128 * mib as u64,
                128 * mib,
                128 * mib as u64,
                15 << 4,
                3
            )
            .is_err()
        );
    }
    #[test]
    fn gm45_invalid_ecam_config_fails_before_hardware_access() {
        use crate::IntelNorthbridgeDriver;
        static INVALID: IntelGm45Config = IntelGm45Config {
            ecam_base: 0,
            ..IntelGm45Config::new()
        };
        assert!(matches!(
            IntelGm45::new_from_config(&INVALID),
            Err(ServiceError::HardwareError)
        ));
    }
    #[test]
    fn gm45_untrained_memory_fails_closed() {
        use crate::IntelNorthbridgeDriver;
        static CONFIG: IntelGm45Config = IntelGm45Config::new();
        let mut driver = IntelGm45::new_from_config(&CONFIG).unwrap();
        // dram_init touches live chipset registers; this host-side test may
        // only exercise pre-training APIs which must not touch stale RAM.
        assert_eq!(driver.detected_size_bytes(), 0);
        assert!(matches!(
            driver.total_ram_bytes(),
            Err(ServiceError::NotInitialized)
        ));
        let mut entries = [E820Entry::zeroed(); 8];
        assert!(matches!(
            driver.detect_memory(&mut entries),
            Err(ServiceError::NotInitialized)
        ));
        assert!(matches!(
            driver.memory_test(),
            Err(ServiceError::NotInitialized)
        ));
        assert_eq!(driver.boot_path, crate::BootPath::Normal);
        assert!(matches!(
            driver.dram_init(),
            Err(ServiceError::NotInitialized)
        ));
        assert!(driver.bootstrap_windows.is_none());
        assert!(
            driver
                .set_bootstrap_windows((16 << 20, 16 << 20), (64 << 20, 80 << 20))
                .is_err()
        );
        assert!(driver.bootstrap_windows.is_none());
        assert!(
            driver
                .set_bootstrap_windows((1u64 << 32, (1u64 << 32) + 4096), (64 << 20, 80 << 20))
                .is_err()
        );
        assert!(driver.bootstrap_windows.is_none());
        assert!(
            driver
                .set_bootstrap_windows((16 << 20, 17 << 20), (64 << 20, 80 << 20))
                .is_ok()
        );
        driver.set_boot_path(crate::BootPath::S3Resume);
        assert_eq!(driver.boot_path, crate::BootPath::S3Resume);
        assert!(matches!(
            driver.dram_init(),
            Err(ServiceError::NotInitialized)
        ));
        assert!(!driver.dram_ready);
        assert_eq!(driver.graphics_policy(), Gm45GraphicsPolicy::Integrated);
        driver.set_graphics_policy(Gm45GraphicsPolicy::Discrete);
        assert_eq!(driver.graphics_policy(), Gm45GraphicsPolicy::Discrete);
    }
    #[test]
    fn gm45_post_dram_rejects_peg_without_board_slot_policy() {
        use crate::IntelNorthbridgeDriver;
        static CONFIG: IntelGm45Config = IntelGm45Config::new();
        let mut driver = IntelGm45::new_from_config(&CONFIG).unwrap();
        driver.trained_mode = Some((6, 800, 400));
        for policy in [Gm45GraphicsPolicy::Discrete, Gm45GraphicsPolicy::Hybrid] {
            driver.set_graphics_policy(policy);
            assert!(matches!(
                driver.early_post_dram_init(),
                Err(ServiceError::InvalidParam)
            ));
        }
    }
    #[test]
    fn gm45_graphics_policy_does_not_select_me_functions() {
        assert_eq!(
            Gm45GraphicsPolicy::Integrated.deven_bits(),
            DEVEN_IGD | DEVEN_IGD_SECONDARY
        );
        assert_eq!(Gm45GraphicsPolicy::Discrete.deven_bits(), DEVEN_PEG);
        assert_eq!(
            Gm45GraphicsPolicy::Hybrid.deven_bits(),
            DEVEN_IGD | DEVEN_IGD_SECONDARY | DEVEN_PEG
        );
    }
}
