//! SMI routing (coreboot `smm.c`). The ACPI I/O block is ICH compatible,
//! so the shared ICH SMI code quiesces sources and the SMM handler is the
//! ICH one; Bay Trail differs in the enables, the GPIO routing in the PMC
//! and where SMI_LOCK lives.

use super::{ACPI_BASE, pmc};
use crate::southbridge::pmio_ich as pmio;
use crate::southbridge::smi::{BAYTRAIL_GPE0, IchSmi, IchSmmConfig, SmiControl, SmiEnableState};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

/// Bay Trail's SMI controller.
#[derive(Debug, Clone, Copy)]
pub struct BaytrailSmi {
    ich: IchSmi,
    /// PMC GPIO_ROUT: SMI or SCI per GPIO.
    gpio_route: u32,
    /// PCIe devices may wake the system.
    pcie_wake: bool,
}

impl BaytrailSmi {
    pub const fn new(gpio_route: u32, pcie_wake: bool) -> Self {
        Self {
            ich: IchSmi::new(ACPI_BASE, BAYTRAIL_GPE0),
            gpio_route,
            pcie_wake,
        }
    }

    /// PMC status: reset and RTC power status (coreboot `clear_pmc_status`).
    fn clear_pmc_status(&self) {
        let regs = pmc::regs();
        let prsts = regs.prsts.get();
        // RPS clears on a 0 write.
        regs.gen_pmcon1.modify(pmc::GEN_PMCON1::RPS::CLEAR);
        regs.prsts.set(prsts);
    }

    /// Route GPIOs to SMI or SCI and enable the SMI ones in ALT_GPIO_SMI.
    fn route_gpios(&self) {
        pmc::regs().gpio_rout.set(self.gpio_route);
        let smi = (0..16).fold(0u32, |acc, gpio| {
            if (self.gpio_route >> (2 * gpio)) & 3 == 1 {
                acc | 1 << gpio
            } else {
                acc
            }
        });
        fstart_log::debug!(
            "baytrail: GPIO_ROUT {:#010x} ALT_GPIO_SMI {:#06x}",
            self.gpio_route,
            smi
        );
        pmc::pm().write32(pmc::ALT_GPIO_SMI, smi);
    }
}

impl SmiControl for BaytrailSmi {
    type EnableState = SmiEnableState;
    type HandlerConfig = IchSmmConfig;

    fn smm_handler_config(&self) -> IchSmmConfig {
        self.ich.smm_handler_config()
    }

    fn set_acpi_mode(&self, enabled: bool) {
        self.ich.set_acpi_mode(enabled);
    }

    fn quiesce_for_relocation(&self) -> SmiEnableState {
        let previous = self.ich.quiesce_for_relocation();
        self.clear_pmc_status();
        previous
    }

    /// Power button and global release in PM1 (PCIe wake only when the
    /// board allows it), no PME_B0 GPE, the GPIO routes, then APM SMIs.
    /// As on the ICH chipsets, the OS's sleep writes are not trapped.
    fn enable_permanent_smi(&self, previous: SmiEnableState) {
        let pm = pmc::pm();
        self.ich.clear_status();
        let mut pm1 = previous.pm1 | pmio::PWRBTN_EN | pmio::GBL_EN;
        if !self.pcie_wake {
            pm1 |= pmio::PCIEXPWAK_DIS;
        }
        pm.write16(pmio::PM1_EN, pm1);
        pm.write32(pmc::GPE0_EN, previous.gpe0_low & !pmc::PME_B0_EN);
        self.route_gpios();
        pm.write32(pmio::SMI_EN, pmio::APMC_EN | pmio::GBL_SMI_EN | pmio::EOS);
    }

    fn lock_permanent_smi(&self) -> bool {
        let regs = pmc::regs();
        regs.gen_pmcon2.modify(pmc::GEN_PMCON2::SMI_LOCK::SET);
        regs.gen_pmcon2.is_set(pmc::GEN_PMCON2::SMI_LOCK)
            && pmc::pm().read32(pmio::SMI_EN) & pmio::GBL_SMI_EN != 0
    }
}
