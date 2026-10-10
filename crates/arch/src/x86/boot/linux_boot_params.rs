//! The 4096-byte Linux x86 boot_params (zero-page) wire layout.
//! Offsets follow Linux `arch/x86/include/uapi/asm/bootparam.h`.

use fstart_core::services::memory_detect::{E820Entry, E820Kind};
use zerocopy::byteorder::{LE, U16, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

pub(super) const E820_CAPACITY: usize = 128;

// The firmware e820 map must always fit the zero page.
const _: () = assert!(fstart_core::services::memory_detect::MAX_E820_ENTRIES <= E820_CAPACITY);

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
    _before_header: [u8; 0x202 - 0x1fc],
    /// `"HdrS"` when the image carries a Linux boot protocol header.
    pub header: U32<LE>,
    /// Boot protocol version, `major << 8 | minor`.
    pub version: U16<LE>,
    _before_loader: [u8; 0x210 - 0x208],
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
    pub relocatable_kernel: u8,
    _min_alignment: u8,
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
    assert!(core::mem::offset_of!(LinuxBootParams, header) == 0x202);
    assert!(core::mem::offset_of!(LinuxBootParams, version) == 0x206);
    assert!(core::mem::offset_of!(LinuxBootParams, type_of_loader) == 0x210);
    assert!(core::mem::offset_of!(LinuxBootParams, loadflags) == 0x211);
    assert!(core::mem::offset_of!(LinuxBootParams, code32_start) == 0x214);
    assert!(core::mem::offset_of!(LinuxBootParams, heap_end_ptr) == 0x224);
    assert!(core::mem::offset_of!(LinuxBootParams, cmd_line_ptr) == 0x228);
    assert!(core::mem::offset_of!(LinuxBootParams, kernel_alignment) == 0x230);
    assert!(core::mem::offset_of!(LinuxBootParams, relocatable_kernel) == 0x234);
    assert!(core::mem::offset_of!(LinuxBootParams, xloadflags) == 0x236);
    assert!(core::mem::offset_of!(LinuxBootParams, pref_address) == 0x258);
    assert!(core::mem::offset_of!(LinuxBootParams, init_size) == 0x260);
    assert!(core::mem::offset_of!(LinuxBootParams, e820_table) == 0x2d0);
    assert!(core::mem::size_of::<LinuxE820Entry>() == 20);
};

const KERNEL_LIMIT: u64 = 1 << 32;

/// Where to put the protected-mode kernel: the lowest `align`-aligned base
/// at or above `pref` whose whole `[base, base + init_size)` footprint is e820
/// RAM below 4 GiB. A kernel that is not relocatable only runs at `pref`.
///
/// The footprint must avoid firmware reservations: memory the firmware keeps
/// (for example the stage windows it reloads on S3 resume) is not the
/// kernel's to run from, even when it is the kernel's preferred address.
/// Nothing below `pref` qualifies: a relocatable kernel loaded there moves
/// itself up to `pref` before decompressing. The 4 GiB limit is the reach of
/// `code32_start` and of the firmware's identity map.
pub(super) fn place_kernel(
    e820: &[E820Entry],
    pref: u64,
    align: u64,
    init_size: u64,
    relocatable: bool,
) -> Option<u64> {
    let align = align.max(1);
    e820.iter()
        .filter(|entry| ({ entry.kind }) == E820Kind::Ram as u32)
        .filter_map(|entry| {
            let start = if relocatable {
                entry.addr.max(pref).checked_next_multiple_of(align)?
            } else {
                pref
            };
            let end = start.checked_add(init_size)?;
            (start >= entry.addr
                && end <= entry.addr.checked_add(entry.size)?
                && end <= KERNEL_LIMIT)
                .then_some(start)
        })
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_avoids_reserved_preferred_address() {
        let e820 = [
            E820Entry::new(0x10_0000, 0xf0_0000, E820Kind::Ram),
            E820Entry::new(0x100_0000, 0x2_0000, E820Kind::Reserved),
            E820Entry::new(0x102_0000, 0x2fe_0000, E820Kind::Ram),
            E820Entry::new(0x503_b000, 0x7000_0000, E820Kind::Ram),
        ];
        let place =
            |size, relocatable| place_kernel(&e820, 0x100_0000, 0x20_0000, size, relocatable);
        assert_eq!(place(0x13a_d000, true), Some(0x120_0000));
        // Too big for the gap below 64 MiB: next RAM range, aligned.
        assert_eq!(place(0x3000_0000, true), Some(0x520_0000));
        assert_eq!(place(0x13a_d000, false), None);
        // RAM above 4 GiB is out of reach.
        let high = [E820Entry::new(0x1_0000_0000, 0x1_0000_0000, E820Kind::Ram)];
        assert_eq!(
            place_kernel(&high, 0x100_0000, 0x20_0000, 0x100_0000, true),
            None
        );
    }

    #[test]
    fn zero_page_fields_encode_at_protocol_offsets() {
        let mut bytes = [0u8; 4096];
        let params = LinuxBootParams::mut_from_bytes(&mut bytes[..]).unwrap();
        params.acpi_rsdp_addr.set(0x1122_3344_5566_7788);
        params.e820_count = 1;
        params.header.set(u32::from_le_bytes(*b"HdrS"));
        params.version.set(0x020f);
        params.e820_table[0] = LinuxE820Entry {
            addr: U64::new(0x1234),
            size: U64::new(0x1000),
            kind: U32::new(1),
        };
        assert_eq!(&bytes[0x70..0x78], &0x1122_3344_5566_7788u64.to_le_bytes());
        assert_eq!(bytes[0x1e8], 1);
        assert_eq!(&bytes[0x202..0x206], b"HdrS");
        assert_eq!(&bytes[0x206..0x208], &0x020fu16.to_le_bytes());
        assert_eq!(&bytes[0x2d0..0x2d8], &0x1234u64.to_le_bytes());
    }
}
