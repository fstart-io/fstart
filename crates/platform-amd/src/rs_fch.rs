// SPDX-License-Identifier: MIT OR Apache-2.0
//! Songshan FCH platform sequences of an Rs SoC: the board's CPM GPIO table,
//! the ACPI I/O decode and direct ACPI mode. Mechanisms live in
//! `fstart_driver_amd::fch`; policy (tables, PM base, console IRQ) comes
//! from the platform and board.
use fstart_driver_amd::fch;
#[cfg(target_os = "none")]
use fstart_driver_amd::fch::{Acpimmio, Bus, Fch, gpio::Gpio, songshan};
#[cfg(target_os = "none")]
use tock_registers::interfaces::Readable;

/// Apply the board's AMD CPM GPIO table (AmdCpmGpioInitPeim and the OEM PEI
/// callbacks): `(logical pin, CPM setting)` rows on local bank 0, in order,
/// before MPIO. Pins of the alternate bank (logical >= 256, ACPI MMIO
/// `0x1200`/`0x12c0`) are not decoded yet: they are reported, never guessed.
#[cfg(target_os = "none")]
pub fn apply_cpm_table<B: Bus>(bus: &B, pads: &[(u16, u16)]) {
    let gpio = Gpio::new(Fch::new(bus, Acpimmio::DIE0), &songshan::GPIO_BANK);
    for &(pin, setting) in pads {
        match u8::try_from(pin).ok().map(|p| gpio.apply_cpm(p, setting)) {
            Some(Ok(())) => {}
            _ => fstart_log::warn!(
                "rs: CPM GPIO {} setting {:#x} not applied (alternate bank)",
                pin,
                setting
            ),
        }
    }
    fstart_log::info!("rs: CPM GPIO table applied rows={}", pads.len());
}

/// Decode the ACPI PM1 event/control, PM timer and GPE0 blocks at `pmbase`
/// (+0/+4/+8/+20h) with the global, RTC and timer enables, then prove the
/// PM timer runs. Before I/O routing: the primary root keeps these ports.
#[cfg(target_os = "none")]
pub fn acpi_decode(fch: &Fch<impl Bus>, pmbase: u16) -> Result<(), ()> {
    fch.acpi_ports(fch::AcpiPorts {
        event: pmbase,
        control: pmbase + 4,
        timer: pmbase + 8,
        gpe: pmbase + 0x20,
    });
    let timer = fch.reg(fch::acpi::at::<u32, ()>(0x08));
    let first = timer.get();
    if first == u32::MAX || !fstart_arch::x86::wait_us(100_000, || timer.get() != first) {
        fstart_log::error!("rs: ACPI PM timer at {:#x} not advancing", pmbase + 8);
        return Err(());
    }
    Ok(())
}

tock_registers::register_bitfields![u8, Mask [ALL OFFSET(0) NUMBITS(8) []]];

/// Cold boot accepts reset S0 and a completed S5 shutdown, not S1-S4 resume
/// or a sleep request still in progress. After an S5 shutdown PM1_CNT reads
/// 1401h: type 5 is retained even though SLP_EN self-cleared.
fn cold_sleep_type(control: u16) -> Result<bool, ()> {
    let control = tock_registers::LocalRegisterCopy::<u16, fch::Pm1Control::Register>::new(control);
    if control.is_set(fch::Pm1Control::SLP_EN) {
        return Err(());
    }
    match control.read(fch::Pm1Control::SLP_TYP) {
        0 => Ok(false),
        5 => Ok(true),
        _ => Err(()),
    }
}

/// Direct ACPI mode without a fake SMI enable/disable protocol: route SCI
/// (and the console UART's FCH interrupt, `(PCI_INTR source, IRQ)`) in PIC
/// and APIC mode, mask the PICs, clear and mask every event, enable the power
/// button and S5, then hand over with SCI_EN set. All event enables remain
/// masked until the OS installs its SCI handler.
#[cfg(target_os = "none")]
pub fn prepare_acpi_mode(fch: &Fch<impl Bus>, uart: Option<(u8, u8)>) -> Result<(), ()> {
    use tock_registers::interfaces::{ReadWriteable, Writeable};
    // SMIs may be on only for the PSP's SMM (`rs_smm`): the SMI command
    // port and the PSP's SMI, no source the OS takes as an SCI.
    let smi_ok = || {
        fch.reg(fch::SMI_TRIGGER0)
            .is_set(fch::SmiTrigger0::SMI_DISABLE)
            || (0..(fch::SMI_CONTROLS * 16) as u8)
                .filter(|&source| fch::smi_routing(fch, source) == fch::SmiMode::Smi as u32)
                .all(|source| {
                    matches!(
                        source,
                        fch::smi::source::SMI_COMMAND | fch::smi::source::PSP
                    )
                })
    };
    if !smi_ok()
        || !fch.reg(fch::PM_DECODE).is_set(fch::Decode::IOAPIC)
        || !fch.reg(fch::PM_DECODE).is_set(fch::Decode::HPET)
    {
        fstart_log::error!(
            "rs: ACPI mode needs SMIs off (or only the PSP's) and IOAPIC/HPET decoded: SMI-trigger={:#x} decode={:#x}",
            fch.reg(fch::SMI_TRIGGER0).get(),
            fch.reg(fch::PM_DECODE).get()
        );
        return Err(());
    }
    let control = fch.reg(fch::ACPI_PM1_CONTROL);
    let cold = cold_sleep_type(control.get()).inspect_err(|_| {
        fstart_log::error!(
            "rs: PM1 control={:#x} is neither S0 nor a completed S5",
            control.get()
        );
    })?;
    if cold {
        // Clear the completed S5 type without asserting SLP_EN or altering
        // unrelated PM control bits. This is full cold init, never resume.
        control.modify(fch::Pm1Control::SLP_TYP.val(0) + fch::Pm1Control::SLP_EN::CLEAR);
        if control.read(fch::Pm1Control::SLP_TYP) != 0 || control.is_set(fch::Pm1Control::SLP_EN) {
            return Err(());
        }
        fstart_log::info!("rs: completed S5 sleep type cleared for cold boot");
    }
    // The verified DF port routes are installed, BSP IF=0, APs parked.
    // Initialize the FCH legacy interrupt paths before advertising PCAT
    // compatibility and SCI9. Reading the IOAPIC ID alone isn't init.
    use fch::{HPET, Reg, irq};
    let bus = fch.bus();
    let misc = irq::register::<_, ()>(bus, 0x08, irq::Mode::Pic);
    let misc0 = irq::register::<_, ()>(bus, 0x09, irq::Mode::Pic);
    fstart_log::info!(
        "rs: FCH IRQ cold Misc={:#x} Misc0={:#x} HPET-config={:#x}",
        misc.get(),
        misc0.get(),
        bus.reg(Reg::<u32>::mmio(HPET + 0x10)).get()
    );
    if misc.get() == 0xff || misc0.get() == 0xff {
        return Err(());
    }
    irq::route_legacy(bus);
    if misc.get() & 0xfa != 0xfa || misc0.get() & 0x76 != 0x10 {
        return Err(());
    }
    let sci = (0x10, fch::SCI_IRQ);
    for (source, pin) in core::iter::once(sci).chain(uart) {
        for mode in [irq::Mode::Pic, irq::Mode::Apic] {
            irq::write(bus, source, mode, pin);
            if irq::read(bus, source, mode) != pin {
                return Err(());
            }
        }
    }
    fstart_arch::x86::legacy_pc::initialize_pic(0x20, 0x28);
    for port in [0x21, 0xa1] {
        bus.reg(Reg::<u8, Mask::Register>::io(port))
            .write(Mask::ALL.val(0xff));
    }
    fstart_log::info!(
        "rs: FCH IRQ initialized Misc={:#x} Misc0={:#x} SCI={} PICs masked",
        misc.get(),
        misc0.get(),
        fch::SCI_IRQ
    );
    fch.reg(fch::ACPI_PM1_ENABLE).set(0);
    fch.reg(fch::acpi::at::<u32, ()>(0x24)).set(0);
    let status = fch.reg(fch::ACPI_PM1_STATUS);
    status.set(status.get());
    let gpe = fch.reg(fch::acpi::at::<u32, ()>(0x20));
    gpe.set(gpe.get());
    // Fixed PM events follow SCI_EN. Disable the alternative SMI paths,
    // and the SMI command port: the PSP's SMM needed it only at boot. Do
    // not request retry/interception; only the PSP's SMI may stay.
    for source in [51, 65, 75] {
        fch::smi_route(fch, source, fch::SmiMode::Disable);
    }
    fch.reg(fch::PM_PCI_CONTROL).modify(
        fch::PciControl::FORCE_SLPSTATE_RETRY::CLEAR + fch::PciControl::FORCE_STPCLK_RETRY::CLEAR,
    );
    fch.reg(fch::PM_RESET_CONTROL1)
        .modify(fch::ResetControl1::SLP_TYPE_EN::SET);
    fch.reg(fch::PM_ACPI_CONFIG_HIGH)
        .modify(fch::AcpiConfigHigh::PWRBTN_EN::SET);
    fch.reg(fch::ACPI_PM1_CONTROL)
        .modify(fch::Pm1Control::SCI_EN::SET + fch::Pm1Control::SLP_EN::CLEAR);
    if !fch
        .reg(fch::ACPI_PM1_CONTROL)
        .is_set(fch::Pm1Control::SCI_EN)
        || !smi_ok()
    {
        return Err(());
    }
    fstart_log::info!(
        "rs: direct ACPI mode SCI_EN=1, events masked, SMI-trigger={:#x}; no SMI_CMD advertised",
        fch.reg(fch::SMI_TRIGGER0).get()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cold_boot_accepts_only_s0_or_completed_s5_not_resume_or_pending_sleep() {
        assert_eq!(cold_sleep_type(0), Ok(false));
        assert_eq!(cold_sleep_type(1), Ok(false));
        assert_eq!(cold_sleep_type(0x1401), Ok(true));
        for sleep in 0u16..8 {
            assert!(cold_sleep_type((sleep << 10) | 0x2000 | 1).is_err());
            if !matches!(sleep, 0 | 5) {
                assert!(cold_sleep_type((sleep << 10) | 1).is_err());
            }
        }
        let mut control =
            tock_registers::LocalRegisterCopy::<u16, fch::Pm1Control::Register>::new(0xd601);
        assert_eq!(cold_sleep_type(control.get()), Ok(true));
        control.modify(fch::Pm1Control::SLP_TYP.val(0) + fch::Pm1Control::SLP_EN::CLEAR);
        assert_eq!(control.get(), 0xc201); // unrelated bits, including SCI_EN, survive
    }
}
