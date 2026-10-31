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
}

/// ICH GPE0 register geometry, owned by the Intel southbridge driver.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gpe0Block {
    pub sts_offset: u16,
    pub wide: bool,
}

impl Gpe0Block {
    #[inline(always)]
    const fn enable_offset(self) -> u16 {
        self.sts_offset + if self.wide { 8 } else { 4 }
    }

    #[inline(always)]
    fn clear_status(self, pm: &PmIo) {
        pm.write32(self.sts_offset, u32::MAX);
        if self.wide {
            pm.write32(self.sts_offset + 4, u32::MAX);
        }
    }
}

/// Chipset event enables saved while relocation admits only LAPIC self-SMIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmiEnableState {
    pub pm1: u16,
    pub gpe0_low: u32,
    pub gpe0_high: u32,
    pub alt_gp: u16,
}

/// ICH7/NM10: one 32-bit GPE0 status word at PMBASE+0x28.
pub const ICH7_GPE0: Gpe0Block = Gpe0Block {
    sts_offset: 0x28,
    wide: false,
};
/// ICH8 through ICH10: 64-bit GPE0 status at PMBASE+0x20.
pub const ICH8_GPE0: Gpe0Block = Gpe0Block {
    sts_offset: 0x20,
    wide: true,
};

/// SMI controller view of one ICH PM I/O block.
#[derive(Debug, Clone, Copy)]
pub struct IchSmi {
    pm: PmIo,
    gpe0: Gpe0Block,
}

impl IchSmi {
    #[must_use]
    pub const fn new(pm_base: u16, gpe0: Gpe0Block) -> Self {
        Self {
            pm: PmIo::new(pm_base),
            gpe0,
        }
    }

    /// Switch one ICH GPI between OS SCI and firmware SMI, preserving all
    /// other pins and enables (coreboot `gpi_route_interrupt`).
    ///
    /// # Safety
    /// LPC config/PM decode must be active and PCI config access serialized.
    pub unsafe fn route_gpi(&self, gpi: u8, acpi: bool) {
        if gpi >= 16 {
            return;
        }
        let gpe_enable = self.gpe0.enable_offset();
        self.pm.clrbits16(pmio::ALT_GP_SMI_EN, 1 << gpi);
        self.pm.clrbits32(gpe_enable, 1 << (gpi + 16));
        unsafe {
            let routing = fstart_core::pio::pci_cfg_read32(0, 0x1f, 0, 0xb8);
            fstart_core::pio::pci_cfg_write32(0, 0x1f, 0, 0xb8, gpi_routing(routing, gpi, acpi));
        }
        if acpi {
            self.pm.setbits32(gpe_enable, 1 << (gpi + 16));
        } else {
            self.pm.setbits16(pmio::ALT_GP_SMI_EN, 1 << gpi);
            self.pm.setbits32(pmio::SMI_EN, GPI_SMI);
        }
    }

    /// Whether the OS currently owns the legacy EC/SCI channel.
    pub fn acpi_enabled(&self) -> bool {
        self.pm.read16(pmio::PM1_CNT) & pmio::SCI_EN as u16 != 0
    }

    /// coreboot `smm_southbridge_clear_state()`: clear every W1C status
    /// register so no stale event fires once SMIs are enabled.
    fn clear_status(&self) {
        self.pm.reset_smi_status();
        self.pm.reset_pm1_status();
        self.pm.tco().reset_tco_status();
        self.gpe0.clear_status(&self.pm);
        self.pm.write16(pmio::ALT_GP_SMI_STS, u16::MAX);
    }
}

impl SmiControl for IchSmi {
    type EnableState = SmiEnableState;
    type HandlerConfig = IchSmmConfig;

    fn smm_handler_config(&self) -> IchSmmConfig {
        IchSmmConfig {
            pm_base: self.pm.base(),
            gpe0: self.gpe0,
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
        self.pm.write32(pmio::SMI_EN, 0);
        let gpe0_en = self.gpe0.enable_offset();
        let previous = SmiEnableState {
            pm1: self.pm.read16(pmio::PM1_EN),
            gpe0_low: self.pm.read32(gpe0_en),
            gpe0_high: if self.gpe0.wide {
                self.pm.read32(gpe0_en + 4)
            } else {
                0
            },
            alt_gp: self.pm.read16(pmio::ALT_GP_SMI_EN),
        };
        self.pm.write16(pmio::PM1_EN, 0);
        self.pm.write32(gpe0_en, 0);
        if self.gpe0.wide {
            self.pm.write32(gpe0_en + 4, 0);
        }
        self.pm.write16(pmio::ALT_GP_SMI_EN, 0);
        self.clear_status();
        // LAPIC self-SMIs do not depend on a chipset source bit. Keep only the
        // global gate and EOS set while the shared relocation stub is live.
        self.pm.write32(pmio::SMI_EN, pmio::GBL_SMI_EN | pmio::EOS);
        previous
    }

    fn lock_permanent_smi(&self) -> bool {
        use tock_registers::LocalRegisterCopy;
        // ICH7 through ICH10 share the LPC BDF and SMI_LOCK register.
        // CF8/CFC also works on Q35, which has no global ECAM accessor.
        // SAFETY: the installer calls this on the BSP after AP relocation;
        // neither APs nor the permanent handler access PCI config ports.
        let mut value = LocalRegisterCopy::<u16, GEN_PMCON_1_REG::Register>::new(unsafe {
            fstart_core::pio::pci_cfg_read16(0, 0x1f, 0, GEN_PMCON_1)
        });
        value.modify(GEN_PMCON_1_REG::SMI_LOCK::SET);
        unsafe { fstart_core::pio::pci_cfg_write16(0, 0x1f, 0, GEN_PMCON_1, value.get()) };
        let observed = LocalRegisterCopy::<u16, GEN_PMCON_1_REG::Register>::new(unsafe {
            fstart_core::pio::pci_cfg_read16(0, 0x1f, 0, GEN_PMCON_1)
        });
        observed.is_set(GEN_PMCON_1_REG::SMI_LOCK)
            && self.pm.read32(pmio::SMI_EN) & pmio::GBL_SMI_EN != 0
    }

    fn enable_permanent_smi(&self, previous: SmiEnableState) {
        self.clear_status();
        let gpe0_en = self.gpe0.enable_offset();
        self.pm
            .write16(pmio::PM1_EN, previous.pm1 | pmio::PWRBTN_EN | pmio::GBL_EN);
        self.pm.write32(gpe0_en, previous.gpe0_low);
        if self.gpe0.wide {
            self.pm.write32(gpe0_en + 4, previous.gpe0_high);
        }
        self.pm.write16(pmio::ALT_GP_SMI_EN, previous.alt_gp);
        // No `SLP_SMI_EN`: this SMM has no sleep-transition work to do, and
        // intercepting the OS's sleep write would run a handler during the
        // machine's most delicate transition. coreboot's ICH7 boards without
        // SMM do not intercept it either (their SMM sleep handler exists only
        // to gate the memory reset on newer PCHs).
        self.pm.write32(
            pmio::SMI_EN,
            pmio::TCO_EN | pmio::APMC_EN | pmio::GBL_SMI_EN | pmio::EOS,
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpi_route_preserves_other_pins() {
        assert_eq!(
            gpi_routing(0xa5a5_a5a5, 2, true),
            0xa5a5_a5a5 & !0x30 | 0x20
        );
        assert_eq!(gpi_routing(0xffff_ffff, 2, false), 0xffff_ffdf);
    }
}

/// The sources of one SMI on any ICH PM I/O layout, read once by
/// [`PendingSmi::take`]; the board's SMI handler acts on the ones it owns
/// and ends the SMI with [`PendingSmi::finish`].
///
/// PMBASE and the GPE0 block come from the [`IchSmmConfig`] the installer
/// wrote (see [`IntelSmm`](fstart_arch::x86::cpu::intel::smm::IntelSmm)), so
/// it serves ICH7 and ICH8+ boards alike.
pub struct PendingSmi {
    pm: PmIo,
    gpe0: Gpe0Block,
    smi_sts: u32,
    /// The APM command, when the APM port raised this SMI. ACPI enable and
    /// disable are applied already; FINALIZE never shows up here.
    pub apm: Option<u8>,
    /// The software TCO command (`TCO_DAT_IN`); answer with
    /// [`PendingSmi::tco_reply`].
    pub tco: Option<u8>,
    /// GPE0 status, when GPE0 raised this SMI.
    pub gpe: u64,
    /// Enabled alternate GPI events, indexed by GPIO number (not GPE bit).
    pub gpi: u16,
}

impl PendingSmi {
    /// Read this SMI's sources and do the chipset's own APM work.
    ///
    /// # Safety
    /// Called only by the SMI handler that owns the SMM rendezvous.
    #[inline(always)]
    pub unsafe fn take(config: &IchSmmConfig) -> Self {
        let gpe0 = config.gpe0;
        let pm = PmIo::new(config.pm_base);
        // Handle only the sources this SMI reports (coreboot's
        // `southbridge_smi_handler` dispatches on `SMI_STS` bits the same
        // way). With `SCI_EN` set, PM1/GPE0 events raise an SCI for the OS,
        // so their status must survive an unrelated APMC or TCO SMI.
        let smi_sts = pm.read32(pmio::SMI_STS);

        // The APM command port keeps its last written value, so a command
        // may only be consumed when this SMI came from the APM port.
        // Otherwise the install-time ACPI-disable would replay on every
        // unrelated SMI (TCO, GPE, sleep) and drop the chipset out of
        // ACPI mode in the middle of a suspend.
        // SAFETY: the SMM rendezvous owner reads the decoded APM port.
        let apm = (smi_sts & pmio::APM_STS != 0).then(|| unsafe { fstart_core::pio::inb(APM_CNT) });
        match apm {
            // 16-bit PM1a access: QEMU TCG mishandles 32-bit PIO to
            // ACPI-core PM registers (writes vanish), while 16-bit works
            // on every engine; SCI_EN is a PM1a bit either way.
            Some(APM_CNT_ACPI_DISABLE) => pm.clrbits16(pmio::PM1_CNT, pmio::SCI_EN as u16),
            Some(APM_CNT_ACPI_ENABLE) => pm.setbits16(pmio::PM1_CNT, pmio::SCI_EN as u16),
            _ => {}
        }

        let tco = (smi_sts & pmio::TCO_STS != 0)
            .then(|| pm.tco())
            .filter(|tco| tco.read32(pmio::TCO1_STS) & pmio::SW_TCO_SMI != 0)
            .map(|tco| tco.read_dat_in());

        let mut gpe = 0;
        if smi_sts & pmio::GPE0_SMI_STS != 0 {
            gpe = u64::from(pm.read32(gpe0.sts_offset));
            if gpe0.wide {
                gpe |= u64::from(pm.read32(gpe0.sts_offset + 4)) << 32;
            }
        }
        let mut gpi = 0;
        if smi_sts & GPI_SMI != 0 {
            let status = pm.read16(pmio::ALT_GP_SMI_STS);
            pm.write16(pmio::ALT_GP_SMI_STS, status);
            gpi = status & pm.read16(pmio::ALT_GP_SMI_EN);
        }
        Self {
            pm,
            gpe0,
            smi_sts,
            // FINALIZE has no chipset work yet; never hand it to the board.
            apm: apm.filter(|command| *command != APM_CNT_FINALIZE),
            tco,
            gpe,
            gpi,
        }
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
        if self.smi_sts & pmio::TCO_STS != 0 {
            pm.tco().reset_tco_status();
        }
        if self.smi_sts & pmio::PM1_SMI_STS != 0 {
            pm.write16(pmio::PM1_STS, 0xffff);
        }
        if self.smi_sts & pmio::GPE0_SMI_STS != 0 {
            self.gpe0.clear_status(&pm);
        }
        pm.write32(pmio::SMI_STS, self.smi_sts);
        pm.setbits32(pmio::SMI_EN, pmio::EOS);
    }
}

/// The SMI handler of a board with no SMI sources of its own: the ICH's
/// APM, TCO, PM1 and GPE handling only.
#[inline(always)]
pub fn handle(_ctx: &mut SmmContext<'_>, config: &IchSmmConfig) {
    // SAFETY: bound as the board's SMI handler, so it owns the rendezvous.
    unsafe { PendingSmi::take(config) }.finish();
}
