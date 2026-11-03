//! Intel Sandy Bridge / Ivy Bridge system agent: the northbridge integrated
//! into the model 206AX/306AX CPU (coreboot `northbridge/intel/sandybridge`).
//!
//! Early decode and graphics stolen memory, DMI link training, native DDR3
//! initialization ([`raminit`]) with the fast-boot/S3 training cache, the
//! memory map, the ramstage system-agent and PEG setup, power limits,
//! VT-d, IGD power management and native graphics, the finalize locks and
//! the host bridge's ACPI (DSDT, MCFG, DMAR).

mod gma;
#[allow(clippy::unreadable_literal)]
mod powermeter;
pub mod raminit;
pub mod regs;

use crate::BootPath;
use fstart_arch::x86::cpu::intel::model_206ax;
use fstart_core::services::memory_detect::{
    E820Entry, E820Kind, E820State, MemoryDetector, build_pc_compatible_e820,
};
use fstart_core::services::{ServiceError, SmBus};
use fstart_pci::{PciRootInfo, ecam};
use regs::{CAPID0_A, DEVEN, DMI_LSTS, Dmibar, GGC, HostBridgeConfig, Mchbar, VC_RSTS};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

pub use gma::{PanelPort, SandybridgeIgdConfig};
pub use model_206ax::{CpuCstate, Model206axConfig, PsiState, Vr12Phases};

/// Fixed system-agent resources (coreboot Kconfig defaults).
pub const DEFAULT_MCHBAR: u64 = 0xfed1_0000;
pub const DEFAULT_DMIBAR: u64 = 0xfed1_8000;
pub const DEFAULT_EPBAR: u64 = 0xfed1_9000;
pub const DEFAULT_ECAM_BASE: u64 = 0xf000_0000;
pub const DEFAULT_ECAM_BUSES: u16 = 64;
/// IOMMU BARs (coreboot `GFXVT_BASE`, `VTVC0_BASE`).
pub const GFXVT_BASE: u64 = 0xfed9_0000;
pub const VTVC0_BASE: u64 = 0xfed9_1000;
/// Sandy Bridge sighting 3715511: pages the OS must not use.
const SNB_BAD_RAM: [(u64, u64); 2] = [(0x2000_0000, 0x20_0000), (0x4000_0000, 0x20_0000)];
/// Graphics device IDs coreboot initializes (`sandybridge_setup_graphics`).
const IGD_IDS: [u16; 11] = [
    0x0102, 0x0106, 0x010a, 0x0112, 0x0116, 0x0122, 0x0126, 0x0152, 0x0156, 0x0162, 0x0166,
];
/// Raminit-done marker in SSKPD_HI (coreboot `northbridge_romstage_finalize`).
const RAMINIT_DONE: u32 = 0xcafe;

/// PEG ports and other root functions the board leaves on (devicetree).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SandybridgeDevices {
    pub peg10: bool,
    pub peg11: bool,
    pub peg12: bool,
    pub peg60: bool,
    pub igd: bool,
    pub dev4: bool,
    pub dev7: bool,
}

/// Sandy Bridge / Ivy Bridge northbridge configuration.
#[derive(Debug, Clone, Copy)]
pub struct IntelSandybridgeConfig {
    pub mchbar: u64,
    pub dmibar: u64,
    pub epbar: u64,
    pub ecam_base: u64,
    /// 64, 128 or 256 buses.
    pub ecam_buses: u16,
    /// PCH RCBA, reserved by the host bridge (`PDRC`).
    pub rcba: u64,
    /// PCH ACPI PM base, for the graphics SWSCI.
    pub pmbase: u16,
    /// SPD addresses: channel 0 slot 0/1, channel 1 slot 0/1.
    pub spd_addresses: [u8; 4],
    /// DDR3 clock cap in MHz (devicetree `max_mem_clock_mhz`); zero uses fuses.
    pub max_mem_clock_mhz: u16,
    /// IGD stolen memory: 32 MiB * (index + 1) (CMOS `gfx_uma_size`).
    pub gfx_uma_index: u8,
    /// Enable ECC when every module supports it.
    pub ecc: bool,
    pub devices: SandybridgeDevices,
    /// HD Audio is enabled in the PCH (VT-d isolation policy).
    pub hda_enabled: bool,
    pub igd: SandybridgeIgdConfig,
    pub cpu: Model206axConfig,
    /// TSEG in MiB (coreboot `SMM_TSEG_SIZE`).
    pub tseg_mib: u32,
}

impl IntelSandybridgeConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            mchbar: DEFAULT_MCHBAR,
            dmibar: DEFAULT_DMIBAR,
            epbar: DEFAULT_EPBAR,
            ecam_base: DEFAULT_ECAM_BASE,
            ecam_buses: DEFAULT_ECAM_BUSES,
            rcba: crate::bd82x6x::DEFAULT_RCBA,
            pmbase: crate::bd82x6x::DEFAULT_PMBASE,
            spd_addresses: [0x50, 0, 0x52, 0],
            max_mem_clock_mhz: 0,
            gfx_uma_index: 0,
            ecc: false,
            devices: SandybridgeDevices {
                peg10: false,
                peg11: false,
                peg12: false,
                peg60: false,
                igd: true,
                dev4: false,
                dev7: false,
            },
            hda_enabled: true,
            igd: SandybridgeIgdConfig::new(),
            cpu: Model206axConfig::new(),
            tseg_mib: model_206ax::TSEG_SIZE >> 20,
        }
    }
}

impl Default for IntelSandybridgeConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::IntelEcamConfig for IntelSandybridgeConfig {
    fn ecam_base(&self) -> u64 {
        self.ecam_base
    }
}

/// The Sandy Bridge / Ivy Bridge system agent.
pub struct IntelSandybridge {
    config: &'static IntelSandybridgeConfig,
    boot_path: BootPath,
    /// DRAM was trained in this power cycle (SSKPD_HI marker).
    dram_ready: bool,
    /// The user options over the board config.
    options: crate::igd::options::Options,
    /// Where PCI MMIO starts below 4 GiB, once memory is detected.
    low_dram_end: Option<u64>,
    memory: Option<fstart_core::memory_info::MemoryInfo>,
    igd_bars: Option<crate::igd::IgdBars>,
    display: crate::igd::IgdDisplay,
}

impl IntelSandybridge {
    fn host(&self) -> &'static HostBridgeConfig {
        // SAFETY: ECAM is enabled in pre_console_init before any access.
        unsafe { ecam::EcamDevice::new(0, 0, 0).regs::<HostBridgeConfig>() }
    }

    fn mch(&self) -> &'static Mchbar {
        // SAFETY: MCHBAR is programmed at the validated base by early init.
        unsafe { &*(self.config.mchbar as *const Mchbar) }
    }

    fn dmi(&self) -> &'static Dmibar {
        // SAFETY: DMIBAR is programmed at the validated base by early init.
        unsafe { &*(self.config.dmibar as *const Dmibar) }
    }

    fn cpuid(&self) -> u32 {
        model_206ax::cpuid_signature()
    }

    fn is_sandy(&self) -> bool {
        self.host().device_id.get() & 0x50 == 0
    }

    /// coreboot `get_platform_type()`.
    fn mobile(&self) -> bool {
        self.host().device_id.get() & 0xc == 0x4
    }

    fn mchbar_enabled(&self) -> bool {
        self.host().mchbar.get() & 1 != 0
    }

    fn raminit_done(&self) -> bool {
        self.mchbar_enabled() && self.mch().sskpd_hi.get() & 0xffff == RAMINIT_DONE
    }

    /// coreboot `bootblock_early_northbridge_init`.
    fn enable_ecam(&self) {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            let length = match self.config.ecam_buses {
                256 => 0,
                128 => 1,
                _ => 2,
            };
            let pciexbar = regs::PCIEXBAR::ADDRESS.val((self.config.ecam_base >> 26) as u32)
                + regs::PCIEXBAR::LENGTH.val(length)
                + regs::PCIEXBAR::ENABLE::SET;
            // SAFETY: the only legacy config cycles, needed to enable ECAM.
            unsafe {
                fstart_pci::cf8::write32(0, 0, 0, 0x64, 0);
                fstart_pci::cf8::write32(0, 0, 0, 0x60, pciexbar.value);
            }
        }
        ecam::init(self.config.ecam_base as usize);
    }

    /// coreboot `systemagent_vtd_init`.
    fn vtd_init(&self) {
        if self.host().capid0_a.is_set(CAPID0_A::VTDD) {
            return;
        }
        let mch = self.mch();
        mch.gfxvtbar_hi.set((GFXVT_BASE >> 32) as u32);
        mch.gfxvtbar.set(GFXVT_BASE as u32 | 1);
        mch.vtvc0bar_hi.set((VTVC0_BASE >> 32) as u32);
        mch.vtvc0bar.set(VTVC0_BASE as u32 | 1);
        let policy = |base: u64, value: u32| {
            // SAFETY: the VT-d units were just mapped at their fixed BARs.
            unsafe { fstart_core::mmio::write32((base + 0xff0) as *mut u32, value) };
        };
        // Lock the remapping policies.
        policy(GFXVT_BASE, 0x8000_0000);
        if self.config.hda_enabled {
            policy(VTVC0_BASE, 0x2000_0000);
            policy(VTVC0_BASE, 0xa000_0000);
        } else {
            policy(VTVC0_BASE, 0x8000_0000);
        }
    }

    /// coreboot `sandybridge_setup_graphics`.
    fn setup_graphics(&self) {
        let igd = ecam::EcamDevice::new(0, 2, 0);
        if !IGD_IDS.contains(&igd.read16(2)) {
            fstart_log::info!("sandybridge: graphics not supported by this CPU");
            return;
        }
        let hb = self.host();
        hb.ggc.modify(
            GGC::GMS.val(u16::from(self.gms_index()) + 1) + GGC::GGMS.val(2) + GGC::IVD::CLEAR,
        );
        // 256 MiB aperture (MSAC).
        igd.and8_or8(0x62, !0x06, 0x02);
        let mch = self.mch();
        // Erratum workarounds and SA clock gating.
        mch.sapmctl.set(mch.sapmctl.get() | 1 << 9 | 1 << 10 | 1);
        // GPU RC6 workaround for sighting 366252.
        mch.sskpd_hi.set(mch.sskpd_hi.get() | 1 << 31);
        // Virtual legacy wire.
        mch.vlw.set(mch.vlw.get() & !1);
        mch.intrdirctl.set(mch.intrdirctl.get() | 1 << 4 | 1 << 5);
    }

    /// coreboot `start_peg_link_training`: Ivy Bridge PEG offset calibration.
    fn start_peg_link_training(&self) {
        if self.is_sandy() {
            return;
        }
        let deven = self.host().deven.extract();
        for (enabled, dev, func) in [
            (deven.is_set(DEVEN::PEG10), 1, 0),
            (deven.is_set(DEVEN::PEG11), 1, 1),
            (deven.is_set(DEVEN::PEG12), 1, 2),
            (deven.is_set(DEVEN::PEG60), 6, 0),
        ] {
            if enabled {
                // AFE_PWRON: three OC retries, clear DEFER_OC.
                ecam::EcamDevice::new(0, dev, func).modify32(0xc24, !(1 << 16), 1 << 5);
            }
        }
    }

    /// coreboot `systemagent_early_init`.
    fn systemagent_early_init(&self) {
        let hb = self.host();
        if hb.capid0_a.is_set(CAPID0_A::DIDOE) {
            let didor = hb.didor.get() & !7;
            hb.didor.set(didor | u8::from(self.mobile()));
        }
        fstart_log::info!(
            "sandybridge: {} system",
            if self.mobile() { "mobile" } else { "desktop" }
        );
        hb.epbar.set(self.config.epbar as u32 | 1);
        hb.epbar_hi.set(0);
        hb.mchbar.set(self.config.mchbar as u32 | 1);
        hb.mchbar_hi.set(0);
        hb.dmibar.set(self.config.dmibar as u32 | 1);
        hb.dmibar_hi.set(0);
        // C0000-FFFFF reads and writes go to RAM.
        hb.pam[0].set(0x30);
        for pam in &hb.pam[1..] {
            pam.set(0x33);
        }
        self.vtd_init();
        // DEVEN: leave the PEG bits.
        hb.deven.modify(DEVEN::IGD::SET);
        self.setup_graphics();
        self.start_peg_link_training();
    }

    /// coreboot `dmi_recipe`: Ivy Bridge DMI analog settings.
    fn dmi_recipe(&self) {
        if !model_206ax::is_ivy(self.cpuid()) {
            return;
        }
        let d = self.dmi();
        let set = |r: &fstart_core::mmio::MmioReadWrite<u32>, bits: u32| r.set(r.get() | bits);
        let clr = |r: &fstart_core::mmio::MmioReadWrite<u32>, bits: u32| r.set(r.get() & !bits);
        // Every coreboot step reads the register before writing it.
        let rw = |r: &fstart_core::mmio::MmioReadWrite<u32>, value: u32| {
            let _ = r.get();
            r.set(value);
        };
        for b in &d.afe_900 {
            set(&b.r914, 1 << 31);
        }
        for l in &d.afe_a00 {
            clr(&l.ra00, 3 << 26);
            set(&l.ra04, 1 << 11);
        }
        d.afe_c30.set((d.afe_c30.get() & !(0xf << 28)) | 1 << 30);
        for b in &d.afe_900 {
            clr(&b.r904, 7 << 22);
            clr(&b.r90c, 7 << 17);
        }
        for b in &d.afe_900 {
            clr(&b.r90c, 0xf << 21);
        }
        for b in &d.afe_900 {
            rw(&b.r904, 0x7a18_42ec);
            rw(&b.r90c, 0x0000_0128);
        }
        for p in &d.afe_700 {
            rw(&p.reg, 0x4613_9008);
        }
        rw(&d.afe_c04, 0x2e68_0008);
        for b in &d.afe_900 {
            rw(&b.r904, 0x3a18_42ec);
        }
        for b in &d.afe_900 {
            rw(&b.r910, 0x0000_4300);
        }
        for l in &d.afe_a00 {
            rw(&l.ra00, 0x0304_2018);
        }
        rw(&d.afe_c00, 0x2970_0c08);
        for l in &d.afe_a00 {
            rw(&l.ra04, 0x0c07_18f0);
        }
        for b in &d.afe_900 {
            rw(&b.r900, 0x5000_0000);
        }
        for b in &d.afe_900 {
            rw(&b.r908, 0x51ff_ffff);
        }
        for l in &d.afe_a00 {
            rw(&l.ra00, 0x0304_2018);
        }
        for p in &d.afe_700 {
            rw(&p.reg, 0x4613_9008);
        }
        for b in &d.afe_900 {
            rw(&b.r904, 0x3a18_46ec);
        }
        for l in &d.afe_a00 {
            rw(&l.ra00, 0x0304_2018);
        }
        for b in &d.afe_900 {
            rw(&b.r908, 0x51ff_ffff);
        }
        rw(&d.afe_c00, 0x2970_0c08);
        rw(&d.afe_c0c, 0x0006_3400);
        for value in [
            0x4633_9008,
            0x4533_9008,
            0x453b_9008,
            0x45bb_9008,
            0x45fb_9008,
        ] {
            for p in &d.afe_700 {
                rw(&p.reg, value);
            }
        }
        for b in &d.afe_900 {
            rw(&b.r914, 0x9021_a280);
        }
        for b in &d.afe_900 {
            rw(&b.r914, 0x9821_a280);
        }
        for l in &d.afe_a00 {
            rw(&l.ra00, 0x0324_2018);
        }
        rw(&d.cir_258, 0x6000_0600);
        for b in &d.afe_900 {
            rw(&b.r904, 0x2a18_46ec);
            rw(&b.r914, 0x9820_0280);
        }
        rw(&d.l0slat, 0x00c2_403c);
    }

    /// coreboot `wait_txt_clear`, bounded to about one second.
    fn wait_txt_clear(&self) -> Result<(), ServiceError> {
        if fstart_arch::x86::cpuid(1).2 & (1 << 6) == 0 {
            return Ok(());
        }
        // SAFETY: fixed TXT public and TPM locality-zero MMIO, read only.
        let txt = unsafe { fstart_core::mmio::read32(0xfed3_0010 as *const u32) };
        if txt & 1 == 0 {
            return Ok(());
        }
        // SAFETY: as above.
        let cleared = fstart_arch::x86::wait_us(1_000_000, || unsafe {
            fstart_core::mmio::read8(0xfed4_0000 as *const u8) & (1 << 7) != 0
        });
        if cleared {
            Ok(())
        } else {
            Err(ServiceError::Timeout)
        }
    }

    fn reset(&self) -> ! {
        fstart_log::flush();
        fstart_arch::x86::boot::system_reset(true)
    }

    /// The memory map from the host bridge (coreboot `mc_read_resources`):
    /// (top of usable low DRAM, TOLUD-equivalent reserved top, TOUUD).
    fn memory_map(&self) -> Result<(u32, u32, u64), ServiceError> {
        let hb = self.host();
        let touud = (u64::from(hb.touud_hi.get()) << 32) | u64::from(hb.touud.get() & 0xfff0_0000);
        let tolud = hb.tolud.get() & 0xfff0_0000;
        let tom = (u64::from(hb.tom_hi.get()) << 32) | u64::from(hb.tom.get() & 0xfff0_0000);
        let me_base = (u64::from(hb.meseg_base_hi.get()) << 32)
            | u64::from(hb.meseg_base.get() & 0xfff0_0000);
        let mut reserved_top = tolud;
        if me_base == u64::from(tolud) {
            // The ME UMA sits between TOLUD and TOM.
            reserved_top = tom.min(0x1_0000_0000) as u32;
        }
        let tseg = hb.tsegmb.get() & 0xfff0_0000;
        let mut usable = tseg;
        // DMA protected range below TSEG (TXT): DPR bit 2 EPM, size [11:4] MiB.
        let dpr = hb.dpr.get();
        if dpr & (1 << 2) != 0 {
            usable -= ((dpr >> 4) & 0xff) << 20;
        }
        if usable <= 0x10_0000 || u64::from(tolud) > tom || tseg > tolud {
            return Err(ServiceError::HardwareError);
        }
        Ok((usable, reserved_top, touud))
    }

    fn tseg(&self) -> Option<(u64, u32)> {
        if !self.dram_ready {
            return None;
        }
        let base = self.host().tsegmb.get() & 0xfff0_0000;
        (base != 0).then_some((u64::from(base), self.config.tseg_mib << 20))
    }

    /// coreboot `northbridge_dmi_init`.
    fn northbridge_dmi_init(&self) {
        let d = self.dmi();
        let sandy = self.is_sandy();
        let stepping = self.cpuid() & 0xf;
        if sandy {
            d.cir_250.set((d.cir_250.get() & !(7 << 20)) | 2 << 20);
        }
        d.lltc.set(d.lltc.get() | 1 << 29);
        if sandy && stepping == 3 {
            d.cir_bc8
                .set((d.cir_bc8.get() & !(0xfff << 7)) | 0x7d3 << 7);
        }
        if !sandy || stepping >= 6 {
            d.cir_1f8.set((d.cir_1f8.get() & !(1 << 26)) | 1 << 16);
            d.cir_1fc.set(d.cir_1fc.get() | 1 << 12 | 1 << 23);
        } else if stepping >= 5 {
            d.cir_1f8.set(d.cir_1f8.get() | 1 << 16);
        }
        d.uests.set(0xffff_ffff);
        d.cests.set(0xffff_ffff);
        if !sandy {
            d.cir_c34.set(0xffff_ffff);
        }
        if sandy {
            d.cir_d04.set(d.cir_d04.get() | 1 << 4);
        }
        d.lctl.set(d.lctl.get() | 3);
    }

    /// coreboot `disable_peg`.
    fn disable_unused_devices(&self) {
        let dev = self.config.devices;
        let hb = self.host();
        let mut deven = hb.deven.extract();
        let bridge_active =
            |enabled: bool, d: u8, f: u8| enabled && ecam::EcamDevice::new(0, d, f).is_present();
        for (field, active) in [
            (DEVEN::PEG12, bridge_active(dev.peg12, 1, 2)),
            (DEVEN::PEG11, bridge_active(dev.peg11, 1, 1)),
            (DEVEN::PEG10, bridge_active(dev.peg10, 1, 0)),
            (DEVEN::IGD, dev.igd),
            (DEVEN::D4, dev.dev4),
            (DEVEN::PEG60, bridge_active(dev.peg60, 6, 0)),
            (DEVEN::D7, dev.dev7),
        ] {
            if !active {
                deven.modify(field.val(0));
            }
        }
        hb.deven.set(deven.get());
        let pegctl = &self.mch().pegctl;
        if deven.read(DEVEN::PEG60) == 0
            && deven.read(DEVEN::PEG10) == 0
            && deven.read(DEVEN::PEG11) == 0
            && deven.read(DEVEN::PEG12) == 0
        {
            // Clock gate all PEG controllers.
            pegctl.set(pegctl.get() | 1);
        } else {
            pegctl.set(pegctl.get() & !1);
        }
    }

    /// coreboot `northbridge_init`, the host bridge device init.
    fn northbridge_init(&self) {
        self.northbridge_dmi_init();
        let mch = self.mch();
        let mut bridge_type = mch.sapmtimers.get() & !0xff;
        if self.is_sandy() {
            bridge_type |= 0x20;
        } else {
            // Power-aware interrupt routing, fixed priority.
            mch.intrdirctl.set((mch.intrdirctl.get() & !0xf) | 0x4);
            bridge_type |= 0x30;
        }
        mch.sapmtimers.set(bridge_type);
        // Before BIOS_RESET_CPL.
        self.disable_unused_devices();
        mch.bios_reset_cpl.set(mch.bios_reset_cpl.get() | 1);
        fstart_log::info!("sandybridge: set BIOS_RESET_CPL");
        // Turbo power limits 1 ms after reset complete.
        fstart_arch::udelay(1000);
        model_206ax::set_power_limits(&self.config.cpu, 28);
        if model_206ax::config_tdp_levels() != 0 {
            // SAFETY: model 206AX/306AX implements MSR_PKG_POWER_LIMIT.
            let limit = unsafe { fstart_arch::x86::msr::rdmsr(0x610) };
            mch.pkg_power_limit_lo.set(limit as u32);
            mch.pkg_power_limit_hi.set((limit >> 32) as u32);
        }
        // Before graphics PM init.
        mch.pavp_msg.set(0x0010_0001);
    }

    /// SMBIOS inventory from the trained modules (coreboot
    /// `setup_sdram_meminfo`).
    fn record_memory_info(&mut self, init: &raminit::Raminit, spds: &raminit::Spds) {
        use fstart_core::memory_info::{MemoryDevice, MemoryInfo};
        use fstart_driver_spd::ddr3;
        let capid = self.host().capid0_a.extract();
        let slots = 1 + u32::from(!capid.is_set(CAPID0_A::DDPCD));
        let channels = 1 + u32::from(!capid.is_set(CAPID0_A::PDCD));
        let per_channel = match capid.read(CAPID0_A::DDRSZ) {
            1 => 8192,
            2 => 2048,
            3 => 512,
            _ => 16384,
        } / 2
            * slots;
        let mut info = MemoryInfo::new(channels * per_channel);
        if !capid.is_set(CAPID0_A::ECCDIS) && init.c.ecc_enabled {
            info.ecc = 0x05; // single-bit ECC
        }
        let mts = (2 * (1000 << 8) / init.c.t_ck.max(1)) as u16;
        for ch in 0..raminit::NUM_CHANNELS {
            for slot in 0..raminit::NUM_SLOTS {
                let spd = &spds[2 * ch + slot];
                let device = match ddr3::decode_dimm(spd) {
                    Ok(dimm) if init.c.dimms[ch][slot].size_mb != 0 => {
                        ddr3::memory_device(&dimm, spd, ch as u8, slot as u8, mts)
                    }
                    _ if init.c.dimms[ch][slot].size_mb != 0 => {
                        MemoryDevice::unknown(ch as u8, slot as u8)
                    }
                    _ => MemoryDevice::empty(ch as u8, slot as u8),
                };
                info.push(device);
            }
        }
        self.memory = Some(info);
    }

    /// coreboot `init_dram_ddr3` with the platform's training cache.
    fn raminit(
        &mut self,
        smbus: &mut dyn SmBus,
        cached: Option<&[u8]>,
    ) -> Result<Option<raminit::TrainingRecord>, ServiceError> {
        let s3 = self.boot_path == BootPath::S3Resume;
        let cpuid = self.cpuid();
        let spds = raminit::read_spds(smbus, &self.config.spd_addresses)?;
        let crcs = raminit::spd_crcs(&spds);
        let me = crate::bd82x6x::me::Me::new();
        let mut cached =
            cached.and_then(|bytes| raminit::TrainingRecord::parse(bytes, cpuid, &crcs));
        if s3 && cached.is_none() {
            fstart_log::error!("sandybridge: S3 resume without matching training");
            return Err(ServiceError::HardwareError);
        }
        if cached.is_some() && !s3 {
            let since_boot_ms = (fstart_arch::x86::timestamp_us() / 1000) as u32;
            match me.cpu_replaced(since_boot_ms) {
                Ok(true) => cached = None,
                Ok(false) => {}
                Err(action) => {
                    // Dynamic fusing: DRAM init not done for the MRC, reset.
                    let lpc = ecam::EcamDevice::new(0, 0x1f, 0);
                    lpc.and8(0xa2, 0x7f);
                    action.perform();
                    self.reset();
                }
            }
        }
        let me_uma = me.uma_size_mib();
        let mut init = raminit::Raminit::new(
            self.mch(),
            self.host(),
            raminit::RaminitConfig {
                spd_addresses: self.config.spd_addresses,
                max_mem_clock_mhz: self.config.max_mem_clock_mhz,
                mobile: self.mobile(),
                me_uma_mib: me_uma,
                tseg_mib: self.config.tseg_mib,
                ignore_xmp_max_dimms: false,
                ignore_xmp_voltage: false,
                enable_ecc: self.config.ecc,
            },
        );
        let fast_boot = init.init(cpuid, &spds, cached.as_ref(), s3)?;
        init.finish(s3);
        let action = me.init_done(init.me_uma_base_mib());
        action.perform();
        me.log_status();
        init.report();
        self.record_memory_info(&init, &spds);
        if fast_boot {
            return Ok(None);
        }
        Ok(raminit::TrainingRecord::capture(&init.c))
    }
}

/// The user's stolen-memory sizes: coreboot's `gfx_uma_size` choices.
const UMA_SIZES: &[fstart_options::Choice] = &[
    fstart_options::Choice::new(32, "32 MiB"),
    fstart_options::Choice::new(64, "64 MiB"),
    fstart_options::Choice::new(96, "96 MiB"),
    fstart_options::Choice::new(128, "128 MiB"),
    fstart_options::Choice::new(160, "160 MiB"),
    fstart_options::Choice::new(192, "192 MiB"),
    fstart_options::Choice::new(224, "224 MiB"),
];

impl IntelSandybridgeConfig {
    /// The board's values of the user options.
    pub const fn options(&self) -> crate::igd::options::Options {
        crate::igd::options::Options::new(self.devices.igd, (self.gfx_uma_index as u16 + 1) * 32)
    }
}

impl IntelSandybridge {
    /// The GMS index (stolen memory in 32 MiB steps, minus one) of the
    /// user's stolen memory.
    fn gms_index(&self) -> u8 {
        match self.options.uma_mb {
            mb @ 32..=512 if mb % 32 == 0 => (mb / 32 - 1) as u8,
            _ => self.config.gfx_uma_index,
        }
    }
}

impl crate::IntelNorthbridgeDriver for IntelSandybridge {
    const ACPI_PCI_ROOT_NAME: &'static str = "PCI0";
    const ACPI_IGD_NAME: &'static str = "GFX0";
    type Config = IntelSandybridgeConfig;
    type Options = crate::igd::options::Options;
    const KNOBS: &'static [fstart_options::Knob<Self::Options>] =
        &[crate::igd::options::uma(UMA_SIZES)];

    fn board_options(config: &Self::Config) -> Self::Options {
        config.options()
    }

    fn options(&self) -> Self::Options {
        self.options
    }

    fn set_options(&mut self, options: Self::Options) {
        self.options = options;
    }

    /// The root bridge: what every host bridge forwards, PCI MMIO above
    /// TOUUD (coreboot `A4GB`/`A4GS`), and the chipset's own decodes
    /// (coreboot `hostbridge.asl` `PDRC`).
    fn pci_root(&self) -> Result<fstart_pci::PciRoot, fstart_pci::PciEcamError> {
        use fstart_pci::PciRole::Own;
        let config = self.config;
        let touud = self
            .dram_ready
            .then(|| self.memory_map().ok())
            .flatten()
            .map_or(1 << 32, |(_, _, touud)| touud);
        let above_4g = Some((
            touud.max(1 << 32),
            1 << fstart_arch::x86::physical_address_bits(),
        ));
        let chipset = [
            Own.mmio(config.ecam_base, u64::from(config.ecam_buses) << 20),
            Own.mmio(config.rcba, 0x4000),
            Own.mmio(config.mchbar, 0x8000),
            Own.mmio(config.dmibar, 0x1000),
            Own.mmio(config.epbar, 0x1000),
            Own.mmio(GFXVT_BASE, 0x1000),
            Own.mmio(VTVC0_BASE, 0x1000),
            // TXT, and the chipset's reserved block above the TPM.
            Own.mmio(0xfed2_0000, 0x2_0000),
            Own.mmio(0xfed4_5000, 0x4_b000),
        ];
        fstart_pci::PciRoot::new(
            PciRootInfo {
                segment: 0,
                ecam_base: config.ecam_base,
                bus_start: 0,
                bus_end: (config.ecam_buses - 1) as u8,
            },
            (crate::host_bridge::forwarded(self.low_dram_end, above_4g).into_iter()).chain(chipset),
        )
    }

    fn new_from_config(config: &'static Self::Config) -> Result<Self, ServiceError> {
        let buses = config.ecam_buses;
        if !matches!(buses, 64 | 128 | 256)
            || config.ecam_base == 0
            || config.ecam_base & (u64::from(buses) * 1024 * 1024 - 1) != 0
            || config
                .ecam_base
                .checked_add(u64::from(buses) << 20)
                .is_none_or(|end| end > 0x1_0000_0000)
            || config.mchbar & 0x7fff != 0
            || [config.mchbar, config.dmibar, config.epbar]
                .iter()
                .any(|base| *base == 0 || *base > u32::MAX as u64 || base & 0xfff != 0)
            || config.gfx_uma_index > 15
            || config.tseg_mib << 20 < model_206ax::TSEG_SIZE
            || !config.tseg_mib.is_power_of_two()
        {
            return Err(ServiceError::InvalidParam);
        }
        Ok(Self {
            config,
            boot_path: BootPath::Normal,
            dram_ready: false,
            options: Self::board_options(config),
            low_dram_end: None,
            memory: None,
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

    /// Bootblock: the system-agent early init. The ramstage finds the
    /// raminit marker and only recovers the decode.
    fn early_init(&mut self) -> Result<(), ServiceError> {
        self.enable_ecam();
        self.dram_ready = self.raminit_done() && self.memory_map().is_ok();
        if !self.dram_ready {
            self.systemagent_early_init();
        }
        Ok(())
    }

    fn set_boot_path(&mut self, boot_path: BootPath) {
        self.boot_path = boot_path;
    }

    fn dram_link_init(&mut self) -> Result<(), ServiceError> {
        // coreboot romstage: DRAM already trained in this power cycle means a
        // reset after raminit; start over.
        if self.raminit_done() {
            fstart_log::info!("sandybridge: raminit marker set, resetting");
            self.reset();
        }
        let mch = self.mch();
        mch.sapmctl.set(mch.sapmctl.get() | 1);
        // Wait for the ME to size its UMA.
        crate::bd82x6x::me::Me::new().early_init();
        self.wait_txt_clear()?;
        // MSR_LT_LOCK_MEMORY.
        // SAFETY: model 206AX/306AX implements MSR 0x2e6.
        unsafe { fstart_arch::x86::msr::wrmsr(0x2e6, 0) };
        let lpc = ecam::EcamDevice::new(0, 0x1f, 0);
        if lpc.read16(0xa2) & 0xa0 == 0x20
            && mch.sskpd.get() != 0
            && self.boot_path != BootPath::S3Resume
        {
            mch.sskpd.set(0);
            self.reset();
        }
        // coreboot `early_init_dmi`, after the PCH's DMI pre-init.
        self.dmi_recipe();
        let d = self.dmi();
        // Write-once: 5 GT/s, L0s 128-256 ns, L1 2-4 us.
        d.lcap
            .set((d.lcap.get() & !0x3f00f) | 2 | 2 << 12 | 2 << 15);
        for _ in 0..2 {
            d.lctl.set(d.lctl.get() | 1 << 5);
            while d.lsts.is_set(DMI_LSTS::TRAINING) {
                core::hint::spin_loop();
            }
        }
        let lsts = d.lsts.extract();
        fstart_log::info!(
            "sandybridge: DMI x{} at {} MT/s",
            lsts.read(DMI_LSTS::WIDTH),
            u32::from(lsts.read(DMI_LSTS::SPEED)) * 2500
        );
        // VC0: TC0/3/4; VC1: TC1/5; VCp: TC2/6; VCm: TC7.
        d.vc0rctl.set(1 << 31 | 0x0c << 1 | 1);
        d.vc1rctl.set(1 << 31 | 1 << 24 | 0x11 << 1);
        d.vcprctl.set(1 << 31 | 2 << 24 | 0x22 << 1);
        d.vcmrctl.set(1 << 31 | 7 << 24 | 0x40 << 1);
        // Extended VC count: VC1 is active.
        d.pvccap1.set(d.pvccap1.get() | 1);
        Ok(())
    }

    fn finish_dram_link_init(&mut self) -> Result<(), ServiceError> {
        let d = self.dmi();
        for status in [&d.vc0rsts, &d.vc1rsts, &d.vcprsts, &d.vcmrsts] {
            let done =
                fstart_arch::x86::wait_us(100_000, || !status.is_set(VC_RSTS::NEGOTIATION_PENDING));
            if !done {
                fstart_log::error!("sandybridge: DMI VC negotiation timeout");
                return Err(ServiceError::Timeout);
            }
        }
        Ok(())
    }

    fn early_post_dram_init(&mut self) -> Result<(), ServiceError> {
        // coreboot `northbridge_romstage_finalize`.
        let sskpd_hi = &self.mch().sskpd_hi;
        sskpd_hi.set((sskpd_hi.get() & !0xffff) | RAMINIT_DONE);
        Ok(())
    }

    fn supports_s3_replay(&self) -> bool {
        true
    }

    fn training_identity(&self) -> Option<[u8; 32]> {
        let hb = self.host();
        Some(crate::generic::training::identity(
            b"sandybridge-raminit-v1",
            &self.config.spd_addresses,
            u16::from(self.gms_index()),
            (self.config.max_mem_clock_mhz / 133) as u8,
            crate::generic::training::HardwareIdentity {
                vendor_id: hb.vendor_id.get(),
                device_id: hb.device_id.get(),
                revision_id: hb.revision_id.get(),
                capabilities: [hb.capid0_a.get(), hb.capid0_b.get()],
                cpu_signature: self.cpuid(),
            },
        ))
    }

    fn memory_info(&self) -> Option<fstart_core::memory_info::MemoryInfo> {
        self.memory
    }

    fn stage_local_init(&mut self) -> Result<(), ServiceError> {
        self.memory_map()?;
        self.dram_ready = true;
        self.igd_func0_init(false);
        Ok(())
    }

    fn post_dram_init(&mut self) -> Result<(), ServiceError> {
        self.northbridge_init();
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
        self.memory_map()?;
        self.dram_ready = true;
        // coreboot runs gma_func0_init on resume, without the modeset.
        self.igd_func0_init(true);
        self.igd_restore_opregion(opregion);
        Ok(())
    }

    fn finalize_init(&mut self) -> Result<(), ServiceError> {
        // coreboot `intel_sandybridge_finalize_smm`.
        let hb = self.host();
        hb.ggc.modify(GGC::LOCK::SET);
        hb.pavpc.set(hb.pavpc.get() | 1 << 2);
        hb.dpr.set(hb.dpr.get() | 1);
        hb.meseg_mask.modify(regs::MESEG_MASK::LOCK::SET);
        for reg in [
            &hb.remapbase,
            &hb.remaplimit,
            &hb.tom,
            &hb.touud,
            &hb.bdsm,
            &hb.bgsm,
            &hb.tsegmb,
            &hb.tolud,
        ] {
            reg.set(reg.get() | 1);
        }
        let mch = self.mch();
        mch.pavp_msg.set(mch.pavp_msg.get() | 1);
        mch.sapmctl.set(mch.sapmctl.get() | 1 << 31);
        mch.umagfxctl.set(mch.umagfxctl.get() | 1);
        mch.vtdtrklck.set(mch.vtdtrklck.get() | 1);
        mch.reqlim.set(mch.reqlim.get() | 1 << 31);
        mch.dmivclim.set(mch.dmivclim.get() | 1 << 31);
        mch.crdtlck.set(mch.crdtlck.get() | 1);
        // Memory controller lockdown.
        mch.mc_lock.set(0x8f);
        // Read and write back.
        for reg in [&mch.vdmbdfbarkvm, &mch.vdmbdfbarpavp, &mch.hdaudrid] {
            reg.set(reg.get());
        }
        // coreboot `intel_model_206ax_finalize_smm`.
        model_206ax::finalize();
        Ok(())
    }

    fn memory_detected(&mut self, state: &fstart_core::services::memory_detect::E820State) {
        self.low_dram_end = crate::host_bridge::low_dram_end(state);
    }

    fn framebuffer_info(&self) -> Option<fstart_core::services::FramebufferInfo> {
        self.display.framebuffer_info()
    }
}

impl fstart_core::services::MemoryInit for IntelSandybridge {
    type Input<'a> = crate::SpdTraining<'a>;
    type Output = Option<usize>;

    /// Train, or replay the cached training on a matching fast boot or S3
    /// resume; capture fresh training for the cache.
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
        let record = self.raminit(smbus, cached)?;
        self.memory_map()?;
        self.dram_ready = true;
        let Some(record) = record else {
            return Ok(None);
        };
        if capture.is_empty() || self.boot_path == BootPath::S3Resume {
            return Ok(None);
        }
        let bytes = record.as_bytes();
        capture
            .get_mut(..bytes.len())
            .ok_or(ServiceError::InvalidParam)?
            .copy_from_slice(bytes);
        Ok(Some(bytes.len()))
    }
}

/// The PC-compatible map, with Sandy Bridge's sighting pages reserved
/// (coreboot `mc_read_resources`).
fn build_sandybridge_e820(
    usable: u32,
    reserved_top: u32,
    touud: u64,
    tom: u64,
    sandy: bool,
    entries: &mut [E820Entry],
) -> Result<usize, ServiceError> {
    let mut state = E820State::new();
    let count = build_pc_compatible_e820(state.entries_mut(), usable, touud, reserved_top)?;
    state.set_detected(count, tom)?;
    if sandy {
        for (base, size) in SNB_BAD_RAM {
            state.reserve_range_as(base, size, E820Kind::Reserved)?;
        }
    }
    let count = state.count();
    entries
        .get_mut(..count)
        .ok_or(ServiceError::NotSupported)?
        .copy_from_slice(state.entries());
    Ok(count)
}

impl MemoryDetector for IntelSandybridge {
    fn detect_memory(&self, entries: &mut [E820Entry]) -> Result<usize, ServiceError> {
        if !self.dram_ready {
            return Err(ServiceError::NotInitialized);
        }
        let (usable, reserved_top, touud) = self.memory_map()?;
        let count = build_sandybridge_e820(
            usable,
            reserved_top,
            touud,
            self.total_ram_bytes()?,
            self.is_sandy(),
            entries,
        )?;
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
        let hb = self.host();
        Ok((u64::from(hb.tom_hi.get()) << 32) | u64::from(hb.tom.get() & 0xfff0_0000))
    }
}

impl fstart_arch::x86::cpu::intel::smm::SmramControl for IntelSandybridge {
    fn tseg(&self) -> Option<(u64, u32)> {
        IntelSandybridge::tseg(self)
    }
    fn smram_open(&self) -> bool {
        self.host().smram.set(crate::gmch::smram::open());
        crate::gmch::smram::is_open(self.host().smram.get())
    }
    fn smram_close(&self) {
        self.host().smram.set(crate::gmch::smram::closed());
    }
    fn smram_lock(&self) -> bool {
        self.host().smram.set(crate::gmch::smram::locked());
        crate::gmch::smram::is_locked(self.host().smram.get()) && self.tseg().is_some()
    }
}

mod acpi;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e820_reserves_the_snb_sighting_pages() {
        let mut entries = [E820Entry::zeroed(); 16];
        let count = build_sandybridge_e820(
            0x8000_0000,
            0x8000_0000,
            0x1_0000_0000,
            0x8000_0000,
            true,
            &mut entries,
        )
        .unwrap();
        let reserved: heapless::Vec<(u64, u64), 16> = entries[..count]
            .iter()
            .filter(|e| e.kind == E820Kind::Reserved as u32 && e.addr >= 0x10_0000)
            .map(|e| (e.addr, e.size))
            .collect();
        assert_eq!(
            &reserved[..],
            &[(0x2000_0000, 0x20_0000), (0x4000_0000, 0x20_0000)]
        );
        let ram: u64 = entries[..count]
            .iter()
            .filter(|e| e.kind == E820Kind::Ram as u32)
            .map(|e| e.size)
            .sum();
        assert_eq!(ram, 0x9e000 + 0x8000_0000 - 0x10_0000 - 0x40_0000);
    }

    #[test]
    fn invalid_config_fails_before_hardware_access() {
        use crate::IntelNorthbridgeDriver;
        static BAD: IntelSandybridgeConfig = IntelSandybridgeConfig {
            mchbar: 0xfed1_4000,
            ..IntelSandybridgeConfig::new()
        };
        assert!(IntelSandybridge::new_from_config(&BAD).is_err());
        static GOOD: IntelSandybridgeConfig = IntelSandybridgeConfig::new();
        assert!(IntelSandybridge::new_from_config(&GOOD).is_ok());
    }
}
