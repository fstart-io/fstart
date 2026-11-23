//! Haswell System Agent northbridge: ECAM, MCHBAR/DMIBAR/EPBAR, native DDR3
//! bring-up, memory map, SMRAM, VT-d, PEG, and IGD orchestration.
//!
//! All addresses in this module are bytes; the native training API uses MiB.
//! Southbridge resources and ME callbacks are injected via [`HaswellPch`] so
//! this northbridge driver stays decoupled from any southbridge crate module.

use super::{
    DMIBAR_SIZE, DmiBar, EPBAR_SIZE, EpBar, HaswellIgdConfig, MCHBAR_SIZE, MchBar,
    raminit::{
        self, Board, BootState, Workspace, configure_mc::Config, ddr3::ResetPin,
        memory_map::Reservations, spd::Slot,
    },
    registers::PCI_CAPID0_A,
};
use crate::{
    BootPath, IntelNorthbridgeDriver, SpdTraining, igd,
    southbridge::{lpc, rcba_window},
};
use fstart_core::{
    memory_info::MemoryInfo,
    mmio::MmioWindow,
    services::{
        FramebufferInfo, MemoryInit, ServiceError,
        memory_detect::{E820Entry, E820Kind, MemoryDetector, build_pc_compatible_e820},
    },
};
use fstart_pci::{
    PciEcamError,
    PciRole::{Fixed, Own},
    PciRoot, PciRootInfo,
    ecam::{self, EcamDevice},
};

/// PCI Express Enhanced Configuration Access Mechanism base (64 MiB, buses 0..63).
pub const ECAM: u64 = 0xf000_0000;
pub(crate) const MCH: u32 = 0xfed1_0000;
pub(crate) const DMI: u32 = 0xfed1_8000;
pub(crate) const EP: u32 = 0xfed1_9000;
const VTD_GFX_BASE: u32 = 0xfed9_0000;
const VTD_ALL_BASE: u32 = 0xfed9_1000;
const VTD_BAR_SIZE: u32 = 0x1000;

/// DIMM slot wiring across channels 0..2 and slots 0..2.
pub type Slots = [[Slot<'static>; 2]; 2];

/// Fixed PCH I/O and MMIO apertures owned by the companion southbridge.
///
/// Provided by the platform composition layer so Haswell never imports
/// southbridge-specific modules directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PchResources {
    /// Root Complex Base Address (`RCBA`, 16 KiB).
    pub rcba_base: u32,
    /// ACPI PM I/O base (`PMBASE`).
    pub pm_base: u16,
    /// ACPI PM I/O aperture length in bytes.
    pub pm_size: u16,
    /// GPIO I/O base (`GPIOBASE`).
    pub gpio_base: u16,
    /// GPIO I/O aperture length in bytes.
    pub gpio_size: u16,
    /// True when the GPIO window is fixed (Lynx Point-LP 1 KiB I/O window).
    pub gpio_fixed: bool,
}

impl PchResources {
    /// Standard Lynx Point-LP (ULT/ULX) apertures.
    pub const LP: Self = Self {
        rcba_base: 0xfed1_c000,
        pm_base: 0x1000,
        pm_size: 256,
        gpio_base: 0x1400,
        gpio_size: 1024,
        gpio_fixed: true,
    };

    /// Standard Lynx Point-H (Traditional desktop/mobile) apertures.
    pub const TRADITIONAL: Self = Self {
        rcba_base: 0xfed1_c000,
        pm_base: 0x500,
        pm_size: 256,
        gpio_base: 0x480,
        gpio_size: 128,
        gpio_fixed: false,
    };
}

fn no_me_uma() -> Result<u32, ServiceError> {
    Ok(0)
}

fn no_me_replaced() -> bool {
    false
}

fn no_me_done(_status: u32, _meseg_base_mib: u16) -> Result<(), ServiceError> {
    Ok(())
}

/// Companion southbridge resources and ME9 handshake callbacks injected into
/// [`HaswellConfig`].
#[derive(Clone, Copy, Debug)]
pub struct HaswellPch {
    /// Fixed PCH RCBA/PM/GPIO decode windows.
    pub resources: PchResources,
    /// Query pre-memory ME UMA size in MiB (zero when ME requests no UMA).
    pub me_uma: fn() -> Result<u32, ServiceError>,
    /// Query whether ME detected a CPU replacement since the last boot.
    pub me_replaced: fn() -> bool,
    /// Send DRAM Init Done (DID) to ME with `MESEG_BASE[35:20]` in MiB.
    pub me_done: fn(status: u32, meseg_base_mib: u16) -> Result<(), ServiceError>,
}

impl HaswellPch {
    /// Default PCH apertures for `ult` with no-op ME stubs (suitable for tests
    /// and ME-less configurations; platform code wires real ME callbacks).
    #[must_use]
    pub const fn default_for_ult(ult: bool) -> Self {
        Self {
            resources: if ult {
                PchResources::LP
            } else {
                PchResources::TRADITIONAL
            },
            me_uma: no_me_uma,
            me_replaced: no_me_replaced,
            me_done: no_me_done,
        }
    }
}

/// Static Haswell northbridge configuration.
///
/// `select_spd` is evaluated only in the bootblock, after board EC and pad
/// setup. The training journal's outer identity uses the static wiring; the
/// inner native frame also authenticates the selected population,
/// compensation, and SPD bytes.
#[derive(Debug, Clone, Copy)]
pub struct HaswellConfig {
    pub memory: Config,
    pub slots: Slots,
    pub select_spd: Option<fn() -> Result<Slots, ServiceError>>,
    pub tseg_mib: u32,
    pub dpr_mib: u32,
    pub igd_enabled: bool,
    pub igd: HaswellIgdConfig,
    pub minihd: bool,
    /// Keep fuse-enabled SA functions 4 (thermal) and 7 (chap/telemetry).
    pub device4: bool,
    pub device7: bool,
    pub peg: [Option<super::Peg>; 3],
    pub lpc_generic_io: &'static [lpc::LpcGenericIoDecode],
    pub pch: HaswellPch,
    pub vr: fstart_arch::x86::cpu::intel::haswell::VrConfig,
    pub tcc_offset: u8,
}

impl HaswellConfig {
    /// Construct default Haswell northbridge configuration for ULT (`true`) or
    /// Traditional (`false`) silicon.
    #[must_use]
    pub const fn new(ult: bool) -> Self {
        Self {
            memory: Config {
                ult,
                dq_pins_interleaved: false,
                double_refresh: false,
                rcomp: None,
            },
            slots: [[Slot::Empty; 2]; 2],
            select_spd: None,
            tseg_mib: 8,
            dpr_mib: 0,
            igd_enabled: true,
            igd: HaswellIgdConfig::new(),
            minihd: true,
            device4: false,
            device7: false,
            peg: if ult {
                [None; 3]
            } else {
                [Some(super::Peg::DEFAULT); 3]
            },
            lpc_generic_io: &[],
            pch: HaswellPch::default_for_ult(ult),
            vr: fstart_arch::x86::cpu::intel::haswell::VrConfig {
                min_vid: 0,
                slow_ramp: None,
            },
            tcc_offset: 0,
        }
    }
}

impl crate::IntelEcamConfig for HaswellConfig {
    fn ecam_base(&self) -> u64 {
        ECAM
    }
}

/// Haswell System Agent northbridge driver.
pub struct Haswell {
    config: &'static HaswellConfig,
    path: BootPath,
    training: Option<(BootState, u32)>,
    info: Option<MemoryInfo>,
    map: Option<Map>,
    windows: [(u64, u64); 2],
    igd_bars: Option<igd::IgdBars>,
    display: igd::IgdDisplay,
}

#[derive(Clone, Copy, Debug)]
struct Map {
    tom: u64,
    touud: u64,
    tolud: u32,
    bdsm: u32,
    tseg: u32,
    gtt: u32,
    usable: u32,
    me_base: u64,
}

impl Map {
    #[allow(clippy::too_many_arguments)]
    fn decode(
        tom: u64,
        touud: u64,
        tolud: u32,
        bdsm: u32,
        tseg: u32,
        gtt: u32,
        dpr: u32,
        me_base: u64,
    ) -> Result<Self, ServiceError> {
        // EPM controls DMA enforcement, not ownership of the reservation.
        let dpr_size = ((dpr >> 4) & 255) << 20;
        let usable = tseg
            .checked_sub(dpr_size)
            .ok_or(ServiceError::HardwareError)?;
        if usable <= 1 << 20
            || tseg >= gtt
            || gtt > bdsm
            || bdsm > tolud
            || u64::from(tolud) > tom
            || touud < u64::from(tolud)
            || tom > 1 << 39
            || touud > 1 << 39
        {
            return Err(ServiceError::HardwareError);
        }
        Ok(Self {
            tom,
            touud,
            tolud,
            bdsm,
            tseg,
            gtt,
            usable,
            me_base,
        })
    }

    fn pci_base(self) -> u64 {
        if u64::from(self.tolud) == self.me_base {
            self.tom
        } else {
            self.tolud.into()
        }
    }

    /// Reserved RAM uses WB up to BGSM; graphics/GTT stolen RAM above it is UC.
    fn reserved_wb(self) -> (u64, u64) {
        (self.usable.into(), (self.gtt - self.usable).into())
    }

    fn contains_bootstrap(self, (lo, hi): (u64, u64)) -> bool {
        lo < hi && hi <= u64::from(self.usable)
    }

    fn read() -> Result<Self, ServiceError> {
        let hb = host();
        let addr = |off| hb.read32(off) & 0xfff0_0000;
        let wide = |off| (u64::from(hb.read32(off + 4)) << 32) | u64::from(addr(off));
        Self::decode(
            wide(0xa0),
            wide(0xa8),
            addr(0xbc),
            addr(0xb0),
            addr(0xb8),
            addr(0xb4),
            hb.read32(0x5c),
            wide(0x70),
        )
    }
}

/// Check whether `cpuid` (EAX from `CPUID.01h`) is a supported Haswell stepping
/// matching the configured `ult` topology.
///
/// Consistent with `HaswellCpu::id_table()` and `raminit::testing_io::Stepping`:
/// accepts both B0 (`0x306c2`, `0x40650`, `0x40660`) and C0 (`0x306c3`,
/// `0x40651`, `0x40661`) steppings.
#[must_use]
pub(crate) const fn is_supported_cpu(cpuid: u32, ult: bool) -> bool {
    matches!(
        (cpuid, ult),
        (0x306c2 | 0x306c3 | 0x40660 | 0x40661, false) | (0x40650 | 0x40651, true)
    )
}

/// True when `CAPID0_A` (`00:00.0 + 0xe4`) reports VT-d enabled (`VTDD == 0`).
#[must_use]
pub(crate) fn vtd_capable_from_capid0_a(capid0_a: u32) -> bool {
    !PCI_CAPID0_A::VTDD.is_set(capid0_a)
}

/// Extract `MESEG_BASE[35:20]` in 1 MiB units from `00:00.0 + 0x70..0x78`.
#[must_use]
pub(crate) const fn meseg_base_mib(low: u32, high: u32) -> u16 {
    (((low >> 20) | ((high & 0xf) << 12)) & 0xffff) as u16
}

fn host() -> EcamDevice {
    EcamDevice::new(0, 0, 0)
}

fn gfx() -> EcamDevice {
    EcamDevice::new(0, 2, 0)
}

pub(crate) fn mch() -> MchBar {
    // SAFETY: MCHBAR is enabled at fixed `MCH` during `early_init`.
    unsafe { MchBar::new(MCH as usize) }
}

pub(crate) fn dmi() -> DmiBar {
    // SAFETY: DMIBAR is enabled at fixed `DMI` during `early_init`.
    unsafe { DmiBar::new(DMI as usize) }
}

pub(crate) fn ep() -> EpBar {
    // SAFETY: EPBAR is enabled at fixed `EP` during `early_init`.
    unsafe { EpBar::new(EP as usize) }
}

impl Haswell {
    fn board(&self, slots: Slots, me: u32) -> Board<'static> {
        Board {
            config: self.config.memory,
            reservations: Reservations {
                tseg: self.config.tseg_mib,
                dpr: self.config.dpr_mib,
                me,
                igd_primary: self.config.igd_enabled,
            },
            slots,
        }
    }

    fn setup(&self) {
        let h = host();
        for (off, base) in [(0x40, EP), (0x48, MCH), (0x68, DMI)] {
            h.write32(off + 4, 0);
            h.write32(off, base | 1);
        }
        let m = mch();
        m.write32(0x5408, 0xfed8_0001);
        m.write32(0x5420, 0xfed8_4001);
        h.write8(0x80, 0x30);
        for off in 0x81..=0x86 {
            h.write8(off, 0x33);
        }
        if !self.config.igd_enabled {
            h.and32(0x54, !((1 << 4) | (1 << 5)));
        }
        if vtd_capable_from_capid0_a(h.read32(0xe4)) {
            for (off, base, policy) in [
                (0x5400, VTD_GFX_BASE, 0x8210_0000),
                (
                    0x5410,
                    VTD_ALL_BASE,
                    0x800a_5003 | if gfx().is_present() { 0 } else { 1 << 25 },
                ),
            ] {
                m.write32(off + 4, 0);
                m.write32(off, base | 1);
                // SAFETY: VT-d engine BAR was just enabled at `base` (4 KiB aperture).
                let vtd = unsafe { MmioWindow::new(base as usize, VTD_BAR_SIZE) };
                if off == 0x5410 {
                    vtd.clrsetbits32(0xf04, 15 << 15, 1 << 15);
                }
                vtd.write32(0xff0, policy);
            }
        }
        if !self.config.memory.ult {
            for func in 0..3 {
                if h.read32(0x54) & (8 >> func) != 0 {
                    EcamDevice::new(0, 1, func).modify32(0xc24, !(1 << 16), 1 << 5);
                }
            }
        }
        let enabled = h.read32(0x54) & 16 != 0;
        h.write16(
            0x50,
            (h.read16(0x50) & !0x3fa) | if enabled { 0x208 } else { 2 },
        );
        if enabled {
            gfx().and8_or8(0x62, !6, 2);
        }
        m.setbits32(0x5f00, 3 << 9);
        m.setbits32(0x5418, 3 << 4);
    }

    fn recover_map(&mut self) -> Result<(), ServiceError> {
        self.map = Some(Map::read()?);
        Ok(())
    }

    fn vtd_capable(&self) -> bool {
        #[cfg(target_os = "none")]
        {
            ecam::base() != 0 && vtd_capable_from_capid0_a(host().read32(0xe4))
        }
        #[cfg(not(target_os = "none"))]
        {
            if ecam::base() != 0 {
                vtd_capable_from_capid0_a(host().read32(0xe4))
            } else {
                true
            }
        }
    }

    /// Build the [`PciRoot`] descriptor with explicit VT-d capability gating.
    ///
    /// When `vtd_capable` is false (`CAPID0_A::VTDD` set), the `0xfed9_0000`
    /// VT-d MMIO aperture is omitted from both PCI reservations and ACPI
    /// `PDRC`.
    pub(crate) fn pci_root_for(&self, vtd_capable: bool) -> Result<PciRoot, PciEcamError> {
        let pch = self.config.pch.resources;
        let gpio_window = if pch.gpio_fixed {
            Fixed.io(u64::from(pch.gpio_base), u64::from(pch.gpio_size))
        } else {
            Own.io(u64::from(pch.gpio_base), u64::from(pch.gpio_size))
        };
        let chipset = [
            Own.mmio(ECAM, 64 << 20),
            Fixed.io(u64::from(pch.pm_base), u64::from(pch.pm_size)),
            gpio_window,
            Own.mmio(MCH.into(), MCHBAR_SIZE as u64),
            Own.mmio(DMI.into(), DMIBAR_SIZE as u64),
            Own.mmio(EP.into(), EPBAR_SIZE as u64),
            Own.mmio(u64::from(pch.rcba_base), 0x4000),
            Own.mmio(0xfed2_0000, 0x20000),
            Own.mmio(0xfed4_5000, 0x4b000),
        ];
        let vtd_window = vtd_capable.then(|| Own.mmio(u64::from(VTD_GFX_BASE), 0x2000));
        PciRoot::new(
            PciRootInfo {
                segment: 0,
                ecam_base: ECAM,
                bus_start: 0,
                bus_end: 63,
            },
            crate::host_bridge::forwarded_below(self.map.map(Map::pci_base), None, ECAM)
                .into_iter()
                .chain(chipset)
                .chain(vtd_window)
                .chain(
                    self.config
                        .lpc_generic_io
                        .iter()
                        .map(|w| Fixed.io(u64::from(w.base), u64::from(w.size))),
                ),
        )
    }
}

impl IntelNorthbridgeDriver for Haswell {
    type Config = HaswellConfig;
    type Options = ();
    const ACPI_PCI_ROOT_NAME: &'static str = "PCI0";
    const ACPI_IGD_NAME: &'static str = "GFX0";
    const KNOBS: &'static [fstart_options::Knob<()>] = &[];

    fn board_options(_: &Self::Config) {}
    fn options(&self) {}
    fn set_options(&mut self, _: ()) {}

    fn config(&self) -> &'static Self::Config {
        self.config
    }

    fn new_from_config(config: &'static HaswellConfig) -> Result<Self, ServiceError> {
        let levels = config.igd.brightness_levels;
        if (!levels.is_empty()
            && (levels.first() != Some(&0)
                || levels.last() != Some(&100)
                || levels.windows(2).any(|pair| pair[0] >= pair[1])))
            || !config.tseg_mib.is_power_of_two()
            || config.tseg_mib < 8
            || config.dpr_mib > 255
            || config
                .peg
                .iter()
                .flatten()
                .any(|p| p.physical_slot > 8191 || p.power_limit_scale > 3 || config.memory.ult)
            || config.vr.slow_ramp.is_some_and(|v| v > 3)
            || config.tcc_offset > 15
        {
            return Err(ServiceError::InvalidParam);
        }
        Ok(Self {
            config,
            path: BootPath::Normal,
            training: None,
            info: None,
            map: None,
            windows: [(0, 0); 2],
            igd_bars: None,
            display: igd::IgdDisplay::new(),
        })
    }

    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        // SAFETY: PCI CF8/CFC enables PCIEXBAR at `ECAM` (64 MiB) before ECAM init.
        unsafe {
            fstart_pci::cf8::write32(0, 0, 0, 0x64, 0);
            fstart_pci::cf8::write32(0, 0, 0, 0x60, ECAM as u32 | 5);
        }
        ecam::init(ECAM as usize);
        Ok(())
    }

    fn early_init(&mut self) -> Result<(), ServiceError> {
        let cpu = fstart_arch::x86::cpuid(1).0;
        if !is_supported_cpu(cpu, self.config.memory.ult) {
            return Err(ServiceError::NotSupported);
        }
        // Raise the BSP ratio to max non-turbo before memory training.
        // SAFETY: validated Haswell CPU; MSRs `PLATFORM_INFO` and `PERF_CTL` exist.
        unsafe { fstart_arch::x86::cpu::intel::haswell::set_max_freq() };
        self.setup();
        Ok(())
    }

    fn ramstage_early_init(&mut self) -> Result<(), ServiceError> {
        ecam::init(ECAM as usize);
        self.recover_map()
    }

    fn stage_local_init(&mut self) -> Result<(), ServiceError> {
        self.ramstage_early_init()?;
        if self.config.igd_enabled && gfx().is_present() {
            let igd = gfx();
            self.igd_bars = igd::assigned_bars(&igd, false);
            if let Some(bars) = self.igd_bars {
                // Enable memory decode and bus master before touching GTTMMADR.
                igd.or16(0x04, (1 << 1) | (1 << 2));
                super::igd::init_display_engine(
                    bars.gtt_mmio,
                    self.config.memory.ult,
                    self.config.igd.panel,
                    igd.device_id(),
                );
            } else {
                fstart_log::warn!("Haswell: IGD windows unassigned, skipping display engine");
            }
        }
        Ok(())
    }

    fn chip_init(&mut self) -> Result<(), ServiceError> {
        if !self.config.memory.ult {
            super::peg::setup(&self.config.peg);
        }
        host().and32(
            0x54,
            !((u32::from(!self.config.device4) << 7)
                | (u32::from(!self.config.device7) << 14)
                | (u32::from(!self.config.minihd) << 5)),
        );
        Ok(())
    }

    fn bootstrap_ram_end(&self) -> Result<u64, ServiceError> {
        Ok(u64::from(
            self.map.ok_or(ServiceError::NotInitialized)?.usable,
        ))
    }

    fn set_bootstrap_windows(&mut self, a: (u64, u64), b: (u64, u64)) -> Result<(), ServiceError> {
        self.windows = [a, b];
        Ok(())
    }

    fn set_boot_path(&mut self, path: BootPath) {
        self.path = path;
    }

    fn detect_warm_reset(&self) -> bool {
        lpc::device().read8(0xa2) & 0xa0 == 0xa0
    }

    fn prepare_training(&mut self) -> Result<(), ServiceError> {
        let pm = lpc::device().read16(0xa2);
        let resume = self.path == BootPath::S3Resume;
        let size = (self.config.pch.me_uma)()?;
        let boot = BootState::from_pmcon(
            pm,
            resume,
            !resume && pm & 0xa0 != 0xa0 && (self.config.pch.me_replaced)(),
        );
        self.training = Some((boot, size));
        Ok(())
    }

    fn prepare_resume_reset(&self) {
        lpc::device().and8(0xa2, !0x80);
    }

    fn supports_s3_replay(&self) -> bool {
        true
    }

    fn training_identity(&self) -> Option<[u8; 32]> {
        Some(
            self.board(
                self.config.slots,
                match self.training {
                    Some((_, size)) => size,
                    None => (self.config.pch.me_uma)().ok()?,
                },
            )
            .cache_key(host(), fstart_arch::x86::cpuid(1).0),
        )
    }

    fn memory_info(&self) -> Option<MemoryInfo> {
        self.info
    }

    fn pci_root(&self) -> Result<PciRoot, PciEcamError> {
        self.pci_root_for(self.vtd_capable())
    }

    fn post_dram_init(&mut self) -> Result<(), ServiceError> {
        let rcba_base = self.config.pch.resources.rcba_base;
        // Coreboot `pre_mp_init`: VR configuration, ULT C9/C10 voltage, 24 MHz
        // BCLK calibration, and PCH power sharing must run before MP init,
        // before `BIOS_RESET_CPL`, and before PCH PM_SYNC is enabled.
        // SAFETY: Haswell VR MSRs are valid for this CPU family.
        unsafe { self.config.vr.apply(self.config.memory.ult) };
        if self.config.memory.ult {
            super::links::ult_power_sharing(rcba_base);
        }
        Ok(())
    }

    fn post_mp_init(&mut self) -> Result<(), ServiceError> {
        // Coreboot `northbridge_init`: runs strictly after `mp_init_cpus` has
        // programmed per-core C-state MSRs/IRTLs and SMM, and strictly before
        // PCH PM_SYNC / dynamic clock gating and IGD GT PM.
        let rcba_base = self.config.pch.resources.rcba_base;
        super::links::topology(self.config.memory.ult, rcba_base)?;
        let m = mch();
        m.clrsetbits8(0x5418, 7, 4);
        m.setbits32(0x5f00, 1);
        m.setbits8(0x5da8, 3);
        fstart_log::info!("Haswell: BIOS_RESET_CPL set");
        fstart_arch::x86::udelay(1000);
        // SAFETY: Haswell RAPL MSRs exist and MCHBAR is mapped.
        unsafe {
            fstart_arch::x86::cpu::intel::haswell::set_power_limits(|l| {
                m.write32(0x59a0, l as u32);
                m.write32(0x59a4, (l >> 32) as u32);
                let low = m.read32(0x58e0);
                u64::from(low) | (u64::from(m.read32(0x58e4)) << 32)
            });
        }
        Ok(())
    }

    fn finalize_init(&mut self) -> Result<(), ServiceError> {
        let h = host();
        // SAFETY: Haswell memory-configuration lock MSR exists.
        unsafe { fstart_arch::x86::cpu::intel::haswell::lock_memory_config() };
        h.or16(0x50, 1);
        h.or32(0x5c, 1);
        h.or32(0x78, 1 << 10);
        for off in [0x90, 0x98, 0xa0, 0xa8, 0xb0, 0xb4, 0xb8, 0xbc] {
            h.or32(off, 1);
        }
        let m = mch();
        for (off, set) in [
            (0x50fc, 0x8f),
            (0x5500, 1),
            (0x5880, 32),
            (0x7000, 1 << 31),
            (0x77fc, 1),
            (0x7ffc, 1),
            (0x6800, 1 << 31),
            (0x6020, 1),
            (0x63fc, 1),
            (0x6030, 0),
            (0x6034, 0),
            (0x6008, 0),
        ] {
            m.setbits32(off, set);
        }
        Ok(())
    }

    fn post_verify_init(&mut self, mem: igd::OpRegionMemory<'_>) -> Result<(), ServiceError> {
        if !self.config.igd_enabled || !gfx().is_present() {
            return Ok(());
        }
        let Some(bars) = self.igd_bars else {
            return Ok(());
        };
        let igd = gfx();
        let vbt = self.config.igd.vbt.as_ref().and_then(|vbt_src| {
            igd::publish_opregion_version(
                &igd,
                igd::OpRegionSci::Swsci,
                vbt_src,
                mem,
                igd::OpRegionVersion::V20,
            )
        });
        if self.config.igd.display.is_some()
            && let Some(map) = self.map
        {
            if igd.read16(0x50) & (1 << 1) != 0 {
                fstart_log::info!(
                    "Haswell: IGD VGA decode disabled (GGC.IVD), skipping native display init"
                );
            } else {
                let addresses = igd::IgdAddresses {
                    pci_bdf: crate::gma::types::PciAddress::new(0, 0, 2, 0),
                    gtt_mmio_base: bars.gtt_mmio,
                    gtt_mmio_size: super::igd::GTTMMADR_SIZE,
                    gtt_pte_base: None,
                    gmadr_base: Some(bars.gmadr),
                    gmadr_size: igd::gmadr_size_from_msac(igd.read8(0x62)),
                    stolen_base: u64::from(map.bdsm),
                    stolen_size: map.tolud.saturating_sub(map.bdsm),
                    gtt_size: map.bdsm.saturating_sub(map.gtt),
                    gcfgc: None,
                };
                let cpu = super::igd::cpu_variant(self.config.memory.ult, igd.device_id());
                self.display.initialize::<crate::gma::Haswell>(
                    cpu,
                    self.config.igd.display.as_ref(),
                    &addresses,
                    vbt,
                );
            }
        }
        if self.config.minihd {
            // Native eDP-only modeset drops the display power well; re-request
            // it so the Mini-HD audio codec responds to detection and verbs.
            super::igd::ensure_power_well(bars.gtt_mmio);
            super::igd::minihd();
        }
        Ok(())
    }

    fn resume_init(&mut self, address: Option<u32>) -> Result<(), ServiceError> {
        self.stage_local_init()?;
        if self.config.minihd && self.igd_bars.is_some() {
            super::igd::minihd();
        }
        if let Some(a) = address.filter(|_| self.config.igd_enabled && gfx().is_present()) {
            igd::register_opregion(&gfx(), igd::OpRegionSci::Swsci, a);
        }
        Ok(())
    }

    fn framebuffer_info(&self) -> Option<FramebufferInfo> {
        self.display.framebuffer_info()
    }
}

impl MemoryInit for Haswell {
    type Input<'a> = SpdTraining<'a>;
    type Output = Option<usize>;

    fn bring_up(&mut self, input: SpdTraining<'_>) -> Result<Option<usize>, ServiceError> {
        let resume = self.path == BootPath::S3Resume;
        let (boot, me) = self.training.ok_or(ServiceError::NotInitialized)?;
        if !resume && !boot.self_refresh {
            self.prepare_resume_reset();
        }
        let rcba_base = self.config.pch.resources.rcba_base;
        if !self.config.memory.ult {
            super::links::early_dmi(rcba_base)?;
        }
        let slots = match self.config.select_spd {
            Some(select) => select()?,
            None => self.config.slots,
        };
        let board = self.board(slots, me);
        let mut reset = |asserted| {
            // SAFETY: `rcba_base` is the mapped 16 KiB PCH RCBA configured by the platform.
            let rcba = unsafe { rcba_window(rcba_base as usize) };
            rcba.clrsetbits32(0x333c, 1 << 26, if asserted { 0 } else { 1 << 26 });
        };
        let pin = if board.config.ult {
            ResetPin::LynxPointLp(&mut reset)
        } else {
            ResetPin::SystemAgent
        };
        struct Car(core::cell::UnsafeCell<Workspace>);
        // SAFETY: only the successful one-shot TAKEN claimant accesses the cell.
        unsafe impl Sync for Car {}
        static WORK: Car = Car(core::cell::UnsafeCell::new(Workspace::EMPTY));
        static TAKEN: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
        if TAKEN.swap(true, core::sync::atomic::Ordering::Relaxed) {
            return Err(ServiceError::HardwareError);
        }
        // Exclusive BSP workspace; no reference escapes CAR.
        let result = unsafe {
            raminit::raminit(
                mch(),
                &board,
                boot,
                Some(input.smbus),
                pin,
                input.cached,
                &mut *WORK.0.get(),
            )
        };
        let h = host();
        let meseg = || meseg_base_mib(h.read32(0x70), h.read32(0x74));
        let trained = result.map_err(|e| {
            if me != 0 {
                let status = if e == raminit::Error::NoMemory { 1 } else { 2 };
                let _ = (self.config.pch.me_done)(status, meseg());
            }
            if e == raminit::Error::PllBusy {
                fstart_log::error!("Haswell: outstanding MPLL request; reset required");
                return ServiceError::Timeout;
            }
            ServiceError::HardwareError
        })?;
        if me != 0 {
            (self.config.pch.me_done)(if trained.memory_lost { 3 } else { 0 }, meseg())?;
        }
        let map = Map::read()?;
        if map.usable >> 20 != trained.map.dpr_base
            || map.tseg >> 20 != trained.map.tseg
            || map.gtt >> 20 != trained.map.gtt
            || map.bdsm >> 20 != trained.map.bdsm
            || map.tolud >> 20 != trained.map.tolud
            || map.touud >> 20 != u64::from(trained.map.touud)
            || !self
                .windows
                .iter()
                .all(|&window| map.contains_bootstrap(window))
        {
            return Err(ServiceError::HardwareError);
        }
        self.map = Some(map);
        self.info = Some(trained.memory_info());
        if resume || input.capture.is_empty() {
            return Ok(None);
        }
        let bytes = trained.save_data;
        input
            .capture
            .get_mut(..bytes.len())
            .ok_or(ServiceError::InvalidParam)?
            .copy_from_slice(bytes);
        Ok(Some(bytes.len()))
    }
}

impl MemoryDetector for Haswell {
    fn total_ram_bytes(&self) -> Result<u64, ServiceError> {
        Ok(self.map.ok_or(ServiceError::NotInitialized)?.tom)
    }

    fn detect_memory(&self, entries: &mut [E820Entry]) -> Result<usize, ServiceError> {
        let m = self.map.ok_or(ServiceError::NotInitialized)?;
        let mut n = build_pc_compatible_e820(entries, m.usable, m.touud, m.tolud)?;
        // Reserve Haswell fixed chipset MMIO apertures (ECAM, MCHBAR, DMIBAR,
        // EPBAR, PCH RCBA, EDRAMBAR, GDXCBAR, and VT-d when enabled) so both
        // E820 and EFI memory maps match coreboot `mc_read_resources()`.
        let fixed_mmio = [
            (ECAM, 64 << 20),
            (u64::from(MCH), MCHBAR_SIZE as u64),
            (u64::from(DMI), DMIBAR_SIZE as u64),
            (u64::from(EP), EPBAR_SIZE as u64),
            (u64::from(self.config.pch.resources.rcba_base), 0x4000),
            (0xfed8_0000, 0x4000),
            (0xfed8_4000, 0x1000),
        ];
        for &(base, size) in &fixed_mmio {
            if n >= entries.len() {
                return Err(ServiceError::HardwareError);
            }
            entries[n] = E820Entry::new(base, size, E820Kind::Reserved);
            n += 1;
        }
        if self.vtd_capable() {
            if n >= entries.len() {
                return Err(ServiceError::HardwareError);
            }
            entries[n] = E820Entry::new(u64::from(VTD_GFX_BASE), 0x2000, E820Kind::Reserved);
            n += 1;
        }
        entries[..n].sort_unstable_by_key(|e| e.addr);
        fstart_arch::x86::mtrr::set_ram_wb_ranges_from(
            entries[..n]
                .iter()
                .filter(|e| e.kind == E820Kind::Ram as u32)
                .map(|e| (e.addr, e.size))
                .chain([m.reserved_wb()]),
        )?;
        Ok(n)
    }
}

impl fstart_arch::x86::cpu::intel::smm::SmramControl for Haswell {
    fn tseg(&self) -> Option<(u64, u32)> {
        let m = self.map?;
        Some((m.tseg.into(), m.gtt - m.tseg))
    }

    fn smram_open(&self) -> bool {
        host().write8(0x88, crate::gmch::smram::open());
        crate::gmch::smram::is_open(host().read8(0x88))
    }

    fn smram_close(&self) {
        host().write8(0x88, crate::gmch::smram::closed());
    }

    fn smram_lock(&self) -> bool {
        host().write8(0x88, crate::gmch::smram::locked());
        crate::gmch::smram::is_locked(host().read8(0x88))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpr_reservation_is_independent_of_enforcement() {
        let decode = |dpr| {
            Map::decode(
                8 << 30,
                10 << 30,
                2114 << 20,
                2050 << 20,
                2040 << 20,
                2048 << 20,
                dpr,
                0,
            )
            .unwrap()
        };
        let trained = (2040 << 20) | (4 << 4);
        assert_eq!(decode(trained).usable, 2036 << 20);
        assert_eq!(decode(trained | 4).usable, 2036 << 20);
        assert_eq!(decode(2040 << 20).usable, 2040 << 20);
        let map = decode(trained);
        assert_eq!(map.reserved_wb(), (2036 << 20, 12 << 20));
        assert_eq!(map.pci_base(), u64::from(map.tolud));
        assert_eq!(
            Map {
                me_base: map.tolud.into(),
                ..map
            }
            .pci_base(),
            map.tom
        );
        assert!(map.contains_bootstrap((2035 << 20, 2036 << 20)));
        assert!(!map.contains_bootstrap((2035 << 20, (2036 << 20) + 1)));
        static CONFIG: HaswellConfig = HaswellConfig::new(true);
        let mut north = Haswell::new_from_config(&CONFIG).unwrap();
        north.map = Some(map);
        assert_eq!(north.bootstrap_ram_end(), Ok(2036 << 20));
        assert_eq!(north.total_ram_bytes(), Ok(8 << 30));
        let mut entries = [E820Entry::zeroed(); 32];
        let n = north.detect_memory(&mut entries).unwrap();
        assert!(
            entries[..n]
                .windows(2)
                .all(|w| w[0].addr + w[0].size <= w[1].addr)
        );
        assert!(entries[..n].iter().any(|e| {
            e.kind == E820Kind::Reserved as u32
                && e.addr == 2036 << 20
                && e.addr + e.size == 2114 << 20
        }));
        for (base, size) in [
            (ECAM, 64 << 20),
            (u64::from(MCH), MCHBAR_SIZE as u64),
            (u64::from(DMI), DMIBAR_SIZE as u64),
            (u64::from(EP), EPBAR_SIZE as u64),
            (u64::from(PchResources::LP.rcba_base), 0x4000),
            (0xfed8_0000, 0x4000),
            (0xfed8_4000, 0x1000),
            (u64::from(VTD_GFX_BASE), 0x2000),
        ] {
            assert!(
                entries[..n]
                    .iter()
                    .any(|e| e.kind == E820Kind::Reserved as u32
                        && e.addr == base
                        && e.size == size),
                "missing reserved fixed MMIO {base:#x} +{size:#x}"
            );
        }
    }

    #[test]
    fn supported_cpus_include_b0_and_c0_steppings() {
        for trad in [0x306c2, 0x306c3, 0x40660, 0x40661] {
            assert!(is_supported_cpu(trad, false));
            assert!(!is_supported_cpu(trad, true));
        }
        for ult in [0x40650, 0x40651] {
            assert!(is_supported_cpu(ult, true));
            assert!(!is_supported_cpu(ult, false));
        }
        assert!(!is_supported_cpu(0x306c1, false));
        assert!(!is_supported_cpu(0x306d4, true));
    }

    #[test]
    fn vtd_gating_omits_remapping_window_when_fused_off() {
        assert!(vtd_capable_from_capid0_a(0));
        assert!(!vtd_capable_from_capid0_a(1 << 23));

        static CONFIG: HaswellConfig = HaswellConfig::new(true);
        let north = Haswell::new_from_config(&CONFIG).unwrap();
        let with_vtd = north.pci_root_for(true).unwrap();
        let without_vtd = north.pci_root_for(false).unwrap();
        let has_vtd = |root: &PciRoot| {
            root.motherboard()
                .iter()
                .any(|w| w.base == u64::from(VTD_GFX_BASE) && w.size == 0x2000)
        };
        assert!(has_vtd(&with_vtd));
        assert!(!has_vtd(&without_vtd));
    }

    #[test]
    fn pch_resources_and_me_callbacks_are_decoupled() {
        static CONFIG: HaswellConfig = HaswellConfig {
            pch: HaswellPch {
                resources: PchResources {
                    rcba_base: 0xfed2_4000,
                    pm_base: 0x1800,
                    pm_size: 128,
                    gpio_base: 0x1900,
                    gpio_size: 256,
                    gpio_fixed: false,
                },
                ..HaswellPch::default_for_ult(true)
            },
            ..HaswellConfig::new(true)
        };
        let north = Haswell::new_from_config(&CONFIG).unwrap();
        let root = north.pci_root_for(false).unwrap();
        assert!(
            root.motherboard()
                .iter()
                .any(|w| w.base == 0xfed2_4000 && w.size == 0x4000)
        );
        // PMBASE is claimed by LPC LDRC (PNP0C02), so it is Fixed rather than
        // duplicated in host-bridge PDRC.
        assert!(!root.motherboard().iter().any(|w| w.base == 0x1800));
        assert!(
            root.motherboard()
                .iter()
                .any(|w| w.base == 0x1900 && w.size == 256)
        );
    }

    #[test]
    fn meseg_base_mib_decodes_36_bit_address() {
        // 0x0_fee0_0000 -> 0x0fee MiB; 0x3_f000_0000 -> 0x3f00 MiB.
        assert_eq!(meseg_base_mib(0xfee0_0000, 0), 0x0fee);
        assert_eq!(meseg_base_mib(0xf000_0000, 0x3), 0x3f00);
    }

    #[test]
    fn config_validation_rejects_bad_brightness_and_peg_on_ult() {
        static BAD_BRIGHTNESS: HaswellConfig = HaswellConfig {
            igd: HaswellIgdConfig {
                brightness_levels: &[0, 50, 50, 100],
                ..HaswellIgdConfig::new()
            },
            ..HaswellConfig::new(true)
        };
        assert_eq!(
            Haswell::new_from_config(&BAD_BRIGHTNESS).err(),
            Some(ServiceError::InvalidParam)
        );

        static BAD_PEG_ON_ULT: HaswellConfig = HaswellConfig {
            peg: [Some(super::super::Peg::DEFAULT), None, None],
            ..HaswellConfig::new(true)
        };
        assert_eq!(
            Haswell::new_from_config(&BAD_PEG_ON_ULT).err(),
            Some(ServiceError::InvalidParam)
        );
    }
}
