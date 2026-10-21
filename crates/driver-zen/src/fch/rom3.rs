//! How the x86 cores see the SPI flash: the 16 MiB ROM2 window below 4 GiB
//! and the 64 MiB ROM3 window above it, and where the reset image finds its
//! FFS (the flash anchor the image builder patches).
//!
//! # Facts
//!
//! - **ROM2** `[FF00_0000h, 1_0000_0000h)` decodes 24-bit SPI addresses
//!   (PPR 55570 rev 3.16 §9.1.13 address mapping table, §9.2.9.2 "limited to a 24-bit
//!   address"). Bits 24/25 come from `SPIx5C` Addr32_Ctrl3\[SPI_ROM_page\]:
//!   page `p` shows SPI `[p × 16 MiB, (p + 1) × 16 MiB)` (the 16 MiB pages
//!   in the order 0-1-2-3 for `p = 0`, 1-0-3-2 for `p = 1`). The PSP boot
//!   ROM sets the page while searching for the EFS (AMD #55758 rev 1.13
//!   §4.1.7, Tables 21–22: XOR\[24\] = `SPIx5C[0]`, XOR\[25\]), so ROM2
//!   shows the half whose EFS it took.
//! - **ROM3** `[FD_0000_0000h, FD_0400_0000h)` (PPR 55570 §9.1.13, "LPC/SPI
//!   ROM_3", 64 MiB) is the flash above 4 GiB, inside the 12 GiB hole
//!   `[FD_0000_0000h, 1 TiB)` that DRAM and the DF's MMIO pools skip
//!   (`df::plan`, PSP_APOB §3.6; illumos `zen/physaddrs.h` names the hole
//!   and the SPI controller's 64 MiB flash aperture in it).
//!   fstart's reset image identity-maps it with its 1 GiB pages and reads it
//!   uncached (MTRR default type).
//!
//! # Open (no public source; checked at run time, fail closed)
//!
//! - Whether ROM3 decodes after the PSP/ABL hand over on SSP/GN (its enable
//!   and base are not in the public PPRs; the Rome/Milan APCB carries a
//!   `FchRom3BaseHigh` token, `0x7fc` in the S8030 donors, `0` in Oxide's
//!   Milan configs, meaning unknown), and whether the DF routes the hole to
//!   the FCH by default.
//! - Whether ROM3 is linear or also applies the `SPI_ROM_page` XOR.
//! - Whether the controller issues 4-byte addresses for ROM3 reads above
//!   16 MiB (the PSP itself reads the upper half when it boots it, so the
//!   part must already be in a mode that reaches it).
//!
//! [`FlashMap::probe`] answers the last two by comparing ROM3 with ROM2 at
//! the EFS of the shown page; without a usable ROM3 only the shown page is
//! readable (through ROM2) and everything else fails closed.

use tock_registers::interfaces::Readable;
use tock_registers::register_bitfields;
use zerocopy::little_endian::U32;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use super::{FchHw, Window, spi};

/// ROM3 window base and size (PPR 55570 §9.1.13).
pub const BASE: u64 = 0xFD_0000_0000;
pub const SIZE: u64 = 64 << 20;
/// ROM2 window base: the top 16 MiB below 4 GiB.
pub const ROM2_BASE: u64 = 0xFF00_0000;
/// A ROM2 page (one flash half on SP3).
pub const PAGE_SIZE: u32 = 16 << 20;

/// SPIx5C Addr32_Ctrl3 (AMD #55758 §4.1.7).
pub const ADDR32_CTRL3: u32 = 0x5C;

register_bitfields![u32,
    /// SPIx5C FCH::ITF::SPI::Addr32_Ctrl3.
    pub ADDR32_CTRL3 [
        /// XOR\[25:24\] of host and PSP SPI addresses: the 16 MiB page ROM2
        /// shows.
        SPI_ROM_PAGE OFFSET(0) NUMBITS(2) []
    ]
];

/// The SPI ROM page ROM2 currently shows.
pub fn rom_page<H: FchHw>(hw: &H) -> u8 {
    hw.mmio()
        .reg::<u32, ADDR32_CTRL3::Register>(spi::BASE + ADDR32_CTRL3)
        .read(ADDR32_CTRL3::SPI_ROM_PAGE) as u8
}

/// What ROM3 turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rom3 {
    /// Not decoding the flash (or not distinguishable from ROM2 garbage).
    Absent,
    /// ROM3 offset = flash offset.
    Linear,
    /// ROM3 applies the `SPI_ROM_page` XOR like ROM2.
    Paged,
}

/// Flash offset of the probe: the EFS of the page ROM2 shows (the booted
/// half always has one).
const PROBE_OFFSET: u32 = 0x2_0000;
const PROBE_LEN: usize = 0x40;
const EFS_SIGNATURE: [u8; 4] = 0x55aa_55aau32.to_le_bytes();

/// The CPU's view of an SPI flash of `size` bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlashMap {
    /// `SPIx5C` page: ROM2 shows flash `[page × 16 MiB, +16 MiB)`.
    pub page: u8,
    pub rom3: Rom3,
    /// Size of the flash part (16 or 32 MiB on SP3 boards).
    pub size: u32,
}

impl FlashMap {
    /// Find out how ROM3 maps the flash: compare the EFS ROM2 shows with
    /// ROM3 at that page's linear offset and at the XORed one. Exactly one
    /// must match; no EFS in ROM2, no match or two matches (identical
    /// halves) leave ROM3 unused. `read(address, buf)` reads physical
    /// memory.
    pub fn probe(page: u8, size: u32, read: impl Fn(u64, &mut [u8])) -> Self {
        let mut rom2 = [0u8; PROBE_LEN];
        read(ROM2_BASE + u64::from(PROBE_OFFSET), &mut rom2);
        let shown = u32::from(page & 3) * PAGE_SIZE;
        let rom3_shows = |offset: u32| {
            let mut bytes = [0u8; PROBE_LEN];
            read(BASE + u64::from(offset), &mut bytes);
            bytes == rom2
        };
        let linear = shown + PROBE_OFFSET < size && rom3_shows(shown + PROBE_OFFSET);
        let paged = page & 3 != 0 && rom3_shows(PROBE_OFFSET);
        let rom3 = match (rom2[..4] == EFS_SIGNATURE, linear, paged) {
            (true, true, false) => Rom3::Linear,
            (true, false, true) => Rom3::Paged,
            _ => Rom3::Absent,
        };
        Self { page, rom3, size }
    }

    /// Flash offset of the page ROM2 shows.
    pub const fn rom2_offset(&self) -> u32 {
        (self.page as u32 & 3) * PAGE_SIZE
    }

    /// The physical address of flash `[offset, offset + len)` as one
    /// contiguous range: through ROM3 when it decodes, else through ROM2 if
    /// the range lies in its page. `None` if the CPU cannot see it.
    pub fn address(&self, offset: u32, len: u32) -> Option<u64> {
        let end = offset.checked_add(len.max(1)).filter(|&e| e <= self.size)?;
        let same_page = offset / PAGE_SIZE == (end - 1) / PAGE_SIZE;
        let xor = self.rom2_offset();
        match self.rom3 {
            Rom3::Linear => Some(BASE + u64::from(offset)),
            Rom3::Paged if xor == 0 || same_page => Some(BASE + u64::from(offset ^ xor)),
            Rom3::Paged => None,
            Rom3::Absent => (offset >= xor && end <= xor + PAGE_SIZE)
                .then(|| ROM2_BASE + u64::from(offset - xor)),
        }
    }
}

/// Magic and version of [`FlashAnchor`].
pub const ANCHOR_MAGIC: [u8; 8] = *b"SP3FLASH";
pub const ANCHOR_VERSION: u32 = 1;
pub const ANCHOR_SIZE: usize = 24;

/// Where the reset image's FFS lives in the SPI flash, as absolute flash
/// offsets (read through ROM3, so it does not matter which half ROM2
/// shows). The reset image links a [`FlashAnchor::placeholder`] at an
/// 8-byte aligned address; the image builder fills it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C, align(8))]
pub struct FlashAnchor {
    pub magic: [u8; 8],
    pub version: U32,
    /// Size of the flash part.
    pub flash_size: U32,
    /// FFS start, flash offset.
    pub ffs_offset: U32,
    pub ffs_size: U32,
}

const _: () = assert!(core::mem::size_of::<FlashAnchor>() == ANCHOR_SIZE);

impl FlashAnchor {
    /// The unpatched site: magic and version, no FFS.
    pub const fn placeholder() -> Self {
        Self::new(0, 0, 0)
    }

    pub const fn new(flash_size: u32, ffs_offset: u32, ffs_size: u32) -> Self {
        Self {
            magic: ANCHOR_MAGIC,
            version: U32::new(ANCHOR_VERSION),
            flash_size: U32::new(flash_size),
            ffs_offset: U32::new(ffs_offset),
            ffs_size: U32::new(ffs_size),
        }
    }

    /// A patched anchor: right magic and version, an FFS inside a flash
    /// of 16, 32 or 64 MiB.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let anchor = Self::read_from_bytes(bytes.get(..ANCHOR_SIZE)?).ok()?;
        let flash = anchor.flash_size.get();
        let end = anchor.ffs_offset.get().checked_add(anchor.ffs_size.get())?;
        (anchor.magic == ANCHOR_MAGIC
            && anchor.version.get() == ANCHOR_VERSION
            && matches!(flash, 0x100_0000 | 0x200_0000 | 0x400_0000)
            && anchor.ffs_size.get() != 0
            && end <= flash)
            .then_some(anchor)
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::fch;
    use super::*;

    const HALF: u32 = PAGE_SIZE;

    /// A 32 MiB flash whose halves differ at the EFS, seen through ROM2
    /// (page `page`) and ROM3 (`rom3`); unmapped addresses read 0xff.
    fn bus(page: u8, rom3: Rom3) -> impl Fn(u64, &mut [u8]) {
        fn efs(offset: u32) -> u8 {
            let at = offset % HALF;
            if (PROBE_OFFSET..PROBE_OFFSET + 4).contains(&at) {
                EFS_SIGNATURE[(at - PROBE_OFFSET) as usize]
            } else {
                (offset >> 24) as u8 ^ (offset as u8).wrapping_mul(7)
            }
        }
        let shown = u32::from(page) * HALF;
        move |address: u64, buf: &mut [u8]| {
            buf.iter_mut().enumerate().for_each(|(i, b)| {
                let a = address + i as u64;
                *b = if (ROM2_BASE..1 << 32).contains(&a) {
                    efs(shown + (a - ROM2_BASE) as u32)
                } else if (BASE..BASE + u64::from(2 * HALF)).contains(&a) {
                    let o = (a - BASE) as u32;
                    match rom3 {
                        Rom3::Linear => efs(o),
                        Rom3::Paged => efs(o ^ shown),
                        Rom3::Absent => 0xff,
                    }
                } else {
                    0xff
                };
            })
        }
    }

    #[test]
    fn probe_tells_linear_paged_and_absent_rom3_apart() {
        for page in [0, 1] {
            for rom3 in [Rom3::Linear, Rom3::Paged, Rom3::Absent] {
                let map = FlashMap::probe(page, 2 * HALF, bus(page, rom3));
                // With page 0 both mappings are the same: linear.
                let expected = match (page, rom3) {
                    (0, Rom3::Paged) => Rom3::Linear,
                    _ => rom3,
                };
                assert_eq!(map.rom3, expected, "page {page} {rom3:?}");
            }
        }
        // Identical halves cannot be told apart.
        let same = |a: u64, buf: &mut [u8]| {
            bus(0, Rom3::Linear)(
                if a >= BASE {
                    BASE + (a - BASE) % u64::from(HALF)
                } else {
                    a
                },
                buf,
            )
        };
        assert_eq!(FlashMap::probe(1, 2 * HALF, same).rom3, Rom3::Absent);
        // No EFS in ROM2: nothing to compare against.
        let erased = |_: u64, buf: &mut [u8]| buf.fill(0xff);
        assert_eq!(FlashMap::probe(0, 2 * HALF, erased).rom3, Rom3::Absent);
    }

    #[test]
    fn addresses_follow_the_mapping() {
        let map = |page, rom3| FlashMap {
            page,
            rom3,
            size: 2 * HALF,
        };
        let ffs = (0x60_0000, 0x7e_0000);
        // Linear ROM3 reaches everything, ROM2 page irrelevant.
        assert_eq!(
            map(1, Rom3::Linear).address(ffs.0, ffs.1),
            Some(BASE + 0x60_0000)
        );
        assert_eq!(map(0, Rom3::Linear).address(0, 2 * HALF), Some(BASE));
        assert_eq!(map(0, Rom3::Linear).address(HALF, HALF + 1), None);
        // Paged ROM3 with page 1: the lower half sits in ROM3's upper half.
        let paged = map(1, Rom3::Paged);
        assert_eq!(
            paged.address(ffs.0, ffs.1),
            Some(BASE + u64::from(HALF) + 0x60_0000)
        );
        assert_eq!(paged.address(HALF, HALF), Some(BASE));
        assert_eq!(paged.address(0, 2 * HALF), None, "not contiguous");
        assert_eq!(map(0, Rom3::Paged).address(0, 2 * HALF), Some(BASE));
        // Without ROM3 only ROM2's page is visible.
        let rom2 = map(0, Rom3::Absent);
        assert_eq!(rom2.address(ffs.0, ffs.1), Some(ROM2_BASE + 0x60_0000));
        assert_eq!(rom2.address(HALF + 0x2_0000, 0x40), None);
        assert_eq!(map(1, Rom3::Absent).address(HALF, HALF), Some(ROM2_BASE));
    }

    #[test]
    fn rom_page_reads_spix5c() {
        let hw = fch();
        hw.mmio.put(spi::BASE + ADDR32_CTRL3, 4, 0xffff_fffd);
        assert_eq!(rom_page(&hw), 1);
    }

    #[test]
    fn anchor_placeholder_and_patched_value() {
        let placeholder = FlashAnchor::placeholder();
        assert_eq!(placeholder.as_bytes().len(), ANCHOR_SIZE);
        assert_eq!(&placeholder.as_bytes()[..8], b"SP3FLASH");
        assert!(FlashAnchor::parse(placeholder.as_bytes()).is_none());
        let patched = FlashAnchor::new(2 * HALF, 0x60_0000, 0x10_0000);
        assert_eq!(FlashAnchor::parse(patched.as_bytes()), Some(patched));
        let outside = FlashAnchor::new(HALF, 0xf0_0000, 0x20_0000);
        assert!(FlashAnchor::parse(outside.as_bytes()).is_none());
    }
}
