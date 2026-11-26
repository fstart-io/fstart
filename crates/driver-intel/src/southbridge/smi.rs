//! ICH SMI routing for SMM installation (coreboot `southbridge/intel/common/smi.c`).
//!
//! One implementation covers ICH7 through ICH10: the register layout only
//! differs in the GPE0 block, which [`Gpe0Block`] describes.

use super::pmio_ich::{self as pmio, PmIo};
pub use fstart_arch::x86::cpu::intel::smm::SmiControl;

pub const APM_CNT: u16 = 0x00b2;
const GEN_PMCON_1: u8 = 0xa0;
tock_registers::register_bitfields![u16,
    GEN_PMCON_1_REG [ SMI_LOCK OFFSET(4) NUMBITS(1) [] ]
];

/// Configuration the installer writes into SMRAM for the board's SMI
/// handler ([`PendingSmi`]).
///
/// Both sides are built from this crate, so the type itself is the contract;
/// the generic SMM layer only checks its size and alignment.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IchSmmConfig {
    pm_base: u16,
    gpe0: Gpe0Block,
    sleep: SleepSmi,
}

/// Sleep-transition SMI policy (coreboot `southbridge_smi_sleep()`): off on
/// chipsets whose SMM has no sleep work. 5 Series and later PCHs gate the
/// DIMM reset across S3 from it.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SleepSmi {
    enabled: bool,
    /// GPIOBASE, for the DRAM reset gate.
    gpio_base: u16,
    /// coreboot `DRAM_RESET_GATE_GPIO`, driven low on S3 entry; 0xff: none.
    dram_reset_gate: u8,
    /// Leave AFTER_G3 at "power on" when entering S5 (coreboot's
    /// `power_on_after_fail` CMOS option, `MAINBOARD_POWER_ON`).
    power_on_after_fail: bool,
    /// Update ICH/PCH LPC `GEN_PMCON_3` (00:1f.0:0xa4) on S5 entry.
    ich_pmcon3: bool,
}

impl SleepSmi {
    const NONE: Self = Self {
        enabled: false,
        gpio_base: 0,
        dram_reset_gate: 0xff,
        power_on_after_fail: false,
        ich_pmcon3: false,
    };
}

/// ICH GPE0 register geometry, owned by the Intel southbridge driver.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gpe0Block {
    pub sts_offset: u16,
    pub en_offset: u16,
    words: u8,
    alt: AltGpi,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AltGpi {
    Legacy,
    Split,
    Lp,
}

impl AltGpi {
    #[inline(always)]
    fn acknowledge(self, pm: &PmIo, gpio_base: u16) -> u32 {
        if self == Self::Split {
            // H: snapshot and acknowledge high before touching low.
            let high = pm.read16(0x5e);
            pm.write16(0x5e, high);
            let low = pm.read16(pmio::ALT_GP_SMI_STS);
            pm.write16(pmio::ALT_GP_SMI_STS, low);
            return ((u32::from(high) << 16) | u32::from(low))
                & self.access(pm, gpio_base, false, None);
        }
        let status = self.access(pm, gpio_base, true, None);
        self.access(pm, gpio_base, true, Some(status));
        status & self.access(pm, gpio_base, false, None)
    }

    #[inline(always)]
    fn access(self, pm: &PmIo, gpio_base: u16, status: bool, write: Option<u32>) -> u32 {
        if self == Self::Lp {
            if gpio_base == 0 {
                return 0;
            }
            let gpio = PmIo::new(gpio_base);
            let offset = if status { 0x50 } else { 0x54 };
            if let Some(value) = write {
                gpio.write32(offset, value);
                return value;
            }
            return gpio.read32(offset);
        }
        let lo = if status {
            pmio::ALT_GP_SMI_STS
        } else {
            pmio::ALT_GP_SMI_EN
        };
        let hi = if status { 0x5e } else { 0x5c };
        if let Some(value) = write {
            pm.write16(lo, value as u16);
            if self == Self::Split {
                pm.write16(hi, (value >> 16) as u16);
            }
            return value;
        }
        let high = if self == Self::Split {
            u32::from(pm.read16(hi)) << 16
        } else {
            0
        };
        high | u32::from(pm.read16(lo))
    }
}

impl Gpe0Block {
    #[inline(always)]
    const fn enable_offset(self) -> u16 {
        self.en_offset
    }

    /// Read/ack each W1C bank before board dispatch. Never write all-ones:
    /// a new bit arriving after the read must remain pending for the next SMI.
    #[inline(always)]
    fn take_gpe(self, read: impl Fn(u16) -> u32, ack: impl Fn(u16, u32)) -> u128 {
        let mut events = 0;
        for i in 0..self.words {
            let off = 4 * u16::from(i);
            let status = read(self.sts_offset + off);
            ack(self.sts_offset + off, status);
            let report = if self.alt == AltGpi::Legacy {
                status
            } else {
                status & read(self.en_offset + off)
            };
            events |= u128::from(report) << (32 * i);
        }
        events
    }

    /// The PMBASE accessor covering this GPE0 geometry.
    #[inline(always)]
    const fn pm(self, base: u16) -> PmIo {
        let end = self.en_offset + 4 * self.words as u16;
        if end > 0x80 {
            PmIo::with_size(base, pmio::LYNXPOINT_PMSIZE)
        } else {
            PmIo::new(base)
        }
    }

    /// coreboot S5 entry: `GPE0_EN = 0`.
    #[inline(always)]
    fn disable_all(self, pm: &PmIo) {
        let enable = self.enable_offset();
        for i in 0..self.words {
            pm.write32(enable + 4 * u16::from(i), 0);
        }
    }

    #[inline(always)]
    fn clear_status(self, pm: &PmIo) {
        for i in 0..self.words {
            let offset = self.sts_offset + 4 * u16::from(i);
            pm.write32(offset, pm.read32(offset));
        }
    }
}

/// Chipset event enables saved while relocation admits only LAPIC self-SMIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmiEnableState {
    pub smi: u32,
    pub pm1: u16,
    pub gpe0: [u32; 4],
    pub alt_gp: u32,
}

/// ICH7/NM10: one 32-bit GPE0 status word at PMBASE+0x28.
pub const ICH7_GPE0: Gpe0Block = Gpe0Block {
    sts_offset: 0x28,
    en_offset: 0x2c,
    words: 1,
    alt: AltGpi::Legacy,
};
/// ICH8 through ICH10 and 5 Series PCH: 64-bit GPE0 status at PMBASE+0x20.
pub const ICH8_GPE0: Gpe0Block = Gpe0Block {
    sts_offset: 0x20,
    en_offset: 0x28,
    words: 2,
    alt: AltGpi::Legacy,
};
/// Bay Trail PCU: one 32-bit GPE0 status word at ABASE+0x20, the enables
/// at +0x28.
pub const BAYTRAIL_GPE0: Gpe0Block = Gpe0Block {
    sts_offset: 0x20,
    en_offset: 0x28,
    words: 1,
    alt: AltGpi::Legacy,
};
/// Lynx Point-H: two 16-bit alternate GPI SMI words at PMBASE+0x38/+0x5c.
pub const LYNXPOINT_GPE0: Gpe0Block = Gpe0Block {
    alt: AltGpi::Split,
    ..ICH8_GPE0
};
/// Lynx Point-LP: 128-bit GPE0 at PMBASE+0x80/+0x90, alternate GPI SMI at GPIOBASE+0x50/+0x54.
pub const LYNXPOINT_LP_GPE0: Gpe0Block = Gpe0Block {
    sts_offset: 0x80,
    en_offset: 0x90,
    words: 4,
    alt: AltGpi::Lp,
};

/// SMI controller view of one ICH/PCH PM I/O block.
#[derive(Debug, Clone, Copy)]
pub struct IchSmi {
    bios_protection: bool,
    pm: PmIo,
    gpe0: Gpe0Block,
    sleep: SleepSmi,
}

impl IchSmi {
    #[must_use]
    pub const fn new(pm_base: u16, gpe0: Gpe0Block) -> Self {
        Self {
            bios_protection: false,
            pm: gpe0.pm(pm_base),
            gpe0,
            sleep: SleepSmi::NONE,
        }
    }

    /// Trap the OS's sleep transitions (`SLP_SMI_EN`) and run coreboot's
    /// `southbridge_smi_sleep()` before entering them.
    #[must_use]
    pub const fn with_sleep_smi(mut self, gpio_base: u16) -> Self {
        self.sleep.enabled = true;
        self.sleep.gpio_base = gpio_base;
        self.sleep.ich_pmcon3 = true;
        self
    }

    /// Skip ICH/PCH LPC `GEN_PMCON_3` (00:1f.0:0xa4) updates on S5 entry
    /// (Bay Trail keeps `AFTERG3_EN` in PMC MMIO `GEN_PMCON1`).
    #[must_use]
    pub const fn without_ich_pmcon3(mut self) -> Self {
        self.sleep.ich_pmcon3 = false;
        self
    }

    /// Set the GPIOBASE I/O port without enabling sleep-transition SMIs.
    #[must_use]
    pub const fn with_gpio_base(mut self, gpio_base: u16) -> Self {
        self.sleep.gpio_base = gpio_base;
        self
    }

    /// Enable TCO `BIOSWR` write-protection SMI trapping on Lynx Point.
    #[must_use]
    pub const fn with_bios_protection(mut self, enabled: bool) -> Self {
        self.bios_protection = enabled;
        self
    }

    /// Drive `gpio` low on S3 entry so the DIMMs keep their contents
    /// (coreboot `southbridge_gate_memory_reset()`).
    #[must_use]
    pub const fn with_dram_reset_gate(mut self, gpio: u8) -> Self {
        self.sleep.dram_reset_gate = gpio;
        self
    }

    /// Keep "power on after power failure" when entering S5.
    #[must_use]
    pub const fn with_power_on_after_fail(mut self, power_on: bool) -> Self {
        self.sleep.power_on_after_fail = power_on;
        self
    }

    /// Switch one ICH GPI between OS SCI and firmware SMI, preserving all
    /// other pins and enables (coreboot `gpi_route_interrupt`).
    ///
    /// # Safety
    /// LPC config/PM decode must be active and PCI config access serialized.
    pub unsafe fn route_gpi(&self, gpi: u8, acpi: bool) {
        if gpi >= 16 || self.gpe0.alt != AltGpi::Legacy {
            return;
        }
        let gpe_enable = self.gpe0.enable_offset();
        self.pm.clrbits16(pmio::ALT_GP_SMI_EN, 1 << gpi);
        self.pm.clrbits32(gpe_enable, 1 << (gpi + 16));
        unsafe {
            let routing = fstart_pci::cf8::read32(0, 0x1f, 0, 0xb8);
            fstart_pci::cf8::write32(0, 0x1f, 0, 0xb8, gpi_routing(routing, gpi, acpi));
        }
        if acpi {
            self.pm.setbits32(gpe_enable, 1 << (gpi + 16));
        } else {
            self.pm.setbits16(pmio::ALT_GP_SMI_EN, 1 << gpi);
            self.pm.setbits32(pmio::SMI_EN, GPI_SMI);
        }
    }

    fn permanent_policy(&self, mut previous: SmiEnableState) -> SmiEnableState {
        let lpt = self.gpe0.alt != AltGpi::Legacy;
        if lpt {
            previous.pm1 = 0;
            previous.gpe0[if self.gpe0.alt == AltGpi::Lp { 3 } else { 0 }] &= !(1 << 13);
            previous.smi &= !(pmio::TCO_EN | GPI_SMI);
        } else {
            previous.smi = 0;
        }
        previous.pm1 |= pmio::PWRBTN_EN | pmio::GBL_EN;
        previous.smi |= pmio::GBL_SMI_EN | pmio::EOS | pmio::APMC_EN;
        if !lpt || self.bios_protection {
            previous.smi |= pmio::TCO_EN;
        }
        if self.sleep.enabled {
            previous.smi |= pmio::SLP_SMI_EN;
        }
        previous
    }

    /// Whether the OS currently owns the legacy EC/SCI channel.
    pub fn acpi_enabled(&self) -> bool {
        self.pm.read16(pmio::PM1_CNT) & pmio::SCI_EN as u16 != 0
    }

    /// coreboot `smm_southbridge_clear_state()`: clear every W1C status
    /// register so no stale event fires once SMIs are enabled.
    pub fn clear_status(&self) {
        self.pm.reset_smi_status();
        self.pm.reset_pm1_status();
        self.pm.tco().reset_tco_status();
        self.gpe0.clear_status(&self.pm);
        self.gpe0.alt.acknowledge(&self.pm, self.sleep.gpio_base);
    }
}

impl SmiControl for IchSmi {
    type EnableState = SmiEnableState;
    type HandlerConfig = IchSmmConfig;

    fn smm_handler_config(&self) -> IchSmmConfig {
        IchSmmConfig {
            pm_base: self.pm.base(),
            gpe0: self.gpe0,
            sleep: self.sleep,
        }
    }

    fn set_acpi_mode(&self, enabled: bool) {
        if enabled {
            self.pm.setbits16(pmio::PM1_CNT, pmio::SCI_EN as u16);
        } else {
            self.pm.clrbits16(pmio::PM1_CNT, pmio::SCI_EN as u16);
        }
    }

    fn quiesce_for_relocation(&self) -> SmiEnableState {
        // Block chipset-originated SMIs before changing individual source
        // enables. PM1_CNT is deliberately untouched so SCI_EN is preserved.
        let smi = self.pm.read32(pmio::SMI_EN);
        self.pm.write32(pmio::SMI_EN, 0);
        let gpe0_en = self.gpe0.enable_offset();
        let previous = SmiEnableState {
            smi,
            pm1: self.pm.read16(pmio::PM1_EN),
            gpe0: core::array::from_fn(|i| {
                if i < usize::from(self.gpe0.words) {
                    self.pm.read32(gpe0_en + i as u16 * 4)
                } else {
                    0
                }
            }),
            alt_gp: self
                .gpe0
                .alt
                .access(&self.pm, self.sleep.gpio_base, false, None),
        };
        self.pm.write16(pmio::PM1_EN, 0);
        self.gpe0.disable_all(&self.pm);
        self.gpe0
            .alt
            .access(&self.pm, self.sleep.gpio_base, false, Some(0));
        if self.gpe0.alt == AltGpi::Legacy || smi & pmio::APMC_EN == 0 {
            self.clear_status();
        }
        // LAPIC self-SMIs do not depend on a chipset source bit. Keep only the
        // global gate and EOS set while the shared relocation stub is live.
        self.pm.write32(pmio::SMI_EN, pmio::GBL_SMI_EN | pmio::EOS);
        previous
    }

    fn lock_permanent_smi(&self) -> bool {
        use tock_registers::LocalRegisterCopy;
        // ICH7 through ICH10 and 5/8 Series PCH share the LPC BDF and SMI_LOCK register.
        // CF8/CFC also works on Q35, which has no global ECAM accessor.
        // SAFETY: the installer calls this on the BSP after AP relocation;
        // neither APs nor the permanent handler access PCI config ports.
        let mut value = LocalRegisterCopy::<u16, GEN_PMCON_1_REG::Register>::new(unsafe {
            fstart_pci::cf8::read16(0, 0x1f, 0, GEN_PMCON_1)
        });
        value.modify(GEN_PMCON_1_REG::SMI_LOCK::SET);
        unsafe { fstart_pci::cf8::write16(0, 0x1f, 0, GEN_PMCON_1, value.get()) };
        let observed = LocalRegisterCopy::<u16, GEN_PMCON_1_REG::Register>::new(unsafe {
            fstart_pci::cf8::read16(0, 0x1f, 0, GEN_PMCON_1)
        });
        observed.is_set(GEN_PMCON_1_REG::SMI_LOCK)
            && self.pm.read32(pmio::SMI_EN) & pmio::GBL_SMI_EN != 0
    }

    fn enable_permanent_smi(&self, previous: SmiEnableState) {
        let lpt = self.gpe0.alt != AltGpi::Legacy;
        if !lpt || previous.smi & pmio::APMC_EN == 0 {
            self.clear_status();
        }
        let previous = self.permanent_policy(previous);
        let gpe0_en = self.gpe0.enable_offset();
        self.pm.write16(pmio::PM1_EN, previous.pm1);
        for i in 0..self.gpe0.words {
            self.pm
                .write32(gpe0_en + 4 * u16::from(i), previous.gpe0[usize::from(i)]);
        }
        self.gpe0
            .alt
            .access(&self.pm, self.sleep.gpio_base, false, Some(previous.alt_gp));
        self.pm.write32(pmio::SMI_EN, previous.smi);
    }
}

// ---------------------------------------------------------------------------
// SMI handler (runs inside the SMM image)
// ---------------------------------------------------------------------------

use fstart_smm::SmmContext;

pub const APM_CNT_ACPI_DISABLE: u8 = 0x1e;
pub const APM_CNT_ACPI_ENABLE: u8 = 0xe1;
pub const APM_CNT_FINALIZE: u8 = 0xcb;
// ICH7..10 alternate GPIO source: bit 10 in both SMI_EN and SMI_STS.
const GPI_SMI: u32 = 1 << 10;

const fn gpi_routing(previous: u32, gpi: u8, acpi: bool) -> u32 {
    let shift = 2 * gpi;
    (previous & !(3 << shift)) | ((if acpi { 2 } else { 1 }) << shift)
}

/// A PM1 event raised this SMI while firmware, not OSPM, owns PM1 events.
///
/// `apm` is this SMI's APM command: an ACPI enable or disable changes the
/// owner before the SMI returns. Firmware takes PM1 status only when it owns
/// PM1 both now and after the command; otherwise the status stays pending,
/// for the OS's SCI or for the next SMI once `SCI_EN` is clear.
#[inline(always)]
const fn legacy_pm1_smi(smi_status: u32, pm1_control: u16, apm: Option<u8>) -> bool {
    let sci_now = pm1_control & pmio::SCI_EN as u16 != 0;
    let sci_after = match apm {
        Some(APM_CNT_ACPI_ENABLE) => true,
        Some(APM_CNT_ACPI_DISABLE) => false,
        _ => sci_now,
    };
    smi_status & pmio::PM1_SMI_STS != 0 && !sci_now && !sci_after
}

/// Result of board preparation before committing a trapped transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrepareError {
    /// Nothing was committed, or normal runtime state was restored; allow retry.
    Cancelled,
    /// Runtime state could not be restored safely; reset rather than resume it.
    RestoreFailed,
}

/// The sources of one SMI on any ICH/PCH PM I/O layout, read once by
/// [`PendingSmi::take`]; the board's SMI handler acts on the ones it owns
/// and ends the SMI with [`PendingSmi::finish`].
///
/// PMBASE and the GPE0 block come from the [`IchSmmConfig`] the installer
/// wrote (see [`IntelSmm`](fstart_arch::x86::cpu::intel::smm::IntelSmm)), so
/// it serves ICH7 through Lynx Point boards alike.
pub struct PendingSmi {
    pm: PmIo,
    gpe0: Gpe0Block,
    sleep_policy: SleepSmi,
    smi_sts: u32,
    pm1_status: u16,
    /// The ACPI sleep state the OS is entering (`SLP_TYP` decoded: 0, 1,
    /// 3, 4 or 5), when the sleep trap raised this SMI. [`Self::finish`]
    /// enters it after the board's own sleep work.
    pub sleep: Option<u8>,
    /// The APM command, when the APM port raised this SMI. ACPI enable and
    /// disable are pending during fallible dispatch; FINALIZE is forwarded on LPT.
    pub apm: Option<u8>,
    /// The software TCO command (`TCO_DAT_IN`); answer with
    /// [`PendingSmi::tco_reply`].
    pub tco: Option<u8>,
    /// GPE0 status, when GPE0 raised this SMI.
    pub gpe: u128,
    /// Enabled alternate GPI events, indexed by GPIO number (not GPE bit).
    pub gpi: u32,
}

#[inline(always)]
const fn tco_sources(sts: u32) -> (bool, bool) {
    (sts & pmio::BIOSWR_STS != 0, sts & pmio::SW_TCO_SMI != 0)
}

impl PendingSmi {
    /// Infallible legacy dispatch: apply ACPI mode before handing events to
    /// handlers which choose their EC channel from the new mode.
    ///
    /// # Safety
    /// Called only by the SMI handler that owns the SMM rendezvous.
    #[inline(always)]
    pub unsafe fn take(config: &IchSmmConfig) -> Self {
        let pending = unsafe { Self::read_sources(config) };
        pending.commit_apm();
        pending
    }

    /// Read sources without acknowledging a board ownership handoff yet.
    ///
    /// # Safety
    /// Called only by the SMI handler that owns the SMM rendezvous.
    #[inline(always)]
    pub(crate) unsafe fn read_sources(config: &IchSmmConfig) -> Self {
        let gpe0 = config.gpe0;
        let pm = gpe0.pm(config.pm_base);
        // Handle only the sources this SMI reports (coreboot's
        // `southbridge_smi_handler` dispatches on `SMI_STS` bits the same
        // way). With `SCI_EN` set, PM1/GPE0 events raise an SCI for the OS,
        // so their status must survive an unrelated APMC or TCO SMI.
        let smi_sts = pm.reset_smi_status();

        // The APM command port keeps its last written value, so a command
        // may only be consumed when this SMI came from the APM port.
        // Otherwise the install-time ACPI-disable would replay on every
        // unrelated SMI (TCO, GPE, sleep) and drop the chipset out of
        // ACPI mode in the middle of a suspend.
        // SAFETY: the SMM rendezvous owner reads the decoded APM port.
        let apm = (smi_sts & pmio::APM_STS != 0).then(|| unsafe { fstart_core::pio::inb(APM_CNT) });
        // A power-button press in the same SMI as the OS's ACPI enable is
        // the OS's: acknowledging it here would leave nobody to service it.
        let pm1_status = if legacy_pm1_smi(smi_sts, pm.read16(pmio::PM1_CNT), apm) {
            pm.reset_pm1_status()
        } else {
            0
        };

        // coreboot southbridge_smi_sleep(): no further sleep SMIs, then
        // decode SLP_TYP.
        let sleep = (smi_sts & pmio::SLP_SMI_STS != 0)
            .then(|| {
                pm.clrbits32(pmio::SMI_EN, pmio::SLP_SMI_EN);
                let state = acpi_sleep_state(pm.read32(pmio::PM1_CNT));
                if state.is_none() {
                    pm.clrbits32(pmio::PM1_CNT, pmio::SLP_TYP_MASK | pmio::SLP_EN);
                    pm.setbits32(pmio::SMI_EN, pmio::SLP_SMI_EN);
                }
                state
            })
            .flatten();

        let tco_status = if smi_sts & pmio::TCO_STS != 0 {
            pm.tco().reset_tco_status()
        } else {
            0
        };
        let (bioswr, software) = tco_sources(tco_status);
        if bioswr {
            use super::lpc::{BIOS_CONTROL, LPC_DEV, LPC_FUNC, LpcFlashConfig};
            const CNTL: u8 = core::mem::offset_of!(LpcFlashConfig, bios_control) as u8;
            // SMM does not inherit ramstage's ECAM binding.
            unsafe {
                let ctrl = fstart_pci::cf8::read8(0, LPC_DEV, LPC_FUNC, CNTL);
                if BIOS_CONTROL::WRITE_ENABLE.is_set(ctrl) {
                    fstart_pci::cf8::write8(
                        0,
                        LPC_DEV,
                        LPC_FUNC,
                        CNTL,
                        ctrl & !BIOS_CONTROL::WRITE_ENABLE::SET.value,
                    );
                }
            }
        }
        let tco = software.then(|| pm.tco().read_dat_in());

        let gpe = if smi_sts & pmio::GPE0_SMI_STS != 0 {
            gpe0.take_gpe(|reg| pm.read32(reg), |reg, v| pm.write32(reg, v))
        } else {
            0
        };
        let mut gpi = 0;
        if smi_sts & GPI_SMI != 0 {
            gpi = gpe0.alt.acknowledge(&pm, config.sleep.gpio_base);
        }
        Self {
            pm,
            gpe0,
            sleep_policy: config.sleep,
            smi_sts,
            pm1_status,
            sleep,
            apm: apm.filter(|command| gpe0.alt != AltGpi::Legacy || *command != APM_CNT_FINALIZE),
            tco,
            gpe,
            gpi,
        }
    }

    #[inline(always)]
    pub fn acpi_enabled(&self) -> bool {
        self.pm.read16(pmio::PM1_CNT) & pmio::SCI_EN as u16 != 0
    }

    #[inline(always)]
    pub fn request_poweroff(&self) {
        self.pm.request_poweroff();
    }

    #[inline(always)]
    pub fn requested_acpi_mode(&self) -> Option<bool> {
        match self.apm {
            Some(APM_CNT_ACPI_DISABLE) => Some(false),
            Some(APM_CNT_ACPI_ENABLE) => Some(true),
            _ => None,
        }
    }

    #[inline(always)]
    fn commit_apm(&self) {
        // QEMU TCG needs 16-bit accesses to the ACPI-core PM register.
        match self.requested_acpi_mode() {
            Some(true) => self.pm.setbits16(pmio::PM1_CNT, pmio::SCI_EN as u16),
            Some(false) => self.pm.clrbits16(pmio::PM1_CNT, pmio::SCI_EN as u16),
            None => {}
        }
    }

    /// Commit any pending APM command and sleep transition, or roll back the
    /// sleep trap on [`PrepareError`].
    #[inline(always)]
    pub fn complete(mut self, prepared: Result<(), PrepareError>, prepare_sleep: impl FnOnce(u8)) {
        match prepared {
            Ok(()) => {
                self.commit_apm();
                if let Some(state) = self.sleep {
                    prepare_sleep(state);
                }
            }
            Err(error) => {
                if self.sleep.take().is_some() {
                    // Drop the intercepted request with the trap still disabled,
                    // then re-arm it for a fresh OS attempt. Do not prepare USB,
                    // gate DRAM reset or reissue SLP_EN on this path.
                    self.pm
                        .clrbits32(pmio::PM1_CNT, pmio::SLP_TYP_MASK | pmio::SLP_EN);
                    self.pm.setbits32(pmio::SMI_EN, pmio::SLP_SMI_EN);
                }
                if error == PrepareError::RestoreFailed {
                    fstart_arch::x86::boot::full_reset();
                }
            }
        }
        self.finish();
    }

    /// Answer the software TCO command (`TCO_DAT_OUT`).
    #[inline(always)]
    pub fn tco_reply(&self, response: u8) {
        self.pm.tco().write_dat_out(response);
    }

    /// Clear the handled sources and end the SMI (EOS).
    #[inline(always)]
    pub fn finish(self) {
        let pm = self.pm;
        if self.gpe0.alt != AltGpi::Legacy && self.smi_sts & GPI_SMI != 0 {
            self.gpe0.alt.acknowledge(&pm, self.sleep_policy.gpio_base);
        }
        if let Some(state) = self.sleep {
            self.enter_sleep(state);
        }
        if service_power_button(self.sleep, self.pm1_status)
            && pm.read16(pmio::PM1_CNT) & pmio::SCI_EN as u16 == 0
        {
            pm.request_poweroff();
        }
        pm.setbits32(pmio::SMI_EN, pmio::EOS);
    }

    /// coreboot `southbridge_smi_sleep()` after `mainboard_smi_sleep()`:
    /// prepare the platform and re-issue the OS's sleep request. Never
    /// returns for S3, S4 and S5.
    #[inline(always)]
    fn enter_sleep(&self, state: u8) {
        let pm = self.pm;
        let policy = self.sleep_policy;
        match state {
            3 => {
                if policy.dram_reset_gate != 0xff {
                    super::gpio_ich::IchGpio::new(policy.gpio_base)
                        .output(policy.dram_reset_gate, false);
                }
                // SAFETY: SMM owns the CPU; caches are on.
                unsafe { fstart_arch::x86::writeback_invalidate_caches() };
            }
            5 => {
                self.gpe0.disable_all(&pm);
                // SAFETY: the SMM rendezvous owner accesses PCI config.
                unsafe {
                    if policy.ich_pmcon3 {
                        let pmcon3 = fstart_pci::cf8::read8(0, 0x1f, 0, 0xa4);
                        let pmcon3 = if policy.power_on_after_fail {
                            pmcon3 & !1
                        } else {
                            pmcon3 | 1
                        };
                        fstart_pci::cf8::write8(0, 0x1f, 0, 0xa4, pmcon3);
                    }
                    busmaster_disable_all();
                }
            }
            _ => {}
        }
        let pm1 = pm.read32(pmio::PM1_CNT);
        pm.write32(pmio::PM1_CNT, pm1 | pmio::SLP_EN);
        if state >= 3 {
            loop {
                // SAFETY: the platform is powering down.
                unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
            }
        }
        // S1 woke up: a non-ACPI OS gets S0 back.
        let pm1 = pm.read32(pmio::PM1_CNT);
        if pm1 & pmio::SCI_EN != 0 {
            pm.write32(pmio::PM1_CNT, pm1 & !(pmio::SLP_EN | pmio::SLP_TYP_MASK));
        }
    }
}

/// A power-button S5 cannot replace an already committed deep sleep. A
/// cancelled sleep clears `sleep` so normal PM1 handling may resume.
#[inline(always)]
const fn service_power_button(sleep: Option<u8>, status: u16) -> bool {
    !matches!(sleep, Some(3..=5)) && status & pmio::PWRBTN_STS != 0
}

/// coreboot `acpi_sleep_from_pm1()` for Intel PM1_CNT.SLP_TYP.
#[inline(always)]
const fn acpi_sleep_state(pm1_cnt: u32) -> Option<u8> {
    match (pm1_cnt & pmio::SLP_TYP_MASK) >> pmio::SLP_TYP_SHIFT {
        0 => Some(0),
        1 => Some(1),
        5 => Some(3),
        6 => Some(4),
        7 => Some(5),
        _ => None,
    }
}

/// coreboot `busmaster_disable_on_bus()`: no DMA while powering off.
///
/// Iterative over a 256-bit pending-bus bitmap rather than recursive so a deep
/// OS-programmed PCI bridge chain cannot overflow the bounded SMM stack. Since
/// only bridges with `secondary > bus` are followed, a single ascending pass
/// over `0..=255` visits every reachable bus at most once in O(1) stack space.
///
/// # Safety
/// The caller owns PCI configuration access (the SMM rendezvous).
#[inline(always)]
unsafe fn busmaster_disable_all() {
    let mut pending = [0u64; 4];
    pending[0] = 1; // Start from root bus 0.
    for bus in 0..=255u8 {
        if pending[usize::from(bus >> 6)] & (1u64 << (bus & 63)) == 0 {
            continue;
        }
        for slot in 0..32u8 {
            for func in 0..8u8 {
                let id = unsafe { fstart_pci::cf8::read32(bus, slot, func, 0) };
                if matches!(id, 0xffff_ffff | 0 | 0x0000_ffff | 0xffff_0000) {
                    continue;
                }
                let command = unsafe { fstart_pci::cf8::read16(bus, slot, func, 0x04) };
                unsafe { fstart_pci::cf8::write16(bus, slot, func, 0x04, command & !(1 << 2)) };
                let header = unsafe { fstart_pci::cf8::read8(bus, slot, func, 0x0e) } & 0x7f;
                if header == 1 || header == 2 {
                    let buses = unsafe { fstart_pci::cf8::read32(bus, slot, func, 0x18) };
                    let secondary = ((buses >> 8) & 0xff) as u8;
                    if secondary > bus {
                        pending[usize::from(secondary >> 6)] |= 1u64 << (secondary & 63);
                    }
                }
            }
        }
    }
}

/// The SMI handler of a board with no SMI sources of its own: the ICH's
/// APM, TCO, PM1 and GPE handling only.
#[inline(always)]
pub fn handle(_ctx: &mut SmmContext<'_>, config: &IchSmmConfig) {
    // SAFETY: bound as the board's SMI handler, so it owns the rendezvous.
    unsafe { PendingSmi::take(config) }.finish();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pm1_events_are_owned_only_for_a_legacy_pm1_smi() {
        for source in [
            0,
            pmio::APM_STS,
            pmio::TCO_STS,
            pmio::PM1_SMI_STS,
            pmio::PM1_SMI_STS | pmio::APM_STS,
        ] {
            assert_eq!(
                legacy_pm1_smi(source, 0, None),
                source & pmio::PM1_SMI_STS != 0
            );
            assert!(!legacy_pm1_smi(source, pmio::SCI_EN as u16, None));
        }
    }

    #[test]
    fn power_button_with_acpi_enable_is_left_for_the_os() {
        let both = pmio::PM1_SMI_STS | pmio::APM_STS;
        let sci = pmio::SCI_EN as u16;
        // The OS enables ACPI in this SMI: its SCI services the press.
        assert!(!legacy_pm1_smi(both, 0, Some(APM_CNT_ACPI_ENABLE)));
        // Disabling ACPI: retaken by the next SMI once SCI_EN is clear.
        assert!(!legacy_pm1_smi(both, sci, Some(APM_CNT_ACPI_DISABLE)));
        // Unrelated commands leave the owner unchanged.
        assert!(legacy_pm1_smi(both, 0, Some(APM_CNT_FINALIZE)));
        assert!(legacy_pm1_smi(both, 0, Some(APM_CNT_ACPI_DISABLE)));
        assert!(!legacy_pm1_smi(both, sci, Some(APM_CNT_ACPI_ENABLE)));
    }

    #[test]
    fn committed_sleep_wins_over_pm1_power_button() {
        for state in [3, 4, 5] {
            assert!(!service_power_button(Some(state), pmio::PWRBTN_STS));
        }
        assert!(service_power_button(None, pmio::PWRBTN_STS));
    }

    #[test]
    fn reserved_sleep_is_not_s0() {
        assert_eq!(
            [0, 1, 2, 3, 4, 5, 6, 7].map(|v| acpi_sleep_state(v << pmio::SLP_TYP_SHIFT)),
            [
                Some(0),
                Some(1),
                None,
                None,
                None,
                Some(3),
                Some(4),
                Some(5)
            ]
        );
    }

    #[test]
    fn tco_write_trap_is_not_a_software_command_or_timeout() {
        assert_eq!(tco_sources(1 << 8), (true, false));
        assert_eq!(tco_sources(1 << 1), (false, true));
        assert_eq!(tco_sources(1 << 3), (false, false));
        assert_eq!(tco_sources(0x10a), (true, true));
    }

    #[test]
    fn lp_snapshot_reports_enabled_bits_and_preserves_late_events() {
        use core::cell::Cell;
        let late = Cell::new(0);
        let report = LYNXPOINT_LP_GPE0.take_gpe(
            |reg| match reg {
                0x8c => 5,
                0x9c => 1,
                _ => 0,
            },
            |reg, v| {
                if reg == 0x8c {
                    late.set(7 & !v);
                }
            },
        );
        assert_eq!(report, 1 << 96);
        assert_eq!(late.get(), 2);
        assert_eq!(ICH7_GPE0.take_gpe(|_| 5, |_, v| assert_eq!(v, 5)), 5);
    }

    #[test]
    fn lp_alt_gpi_with_zero_gpio_base_is_noop() {
        let pm = LYNXPOINT_LP_GPE0.pm(0x1800);
        assert_eq!(AltGpi::Lp.access(&pm, 0, true, None), 0);
        assert_eq!(AltGpi::Lp.access(&pm, 0, false, Some(0x1234)), 0);
    }

    #[test]
    fn permanent_lpt_masks_do_not_enable_tco_without_bios_policy() {
        for (block, std) in [(LYNXPOINT_GPE0, 0), (LYNXPOINT_LP_GPE0, 3)] {
            let prev = SmiEnableState {
                pm1: 0xffff,
                gpe0: [u32::MAX; 4],
                alt_gp: 1,
                smi: pmio::TCO_EN | GPI_SMI | 0x10000,
            };
            for bwp in [false, true] {
                let p = IchSmi::new(0x500, block)
                    .with_sleep_smi(0x1400)
                    .with_bios_protection(bwp)
                    .permanent_policy(prev);
                assert_eq!(p.pm1, pmio::PWRBTN_EN | pmio::GBL_EN);
                assert_eq!(
                    p.smi,
                    0x10000
                        | pmio::GBL_SMI_EN
                        | pmio::EOS
                        | pmio::APMC_EN
                        | pmio::SLP_SMI_EN
                        | if bwp { pmio::TCO_EN } else { 0 }
                );
                assert_eq!(p.gpe0[std], !(1 << 13));
                assert_eq!(p.alt_gp, 1);
            }
        }
    }

    #[test]
    fn gpi_route_preserves_other_pins() {
        assert_eq!(
            gpi_routing(0xa5a5_a5a5, 2, true),
            0xa5a5_a5a5 & !0x30 | 0x20
        );
        assert_eq!(gpi_routing(0xffff_ffff, 2, false), 0xffff_ffdf);
    }
}
