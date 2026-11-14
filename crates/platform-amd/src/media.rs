// SPDX-License-Identifier: MIT OR Apache-2.0
//! The boot media of a PSP-booted stage: the FFS in the boot flash, verified
//! with the policy the image builder patched into the stage. Every AMD flow
//! mounts it the same way; what differs is where the firmware ([`Layout`])
//! and the FFS are.
use crate::layout::{Layout, Region};
use alloc::vec::Vec;
use fstart_core::services::memory_detect::{E820Entry, E820Kind};
use fstart_stage::boot::{Codec, MemoryPolicy, MemoryWindow};

/// The flash copy of the reset image the PSP loaded (BIOS directory entry
/// 0x62) in `flash`, the boot ROM's view of the booted 16 MiB, found
/// through the BIOS directory of `slot`.
pub fn reset_image(
    flash: &'static [u8],
    slot: fstart_amdfw_format::efs::BiosDirectorySlot,
) -> Option<&'static [u8]> {
    use fstart_amdfw_format::{Flash, Profile, ROM2_BASE, directory::bios};
    let profile = Profile::for_slot(slot);
    Flash::region(flash, 0, ROM2_BASE, profile)
        .bios_directories_checked(profile.bios)
        .ok()?
        .find(bios::RESET_IMAGE, None, None)
        .and_then(|(dir, entry)| dir.data(entry).ok())
}

/// The step of [`mount`] that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountError {
    LoadPolicy,
    Locator,
    Size,
    Mount,
    Verify,
}

impl MountError {
    /// The step's name, for the log.
    pub const fn step(self) -> &'static str {
        match self {
            Self::LoadPolicy => "load policy",
            Self::Locator => "FFS locator",
            Self::Size => "FFS size",
            Self::Mount => "FFS mount",
            Self::Verify => "FFS verify",
        }
    }
}

/// What loads may write and what they may not: the RAM of `e820`, without
/// the `firmware`'s own windows (the running image, its page tables, BSS,
/// heap and stack) and the `media`.
fn windows(
    e820: &[E820Entry],
    firmware: &[MemoryWindow],
    media: MemoryWindow,
) -> Option<(Vec<MemoryWindow>, Vec<MemoryWindow>)> {
    let ram = |entry: &&E820Entry| entry.kind == E820Kind::Ram as u32;
    let window = |entry: &E820Entry| MemoryWindow {
        start: entry.addr,
        size: entry.size,
    };
    let mut writable = Vec::new();
    let mut reserved = Vec::new();
    writable.try_reserve_exact(e820.len()).ok()?;
    reserved
        .try_reserve_exact(e820.len() + firmware.len() + 1)
        .ok()?;
    writable.extend(e820.iter().filter(ram).map(window));
    reserved.extend(
        e820.iter()
            .filter(|entry| !ram(entry))
            .map(window)
            .chain(firmware.iter().copied())
            .chain([media]),
    );
    Some((writable, reserved))
}

/// Publish the load policy and mount and authenticate the FFS at `ffs`:
/// loads may only go to the RAM of `e820` outside the firmware's DRAM
/// (`layout.early`: the running image, its page tables, BSS, heap, stack
/// and store) and the media. Payload files are decoded with the codec of
/// the platform's `compression::PAYLOAD`.
pub fn mount(e820: &[E820Entry], layout: &Layout, ffs: Region) -> Result<(), MountError> {
    let window = |region: Region| MemoryWindow {
        start: region.base,
        size: region.size,
    };
    let (writable, reserved) =
        windows(e820, &[window(layout.early)], window(ffs)).ok_or(MountError::LoadPolicy)?;
    let policy = MemoryPolicy {
        writable: &writable,
        reserved: &reserved,
        entry_alignment: 1,
    };
    let codec: &'static dyn Codec =
        const { fstart_stage::boot::codec(crate::compression::PAYLOAD) };
    // SAFETY: discovered RAM only, with the firmware's windows (the live
    // stage within them) and the media excluded; no device has DMA buffers
    // outside the firmware's early region.
    unsafe { fstart_stage::directory::set_load_policy(&policy, codec) }
        .map_err(|_| MountError::LoadPolicy)?;
    let locator = fstart_stage::anchor::media_locator().ok_or(MountError::Locator)?;
    if !locator.validate(ffs.size) {
        return Err(MountError::Locator);
    }
    let ffs = fstart_stage::fixed_helpers::MemoryMappedFfs::new(
        ffs.base,
        usize::try_from(locator.image_size).map_err(|_| MountError::Size)?,
    );
    ffs.mount().map_err(|_| MountError::Mount)?;
    ffs.verify().map_err(|_| MountError::Verify)
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn entry(addr: u64, size: u64, kind: E820Kind) -> E820Entry {
        E820Entry {
            addr,
            size,
            kind: kind as u32,
        }
    }

    #[test]
    fn loads_get_the_ram_without_the_firmware_and_the_media() {
        let e820 = [
            entry(0, 0x1000, E820Kind::Reserved),
            entry(0x1000, 0x9e000, E820Kind::Ram),
            entry(0x10_0000, 0x7ff0_0000, E820Kind::Ram),
            entry(0xe000_0000, 0x1000_0000, E820Kind::Reserved),
        ];
        let window = |start, size| MemoryWindow { start, size };
        let firmware = [window(0x7600_0000, 0x100_0000), window(0x7700_0000, 0x1000)];
        let (writable, reserved) =
            windows(&e820, &firmware, window(0xff00_0000, 0x100_0000)).unwrap();
        let spans = |list: &[MemoryWindow]| -> Vec<(u64, u64)> {
            list.iter().map(|w| (w.start, w.size)).collect()
        };
        assert_eq!(
            spans(&writable),
            [(0x1000, 0x9e000), (0x10_0000, 0x7ff0_0000)]
        );
        assert_eq!(
            spans(&reserved),
            [
                (0, 0x1000),
                (0xe000_0000, 0x1000_0000),
                (0x7600_0000, 0x100_0000),
                (0x7700_0000, 0x1000),
                (0xff00_0000, 0x100_0000),
            ]
        );
    }

    #[test]
    fn errors_name_their_step() {
        assert_eq!(MountError::LoadPolicy.step(), "load policy");
        assert_eq!(MountError::Verify.step(), "FFS verify");
    }
}
