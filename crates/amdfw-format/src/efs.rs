// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD #55758 EFS wire layout and directory selection. No silicon driver
//! or host packing dependencies; board layout is supplied by the caller.
use zerocopy::little_endian::U32;
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
    pub rest: [u8; 0x16],
}
const _: () = assert!(size_of::<Efs>() == 0x60);
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
}
