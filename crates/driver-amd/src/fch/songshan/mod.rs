// SPDX-License-Identifier: MIT OR Apache-2.0
//! Songshan, the FCH of the RS I/O die (Genoa, Siena): what it does
//! differently from the other FCHs. The registers it shares with them are
//! the common ones of [`crate::fch`].
pub mod uart;
pub mod usb;

use super::acpimmio::{
    AcpiConfigHigh, AcpiPorts, CgPllConfig1, CgPllConfig3, CgPllConfig4, CgPllConfig5, Decode,
    MISC_48, MISC_CGPLL_CONFIG1, MISC_CGPLL_CONFIG3, MISC_CGPLL_CONFIG4, MISC_CGPLL_CONFIG5,
    MISC_CLK_CNTL1, Misc48, MiscClkCntl1, PM_ACPI_CONFIG_HIGH, PM_D2, PM_DECODE, PM_PCI_CONTROL,
    PM_RESET_CONTROL1, PciControl, PmioDebug, ResetControl1,
};
use super::{FchHw, irq, sx};
use crate::window::Window;
use tock_registers::interfaces::{ReadWriteable, Writeable};
use tock_registers::register_bitfields;

register_bitfields![u8,
    /// PCI_INTR index 08h: where the IOAPIC inputs shared with legacy
    /// devices come from. IRQ14/15 come from the legacy IDE unless routed.
    pub IntrMisc [
        IRQ1_ROUTED OFFSET(1) NUMBITS(1) [],
        IRQ12_ROUTED OFFSET(3) NUMBITS(1) [],
        IRQ14 OFFSET(4) NUMBITS(2) [
            LegacyIde = 0,
            Routed = 3
        ],
        IRQ15 OFFSET(6) NUMBITS(2) [
            LegacyIde = 0,
            Routed = 3
        ]
    ],
    /// PCI_INTR index 09h; the controls are not named in public.
    pub IntrMisc0 [
        CONTROL_2_1 OFFSET(1) NUMBITS(2) [],
        CONTROL_6_4 OFFSET(4) NUMBITS(3) []
    ]
];

/// The FCH as the native flow takes it over from the PSP, before the
/// console: CF9h resets on, no SMI until a handler is installed, the
/// legacy decodes, the IOAPIC and the HPET decoded (not HPET MSI, as the
/// vendor's FCH init leaves it), and the legacy SD controller off (no
/// driver, no BAR, no INTx route; NFX FchSongshanDxe, openSIL's
/// `SdConfig`).
///
/// CF9h resets need PMxD2\[6\] (Cf9RstDisable) clear, which nothing else
/// on the way to the OS clears: the vendors' first FCH reset task clears
/// it (the S8040's V2.00 FchSongshanPei, task `af4f` at `ffd1093b`, as
/// openSIL's `FchInitResetHwAcpiP`, FchHwAcpi.c:493), and their SMM sets
/// it again to trap CF9h (AMI CF9IoTrap, in the S8040's and the S4056's
/// firmware). The S8040 had it set on every fstart boot, cold ones too,
/// and its CF9h resets did nothing: the OS's, and the warm reset that keeps
/// a fatal error for the BERT. As the vendor's task, this goes through
/// the ACPI MMIO: the PM I/O ports (CD6h/CD7h) read all-ones on the
/// S8040.
pub fn take_over(fch: &impl FchHw) {
    let acpi = fch.acpi();
    acpi.at(PM_D2).modify(PmioDebug::CF9_RST_DISABLE::CLEAR);
    super::smi::disable(acpi);
    super::acpimmio::enable_legacy_decode(acpi);
    acpi.at(PM_DECODE)
        .modify(Decode::IOAPIC::SET + Decode::HPET::SET);
    super::acpimmio::legacy_sd_off(acpi);
}

/// Spread the PCIe reference clocks by -0.3 % with the CG1 PLL, or stop
/// spreading them: the FCH 8.0 sequence of openSIL's
/// `ProgramFchEnvSpreadSpectrumV80` (Genoa) and the S8040's V2.00
/// FchSongshanDxe `ProgramFchEnvSpreadSpectrum` (`0x3730`). In external
/// clock mode the PLL never spreads. Nothing is written if MISCx08\[0\]
/// already has the wanted state.
///
/// The S8040's AGESA runs it in its DXE env, after PCIe training; openSIL
/// runs it at TP1, before MPIO. A PLL update with the links up is what
/// the vendor's own image does, and what the MJ11's runtime toggles did
/// without losing a link.
pub fn spread_spectrum(fch: &impl FchHw, enable: bool) -> Spread {
    use fstart_core::services::Timer;
    use tock_registers::interfaces::Readable;
    let acpi = fch.acpi();
    let enable = enable && !super::acpimmio::external_clock(acpi);
    let config1 = acpi.at(MISC_CGPLL_CONFIG1);
    if config1.is_set(CgPllConfig1::CG1_SPREAD_SPECTRUM) == enable {
        return Spread {
            on: enable,
            changed: false,
        };
    }
    Timer::delay_us(fch.timer(), 5);
    if enable {
        acpi.at(MISC_CGPLL_CONFIG4)
            .modify(CgPllConfig4::CG1_FCW1_FRAC_OVERRIDE.val(0x1333));
        acpi.at(MISC_CGPLL_CONFIG5)
            .modify(CgPllConfig5::CG1_FCW_SLEW_FRAC_OVERRIDE.val(0x3a));
    }
    acpi.at(MISC_CGPLL_CONFIG3)
        .modify(CgPllConfig3::CG1_FRACN_EN_OVERRIDE::SET);
    config1.modify(CgPllConfig1::CG1_SPREAD_SPECTRUM.val(enable.into()));
    let cntl1 = acpi.at(MISC_CLK_CNTL1);
    if enable {
        cntl1.modify(MiscClkCntl1::CG1_FBDIV_LOADEN::SET);
    }
    acpi.at(MISC_48).modify(Misc48::BIT11::SET);
    cntl1.modify(MiscClkCntl1::CG1_CFG_UPDATE_REQ::SET);
    Timer::delay_us(fch.timer(), 15);
    Spread {
        on: enable,
        changed: true,
    }
}

/// What [`spread_spectrum`] left: whether the PLL spreads, and whether
/// that changed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spread {
    pub on: bool,
    pub changed: bool,
}

/// PCI_INTR 08h/09h as AMD's FCH init leaves them (Picasso AGESA, openSIL,
/// the S8040's NFX FchSongshanPei; both RS vendor firmwares read back
/// FAh/91h): the keyboard, mouse and IRQ14/15 inputs of the IOAPIC come
/// from their internal sources, and 09h is `(old & 99h) | 10h`.
pub fn route_legacy(fch: &impl FchHw) {
    let (misc, misc0) = (
        irq::index(0x08, irq::Mode::Pic),
        irq::index(0x09, irq::Mode::Pic),
    );
    let routed = IntrMisc::IRQ1_ROUTED::SET
        + IntrMisc::IRQ12_ROUTED::SET
        + IntrMisc::IRQ14::Routed
        + IntrMisc::IRQ15::Routed;
    irq::update(fch.io(), misc, routed.mask(), routed.value);
    let controls = IntrMisc0::CONTROL_2_1.val(0) + IntrMisc0::CONTROL_6_4.val(1);
    irq::update(fch.io(), misc0, controls.mask(), controls.value);
}

/// Hand the OS ACPI directly, without an SMI command port: the legacy
/// interrupt inputs routed, the 8259s masked, the power button and the FCH's own S5 entry on SLP_TYP enabled,
/// a completed S5's sleep type cleared, every event cleared and masked,
/// PCIe WAKE# kept from waking the system, and SCI_EN set
/// ([`sx::acpi_mode`]). The OS enables the events it handles.
///
/// PM1_EN's PCIEXP_WAKE_DIS is set at power-on and the OS leaves it as it
/// finds it (the FADT has no PCI_EXP_WAK): cleared, a PCIe WAKE# took the
/// S8040 out of the OS's S5 a second after it entered it (PM1_STS
/// WAK_STS and PCIEXP_WAKE_STS on the next boot, the BMC seeing the
/// supply's power-good come back without a power-button press). A device
/// that is to wake the system does it through its GPE.
pub fn acpi_mode(fch: &impl FchHw, ports: &AcpiPorts) {
    use super::acpimmio::{Pm1Control, Pm1Enable};
    route_legacy(fch);
    fstart_arch::x86::legacy_pc::initialize_pic(0x20, 0x28);
    for mask in [0x21u16, 0xa1] {
        fch.io().write8(mask.into(), 0xff);
    }
    let acpi = fch.acpi();
    acpi.at(PM_PCI_CONTROL)
        .modify(PciControl::FORCE_SLPSTATE_RETRY::CLEAR + PciControl::FORCE_STPCLK_RETRY::CLEAR);
    acpi.at(PM_RESET_CONTROL1)
        .modify(ResetControl1::SLP_TYPE_EN::SET);
    acpi.at(PM_ACPI_CONFIG_HIGH)
        .modify(AcpiConfigHigh::PWRBTN_EN::SET);
    // A cold boot after an S5 shutdown still reads SLP_TYP 5.
    let control = fch
        .io()
        .reg::<u16, Pm1Control::Register>(ports.control.into());
    control.modify(Pm1Control::SLP_TYP.val(0) + Pm1Control::SLP_EN::CLEAR);
    fch.io()
        .reg::<u16, Pm1Enable::Register>(u32::from(ports.event) + 2)
        .write(Pm1Enable::PCIEXP_WAKE_DIS::SET);
    sx::acpi_mode(fch, ports, true);
}

/// A PCIe slot reset (PERST#) output of a board: its pad, and the drive
/// strength (pad control\[18:17\]) it drives with once released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Perst {
    pub pin: super::gpio::Pin,
    pub drive_strength: u8,
}

/// Release the board's PCIe slot resets `outputs` after MPIO's PERST
/// phase: each pad driven high, its output enabled at its drive strength,
/// its other controls clear (as Genoa openSIL's `GpioSlotResetControl` and
/// the S8040's FchPerstDeassertGpio `ffa46748` write them). `false`, with
/// nothing written, if a pad is not one of the FCH's, doesn't decode or
/// has a drive strength the field can't hold; `false` too if one doesn't
/// read back.
pub fn release_perst(fch: &impl FchHw, outputs: &[Perst]) -> bool {
    use super::gpio::Control;
    use tock_registers::interfaces::Readable;
    let acpi = fch.acpi();
    let control = |pin: super::gpio::Pin| acpi.reg::<u32, Control::Register>(pin.control());
    if outputs.iter().any(|output| {
        !output.pin.is_valid() || output.drive_strength > 3 || control(output.pin).get() == u32::MAX
    }) {
        return false;
    }
    outputs.iter().all(|output| {
        let value = Control::OUTPUT::SET
            + Control::OUTPUT_ENABLE::SET
            + Control::DRIVE_STRENGTH.val(output.drive_strength.into());
        control(output.pin).write(value);
        control(output.pin).matches_all(value)
    })
}

#[cfg(test)]
mod tests {
    use crate::fch::model::fch;

    use super::{Spread, spread_spectrum};
    const MISC: u32 = crate::fch::acpimmio::MISC;
    /// MISCx82 with the internal clock strap.
    fn internal_clock(hw: &crate::fch::model::TestFch) {
        hw.acpi.put(MISC + 0x82, 1, 0x02);
    }
    /// The MISC writes, as `(offset, value)`.
    fn writes(hw: &crate::fch::model::TestFch) -> Vec<(u32, u32)> {
        hw.acpi
            .writes()
            .into_iter()
            .map(|w| (w.0 - MISC, w.2))
            .collect()
    }
    const CHANGED: fn(bool) -> Spread = |on| Spread { on, changed: true };
    const KEPT: fn(bool) -> Spread = |on| Spread { on, changed: false };

    /// Turning it on writes the -0.3 % values in the vendor's order,
    /// keeping the other bits, between a 5 us and a 15 us stall.
    #[test]
    fn spread_spectrum_on_is_the_fch80_sequence() {
        let hw = fch();
        internal_clock(&hw);
        hw.acpi.put(MISC + 0x08, 4, 0x8000_0000);
        hw.acpi.put(MISC + 0x10, 4, 0x0000_0001);
        hw.acpi.put(MISC + 0x14, 4, 0xaaff_ffaa);
        hw.acpi.put(MISC + 0x18, 4, 0xffff_5555);
        hw.acpi.put(MISC + 0x40, 4, 0x0000_0002);
        hw.acpi.put(MISC + 0x48, 4, 0x0000_0004);
        assert_eq!(spread_spectrum(&hw, true), CHANGED(true));
        assert_eq!(
            writes(&hw),
            [
                (0x14, 0xaa13_33aa),
                (0x18, 0x003a_5555),
                (0x10, 0x2000_0001),
                (0x08, 0x8000_0001),
                (0x40, 0x0200_0002),
                (0x48, 0x0000_0804),
                (0x40, 0x4200_0002),
            ]
        );
        assert_eq!(hw.timer.now(), 20);
    }

    /// Turning it off leaves the fractions and the feedback divider.
    #[test]
    fn spread_spectrum_off_leaves_the_fractions() {
        let hw = fch();
        internal_clock(&hw);
        hw.acpi.put(MISC + 0x08, 4, 0x8000_0001);
        assert_eq!(spread_spectrum(&hw, false), CHANGED(false));
        assert_eq!(
            writes(&hw),
            [
                (0x10, 0x2000_0000),
                (0x08, 0x8000_0000),
                (0x48, 0x0000_0800),
                (0x40, 0x4000_0000),
            ]
        );
        assert_eq!(hw.timer.now(), 20);
    }

    /// An external clock is never spread: on is turned off, and off is
    /// left as it is.
    #[test]
    fn spread_spectrum_never_spreads_an_external_clock() {
        let hw = fch();
        hw.acpi.put(MISC + 0x08, 4, 1);
        assert_eq!(spread_spectrum(&hw, true), CHANGED(false));
        assert_eq!(
            writes(&hw).iter().map(|w| w.0).collect::<Vec<_>>(),
            [0x10, 0x08, 0x48, 0x40]
        );
        assert_eq!(hw.acpi.get(MISC + 0x08, 4), 0);

        let off = fch();
        assert_eq!(spread_spectrum(&off, true), KEPT(false));
        assert!(off.acpi.writes().is_empty());
        assert_eq!(off.timer.now(), 0);
    }

    /// Already as wanted: nothing written, no stall.
    #[test]
    fn spread_spectrum_changes_only_what_differs() {
        let hw = fch();
        internal_clock(&hw);
        assert_eq!(spread_spectrum(&hw, false), KEPT(false));
        hw.acpi.put(MISC + 0x08, 4, 1);
        assert_eq!(spread_spectrum(&hw, true), KEPT(true));
        assert!(hw.acpi.writes().is_empty());
        assert_eq!(hw.timer.now(), 0);
    }

    #[test]
    fn take_over_lets_cf9_reset() {
        let hw = fch();
        // Cf9RstDisable as the vendor's SMM leaves it.
        hw.acpi.put(crate::fch::acpimmio::PM + 0xd2, 1, 0x45);
        super::take_over(&hw);
        assert_eq!(hw.acpi.get(crate::fch::acpimmio::PM + 0xd2, 1), 0x05);
    }

    /// The board's pads, as openSIL's reference board writes them: pad 26
    /// and pad 10 of the remote bank at drive strength 2.
    #[test]
    fn perst_is_the_boards_pads_at_the_boards_strength() {
        use super::{Perst, release_perst};
        use crate::fch::gpio::Pin;
        let hw = fch();
        let outputs = [
            Perst {
                pin: Pin(26),
                drive_strength: 2,
            },
            Perst {
                pin: Pin(266),
                drive_strength: 2,
            },
        ];
        assert!(release_perst(&hw, &outputs));
        assert_eq!(hw.acpi.get(0x1500 + 0x68, 4), 0xc4_0000);
        assert_eq!(hw.acpi.get(0x1200 + 0x28, 4), 0xc4_0000);
        // A pad that isn't the FCH's, or a strength the field can't hold:
        // nothing is written.
        for bad in [
            Perst {
                pin: Pin(400),
                drive_strength: 2,
            },
            Perst {
                pin: Pin(27),
                drive_strength: 4,
            },
        ] {
            let hw = fch();
            assert!(!release_perst(&hw, &[outputs[0], bad]));
            assert_eq!(hw.acpi.get(0x1500 + 0x68, 4), 0);
        }
    }
}
