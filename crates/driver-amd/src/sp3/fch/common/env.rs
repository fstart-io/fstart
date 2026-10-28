//! DXE env tasks shared by Taishan and Huashan (FCH_SP3 §3.2).

use tock_registers::interfaces::{ReadWriteable, Writeable};

use super::super::acpimmio::{self, AoacDevice, GPIO, IOMUX, MISC, PM, RTC_SHADOW, pm};
use super::super::alink;
use super::super::{ConfigWindow, FchHw, Window, apply, uart, update};
use super::sci_map;
use crate::sp3::soc::Generation;

/// What the platform does when AC power returns (PMx5B, FCH_SP3 §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PowerFailState {
    /// Stay off (the donor default on all generations).
    #[default]
    Off,
    On,
    Previous,
}

/// Policy of [`hw_acpi`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EnvPolicy {
    pub power_fail: PowerFailState,
}

/// af5f: ACPI config, SCI map, (ZP) clock generator, Huashan PM bits.
/// The donor also stores a pointer to its FCH data block in CMOS RAM
/// `0x608..0x60B` for its SMM code; fstart's SMM does not use it.
pub fn hw_acpi_pre<H: FchHw>(hw: &H, generation: Generation) {
    let acpi = hw.acpi();
    if generation == Generation::Naples {
        update::<u8, _>(acpi, PM + 0xCC, 0x04, 0x03);
    }
    // AcpiCfg: DecEnAcpi, GblEnEn, RtcEnEn, TmrEnEn.
    acpi.write8(PM + pm::ACPI_CONFIG, 0x17);
    if generation != Generation::Naples {
        acpi.write8(PM + pm::ACPI_CONFIG + 2, 0x06);
    }
    // AcpiCfg[29] RtcWakeAlarm off.
    update::<u8, _>(acpi, PM + pm::ACPI_CONFIG + 3, 0x20, 0);
    update::<u8, _>(acpi, PM + 0xBA, 0, 0x08);
    update::<u8, _>(acpi, PM + 0xBC, 0, 0x02);
    apply(acpi, sci_map(generation));
    match generation {
        #[cfg(feature = "naples")]
        Generation::Naples => apply(acpi, super::super::tables::CLOCK_GEN_NAPLES),
        Generation::Rome | Generation::Milan => {
            // PMxC0 byte: write-1-to-clear ThermalTrip.
            acpi.write8(PM + pm::S5_RESET_STATUS, 0x01);
            if generation == Generation::Rome {
                acpi.reg::<u16, ()>(PM + 0x3C).set(0x0E80);
            }
            acpi.write8(PM + 0x70, 0x01);
            update::<u32, _>(acpi, MISC + 0x10, 0, 1 << 29);
            if generation == Generation::Milan {
                update::<u32, _>(acpi, MISC + 0x08, 1, 0);
            }
            update::<u32, _>(acpi, MISC + 0x40, 0, 1 << 30);
        }
        #[allow(unreachable_patterns)]
        g => super::not_built(g),
    }
}

/// af50: HW ACPI env: SMBus function IDs, PM and decode bits, AC-loss
/// policy, HPET.
pub fn hw_acpi<H: FchHw>(hw: &H, generation: Generation, policy: &EnvPolicy) {
    let acpi = hw.acpi();
    // D14F0x10/x11 (SMBus function; ZP leaves them 0). Unnamed.
    // SAFETY: tasks own the SMBus function.
    let smbus =
        unsafe { ConfigWindow::new(hw.pci().clone(), fstart_pci::PciAddress::new(0, 0, 0x14, 0)) };
    let (b10, b11) = match generation {
        Generation::Naples => (0, 0),
        Generation::Rome | Generation::Milan => (1, 6),
    };
    smbus.write8(0x10, b10);
    smbus.write8(0x11, b11);
    update::<u8, _>(acpi, PM + 0xED, 0, 0x02);
    update::<u8, _>(acpi, PM + 0xBB, 0x40, 0);
    // AcpiCfg[31:24]: PcieNative, WakePinAsGevent on; PcieWakMask (ZP also
    // RstUSBS5) off.
    match generation {
        Generation::Naples => update::<u8, _>(acpi, PM + pm::ACPI_CONFIG + 3, 0x23, 0x08),
        Generation::Rome | Generation::Milan => {
            update::<u8, _>(acpi, PM + pm::ACPI_CONFIG + 3, 0x22, 0x09)
        }
    }
    // PMx02 [4:3]/[2] (Milan does this in ResetHwAcpi).
    if generation != Generation::Milan {
        update::<u8, _>(acpi, PM + 0x02, 0x18, 0x04);
    }
    if generation == Generation::Naples {
        update::<u8, _>(acpi, PM + 0x81, 0, 0x18);
        acpi.write8(PM + 0x94, 0x01);
        update::<u8, _>(acpi, PM + 0x83, 0x01, 0x20);
    }
    update::<u8, _>(acpi, MISC + 0x6E, 0, 0xA0);
    if generation != Generation::Naples {
        // GPIO 120 pull-up on, pull-down off.
        update::<u32, _>(acpi, GPIO + 4 * 120, 1 << 21, 1 << 20);
    }
    acpi.reg::<u8, RTC_SHADOW::Register>(PM + pm::RTC_SHADOW)
        .modify(
            RTC_SHADOW::PWR_FAIL_BIT3::CLEAR
                + RTC_SHADOW::PWR_FAIL_SHADOW_EN::SET
                + match policy.power_fail {
                    PowerFailState::Off => RTC_SHADOW::PWR_FAIL::Off,
                    PowerFailState::On => RTC_SHADOW::PWR_FAIL::On,
                    PowerFailState::Previous => RTC_SHADOW::PWR_FAIL::Previous,
                },
        );
    // PmDecodeEn: HPET on (64-bit width off), ASF clock switch, SMBus/ASF
    // base B00h; Rome/Naples HPET MSI on, Milan off. The contract shows the
    // low byte stored as 40h first, which would turn the legacy, CF9,
    // SMBus and IOAPIC decodes off; am4-re records PmDecodeEn = 20040BF7
    // after the same task on Huashan, so the low byte's other bits are kept
    // here (FCH_SP3 §3.2 af50, open item).
    update::<u8, _>(acpi, PM + pm::DECODE_EN, 0, 0x40);
    let (clear, set) = match generation {
        Generation::Milan => (0x2018_F400, 0x0004_0B40),
        Generation::Rome | Generation::Naples => (0x0018_F400, 0x2004_0B40),
    };
    update::<u32, _>(acpi, PM + pm::DECODE_EN, clear, set);
    uart::env(hw);
    // IOMUX 59h = 1 (pin 89 function 1).
    acpi.write8(IOMUX + 0x59, 1);
    if generation != Generation::Naples {
        alink::modify(hw.io(), alink::MISC_CTL, 1 << 3, 0);
    }
}

/// af55: legacy SD off; Huashan also sets SMN 13D0004C/13E0004C bit 4
/// (the xHCI strap/SSID lock of both USB controllers, am4-re).
pub fn sd<H: FchHw>(hw: &H, generation: Generation) {
    let acpi = hw.acpi();
    update::<u8, _>(acpi, PM + pm::MANUAL_RESET, 0x40, 0);
    update::<u8, _>(acpi, PM + pm::PM_E8, 0x01, 0);
    acpimmio::aoac_power(acpi, AoacDevice::SD, false);
    if generation != Generation::Naples {
        let smn = hw.smn_window();
        [0x13D0_004C, 0x13E0_004C]
            .into_iter()
            .for_each(|r| update::<u32, _>(&smn, r, 0, 0x10));
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::model::fch;
    use super::*;

    #[cfg(feature = "milan")]
    #[test]
    fn milan_env() {
        let hw = fch();
        hw.acpi.put(PM, 4, 0x2004_0bf7);
        hw_acpi_pre(&hw, Generation::Milan);
        hw_acpi(&hw, Generation::Milan, &EnvPolicy::default());
        assert_eq!(hw.acpi.get(PM, 4), 0x0004_0bf7);
        assert_eq!(hw.acpi.get(PM + 0x5b, 1), 0x04);
        assert_eq!(hw.acpi.get(PM + 0x77, 1), 0x09);
        assert_eq!(hw.acpi.get(0x279, 1), 0, "Milan does not map 79h");
        assert_eq!(hw.acpi.get(0x270, 1), 0x09);
        assert_eq!(hw.acpi.get(MISC + 0x40, 4), 1 << 30);
        assert_eq!(hw.acpi.get(PM + 0x3c, 2), 0, "Rome only");
    }

    #[test]
    fn power_fail_previous() {
        let hw = fch();
        hw_acpi(
            &hw,
            Generation::Rome,
            &EnvPolicy {
                power_fail: PowerFailState::Previous,
            },
        );
        assert_eq!(hw.acpi.get(PM + 0x5b, 1), 0x07);
    }

    #[test]
    fn sd_off_and_xhci_lock() {
        let hw = fch();
        sd(&hw, Generation::Rome);
        assert_eq!(
            super::super::super::model::smn(&hw.pci, 0, 0x13e0_004c),
            0x10
        );
        assert_eq!(hw.acpi.writes_to(0x1e72), vec![0]);
    }
}
