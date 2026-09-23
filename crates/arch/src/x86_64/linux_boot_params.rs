//! The 4096-byte Linux x86 boot_params (zero-page) wire layout.
//! Offsets follow Linux `arch/x86/include/uapi/asm/bootparam.h`.

use zerocopy::byteorder::{LE, U16, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

pub(super) const E820_CAPACITY: usize = 128;

#[repr(C)]
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout)]
pub(super) struct LinuxE820Entry {
    pub addr: U64<LE>,
    pub size: U64<LE>,
    pub kind: U32<LE>,
}

#[repr(C)]
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout)]
pub(super) struct LinuxBootParams {
    _before_acpi: [u8; 0x070],
    pub acpi_rsdp_addr: U64<LE>,
    _before_e820_count: [u8; 0x1e8 - 0x078],
    pub e820_count: u8,
    _before_setup_sects: [u8; 0x1f1 - 0x1e9],
    pub setup_sects: u8,
    _before_syssize: [u8; 0x1f4 - 0x1f2],
    pub syssize: U32<LE>,
    _before_vid_mode: [u8; 0x1fa - 0x1f8],
    pub vid_mode: U16<LE>,
    _before_loader: [u8; 0x210 - 0x1fc],
    pub type_of_loader: u8,
    pub loadflags: u8,
    _before_code32_start: [u8; 0x214 - 0x212],
    pub code32_start: U32<LE>,
    _before_heap_end: [u8; 0x224 - 0x218],
    pub heap_end_ptr: U16<LE>,
    _before_cmd_line: [u8; 0x228 - 0x226],
    pub cmd_line_ptr: U32<LE>,
    _before_kernel_alignment: [u8; 0x230 - 0x22c],
    pub kernel_alignment: U32<LE>,
    _before_xloadflags: [u8; 0x236 - 0x234],
    pub xloadflags: U16<LE>,
    _before_pref_address: [u8; 0x258 - 0x238],
    pub pref_address: U64<LE>,
    pub init_size: U32<LE>,
    _before_e820_table: [u8; 0x2d0 - 0x264],
    pub e820_table: [LinuxE820Entry; E820_CAPACITY],
    _remaining: [u8; 4096 - (0x2d0 + E820_CAPACITY * 20)],
}

const _: () = {
    assert!(core::mem::size_of::<LinuxBootParams>() == 4096);
    assert!(core::mem::offset_of!(LinuxBootParams, acpi_rsdp_addr) == 0x070);
    assert!(core::mem::offset_of!(LinuxBootParams, e820_count) == 0x1e8);
    assert!(core::mem::offset_of!(LinuxBootParams, setup_sects) == 0x1f1);
    assert!(core::mem::offset_of!(LinuxBootParams, syssize) == 0x1f4);
    assert!(core::mem::offset_of!(LinuxBootParams, vid_mode) == 0x1fa);
    assert!(core::mem::offset_of!(LinuxBootParams, type_of_loader) == 0x210);
    assert!(core::mem::offset_of!(LinuxBootParams, loadflags) == 0x211);
    assert!(core::mem::offset_of!(LinuxBootParams, code32_start) == 0x214);
    assert!(core::mem::offset_of!(LinuxBootParams, heap_end_ptr) == 0x224);
    assert!(core::mem::offset_of!(LinuxBootParams, cmd_line_ptr) == 0x228);
    assert!(core::mem::offset_of!(LinuxBootParams, kernel_alignment) == 0x230);
    assert!(core::mem::offset_of!(LinuxBootParams, xloadflags) == 0x236);
    assert!(core::mem::offset_of!(LinuxBootParams, pref_address) == 0x258);
    assert!(core::mem::offset_of!(LinuxBootParams, init_size) == 0x260);
    assert!(core::mem::offset_of!(LinuxBootParams, e820_table) == 0x2d0);
    assert!(core::mem::size_of::<LinuxE820Entry>() == 20);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_page_fields_encode_at_protocol_offsets() {
        let mut bytes = [0u8; 4096];
        let params = LinuxBootParams::mut_from_bytes(&mut bytes[..]).unwrap();
        params.acpi_rsdp_addr.set(0x1122_3344_5566_7788);
        params.e820_count = 1;
        params.e820_table[0] = LinuxE820Entry {
            addr: U64::new(0x1234),
            size: U64::new(0x1000),
            kind: U32::new(1),
        };
        assert_eq!(&bytes[0x70..0x78], &0x1122_3344_5566_7788u64.to_le_bytes());
        assert_eq!(bytes[0x1e8], 1);
        assert_eq!(&bytes[0x2d0..0x2d8], &0x1234u64.to_le_bytes());
    }
}
