//! SMM chipset side: Sx entry, power button, ACPI enable/disable and the
//! USB SMI (FCH_SP3 §5). These are the steps an SMM handler calls; source
//! decoding and dispatch belong to the handler (see [`super::super::smi`]).

use tock_registers::interfaces::Writeable;

use super::super::acpimmio::{
    AOAC_94, AOAC_94_SX, GPE0_EN, GPE0_STS, MISC, PM, PM1_CNT, PM1_STS, pm,
};
use super::super::smi::{self, CONTROL0, source};
use super::super::{FchHw, Window, irq, update};
use crate::smu::{Error as SmuError, SmuMessages};
use crate::sp3::soc::Generation;

/// FCH IOAPIC (index/data/pin assertion at `FEC0_0000h` + 0/10h/20h).
pub const IOAPIC: u32 = 0xFEC0_0000;

/// SMU messages of the Sx path (NBIO_SMU §3.8; FCH_SP3 §5).
mod msg {
    /// V9 (Naples) SleepEntry, `smu::v9::msg::SLEEP_ENTRY`.
    pub const V9_SLEEP_ENTRY: u32 = 0x13;
    /// V11 (Rome, Milan) `smu::v11::msg::{DXIO, SLEEP_ENTRY, USB_SX_ENTRY}`.
    pub const V11_DXIO: u32 = 0x08;
    pub const V11_SLEEP_ENTRY: u32 = 0x0B;
    pub const V11_USB_SX_ENTRY: u32 = 0x14;
    /// Milan's DXIO request before SleepEntry: args `3Ah, 1, 5` (meaning
    /// not public, FCH_SP3 §10).
    pub const MILAN_DXIO_SX_ARGS: [u32; 6] = [0x3A, 1, 5, 0, 0, 0];
}

/// Why an Sx step failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SxError {
    Smu(SmuError),
    /// The Naples USB/SMU handshake did not complete (bounded poll).
    UsbHandshake,
    /// The generation's support is not built (an error, not a panic: SMM
    /// images may not link one).
    NotBuilt,
}

impl From<SmuError> for SxError {
    fn from(e: SmuError) -> Self {
        Self::Smu(e)
    }
}

/// SW SMI A0 (ACPI enable): power-button SMI controls off (the donor stores
/// SMIxAC byte 0), GPE0 cleared, SCI on IRQ 9, SCI_EN.
pub fn acpi_enable<H: FchHw>(hw: &H) {
    let (acpi, io) = (hw.acpi(), hw.io());
    acpi.write8(CONTROL0 + 0x0C, 0);
    io.write32(GPE0_EN, 0);
    io.write32(GPE0_STS, 0);
    irq::write(io, irq::SCI_PIC, 9);
    irq::write(io, irq::SCI_IOAPIC, 9);
    io.write32(PM1_CNT, 1);
}

/// SW SMI A1 (ACPI disable).
pub fn acpi_disable<H: FchHw>(hw: &H) {
    hw.io().write32(PM1_CNT, 0);
}

/// USB SMI (SmiStat1\[21\]): acknowledge in the IOHC of the USB
/// controllers. `buses` are the root buses of every die (Naples) or socket 0
/// (Rome/Milan, both controllers in the IOD).
pub fn usb_smi<H: FchHw>(hw: &H, generation: Generation, buses: &[u8]) {
    let acks: &[(u32, u32)] = match generation {
        Generation::Naples => &[(0x13B1_0120, 1), (0x13B2_0020, 0x100)],
        Generation::Rome | Generation::Milan => &[
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
/// = FFh, then pin-assertion register = 17h), as the donor does on Sx.
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

/// The common tail before the SMU: clear SLP_TYP status, SlpTypeControl,
/// PMx0B\[1:0\] = 1, only the SMI command port left routed, PM1_CNT.SLP_TYP.
fn arm_sleep<H: FchHw>(hw: &H, slp_typ: u8) {
    let acpi = hw.acpi();
    update::<u8, _>(acpi, PM + pm::RESET_CONTROL1, 0, 0x20);
    update::<u8, _>(acpi, PM + pm::PM_0B, 0x03, 0);
    update::<u8, _>(acpi, PM + pm::PM_0B, 0, 0x01);
    acpi.reg::<u32, ()>(CONTROL0 + 0x10).set(0x0040_0000);
    hw.io().write16(PM1_CNT, u16::from(slp_typ) << 10);
}

/// The final SMU message(s), then SLP_EN. The donor's trace ends in the SMU
/// message; setting SLP_EN afterwards is inferred (the Naples power-button
/// path, which has no SMU message, writes it, FCH_SP3 §5).
fn sleep<H: FchHw, S: SmuMessages + ?Sized>(
    hw: &H,
    generation: Generation,
    smu: &S,
    slp_typ: u8,
) -> Result<(), SxError> {
    match generation {
        Generation::Naples => smu.send(msg::V9_SLEEP_ENTRY, &mut [0; 6])?,
        Generation::Rome | Generation::Milan => {
            if generation == Generation::Milan {
                smu.send(msg::V11_DXIO, &mut msg::MILAN_DXIO_SX_ARGS.clone())?;
            }
            smu.send(msg::V11_SLEEP_ENTRY, &mut [0; 6])?;
        }
    }
    hw.io().write16(PM1_CNT, u16::from(slp_typ) << 10 | 1 << 13);
    Ok(())
}

/// The SLP_TYP SMI handler for S3, S4 and S5 (other types return at once).
///
/// `smu` is the SMU of die 0 (Naples) or socket 0. `usb_dies` are the root
/// buses whose USB controllers take the Naples S3 handshake (every die with
/// a running controller, die 0 first); Rome/Milan ignore it.
pub fn enter<H: FchHw, S: SmuMessages + ?Sized>(
    hw: &H,
    generation: Generation,
    slp_typ: u8,
    smu: &S,
    usb_dies: &[u8],
) -> Result<(), SxError> {
    if !(3..=5).contains(&slp_typ) {
        return Ok(());
    }
    let acpi = hw.acpi();
    let s3_or_s4 = slp_typ != 5;
    if s3_or_s4 {
        update::<u8, _>(acpi, PM + pm::PM_EE, 0, 0x03);
    }
    update::<u32, _>(acpi, AOAC_94, 0, AOAC_94_SX);
    mask_ioapic(hw);
    match generation {
        #[cfg(feature = "naples")]
        Generation::Naples => {
            if slp_typ == 3 {
                usb_dies.iter().try_for_each(|&bus| {
                    super::super::taishan::usb::sx_handshake(hw, bus)
                        .then_some(())
                        .ok_or(SxError::UsbHandshake)
                })?;
            }
        }
        Generation::Rome | Generation::Milan => {
            smu.send(msg::V11_USB_SX_ENTRY, &mut [0; 6])?;
            if s3_or_s4 {
                let smn = hw.smn_window();
                smn.write32(0x13D0_004C, 0);
                smn.write32(0x13E0_004C, 0);
            }
        }
        #[allow(unreachable_patterns)]
        _ => return Err(SxError::NotBuilt),
    }
    // Only the Naples arm uses the die list.
    let _ = usb_dies;
    smi::clear(acpi, source::SLP_TYP);
    arm_sleep(hw, slp_typ);
    sleep(hw, generation, smu, slp_typ)
}

/// Power button SMI (SmiStat1\[19\]): go to S5 (FCH_SP3 §5).
pub fn power_button<H: FchHw, S: SmuMessages + ?Sized>(
    hw: &H,
    generation: Generation,
    smu: &S,
) -> Result<(), SxError> {
    let acpi = hw.acpi();
    smi::clear(acpi, source::PWR_BTN);
    acpi.write32(MISC + 0x50, 0);
    update::<u8, _>(acpi, PM + pm::RESET_CONTROL1, 0, 0x20);
    update::<u8, _>(acpi, PM + pm::PM_0B, 0x03, 0x01);
    acpi.reg::<u32, ()>(CONTROL0 + 0x10).set(0x0040_0000);
    hw.io().write16(PM1_STS, 0x0100);
    hw.io().write16(PM1_CNT, 5 << 10);
    match generation {
        Generation::Naples => {
            hw.io().write16(PM1_CNT, 5 << 10 | 1 << 13);
            Ok(())
        }
        Generation::Rome | Generation::Milan => sleep(hw, generation, smu, 5),
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::model::{Smu, fch};
    use super::*;

    #[test]
    fn milan_s3_entry() {
        let hw = fch();
        let smu = Smu::default();
        enter(&hw, Generation::Milan, 3, &smu, &[]).unwrap();
        assert_eq!(smu.ids(), vec![0x14, 0x08, 0x0b]);
        assert_eq!(smu.0.borrow()[1].1[..3], [0x3a, 1, 5]);
        assert_eq!(hw.acpi.get(PM + 0xee, 1), 3);
        assert_eq!(hw.acpi.get(0x1e94, 4), 0xc000);
        assert_eq!(hw.acpi.writes_to(0x288), vec![2]);
        assert_eq!(hw.acpi.get(0x2b0, 4), 0x40_0000);
        assert_eq!(hw.io.writes_to(PM1_CNT), vec![0xc00, 0x2c00]);
        // 24 RTE writes + the flush.
        assert_eq!(hw.mmio.writes_to(IOAPIC + 0x10).len(), 25);
        assert!(super::super::super::model::smn_writes(&hw.pci, 0).contains(&(0x13e0_004c, 0)));
    }

    #[test]
    fn rome_s5_skips_ee_and_usb_lock() {
        let hw = fch();
        let smu = Smu::default();
        enter(&hw, Generation::Rome, 5, &smu, &[]).unwrap();
        assert_eq!(smu.ids(), vec![0x14, 0x0b]);
        assert!(hw.acpi.writes_to(PM + 0xee).is_empty());
        assert!(super::super::super::model::smn_writes(&hw.pci, 0).is_empty());
        enter(&hw, Generation::Rome, 1, &smu, &[]).unwrap();
        assert_eq!(smu.ids().len(), 2, "S1 does nothing");
    }

    #[test]
    fn acpi_enable_routes_sci() {
        let hw = fch();
        acpi_enable(&hw);
        assert_eq!(hw.io.at(0xc01, 0x10), 9);
        assert_eq!(hw.io.at(0xc01, 0x90), 9);
        assert_eq!(hw.io.get(PM1_CNT, 4), 1);
    }
}
