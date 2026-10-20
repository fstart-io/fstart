// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Picasso Data Fabric sequencing.
use super::registers::{Df, DfReg, DfRecord, apply_df, df_rmw, df_write};
use crate::fam17h::df::{
    IO_PAIRS, IoBase, IoLimit, MMIO_PAIRS, MmioCtrl, VGA_EN, VgaEn, io_base, io_limit, mmio_base,
    mmio_ctrl, mmio_limit,
};
use fstart_pci::ConfigRegionAccess;
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_bitfields,
};

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
pub fn ensure_legacy_routes(fabric: &Df<impl ConfigRegionAccess>, low_top: u32) -> Result<(), Error> {
    if low_top == 0 || low_top >= IOAPIC || low_top & 0xffff != 0 {
        return Err(Error::MemoryBoundary);
    }
    let enabled = |base: u32| base & 3 == 3;
    let (mut free_mmio, mut free_io) = (None, None);
    let (mut have_mmio, mut have_io) = (false, false);
    for i in 0..MMIO_PAIRS {
        let control = fabric.reg(mmio_ctrl(i)).extract();
        if control.get() & 3 == 0 && free_mmio.is_none() {
            free_mmio = Some(i);
        }
        if !enabled(control.get())
            || control.is_set(MmioCtrl::NON_POSTED)
            || control.read(MmioCtrl::DST_FABRIC_ID) != IOMS
        {
            continue;
        }
        let base = u64::from(fabric.reg(mmio_base(i)).get()) << 16;
        let end = (u64::from(fabric.reg(mmio_limit(i)).get()) + 1) << 16;
        have_mmio |= base <= u64::from(low_top) && end >= u64::from(IOAPIC);
    }
    for i in 0..IO_PAIRS {
        let base = fabric.reg(io_base(i)).extract();
        let limit = fabric.reg(io_limit(i)).extract();
        if base.get() & 3 == 0 && free_io.is_none() {
            free_io = Some(i);
        }
        have_io |= enabled(base.get())
            && base.read(IoBase::BASE) == 0
            && limit.read(IoLimit::DST_FABRIC_ID) == IOMS
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
        fabric.reg(mmio_ctrl(i)).set(0);
        fabric.reg(mmio_base(i)).set(low_top >> 16);
        fabric.reg(mmio_limit(i)).set((IOAPIC - 1) >> 16);
        fabric.reg(mmio_ctrl(i)).write(
            MmioCtrl::RE::SET + MmioCtrl::WE::SET + MmioCtrl::DST_FABRIC_ID.val(IOMS),
        );
    }
    if !have_io {
        let i = free_io.ok_or(Error::NoIoWindow)?;
        fabric.reg(io_limit(i))
            .write(IoLimit::DST_FABRIC_ID.val(IOMS) + IoLimit::LIMIT.val(0xf));
        fabric.reg(io_base(i))
            .write(IoBase::RE::SET + IoBase::WE::SET);
    }
    fabric.reg(VGA_EN)
        .write(VgaEn::DST_FABRIC_ID.val(IOMS) + VgaEn::ENABLE::SET);
    Ok(())
}

register_bitfields![u32,
    DramBase [VALID OFFSET(0) NUMBITS(1) []],
    InstanceCount [COUNT OFFSET(0) NUMBITS(8) []]
];
const F0_INSTANCE_COUNT: DfReg<InstanceCount::Register> = DfReg::new(0, 0x040);
const fn f0_dram_base(i: u16) -> DfReg<DramBase::Register> {
    DfReg::new(0, 0x110 + 8 * i)
}
const F0_CLK_GATE: DfReg = DfReg::new(0, 0x3f0);
const F0_RAS_CTRL: DfReg = DfReg::new(0, 0x3f8);
const F2_048: DfReg = DfReg::new(2, 0x048);
const F2_050: DfReg = DfReg::new(2, 0x050);
const F6_ERR_STATUS: [DfReg; 2] = [DfReg::new(6, 0x1f0), DfReg::new(6, 0x1f4)];

/// The vendor UMC coordination values: their meaning isn't known.
const EARLY_CS: [DfRecord; 4] = [
    df_rmw(DfReg::new(2, 0x040), None, 0, 1 << 30),
    df_rmw(DfReg::new(2, 0x044), None, 1 << 26, 0),
    df_rmw(F2_050, None, 0, 1 << 5),
    df_rmw(F2_048, None, 0, 0x8040),
];
const fn on(record: DfRecord, instance: u8) -> DfRecord {
    DfRecord {
        instance: Some(instance),
        ..record
    }
}

/// Only changes DF coordination registers; never rewrites PSP-trained UMC data.
pub fn early_init(fabric: &Df<impl ConfigRegionAccess>) {
    apply_df(fabric, [df_write(F0_CLK_GATE, None, 1)]);
    apply_df(
        fabric,
        (0..2).flat_map(|instance| EARLY_CS.map(|record| on(record, instance))),
    );
    apply_df(
        fabric,
        [
            df_write(DfReg::new(2, 0x068), None, 0x65019),
            df_write(DfReg::new(2, 0x06c), None, 0x14060619),
        ],
    );
    for instance in 0..2 {
        if !fabric.instance(f0_dram_base(0), instance).is_set(DramBase::VALID) {
            apply_df(fabric, [df_rmw(F2_048, Some(instance), 1 << 19, 0)]);
        }
    }
    apply_df(
        fabric,
        [
            df_rmw(F2_050, None, 1, 0),
            df_write(DfReg::new(4, 0x0c4), Some(2), 0x20001),
            df_write(DfReg::new(4, 0x200), None, 0x1020000),
            df_rmw(DfReg::new(3, 0x104), Some(2), 0, 1 << 28),
            df_rmw(DfReg::new(3, 0x108), Some(2), 0, 1 << 6),
            df_write(DfReg::new(3, 0x20c), None, 3),
        ],
    );
    let channels = (0..2)
        .filter(|&instance| {
            let first = fabric.instance(f0_dram_base(0), instance).get();
            let second = fabric.instance(f0_dram_base(1), instance).get();
            (first | second) & 1 != 0
        })
        .count();
    apply_df(
        fabric,
        [
            df_rmw(
                DfReg::new(4, 0x0c0),
                Some(6),
                0xff0,
                if channels == 2 { 0x480 } else { 0x300 },
            ),
            df_rmw(DfReg::new(3, 0x04c), Some(9), 0, 1 << 5),
            df_rmw(DfReg::new(3, 0x300), Some(9), 0, 1 << 8),
        ],
    );
    let instances = fabric.reg(F0_INSTANCE_COUNT).read(InstanceCount::COUNT);
    for instance in 0..instances {
        for status in F6_ERR_STATUS {
            let status = fabric.instance(status, instance as u8);
            if status.get() != 0 {
                // DF error status uses a zero write, not GPIO-style W1C.
                status.set(0);
            }
        }
    }
    apply_df(fabric, [df_rmw(F0_RAS_CTRL, None, 1 << 3, 0)]);
}

/// The vendor PIE power values: their meaning isn't known.
pub fn late_init(fabric: &Df<impl ConfigRegionAccess>, raven2: bool) {
    let pie = |offset| DfReg::new(5, offset);
    apply_df(
        fabric,
        [
            df_rmw(
                pie(0x300),
                None,
                (1 << 5) | if raven2 { 1 << 11 } else { 0 },
                if raven2 { 0x3300 } else { 0x3b00 },
            ),
            df_rmw(pie(0x338), None, 0, 1 << 3),
            df_rmw(pie(0x364), None, 0, 1 << 1),
            df_rmw(pie(0x348), None, 0x7f7f7f7f, 0x14171710),
            df_rmw(pie(0x34c), None, 0x00ff0000, 0x00590000),
            df_rmw(pie(0x360), None, 1 << 21, u32::from(raven2)),
        ],
    );
    if raven2 {
        apply_df(fabric, [df_rmw(pie(0x350), None, 0xff000000, 0x0e000000)]);
    }
}
