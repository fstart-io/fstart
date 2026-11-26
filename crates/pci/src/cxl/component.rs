//! The CXL component register block (CXL 3.1 §8.2.3, §8.2.4): 64 KiB, the
//! CXL.io registers in its first 4 KiB and the CXL.cache/mem registers from
//! [`CACHE_MEM`], which open with a capability array: one header of the
//! array, then one header per capability pointing into the range.
//!
//! The walk reads dwords through a closure so the same code serves an MMIO
//! block, a host's emulated copy in DRAM and a test buffer.

use tock_registers::{LocalRegisterCopy, register_bitfields};

/// Size of a component register block.
pub const BLOCK_SIZE: u64 = 0x1_0000;
/// Offset of the CXL.cache/mem primary range in the block.
pub const CACHE_MEM: u32 = 0x1000;
/// Size of the CXL.cache/mem primary range.
pub const CACHE_MEM_SIZE: u32 = 0x1000;

register_bitfields![u32,
    /// CXL.cache/mem +0x00 (32-bit), CXL_Capability_Header: the array's.
    pub ArrayHeader [
        /// Always [`id::CAPABILITY`].
        ID OFFSET(0) NUMBITS(16) [],
        VERSION OFFSET(16) NUMBITS(4) [],
        CACHE_MEM_VERSION OFFSET(20) NUMBITS(4) [],
        /// Capability headers after this one.
        ARRAY_SIZE OFFSET(24) NUMBITS(8) []
    ],
    /// CXL.cache/mem +0x04 + 4n (32-bit): the header of capability n.
    pub CapabilityHeader [
        ID OFFSET(0) NUMBITS(16) [],
        VERSION OFFSET(16) NUMBITS(4) [],
        /// Offset of the capability from the CXL.cache/mem range's start.
        POINTER OFFSET(20) NUMBITS(12) []
    ]
];

/// CXL.cache/mem capability IDs (CXL 3.1 Table 8-22).
pub mod id {
    /// The array's own header.
    pub const CAPABILITY: u16 = 1;
    pub const RAS: u16 = 2;
    pub const SECURITY: u16 = 3;
    pub const LINK: u16 = 4;
    pub const HDM_DECODER: u16 = 5;
    pub const EXTENDED_SECURITY: u16 = 6;
    pub const IDE: u16 = 7;
    pub const SNOOP_FILTER: u16 = 8;
    pub const TIMEOUT_ISOLATION: u16 = 9;
    pub const CACHE_MEM_EXTENDED: u16 = 0xa;
    pub const BI_ROUTE_TABLE: u16 = 0xb;
    pub const BI_DECODER: u16 = 0xc;
    pub const CACHE_ID_ROUTE_TABLE: u16 = 0xd;
    pub const CACHE_ID_DECODER: u16 = 0xe;
    pub const EXTENDED_HDM_DECODER: u16 = 0xf;
}

/// The array header word of an array of `count` capabilities.
pub fn array_header(count: u8) -> u32 {
    let mut header = LocalRegisterCopy::<u32, ArrayHeader::Register>::new(0);
    header.write(
        ArrayHeader::ID.val(id::CAPABILITY.into())
            + ArrayHeader::VERSION.val(1)
            + ArrayHeader::CACHE_MEM_VERSION.val(1)
            + ArrayHeader::ARRAY_SIZE.val(count.into()),
    );
    header.get()
}

/// The header word of capability `id`, version `version`, at `pointer`
/// from the CXL.cache/mem range's start.
pub fn capability_header(id: u16, version: u8, pointer: u16) -> u32 {
    let mut header = LocalRegisterCopy::<u32, CapabilityHeader::Register>::new(0);
    header.write(
        CapabilityHeader::ID.val(id.into())
            + CapabilityHeader::VERSION.val(version.into())
            + CapabilityHeader::POINTER.val(pointer.into()),
    );
    header.get()
}

/// Find capability `id` in the CXL.cache/mem range whose dwords `read`
/// returns (by byte offset from the range's start): its offset there and
/// its version. `None` without a valid array or without the capability.
pub fn find(read: impl Fn(u32) -> u32, id: u16) -> Option<(u32, u8)> {
    let array = LocalRegisterCopy::<u32, ArrayHeader::Register>::new(read(0));
    if array.read(ArrayHeader::ID) != u32::from(id::CAPABILITY) {
        return None;
    }
    (1..=array.read(ArrayHeader::ARRAY_SIZE)).find_map(|n| {
        let header = LocalRegisterCopy::<u32, CapabilityHeader::Register>::new(read(4 * n));
        let pointer = header.read(CapabilityHeader::POINTER);
        (header.read(CapabilityHeader::ID) == u32::from(id)
            && pointer != 0
            && pointer < CACHE_MEM_SIZE)
            .then(|| (pointer, header.read(CapabilityHeader::VERSION) as u8))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_array_round_trips() {
        let mut range = [0u32; 0x400];
        range[0] = array_header(2);
        range[1] = capability_header(id::RAS, 2, 0x40);
        range[2] = capability_header(id::HDM_DECODER, 1, 0x200);
        assert_eq!(range[0], 0x0211_0001);
        let read = |offset: u32| range[offset as usize / 4];
        assert_eq!(find(read, id::HDM_DECODER), Some((0x200, 1)));
        assert_eq!(find(read, id::RAS), Some((0x40, 2)));
        assert_eq!(find(read, id::LINK), None);
        // The AMD emulated host bridge's array: one HDM decoder capability
        // at 0x008 (CxlV2Manager `CxlPopulateComponentRegisters`).
        assert_eq!(array_header(1), 0x0111_0001);
        assert_eq!(capability_header(id::HDM_DECODER, 1, 8), 0x0081_0005);
        // Not an array.
        assert_eq!(find(|_| 0, id::HDM_DECODER), None);
    }
}
