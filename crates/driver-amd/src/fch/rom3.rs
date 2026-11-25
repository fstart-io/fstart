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
//!   ROM_3", 64 MiB; [`BASE`]) is the flash above 4 GiB on SP3, inside the
//!   12 GiB hole
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
//! - On Qilian (Venice) ROM3's base is the APCB's (`FchRom3BaseHigh`):
//!   `SPIx60[31:26] | SPIx64 << 32` ([`hardware_base`]; Venice FchPei
//!   `0xcf11e0`, AmdPspPeiV2Wh `0xe576c0` [I]), `0x2FFD_0000_0000` on the
//!   GIGABYTE MD25-CG0, 0 (below 4 GiB, unusable) on Dell's PR9XJ. The
//!   vendors' x86 code reads flash through SMN instead
//!   ([`super::spi::smn`]).
//!
//! [`FlashMap::probe`] answers the last two by comparing ROM3 with ROM2 at
//! the EFS of the shown page; without a usable ROM3 only the shown page is
//! readable (through ROM2) and everything else fails closed.
//!
//! ROM2 is the window the vendor's own x86 code starts from, so it is
//! always preferred: [`FlashMap::rom2`] needs no ROM3 access at all, and a
//! caller probes ROM3 only for flash outside the shown page.

use fstart_amdfw_format::ROM2_BASE;
use tock_registers::interfaces::Readable;
use tock_registers::register_bitfields;

use super::FchHw;
use crate::window::Window;

/// ROM3 window base and size on SP3 (PPR 55570 §9.1.13).
pub const BASE: u64 = 0xFD_0000_0000;
pub const SIZE: u64 = 64 << 20;
/// A ROM2 page (one flash half on SP3).
pub const PAGE_SIZE: u32 = 16 << 20;

/// SPIx5C Addr32_Ctrl3 (AMD #55758 §4.1.7).
pub const ADDR32_CTRL3: u32 = 0x5C;
/// SPIx60: the ROM3 base's low bits and the flash size.
pub const ROM3_LOW: u32 = 0x60;
/// SPIx64: the ROM3 base's high dword.
pub const ROM3_HIGH: u32 = 0x64;

register_bitfields![u32,
    /// SPIx5C FCH::ITF::SPI::Addr32_Ctrl3.
    pub ADDR32_CTRL3 [
        /// XOR\[25:24\] of host and PSP SPI addresses: the 16 MiB page ROM2
        /// shows.
        SPI_ROM_PAGE OFFSET(0) NUMBITS(2) []
    ],
    /// SPIx60 (32-bit, Qilian; SMN `0x02DC_4060`): ROM3 base\[31:26\] and
    /// the size of the first flash part, `64 MiB >> ROM_SIZE`.
    pub Rom3Low [
        ROM_SIZE OFFSET(0) NUMBITS(3) [],
        BASE OFFSET(26) NUMBITS(6) []
    ],
    /// SPIx64 (32-bit, Qilian): ROM3 base\[63:32\].
    pub Rom3High [
        BASE OFFSET(0) NUMBITS(32) []
    ]
];

/// The SPI ROM page ROM2 currently shows.
pub fn rom_page<H: FchHw>(hw: &H) -> u8 {
    hw.mmio()
        .reg::<u32, ADDR32_CTRL3::Register>(super::SPI_BASE + ADDR32_CTRL3)
        .read(ADDR32_CTRL3::SPI_ROM_PAGE) as u8
}

/// ROM3's base as the FCH decodes it (Qilian: `SPIx60[31:26] | SPIx64 <<
/// 32`, the APCB's `FchRom3BaseHigh`), `None` if the controller reads
/// all-ones or the base is below 4 GiB, where the ROM3 window would
/// overlap the 32-bit MMIO (Dell's APCB leaves it 0).
pub fn hardware_base<H: FchHw>(hw: &H) -> Option<u64> {
    let low = hw
        .mmio()
        .reg::<u32, Rom3Low::Register>(super::SPI_BASE + ROM3_LOW)
        .extract();
    let high = hw
        .mmio()
        .reg::<u32, Rom3High::Register>(super::SPI_BASE + ROM3_HIGH)
        .extract();
    if low.get() == u32::MAX || high.get() == u32::MAX {
        return None;
    }
    let base =
        u64::from(low.read(Rom3Low::BASE)) << 26 | u64::from(high.read(Rom3High::BASE)) << 32;
    (base >= 1 << 32).then_some(base)
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
const EFS_SIGNATURE: [u8; 4] = fstart_amdfw_format::efs::SIGNATURE.to_le_bytes();

/// The CPU's view of an SPI flash of `size` bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlashMap {
    /// `SPIx5C` page: ROM2 shows flash `[page × 16 MiB, +16 MiB)`.
    pub page: u8,
    pub rom3: Rom3,
    /// Where ROM3 decodes ([`BASE`] on SP3, [`hardware_base`] on Qilian).
    pub base: u64,
    /// Size of the flash part (16 or 32 MiB on SP3 boards).
    pub size: u32,
}

impl FlashMap {
    /// ROM2 only: ROM3 not probed (and never read).
    pub const fn rom2(page: u8, size: u32) -> Self {
        Self {
            page,
            rom3: Rom3::Absent,
            base: BASE,
            size,
        }
    }

    /// Find out how ROM3 maps the flash: compare the EFS ROM2 shows with
    /// ROM3 at that page's linear offset and at the XORed one. Exactly one
    /// must match; no EFS in ROM2, no match or two matches (identical
    /// halves) leave ROM3 unused. `read(address, buf)` reads physical
    /// memory.
    pub fn probe(page: u8, size: u32, read: impl Fn(u64, &mut [u8])) -> Self {
        Self::probe_with(BASE, page, size, PROBE_OFFSET, read, |rom2| {
            rom2[..4] == EFS_SIGNATURE
        })
    }

    /// As [`Self::probe`] with ROM3 at `base`, comparing ROM2 and ROM3 at
    /// `offset` within the page ROM2 shows instead of at its EFS: for
    /// layouts whose shown page has no EFS (Venice's dual-origin images
    /// show the BIOS bank). The bytes there must be programmed (neither
    /// erased nor zero); a reset image's 16-bit entry at the top of the
    /// page is.
    pub fn probe_at(
        base: u64,
        page: u8,
        size: u32,
        offset: u32,
        read: impl Fn(u64, &mut [u8]),
    ) -> Self {
        Self::probe_with(base, page, size, offset, read, |rom2| {
            rom2.iter().any(|&b| b != 0xff) && rom2.iter().any(|&b| b != 0)
        })
    }

    /// As [`Self::probe`] with ROM3 at `base` (the EFS of the shown page).
    pub fn probe_efs_at(base: u64, page: u8, size: u32, read: impl Fn(u64, &mut [u8])) -> Self {
        Self::probe_with(base, page, size, PROBE_OFFSET, read, |rom2| {
            rom2[..4] == EFS_SIGNATURE
        })
    }

    fn probe_with(
        base: u64,
        page: u8,
        size: u32,
        offset: u32,
        read: impl Fn(u64, &mut [u8]),
        programmed: impl Fn(&[u8; PROBE_LEN]) -> bool,
    ) -> Self {
        let absent = Self {
            base,
            ..Self::rom2(page, size)
        };
        let Some(end) = offset.checked_add(PROBE_LEN as u32) else {
            return absent;
        };
        if end > PAGE_SIZE {
            return absent;
        }
        let mut rom2 = [0u8; PROBE_LEN];
        read(ROM2_BASE + u64::from(offset), &mut rom2);
        let shown = u32::from(page & 3) * PAGE_SIZE;
        let rom3_shows = |offset: u32| {
            let mut bytes = [0u8; PROBE_LEN];
            read(base + u64::from(offset), &mut bytes);
            bytes == rom2
        };
        let linear = shown + end <= size && rom3_shows(shown + offset);
        let paged = page & 3 != 0 && rom3_shows(offset);
        let rom3 = match (programmed(&rom2), linear, paged) {
            (true, true, false) => Rom3::Linear,
            (true, false, true) => Rom3::Paged,
            _ => Rom3::Absent,
        };
        Self {
            page,
            rom3,
            base,
            size,
        }
    }

    /// Flash offset of the page ROM2 shows.
    pub const fn rom2_offset(&self) -> u32 {
        (self.page as u32 & 3) * PAGE_SIZE
    }

    /// The physical address of flash `[offset, offset + len)` as one
    /// contiguous range: through ROM2 if the range lies in the page it
    /// shows, else through ROM3 when it decodes. `None` if the CPU cannot
    /// see it.
    pub fn address(&self, offset: u32, len: u32) -> Option<u64> {
        let end = offset.checked_add(len.max(1)).filter(|&e| e <= self.size)?;
        let same_page = offset / PAGE_SIZE == (end - 1) / PAGE_SIZE;
        let xor = self.rom2_offset();
        if offset >= xor && end <= xor + PAGE_SIZE {
            return Some(ROM2_BASE + u64::from(offset - xor));
        }
        match self.rom3 {
            Rom3::Linear => Some(self.base + u64::from(offset)),
            Rom3::Paged if xor == 0 || same_page => Some(self.base + u64::from(offset ^ xor)),
            Rom3::Paged | Rom3::Absent => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::model::fch;

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
    fn probe_at_works_without_an_efs_in_the_shown_page() {
        let top = PAGE_SIZE - PROBE_LEN as u32;
        for page in [0, 1] {
            for rom3 in [Rom3::Linear, Rom3::Paged, Rom3::Absent] {
                let map = FlashMap::probe_at(BASE, page, 2 * HALF, top, bus(page, rom3));
                let expected = match (page, rom3) {
                    (0, Rom3::Paged) => Rom3::Linear,
                    _ => rom3,
                };
                assert_eq!(map.rom3, expected, "page {page} {rom3:?}");
            }
        }
        // Erased flash at the probe proves nothing.
        let erased = |_: u64, buf: &mut [u8]| buf.fill(0xff);
        assert_eq!(
            FlashMap::probe_at(BASE, 0, 2 * HALF, top, erased).rom3,
            Rom3::Absent
        );
        // A probe beyond the page is refused.
        assert_eq!(
            FlashMap::probe_at(BASE, 0, 2 * HALF, PAGE_SIZE, bus(0, Rom3::Linear)).rom3,
            Rom3::Absent
        );
    }

    #[test]
    fn addresses_follow_the_mapping() {
        let map = |page, rom3| FlashMap {
            page,
            rom3,
            base: BASE,
            size: 2 * HALF,
        };
        let ffs = (0x60_0000, 0x7e_0000);
        // Linear ROM3 reaches everything outside ROM2's page.
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
        // The shown page goes through ROM2 even when ROM3 decodes.
        assert_eq!(paged.address(HALF, HALF), Some(ROM2_BASE));
        assert_eq!(
            map(0, Rom3::Linear).address(ffs.0, ffs.1),
            Some(ROM2_BASE + 0x60_0000)
        );
        assert_eq!(paged.address(0, 2 * HALF), None, "not contiguous");
        assert_eq!(map(0, Rom3::Paged).address(0, 2 * HALF), Some(BASE));
        // Without ROM3 only ROM2's page is visible.
        let rom2 = map(0, Rom3::Absent);
        assert_eq!(rom2.address(ffs.0, ffs.1), Some(ROM2_BASE + 0x60_0000));
        assert_eq!(rom2.address(HALF + 0x2_0000, 0x40), None);
        assert_eq!(map(1, Rom3::Absent).address(HALF, HALF), Some(ROM2_BASE));
    }

    #[test]
    fn hardware_base_is_the_apcb_policy() {
        let hw = fch();
        let spi = crate::fch::SPI_BASE;
        // GIGABYTE MD25-CG0: FchRom3BaseHigh = 0x2FFD; size code 0 (64 MiB).
        hw.mmio.put(spi + ROM3_LOW, 4, 0x0000_0000);
        hw.mmio.put(spi + ROM3_HIGH, 4, 0x2ffd);
        assert_eq!(hardware_base(&hw), Some(0x2ffd_0000_0000));
        // The low bits come from SPIx60[31:26] only.
        hw.mmio.put(spi + ROM3_LOW, 4, 0xfc00_0003);
        assert_eq!(hardware_base(&hw), Some(0x2ffd_fc00_0000));
        // Dell's 0: below 4 GiB, not used.
        hw.mmio.put(spi + ROM3_HIGH, 4, 0);
        assert_eq!(hardware_base(&hw), None);
        hw.mmio.put(spi + ROM3_LOW, 4, u32::MAX);
        assert_eq!(hardware_base(&hw), None);
    }

    #[test]
    fn a_hardware_base_moves_the_rom3_probe_and_addresses() {
        const ROM3: u64 = 0x2ffd_0000_0000;
        let moved = |a: u64, buf: &mut [u8]| {
            let a = if a >= ROM3 {
                a - ROM3 + BASE
            } else if a >= BASE {
                0x1000
            } else {
                a
            };
            bus(1, Rom3::Linear)(a, buf)
        };
        let map = FlashMap::probe_efs_at(ROM3, 1, 2 * HALF, moved);
        assert_eq!(map.rom3, Rom3::Linear);
        assert_eq!(map.address(0x60_0000, 0x100), Some(ROM3 + 0x60_0000));
        // The SP3 base sees nothing there.
        assert_eq!(FlashMap::probe(1, 2 * HALF, moved).rom3, Rom3::Absent);
    }

    #[test]
    fn rom_page_reads_spix5c() {
        let hw = fch();
        hw.mmio
            .put(crate::fch::SPI_BASE + ADDR32_CTRL3, 4, 0xffff_fffd);
        assert_eq!(rom_page(&hw), 1);
    }
}
