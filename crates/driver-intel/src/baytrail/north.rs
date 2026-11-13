//! The north cluster: IOSF transaction router (00:00.0), memory controller,
//! graphics and SMRAM (coreboot `northcluster.c`, `memmap.c`, romstage).

use super::bunit::{self, BMBOUND, BMBOUND_HI, MMCONF, SMRR};
use super::devices::gfx::{self, GfxConfig};
use super::devices::lpe;
use super::iosf::{self, Iosf};
use super::punit::{self, VrPs2};
use super::raminit;
use super::{TSEG_MB, txe};
use crate::BootPath;
use fstart_core::services::ServiceError;
use fstart_core::services::memory_detect::{E820Entry, E820Kind, MemoryDetector};
use fstart_pci::PciRootInfo;
use fstart_pci::legacy::LegacyConfig;
use tock_registers::interfaces::{Readable, Writeable};

/// Where raminit finds the SPD of channel 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpdSource {
    /// A module SPD EEPROM on the PCU SMBus.
    Smbus(u8),
    /// Memory down: the board hands the bytes over with
    /// [`BaytrailNorth::set_spd`] before training.
    Board,
}

/// North cluster policy.
#[derive(Debug, Clone, Copy)]
pub struct NorthConfig {
    pub ecam_base: u64,
    pub spd: SpdSource,
    /// Run the TXE UMA handshake. A board that hides the TXE must say so:
    /// a TXE hidden after DRAM Init Done locks up.
    pub txe: bool,
    pub vr_ps2: VrPs2,
    /// Refresh at twice the DDR3 rate (coreboot's default unless
    /// `disable_ddr_2x_refresh_rate`).
    pub ddr_2x_refresh: bool,
    pub gfx: GfxConfig,
}

impl NorthConfig {
    pub const fn new() -> Self {
        Self {
            ecam_base: super::ECAM_BASE,
            spd: SpdSource::Smbus(0x50),
            txe: true,
            vr_ps2: VrPs2 {
                vnn: false,
                vcc: false,
            },
            ddr_2x_refresh: true,
            gfx: GfxConfig::new(),
        }
    }
}

impl Default for NorthConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::IntelEcamConfig for NorthConfig {
    fn ecam_base(&self) -> u64 {
        self.ecam_base
    }
}

/// The north cluster driver.
pub struct BaytrailNorth {
    config: &'static NorthConfig,
    spd: Option<&'static [u8]>,
    memory_info: Option<fstart_core::memory_info::MemoryInfo>,
    boot_path: BootPath,
    /// BMBOUND and BMBOUND_HI latched before the bus scan (0 until then):
    /// the B-unit stops answering host reads during device init.
    tolm: u32,
    high_end: u64,
}

/// End of PCI MMIO, as the DSDT `_CRS` describes it (coreboot's `PM02`).
const PCI_MMIO32_LIMIT: u64 = 0xfe00_0000;

impl BaytrailNorth {
    /// Memory-down SPD for channel 0, from the board, before training.
    pub fn set_spd(&mut self, spd: &'static [u8]) {
        self.spd = Some(spd);
    }

    /// The sideband, for the north cluster's hooks: the platform flow
    /// calls them on the BSP outside any MP section.
    fn iosf(&self) -> Iosf<fstart_pci::ecam::GlobalEcam> {
        // SAFETY: the driver hooks run in the BSP's single-threaded flow.
        unsafe { iosf::ecam() }
    }

    /// Decode ECAM. The B-unit register lives behind the IOSF registers
    /// in config space, so the first access goes through CF8/CFC.
    fn setup_mmconfig(&self) {
        // SAFETY: bootblock and ramstage own the sideband on the BSP.
        let iosf = unsafe { Iosf::new(LegacyConfig) };
        iosf.reg(bunit::MMCONF_REG)
            .set((self.config.ecam_base as u32 & !0xfff_ffff) | MMCONF::ENABLE::SET.value);
        fstart_pci::ecam::init(self.config.ecam_base as usize);
    }

    /// Top of low memory: BMBOUND, as latched before the bus scan.
    pub fn tolm(&self) -> u32 {
        if self.tolm != 0 {
            return self.tolm;
        }
        self.iosf().reg(bunit::BMBOUND_REG).get() & BMBOUND::ADDRESS.mask << BMBOUND::ADDRESS.shift
    }

    /// End of DRAM above 4 GiB.
    fn bmbound_hi(&self) -> u64 {
        if self.tolm != 0 {
            return self.high_end;
        }
        u64::from(
            self.iosf()
                .reg(bunit::BMBOUND_HI_REG)
                .read(BMBOUND_HI::ADDRESS),
        ) << 28
    }

    /// TSEG base and end (the SMRR range, 1 MiB granular).
    fn smm_range(&self) -> (u64, u64) {
        let iosf = self.iosf();
        let low = u64::from(iosf.reg(bunit::SMRRL).read(SMRR::MB)) << 20;
        let high = (u64::from(iosf.reg(bunit::SMRRH).read(SMRR::MB)) + 1) << 20;
        (low, high)
    }

    fn igd_stolen() -> raminit::Stolen {
        gfx::stolen()
    }

    fn spd_bytes<'a>(
        &self,
        smbus: &mut dyn fstart_core::services::SmBus,
        buffer: &'a mut [u8; 256],
    ) -> Result<&'a [u8], ServiceError> {
        match self.config.spd {
            SpdSource::Board => self
                .spd
                .map(|spd| {
                    let len = spd.len().min(256);
                    buffer[..len].copy_from_slice(&spd[..len]);
                    &buffer[..len]
                })
                .ok_or_else(|| {
                    fstart_log::error!("baytrail: memory-down board supplied no SPD");
                    ServiceError::NotInitialized
                }),
            SpdSource::Smbus(address) => {
                for (offset, byte) in buffer.iter_mut().enumerate() {
                    *byte = smbus.read_byte(address, offset as u8)?;
                }
                Ok(&buffer[..])
            }
        }
    }
}

impl BaytrailNorth {
    pub(crate) fn root_info(&self) -> PciRootInfo {
        PciRootInfo {
            segment: 0,
            ecam_base: self.config.ecam_base,
            bus_start: 0,
            bus_end: 0xff,
        }
    }
}

impl crate::IntelNorthbridgeDriver for BaytrailNorth {
    type Config = NorthConfig;
    const ACPI_PCI_ROOT_NAME: &'static str = "PCI0";
    const ACPI_IGD_NAME: &'static str = "GFX0";

    /// What the B-unit forwards to PCI, and what it decodes itself: the
    /// ECAM window and the LPSS/LPE/PMC blocks are firmware territory, and
    /// nothing above [`PCI_MMIO32_LIMIT`] is forwarded at all.
    fn pci_root(&self) -> Result<fstart_pci::PciRoot, fstart_pci::PciEcamError> {
        use fstart_pci::PciRole::Own;
        // Live bounds on the target; a fixed 2 GiB map in host tests.
        #[cfg(target_os = "none")]
        let low = u64::from(self.tolm());
        #[cfg(not(target_os = "none"))]
        let low = 0x8000_0000u64;
        let ecam = self.config.ecam_base;
        let chipset = [
            Own.mmio(ecam, u64::from(super::ECAM_BUSES) * 0x10_0000),
            // LPE firmware, PMC, GPIO and the LPSS blocks (coreboot's
            // `pci_domain_read_resources`).
            Own.mmio(0xfeb0_0000, 0x10_0000),
            Own.mmio(0xfed0_1000, 0x400),
            Own.mmio(0xfed0_3000, 0x400),
            Own.mmio(0xfed0_5000, 0x800),
            Own.mmio(0xfed0_8000, 0x400),
            Own.mmio(0xfed0_c000, 0x4000),
            Own.mmio(0xfed1_c000, 0x400),
            Own.mmio(0xfef0_0000, 0x10_0000),
        ];
        // The B-unit forwards no PCI memory above `PCI_MMIO32_LIMIT`;
        // describing it would advertise an aperture the SoC drops.
        fstart_pci::PciRoot::new(
            self.root_info(),
            crate::host_bridge::forwarded_below(Some(low), None, PCI_MMIO32_LIMIT)
                .into_iter()
                .chain(chipset),
        )
    }

    type Options = ();
    const KNOBS: &'static [fstart_options::Knob<Self::Options>] = &[];

    fn options(&self) -> Self::Options {}

    fn set_options(&mut self, _options: Self::Options) {}

    fn new_from_config(config: &'static Self::Config) -> Result<Self, ServiceError> {
        Ok(Self {
            config,
            spd: None,
            memory_info: None,
            boot_path: BootPath::Normal,
            tolm: 0,
            high_end: 0,
        })
    }

    fn config(&self) -> &'static Self::Config {
        self.config
    }

    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        self.setup_mmconfig();
        Ok(())
    }

    fn early_init(&mut self) -> Result<(), ServiceError> {
        Ok(())
    }

    fn set_boot_path(&mut self, boot_path: BootPath) {
        self.boot_path = boot_path;
    }

    fn memory_info(&self) -> Option<fstart_core::memory_info::MemoryInfo> {
        self.memory_info
    }

    /// Chip init before the bus scan (coreboot `baytrail_init_pre_device`,
    /// north part): the CPU attributes and the refresh rate.
    fn pre_bus_scan(&mut self) -> Result<(), ServiceError> {
        let rid = super::lpc::device().read8(0x08);
        fstart_log::info!(
            "baytrail: cpuid {:#x} rid {:#04x} stepping {} bclk {} kHz",
            fstart_arch::x86::cpuid(1).0,
            rid,
            super::Stepping::from_revision(rid).name(),
            fstart_arch::x86::cpu::intel::silvermont::bus_freq_khz()
        );
        (self.tolm, self.high_end) = (self.tolm(), self.bmbound_hi());
        if self.config.ddr_2x_refresh {
            // DRFC tREFI: 2x refresh.
            self.iosf().modify(iosf::DUNIT_CH0, 0x08, 0x7000, 0x2000);
        }
        Ok(())
    }

    /// Graphics power wells, PM and panel setup after allocation, and the
    /// B-unit SMRAM eviction policy before the APs start.
    fn post_dram_init(&mut self) -> Result<(), ServiceError> {
        // Dirty lines written in SMM may be evicted outside it; without the
        // untrusted agent in SMRWAC those writes are silently dropped.
        let iosf = self.iosf();
        let wac = iosf.reg(bunit::SMRWAC);
        wac.set(wac.get() | iosf::SAI_IA_UNTRUSTED);
        gfx::init(&self.config.gfx, &iosf)
    }

    fn stage_local_init(&mut self) -> Result<(), ServiceError> {
        Ok(())
    }

    fn post_verify_init(
        &mut self,
        opregion: crate::igd::OpRegionMemory<'_>,
    ) -> Result<(), ServiceError> {
        gfx::publish_opregion(&self.config.gfx, opregion);
        Ok(())
    }
}

impl fstart_core::services::MemoryInit for BaytrailNorth {
    type Input<'a> = crate::SpdTraining<'a>;
    type Output = Option<usize>;

    /// The romstage sequence: guaranteed core frequency, P-unit start,
    /// graphics stolen sizes, the TXE UMA size, training and the TXE
    /// DRAM Init Done.
    fn bring_up(&mut self, input: crate::SpdTraining<'_>) -> Result<Option<usize>, ServiceError> {
        if self.boot_path == BootPath::S3Resume {
            // Native raminit has no self-refresh exit path.
            return Err(ServiceError::NotSupported);
        }
        fstart_arch::x86::cpu::intel::silvermont::set_max_freq();
        let iosf = self.iosf();
        punit::init(&iosf, self.config.vr_ps2);
        gfx::pre_raminit();
        let mut buffer = [0u8; 256];
        let spd = self.spd_bytes(input.smbus, &mut buffer)?;
        let txe_mb = txe::uma_size_mb(self.config.txe);
        let trained = raminit::sdram_initialize(&iosf, spd, Self::igd_stolen(), txe_mb)?;
        gfx::set_stolen_bases(&trained.map);
        txe::dram_init_done(trained.map.txe_base_mb, trained.map.txe_mb)?;
        self.memory_info = Some(trained.memory_info);
        Ok(None)
    }
}

impl MemoryDetector for BaytrailNorth {
    /// The host memory map (coreboot `nc_read_resources`).
    fn detect_memory(&self, entries: &mut [E820Entry]) -> Result<usize, ServiceError> {
        let (smm_low, smm_high) = self.smm_range();
        let tolm = u64::from(self.tolm());
        let high = self.bmbound_hi();
        if smm_low <= 0x10_0000 || smm_high > tolm || entries.len() < 9 {
            fstart_log::error!(
                "baytrail: invalid memory map SMRR {:#x}-{:#x} BMBOUND {:#x}",
                smm_low,
                smm_high,
                tolm
            );
            return Err(ServiceError::HardwareError);
        }
        let mut count = 0;
        let mut push = |base: u64, end: u64, kind: E820Kind| {
            if end > base {
                entries[count] = E820Entry::new(base, end - base, kind);
                count += 1;
            }
        };
        push(0, 0xa_0000, E820Kind::Ram);
        // Legacy VGA, then the BIOS shadow kept by firmware.
        push(0xa_0000, 0x10_0000, E820Kind::Reserved);
        // The LPE audio firmware window is DRAM the DSP owns.
        let lpe_window = lpe::present()
            .then_some((lpe::FIRMWARE_BASE, lpe::FIRMWARE_BASE + lpe::FIRMWARE_SIZE))
            .filter(|&(base, end)| base >= 0x10_0000 && end <= smm_low);
        match lpe_window {
            Some((base, end)) => {
                push(0x10_0000, base, E820Kind::Ram);
                push(base, end, E820Kind::Reserved);
                push(end, smm_low, E820Kind::Ram);
            }
            None => push(0x10_0000, smm_low, E820Kind::Ram),
        }
        // TSEG, then TXE UMA, graphics stolen memory and GTT.
        push(smm_low, smm_high, E820Kind::Reserved);
        push(smm_high, tolm, E820Kind::Reserved);
        push(0x1_0000_0000, high, E820Kind::Ram);
        // TSEG stays cacheable (SMRR covers SMM accesses), and so does the
        // LPE window: it is ordinary DRAM, and a hole would cost MTRRs.
        fstart_arch::x86::mtrr::set_ram_wb_ranges_from(
            entries[..count]
                .iter()
                .filter(|entry| entry.kind == E820Kind::Ram as u32)
                .map(|entry| (entry.addr, entry.size))
                .chain([(smm_low, smm_high - smm_low)])
                .chain(lpe_window.map(|(base, end)| (base, end - base))),
        )?;
        Ok(count)
    }

    fn total_ram_bytes(&self) -> Result<u64, ServiceError> {
        let (smm_low, _) = self.smm_range();
        Ok(smm_low.max(self.bmbound_hi()))
    }
}

/// SMRAM is TSEG. Normal mode reaches it while the CPU carries the boot
/// SAI that SMRRAC/SMRWAC admit (SMRCP keeps firmware from changing the
/// read policy); SMRR then fences it off from normal mode on every core.
impl fstart_arch::x86::cpu::intel::smm::SmramControl for BaytrailNorth {
    fn tseg(&self) -> Option<(u64, u32)> {
        let (low, high) = self.smm_range();
        (high > low && high - low == u64::from(TSEG_MB) << 20).then_some((low, TSEG_MB << 20))
    }

    /// TSEG is open when a write from normal mode reads back.
    fn smram_open(&self) -> bool {
        let Some((base, _)) = self.tseg() else {
            return false;
        };
        let probe = base as usize as *mut u32;
        // SAFETY: TSEG is reserved firmware memory nothing else uses before
        // the SMM installation, which overwrites it.
        unsafe {
            probe.write_volatile(0x5a5a_a5a5);
            let open = probe.read_volatile() == 0x5a5a_a5a5;
            probe.write_volatile(0);
            open
        }
    }

    /// The access policy has no closed state for the boot agent; SMRR
    /// closes TSEG to normal mode once relocation programs it.
    fn smram_close(&self) {}

    /// The B-unit has no SMRAM lock bit: SMRR is the protection.
    fn smram_lock(&self) -> bool {
        self.tseg().is_some()
    }
}
