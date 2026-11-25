//! Intel 8 Series PCH-H and PCH-LP (Lynx Point).
//!
//! Pads, decode windows, USB port tables, and interrupt assignments are board
//! inputs; no board names enter the driver.

mod acpi;
mod devices;
#[cfg(test)]
mod finalize_tests;
pub mod gpio;
mod hsio;
mod hsio_tables;
pub mod me;
mod pcie;
mod power;
pub mod regs;
pub mod smm;
mod usb;

use crate::{
    IntelSouthbridgeDriver,
    southbridge::{
        gpio_ich::{GpioConfig, IchGpio},
        hda::HdaConfig,
        ich,
        lpc::{self, LpcGenericIoDecode},
        options::{self, Options},
        pmio_ich::{self as pmio, PmIo},
        smbus::I801SmBus,
        smi::{self, SmiControl},
    },
};
use core::sync::atomic::{AtomicBool, Ordering};
use fstart_core::{
    mmio::MmioWindow,
    services::{ServiceError, SmBus},
};
use fstart_pci::{PciFixedBar, PciFixedBarType, PciFixedBars, ecam::EcamDevice};
use regs::{
    ACPI_CNTL, ETR3, FD, FD2, FDSW, GCS, GEN_PMCON_1, GEN_PMCON_LOCK, GPIO_CNTL, HSFS,
    LPC_FINAL_LOCK, LynxPointLpcConfig, OIC, PMSYNC_CONFIG, PRSTS, RC, RCBA as RCBA_REG, RcbaRegs,
    TCLOCKDN, ThermalMmio, dev,
};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

pub use crate::southbridge::ehci_debug::EhciController;
pub use usb::{Location, Usb2Port, Usb3Port};

/// Fixed Root Complex Base Address (`RCBA`, 16 KiB).
pub const RCBA: usize = 0xfed1_c000;

/// Raw 16 KiB RCBA MMIO window for undocumented chipset-initialization writes
/// and shared southbridge helpers.
pub(crate) fn rcba_window() -> MmioWindow {
    // SAFETY: RCBA is programmed and enabled at `RCBA` in `pre_console_init`.
    unsafe { crate::southbridge::rcba_window(RCBA) }
}

/// Typed named RCBA registers.
pub(crate) fn rcba_regs() -> &'static RcbaRegs {
    // SAFETY: RCBA is programmed and enabled at `RCBA` in `pre_console_init`.
    unsafe { &*(RCBA as *const RcbaRegs) }
}

/// Typed LPC bridge (`00:1f.0`) PCI configuration registers.
pub(crate) fn lpc_regs() -> &'static LynxPointLpcConfig {
    // SAFETY: ECAM is bound by the northbridge before southbridge access.
    unsafe { lpc::device().regs::<LynxPointLpcConfig>() }
}

#[derive(Clone, Copy, Debug)]
pub enum Gpios {
    Lp(&'static gpio::PadTable),
    Traditional(&'static GpioConfig),
}

impl Gpios {
    #[must_use]
    pub const fn lp(self) -> bool {
        matches!(self, Self::Lp(_))
    }
}

/// `DxxIP` per-function nibbles (PCI interrupt pin `0..4`), `DxxIR` per-pin
/// nibbles (PIRQ `A..H`). `None` leaves the reset assignment untouched.
#[derive(Clone, Copy, Debug)]
pub struct IrqRoute {
    pub device: u8,
    pub pins: Option<u32>,
    pub pirqs: Option<u16>,
}

impl IrqRoute {
    fn offsets(self) -> Option<(Option<u32>, u32)> {
        Some(match self.device {
            31 => (Some(0x3100), 0x3140),
            30 => (Some(0x3104), 0x3142),
            29 => (Some(0x3108), 0x3144),
            28 => (Some(0x310c), 0x3146),
            27 => (Some(0x3110), 0x3148),
            26 => (Some(0x3114), 0x314c),
            25 => (Some(0x3118), 0x3150),
            23 => (None, 0x3158),
            22 => (Some(0x3124), 0x315c),
            20 => (Some(0x3128), 0x3160),
            21 => (None, 0x3164),
            19 => (None, 0x3168),
            _ => return None,
        })
    }
}

/// Bitmask of Lynx Point-LP Serial IO controllers (bit `i` = function `i` of
/// `00:15.x`, or `00:17.0` for SDIO).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SerialIoMask(pub u8);

impl SerialIoMask {
    pub const EMPTY: Self = Self(0);
    pub const SDMA: u8 = 1 << 0;
    pub const I2C0: u8 = 1 << 1;
    pub const I2C1: u8 = 1 << 2;
    pub const SPI0: u8 = 1 << 3;
    pub const SPI1: u8 = 1 << 4;
    pub const UART0: u8 = 1 << 5;
    pub const UART1: u8 = 1 << 6;
    pub const SDIO: u8 = 1 << 7;

    /// True if all controller bits in `bits` are set.
    #[must_use]
    pub const fn contains(self, bits: u8) -> bool {
        self.0 & bits == bits
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LynxPointConfig {
    pub gpio: Gpios,
    pub lpc_generic_io: &'static [LpcGenericIoDecode],
    pub irq: &'static [IrqRoute],
    pub gpi_routing: [crate::southbridge::gpio_ich::GpiRoute; 16],
    pub gpe0_en: [u32; 4],
    pub alt_gp_smi_en: u32,
    pub power_on_after_fail: u8,
    pub nmi: bool,
    pub usb2: &'static [Usb2Port],
    pub usb3: &'static [Usb3Port],
    pub xhci_default: bool,
    pub usb_xhci_on_resume: bool,
    pub sata_port_map: u8,
    pub sata_hotplug_map: u8,
    pub sata_devslp_mux: bool,
    pub sata_devslp_disable: bool,
    /// Per-port Gen3 transmitter override (ports 0/1, zero preserves reset).
    pub sata_gen3_tx: [u32; 2],
    /// Optional 4-bit Gen3 DATA and EDGE equalization trim for ports 0/1.
    pub sata_gen3_dtle: [Option<u8>; 2],
    /// Board's unused PCIe/XDP clock outputs, disabled through ME9 ICC.
    pub icc_clock_disable: u32,
    pub hda: Option<&'static HdaConfig>,
    pub pcie_ports: u8,
    pub pcie_force_aspm: u8,
    pub serialio_enable: u8,
    pub sio_acpi_mode: bool,
    pub sio_i2c_1v8: [bool; 2],
    pub gbe: bool,
    pub adsp: bool,
    pub mei_aux: u8,
    pub ec_present: bool,
    pub me_disable: bool,
    /// Require SMM BIOS-write protection at handoff (requires installed SMM).
    pub smm_bios_protection: bool,
    pub xhci_enabled: bool,
    pub ehci_enabled: bool,
    /// Optional EHCI controller wired to the board's USB debug port (coreboot
    /// `USBDEBUG_HCD_INDEX`). On Lynx Point-LP only [`EhciController::Ehci1`]
    /// exists; on Lynx Point-H `None` prefers EHCI2 then EHCI1.
    pub ehci_debug_controller: Option<EhciController>,
    pub thermal_enabled: bool,
    pub smbus_enabled: bool,
    /// Advertise ACPI S1 (standby) when supported by the board.
    pub s1: bool,
    /// Advertise ACPI FADT `SEALED_CASE` for sealed/non-expandable chassis.
    pub sealed_case: bool,
    /// A PIC-only payload cannot consume the APIC-only PCI routing table.
    pub pic_only_payload: bool,
}

impl LynxPointConfig {
    #[must_use]
    pub const fn new(gpio: Gpios) -> Self {
        Self {
            gpio,
            lpc_generic_io: &[],
            irq: &[],
            gpi_routing: [crate::southbridge::gpio_ich::GpiRoute::Disabled; 16],
            gpe0_en: [0; 4],
            alt_gp_smi_en: 0,
            power_on_after_fail: 0,
            nmi: false,
            usb2: &[],
            usb3: &[],
            xhci_default: true,
            usb_xhci_on_resume: true,
            sata_port_map: 1,
            sata_hotplug_map: 0,
            sata_devslp_mux: false,
            sata_devslp_disable: false,
            sata_gen3_tx: [0; 2],
            sata_gen3_dtle: [None; 2],
            icc_clock_disable: 0,
            hda: None,
            pcie_ports: if gpio.lp() { 63 } else { 255 },
            pcie_force_aspm: 0,
            serialio_enable: 0,
            sio_acpi_mode: false,
            sio_i2c_1v8: [false; 2],
            gbe: false,
            adsp: false,
            mei_aux: 0,
            ec_present: false,
            me_disable: true,
            smm_bios_protection: false,
            xhci_enabled: true,
            ehci_enabled: true,
            ehci_debug_controller: None,
            thermal_enabled: true,
            smbus_enabled: true,
            s1: false,
            sealed_case: false,
            pic_only_payload: false,
        }
    }

    #[must_use]
    pub const fn pm_base(&self) -> u16 {
        if self.gpio.lp() { 0x1000 } else { 0x500 }
    }

    #[must_use]
    pub const fn gpio_base(&self) -> u16 {
        if self.gpio.lp() { 0x1400 } else { 0x480 }
    }

    /// The EHCI debug console if the platform selects USB debug.
    #[must_use]
    pub fn ehci_debug(
        &self,
        ecam_base: usize,
        ecam_size: u64,
    ) -> Option<fstart_driver_usbdebug::Config> {
        let controller = EhciController::select(self.ehci_debug_controller, |controller| {
            self.ehci_enabled && (controller == EhciController::Ehci1 || !self.gpio.lp())
        })?;
        let devfn = match controller {
            EhciController::Ehci1 => dev::EHCI1,
            EhciController::Ehci2 => dev::EHCI2,
        };
        crate::southbridge::ehci_debug::config_with_ecam_size(
            devfn,
            ecam_base,
            ecam_size,
            &[(RCBA as u64, 0x4000)],
        )
    }
}

pub struct LynxPoint {
    config: &'static LynxPointConfig,
    bus: I801SmBus,
    s3: bool,
    resume: bool,
    ehci_debug: Option<(u8, u32)>,
    serial_bars: [Option<(u32, u32)>; 8],
    smm_installed: AtomicBool,
    hpet_id: u32,
    d28ir: u16,
    root_functions: [Option<u8>; 8],
    requested_ports: u8,
    options: Options,
}

impl LynxPoint {
    /// ACPI-mode controllers with validated assigned BARs, for board AML children.
    pub fn serial_io_populated(&self) -> SerialIoMask {
        SerialIoMask(
            self.serial_bars
                .iter()
                .enumerate()
                .fold(0, |m, (i, b)| m | (u8::from(b.is_some()) << i)),
        )
    }

    /// Configured Lynx Point-LP GPIO controller, if this PCH is LP.
    #[must_use]
    pub const fn lp_gpio(&self) -> Option<gpio::LpGpio> {
        if self.config.gpio.lp() {
            Some(gpio::LpGpio::new(self.config.gpio_base()))
        } else {
            None
        }
    }

    /// Populate synthetic PCIe root-port and Serial IO BAR state for host ACPI
    /// baseline assembly where PCI enumeration does not run.
    #[cfg(not(target_os = "none"))]
    pub fn set_host_acpi_baseline(&mut self) {
        let max_ports = if self.config.gpio.lp() { 6 } else { 8 };
        for n in 0..max_ports {
            if self.requested_ports & (1 << n) != 0 {
                self.root_functions[usize::from(n)] = Some(n);
            }
        }
        if self.config.sio_acpi_mode {
            for i in 0..8u8 {
                if self.config.serialio_enable & (1 << i) != 0 {
                    let b0 = 0xe000_0000 + u32::from(i) * 0x2000;
                    self.serial_bars[usize::from(i)] = Some((b0, b0 + 0x1000));
                }
            }
        }
    }

    /// Before `chip_init`, suppress physical ports for optional, unpopulated
    /// slots. Neither board wiring nor lane ownership can be expanded here.
    pub fn disable_root_ports(&mut self, mask: u8) {
        self.requested_ports &= !mask;
    }

    /// Postcar has no stage-local ECAM binding. Clear the DRAM-init and
    /// global reset indicators via config I/O before requesting a power cycle.
    pub fn reset_failed_resume() -> ! {
        use fstart_pci::cf8;
        unsafe {
            cf8::write8(0, 31, 0, 0xa2, cf8::read8(0, 31, 0, 0xa2) & !128);
            cf8::write32(0, 31, 0, 0xac, cf8::read32(0, 31, 0, 0xac) & !(1 << 20));
        }
        fstart_arch::x86::boot::full_reset()
    }

    pub fn smi(&self) -> smi::IchSmi {
        smi::IchSmi::new(
            self.config.pm_base(),
            if self.config.gpio.lp() {
                smi::LYNXPOINT_LP_GPE0
            } else {
                smi::LYNXPOINT_GPE0
            },
        )
        .with_sleep_smi(self.config.gpio_base())
        .with_bios_protection(self.config.smm_bios_protection)
        .with_power_on_after_fail(self.options.power_on_after_fail == 1)
    }

    fn pm(&self) -> PmIo {
        PmIo::with_size(self.config.pm_base(), pmio::LYNXPOINT_PMSIZE)
    }

    fn sata_mask(&self) -> u8 {
        if self.config.gpio.lp() { 15 } else { 63 }
    }
}

/// Program the SPI opcode menu before latching `RCBA+0x3a6c` bit 0.
pub(crate) fn finalize_lpc_menu(rcba: MmioWindow) {
    /// RCBA `0x3a6c` (32 bits), LPC final configuration lock, bit 0.
    const LPC_FINAL_LOCK_OFF: u32 = 0x3a6c;
    crate::southbridge::spi_menu(rcba);
    rcba.setbits32(LPC_FINAL_LOCK_OFF, LPC_FINAL_LOCK::LOCK::SET.value);
}

/// Set the PCH lockdown bits at payload handoff, including `GEN_PMCON_1.SMI_LOCK`
/// unconditionally (matching coreboot `southbridge/intel/common/finalize.c`).
pub(crate) fn apply_chipset_locks(rcba: &RcbaRegs, lpc: &LynxPointLpcConfig) {
    rcba.hsfs.modify(HSFS::FLOCKDN::SET);
    rcba.tclockdn.modify(TCLOCKDN::LOCK::SET);
    rcba.gcs.modify(GCS::BILD::SET);
    rcba.fdsw.modify(FDSW::LOCK::SET);
    lpc.gen_pmcon_1.modify(GEN_PMCON_1::SMI_LOCK::SET);
    lpc.gen_pmcon_lock
        .modify(GEN_PMCON_LOCK::ACPI_BASE_LOCK::SET + GEN_PMCON_LOCK::SLP_STR_POL_LOCK::SET);
    lpc.etr3.modify(ETR3::CF9GR::CLEAR + ETR3::CF9LOCK::SET);
    rcba.pmsync_config.modify(PMSYNC_CONFIG::LOCK::SET);
}

/// Verify that all PCH lockdown bits latched as expected.
pub(crate) fn chipset_locks_held(rcba: &RcbaRegs, lpc: &LynxPointLpcConfig) -> bool {
    rcba.hsfs.is_set(HSFS::FLOCKDN)
        && rcba.lpc_final_lock.is_set(LPC_FINAL_LOCK::LOCK)
        && rcba.fdsw.is_set(FDSW::LOCK)
        && rcba.gcs.is_set(GCS::BILD)
        && rcba.pmsync_config.is_set(PMSYNC_CONFIG::LOCK)
        && rcba.tclockdn.is_set(TCLOCKDN::LOCK)
        && lpc.gen_pmcon_1.is_set(GEN_PMCON_1::SMI_LOCK)
        && lpc.gen_pmcon_lock.is_set(GEN_PMCON_LOCK::ACPI_BASE_LOCK)
        && lpc.gen_pmcon_lock.is_set(GEN_PMCON_LOCK::SLP_STR_POL_LOCK)
        && lpc.etr3.is_set(ETR3::CF9LOCK)
        && !lpc.etr3.is_set(ETR3::CF9GR)
}

fn onboard_subsystem(board: u32, silicon: u32) -> u32 {
    ((if board & 0xffff == 0 { silicon } else { board }) & 0xffff)
        | ((if board >> 16 == 0 { silicon } else { board }) & 0xffff_0000)
}

fn supported_lpc(lp: bool, did: u16) -> bool {
    if lp {
        matches!(did, 0x9c41 | 0x9c43 | 0x9c45 | 0x9c47)
    } else {
        matches!(
            did,
            0x8c41
                | 0x8c42
                | 0x8c44
                | 0x8c46
                | 0x8c49
                | 0x8c4a
                | 0x8c4b
                | 0x8c4c
                | 0x8c4e
                | 0x8c4f
                | 0x8c50
                | 0x8c52
                | 0x8c54
                | 0x8c56
                | 0x8c5c
        ) || did & 0xfff0 == 0x8cc0
    }
}

fn hsio_supported(lp: bool, did: u16, rev: u8) -> bool {
    supported_lpc(lp, did)
        && if lp {
            (2..=4).contains(&rev)
        } else {
            rev >= 3 || did & 0xfff0 == 0x8cc0
        }
}

/// BWG steps 5..10, PCI SATA `0x98` (32 bits). Each write is a separate step.
const SATA_EARLY: [(u32, u32); 6] = [
    (0, 1 << 22),
    (0, 1 << 19),
    (0x3f << 7, 4 << 7),
    (0, 1 << 20),
    (3 << 5, 1 << 5),
    (0, 1 << 18),
];

impl IntelSouthbridgeDriver for LynxPoint {
    type Config = LynxPointConfig;
    const ACPI_LPC_NAME: &'static str = "LPCB";
    type Options = Options;
    const KNOBS: &'static [fstart_options::Knob<Options>] = &[
        options::POWER_ON_AFTER_FAIL_KEEP,
        options::NMI,
        options::ME_DISABLE,
    ];

    fn board_options(c: &Self::Config) -> Options {
        Options {
            nmi: c.nmi,
            me_disable: c.me_disable,
            ..Options::from_config(c.power_on_after_fail, None)
        }
    }

    fn options(&self) -> Options {
        self.options
    }

    fn set_options(&mut self, options: Options) {
        self.options = options;
    }

    fn new_from_config(config: &'static Self::Config) -> Result<Self, ServiceError> {
        let d = config.lpc_generic_io;
        usb::validate(config.gpio.lp(), config.usb2, config.usb3)?;
        if (!config.xhci_enabled && (config.xhci_default || config.usb_xhci_on_resume))
            || (!config.ehci_enabled && (!config.xhci_default || !config.usb_xhci_on_resume))
            || config.pic_only_payload
            || config.power_on_after_fail > 2
            || config.sata_gen3_dtle.iter().flatten().any(|v| *v > 15)
            || d.len() > 4
            || d.iter().enumerate().any(|(i, w)| {
                w.encode().is_none() || d[i + 1..].iter().any(|other| w.overlaps(*other))
            })
            || config.irq.iter().any(|r| {
                r.offsets()
                    .is_none_or(|(ip, _)| ip.is_none() && r.pins.is_some())
                    || r.pirqs.unwrap_or(0) & 0x8888 != 0
            })
            || config.sata_hotplug_map & !config.sata_port_map != 0
            || config.sata_port_map & !if config.gpio.lp() { 15 } else { 63 } != 0
            || (config.gpio.lp() && config.pcie_ports & 0xc0 != 0)
            || (!config.gpio.lp() && (config.serialio_enable != 0 || config.gpe0_en[2..] != [0; 2]))
        {
            return Err(ServiceError::InvalidParam);
        }
        let d28ir = config
            .irq
            .iter()
            .find(|r| r.device == dev::PCIE)
            .and_then(|r| r.pirqs)
            .unwrap_or(0x3210);
        Ok(Self {
            config,
            bus: I801SmBus::new(0x400),
            s3: false,
            resume: false,
            ehci_debug: None,
            serial_bars: [None; 8],
            smm_installed: AtomicBool::new(false),
            hpet_id: 0,
            d28ir,
            root_functions: [None; 8],
            requested_ports: config.pcie_ports,
            options: Self::board_options(config),
        })
    }

    fn config(&self) -> &'static Self::Config {
        self.config
    }

    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        let c = self.config;
        let l = lpc::device();
        if l.vendor_id() != 0x8086 || !supported_lpc(c.gpio.lp(), l.device_id()) {
            return Err(ServiceError::NoDevice);
        }
        let l_regs = lpc_regs();
        l_regs
            .rcba
            .write(RCBA_REG::BASE.val((RCBA as u32) >> 14) + RCBA_REG::ENABLE::SET);
        let flash = unsafe { lpc::flash_config() };
        flash.enable_prefetching_and_caching();
        // Clear reset-default SMM_BWP so ramstage can commit the MRC cache to
        // flash before finalize() restores BIOS write protection.
        flash.prepare_firmware_writes();
        let r = rcba_regs();
        r.gcs.modify(GCS::RPR::CLEAR);
        r.fdoc.set(0x1000);
        let opt = (r.fdod.get() >> 24) as u8 & 7;
        r.spi_opt.set((r.spi_opt.get() & !7) | opt);
        r.rc.write(RC::UPPER_128_ENABLE::SET);
        l_regs.lpc_io_dec.set(0x10);
        l_regs.lpc_en.set(0x3d01);
        for (n, reg) in l_regs.gen_dec.iter().enumerate() {
            reg.set(
                c.lpc_generic_io
                    .get(n)
                    .and_then(|d| d.encode())
                    .unwrap_or(0),
            );
        }
        l_regs.pmbase.set(u32::from(c.pm_base()) | 1);
        l_regs.acpi_cntl.write(ACPI_CNTL::ACPI_EN::SET);
        l_regs.gpiobase.set(u32::from(c.gpio_base()) | 1);
        l_regs.gpio_cntl.write(GPIO_CNTL::GPIO_EN::SET);
        if let Some((devfn, _)) = self.ehci_debug {
            usb::prepare_ehci_debug(devfn, r, EcamDevice::from_devfn(0, dev::XHCI));
        }
        Ok(())
    }

    fn bootblock_pre_console_init(&mut self) -> Result<(), ServiceError> {
        self.pre_console_init()?;
        let r = rcba_regs();
        let c = self.config;
        r.gcs.modify(GCS::NO_REBOOT::SET);
        self.pm().tco().halt_and_clear_timeouts();
        if let Some(ratio) = unsafe { fstart_arch::x86::cpu::intel::haswell::nominal_flex_ratio() }
        {
            r.soft_reset_data
                .set((r.soft_reset_data.get() & !(63 << 6)) | (u32::from(ratio & 63) << 6));
            r.soft_reset_ctrl.set(r.soft_reset_ctrl.get() | 1);
            fstart_arch::x86::udelay(30_000);
            me::reset(false, false);
        }
        match c.gpio {
            Gpios::Lp(p) => gpio::LpGpio::setup_at(c.gpio_base(), p),
            Gpios::Traditional(p) => IchGpio::new(c.gpio_base()).setup(p),
        }
        Ok(())
    }

    fn early_init(&mut self) -> Result<(), ServiceError> {
        self.bus = I801SmBus::enable_on_i801(dev::SMBUS, 0x400);
        Ok(())
    }

    fn detect_s3_resume(&self) -> bool {
        ich::detect_s3_resume(&self.pm())
    }

    fn set_s3_enabled(&mut self, v: bool) {
        self.s3 = v;
    }

    fn set_resume(&mut self, v: bool) {
        self.resume = v;
    }

    fn set_ehci_debug(&mut self, devfn: u8, bar: u32) {
        self.ehci_debug = Some((devfn, bar));
    }

    fn pre_raminit_init(&mut self) -> Result<(), ServiceError> {
        let r_win = rcba_window();
        let r = rcba_regs();
        let c = self.config;
        let l = lpc::device();
        let l_regs = lpc_regs();
        let lp = c.gpio.lp();
        let mobile = lp || l.device_id() & 1 != 0;
        r.oic.write(OIC::AEN::SET);
        let _ = r.oic.get();
        for route in c.irq {
            let (ip, ir) = route.offsets().ok_or(ServiceError::InvalidParam)?;
            if let (Some(o), Some(v)) = (ip, route.pins) {
                r_win.write32(o, v);
            }
            if let Some(v) = route.pirqs {
                r_win.write16(ir, v);
            }
        }
        r.fd.modify(FD::BIOS_MUST_WRITE::SET);
        r.dmi_2088.set(0x0010_9000);
        r.dmi_20ac.set(r.dmi_20ac.get() | (1 << 30));
        if !lp {
            r.dmi_2340
                .set((r.dmi_2340.get() & !0x00ff_00ff) | 0x003a_001b);
            r.dmi_2324.set(0x0085_4c74);
        }
        r.dispbdf.set(16);
        r.fd2.modify(FD2::DBDF::SET);
        r.prsts.write(PRSTS::ME_HOST_POWERDOWN::SET);
        let rev = l.read8(8);
        let recipe = hsio_supported(lp, l.device_id(), rev);
        if !recipe {
            fstart_log::warn!(
                "LPT {:#x} stepping {:#x}: skipping HSIO tables",
                l.device_id(),
                rev
            );
        }
        let root = EcamDevice::new(0, dev::PCIE, 0);
        let owner = root.read8(0x410);
        let sata = EcamDevice::from_devfn(0, dev::SATA1);
        sata.modify32(0x94, !0x1ff, 0x183);
        sata.or16(0x92, 1 << 15);
        if recipe {
            hsio::sata(lp, mobile, owner);
        }
        for (clear, set) in SATA_EARLY {
            sata.modify32(0x98, !clear, set);
        }
        sata.or8(
            0x92,
            if lp {
                !owner.reverse_bits() & 15
            } else {
                15 | (!owner & 0x30)
            },
        );
        l_regs
            .gen_pmcon_lock
            .modify(GEN_PMCON_LOCK::ACPI_BASE_LOCK::SET);
        l_regs.etr3.modify(ETR3::CF9GR::CLEAR);
        for i in 0..if lp { 6 } else { 8 } {
            EcamDevice::new(0, dev::PCIE, i).and32(0x338, !(1 << 26));
        }
        root.and8_or8(0xf4, !0x60, 0x80);
        EcamDevice::from_devfn(0, dev::EHCI1).and8(0x88, !4);
        if !lp {
            EcamDevice::from_devfn(0, dev::EHCI2).and8(0x88, !4);
        }
        if recipe {
            hsio::usb_gbe(lp, owner);
        }
        let th = EcamDevice::from_devfn(0, dev::THERMAL);
        if th.is_present() && c.thermal_enabled {
            const TEMP_TBARB: u32 = 0x4000_0000;
            th.write32(0x40, TEMP_TBARB);
            th.write32(0x44, 0);
            th.or32(0x40, 1);
            // SAFETY: temporary 256-byte thermal TBARB enabled above in pre-RAM init.
            let t = unsafe { &*(TEMP_TBARB as usize as *const ThermalMmio) };
            t.ctt.set(0x154);
            t.tss.set(0xff);
            t.tas.set(0xff);
            t.tsgpen.set(0);
            t.tspien.set(0);
            t.tsrel.set(u8::from(c.ec_present));
            th.and32(0x40, !1);
            th.write32(0x40, 0);
        }
        usb::init(c, mobile, self.resume, self.ehci_debug);
        Ok(())
    }

    fn chip_init(&mut self) -> Result<(), ServiceError> {
        if self.resume && (self.options.me_disable || me::is_disabled_mode()) {
            me::hide();
        }
        self.enable_devices()
    }

    fn post_dram_init(&mut self) -> Result<(), ServiceError> {
        self.init_devices()?;
        if !self.resume {
            me::mbp(self.config.icc_clock_disable);
        }
        Ok(())
    }

    fn post_mp_init(&mut self) -> Result<(), ServiceError> {
        self.power_management()?;
        lpc_regs()
            .gen_pmcon_2
            .modify(regs::GEN_PMCON_2::DRAM_INIT::SET);
        fstart_log::info!("LPT: PM_SYNC and dynamic clock gating enabled");
        Ok(())
    }

    fn finalize_init(&mut self) -> Result<(), ServiceError> {
        if !self.resume {
            me::finalize(self.options.me_disable);
        }
        let protected = self.config.smm_bios_protection;
        let installed = self.smm_installed.load(Ordering::Acquire);
        let flash_ok = unsafe { lpc::flash_config() }.finalize_bios_protection(protected)
            && (!protected || (installed && self.pm().read32(pmio::SMI_EN) & pmio::TCO_EN != 0));
        let r_win = rcba_window();
        let r_regs = rcba_regs();
        let l_regs = lpc_regs();
        finalize_lpc_menu(r_win);
        if installed {
            // The handler is installed and the APM source is armed. Deliver
            // board FINALIZE even without a payload issuing the command.
            unsafe { fstart_core::pio::outb(smi::APM_CNT, smi::APM_CNT_FINALIZE) };
        }
        apply_chipset_locks(r_regs, l_regs);
        let hda = EcamDevice::from_devfn(0, dev::HDA);
        if hda.is_present() {
            hda.or16(0x40, 2);
        }
        crate::southbridge::complete_pch_rwo(r_win, fstart_pci::ecam::GlobalEcam);
        let tco = self.pm().tco();
        tco.lock();
        if !flash_ok
            || !chipset_locks_held(r_regs, l_regs)
            || tco.read16(pmio::TCO1_CNT) & pmio::TCO_LOCK == 0
        {
            fstart_log::error!(
                "LPT lockdown: BIOS_CNTL={:#x} HSFS={:#x} LPC={:#x} TC={:#x} GCS={:#x} FDSW={:#x} PMCON1={:#x} PMLOCK={:#x} ETR3={:#x} PMSYNC={:#x} TCO={:#x}",
                l_regs.bios_cntl.get(),
                r_regs.hsfs.get(),
                r_regs.lpc_final_lock.get(),
                r_regs.tclockdn.get(),
                r_regs.gcs.get(),
                r_regs.fdsw.get(),
                l_regs.gen_pmcon_1.get(),
                l_regs.gen_pmcon_lock.get(),
                l_regs.etr3.get(),
                r_regs.pmsync_config.get(),
                tco.read16(pmio::TCO1_CNT)
            );
            return Err(ServiceError::HardwareError);
        }
        Ok(())
    }

    fn smbus_mut(&mut self) -> Option<&mut dyn SmBus> {
        self.config.smbus_enabled.then_some(&mut self.bus)
    }

    fn training_identity(&self) -> Option<[u8; 5]> {
        Some(lpc::training_identity(unsafe { lpc::device().regs() }))
    }

    fn matches_acpi_population(&self, cold: &[u8]) -> bool {
        cold.first() == Some(&self.requested_ports)
    }

    fn acpi_bar_snapshot(&mut self) -> Result<alloc::vec::Vec<u8>, ServiceError> {
        // The suspended OS keeps both its connector descriptions (board
        // population) and its root-port companions (physical port -> BDF).
        // Validate both, even if a fuse happens to hide the optional port.
        let mut snapshot = alloc::vec![self.requested_ports];
        snapshot.extend(self.root_functions.map(|func| func.unwrap_or(0xff)));
        if self.config.sio_acpi_mode {
            for i in 0..8 {
                if self.config.serialio_enable & (1 << i) == 0 {
                    continue;
                }
                let dev = devices::serial(i);
                let bar = |reg| {
                    dev.memory_bar(reg)
                        .and_then(|a| u32::try_from(a).ok())
                        .filter(|a| *a != 0)
                };
                let (Some(a), Some(b)) = (bar(0x10), bar(0x14)) else {
                    fstart_log::warn!("Serial IO {}: missing BAR, omitting from ACPI", i);
                    self.serial_bars[i as usize] = None;
                    continue;
                };
                let pair = (a, b);
                self.serial_bars[i as usize] = Some(pair);
                snapshot.push(i);
                snapshot.extend(pair.0.to_le_bytes());
                snapshot.extend(pair.1.to_le_bytes());
            }
        }
        Ok(snapshot)
    }

    fn onboard_subsystem_fallback(&self) -> bool {
        true
    }

    fn program_subsystem_id(&self, device: EcamDevice, id: u32) -> bool {
        // The allocator knows which functions are onboard; zero means use
        // that half of the controller's own VID/DID, as pci_device.c does.
        let id = onboard_subsystem(id, device.read32(0));
        if device.bus() == 0 && device.func() == 0 && matches!(device.dev(), 26 | 29) {
            return crate::southbridge::with_ehci_unlocked(device, || device.set_subsystem(id));
        }
        device.set_subsystem(id)
    }

    fn fixed_pci_bars(&self) -> PciFixedBars {
        let mut bars = PciFixedBars::new();
        if !self.config.smbus_enabled {
            return bars;
        }
        bars.push(PciFixedBar {
            address: EcamDevice::from_devfn(0, dev::SMBUS).address(),
            register: 0x20,
            kind: PciFixedBarType::Io,
            base: 0x400,
            size: 32,
            prefetchable: false,
        })
        .expect("SMBus BAR");
        bars
    }

    fn system_reset(&self, hard: bool) -> ! {
        lpc_regs().etr3.modify(ETR3::CF9GR::CLEAR);
        if hard {
            fstart_arch::x86::boot::full_reset()
        } else {
            fstart_arch::x86::boot::system_reset(false)
        }
    }
}

impl SmBus for LynxPoint {
    fn read_byte(&mut self, a: u8, c: u8) -> Result<u8, ServiceError> {
        self.bus.read_byte(a, c)
    }
    fn write_byte(&mut self, a: u8, c: u8, v: u8) -> Result<(), ServiceError> {
        self.bus.write_byte(a, c, v)
    }
    fn read_word(&mut self, a: u8, c: u8) -> Result<u16, ServiceError> {
        self.bus.read_word(a, c)
    }
    fn write_word(&mut self, a: u8, c: u8, v: u16) -> Result<(), ServiceError> {
        self.bus.write_word(a, c, v)
    }
    fn block_read(&mut self, a: u8, c: u8, b: &mut [u8]) -> Result<usize, ServiceError> {
        self.bus.block_read(a, c, b)
    }
    fn block_write(&mut self, a: u8, c: u8, b: &[u8]) -> Result<(), ServiceError> {
        self.bus.block_write(a, c, b)
    }
    fn i2c_eeprom_read(&mut self, a: u8, c: u8, b: &mut [u8]) -> Result<(), ServiceError> {
        self.bus.i2c_eeprom_read(a, c, b)
    }
}

impl SmiControl for LynxPoint {
    type EnableState = smi::SmiEnableState;
    type HandlerConfig = smm::Config;

    fn smm_handler_config(&self) -> Self::HandlerConfig {
        smm::Config::new(self)
    }

    fn set_acpi_mode(&self, v: bool) {
        self.smi().set_acpi_mode(v);
    }

    fn quiesce_for_relocation(&self) -> Self::EnableState {
        self.smi().quiesce_for_relocation()
    }

    fn enable_permanent_smi(&self, v: Self::EnableState) {
        self.smi().enable_permanent_smi(v);
    }

    fn lock_permanent_smi(&self) -> bool {
        let locked = self.smi().lock_permanent_smi();
        self.smm_installed.store(locked, Ordering::Release);
        locked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ehci_debug_selects_valid_controller_for_lp_and_traditional() {
        static LP_PADS: gpio::PadTable = [gpio::Pad::new(gpio::Mode::Native); 95];
        static H_GPIO: GpioConfig = GpioConfig::new();

        let lp = LynxPointConfig::new(Gpios::Lp(&LP_PADS));
        let lp_dbg = lp.ehci_debug(0xf000_0000, 64 << 20).unwrap();
        assert_eq!(
            (lp_dbg.device, lp_dbg.function, lp_dbg.bar),
            (0x1d, 0, 0xfef0_0000)
        );

        let lp_ehci2 = LynxPointConfig {
            ehci_debug_controller: Some(EhciController::Ehci2),
            ..lp
        };
        assert!(lp_ehci2.ehci_debug(0xf000_0000, 64 << 20).is_none());

        let lp_disabled = LynxPointConfig {
            ehci_enabled: false,
            ..lp
        };
        assert!(lp_disabled.ehci_debug(0xf000_0000, 64 << 20).is_none());

        let trad = LynxPointConfig::new(Gpios::Traditional(&H_GPIO));
        let trad_default = trad.ehci_debug(0xf000_0000, 64 << 20).unwrap();
        assert_eq!((trad_default.device, trad_default.function), (0x1a, 0));

        let trad_ehci1 = LynxPointConfig {
            ehci_debug_controller: Some(EhciController::Ehci1),
            ..trad
        };
        let trad_dbg = trad_ehci1.ehci_debug(0xf000_0000, 64 << 20).unwrap();
        assert_eq!((trad_dbg.device, trad_dbg.function), (0x1d, 0));
    }

    #[test]
    fn subsystem_override_can_select_just_vendor_or_device() {
        assert_eq!(onboard_subsystem(0, 0x9c26_8086), 0x9c26_8086);
        assert_eq!(onboard_subsystem(0x0000_1028, 0x9c26_8086), 0x9c26_1028);
        assert_eq!(onboard_subsystem(0x1234_0000, 0x9c26_8086), 0x1234_8086);
    }

    #[test]
    fn hsio_selection_and_sata_steps() {
        assert!(!hsio_supported(true, 0x9cc3, 3));
        assert!(!hsio_supported(true, 0x9c43, 1));
        assert!(hsio_supported(true, 0x9c43, 3));
        assert!(hsio_supported(false, 0x8cc6, 0));
        let mut v = u32::MAX;
        assert_eq!(
            SATA_EARLY.map(|(c, s)| {
                v = (v & !c) | s;
                v
            }),
            [
                u32::MAX,
                u32::MAX,
                0xffff_e27f,
                0xffff_e27f,
                0xffff_e23f,
                0xffff_e23f,
            ]
        );
    }
}
