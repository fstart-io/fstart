// SPDX-License-Identifier: MIT OR Apache-2.0
//! The AMD PSP firmware image format (AMD #55758): the embedded firmware structure ([`efs`]) points to
//! the PSP and BIOS directories ([`directory`]). The firmware looks files up
//! in it (the microcode), the host tools pack and check images.
//!
//! Addresses are the physical addresses of the flash mapped below 4 GiB, the
//! address mode of the Family 17h images.
#![no_std]

pub mod directory;
pub mod efs;

use directory::{BiosEntry, Levels};
use efs::{BiosDirectorySlot, Efs};
use zerocopy::FromBytes;

/// A flash image, mapped so that it ends at 4 GiB.
#[derive(Clone, Copy)]
pub struct Flash<'a> {
    bytes: &'a [u8],
}

impl<'a> Flash<'a> {
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    fn base(&self) -> u64 {
        (1u64 << 32).wrapping_sub(self.bytes.len() as u64)
    }

    /// The mapped address of flash `offset`.
    pub fn address(&self, offset: u32) -> u64 {
        self.base() + u64::from(offset)
    }

    /// The flash offset of `address`. Below the mapping, vendor images use
    /// plain flash offsets.
    pub fn offset(&self, address: u64) -> Option<usize> {
        usize::try_from(address.checked_sub(self.base()).unwrap_or(address)).ok()
    }

    /// The `size` bytes at `address`, if they are in the image.
    pub fn get(&self, address: u64, size: u32) -> Option<&'a [u8]> {
        let start = self.offset(address)?;
        self.bytes.get(start..start.checked_add(size as usize)?)
    }

    /// The EFS and its offset, at the first offset the PSP probes.
    pub fn efs(&self) -> Option<(usize, &'a Efs)> {
        efs::PROBES.iter().find_map(|&probe| {
            let offset = (self.bytes.len() + probe as usize).checked_sub(efs::WINDOW as usize)?;
            let (efs, _) = Efs::ref_from_prefix(self.bytes.get(offset..)?).ok()?;
            (efs.signature.get() == efs::SIGNATURE).then_some((offset, efs))
        })
    }

    /// The BIOS directories the PSP of `slot` boots from.
    pub fn bios_directories(&self, slot: BiosDirectorySlot) -> Option<Levels<'a, BiosEntry>> {
        let (_, efs) = self.efs()?;
        Levels::read(*self, self.offset(efs.bios_directory(slot)?)?)
    }

    /// The files of `kind` in the BIOS directory the PSP boots from.
    pub fn bios_files(
        &self,
        slot: BiosDirectorySlot,
        kind: u8,
    ) -> impl Iterator<Item = &'a [u8]> + use<'a> {
        let flash = *self;
        self.bios_directories(slot)
            .into_iter()
            .flat_map(move |levels| levels.boot().files(flash))
            .filter(move |(entry, _)| entry.kind == kind)
            .map(|(_, data)| data)
    }
}

#[cfg(test)]
mod tests;
