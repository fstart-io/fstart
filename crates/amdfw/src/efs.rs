// SPDX-License-Identifier: MIT OR Apache-2.0
//! The embedded firmware structure (EFS): the
//! signature the PSP searches for, with the PSP and BIOS directories of each
//! SoC generation and its SPI settings.
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::{LittleEndian, U32},
};

pub const SIGNATURE: u32 = 0x55aa_55aa;

/// The offsets the PSP probes for the EFS in the top 16 MiB of the flash
/// (`0xfffa0000`, ...), in its search order.
pub const PROBES: [u32; 6] = [
    0xfa_0000, 0xf2_0000, 0xe2_0000, 0xc2_0000, 0x82_0000, 0x02_0000,
];
/// The flash window the probe offsets are in.
pub const WINDOW: u32 = 0x100_0000;

/// The EFS field a SoC's PSP takes its BIOS directory from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum BiosDirectorySlot {
    /// Family 17h models 00h-0Fh (Zeppelin): `bios0_entry`.
    Family17hModel00,
    /// Family 17h models 10h-2Fh (Raven, Picasso): `bios1_entry`.
    Family17hModel10,
}

/// An address field of the EFS; 0 and `!0` are none.
pub type Pointer = U32<LittleEndian>;

#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct Efs {
    pub signature: U32<LittleEndian>,
    pub imc: Pointer,
    pub gec: Pointer,
    pub xhci: Pointer,
    pub psp_directory: Pointer,
    /// The PSP directory of Family 17h on.
    pub new_psp_directory: Pointer,
    pub bios0: Pointer,
    pub bios1: Pointer,
    pub bios2: Pointer,
    pub generation: U32<LittleEndian>,
    pub bios3: Pointer,
    pub psp_backup_directory: Pointer,
    pub promontory: Pointer,
    pub lp_promontory: Pointer,
    pub promontory19: Pointer,
    pub reserved_3c: U32<LittleEndian>,
    pub spi_read_mode_f15_60: u8,
    pub spi_speed_f15_60: u8,
    pub reserved_42: u8,
    /// SPI settings of Family 17h models 00h-2Fh.
    pub spi_read_mode: u8,
    pub spi_speed: u8,
    pub qpr_dummy_cycle: u8,
    pub reserved_46: u8,
    pub spi_read_mode_f17_30: u8,
    pub spi_speed_f17_30: u8,
    pub micron_detect_f17_30: u8,
    /// Board IDs, eSPI configuration and the BIOS size of later SoCs.
    pub rest: [u8; 0x16],
}
const _: () = assert!(core::mem::size_of::<Efs>() == 0x60);

impl Efs {
    /// An erased EFS with its signature.
    pub fn erased() -> Self {
        let mut efs = Self::read_from_bytes(&[0xff; core::mem::size_of::<Self>()]).unwrap();
        efs.signature.set(SIGNATURE);
        efs
    }

    /// The BIOS directory field of `slot`.
    pub fn bios_directory_mut(&mut self, slot: BiosDirectorySlot) -> &mut Pointer {
        match slot {
            BiosDirectorySlot::Family17hModel00 => &mut self.bios0,
            BiosDirectorySlot::Family17hModel10 => &mut self.bios1,
        }
    }

    /// The address of the BIOS directory the PSP of `slot` boots from.
    pub fn bios_directory(&self, slot: BiosDirectorySlot) -> Option<u64> {
        let pointer = match slot {
            BiosDirectorySlot::Family17hModel00 => self.bios0,
            BiosDirectorySlot::Family17hModel10 => self.bios1,
        };
        Some(pointer.get())
            .filter(|&p| p != 0 && p != u32::MAX)
            .map(u64::from)
    }
}
