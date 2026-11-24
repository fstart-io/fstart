// SPDX-License-Identifier: MIT OR Apache-2.0
//! Primary BIOS decode and declared descriptor-layout checks, not identity,
//! authentication, write protection or authorization to reflash a machine.
//! Intel 332691-002 sections 7.2.1, 7.2.3, 7.2.22..27, 7.2.35..36.
//! fast_spi.c SHA256 ee827387485c492bbc726b2f8387a752ba9cbb21db3acf938e3851380bb28f17
//! fast_spi_def.h SHA256 d374e9656d2b89efa314f33f081897a8ebf1e44e918dd6acf426ee0f50b2269b
//! Native IFD 1.5/2 density fields are FOUR bits (unlike old ICH's three).

use super::*;
use core::ops::Range;
use fstart_core::{indexed::Indexed, services::FirmwareImage};
use tock_registers::LocalRegisterCopy;

register_bitfields![u32,
    pub(super) DescriptorSelect [
        INDEX OFFSET(2) NUMBITS(10) [],
        SECTION OFFSET(12) NUMBITS(3) []
    ],
    DescriptorMap [ COMPONENTS OFFSET(8) NUMBITS(2) [] ],
    Component [
        FIRST OFFSET(0) NUMBITS(4) [],
        SECOND OFFSET(4) NUMBITS(4) []
    ]
];
const SELECT_MASK: u32 = 0x7ffc;

// Only the selector is written. This is not the hardware/software sequencer,
// descriptor programming, a flash transfer, or an ownership/drain handshake.
struct Selection<I: Readable<T = u32> + Writeable<T = u32>> {
    index: I,
    saved: u32,
    closed: bool,
}
impl<I: Readable<T = u32> + Writeable<T = u32>> Drop for Selection<I> {
    fn drop(&mut self) {
        if !self.closed {
            self.index.set(self.saved);
        }
    }
}
fn descriptor_words<I, D>(index: I, data: D) -> Result<[u32; 3], Error>
where
    I: Readable<T = u32> + Writeable<T = u32> + Copy,
    D: Readable<T = u32> + Copy,
{
    let saved = index.get();
    if saved & !SELECT_MASK != 0
        || LocalRegisterCopy::<u32, DescriptorSelect::Register>::new(saved)
            .read(DescriptorSelect::SECTION)
            > 3
    {
        return Err(Error::Descriptor);
    }
    let mut restore = Selection {
        index,
        saved,
        closed: false,
    };
    let mut words = [0; 3];
    // Signature/map section DW0/DW1, then component section DW0. INDEX is
    // expressed as a DWORD index by tock, not a byte offset masquerading as one.
    let selections = [
        0,
        DescriptorSelect::INDEX.val(1).value,
        DescriptorSelect::SECTION.val(1).value,
    ];
    for (at, word) in selections.into_iter().zip(&mut words) {
        *word = Indexed::<_, _, ()>::new(index, data, at).get();
        if index.get() != at {
            return Err(Error::Readback);
        }
    }
    restore.index.set(saved);
    if restore.index.get() != saved {
        return Err(Error::Readback);
    }
    restore.closed = true;
    Ok(words)
}

pub(crate) struct Observation {
    image: FirmwareImage,
}
impl Observation {
    /// Owned PCH-H SPI observer, with its entire fixed BAR mapped UC. The BSP
    /// owner excludes other selector users; the native SMM handler accesses
    /// BIOS_CONTROL, never FDOC/FDOD. No stage-global ECAM is consulted.
    pub(crate) fn read(device: &EcamDevice) -> Result<Self, Error> {
        require(device)?;
        if device.memory_bar(0x10) != Some(SPI_BASE as u64)
            || device.try_read16(4).ok_or(Error::Device)? & 2 == 0
        {
            return Err(Error::Decode);
        }
        let bios = device.try_read32(BIOS_CONTROL).ok_or(Error::Device)?;
        if bios == u32::MAX || bios & BiosControl::EXT_ENABLE::SET.value != 0 {
            return Err(Error::Decode);
        }
        let r = unsafe { &*(SPI_BASE as *const Registers) };
        require_idle_descriptor(r.sequencing.get())?;
        // Bind the canonical indexed transport to the existing typed layout;
        // no duplicate offset/data helper or flash-command implementation.
        let index = unsafe {
            MmioRegister::<u32, DescriptorSelect::Register>::new(core::ptr::addr_of!(
                r.descriptor_control
            ) as usize)
        };
        let data =
            unsafe { MmioRegister::<u32>::new(core::ptr::addr_of!(r.descriptor_data) as usize) };
        let [signature, map, component] = descriptor_words(index, data)?;
        let primary = r.primary_region.get();
        let regions = core::array::from_fn(|i| r.regions[i].get());
        require_idle_descriptor(r.sequencing.get())?;
        if r.primary_region.get() != primary
            || r.regions
                .iter()
                .zip(regions)
                .any(|(r, word)| r.get() != word)
            || device.memory_bar(0x10) != Some(SPI_BASE as u64)
            || device.try_read16(4).ok_or(Error::Device)? & 2 == 0
            || device.try_read32(BIOS_CONTROL).ok_or(Error::Device)?
                & BiosControl::EXT_ENABLE::SET.value
                != 0
        {
            return Err(Error::Readback);
        }
        Self::decode(signature, map, component, primary, regions)
    }
    fn decode(
        signature: u32,
        map: u32,
        component: u32,
        primary: u32,
        words: [u32; 6],
    ) -> Result<Self, Error> {
        if signature != 0x0ff0a55a || map == u32::MAX || component == u32::MAX {
            return Err(Error::Descriptor);
        }
        let map = LocalRegisterCopy::<u32, DescriptorMap::Register>::new(map);
        let component = LocalRegisterCopy::<u32, Component::Register>::new(component);
        let density = |value: u32| {
            if value <= 8 {
                Some(0x80000u32 << value)
            } else {
                None
            }
        };
        let first = density(component.read(Component::FIRST)).ok_or(Error::Descriptor)?;
        let capacity = match map.read(DescriptorMap::COMPONENTS) {
            0 => first,
            1 => first
                .checked_add(density(component.read(Component::SECOND)).ok_or(Error::Descriptor)?)
                .ok_or(Error::Descriptor)?,
            _ => return Err(Error::Descriptor),
        };
        if !(0x100000..=0x08000000).contains(&capacity) || !capacity.is_power_of_two() {
            return Err(Error::Descriptor);
        }
        let mut regions: [Option<Range<u32>>; 6] = core::array::from_fn(|_| None);
        for (region, word) in regions.iter_mut().zip(words) {
            *region = decode_region(word, capacity)?;
        }
        if regions[0] != Some(0..0x1000) || regions[5].is_some() {
            return Err(Error::Descriptor);
        }
        for i in 1..regions.len() {
            if let Some(region) = &regions[i] {
                if region.start == 0 {
                    return Err(Error::Descriptor);
                }
                for other in regions[..i].iter().flatten() {
                    if region.start < other.end && other.start < region.end {
                        return Err(Error::Descriptor);
                    }
                }
            }
        }
        // BFPREG bit31 is secondary-region selection. decode_region rejects
        // it, so a secondary image can never be mistaken for FREG1's primary.
        let bios = regions[1].as_ref().ok_or(Error::Descriptor)?;
        if decode_region(primary, capacity)?.as_ref() != Some(bios)
            || bios.end - bios.start > 16 << 20
        {
            return Err(Error::Descriptor);
        }
        let image = FirmwareImage::x86_top_of_4g(u64::from(bios.end - bios.start));
        Ok(Self { image })
    }
    pub(crate) fn image(&self) -> FirmwareImage {
        self.image
    }
}
pub(super) fn require_idle_descriptor(word: u32) -> Result<(), Error> {
    let state = LocalRegisterCopy::<u32, HardwareSequence::Register>::new(word);
    if word == u32::MAX || !state.is_set(HardwareSequence::DESCRIPTOR_VALID) {
        return Err(Error::Descriptor);
    }
    if state.is_set(HardwareSequence::IN_PROGRESS) || state.is_set(HardwareSequence::GO) {
        return Err(Error::Busy);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
