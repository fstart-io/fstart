// SPDX-License-Identifier: MIT OR Apache-2.0
//! A-Link/B-Link configuration (ABCFG) through the AB index/data pair at
//! `CD8h`/`CDCh`, and the A-Link + clock-gating env task af51 (FCH_SP3
//! §3.2).
//!
//! The PMxE0 decode of the pair is the caller's (ResetAbBar). A remote
//! FCH's ABCFG is only reachable while the DF routes legacy I/O to it
//! ([`super::multi`]).
use super::FchHw;
use super::acpimmio::MISC;
use crate::cpu::Silicon;
use crate::fch::acpimmio::PM_ISA_CONTROL;
use crate::table::{self, Record};
use crate::window::{At, Reg, Window, update};
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_bitfields,
};

/// The index/data pair.
pub const INDEX: u16 = 0xcd8;
pub const DATA: u16 = 0xcdc;

register_bitfields![u32,
    /// A-Link index: register space in [31:29].
    AbIndex [
        ADDRESS OFFSET(0) NUMBITS(29) [],
        SPACE OFFSET(29) NUMBITS(3) [Abcfg = 6]
    ]
];

/// A record of an ABCFG table ([`crate::table`]); the tables are
/// vendor values.
pub type Rmw = Record<u32, u32>;

/// ABCFG register `register`. Every access selects it and clears the index
/// afterwards (RPR 4.2: required for the 0x100NN registers).
pub struct Abcfg<'a, W> {
    io: &'a W,
    register: u32,
}
/// ABCFG register `register` through the I/O window `io`.
pub fn abcfg<W: Window>(io: &W, register: u32) -> Abcfg<'_, W> {
    Abcfg { io, register }
}
impl<W: Window> Abcfg<'_, W> {
    fn access<T>(&self, f: impl FnOnce(&Reg<&W, u32>) -> T) -> T {
        let index = self.io.at(At::<u32, AbIndex::Register>::new(INDEX.into()));
        index.write(AbIndex::SPACE::Abcfg + AbIndex::ADDRESS.val(self.register));
        let value = f(&self.io.at(At::new(DATA.into())));
        index.set(0);
        value
    }
}
impl<W: Window> Readable for Abcfg<'_, W> {
    type T = u32;
    type R = ();
    fn get(&self) -> u32 {
        self.access(|data| data.get())
    }
}
impl<W: Window> Writeable for Abcfg<'_, W> {
    type T = u32;
    type R = ();
    fn set(&self, value: u32) {
        self.access(|data| data.set(value))
    }
}

/// Apply an ABCFG table in order.
pub fn apply(io: &impl Window, records: &[Rmw]) {
    table::apply(records.iter().copied(), |register| abcfg(io, register));
}

pub fn read<I: Window>(io: &I, r: u32) -> u32 {
    abcfg(io, r).get()
}

pub fn write<I: Window>(io: &I, r: u32, value: u32) {
    abcfg(io, r).set(value)
}

/// `x = (x & !clear) | set` on ABCFG register `r`.
pub fn modify<I: Window>(io: &I, r: u32, clear: u32, set: u32) {
    let x = abcfg(io, r);
    x.set((x.get() & !clear) | set);
}

/// ABCFG 0x10050 (MISCCTL_10050).
pub const MISC_CTL: u32 = 0x10050;
/// MISCCTL_10050\[2\]: reset the CPU on a sync flood (am4-re OPEN_ITEMS).
pub const RESET_CPU_ON_SYNC_FLOOD: u32 = 1 << 2;

/// Policy of af51; `default_for` gives the donor default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbPolicy {
    /// MISCCTL_10050\[2\]: ZP and SSP set it, the Milan donor clears it.
    pub reset_cpu_on_sync_flood: bool,
}

impl AbPolicy {
    pub const fn default_for(generation: Silicon) -> Self {
        Self {
            reset_cpu_on_sync_flood: !matches!(generation, Silicon::Gn),
        }
    }
}

/// Program the A-Link table and the clock gating of the primary FCH
/// (af51). `hw.io()` must reach this FCH's AB ports.
pub fn clock_gating<H: FchHw>(hw: &H, generation: Silicon, policy: &AbPolicy) {
    program(hw, generation, Some(policy));
}

/// The same program on a Rome/Milan socket-1 FCH (FchHuashanMultiFchPei,
/// FCH_SP3 §4.2): no MISCx40 write, no ABCFG 0x94 row, and sync-flood
/// reset left off (10050\[2\] cleared at the end). `hw.io()` must be routed
/// to socket 1 ([`super::multi::with_legacy_io`]).
pub fn clock_gating_secondary<H: FchHw>(hw: &H, generation: Silicon) {
    program(hw, generation, None);
    modify(hw.io(), MISC_CTL, RESET_CPU_ON_SYNC_FLOOD, 0);
}

fn program<H: FchHw>(hw: &H, generation: Silicon, primary: Option<&AbPolicy>) {
    let (io, acpi) = (hw.io(), hw.acpi());
    let huashan = generation != Silicon::Zp;
    if primary.is_some() {
        update::<u8, _>(acpi, MISC + 0x40, 0x02, 0);
    }
    super::tables::ALINK_TABLE
        .iter()
        .filter(|row| primary.is_some() || row.0 != 0x94)
        .for_each(|&(r, clear, set)| modify(io, r, clear, set));
    // MISCCTL_10050: bit 3 is cleared on Huashan (also in af50), bit 2 is
    // the sync-flood policy.
    if let Some(policy) = primary {
        let flood = if policy.reset_cpu_on_sync_flood {
            RESET_CPU_ON_SYNC_FLOOD
        } else {
            0
        };
        modify(
            io,
            MISC_CTL,
            RESET_CPU_ON_SYNC_FLOOD | if huashan { 1 << 3 } else { 0 },
            flood,
        );
    }
    // ABCFG 0x54/0x10054: DMA write sizes (`AbDmaMemoryWrtie3264B`); ZP
    // also sets [23:16] = 0x10 (sp4r2 PCD 0x121 path, taken by the SP3 ZP
    // donor).
    if !huashan {
        modify(io, 0x00054, 0xff << 16, 0x10 << 16);
        modify(io, 0x10054, 0xff << 16, 0x10 << 16);
    }
    modify(io, 0x00054, 0, 0x10);
    modify(io, 0x00054, 0, 1 << 24);
    modify(io, 0x10054, 0, 3 << 24);
    modify(io, 0x00054, 1, 0);
    modify(io, 0x00054, 0, 1 << 2);
    // Memory power saving.
    update::<u8, _>(acpi, MISC + 0x68, 0x04, 0);
    modify(io, 0x000bc, 0, 0x10);
    modify(io, 0x00058, 0, 1 << 29);
    modify(io, 0x00058, 0, 1 << 31);
    update::<u8, _>(acpi, MISC + 0x2e, 0, 0x01);
    update::<u8, _>(acpi, MISC + 0x2e, 0, 0x02);
    if huashan {
        // SMN 02D0001C[31] next to the FCH relocatable block (FCH_SP3 §10:
        // unnamed, inferred clock gate).
        update::<u32, _>(&hw.smn_window(), 0x02D0_001C, 0, 1 << 31);
    }
    // PMx04[ABClkGateEn].
    update::<u8, _>(acpi, PM_ISA_CONTROL.offset + 2, 0, 0x01);
    update::<u8, _>(acpi, MISC + 0x68, 0x02, 0);
    // SBG memory power saving and clock gating (ABCFG 0x208/0x238/0x228).
    modify(io, 0x00208, 0, 0x280);
    if huashan {
        modify(io, 0x00238, 0, 0x400);
        modify(io, 0x00208, 0, 0x43_8000);
        modify(io, 0x00228, 0, 0x0a);
    } else {
        modify(io, 0x00208, 0, 0x40_8010);
    }
    // Policy bits left 0 at the donor defaults (sp4r2 FCH_TAISHAN §3.2).
    modify(io, 0x00180, 1, 0);
    modify(io, 0x00184, 1 << 2, 0);
    modify(io, 0x00180, 0x1f << 8, 0);
    modify(io, 0x00180, 1 << 5, 0);
    modify(io, 0x0022c, 1 << 11, 0);
    modify(io, 0x0022c, 1 << 14, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::model::fch;

    fn ab(io: &crate::fch::model::Mem, r: u32) -> u32 {
        io.at(DATA.into(), 6 << 29 | r)
    }

    #[test]
    fn rome_results_match_the_contract() {
        let hw = fch();
        clock_gating(&hw, Silicon::Ssp, &AbPolicy::default_for(Silicon::Ssp));
        // FCH_SP3 §3.2 af51 results at the Rome default.
        assert_eq!(ab(&hw.io, 0x208), 0x43_8280);
        assert_eq!(ab(&hw.io, 0x238), 0x400);
        assert_eq!(ab(&hw.io, 0x228), 0x0a);
        assert_eq!(ab(&hw.io, 0x10050), 0x04);
        assert_eq!(ab(&hw.io, 0x10054), 0x0300_07ff);
        assert_eq!(ab(&hw.io, 0x54), 0x0100_0014);
        assert_eq!(ab(&hw.io, 0x58), 0xa000_0000);
        assert_eq!(ab(&hw.io, 0x80), 0x6_0001);
        assert_eq!(crate::fch::model::smn(&hw.pci, 0, 0x02d0_001c), 1 << 31);
        assert_eq!(hw.acpi.get(crate::fch::acpimmio::PM + 6, 1), 1);
    }

    #[test]
    fn naples_results_match_the_contract() {
        let naples = fch();
        clock_gating(&naples, Silicon::Zp, &AbPolicy::default_for(Silicon::Zp));
        // ZP: 208 = 408290, 10054 = 31007FF, 54 = 1100014 (FCH_SP3 §3.2).
        assert_eq!(ab(&naples.io, 0x208), 0x40_8290);
        assert_eq!(ab(&naples.io, 0x10054), 0x0310_07ff);
        assert_eq!(ab(&naples.io, 0x54), 0x0110_0014);
    }
}
