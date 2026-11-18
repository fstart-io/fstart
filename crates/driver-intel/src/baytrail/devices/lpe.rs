//! LPE audio (00:15.0) (coreboot `lpe.c`).

use super::{AcpiModeDevice, Bar, Function};
use crate::MmioBar;
use crate::baytrail::iosf::{Iosf, LPE};
use crate::baytrail::pmc::{self, PLT_CLK_CTL};
use fstart_pci::ConfigRegionAccess;
use tock_registers::interfaces::ReadWriteable;

/// LPE firmware memory: 1 MiB aligned to 512 MiB.
pub const FIRMWARE_BASE: u64 = 512 << 20;
pub const FIRMWARE_SIZE: u64 = 1 << 20;
const FIRMWARE_PCI_BASE: u16 = 0xa8;
const FIRMWARE_PCI_LENGTH: u16 = 0xac;
/// C0 and later read the firmware window from MMIO.
const FIRMWARE_MMIO_BASE_C0: u32 = 0x14_4000;
const PCICFGCTR1: u32 = 0x500;
const PCI_CFG_DIS: u32 = 1 << 0;
const ACPI_INT_EN: u32 = 1 << 1;

/// The platform clock feeding the audio codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecClock {
    Mhz19p2,
    Mhz25,
}

/// LPE policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LpeConfig {
    /// Codec clock and the platform clock pin (0-5) carrying it.
    pub codec_clock: Option<(CodecClock, u8)>,
    pub acpi_mode: bool,
}

/// Whether the firmware window must stay reserved.
pub fn present() -> bool {
    Function::Lpe.device().is_present()
}

pub fn init<P: ConfigRegionAccess>(
    config: &LpeConfig,
    iosf: &Iosf<P>,
    mut record: impl FnMut(AcpiModeDevice),
) {
    let lpe = Function::Lpe.device();
    if !lpe.is_present() {
        return;
    }
    // The firmware window, in config space and for C0 and later in MMIO.
    lpe.write32(FIRMWARE_PCI_BASE, FIRMWARE_BASE as u32);
    lpe.write32(FIRMWARE_PCI_LENGTH, FIRMWARE_SIZE as u32);
    if crate::baytrail::stepping() >= crate::baytrail::Stepping::C0
        && let Some(bar) = Bar::of(&lpe)
    {
        bar.write32(FIRMWARE_MMIO_BASE_C0, FIRMWARE_BASE as u32);
        bar.write32(FIRMWARE_MMIO_BASE_C0 + 4, FIRMWARE_SIZE as u32);
    }
    match config.codec_clock {
        Some((clock, pin @ 0..=5)) => {
            fstart_log::debug!("baytrail: LPE codec clock on PLT_CLK{}", pin);
            let freq = match clock {
                CodecClock::Mhz19p2 => PLT_CLK_CTL::FREQ::Mhz19p2,
                CodecClock::Mhz25 => PLT_CLK_CTL::FREQ::Mhz25,
            };
            pmc::regs().plt_clk_ctl[usize::from(pin)].modify(freq + PLT_CLK_CTL::CLK_CTL::On);
        }
        Some(_) => fstart_log::warn!("baytrail: invalid LPE codec clock pin"),
        None => fstart_log::debug!("baytrail: LPE codec clock not required"),
    }
    if config.acpi_mode {
        let mut entry = AcpiModeDevice::capture(Function::Lpe, &lpe);
        entry.extra = FIRMWARE_BASE as u32;
        lpe.or16(0x04, 1 << 1 | 1 << 2 | 1 << 10);
        iosf.modify(LPE, PCICFGCTR1, 0, PCI_CFG_DIS | ACPI_INT_EN);
        record(entry);
    }
}
