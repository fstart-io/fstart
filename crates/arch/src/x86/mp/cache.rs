//! Ordered pre-Rust MSR restoration, resident inside the shared AP trampoline.
//! Vendor routing/gates are supplied by the CPU driver, not guessed here.
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout,
    byteorder::{LittleEndian, U32},
};

pub const MAX_WRITES: usize = 128;
const FIXED: [u32; 11] = [
    0x250, 0x258, 0x259, 0x268, 0x269, 0x26a, 0x26b, 0x26c, 0x26d, 0x26e, 0x26f,
];

/// Assembly consumes exactly three little-endian DWORDs per WRMSR.
#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable)]
pub struct Write {
    index: U32<LittleEndian>,
    low: U32<LittleEndian>,
    high: U32<LittleEndian>,
}
impl Write {
    pub fn new(index: u32, value: u64) -> Self {
        Self {
            index: U32::new(index),
            low: U32::new(value as u32),
            high: U32::new((value >> 32) as u32),
        }
    }
    pub fn index(&self) -> u32 {
        self.index.get()
    }
    pub fn value(&self) -> u64 {
        u64::from(self.low.get()) | (u64::from(self.high.get()) << 32)
    }
}
const _: () = assert!(core::mem::size_of::<Write>() == 12);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Unsupported,
    Capacity,
    /// The BSP cache policy could not be installed.
    Setup,
}

/// A complete, bounded cache-state image. No allocation or image-local writable
/// state is required. The BSP publishes its bytes before releasing any AP.
pub struct State {
    writes: [Write; MAX_WRITES],
    count: usize,
    global_cache_off: bool,
}
impl State {
    /// Require BSP and every intended AP to remain cache-off through the last
    /// restore. A missing CPU leaves all gates closed and invokes fail-stop.
    ///
    /// This selects a protocol, not a hardware attestation. The platform must
    /// exclude asynchronous SMI/NMI handlers, validate the complete CPU set and
    /// cache-disable/WBINVD semantics, and prohibit later unsynchronized shared
    /// cache-policy writes. Independent mode is not a physical-AMD fallback.
    pub fn require_global_cache_off(mut self) -> Self {
        self.global_cache_off = true;
        self
    }

    pub fn global_cache_off(&self) -> bool {
        self.global_cache_off
    }

    /// Capture architectural MTRRs and PAT from a validated BSP state.
    /// `prefix` runs after MTRRs are disabled, before PAT/fixed/variable writes;
    /// `suffix` runs after those writes, before restoring MTRR_DEF_TYPE.
    ///
    /// AMD callers must expose RdDram/WrDram during fixed-register reads, supply
    /// the AP modification gate and TOM/TOM2 routing in `prefix`, and restore
    /// final SYSCFG in `suffix`. They must serialize any sibling-shared BSP
    /// accesses. Capturing ordinary fixed type bytes loses AMD routing.
    ///
    /// Caller guarantees PAT support, valid BSP MTRR/PAT/routing state and an
    /// identical intended AP policy. This does not validate physical mappings.
    pub fn capture(
        mut read: impl FnMut(u32) -> u64,
        prefix: &[Write],
        suffix: &[Write],
    ) -> Result<Self, Error> {
        let cap = read(0xfe);
        let variables = (cap & 0xff) as usize;
        if cap & (1 << 8) == 0 {
            return Err(Error::Unsupported);
        }
        // Disable each variable mask before updating its base, even if invalid.
        let count = 3 * variables + FIXED.len() + 3 + prefix.len() + suffix.len();
        if count > MAX_WRITES {
            return Err(Error::Capacity);
        }
        let default = read(0x2ff);
        let mut result = Self {
            writes: [Write::new(0, 0); MAX_WRITES],
            count: 0,
            global_cache_off: false,
        };
        result.push(Write::new(0x2ff, 0));
        for write in prefix {
            result.push(*write);
        }
        result.push(Write::new(0x277, read(0x277))); // PAT before paging.
        for index in FIXED {
            result.push(Write::new(index, read(index)));
        }
        for pair in 0..variables as u32 {
            let base = 0x200 + 2 * pair;
            let mask = read(base + 1);
            result.push(Write::new(base + 1, 0));
            result.push(Write::new(base, read(base)));
            result.push(Write::new(base + 1, mask));
        }
        for write in suffix {
            result.push(*write);
        }
        result.push(Write::new(0x2ff, default));
        debug_assert_eq!(result.count, count);
        Ok(result)
    }
    /// Wrap an already ordered write sequence (e.g. the architectural MTRR
    /// snapshot). The sequence must end by restoring IA32_MTRR_DEF_TYPE.
    pub fn from_writes(writes: impl IntoIterator<Item = Write>) -> Result<Self, Error> {
        let mut result = Self {
            writes: [Write::new(0, 0); MAX_WRITES],
            count: 0,
            global_cache_off: false,
        };
        for write in writes {
            if result.count == MAX_WRITES {
                return Err(Error::Capacity);
            }
            result.push(write);
        }
        Ok(result)
    }
    fn push(&mut self, write: Write) {
        self.writes[self.count] = write;
        self.count += 1;
    }
    pub fn writes(&self) -> &[Write] {
        &self.writes[..self.count]
    }
    pub fn bytes(&self) -> &[u8] {
        self.writes().as_bytes()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    #[test]
    fn complete_cache_image_preserves_order_values_and_wire_layout() {
        let prefix = [
            Write::new(0xc0010010, 0x7c0000),
            Write::new(0xc001001a, 0x80000000),
            Write::new(0xc001001d, 0x140000000),
        ];
        let suffix = [Write::new(0xc0010010, 0x740000)];
        let mut reads = std::vec::Vec::new();
        let image = State::capture(
            |index| {
                reads.push(index);
                match index {
                    0xfe => 0x102,
                    0x2ff => 0xc06,
                    0x250 => 0x1e1e1e1e1e1e1e1e,
                    _ => (u64::from(index) << 32) | 0x9876,
                }
            },
            &prefix,
            &suffix,
        )
        .unwrap();
        assert!(!image.global_cache_off());
        let image = image.require_global_cache_off();
        assert!(image.global_cache_off());
        let values: std::vec::Vec<_> = image
            .writes()
            .iter()
            .map(|w| (w.index(), w.value()))
            .collect();
        assert_eq!(values[0], (0x2ff, 0));
        assert_eq!(
            &values[1..4],
            &[
                (0xc0010010, 0x7c0000),
                (0xc001001a, 0x80000000),
                (0xc001001d, 0x140000000)
            ]
        );
        assert_eq!(values[5], (0x250, 0x1e1e1e1e1e1e1e1e));
        assert_eq!(
            &values[16..19],
            &[
                (0x201, 0),
                (0x200, (0x200u64 << 32) | 0x9876),
                (0x201, (0x201u64 << 32) | 0x9876)
            ]
        );
        assert_eq!(values[values.len() - 2], (0xc0010010, 0x740000));
        assert_eq!(values.last(), Some(&(0x2ff, 0xc06)));
        assert_eq!(
            &image.bytes()[12..24],
            &[0x10, 0, 1, 0xc0, 0, 0, 0x7c, 0, 0, 0, 0, 0]
        );
        assert!(reads.contains(&0x277));
        assert!(reads.contains(&0x26f));
        assert!(reads.contains(&0x203));
    }
    #[test]
    fn rejected_geometry_reads_no_cache_state() {
        for cap in [0x80, 0x1ff] {
            let mut reads = 0;
            assert!(
                State::capture(
                    |index| {
                        assert_eq!(index, 0xfe);
                        reads += 1;
                        cap
                    },
                    &[],
                    &[]
                )
                .is_err()
            );
            assert_eq!(reads, 1);
        }
    }
}
