// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Picasso Data Fabric sequencing.
use super::registers::{BROADCAST, Fabric};
use tock_registers::{LocalRegisterCopy, register_bitfields};

register_bitfields![u32,
    MmioControl [READ OFFSET(0) NUMBITS(1) [], WRITE OFFSET(1) NUMBITS(1) [],
        DESTINATION OFFSET(4) NUMBITS(8) [], NON_POSTED OFFSET(12) NUMBITS(1) []],
    IoBase [READ OFFSET(0) NUMBITS(1) [], WRITE OFFSET(1) NUMBITS(1) [],
        BASE OFFSET(12) NUMBITS(13) []],
    IoLimit [DESTINATION OFFSET(0) NUMBITS(8) [], LIMIT OFFSET(12) NUMBITS(13) []]
];
const IOMS: u32 = 9;
const IOAPIC: u32 = 0xfec0_0000;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    MemoryBoundary,
    NoMmioWindow,
    NoIoWindow,
}

/// Route peripheral MMIO and I/O to IOMS, preserving PSP-established windows.
/// Discover and validate BOTH free windows before changing either of them.
pub fn ensure_legacy_routes(fabric: &mut impl Fabric, low_top: u32) -> Result<(), Error> {
    if low_top == 0 || low_top >= IOAPIC || low_top & 0xffff != 0 {
        return Err(Error::MemoryBoundary);
    }
    let (mut free_mmio, mut free_io) = (None, None);
    let (mut have_mmio, mut have_io) = (false, false);
    for i in 0..8 {
        let control = LocalRegisterCopy::<u32, MmioControl::Register>::new(fabric.read(
            0,
            0x208 + i * 16,
            BROADCAST,
        ));
        if control.get() & 3 == 0 && free_mmio.is_none() {
            free_mmio = Some(i);
        }
        if control.get() & 3 != 3
            || control.is_set(MmioControl::NON_POSTED)
            || control.read(MmioControl::DESTINATION) != IOMS
        {
            continue;
        }
        let base = u64::from(fabric.read(0, 0x200 + i * 16, BROADCAST)) << 16;
        let end = (u64::from(fabric.read(0, 0x204 + i * 16, BROADCAST)) + 1) << 16;
        have_mmio |= base <= u64::from(low_top) && end >= u64::from(IOAPIC);
    }
    for i in 0..8 {
        let base = LocalRegisterCopy::<u32, IoBase::Register>::new(fabric.read(
            0,
            0xc0 + i * 8,
            BROADCAST,
        ));
        let limit = LocalRegisterCopy::<u32, IoLimit::Register>::new(fabric.read(
            0,
            0xc4 + i * 8,
            BROADCAST,
        ));
        if base.get() & 3 == 0 && free_io.is_none() {
            free_io = Some(i);
        }
        have_io |= base.get() & 3 == 3
            && base.read(IoBase::BASE) == 0
            && limit.read(IoLimit::DESTINATION) == IOMS
            && limit.read(IoLimit::LIMIT) >= 0xf;
    }
    if !have_mmio && free_mmio.is_none() {
        return Err(Error::NoMmioWindow);
    }
    if !have_io && free_io.is_none() {
        return Err(Error::NoIoWindow);
    }
    if !have_mmio {
        let i = free_mmio.ok_or(Error::NoMmioWindow)?;
        fabric.write(0, 0x208 + i * 16, BROADCAST, 0);
        fabric.write(0, 0x200 + i * 16, BROADCAST, low_top >> 16);
        fabric.write(0, 0x204 + i * 16, BROADCAST, (IOAPIC - 1) >> 16);
        fabric.write(
            0,
            0x208 + i * 16,
            BROADCAST,
            (MmioControl::READ::SET + MmioControl::WRITE::SET + MmioControl::DESTINATION.val(IOMS))
                .value,
        );
    }
    if !have_io {
        let i = free_io.ok_or(Error::NoIoWindow)?;
        fabric.write(
            0,
            0xc4 + i * 8,
            BROADCAST,
            (IoLimit::DESTINATION.val(IOMS) + IoLimit::LIMIT.val(0xf)).value,
        );
        fabric.write(
            0,
            0xc0 + i * 8,
            BROADCAST,
            (IoBase::READ::SET + IoBase::WRITE::SET).value,
        );
    }
    fabric.write(0, 0x80, BROADCAST, (IOMS << 4) | 1);
    Ok(())
}

/// Only changes DF coordination registers; never rewrites PSP-trained UMC data.
pub fn early_init(fabric: &mut impl Fabric) {
    fabric.write(0, 0x3f0, BROADCAST, 1);
    for instance in 0..2 {
        fabric.update(2, 0x040, instance, 0, 1 << 30);
        fabric.update(2, 0x044, instance, 1 << 26, 0);
        fabric.update(2, 0x050, instance, 0, 1 << 5);
        fabric.update(2, 0x048, instance, 0, 0x8040);
    }
    fabric.write(2, 0x068, BROADCAST, 0x65019);
    fabric.write(2, 0x06c, BROADCAST, 0x14060619);
    for instance in 0..2 {
        if fabric.read(0, 0x110, instance) & 1 == 0 {
            fabric.update(2, 0x048, instance, 1 << 19, 0);
        }
    }
    fabric.update(2, 0x050, BROADCAST, 1, 0);
    fabric.write(4, 0x0c4, 2, 0x20001);
    fabric.write(4, 0x200, BROADCAST, 0x1020000);
    fabric.update(3, 0x104, 2, 0, 1 << 28);
    fabric.update(3, 0x108, 2, 0, 1 << 6);
    fabric.write(3, 0x20c, BROADCAST, 3);
    let channels = (0..2)
        .filter(|&instance| {
            let first = fabric.read(0, 0x110, instance);
            let second = fabric.read(0, 0x118, instance);
            (first | second) & 1 != 0
        })
        .count();
    fabric.update(
        4,
        0x0c0,
        6,
        0xff0,
        if channels == 2 { 0x480 } else { 0x300 },
    );
    fabric.update(3, 0x04c, 9, 0, 1 << 5);
    fabric.update(3, 0x300, 9, 0, 1 << 8);
    let instances = fabric.read(0, 0x040, BROADCAST) & 0xff;
    for instance in 0..instances {
        for offset in [0x1f0, 0x1f4] {
            if fabric.read(6, offset, instance as u8) != 0 {
                // DF error status uses a zero write, not GPIO-style W1C.
                fabric.write(6, offset, instance as u8, 0);
            }
        }
    }
    fabric.update(0, 0x3f8, BROADCAST, 1 << 3, 0);
}

pub fn late_init(fabric: &mut impl Fabric, raven2: bool) {
    fabric.update(
        5,
        0x300,
        BROADCAST,
        (1 << 5) | if raven2 { 1 << 11 } else { 0 },
        if raven2 { 0x3300 } else { 0x3b00 },
    );
    fabric.update(5, 0x338, BROADCAST, 0, 1 << 3);
    fabric.update(5, 0x364, BROADCAST, 0, 1 << 1);
    fabric.update(5, 0x348, BROADCAST, 0x7f7f7f7f, 0x14171710);
    fabric.update(5, 0x34c, BROADCAST, 0x00ff0000, 0x00590000);
    fabric.update(5, 0x360, BROADCAST, 1 << 21, u32::from(raven2));
    if raven2 {
        fabric.update(5, 0x350, BROADCAST, 0xff000000, 0x0e000000);
    }
}
