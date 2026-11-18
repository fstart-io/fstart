//! Storage control cluster: eMMC (00:10.0, 00:17.0), SDIO (00:11.0) and
//! SD (00:12.0) (coreboot `scc.c`, `emmc.c` and `sd.c`).

use super::{AcpiModeDevice, Bar, Function};
use crate::MmioBar;
use crate::baytrail::iosf::{CCU, Iosf, SCC, SCORE};
use fstart_pci::ConfigRegionAccess;

const CTL_PCI_CFG_DIS: u32 = 1 << 0;
const CTL_ACPI_INT_EN: u32 = 1 << 1;
const CAP_OVERRIDE_LOW: u16 = 0xa0;
const CAP_OVERRIDE_HIGH: u16 = 0xa4;
const USE_CAP_OVERRIDES: u32 = 1 << 31;

/// SCC policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SccConfig {
    /// SD controller capability override `(low, high)`.
    pub sdcard_cap: Option<(u32, u32)>,
    /// Hide the controllers from PCI and describe them in ACPI.
    pub acpi_mode: bool,
}

/// The function's control register on the SCC sideband port.
pub const fn ctl_reg(function: Function) -> Option<u32> {
    match function {
        Function::Sd => Some(0x504),
        Function::Sdio => Some(0x508),
        Function::Mmc45 | Function::Mmc => Some(0x50c),
        _ => None,
    }
}

/// Sideband DLL and pad setup common to every controller (coreboot
/// `baytrail_init_scc`), before enumeration.
pub fn init_sideband<P: ConfigRegionAccess>(iosf: &Iosf<P>) {
    fstart_log::debug!("baytrail: SCC sideband init");
    // Master DLL, its swing/FSM, then run with a local reset.
    iosf.write(SCORE, 0x4964, 0x0007_8000);
    iosf.write(SCORE, 0x4970, 0x0000_0133);
    iosf.write(SCORE, 0x4970, 0x0000_1933);
    // Slave path override from the master DLL.
    let dll = (iosf.read(SCORE, 0x496c) & 0x7_ffff) | (iosf.read(SCORE, 0x4950) & !0xf_ffff);
    iosf.write(SCORE, 0x4950, dll | 1 << 19);
    // Write and read paths.
    for reg in [0x4954, 0x4958, 0x495c, 0x43e4, 0x4324, 0x42b4] {
        iosf.modify(SCORE, reg, 0x7fff, 0x35ad);
    }
    // eMMC 4.5 TX and RX DLL.
    iosf.modify(SCORE, 0x49a4, 0x1f_001f, 0xa_000d);
    for reg in [0x49a8, 0x49ac, 0x49b0, 0x49b4] {
        iosf.modify(SCORE, reg, 0x1f_001f, 0xd_000d);
    }
    iosf.modify(SCORE, 0x49b8, 0x1, 0);
    // MMC1 slew.
    iosf.modify(SCORE, 0x48c0, 0x3c, 0);
    iosf.modify(SCORE, 0x48c4, 0x3c, 0);
    // OCP and XIN clocks, IOSF snoop, SDIO 3 V.
    iosf.modify(SCC, 0x600, 0xf, 0x5);
    iosf.modify(SCC, 0x00, 0, 1 << 7);
    iosf.modify(SCC, 0x600, 0x30, 0x30);
}

fn enable_acpi_mode<P: ConfigRegionAccess>(
    iosf: &Iosf<P>,
    function: Function,
    record: &mut impl FnMut(AcpiModeDevice),
) {
    let device = function.device();
    let Some(ctl) = ctl_reg(function) else {
        return;
    };
    let entry = AcpiModeDevice::capture(function, &device);
    device.or16(0x04, 1 << 1 | 1 << 2 | 1 << 10);
    iosf.modify(SCC, ctl, 0, CTL_PCI_CFG_DIS | CTL_ACPI_INT_EN);
    record(entry);
}

/// After allocation: eMMC 4.5 HS200 setup, SD capability overrides and
/// the optional ACPI mode.
pub fn init<P: ConfigRegionAccess>(
    config: &SccConfig,
    iosf: &Iosf<P>,
    mut record: impl FnMut(AcpiModeDevice),
) {
    let emmc = Function::Mmc45.device();
    if emmc.is_present() {
        fstart_log::debug!("baytrail: eMMC init");
        // 2 ms card stable, HS200.
        emmc.or32(0xa8, 1 << 24);
        emmc.write32(0xa0, 0x446c_c801);
        emmc.write32(0xa4, 0x8000_0807);
        // SDIO1 dummy loopback, MIPI-HSI clock gating.
        iosf.modify(SCORE, 0x49c0, 0, 1 << 3);
        iosf.modify(CCU, 0x1c, 3 << 26, 1 << 26);
        // HS200 slew.
        iosf.modify(SCORE, 0x48c0, 0x3c, 0x3c);
        iosf.modify(SCORE, 0x48c4, 0x3c, 0x3c);
        // Maximum data timeout.
        if let Some(bar) = Bar::of(&emmc) {
            bar.write8(0x2e, 0x0e);
        }
        if config.acpi_mode {
            enable_acpi_mode(iosf, Function::Mmc45, &mut record);
        }
    }
    let sd = Function::Sd.device();
    if sd.is_present() {
        if let Some((low, high)) = config
            .sdcard_cap
            .filter(|&(low, high)| low != 0 || high != 0)
        {
            fstart_log::debug!("baytrail: SD capability override");
            sd.write32(CAP_OVERRIDE_LOW, low);
            sd.write32(CAP_OVERRIDE_HIGH, high | USE_CAP_OVERRIDES);
        }
        if config.acpi_mode {
            enable_acpi_mode(iosf, Function::Sd, &mut record);
        }
    }
}
