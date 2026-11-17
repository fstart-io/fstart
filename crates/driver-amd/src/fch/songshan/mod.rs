// SPDX-License-Identifier: MIT OR Apache-2.0
//! Songshan, the FCH of the RS I/O die (Genoa, Siena): what it does
//! differently from the other FCHs. The registers it shares with them are
//! the common ones of [`crate::fch`].
pub mod uart;
pub mod usb;

use super::acpimmio::{
    AcpiConfigHigh, AcpiPorts, Decode, PM_ACPI_CONFIG_HIGH, PM_DECODE, PM_PCI_CONTROL,
    PM_RESET_CONTROL1, PciControl, ResetControl1,
};
use super::{FchHw, irq, sx};
use crate::window::Window;
use tock_registers::interfaces::ReadWriteable;
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
/// console: no SMI until a handler is installed, the legacy decodes, the
/// IOAPIC and the HPET decoded (not HPET MSI, as the vendor's FCH init
/// leaves it), and the legacy SD controller off (no driver, no BAR, no INTx
/// route; NFX FchSongshanDxe, openSIL's `SdConfig`).
pub fn take_over(fch: &impl FchHw) {
    let acpi = fch.acpi();
    super::smi::disable(acpi);
    super::acpimmio::enable_legacy_decode(acpi);
    acpi.at(PM_DECODE)
        .modify(Decode::IOAPIC::SET + Decode::HPET::SET);
    super::acpimmio::legacy_sd_off(acpi);
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
/// a completed S5's sleep type cleared, every event cleared and masked, and
/// SCI_EN set ([`sx::acpi_mode`]). The OS enables the events it handles.
pub fn acpi_mode(fch: &impl FchHw, ports: &AcpiPorts) {
    use super::acpimmio::Pm1Control;
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
    fch.io().write16(u32::from(ports.event) + 2, 0);
    sx::acpi_mode(fch, ports, true);
}

/// The PCIe slot reset outputs: pad 26 and pad 10 of the remote bank.
const PERST: [super::gpio::Pin; 2] = [super::gpio::Pin(26), super::gpio::Pin(266)];

/// Release PCIe PERST after MPIO's PERST phase: both outputs driven high
/// at drive strength 2 (Genoa openSIL `GpioSlotResetControl`, the S8040's
/// FchPerstDeassertGpio `ffa46748`). `false` if a pad doesn't decode or
/// doesn't read back.
pub fn release_perst(fch: &impl FchHw) -> bool {
    use super::gpio::Control;
    use tock_registers::interfaces::{Readable, Writeable};
    let acpi = fch.acpi();
    let control = |pin: super::gpio::Pin| acpi.reg::<u32, Control::Register>(pin.control());
    if PERST.iter().any(|&pin| control(pin).get() == u32::MAX) {
        return false;
    }
    let value = Control::OUTPUT::SET + Control::OUTPUT_ENABLE::SET + Control::DRIVE_STRENGTH.val(2);
    PERST.iter().all(|&pin| {
        control(pin).write(value);
        control(pin).matches_all(value)
    })
}
