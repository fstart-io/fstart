// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SMM side of every FCH: ACPI mode, sleep entry, the power button and
//! the NBIO USB SMI. These are the steps an SMM handler calls; source
//! decoding and dispatch are [`super::smi`]'s.
//!
//! The sleep entry is the vendor FchTaishanSmmInit/FchHuashan Sx path
//! (FCH_SP3 §5, sp4r2 FCH_TAISHAN): the Sx callbacks, the USB entry, the
//! FCH armed for the OS's SLP_TYP, the SMU's SleepEntry, then SLP_EN.
//! Sandstone (Picasso) has no Sx callbacks and no SMU message.

use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use super::FchHw;
use super::acpimmio::{
    AOAC_94, AcpiConfigHigh, AcpiPorts, Aoac94, MISC, PM_0B, PM_ACPI_CONFIG_HIGH, PM_EE,
    PM_PCI_CONTROL, PM_RESET_CONTROL1, PciControl, Pm1Control, Pm1Enable, Pm1Status, ResetControl1,
};
use super::smi::{self, Route, source};
use super::{IOAPIC, SCI_IRQ, irq};
use crate::smu::{Error as SmuError, SmuMessages, v9, v11};
use crate::window::{At, Reg, Window, update};

/// The FCH and SMU generation the sleep path distinguishes.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    /// Raven/Picasso: no Sx callbacks, no SMU message.
    Sandstone,
    /// Zeppelin dies (the die SoC and Naples): SMU v9.
    Taishan,
    /// Rome: SMU v11.
    Huashan,
    /// Milan: SMU v11 with a DXIO request first.
    Milan,
}

/// Why an Sx step failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SxError {
    Smu(SmuError),
    /// The Naples USB/SMU handshake did not complete (bounded poll).
    UsbHandshake,
}

impl From<SmuError> for SxError {
    fn from(e: SmuError) -> Self {
        Self::Smu(e)
    }
}

/// Milan's DXIO request before SleepEntry: args `3Ah, 1, 5` (meaning not
/// public, FCH_SP3 §10).
const MILAN_DXIO_SX_ARGS: [u32; 6] = [0x3A, 1, 5, 0, 0, 0];

fn pm1_status<'a, I: Window>(io: &'a I, ports: &AcpiPorts) -> Reg<&'a I, u16, Pm1Status::Register> {
    io.at(At::new(ports.event.into()))
}
fn pm1_enable<'a, I: Window>(io: &'a I, ports: &AcpiPorts) -> Reg<&'a I, u16, Pm1Enable::Register> {
    io.at(At::new(u32::from(ports.event) + 2))
}
fn pm1_control<'a, I: Window>(
    io: &'a I,
    ports: &AcpiPorts,
) -> Reg<&'a I, u16, Pm1Control::Register> {
    io.at(At::new(ports.control.into()))
}
/// GPE0 status, then enable at half the block (`GPE0_BLK_LEN` 8).
fn gpe0<'a, I: Window>(io: &'a I, ports: &AcpiPorts, enable: bool) -> Reg<&'a I, u32> {
    io.at(At::new(u32::from(ports.gpe) + if enable { 4 } else { 0 }))
}

/// Clear the PM1 and GPE0 status (write-1-to-clear).
fn clear_pm_gpe_status<I: Window>(io: &I, ports: &AcpiPorts) {
    let event = pm1_status(io, ports);
    event.set(event.get());
    let gpe = gpe0(io, ports, false);
    gpe.set(gpe.get());
}

/// Whether the OS owns ACPI (PM1_CNT.SCI_EN).
pub fn sci_enabled<I: Window>(io: &I, ports: &AcpiPorts) -> bool {
    pm1_control(io, ports).is_set(Pm1Control::SCI_EN)
}

/// Route the power button to SMI: before the OS owns ACPI it powers off
/// through SMM.
pub fn enable_power_button_smi<A: Window, I: Window>(acpi: &A, io: &I, ports: &AcpiPorts) {
    pm1_status(io, ports).write(Pm1Status::PWRBTN::SET);
    pm1_enable(io, ports).modify(Pm1Enable::PWRBTN::SET);
    acpi.at(PM_ACPI_CONFIG_HIGH)
        .modify(AcpiConfigHigh::PWRBTN_EN::SET);
    smi::set_route(acpi, source::PWR_BTN, Route::Smi);
}

/// The ACPI enable and disable SW SMIs. Enable: the power button to the
/// OS, GPE0 off and clear, the SCI on IRQ 9, SCI_EN. Disable: SCI_EN off,
/// the power button back to SMI.
pub fn acpi_mode<H: FchHw>(hw: &H, ports: &AcpiPorts, enable: bool) {
    let (acpi, io) = (hw.acpi(), hw.io());
    if enable {
        smi::set_route(acpi, source::PWR_BTN, Route::Disabled);
        gpe0(io, ports, true).set(0);
        clear_pm_gpe_status(io, ports);
        irq::write(io, irq::index(irq::SCI, irq::Mode::Pic), SCI_IRQ);
        irq::write(io, irq::index(irq::SCI, irq::Mode::Apic), SCI_IRQ);
        pm1_control(io, ports).modify(Pm1Control::SCI_EN::SET);
    } else {
        pm1_control(io, ports).modify(Pm1Control::SCI_EN::CLEAR);
        enable_power_button_smi(acpi, io, ports);
    }
}

/// USB SMI (SmiStat1\[21\]): acknowledge in the IOHC of the USB
/// controllers. `buses` are the root buses of every die (Naples) or socket 0
/// (Rome/Milan, both controllers in the IOD).
pub fn usb_smi<H: FchHw>(hw: &H, version: Version, buses: &[u8]) {
    let acks: &[(u32, u32)] = match version {
        Version::Sandstone => &[],
        Version::Taishan => &[(0x13B1_0120, 1), (0x13B2_0020, 0x100)],
        Version::Huashan | Version::Milan => &[
            (0x13D1_0120, 1),
            (0x13D2_0020, 0x100),
            (0x13E1_0120, 1),
            (0x13E2_0020, 0x100),
        ],
    };
    buses.iter().for_each(|&bus| {
        // SAFETY: SMM owns the SMN pair while it runs (callers save/restore
        // the index).
        let smn = unsafe { crate::smn::Smn::on_bus(hw.pci().clone(), bus) };
        acks.iter().for_each(|&(r, v)| smn.write32(r, v));
    });
    smi::clear(hw.acpi(), source::USB_NBIO);
}

/// Mask every FCH IOAPIC redirection entry after flushing pin 23 (index 3Eh
/// = FFh, then pin-assertion register = 17h), as the vendor code does on Sx.
fn mask_ioapic<H: FchHw>(hw: &H) {
    let m = hw.mmio();
    m.write32(IOAPIC, 0x3E);
    m.write32(IOAPIC + 0x10, 0xFF);
    m.write32(IOAPIC + 0x20, 0x17);
    (0x10..=0x3E).step_by(2).for_each(|index| {
        m.write32(IOAPIC, index);
        m.write32(IOAPIC + 0x10, 0x1_0000);
    });
}

/// A sleep state the FCH is armed for ([`arm`]).
pub struct Armed<'a, I> {
    control: Reg<&'a I, u16, Pm1Control::Register>,
}

impl<I: Window> Armed<'_, I> {
    /// Set SLP_EN: the FCH enters the armed state.
    pub fn enter(self) -> ! {
        self.control.modify(Pm1Control::SLP_EN::SET);
        loop {
            // SAFETY: halting is all that is left before the sleep.
            unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
        }
    }
}

/// Everything of the sleep entry for `slp_typ` (3..=5) but SLP_EN: `None`
/// for other types, which the OS does without SMM.
///
/// `smu` is the SMU of die 0 (Zeppelin, Naples) or socket 0; Sandstone has
/// none. `usb_dies` are the root buses whose USB controllers take the
/// Naples S3 handshake (every die with a running controller, die 0 first).
/// Once the SMU had its SleepEntry, answered or not, the state is armed:
/// the SMU completes the transition itself, and SLP_EN is the fallback.
pub fn arm<'a, H: FchHw, S: SmuMessages + ?Sized>(
    hw: &'a H,
    ports: &AcpiPorts,
    version: Version,
    slp_typ: u8,
    smu: Option<&S>,
    usb_dies: &[u8],
) -> Result<Option<Armed<'a, H::Io>>, SxError> {
    if !(3..=5).contains(&slp_typ) {
        return Ok(None);
    }
    let (acpi, io) = (hw.acpi(), hw.io());
    // SAFETY: SMM owns the CPU; nothing else runs. (Host models have no
    // caches to write back.)
    #[cfg(target_os = "none")]
    unsafe {
        fstart_arch::x86::writeback_invalidate_caches()
    };
    if version != Version::Sandstone {
        // The Sx callbacks.
        if slp_typ != 5 {
            update::<u8, _>(acpi, PM_EE.offset, 0, 0x03);
        }
        acpi.at(AOAC_94).modify(Aoac94::SX.val(3));
        mask_ioapic(hw);
    }
    match (version, smu) {
        (Version::Taishan, _) if slp_typ == 3 => usb_dies.iter().try_for_each(|&bus| {
            // SAFETY: SMM owns the SMN pair while it runs.
            let smn = unsafe { crate::smn::Smn::on_bus(hw.pci().clone(), bus) };
            let delay = &mut fstart_core::services::TimerDelay(hw.timer());
            crate::fch::taishan::usb::sx_handshake(&smn, delay)
                .then_some(())
                .ok_or(SxError::UsbHandshake)
        })?,
        (Version::Huashan | Version::Milan, Some(smu)) => {
            smu.send(v11::msg::USB_SX_ENTRY, &mut [0; 6])?;
            if slp_typ != 5 {
                let smn = hw.smn_window();
                smn.write32(0x13D0_004C, 0);
                smn.write32(0x13E0_004C, 0);
            }
        }
        _ => {}
    }
    smi::clear_all(acpi);
    // The FCH sleeps on the OS's SLP_TYP once SlpTypeEn is set and the
    // SLP_TYP SMI is off.
    acpi.at(PM_PCI_CONTROL)
        .modify(PciControl::FORCE_STPCLK_RETRY::SET + PciControl::FORCE_SLPSTATE_RETRY::CLEAR);
    smi::set_route(acpi, source::SLP_TYP, Route::Disabled);
    acpi.at(PM_RESET_CONTROL1)
        .modify(ResetControl1::SLP_TYPE_EN::SET);
    if version != Version::Sandstone {
        update::<u8, _>(acpi, PM_0B.offset, 0x03, 0x01);
    }
    let control = pm1_control(io, ports);
    control.modify(Pm1Control::SLP_TYP.val(slp_typ.into()) + Pm1Control::SLP_EN::CLEAR);
    // The SoC's last word to its SMU, which completes the transition; the
    // vendor code halts there. This waits 100 ms for it.
    let sleep_entry = match version {
        Version::Sandstone => None,
        Version::Taishan => Some(v9::msg::SLEEP_ENTRY),
        Version::Huashan | Version::Milan => Some(v11::msg::SLEEP_ENTRY),
    };
    if let (Some(smu), Some(message)) = (smu, sleep_entry) {
        if version == Version::Milan {
            let _ = smu.send(v11::msg::DXIO, &mut MILAN_DXIO_SX_ARGS.clone());
        }
        let _ = smu.send(message, &mut [0; 6]);
        fstart_core::services::Timer::delay_us(hw.timer(), 100_000);
    }
    Ok(Some(Armed { control }))
}

/// Enter sleep state `slp_typ` ([`arm`], then SLP_EN). Returns only for
/// S1/S2 or when the USB entry failed.
pub fn enter<H: FchHw, S: SmuMessages + ?Sized>(
    hw: &H,
    ports: &AcpiPorts,
    version: Version,
    slp_typ: u8,
    smu: Option<&S>,
    usb_dies: &[u8],
) -> Result<(), SxError> {
    if let Some(armed) = arm(hw, ports, version, slp_typ, smu, usb_dies)? {
        armed.enter();
    }
    Ok(())
}

/// Power button SMI (SmiStat1\[19\]) while the OS does not own ACPI: S5
/// through the sleep path.
pub fn power_button<H: FchHw, S: SmuMessages + ?Sized>(
    hw: &H,
    ports: &AcpiPorts,
    version: Version,
    smu: Option<&S>,
) -> Result<(), SxError> {
    let (acpi, io) = (hw.acpi(), hw.io());
    smi::clear(acpi, source::PWR_BTN);
    if version != Version::Sandstone {
        acpi.write32(MISC + 0x50, 0);
    }
    clear_pm_gpe_status(io, ports);
    enter(hw, ports, version, 5, smu, &[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::acpimmio::PM;
    use crate::fch::model::{Smu, fch};

    const PORTS: AcpiPorts = AcpiPorts {
        event: 0x800,
        control: 0x804,
        timer: 0x808,
        gpe: 0x820,
    };

    #[test]
    fn acpi_enable_routes_sci() {
        let hw = fch();
        acpi_mode(&hw, &PORTS, true);
        assert_eq!(hw.io.at(0xc01, 0x10), 9);
        assert_eq!(hw.io.at(0xc01, 0x90), 9);
        assert_eq!(hw.io.get(0x804, 2), 1);
        assert_eq!(hw.io.writes_to(0x824), vec![0]);
    }

    #[test]
    fn milan_s3_entry() {
        let hw = fch();
        let smu = Smu::default();
        assert!(
            arm(&hw, &PORTS, Version::Milan, 3, Some(&smu), &[])
                .unwrap()
                .is_some()
        );
        assert_eq!(smu.ids(), vec![0x14, 0x08, 0x0b]);
        assert_eq!(smu.0.borrow()[1].1[..3], [0x3a, 1, 5]);
        assert_eq!(hw.acpi.get(PM + 0xee, 1), 3);
        assert_eq!(hw.acpi.get(0x1e94, 4), 0xc000);
        assert_eq!(hw.acpi.get(PM + 0xbe, 1) & 0x20, 0x20);
        assert_eq!(hw.io.writes_to(0x804), vec![0xc00]);
        // 24 RTE writes + the flush.
        assert_eq!(hw.mmio.writes_to(IOAPIC + 0x10).len(), 25);
        assert!(crate::fch::model::smn_writes(&hw.pci, 0).contains(&(0x13e0_004c, 0)));
    }

    #[test]
    fn rome_s5_skips_ee_and_usb_lock() {
        let hw = fch();
        let smu = Smu::default();
        assert!(
            arm(&hw, &PORTS, Version::Huashan, 5, Some(&smu), &[])
                .unwrap()
                .is_some()
        );
        assert_eq!(smu.ids(), vec![0x14, 0x0b]);
        assert!(hw.acpi.writes_to(PM + 0xee).is_empty());
        assert!(crate::fch::model::smn_writes(&hw.pci, 0).is_empty());
        assert!(
            arm(&hw, &PORTS, Version::Huashan, 1, Some(&smu), &[])
                .unwrap()
                .is_none()
        );
        assert_eq!(smu.ids().len(), 2, "S1 does nothing");
    }

    #[test]
    fn sandstone_sleeps_without_callbacks_or_smu() {
        let hw = fch();
        assert!(
            arm::<_, Smu>(&hw, &PORTS, Version::Sandstone, 5, None, &[])
                .unwrap()
                .is_some()
        );
        assert!(hw.acpi.writes_to(PM + 0xee).is_empty());
        assert!(hw.mmio.writes().is_empty());
        assert_eq!(hw.io.writes_to(0x804), vec![0x1400]);
    }
}
