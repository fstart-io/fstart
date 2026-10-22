// SPDX-License-Identifier: MIT OR Apache-2.0
use super::Key;
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::{LittleEndian, U32, U64},
};

pub const PSP_COOKIE: u32 = 0x50535024;
pub const BIOS_COOKIE: u32 = 0x44484224;

#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct Header {
    pub cookie: U32<LittleEndian>,
    pub checksum: U32<LittleEndian>,
    pub count: U32<LittleEndian>,
    pub additional: U32<LittleEndian>,
}
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct PspEntry {
    pub kind: u8,
    pub subprogram: u8,
    pub reserved: [u8; 2],
    pub size: U32<LittleEndian>,
    pub address: U64<LittleEndian>,
}
impl PspEntry {
    pub fn new(key: Key, size: u32, address: u64) -> Self {
        Self {
            kind: key.kind,
            subprogram: key.subprogram,
            reserved: [key.instance << 3, 0],
            size: size.into(),
            address: address.into(),
        }
    }
}
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct BiosEntry {
    pub kind: u8,
    pub region: u8,
    pub flags_instance: u8,
    pub subprogram_rom: u8,
    pub size: U32<LittleEndian>,
    pub source: U64<LittleEndian>,
    pub destination: U64<LittleEndian>,
}
impl BiosEntry {
    pub fn new(key: Key, flags: u8, size: u32, source: u64, destination: u64) -> Self {
        Self {
            kind: key.kind,
            region: 0,
            flags_instance: flags | (key.instance << 4),
            subprogram_rom: key.subprogram,
            size: size.into(),
            source: source.into(),
            destination: destination.into(),
        }
    }
}
const _: () = {
    assert!(core::mem::size_of::<Header>() == 16);
    assert!(core::mem::size_of::<PspEntry>() == 16);
    assert!(core::mem::size_of::<BiosEntry>() == 24);
};
