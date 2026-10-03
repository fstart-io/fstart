//! Resident fatal AP exception tables, separate from the immutable boot image.
//!
//! All vectors terminate without Rust, logging, unwinding or returning. These
//! tables use the AP's current private stack (IST=0), not a claimed emergency
//! stack/TSS implementation. Invalid/unmapped/exhausted stacks remain a platform
//! gate. NMI/SMI exclusion is required before private-stack/IDT installation and
//! across the mode-switch window; installing an IDT does not establish it.
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout,
    byteorder::{LittleEndian, U16, U32, U64},
};

#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable)]
struct Gate32 {
    low: U16<LittleEndian>,
    selector: U16<LittleEndian>,
    reserved: u8,
    attributes: u8,
    high: U16<LittleEndian>,
}
impl Gate32 {
    const ZERO: Self = Self {
        low: U16::ZERO,
        selector: U16::ZERO,
        reserved: 0,
        attributes: 0,
        high: U16::ZERO,
    };
    fn fatal(handler: u32) -> Self {
        Self {
            low: U16::new(handler as u16),
            selector: U16::new(0x08), // Trampoline flat protected-mode code.
            reserved: 0,
            attributes: 0x8e, // Present, DPL0, 32-bit interrupt gate.
            high: U16::new((handler >> 16) as u16),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable)]
struct Gate64 {
    low: U16<LittleEndian>,
    selector: U16<LittleEndian>,
    ist: u8,
    attributes: u8,
    middle: U16<LittleEndian>,
    high: U32<LittleEndian>,
    reserved: U32<LittleEndian>,
}
impl Gate64 {
    const ZERO: Self = Self {
        low: U16::ZERO,
        selector: U16::ZERO,
        ist: 0,
        attributes: 0,
        middle: U16::ZERO,
        high: U32::ZERO,
        reserved: U32::ZERO,
    };
    fn fatal(handler: u64) -> Self {
        Self {
            low: U16::new(handler as u16),
            selector: U16::new(0x18), // Trampoline long-mode code, not BSP CS.
            ist: 0,
            attributes: 0x8e,
            middle: U16::new((handler >> 16) as u16),
            high: U32::new((handler >> 32) as u32),
            reserved: U32::ZERO,
        }
    }
}

/// Long-mode LIDT consumes ten bytes; protected-mode LIDT consumes the first
/// six. Both bases here are validated below 4 GiB before any table writes.
#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable)]
pub(super) struct Pointer {
    limit: U16<LittleEndian>,
    base: U64<LittleEndian>,
}

#[repr(C, align(16))]
pub(super) struct Tables {
    protected: [Gate32; 256],
    long: [Gate64; 256],
}
impl Tables {
    pub(super) const ZERO: Self = Self {
        protected: [Gate32::ZERO; 256],
        long: [Gate64::ZERO; 256],
    };
}
const _: () = {
    assert!(core::mem::size_of::<Gate32>() == 8);
    assert!(core::mem::size_of::<Gate64>() == 16);
    assert!(core::mem::size_of::<Pointer>() == 10);
    assert!(core::mem::size_of::<Tables>() == 6144);
};

fn pointers(base: u64, handler32: u64, handler64: u64) -> Option<(Pointer, Pointer)> {
    let end = base.checked_add(core::mem::size_of::<Tables>() as u64)?;
    if base == 0
        || base & 15 != 0
        || end > (1u64 << 32)
        || handler32 == 0
        || handler32 > u64::from(u32::MAX)
        || handler64 == 0
        || handler64 > u64::from(u32::MAX)
    {
        return None;
    }
    Some((
        Pointer {
            limit: U16::new(2047),
            base: U64::new(base),
        },
        Pointer {
            limit: U16::new(4095),
            base: U64::new(base + 2048),
        },
    ))
}

/// Initialize all 256 vectors in both modes before publication/release.
/// # Safety
/// `tables` is exclusive, aligned, writable resident identity-mapped DRAM and
/// remains reserved until reset, including error paths. No AP can currently
/// read it. Handlers are resident mapped trampoline instructions. Do not reuse
/// these tables by resetting a live MP instance. Geometry is not mapping proof.
pub(super) unsafe fn prepare(
    tables: *mut Tables,
    handler32: u64,
    handler64: u64,
) -> Option<(Pointer, Pointer)> {
    let pointers = pointers(tables as u64, handler32, handler64)?;
    let protected = unsafe { core::ptr::addr_of_mut!((*tables).protected).cast::<Gate32>() };
    let long = unsafe { core::ptr::addr_of_mut!((*tables).long).cast::<Gate64>() };
    for vector in 0..256 {
        unsafe {
            core::ptr::write(protected.add(vector), Gate32::fatal(handler32 as u32));
            core::ptr::write(long.add(vector), Gate64::fatal(handler64));
        }
    }
    // APs read DRAM with caches disabled. Publish the entire tables, not merely
    // their descriptors inside the copied trampoline.
    unsafe {
        crate::x86::writeback_cache_range(tables.cast::<u8>(), core::mem::size_of::<Tables>())
    };
    Some(pointers)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gates_encode_full_offsets_mode_selectors_and_reserved_bits() {
        assert_eq!(
            Gate32::fatal(0x12345678).as_bytes(),
            &[0x78, 0x56, 8, 0, 0, 0x8e, 0x34, 0x12]
        );
        assert_eq!(
            Gate64::fatal(0x123456789abcdef0).as_bytes(),
            &[
                0xf0, 0xde, 0x18, 0, 0, 0x8e, 0xbc, 0x9a, 0x78, 0x56, 0x34, 0x12, 0, 0, 0, 0
            ]
        );
        let (protected, long) = pointers(0x200000, 0x8100, 0x8200).unwrap();
        assert_eq!(protected.limit.get(), 2047);
        assert_eq!(protected.base.get(), 0x200000);
        assert_eq!(long.limit.get(), 4095);
        assert_eq!(long.base.get(), 0x200800);
        assert_eq!(protected.as_bytes(), &[0xff, 7, 0, 0, 0x20, 0, 0, 0, 0, 0]);
    }
    #[test]
    fn resident_table_geometry_rejects_truncation_wrap_and_bad_handlers() {
        for (base, handler32, handler64) in [
            (0, 0x8100, 0x8200),
            (0x200001, 0x8100, 0x8200),
            (0xfffff000, 0x8100, 0x8200),
            (u64::MAX - 15, 0x8100, 0x8200),
            (0x200000, 0, 0x8200),
            (0x200000, 1u64 << 32, 0x8200),
            (0x200000, 0x8100, 0),
            (0x200000, 0x8100, 1u64 << 32),
        ] {
            assert!(pointers(base, handler32, handler64).is_none());
        }
        assert!(pointers((1u64 << 32) - 6144, 0x8100, 0x8200).is_some());
    }
}
