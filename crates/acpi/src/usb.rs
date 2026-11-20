//! USB port descriptions (ACPI 6.5 §9.14 `_UPC`, §6.1.8 `_PLD`), as
//! coreboot's `drivers/usb/acpi` emits them for board-described ports.

use alloc::string::String;
use alloc::vec::Vec;

use acpi_tables::Aml;
use acpi_tables::aml::{BufferData, Name, Package};

/// `_UPC` connector type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum UpcType {
    A = 0x00,
    MiniAb = 0x01,
    ExpressCard = 0x02,
    Usb3A = 0x03,
    Usb3B = 0x04,
    Usb3MicroB = 0x05,
    Usb3MicroAb = 0x06,
    Usb3PowerB = 0x07,
    CUsb2Only = 0x08,
    CUsb2SsSwitch = 0x09,
    CUsb2Ss = 0x0a,
    /// Proprietary connector, e.g. a soldered-down device.
    Internal = 0xff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PldPanel {
    Top = 0,
    Bottom = 1,
    Left = 2,
    Right = 3,
    Front = 4,
    Back = 5,
    Unknown = 6,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PldVertical {
    Upper = 0,
    Center = 1,
    Lower = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PldHorizontal {
    Left = 0,
    Center = 1,
    Right = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PldShape {
    Round = 0,
    Oval = 1,
    Square = 2,
    VerticalRectangle = 3,
    HorizontalRectangle = 4,
    VerticalTrapezoid = 5,
    HorizontalTrapezoid = 6,
    Unknown = 7,
    Chamfered = 8,
}

/// The `_PLD` fields boards set (coreboot `struct acpi_pld`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pld {
    pub ignore_color: bool,
    pub visible: bool,
    pub panel: PldPanel,
    pub vertical: PldVertical,
    pub horizontal: PldHorizontal,
    pub shape: PldShape,
    pub group_token: u8,
    pub group_position: u8,
}

impl Pld {
    /// coreboot `ACPI_PLD_TYPE_A(panel, horizontal, ACPI_PLD_GROUP(token,
    /// position))`.
    #[must_use]
    pub const fn type_a(panel: PldPanel, horizontal: PldHorizontal, group: (u8, u8)) -> Self {
        Self {
            ignore_color: false,
            visible: true,
            panel,
            vertical: PldVertical::Upper,
            horizontal,
            shape: PldShape::HorizontalRectangle,
            group_token: group.0,
            group_position: group.1,
        }
    }

    /// coreboot `acpi_pld_fill_usb`: the default description of a port type.
    #[must_use]
    pub const fn for_upc(upc: UpcType, group: (u8, u8)) -> Self {
        let (shape, visible) = match upc {
            UpcType::A | UpcType::Usb3A | UpcType::Usb3PowerB => {
                (PldShape::HorizontalRectangle, true)
            }
            UpcType::MiniAb | UpcType::Usb3B => (PldShape::Chamfered, true),
            UpcType::Usb3MicroB | UpcType::Usb3MicroAb => (PldShape::HorizontalTrapezoid, true),
            UpcType::CUsb2Only | UpcType::CUsb2SsSwitch | UpcType::CUsb2Ss => {
                (PldShape::Oval, true)
            }
            UpcType::ExpressCard | UpcType::Internal => (PldShape::Unknown, false),
        };
        Self {
            ignore_color: true,
            visible,
            panel: PldPanel::Unknown,
            vertical: PldVertical::Center,
            horizontal: PldHorizontal::Center,
            shape,
            group_token: group.0,
            group_position: group.1,
        }
    }

    /// The revision 2, 20-byte buffer (coreboot `acpi_pld_to_buffer`).
    #[must_use]
    pub const fn to_buffer(&self) -> [u8; 20] {
        let mut buf = [0u8; 20];
        buf[0] = 0x2 | if self.ignore_color { 0x80 } else { 0 };
        buf[8] = self.visible as u8 | (self.panel as u8 & 7) << 3 | (self.vertical as u8 & 3) << 6;
        buf[9] = (self.horizontal as u8 & 3)
            | (self.shape as u8 & 0xf) << 2
            | (self.group_token & 1) << 7;
        buf[10] = (self.group_token >> 1) & 0x7f | (self.group_position & 1) << 7;
        buf[11] = (self.group_position >> 1) & 0x7f;
        buf
    }
}

/// `_DDN`, `_UPC` and `_PLD` for one port, for the caller's port scope
/// (coreboot `usb_acpi_fill_ssdt_generator`).
#[must_use]
pub fn port_aml(description: Option<&str>, upc: UpcType, pld: &Pld) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(description) = description {
        Name::new("_DDN".into(), &String::from(description)).to_aml_bytes(&mut out);
    }
    let connectable: u8 = 0xff;
    let kind = upc as u8;
    Name::new(
        "_UPC".into(),
        &Package::new(alloc::vec![&connectable, &kind, &0u8, &0u8]),
    )
    .to_aml_bytes(&mut out);
    let buffer = BufferData::new(pld.to_buffer().to_vec());
    Name::new("_PLD".into(), &Package::new(alloc::vec![&buffer])).to_aml_bytes(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_a_matches_coreboot_encoding() {
        // ACPI_PLD_TYPE_A(LEFT, RIGHT, ACPI_PLD_GROUP(1, 0)).
        let buf = Pld::type_a(PldPanel::Left, PldHorizontal::Right, (1, 0)).to_buffer();
        assert_eq!(buf[0], 0x02);
        assert_eq!(buf[8], 0x01 | 2 << 3);
        assert_eq!(buf[9], 2 | 4 << 2 | 1 << 7);
        assert_eq!(buf[10], 0);
    }

    #[test]
    fn internal_ports_are_hidden() {
        let pld = Pld::for_upc(UpcType::Internal, (0, 0));
        let buf = pld.to_buffer();
        assert_eq!(buf[0], 0x82);
        assert_eq!(buf[8] & 1, 0);
        let aml = port_aml(Some("BDC"), UpcType::Internal, &pld);
        assert!(aml.windows(4).any(|w| w == b"_PLD"));
        assert!(aml.windows(4).any(|w| w == b"_DDN"));
    }
}
