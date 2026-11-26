// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SP7 flash as the x86 reaches it after the PSP hands over, for the
//! boot media and for the static root of trust (FCH.md §1.3).
//!
//! - **ROM2** (`FF00_0000h`, 16 MiB) is where the reset image expects the
//!   FFS (bank 0: [`crate::venice::LAYOUT`]`.ffs`), but it is not certain to
//!   show bank 0: both vendors' SEC force a ROM2 override (`SPIx30[3:0]`)
//!   while copying out of it. The FFS is mounted from ROM2 when it
//!   authenticates there; else its window is copied from the flash through
//!   SMN into the DRAM below the firmware's ([`ffs_copy`]) and mounted from
//!   that copy.
//! - **ROM3** decodes at the APCB's base (`FchRom3BaseHigh`,
//!   [`fstart_driver_amd::fch::rom3::hardware_base`]; `0x2FFD_0000_0000` on
//!   the MD25-CG0), which the reset image's identity map may not reach.
//!   The reset image the PSP loaded is read through ROM3 when it is mapped
//!   and shows the flash linearly ([`FlashMap::probe_at`]), else through
//!   the SMN window the vendors' own x86 code reads the flash with
//!   ([`fstart_driver_amd::fch::spi::smn`]): only the pieces the ISH lookup
//!   needs ([`fetch_reset_image`]), into the heap. Both ISH slots must name
//!   the same image, which is measured as read from the flash.
//!
//! [`FlashMap::probe_at`]: fstart_driver_amd::fch::rom3::FlashMap::probe_at

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use fstart_amdfw_format::{Flash, Missing, Piece, Profile, ROM2_BASE, efs};

/// Largest single read the lookup may ask for: the reset image (BIOS entry
/// `0x62`, 3 MiB on Venice) with room to spare; directories are KiBs.
pub const MAX_READ: usize = 8 << 20;
/// Lookups before giving up: the EFS probes of every bank and the
/// directories take about 30.
const MAX_PASSES: usize = 256;

/// What the flash reader read so far.
struct Pieces(Vec<(usize, Vec<u8>)>);

impl Pieces {
    /// The lookup `f` over a sparse view of a `size`-byte flash with its
    /// image origin at `bank`, repeated with each range it missed read in,
    /// until it misses nothing. `None` if a read fails, is too large or
    /// the lookup does not settle.
    fn settle<T>(
        &mut self,
        size: usize,
        bank: usize,
        read: &mut impl FnMut(usize, &mut [u8]) -> bool,
        f: impl Fn(Flash<'_>) -> T,
    ) -> Option<T> {
        let missing = Missing::new(None);
        for _ in 0..MAX_PASSES {
            let result = {
                let view: Vec<Piece<'_>> = self
                    .0
                    .iter()
                    .map(|(offset, bytes)| Piece {
                        offset: *offset,
                        bytes,
                    })
                    .collect();
                f(Flash::sparse(
                    size,
                    &view,
                    &missing,
                    bank,
                    ROM2_BASE,
                    Profile::VENICE,
                ))
            };
            let Some((offset, len)) = missing.take() else {
                return Some(result);
            };
            if len > MAX_READ {
                return None;
            }
            let mut bytes = alloc::vec![0; len];
            if !read(offset, &mut bytes) {
                return None;
            }
            self.0.push((offset, bytes));
        }
        None
    }
}

impl Pieces {
    /// The first bank of a `size`-byte flash with a Venice EFS.
    fn efs_bank(
        &mut self,
        size: usize,
        read: &mut impl FnMut(usize, &mut [u8]) -> bool,
    ) -> Result<usize, Lookup> {
        (0..size)
            .step_by(efs::WINDOW as usize)
            .find_map(|bank| {
                match self.settle(size, bank, read, |flash| flash.efs_checked().is_ok()) {
                    Some(true) => Some(Ok(bank)),
                    Some(false) => None,
                    None => Some(Err(Lookup::Read)),
                }
            })
            .ok_or(Lookup::NoEfs)?
    }
}

/// Whether the first bank with a Venice EFS has both ISH slots, `0x48`
/// and `0x4A`, in its PSP directories: the vendor's `IsABrecovery`
/// (AmdPspPeiV2Wh `0xe4fde0`, GIGABYTE `0x7098f14e`), the precondition of
/// its PSP ResetRequired warm reset (WARM_RESET §2.3). Slots that do not
/// check out count as absent.
pub fn ish_ab(size: usize, mut read: impl FnMut(usize, &mut [u8]) -> bool) -> Result<bool, Lookup> {
    use fstart_amdfw_format::directory::psp::{RECOVERY_A, RECOVERY_B};
    let mut pieces = Pieces(Vec::new());
    let bank = pieces.efs_bank(size, &mut read)?;
    pieces
        .settle(size, bank, &mut read, |flash| {
            flash.ish_slots_checked().is_ok_and(|(slots, _)| {
                [RECOVERY_A, RECOVERY_B]
                    .iter()
                    .all(|&kind| slots.iter().flatten().any(|slot| slot.kind == kind))
            })
        })
        .ok_or(Lookup::Read)
}

/// Why [`fetch_reset_image`] found no image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    /// No bank has a Venice EFS.
    NoEfs,
    /// The EFS's ISH slots name no reset image, or different ones.
    Image(fstart_amdfw_format::Error),
    /// A read failed, was too large, or the lookup did not settle.
    Read,
}

impl Lookup {
    /// For the log (inline: SoC code keeps no strings of its own).
    #[inline]
    pub const fn why(self) -> &'static str {
        match self {
            Self::NoEfs => "no EFS",
            Self::Image(_) => "no consistent ISH reset image",
            Self::Read => "flash read",
        }
    }
}

/// The reset image (BIOS entry `0x62`) both ISH slots of the first bank
/// with a Venice EFS name, in a flash of `size` bytes that only
/// `read(offset, buf)` reaches: the pieces the lookup reads are fetched as
/// it misses them, the image too, and the image's bytes are kept for good
/// (the rest is freed).
pub fn fetch_reset_image(
    size: usize,
    mut read: impl FnMut(usize, &mut [u8]) -> bool,
) -> Result<&'static [u8], Lookup> {
    let mut pieces = Pieces(Vec::new());
    let bank = pieces.efs_bank(size, &mut read)?;
    // The image as a flash range: the piece that holds it is kept.
    let (offset, len) = pieces
        .settle(size, bank, &mut read, |flash| {
            let image = flash.ish_reset_image()?;
            Ok(flash.offset_of(image).map(|offset| (offset, image.len())))
        })
        .ok_or(Lookup::Read)?
        .map_err(Lookup::Image)?
        .ok_or(Lookup::Read)?;
    let i = pieces
        .0
        .iter()
        .position(|(at, bytes)| *at <= offset && offset + len <= at + bytes.len())
        .ok_or(Lookup::Read)?;
    let (at, bytes) = pieces.0.swap_remove(i);
    let kept: &'static [u8] = Box::leak(bytes.into_boxed_slice());
    Ok(&kept[offset - at..offset - at + len])
}

/// ISH slot A (PSP L1 entry `0x48`) in [`ApcbRange::ish`].
pub const ISH_A: u8 = 1 << 0;
/// ISH slot B (`0x4A`).
pub const ISH_B: u8 = 1 << 1;

/// The APCB (`0x60/0`) of one or both ISH slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApcbRange {
    pub offset: usize,
    pub len: usize,
    /// The ISH slots whose BIOS directory names it ([`ISH_A`], [`ISH_B`]).
    pub ish: u8,
}

/// The flash ranges of the APCB every ISH slot's BIOS directory names as
/// entry `0x60` instance 0 (the one the vendor's CxlV2Manager rewrites,
/// CXL_COMPLETE.md §2.1.3), in slot order without repeats, each with
/// the slots that name it, in a flash of `size` bytes that only
/// `read(offset, buf)` reaches. Which slot the PSP booted only the PSP
/// says (`super::cxl_apcb`).
pub fn apcb_ranges(
    size: usize,
    mut read: impl FnMut(usize, &mut [u8]) -> bool,
) -> Result<Vec<ApcbRange>, Lookup> {
    use fstart_amdfw_format::directory::{Entry, bios, psp};
    let mut pieces = Pieces(Vec::new());
    let bank = pieces.efs_bank(size, &mut read)?;
    pieces
        .settle(size, bank, &mut read, |flash| {
            let (slots, _) = flash.ish_slots_checked()?;
            let mut out: Vec<ApcbRange> = Vec::new();
            for slot in slots.iter().flatten() {
                let ish = match slot.kind {
                    psp::RECOVERY_A => ISH_A,
                    psp::RECOVERY_B => ISH_B,
                    _ => 0,
                };
                let (_, Some(dir)) = flash.ish_slot_directories(slot)? else {
                    continue;
                };
                for entry in dir.entries.iter() {
                    if entry.kind() != bios::APCB || entry.instance() != 0 {
                        continue;
                    }
                    let data = dir.data(entry)?;
                    let Some(offset) = flash.offset_of(data) else {
                        continue;
                    };
                    match out
                        .iter_mut()
                        .find(|r| (r.offset, r.len) == (offset, data.len()))
                    {
                        Some(range) => range.ish |= ish,
                        None => out.push(ApcbRange {
                            offset,
                            len: data.len(),
                            ish,
                        }),
                    }
                }
            }
            Ok(out)
        })
        .ok_or(Lookup::Read)?
        .map_err(Lookup::Image)
}

#[cfg(all(feature = "bundle", target_os = "none"))]
pub(super) use bundle::{ab_layout, ffs, reset_image};

#[cfg(all(feature = "bundle", target_os = "none"))]
mod bundle {
    use super::super::Variant;
    use crate::layout::Region;
    use crate::media::MountError;
    use fstart_core::services::memory_detect::{E820Kind, E820State};
    use fstart_driver_amd::ccx::tsc::Tsc;
    use fstart_driver_amd::fch::rom3::{self, FlashMap, Rom3};
    use fstart_driver_amd::fch::spi::smn::SmnFlash;
    use fstart_driver_amd::fch::{FchHw, Native};

    /// The flash through SMN on the primary FCH's IOD.
    fn smn_flash(fch: &Native<Tsc>) -> Option<SmnFlash<fstart_pci::ecam::GlobalEcam>> {
        SmnFlash::new(fch.smn())
    }

    /// The reset image both ISH slots name: through ROM3 when it is
    /// identity-mapped and shows the flash linearly, else through SMN.
    pub fn reset_image<V: Variant>(fch: &Native<Tsc>) -> Option<&'static [u8]> {
        through_rom3::<V>(fch).or_else(|| through_smn::<V>(fch))
    }

    fn through_rom3<V: Variant>(fch: &Native<Tsc>) -> Option<&'static [u8]> {
        use fstart_amdfw_format::{Flash, Profile, ROM2_BASE};
        let size = V::ROM_SIZE;
        let base = rom3::hardware_base(fch)?;
        // The identity map of the reset image: PDPTs of 512 GiB.
        let mapped = (V::LAYOUT.pdpts * 512) << 30;
        if base
            .checked_add(u64::from(size))
            .is_none_or(|end| end > mapped)
        {
            fstart_log::info!("{}: ROM3 at {:#x} is not mapped", V::NAME, base);
            return None;
        }
        let page = rom3::rom_page(fch);
        // The shown page has the EFS (image bank) or the reset image's
        // 16-bit entry at its top (BIOS bank); either tells ROM3's mapping.
        let mut map = FlashMap::probe_efs_at(base, page, size, crate::media::read_phys);
        if map.rom3 == Rom3::Absent {
            map = FlashMap::probe_at(
                base,
                page,
                size,
                rom3::PAGE_SIZE - 0x40,
                crate::media::read_phys,
            );
        }
        let address = map.address(0, size)?;
        let view = crate::media::flash_slice(address, size);
        let image = (0..size)
            .step_by(rom3::PAGE_SIZE as usize)
            .map(|bank| Flash::region(view, bank as usize, ROM2_BASE, Profile::VENICE))
            .find(|flash| flash.efs_checked().is_ok())?
            .ish_reset_image();
        match image {
            Ok(image) => Some(image),
            Err(_) => {
                fstart_log::error!("{}: no consistent ISH reset image in ROM3", V::NAME);
                None
            }
        }
    }

    /// [`super::ish_ab`] through SMN; `false` when the flash cannot tell.
    pub fn ab_layout<V: Variant>(fch: &Native<Tsc>) -> bool {
        let Some(flash) = smn_flash(fch) else {
            fstart_log::error!("{}: the flash's SMN window does not answer", V::NAME);
            return false;
        };
        let size = flash.size().min(V::ROM_SIZE) as usize;
        let read = |offset: usize, buf: &mut [u8]| {
            u32::try_from(offset).is_ok_and(|offset| flash.read(offset, buf).is_ok())
        };
        super::ish_ab(size, read).unwrap_or_else(|why| {
            fstart_log::error!("{}: ISH slots through SMN: {}", V::NAME, why.why());
            false
        })
    }

    fn through_smn<V: Variant>(fch: &Native<Tsc>) -> Option<&'static [u8]> {
        let Some(flash) = smn_flash(fch) else {
            fstart_log::error!("{}: the flash's SMN window does not answer", V::NAME);
            return None;
        };
        let size = flash.size().min(V::ROM_SIZE) as usize;
        let read = |offset: usize, buf: &mut [u8]| {
            u32::try_from(offset).is_ok_and(|offset| flash.read(offset, buf).is_ok())
        };
        match super::fetch_reset_image(size, read) {
            Ok(image) => {
                fstart_log::info!(
                    "{}: reset image ({:#x} bytes) read through SMN",
                    V::NAME,
                    image.len()
                );
                Some(image)
            }
            Err(why) => {
                fstart_log::error!("{}: reset image through SMN: {}", V::NAME, why.why());
                None
            }
        }
    }

    /// The DRAM the FFS window is copied to when ROM2 does not show it:
    /// right below the firmware's early region, 1 MiB aligned. It stays
    /// reserved through the OS handoff, as the early region does (the
    /// flow loads from the mount until the payload runs and has no step
    /// that gives memory back): the FFS window rounded up to 1 MiB (5 MiB
    /// on Venice), only on a boot whose FFS did not authenticate in ROM2.
    pub fn ffs_copy<V: Variant>(ffs: Region) -> Region {
        let size = ffs.size.next_multiple_of(1 << 20);
        Region::new(V::LAYOUT.early.base - size, size)
    }

    /// Mount the FFS: from ROM2 if it authenticates there, else from a
    /// copy of its flash window read through SMN into [`ffs_copy`]'s DRAM,
    /// which leaves the memory map and the loads. Returns where it is
    /// mounted.
    pub fn ffs<V: Variant>(
        fch: &Native<Tsc>,
        e820: &mut E820State,
        ffs: Region,
    ) -> Result<Region, &'static str> {
        let error = match crate::media::mount(e820.entries(), &V::LAYOUT, ffs) {
            Ok(()) => return Ok(ffs),
            Err(error @ (MountError::Mount | MountError::Verify)) => error,
            Err(error) => return Err(error.step()),
        };
        fstart_log::warn!(
            "{}: the FFS in ROM2 at {:#x} fails ({}); reading it through SMN",
            V::NAME,
            ffs.base,
            error.step()
        );
        let copy = ffs_copy::<V>(ffs);
        let in_ram = e820.entries().iter().any(|entry| {
            entry.kind == E820Kind::Ram as u32
                && Region::new(entry.addr, entry.size).contains(&copy)
        });
        let flash = smn_flash(fch).ok_or("FFS flash")?;
        if !in_ram {
            return Err("FFS copy RAM");
        }
        // The FFS's flash offset: ROM2 meant to show flash bank 0.
        let offset = u32::try_from(ffs.base - V::LAYOUT.flash.base).map_err(|_| "FFS offset")?;
        crate::media::reserve(e820, copy.base, copy.size)?;
        // SAFETY: RAM of the memory map, reserved just now for the copy;
        // identity-mapped write-back; nothing else uses it.
        let dst =
            unsafe { core::slice::from_raw_parts_mut(copy.base as *mut u8, ffs.size as usize) };
        flash.read(offset, dst).map_err(|_| "FFS flash")?;
        let mounted = Region::new(copy.base, ffs.size);
        crate::media::mount(e820.entries(), &V::LAYOUT, mounted).map_err(|error| error.step())?;
        fstart_log::info!("{}: FFS mounted from its copy at {:#x}", V::NAME, copy.base);
        Ok(mounted)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use fstart_amdfw_format::directory::{
        BIOS_LEVEL2_COOKIE, BiosEntry, ImageSlotHeader, PSP_COOKIE, PSP_LEVEL2_COOKIE, PspEntry,
        bios, psp, write_table,
    };
    use fstart_amdfw_format::efs::Efs;
    use std::cell::Cell;
    use zerocopy::IntoBytes;

    const BANK: usize = 0x0100_0000;
    const IMAGE: usize = 0x00d0_0000;
    const IMAGE_SIZE: u32 = 0x30_0000;

    /// A 64 MiB Venice dual-origin flash: EFS and ISH A/B in bank 1, the
    /// reset image at the top of bank 0, each slot naming `images[slot]`.
    fn flash(images: [usize; 2]) -> std::vec::Vec<u8> {
        let mut rom = std::vec![0xffu8; 64 << 20];
        for (i, b) in rom[IMAGE..IMAGE + IMAGE_SIZE as usize]
            .iter_mut()
            .enumerate()
        {
            *b = i as u8 ^ 0xa5;
        }
        let mut efs = Efs::erased();
        efs.new_psp_directory = 0x3_0000u32.into();
        efs.generation = 0xffff_fffcu32.into();
        efs.rom_layout_selector = 1;
        rom[BANK + 0x2_0000..BANK + 0x2_0060].copy_from_slice(efs.as_bytes());
        let l1 = [
            PspEntry::new(psp::RECOVERY_A, 0, 0, 0, (1 << 62) | 0x3_2000),
            PspEntry::new(psp::RECOVERY_B, 0, 0, 0, (1 << 62) | 0x3_3000),
        ];
        // Header v1 (bit 31), mode 2, 1 KiB of header.
        let info = (1 << 31) | (2 << 24) | 1;
        write_table(&mut rom[BANK + 0x3_0000..], PSP_COOKIE, info, &l1).unwrap();
        for (slot, (ish, pl2)) in [(0x3_2000usize, 0x3_4000u32), (0x3_3000, 0x4_0000)]
            .into_iter()
            .enumerate()
        {
            let header = ImageSlotHeader::new(1, 2, 0, pl2, 0xbf00_0000, u32::MAX);
            rom[BANK + ish..BANK + ish + 32].copy_from_slice(header.as_bytes());
            let l2 = [PspEntry::new(
                psp::BIOS_TABLE,
                0,
                0,
                0x400,
                (2 << 62) | 0x2000,
            )];
            write_table(&mut rom[BANK + pl2 as usize..], PSP_LEVEL2_COOKIE, 0, &l2).unwrap();
            let bl2 = [BiosEntry::new(
                bios::RESET_IMAGE,
                0,
                0,
                BiosEntry::RESET | BiosEntry::COPY,
                IMAGE_SIZE,
                (1 << 62) | images[slot] as u64,
                0x7000_0000,
            )];
            write_table(
                &mut rom[BANK + pl2 as usize + 0x2000..],
                BIOS_LEVEL2_COOKIE,
                0,
                &bl2,
            )
            .unwrap();
        }
        rom
    }

    #[test]
    fn reads_only_the_pieces_and_keeps_the_flash_bytes() {
        let rom = flash([IMAGE, IMAGE]);
        let read_bytes = Cell::new(0usize);
        let image = fetch_reset_image(rom.len(), |offset, buf| {
            read_bytes.set(read_bytes.get() + buf.len());
            buf.copy_from_slice(&rom[offset..offset + buf.len()]);
            true
        })
        .unwrap();
        assert_eq!(image, &rom[IMAGE..IMAGE + IMAGE_SIZE as usize]);
        // The image and a few KiB of EFS probes and directories, not 64 MiB.
        assert!(
            read_bytes.get() < IMAGE_SIZE as usize + 0x2000,
            "{:#x}",
            read_bytes.get()
        );
    }

    #[test]
    fn both_ish_slots_make_an_ab_layout() {
        let rom = flash([IMAGE, IMAGE]);
        let read = |rom: &std::vec::Vec<u8>| {
            let rom = rom.clone();
            move |offset: usize, buf: &mut [u8]| {
                buf.copy_from_slice(&rom[offset..offset + buf.len()]);
                true
            }
        };
        assert_eq!(ish_ab(rom.len(), read(&rom)), Ok(true));
        // Slot B's L1 entry gone: no A/B layout, as the vendor's check.
        let mut single = rom.clone();
        let l1 = [PspEntry::new(
            psp::RECOVERY_A,
            0,
            0,
            0,
            (1 << 62) | 0x3_2000,
        )];
        let info = (1 << 31) | (2 << 24) | 1;
        write_table(&mut single[BANK + 0x3_0000..], PSP_COOKIE, info, &l1).unwrap();
        assert_eq!(ish_ab(single.len(), read(&single)), Ok(false));
        assert_eq!(ish_ab(rom.len(), |_, _| false), Err(Lookup::Read));
    }

    #[test]
    fn every_slot_s_apcb_is_found() {
        let mut rom = flash([IMAGE, IMAGE]);
        // Each slot's BIOS L2 gets an APCB (0x60, instance 0) of its own.
        for (pl2, apcb) in [(0x3_4000usize, 0x60_0000usize), (0x4_0000, 0x61_0000)] {
            let bl2 = [
                BiosEntry::new(
                    bios::RESET_IMAGE,
                    0,
                    0,
                    BiosEntry::RESET | BiosEntry::COPY,
                    IMAGE_SIZE,
                    (1 << 62) | IMAGE as u64,
                    0x7000_0000,
                ),
                BiosEntry::new(bios::APCB, 0, 0, 0, 0x1000, (1 << 62) | apcb as u64, 0),
            ];
            write_table(&mut rom[BANK + pl2 + 0x2000..], BIOS_LEVEL2_COOKIE, 0, &bl2).unwrap();
        }
        let read = |offset: usize, buf: &mut [u8]| {
            buf.copy_from_slice(&rom[offset..offset + buf.len()]);
            true
        };
        let range = |offset, ish| ApcbRange {
            offset,
            len: 0x1000,
            ish,
        };
        assert_eq!(
            apcb_ranges(rom.len(), read).unwrap(),
            [range(0x60_0000, ISH_A), range(0x61_0000, ISH_B)]
        );
        // Both slots naming one APCB: one range, both slots.
        let pl2 = 0x4_0000usize;
        let bl2 = [
            BiosEntry::new(
                bios::RESET_IMAGE,
                0,
                0,
                BiosEntry::RESET | BiosEntry::COPY,
                IMAGE_SIZE,
                (1 << 62) | IMAGE as u64,
                0x7000_0000,
            ),
            BiosEntry::new(bios::APCB, 0, 0, 0, 0x1000, (1 << 62) | 0x60_0000, 0),
        ];
        write_table(&mut rom[BANK + pl2 + 0x2000..], BIOS_LEVEL2_COOKIE, 0, &bl2).unwrap();
        let read = |offset: usize, buf: &mut [u8]| {
            buf.copy_from_slice(&rom[offset..offset + buf.len()]);
            true
        };
        assert_eq!(
            apcb_ranges(rom.len(), read).unwrap(),
            [range(0x60_0000, ISH_A | ISH_B)]
        );
    }

    #[test]
    fn slots_naming_different_images_or_failing_reads_find_none() {
        let rom = flash([IMAGE, IMAGE - 0x10_0000]);
        let read = |offset: usize, buf: &mut [u8]| {
            buf.copy_from_slice(&rom[offset..offset + buf.len()]);
            true
        };
        assert_eq!(
            fetch_reset_image(rom.len(), read),
            Err(Lookup::Image(
                fstart_amdfw_format::Error::ResetImageMismatch
            ))
        );
        let rom = flash([IMAGE, IMAGE]);
        assert_eq!(
            fetch_reset_image(rom.len(), |_, _| false),
            Err(Lookup::Read)
        );
        let erased = std::vec![0xffu8; 64 << 20];
        let read = |offset: usize, buf: &mut [u8]| {
            buf.copy_from_slice(&erased[offset..offset + buf.len()]);
            true
        };
        assert_eq!(fetch_reset_image(erased.len(), read), Err(Lookup::NoEfs));
    }
}
