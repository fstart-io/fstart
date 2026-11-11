//! Intel Ironlake: the uncore of Arrandale/Clarkdale CPUs (integrated
//! memory controller, IGD and DMI), paired with a 5 Series PCH.
//!
//! A port of coreboot `northbridge/intel/ironlake`. The host bridge at
//! 00:00.0 keeps the GMCH-style BARs and memory map registers; the
//! QuickPath uncore functions on the top ECAM bus own PCIEXBAR, PAM, SMRAM
//! and the DRAM rules.

#[cfg(feature = "acpi")]
mod acpi;
mod igd;
mod raminit;

pub use igd::{IronlakeIgdConfig, PanelPort};

use crate::MmioBar;
use fstart_core::mmio::MmioReadWrite;
use fstart_core::services::ServiceError;
use fstart_core::services::memory_detect::{
    E820Entry, E820Kind, E820State, MemoryDetector, build_pc_compatible_e820,
};
use fstart_pci::{PciRootInfo, ecam, pci_type0_config};
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::register_bitfields;

/// Fixed uncore resources (coreboot Kconfig defaults).
pub const DEFAULT_MCHBAR: u64 = 0xfed1_0000;
pub const DEFAULT_DMIBAR: u64 = 0xfed1_8000;
pub const DEFAULT_EPBAR: u64 = 0xfed1_9000;
pub const DEFAULT_ECAM_BASE: u64 = 0xe000_0000;
/// The QuickPath uncore sits on the topmost bus PCIEXBAR decodes.
pub const DEFAULT_ECAM_BUSES: u16 = 256;
/// QuickPath non-core: bit 0 disables multicore, bit 1 quadcore, bit 8 HT.
const DESIRED_CORES: u16 = 0x80;

register_bitfields![u32,
    /// DEVEN.
    pub DEVEN [
        HOST OFFSET(0) NUMBITS(1) [],
        PEG10 OFFSET(1) NUMBITS(1) [],
        IGD OFFSET(3) NUMBITS(1) []
    ],
    /// PCIEXBAR in the QuickPath SAD (coreboot ironlake `bootblock.c`).
    pub PCIEXBAR [
        ENABLE OFFSET(0) NUMBITS(1) [],
        /// Ironlake's own encoding: 0 = 256, 6 = 128, 7 = 64 buses.
        LENGTH OFFSET(1) NUMBITS(3) [],
        BASE OFFSET(20) NUMBITS(12) []
    ]
];

pci_type0_config! {
    /// Ironlake host bridge (00:00.0).
    pub struct IronlakeHostBridgeConfig {
        (0x40 => pub epbar_lo: MmioReadWrite<u32>),
        (0x44 => pub epbar_hi: MmioReadWrite<u32>),
        (0x48 => pub mchbar_lo: MmioReadWrite<u32>),
        (0x4c => pub mchbar_hi: MmioReadWrite<u32>),
        (0x50 => _hb_rsvd0),
        (0x52 => pub ggc: MmioReadWrite<u16>),
        (0x54 => pub deven: MmioReadWrite<u32, DEVEN::Register>),
        (0x58 => _hb_rsvd1),
        (0x68 => pub dmibar_lo: MmioReadWrite<u32>),
        (0x6c => pub dmibar_hi: MmioReadWrite<u32>),
        (0x70 => _hb_rsvd2),
        (0x98 => pub remap_base: MmioReadWrite<u16>),
        (0x9a => pub remap_limit: MmioReadWrite<u16>),
        (0x9c => _hb_rsvd3),
        (0xa0 => pub tom: MmioReadWrite<u16>),
        (0xa2 => pub touud: MmioReadWrite<u16>),
        (0xa4 => pub igd_base: MmioReadWrite<u32>),
        (0xa8 => pub gtt_base: MmioReadWrite<u32>),
        (0xac => pub tseg: MmioReadWrite<u32>),
        (0xb0 => pub tolud: MmioReadWrite<u16>),
        (0xb2 => _hb_rsvd4),
        (0xe0 => pub capid0: [MmioReadWrite<u32>; 3]),
        (0xec => _hb_rsvd5),
        (0xf3 => pub did_override: MmioReadWrite<u8>),
        (0xf4 => @END),
    }
}

/// The MCHBAR window (32 KiB on Ironlake).
#[derive(Clone, Copy)]
pub(crate) struct MchBar(pub(crate) usize);
impl MmioBar for MchBar {
    fn base(self) -> usize {
        self.0
    }
    fn mapped_size(self) -> Option<u32> {
        Some(0x8000)
    }
}

/// The DMIBAR window.
#[derive(Clone, Copy)]
pub(crate) struct DmiBar(pub(crate) usize);
impl MmioBar for DmiBar {
    fn base(self) -> usize {
        self.0
    }
    fn mapped_size(self) -> Option<u32> {
        Some(0x1000)
    }
}

/// The EPBAR window.
#[derive(Clone, Copy)]
pub(crate) struct EpBar(pub(crate) usize);
impl MmioBar for EpBar {
    fn base(self) -> usize {
        self.0
    }
    fn mapped_size(self) -> Option<u32> {
        Some(0x1000)
    }
}

/// Uncore chipset flavour (coreboot `IRONLAKE_MOBILE`/`IRONLAKE_DESKTOP`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IronlakeChipset {
    Mobile,
    Desktop,
}

/// Ironlake policy: chipset BARs plus the board's DIMM wiring and graphics.
#[derive(Debug, Clone, Copy)]
pub struct IntelIronlakeConfig {
    pub mchbar: u64,
    pub dmibar: u64,
    pub epbar: u64,
    pub ecam_base: u64,
    /// 64, 128 or 256 buses; the QuickPath uncore takes the last one.
    pub ecam_buses: u16,
    pub chipset: IronlakeChipset,
    /// PCH resources raminit touches.
    pub rcba: u64,
    pub pmbase: u16,
    pub gpiobase: u16,
    pub hecibar: u32,
    /// SPD addresses: channel 0 slot 0/1, channel 1 slot 0/1; 0 = absent.
    /// Raminit supports one DIMM per channel: entries 1 and 3 must be zero.
    pub spd_addresses: [u8; 4],
    /// IGD stolen memory selector (coreboot `gfx_uma_size`): 0 = 32 MiB,
    /// 1 = 48, 2 = 64, 3 = 128, 4 = 256, 5 = 96, 6 = 160, 7 = 224, 8 = 352.
    pub gfx_uma_size: u8,
    /// PEG port 00:01.0 enabled in the devicetree.
    pub peg: bool,
    /// IGD 00:02.0 enabled in the devicetree.
    pub igd_enabled: bool,
    /// Board IGD policy (devicetree registers and display ports).
    pub igd: IronlakeIgdConfig,
    /// CPU TCC activation offset below TjMax (coreboot model 2065x
    /// `tcc_offset`); the CPU is the same package as this uncore.
    pub tcc_offset: u8,
    pub acpi_name: Option<&'static str>,
}

impl IntelIronlakeConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            mchbar: DEFAULT_MCHBAR,
            dmibar: DEFAULT_DMIBAR,
            epbar: DEFAULT_EPBAR,
            ecam_base: DEFAULT_ECAM_BASE,
            ecam_buses: DEFAULT_ECAM_BUSES,
            chipset: IronlakeChipset::Mobile,
            rcba: crate::ibexpeak::DEFAULT_RCBA,
            pmbase: crate::ibexpeak::DEFAULT_PMBASE,
            gpiobase: crate::ibexpeak::DEFAULT_GPIOBASE,
            hecibar: crate::ibexpeak::DEFAULT_HECIBAR,
            spd_addresses: [0x50, 0, 0x51, 0],
            gfx_uma_size: 0,
            peg: false,
            igd_enabled: true,
            igd: IronlakeIgdConfig::new(),
            tcc_offset: 0,
            acpi_name: Some("PCI0"),
        }
    }

    /// The QuickPath uncore bus.
    #[must_use]
    pub const fn quickpath_bus(&self) -> u8 {
        (self.ecam_buses - 1) as u8
    }
}

impl Default for IntelIronlakeConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::IntelEcamConfig for IntelIronlakeConfig {
    fn ecam_base(&self) -> u64 {
        self.ecam_base
    }
}

/// The raminit working set lives in CAR, like coreboot's `timings_car`.
static mut RAM_STATE: raminit::RamState = raminit::RamState::new();

/// Ironlake northbridge driver.
pub struct IntelIronlake {
    config: &'static IntelIronlakeConfig,
    /// The user options over the board config.
    options: crate::igd::options::Options,
    dram_ready: bool,
    boot_path: crate::BootPath,
    /// Where PCI MMIO starts below 4 GiB (TOLUD), once memory is detected.
    low_dram_end: Option<u64>,
    igd_bars: Option<crate::igd::IgdBars>,
    display: crate::igd::IgdDisplay,
}

/// The decoded uncore memory map, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MemoryMap {
    tom: u64,
    tolud: u32,
    touud: u64,
    tseg: u32,
}

impl IntelIronlake {
    pub(crate) fn hostbridge(&self) -> &'static IronlakeHostBridgeConfig {
        // SAFETY: ECAM is enabled in pre_console_init before any access.
        unsafe { ecam::EcamDevice::new(0, 0, 0).regs::<IronlakeHostBridgeConfig>() }
    }

    pub(crate) fn mch(&self) -> MchBar {
        MchBar(self.config.mchbar as usize)
    }

    fn dmi(&self) -> DmiBar {
        DmiBar(self.config.dmibar as usize)
    }

    fn qpi_non_core(&self) -> ecam::EcamDevice {
        ecam::EcamDevice::new(self.config.quickpath_bus(), 0, 0)
    }

    fn qpi_sad(&self) -> ecam::EcamDevice {
        ecam::EcamDevice::new(self.config.quickpath_bus(), 0, 1)
    }

    /// coreboot `bootblock_early_northbridge_init()`: PCIEXBAR through the
    /// legacy config ports, on bus 255 where the reset default of 256 buses
    /// puts the uncore.
    fn enable_ecam(&self) {
        #[cfg(target_arch = "x86_64")]
        {
            let length = match self.config.ecam_buses {
                256 => 0,
                128 => 6,
                64 => 7,
                _ => unreachable!(),
            };
            let pciexbar = PCIEXBAR::BASE.val((self.config.ecam_base >> 20) as u32)
                + PCIEXBAR::LENGTH.val(length)
                + PCIEXBAR::ENABLE::SET;
            // SAFETY: the only legacy config cycles, needed to enable ECAM.
            unsafe {
                fstart_core::pio::pci_cfg_write32(255, 0, 1, 0x54, 0);
                fstart_core::pio::pci_cfg_write32(255, 0, 1, 0x50, pciexbar.value);
            }
        }
        ecam::init(self.config.ecam_base as usize);
    }

    /// coreboot `ironlake_setup_bars()`.
    fn setup_bars(&self) {
        let hb = self.hostbridge();
        hb.epbar_lo.set(self.config.epbar as u32 | 1);
        hb.epbar_hi.set(0);
        hb.mchbar_lo.set(self.config.mchbar as u32 | 1);
        hb.mchbar_hi.set(0);
        hb.dmibar_lo.set(self.config.dmibar as u32 | 1);
        hb.dmibar_hi.set(0);
        // C0000-FFFFF read and write DRAM.
        let sad = self.qpi_sad();
        sad.write8(0x40, 0x30);
        for pam in 1..7 {
            sad.write8(0x40 + pam, 0x33);
        }
    }

    /// MCHBAR 0x2ca8 bit 3: raminit finished on this power cycle.
    fn raminit_completed(&self) -> bool {
        self.mch().read8(0x2ca8) & (1 << 3) != 0
    }

    /// coreboot `ironlake_early_initialization()` (the PCH half is the
    /// southbridge's pre-console init).
    fn early_initialization(&self) {
        let hb = self.hostbridge();
        // Device ID override, very early.
        if hb.capid0[1].get() & (1 << 10) != 0 {
            let mut reg8 = hb.did_override.get() & !7;
            if self.config.chipset == IronlakeChipset::Mobile {
                reg8 |= 1;
            }
            hb.did_override.set(reg8);
        }
        self.setup_bars();
        let pm = crate::southbridge::pmio_ich::PmIo::new(self.config.pmbase);
        let s3_resume = pm.read16(crate::southbridge::pmio_ich::PM1_STS)
            & crate::southbridge::pmio_ich::WAK_STS
            != 0
            && (pm.read32(crate::southbridge::pmio_ich::PM1_CNT) >> 10) & 7 == 5;
        hb.deven
            .write(DEVEN::IGD::SET + DEVEN::PEG10::SET + DEVEN::HOST::SET);
        // early_cpu_init(): multicore, quadcore and HT stay enabled.
        self.qpi_non_core()
            .modify32(DESIRED_CORES, 0xffff_fefc, 0x10000);
        // SAFETY: the uncore pairs only with model 2065x CPUs.
        unsafe { fstart_arch::x86::cpu::intel::model_2065x::early_init() };
        // S3 resume magic, before raminit only.
        if s3_resume && !self.raminit_completed() {
            self.mch().clrsetbits32(0x1e8, 1, 6);
            self.mch().clrsetbits32(0x1e8, 3, 4);
        }
    }

    fn read_memory_map(&self) -> Result<MemoryMap, ServiceError> {
        let hb = self.hostbridge();
        let tom = u64::from(hb.tom.get()) << 26;
        let tolud = u32::from(hb.tolud.get() & 0xfff0) << 16;
        let touud = u64::from(hb.touud.get()) << 20;
        let tseg = hb.tseg.get() & 0xfff0_0000;
        if tom <= 0x10_0000
            || tolud <= 0x10_0000
            || tseg <= 0x10_0000
            || tseg >= tolud
            || u64::from(tolud) > 0x1_0000_0000
        {
            return Err(ServiceError::HardwareError);
        }
        Ok(MemoryMap {
            tom,
            tolud,
            touud: touud.max(u64::from(tolud)),
            tseg,
        })
    }
}

/// The stolen-memory sizes of the GGC GMS selectors 0 to 8, in MiB.
const GMS_MB: [u16; 9] = [32, 48, 64, 128, 256, 96, 160, 224, 352];

/// The user's stolen-memory sizes, in MiB.
const UMA_SIZES: &[fstart_options::Choice] = &[
    fstart_options::Choice::new(32, "32 MiB"),
    fstart_options::Choice::new(48, "48 MiB"),
    fstart_options::Choice::new(64, "64 MiB"),
    fstart_options::Choice::new(96, "96 MiB"),
    fstart_options::Choice::new(128, "128 MiB"),
    fstart_options::Choice::new(160, "160 MiB"),
    fstart_options::Choice::new(224, "224 MiB"),
    fstart_options::Choice::new(256, "256 MiB"),
    fstart_options::Choice::new(352, "352 MiB"),
];

/// The GMS selector of `mb` of stolen memory.
const fn gms_of_mb(mb: u16) -> Option<u8> {
    let mut selector = 0;
    while selector < GMS_MB.len() {
        if GMS_MB[selector] == mb {
            return Some(selector as u8);
        }
        selector += 1;
    }
    None
}

impl IntelIronlakeConfig {
    /// The board's values of the user options.
    pub const fn options(&self) -> crate::igd::options::Options {
        crate::igd::options::Options::new(
            self.igd_enabled,
            GMS_MB[self.gfx_uma_size as usize % GMS_MB.len()],
        )
    }
}

impl IntelIronlake {
    /// The GMS selector of the user's stolen memory.
    fn gms(&self) -> u8 {
        gms_of_mb(self.options.uma_mb).unwrap_or(self.config.gfx_uma_size)
    }
}

impl crate::IntelNorthbridgeDriver for IntelIronlake {
    type Config = IntelIronlakeConfig;
    const ACPI_PCI_ROOT_NAME: &'static str = "PCI0";
    const ACPI_IGD_NAME: &'static str = "GFX0";

    fn pci_root_info(&self) -> PciRootInfo {
        PciRootInfo {
            segment: 0,
            ecam_base: self.config.ecam_base,
            bus_start: 0,
            // The QuickPath bus is not part of the PCI domain.
            bus_end: self.config.quickpath_bus() - 1,
        }
    }
    fn pci_root(&self) -> Result<fstart_pci::PciRoot, fstart_pci::PciEcamError> {
        self.host_bridge().root(self.pci_root_info())
    }
    type Options = crate::igd::options::Options;
    const KNOBS: &'static [fstart_options::Knob<Self::Options>] =
        &[crate::igd::options::uma(UMA_SIZES)];

    fn options(&self) -> Self::Options {
        self.options
    }

    fn set_options(&mut self, options: Self::Options) {
        self.options = options;
    }

    fn new_from_config(config: &'static Self::Config) -> Result<Self, ServiceError> {
        let buses = config.ecam_buses;
        if !matches!(buses, 64 | 128 | 256)
            || config.ecam_base == 0
            || config.ecam_base & ((u64::from(buses) << 20) - 1) != 0
            || config.ecam_base + (u64::from(buses) << 20) > 0x1_0000_0000
            || [config.mchbar, config.dmibar, config.epbar, config.rcba]
                .iter()
                .any(|base| *base == 0 || *base > u64::from(u32::MAX) || base & 0xfff != 0)
            || config.mchbar & 0x7fff != 0
            || config.gfx_uma_size > 8
            || config.spd_addresses[1] != 0
            || config.spd_addresses[3] != 0
            || config.pmbase == 0
            || config.gpiobase == 0
        {
            return Err(ServiceError::InvalidParam);
        }
        Ok(Self {
            config,
            options: config.options(),
            dram_ready: false,
            boot_path: crate::BootPath::Normal,
            low_dram_end: None,
            igd_bars: None,
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
        self.early_initialization();
        // The mainstage repeats early init after the bootblock trained.
        self.dram_ready = self.raminit_completed() && self.read_memory_map().is_ok();
        Ok(())
    }

    fn set_boot_path(&mut self, boot_path: crate::BootPath) {
        self.boot_path = boot_path;
    }

    fn supports_s3_replay(&self) -> bool {
        // Replays coreboot's saved training on resume; not hardware-validated.
        true
    }

    fn training_identity(&self) -> Option<[u8; 32]> {
        let hb = self.hostbridge();
        Some(crate::generic::training::identity(
            b"ironlake-raminit-v1",
            &self.config.spd_addresses,
            u16::from(self.gms()),
            u8::from(self.config.igd_enabled),
            crate::generic::training::HardwareIdentity {
                vendor_id: hb.vendor_id.get(),
                device_id: hb.device_id.get(),
                revision_id: hb.revision_id.get(),
                capabilities: [hb.capid0[0].get(), hb.capid0[1].get()],
                cpu_signature: fstart_arch::x86::cpuid(1).0,
            },
        ))
    }

    fn stage_local_init(&mut self) -> Result<(), ServiceError> {
        self.enable_ecam();
        self.read_memory_map()?;
        self.dram_ready = true;
        self.igd_func0_init();
        Ok(())
    }

    /// coreboot `ironlake_init()`: hide PEG10 and the IGD per devicetree.
    fn chip_init(&mut self) -> Result<(), ServiceError> {
        let hb = self.hostbridge();
        let mut deven = hb.deven.extract();
        if !self.config.peg {
            fstart_log::info!("ironlake: disabling PEG10");
            deven.modify(DEVEN::PEG10::CLEAR);
        }
        if !self.config.igd_enabled {
            fstart_log::info!("ironlake: disabling IGD");
            deven.modify(DEVEN::IGD::CLEAR);
        }
        hb.deven.set(deven.get());
        Ok(())
    }

    /// coreboot `northbridge_init()`: clear DMI errors, DMI link tuning.
    fn post_dram_init(&mut self) -> Result<(), ServiceError> {
        const DMIUESTS: u32 = 0x1c4;
        const DMICESTS: u32 = 0x1d0;
        const DMILLTC: u32 = 0x238;
        const DMILCTL: u32 = 0x088;
        let dmi = self.dmi();
        dmi.write32(DMIUESTS, 0xffff_ffff);
        dmi.write32(DMICESTS, 0xffff_ffff);
        dmi.setbits32(DMILLTC, 1 << 29);
        dmi.setbits32(0x1f8, 1 << 16);
        dmi.setbits32(DMILCTL, (1 << 1) | (1 << 0));
        Ok(())
    }

    fn post_verify_init(
        &mut self,
        opregion: crate::igd::OpRegionMemory<'_>,
    ) -> Result<(), ServiceError> {
        self.igd_publish_and_modeset(opregion);
        Ok(())
    }

    fn resume_init(&mut self, opregion: Option<u32>) -> Result<(), ServiceError> {
        self.enable_ecam();
        self.read_memory_map()?;
        self.dram_ready = true;
        self.igd_resume(opregion);
        Ok(())
    }

    fn framebuffer_info(&self) -> Option<fstart_core::services::FramebufferInfo> {
        self.display.framebuffer_info()
    }

    fn memory_detected(&mut self, state: &fstart_core::services::memory_detect::E820State) {
        let _ = state;
        self.low_dram_end = self.read_memory_map().ok().map(|map| u64::from(map.tolud));
    }
}

impl fstart_core::services::MemoryInit for IntelIronlake {
    type Input<'a> = crate::SpdTraining<'a>;
    type Output = Option<usize>;

    /// coreboot romstage `chipset_init()` then `raminit()`. A cold boot's
    /// first pass ends in the CPU-only reset raminit needs and re-enters
    /// the bootblock; cached training is replayed on resume and verified
    /// before use on a cold boot.
    fn bring_up(
        &mut self,
        crate::SpdTraining {
            smbus,
            cached,
            capture,
        }: crate::SpdTraining<'_>,
    ) -> Result<Option<usize>, ServiceError> {
        use zerocopy::IntoBytes;
        self.dram_ready = false;
        let resume = self.boot_path == crate::BootPath::S3Resume;
        let config = raminit::RaminitConfig {
            mchbar: self.config.mchbar as usize,
            dmibar: self.config.dmibar as usize,
            epbar: self.config.epbar as usize,
            rcba: self.config.rcba as usize,
            gpiobase: self.config.gpiobase,
            pmbase: self.config.pmbase,
            hecibar: self.config.hecibar as usize,
            qpi_bus: self.config.quickpath_bus(),
            gfx_uma_size: self.gms(),
        };
        // SAFETY: the bootblock is single-threaded and only raminit uses
        // this CAR-resident state.
        let state = unsafe { &mut *core::ptr::addr_of_mut!(RAM_STATE) };
        let trained = raminit::run(
            state,
            &config,
            smbus,
            &self.config.spd_addresses,
            resume,
            cached,
        )?;
        self.read_memory_map()?;
        self.dram_ready = true;
        fstart_log::info!(
            "ironlake: {} MiB DDR3-{}, QPI {} MHz",
            trained.total_memory_mb,
            (u32::from(trained.clock_speed_index) + 3) * 800 / 3,
            u32::from(trained.fsb_frequency) * 10 / 9
        );
        let Some(record) = trained.record.filter(|_| !capture.is_empty()) else {
            return Ok(None);
        };
        let bytes = record.as_bytes();
        let available = capture.len();
        let Some(destination) = capture.get_mut(..bytes.len()) else {
            fstart_log::error!(
                "ironlake: training record {} bytes exceeds capture {}",
                bytes.len(),
                available
            );
            return Err(ServiceError::InvalidParam);
        };
        destination.copy_from_slice(bytes);
        Ok(Some(bytes.len()))
    }
}

/// Preserve remapped RAM above 8 GiB; only the 64 MiB immediately below
/// 8 GiB is non-DMA-capable (coreboot `mc_read_resources`).
fn build_ironlake_e820(map: MemoryMap, entries: &mut [E820Entry]) -> Result<usize, ServiceError> {
    let mut state = E820State::new();
    let count = build_pc_compatible_e820(state.entries_mut(), map.tseg, map.touud, map.tolud)?;
    state.set_detected(count, map.tom)?;
    state.reserve_range_as(0x1_fc00_0000, 0x0400_0000, E820Kind::Unusable)?;
    let count = state.count();
    entries
        .get_mut(..count)
        .ok_or(ServiceError::NotSupported)?
        .copy_from_slice(state.entries());
    Ok(count)
}

impl MemoryDetector for IntelIronlake {
    /// coreboot `mc_read_resources()`: RAM below TSEG and above 4 GiB;
    /// TSEG, the gap to the GTT and the IGD stolen memory reserved.
    fn detect_memory(&self, entries: &mut [E820Entry]) -> Result<usize, ServiceError> {
        if !self.dram_ready {
            return Err(ServiceError::NotInitialized);
        }
        let map = self.read_memory_map()?;
        let count = build_ironlake_e820(map, entries)?;
        // Publish hardware RAM before ramstage restores write-back caching.
        fstart_arch::x86::mtrr::set_ram_wb_ranges_from(
            entries[..count]
                .iter()
                .filter(|entry| entry.kind == E820Kind::Ram as u32)
                .map(|entry| (entry.addr, entry.size)),
        )?;
        Ok(count)
    }
    fn total_ram_bytes(&self) -> Result<u64, ServiceError> {
        if !self.dram_ready {
            return Err(ServiceError::NotInitialized);
        }
        Ok(self.read_memory_map()?.tom)
    }
}

impl IntelIronlake {
    /// What the host bridge sends to PCI: from TOLUD to the I/O APIC. ECAM
    /// covers the QuickPath bus too.
    fn host_bridge(&self) -> crate::host_bridge::HostBridge {
        let ecam = self.config.ecam_base;
        crate::host_bridge::HostBridge {
            ecam: (ecam, ecam + (u64::from(self.config.ecam_buses) << 20)),
            low_dram_end: self.low_dram_end,
            above_4g: None,
        }
    }
}

/// coreboot `northbridge_write_smram()`: SMRAM lives in the QuickPath SAD.
impl fstart_arch::x86::cpu::intel::smm::SmramControl for IntelIronlake {
    fn tseg(&self) -> Option<(u64, u32)> {
        if !self.dram_ready {
            return None;
        }
        let map = self.read_memory_map().ok()?;
        Some((u64::from(map.tseg), raminit::TSEG_SIZE_MIB << 20))
    }
    fn smram_open(&self) -> bool {
        self.qpi_sad().write8(0x4d, crate::gmch::smram::open());
        crate::gmch::smram::is_open(self.qpi_sad().read8(0x4d))
    }
    fn smram_close(&self) {
        self.qpi_sad().write8(0x4d, crate::gmch::smram::closed());
    }
    fn smram_lock(&self) -> bool {
        self.qpi_sad().write8(0x4d, crate::gmch::smram::locked());
        crate::gmch::smram::is_locked(self.qpi_sad().read8(0x4d)) && self.tseg().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IntelNorthbridgeDriver;

    #[test]
    fn rejects_unusable_bar_policy() {
        static GOOD: IntelIronlakeConfig = IntelIronlakeConfig::new();
        assert!(IntelIronlake::new_from_config(&GOOD).is_ok());
        static BAD_ECAM: IntelIronlakeConfig = IntelIronlakeConfig {
            ecam_base: 0xe800_0000,
            ..IntelIronlakeConfig::new()
        };
        assert!(IntelIronlake::new_from_config(&BAD_ECAM).is_err());
        static BAD_UMA: IntelIronlakeConfig = IntelIronlakeConfig {
            gfx_uma_size: 9,
            ..IntelIronlakeConfig::new()
        };
        assert!(IntelIronlake::new_from_config(&BAD_UMA).is_err());
    }

    #[test]
    fn stolen_memory_options_are_the_gms_sizes() {
        // The choices are exactly the selectors' sizes.
        let mut sizes = GMS_MB.map(u32::from);
        sizes.sort_unstable();
        assert!(UMA_SIZES.iter().map(|c| c.value).eq(sizes));
        // The board's selector is the default, the user's size reaches
        // raminit as its selector.
        static BOARD: IntelIronlakeConfig = IntelIronlakeConfig {
            gfx_uma_size: 5,
            ..IntelIronlakeConfig::new()
        };
        let mut nb = IntelIronlake::new_from_config(&BOARD).unwrap();
        assert_eq!((nb.options().uma_mb, nb.gms()), (96, 5));
        let mut options = nb.options();
        options.uma_mb = 352;
        nb.set_options(options);
        assert_eq!(nb.gms(), 8);
    }

    #[test]
    fn rejects_second_slot_spd_wiring_before_hardware_access() {
        static CONFIGS: [IntelIronlakeConfig; 2] = [
            IntelIronlakeConfig {
                spd_addresses: [0x50, 0x52, 0x51, 0],
                ..IntelIronlakeConfig::new()
            },
            IntelIronlakeConfig {
                spd_addresses: [0x50, 0, 0x51, 0x53],
                ..IntelIronlakeConfig::new()
            },
        ];
        for config in &CONFIGS {
            assert!(matches!(
                IntelIronlake::new_from_config(config),
                Err(ServiceError::InvalidParam)
            ));
        }
    }

    #[test]
    fn e820_keeps_remapped_ram_above_the_non_dma_hole() {
        let map = MemoryMap {
            tom: 8192 << 20,
            tolud: 2112 << 20,
            touud: 10112 << 20,
            tseg: 2040 << 20,
        };
        let mut entries = [E820Entry::zeroed(); 32];
        let count = build_ironlake_e820(map, &mut entries).unwrap();
        assert!(entries[..count].iter().any(|entry| {
            entry.kind == E820Kind::Unusable as u32
                && entry.addr == 0x1_fc00_0000
                && entry.size == 0x0400_0000
        }));
        assert!(entries[..count].iter().any(|entry| {
            entry.kind == E820Kind::Ram as u32
                && entry.addr == 0x2_0000_0000
                && entry.addr + entry.size == 10112 << 20
        }));
    }

    #[test]
    fn quickpath_bus_is_the_last_ecam_bus() {
        assert_eq!(IntelIronlakeConfig::new().quickpath_bus(), 255);
        let pciexbar = PCIEXBAR::BASE.val(0xe00) + PCIEXBAR::LENGTH.val(0) + PCIEXBAR::ENABLE::SET;
        assert_eq!(pciexbar.value, 0xe000_0001);
    }
}
