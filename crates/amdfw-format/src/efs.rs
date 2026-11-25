// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD #55758 EFS wire layout and directory selection. No silicon driver
//! or host packing dependencies; board layout is supplied by the caller.
use zerocopy::little_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};
pub const SIGNATURE: u32 = 0x55aa_55aa;
pub const WINDOW: u32 = 16 << 20;
pub const PROBES: [u32; 6] = [0xfa0000, 0xf20000, 0xe20000, 0xc20000, 0x820000, 0x20000];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum BiosDirectorySlot {
    Family17hModel00,
    Family17hModel10,
    Family17hModel30,
    Family19h,
}
pub type Pointer = U32;
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct Efs {
    pub signature: U32,
    pub imc: Pointer,
    pub gec: Pointer,
    pub xhci: Pointer,
    pub psp_directory: Pointer,
    pub new_psp_directory: Pointer,
    pub bios0: Pointer,
    pub bios1: Pointer,
    pub bios2: Pointer,
    pub generation: U32,
    pub bios3: Pointer,
    pub psp_backup_directory: Pointer,
    pub promontory: Pointer,
    pub lp_promontory: Pointer,
    pub promontory19: Pointer,
    pub reserved_3c: U32,
    pub spi_read_mode_f15_60: u8,
    pub spi_speed_f15_60: u8,
    pub reserved_42: u8,
    pub spi_read_mode: u8,
    pub spi_speed: u8,
    pub qpr_dummy_cycle: u8,
    pub reserved_46: u8,
    pub spi_read_mode_f17_30: u8,
    pub spi_speed_f17_30: u8,
    pub micron_detect_f17_30: u8,
    pub reserved_4a: [u8; 2],
    pub vendor_id: U16,
    pub board_id: U16,
    /// The eSPI controllers' configuration the PSP applies before x86
    /// runs (server SoCs): [`Espi`].
    pub espi: Espi,
    pub reserved_54: [u8; 4],
    /// Integrated firmware ROM layout selector (`+0x58`, Venice / Family 1Ah).
    /// Values `0..=3` map `EfsSpiOffset` (`image_origin`) to `BiosImageBase`
    /// (`bios_origin`); `0xff` (erased) is rejected by Venice firmware.
    pub rom_layout_selector: u8,
    pub reserved_59: [u8; 7],
}
const _: () = assert!(size_of::<Efs>() == 0x60);
const _: () = assert!(core::mem::offset_of!(Efs, rom_layout_selector) == 0x58);

/// The EFS's eSPI configuration bytes (+0x50), erased if unused.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned,
)]
#[repr(C)]
pub struct Espi {
    pub espi0_config0: u8,
    pub espi1_config0: u8,
    pub espi0_config1: u8,
    pub espi1_config1: u8,
}
const _: () = assert!(core::mem::offset_of!(Efs, espi) == 0x50);
impl Efs {
    pub fn erased() -> Self {
        let mut raw = Self::read_from_bytes(&[0xff; size_of::<Self>()]).unwrap();
        raw.signature.set(SIGNATURE);
        raw
    }
    pub fn bios_directory_mut(&mut self, slot: BiosDirectorySlot) -> &mut Pointer {
        match slot {
            BiosDirectorySlot::Family17hModel00 => &mut self.bios0,
            BiosDirectorySlot::Family17hModel10 => &mut self.bios1,
            BiosDirectorySlot::Family17hModel30 => &mut self.bios2,
            BiosDirectorySlot::Family19h => &mut self.bios3,
        }
    }
    pub fn bios_directory(&self, slot: BiosDirectorySlot) -> Option<u64> {
        let value = match slot {
            BiosDirectorySlot::Family17hModel00 => self.bios0,
            BiosDirectorySlot::Family17hModel10 => self.bios1,
            BiosDirectorySlot::Family17hModel30 => self.bios2,
            BiosDirectorySlot::Family19h => self.bios3,
        }
        .get();
        (!matches!(value, 0 | u32::MAX)).then_some(u64::from(value))
    }
    pub fn set_spi(&mut self, slot: BiosDirectorySlot, mode: u8, speed: u8) {
        match slot {
            BiosDirectorySlot::Family17hModel00 | BiosDirectorySlot::Family17hModel10 => {
                self.spi_read_mode = mode;
                self.spi_speed = speed;
            }
            BiosDirectorySlot::Family17hModel30 | BiosDirectorySlot::Family19h => {
                self.spi_read_mode_f17_30 = mode;
                self.spi_speed_f17_30 = speed;
            }
        }
    }

    /// Derive the Venice `BiosImageBase` (`bios_origin`) from EFS `+0x58`
    /// (`rom_layout_selector`) and the EFS bank offset (`image_origin`).
    ///
    /// Matches Venice `AmdDirectoryBaseLib` layout cases `0..=3`:
    /// - `0`: single-origin (`image_origin <= 0x0300_0000` -> `image_origin`)
    /// - `1`: dual-origin (`<= 0x0100_0000` -> `0`; `0x0200_0000..=0x0300_0000` -> `0x0200_0000`)
    /// - `2`: split (`0` -> `0`; `0x0300_0000` -> `0x0100_0000`)
    /// - `3`: BIOS-at-zero (`<= 0x0300_0000` -> `0`)
    pub const fn venice_bios_origin(&self, image_origin: u32) -> Option<u32> {
        match self.rom_layout_selector {
            0 if image_origin <= 0x0300_0000 => Some(image_origin),
            1 if image_origin <= 0x0100_0000 => Some(0),
            1 if image_origin >= 0x0200_0000 && image_origin <= 0x0300_0000 => Some(0x0200_0000),
            2 if image_origin == 0 => Some(0),
            2 if image_origin == 0x0300_0000 => Some(0x0100_0000),
            3 if image_origin <= 0x0300_0000 => Some(0),
            _ => None,
        }
    }

    /// Choose the Venice EFS `+0x58` selector byte encoding `(image_origin, bios_origin)`.
    pub const fn venice_layout_selector_for(image_origin: u32, bios_origin: u32) -> Option<u8> {
        if (image_origin == 0x0100_0000 && bios_origin == 0)
            || (matches!(image_origin, 0x0200_0000..=0x0300_0000) && bios_origin == 0x0200_0000)
        {
            Some(1)
        } else if image_origin <= 0x0300_0000 && bios_origin == image_origin {
            Some(0)
        } else if image_origin == 0x0300_0000 && bios_origin == 0x0100_0000 {
            Some(2)
        } else if image_origin <= 0x0300_0000 && bios_origin == 0 {
            Some(3)
        } else {
            None
        }
    }
}
