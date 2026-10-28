//! Rome (SSP) fabric init, AmdFabricSspPei 1.0.0.L (SOC_DF §3.6, IC).
//!
//! Order: clock gating → CS → CCM (F4xC8) → PIE RAS → scrubber.

use fstart_pci::ConfigRegionAccess;
use tock_registers::interfaces::{ReadWriteable, Writeable};

use super::Df;
use super::topology::Topology;
use super::v3::{self, CCM_F4XC8, ChannelCtl, RasFields, SCRUB_BASE, SCRUB_LIMIT, SocketMemory};
use crate::cpu::Revision;

/// Rome policy; `Default` is the donor (rome-s8030-v404) default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// F0x3F8 fields: PCD 0xC3 (keep), 0xC4 (0xFF → 6); bit 3 cleared.
    pub ras: RasFields,
    /// Scrub period in hours, 0 = off (PCD 0x25: 0xFF → 24 h).
    pub scrub_hours: u8,
    /// F6xB8 controls (PCD 0xC0, 0xC1 TRUE; 0xC2 0xFF → mode 0).
    pub scrub_channel: ChannelCtl,
    /// PCD 0xC5 (TRUE): rate index capped at 3, F2x228 rate field 0.
    pub scrub_rate_cap: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            ras: RasFields::default(),
            scrub_hours: 24,
            scrub_channel: ChannelCtl {
                ctl0: true,
                ctl1: true,
                mode: ChannelCtl::mode_from_option(0xFF),
            },
            scrub_rate_cap: true,
        }
    }
}

/// PEI fabric init on every socket. `memory[socket]` from the APOB.
pub fn init<P: ConfigRegionAccess + Clone>(
    df: &Df<P>,
    topology: &Topology,
    revision: Revision,
    policy: &Policy,
    memory: &[SocketMemory],
) {
    let sockets = topology.sockets;
    // IDS hook 0x20000005 has no handler: the value stays 1.
    v3::clock_gating(df, sockets, 1);
    v3::cs_init(df, sockets);
    for s in 0..sockets {
        // SAFETY: F4xC8 of an existing node.
        unsafe { df.node(s, CCM_F4XC8) }.set(0x0702_0000);
    }
    v3::pie_ras(df, sockets, policy.ras, true);
    for (s, mem) in (0..sockets).zip(memory) {
        scrub(df, s, revision, policy, mem);
    }
}

/// Scrubber of one socket (`ffdc9eec`).
fn scrub<P: ConfigRegionAccess + Clone>(
    df: &Df<P>,
    socket: u8,
    revision: Revision,
    policy: &Policy,
    mem: &SocketMemory,
) {
    let size = v3::rome_scrub_bytes(df, socket);
    if size == 0 || mem.present == 0 {
        return;
    }
    let ctl = policy.scrub_channel;
    if ctl.ctl0 || ctl.ctl1 {
        v3::scrub_channels(df, socket, mem.channels, mem.present, ctl);
    }
    if policy.scrub_hours == 0 {
        return;
    }
    let rate = v3::scrub_rate(policy.scrub_hours, size, 6);
    let rate = if policy.scrub_rate_cap {
        rate.min(3)
    } else {
        rate
    };
    // SAFETY: scrubber registers of an existing node; rate before enable.
    unsafe {
        df.node(socket, v3::SCRUB_LIMIT)
            .modify(SCRUB_LIMIT::RATE.val(u32::from(rate)));
        df.node(socket, v3::SCRUB_BASE)
            .modify(SCRUB_BASE::EN::SET + SCRUB_BASE::REGION_EN::SET);
    }
    if revision == Revision::SspB0 {
        v3::cs_f2x228(df, socket, mem.channels, rate, policy.scrub_rate_cap);
    }
}

#[cfg(test)]
mod tests {
    use super::super::topology::{discover, tests::df3_system};
    use super::*;
    use crate::sp3::soc::Generation;

    #[test]
    fn init_rome_1p() {
        let m = df3_system(1, 0b1111);
        let df = Df::new(m.clone());
        let t = discover(&df, Generation::Rome).unwrap();
        // 256 GiB behind the scrubber, no hole rule.
        m.set(0, None, 6, 0x44, ((256 << 10) - 1) << 4);
        m.set(0, Some(3), 6, 0xB8, 0xF);
        m.set(0, Some(1), 2, 0x228, 0xFFFF_FFFF);
        let mem = SocketMemory {
            channels: 8,
            present: 0b0000_0101,
        };
        init(&df, &t, Revision::SspB0, &Policy::default(), &[mem]);
        assert_eq!(m.get(0, None, 0, 0x3F0), 1);
        assert_eq!(m.get(0, None, 4, 0xC8), 0x0702_0000);
        // Rate 5 for 256 GiB, capped to 3; enable after the rate.
        assert_eq!(m.get(0, None, 6, 0x44) & 0xF, 3);
        assert_eq!(m.get(0, None, 6, 0x40), 3);
        // Channel 0 present: controls on, mode 0; channel 3 absent: bits 1:0
        // cleared, mode kept.
        assert_eq!(m.get(0, Some(0), 6, 0xB8), 0x3);
        assert_eq!(m.get(0, Some(3), 6, 0xB8), 0xC);
        // SSP-B0 F2x228 on every channel: rate field 0 (capped), bit 24 set.
        assert_eq!(m.get(0, Some(1), 2, 0x228), 0xC1FF_FFFF);
        assert_eq!(m.get(0, Some(7), 2, 0x228), 0x0100_0000);
    }
}
