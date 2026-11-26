// SPDX-License-Identifier: MIT OR Apache-2.0
//! `SmuInitialization` (AmdNbioDxe, Dell PR9XJ 1.0.8 `0x3b90`, GIGABYTE
//! MD25-CG0 F01 `0x3400`; instruction-read): what the vendor tells each
//! socket's SMU in DXE, in its order, with the PCD values as a [`Policy`].
//!
//! | Message | When | Argument(s) |
//! |---|---|---|
//! | `05h`, `10h` | always | the [`PpTable`]'s address, then its size `9Ch` |
//! | `06h` | always | the AGM log's address ([`Policy::agm_log_pages`]) |
//! | `07h` | [`Policy::dram_buffer_mib`] non-zero | address, size in bytes |
//! | `4Eh` | always | unused-tile clock gating |
//! | `03h` | always | the feature words ([`features::enable`]) |
//! | `57h` | [`Policy::periodic_circuit_recalibration`] | – |
//! | `37h` | [`Policy::fll_btc`] | – |
//! | `42h` | always | [`Policy::sync_flood_to_apml`] |
//! | `41h` | [`Policy::hsmp`] | – |
//! | `55h` twice | always | the HSMP read mask (`a3` 1), then the write mask (`a3` 0) |
//! | `2Fh` | [`Policy::moc_pbo_limit_scalar`] non-zero | it |
//! | `4Ah` | [`Policy::edc_excursion_reporting`] | it |
//! | `2Bh` | [`Policy::boost_fmax_mhz`] non-zero | it |
//! | `56h` | [`Policy::vdci_test_transfer_mode`] non-zero | it |
//! | `59h` | always (silicon) | the PQoS mask ([`Pqos::mask`]) |
//! | `58h` | [`Policy::update_pcc_limit`] and standard feature bit 5 | – |
//!
//! Left out: `CxlSmuSpeedNotification` (Dell `0x3610`, CXL only) and the
//! emulation paths (`PcdAmdPreSilCtrl0/1`, 0 in both donors).

use super::features::{self, Controls};
use super::msg;
use super::pptable::PpTable;
use crate::smu::{Error, SmuMessages};

/// The PQoS traffic priorities of `EnablePQoSTrafficPriorityGMBFMB` (`59h`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pqos {
    /// `PcdPQosTrafficPriorityIo` (Dell token 0x271).
    pub io: bool,
    /// `PcdPQosTrafficPriorityCcd` (0x270).
    pub ccd: bool,
    /// `PcdPQosGmb` (0x26F).
    pub gmb: bool,
    /// `PcdPQosFmb` (0x26E), only with GMB.
    pub fmb: bool,
}

impl Pqos {
    /// The message's mask: bit 0 IO, 1 CCD, 2 GMB, 3 FMB; GMB and FMB only
    /// when the APCB's `DfDramNumaPerSocket` (`0x2CF3DAC9`, the vendor's
    /// fallback 2 when it cannot read it) is not 3.
    pub const fn mask(&self, nps_token: u8) -> u32 {
        let gmb = self.gmb && nps_token != 3;
        let fmb = self.fmb && gmb;
        self.io as u32 | (self.ccd as u32) << 1 | (gmb as u32) << 2 | (fmb as u32) << 3
    }
}

/// The PCD values `SmuInitialization` sends. [`Policy::DEFAULT`] is both
/// donors' PCD databases where they agree (VALUES.md, `lookup.py` per
/// token), with fused platform limits (board data,
/// [`super::pptable::PlatformLimits`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub pptable: PpTable,
    /// `PcdCfgAgmLogDramSize` (Dell 0x35C, GIGABYTE ex `0x41210`): 4 KiB
    /// pages, 0 means 4. 16 in both.
    pub agm_log_pages: u16,
    /// `PcdSmuAllocateDramBufferSize` (Dell 0x37D, GIGABYTE `0x41203`): MiB,
    /// 0 = no buffer and no `07h`. 0 in both.
    pub dram_buffer_mib: u32,
    /// The feature-word overrides (`0xFD` = none in both).
    pub overrides: features::Overrides,
    /// `PcdAmdDrbStatusCleared` (Dell 0x34C, 0; GIGABYTE `0x35013`): clears
    /// standard feature bit 17, as MSR `C001_0C02[38]` does.
    pub drb_status_cleared: bool,
    /// `PcdAmdPeriodicCircuitRecalibration` (Dell 0x1EC, GIGABYTE
    /// `0x4126A`): 1 in both.
    pub periodic_circuit_recalibration: bool,
    /// `PcdFllBtcEnable` (Dell 0x375, GIGABYTE `0x41266`): 1 in both; the
    /// vendor clears it only on emulation platforms.
    pub fll_btc: bool,
    /// `PcdSyncFloodToApml` (Dell 0x2A0, GIGABYTE `0x4F213`): 1 in both.
    pub sync_flood_to_apml: bool,
    /// `PcdCfgHSMPSupport` (Dell 0x21F, GIGABYTE `0x4F01F`) == 1: 1 in both.
    pub hsmp: bool,
    /// `PcdCfgSMUHsmpReadMask0..2` (Dell 0x239..0x23B).
    pub hsmp_read_mask: [u32; 3],
    /// `PcdCfgSMUHsmpWriteMask0..2` (Dell 0x23C..0x23E).
    pub hsmp_write_mask: [u32; 3],
    /// `PcdMocPBOLimitScalar` (Dell 0x84; GIGABYTE sends `2Fh` from ex
    /// `0x41283`): 0 in both.
    pub moc_pbo_limit_scalar: u32,
    /// `PcdAmdSetEdcExcursionReportEn` / `...Reporting` (Dell 0x357/0x358):
    /// off in both.
    pub edc_excursion_reporting: Option<u32>,
    /// `PcdAmdBoostFmax` (Dell 0x146, GIGABYTE `0x50004`), MHz: 0 in both.
    pub boost_fmax_mhz: u32,
    /// `PcdAmdUmcVdciTestTransferMode` (Dell 0x359): 0 in both.
    pub vdci_test_transfer_mode: u8,
    /// PQoS traffic priorities: all off in both.
    pub pqos: Pqos,
    /// `UpdatePccLimit` (`58h`), sent by Dell's PI 1.0.0.0 when standard
    /// feature bit 5 is set; GIGABYTE's older PI does not have it.
    pub update_pcc_limit: bool,
}

impl Policy {
    pub const DEFAULT: Self = Self {
        pptable: PpTable::DEFAULT,
        agm_log_pages: 16,
        dram_buffer_mib: 0,
        overrides: features::Overrides::NONE,
        drb_status_cleared: false,
        periodic_circuit_recalibration: true,
        fll_btc: true,
        sync_flood_to_apml: true,
        hsmp: true,
        hsmp_read_mask: [0xffff_ffff, 0xffff_ffff, 0x7fff_ffff],
        hsmp_write_mask: [0xffff_ffff, 0xffff_ffff, 0x7fff_ffff],
        moc_pbo_limit_scalar: 0,
        edc_excursion_reporting: None,
        boost_fmax_mhz: 0,
        vdci_test_transfer_mode: 0,
        pqos: Pqos {
            io: false,
            ccd: false,
            gmb: false,
            fmb: false,
        },
        update_pcc_limit: true,
    };

    /// The AGM log's size in bytes.
    pub const fn agm_log_bytes(&self) -> usize {
        let pages = if self.agm_log_pages == 0 {
            4
        } else {
            self.agm_log_pages
        };
        pages as usize * 0x1000
    }
}

impl Default for Policy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The DRAM the SMU is given: the PPTable copy, the AGM log and the
/// optional buffer, all reserved from the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Buffers {
    pub pptable: u64,
    pub agm_log: u64,
    /// Address of [`Policy::dram_buffer_mib`] MiB.
    pub dram_buffer: Option<u64>,
}

/// The message that failed and why.
pub type Failed = (u32, Error);

fn send<M: SmuMessages + ?Sized>(smu: &M, id: u32, a: [u32; 4]) -> Result<(), Failed> {
    let mut args = [a[0], a[1], a[2], a[3], 0, 0];
    smu.send(id, &mut args).map_err(|e| (id, e))
}

const fn split(address: u64) -> (u32, u32) {
    (address as u32, (address >> 32) as u32)
}

/// `SmuInitialization` on one socket's SMU: the messages of the module
/// table in order. `controls` are the feature words (socket 0's defaults
/// with the overrides, [`features::defaults`], [`Controls::with`]),
/// `drb_msr` MSR `C001_0C02[38]` of this CPU, `unused_tile` the
/// unused-tile clock gating, `nps_token` the APCB NPS token ([`Pqos::mask`]).
#[allow(clippy::too_many_arguments)]
pub fn initialize<M: SmuMessages + ?Sized>(
    smu: &M,
    policy: &Policy,
    buffers: &Buffers,
    controls: Controls,
    drb_msr: bool,
    unused_tile: bool,
    nps_token: u8,
) -> Result<(), Failed> {
    let (lo, hi) = split(buffers.pptable);
    send(smu, msg::SET_PPTABLE_DRAM_ADDRESS, [lo, hi, 0, 0])?;
    send(smu, msg::TRANSFER_TABLE_DRAM_TO_SMU, [0x9c, 0, 0, 0])?;
    let (lo, hi) = split(buffers.agm_log);
    send(smu, msg::SET_AGM_LOG_DRAM_ADDRESS, [lo, hi, 0, 0])?;
    if let (Some(buffer), mib @ 1..) = (buffers.dram_buffer, policy.dram_buffer_mib) {
        let (lo, hi) = split(buffer);
        send(smu, msg::SET_SMU_DRAM_BUFFER, [lo, hi, mib << 20, 0])?;
    }
    send(
        smu,
        msg::ENABLE_SPG_UNUSED_PCIE,
        [unused_tile.into(), 0, 0, 0],
    )?;
    let enabled = controls.to_enable(policy.drb_status_cleared || drb_msr);
    features::enable(smu, enabled).map_err(|e| (msg::ENABLE_SMU_FEATURES, e))?;
    if policy.periodic_circuit_recalibration {
        send(smu, msg::PERIODIC_CIRCUIT_RECALIBRATION, [0; 4])?;
    }
    if policy.fll_btc {
        send(smu, msg::FLL_BTC_ENABLE, [0; 4])?;
    }
    send(
        smu,
        msg::SET_SYNC_FLOOD_TO_APML,
        [policy.sync_flood_to_apml.into(), 0, 0, 0],
    )?;
    if policy.hsmp {
        send(smu, msg::ENABLE_HSMP_INTERRUPTS, [0; 4])?;
    }
    let [r0, r1, r2] = policy.hsmp_read_mask;
    send(smu, msg::SET_HSMP_CMDS_READ_WRITE_MASK, [r0, r1, r2, 1])?;
    let [w0, w1, w2] = policy.hsmp_write_mask;
    send(smu, msg::SET_HSMP_CMDS_READ_WRITE_MASK, [w0, w1, w2, 0])?;
    if policy.moc_pbo_limit_scalar != 0 {
        send(smu, msg::UNNAMED_2F, [policy.moc_pbo_limit_scalar, 0, 0, 0])?;
    }
    if let Some(reporting) = policy.edc_excursion_reporting {
        send(smu, msg::SET_EDC_EXCURSION_REPORTING, [reporting, 0, 0, 0])?;
    }
    if policy.boost_fmax_mhz != 0 {
        let mhz = policy.boost_fmax_mhz;
        send(
            smu,
            msg::SET_BOOST_LIMIT_FREQUENCY_ALL_CORES,
            [mhz, 0, 0, 0],
        )?;
    }
    if policy.vdci_test_transfer_mode != 0 {
        let mode = policy.vdci_test_transfer_mode.into();
        send(smu, msg::SET_VDCI_TEST_TRANSFER_MODE, [mode, 0, 0, 0])?;
    }
    let pqos = policy.pqos.mask(nps_token);
    send(smu, msg::ENABLE_PQOS_TRAFFIC_PRIORITY, [pqos, 0, 0, 0])?;
    if policy.update_pcc_limit && enabled.standard & features::bit::PCC_LIMIT != 0 {
        send(smu, msg::UPDATE_PCC_LIMIT, [0; 4])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smu::ARGUMENT_COUNT;
    use core::cell::RefCell;
    use std::vec::Vec;

    #[derive(Default)]
    struct Smu(RefCell<Vec<(u32, [u32; ARGUMENT_COUNT])>>);
    impl SmuMessages for Smu {
        fn send(&self, id: u32, args: &mut [u32; ARGUMENT_COUNT]) -> Result<(), Error> {
            self.0.borrow_mut().push((id, *args));
            Ok(())
        }
    }

    const BUFFERS: Buffers = Buffers {
        pptable: 0x1_2345_6000,
        agm_log: 0x7e00_0000,
        dram_buffer: None,
    };

    #[test]
    fn the_donors_sequence() {
        let smu = Smu::default();
        let controls = Controls {
            standard: features::bit::DRB | features::bit::PCC_LIMIT | 1,
            extended: 2,
            standard64: 3,
        };
        initialize(&smu, &Policy::DEFAULT, &BUFFERS, controls, true, true, 2).unwrap();
        let sent = smu.0.into_inner();
        let ids: Vec<u32> = sent.iter().map(|m| m.0).collect();
        assert_eq!(
            ids,
            [
                0x05, 0x10, 0x06, 0x4e, 0x03, 0x57, 0x37, 0x42, 0x41, 0x55, 0x55, 0x59, 0x58
            ]
        );
        assert_eq!(sent[0].1[..2], [0x2345_6000, 1]);
        assert_eq!(sent[1].1[0], 0x9c);
        assert_eq!(sent[2].1[..2], [0x7e00_0000, 0]);
        assert_eq!(sent[3].1[0], 1, "unused-tile clock gating");
        // DRB set by the MSR: bit 17 cleared.
        assert_eq!(sent[4].1[..3], [features::bit::PCC_LIMIT | 1, 2, 3]);
        assert_eq!(sent[7].1[0], 1, "sync flood to APML");
        assert_eq!(sent[9].1[..4], [!0, !0, 0x7fff_ffff, 1]);
        assert_eq!(sent[10].1[..4], [!0, !0, 0x7fff_ffff, 0]);
        assert_eq!(sent[11].1[0], 0, "no PQoS");
    }

    #[test]
    fn optional_messages_follow_their_values() {
        let smu = Smu::default();
        let policy = Policy {
            dram_buffer_mib: 2,
            hsmp: false,
            fll_btc: false,
            periodic_circuit_recalibration: false,
            boost_fmax_mhz: 3000,
            edc_excursion_reporting: Some(5),
            moc_pbo_limit_scalar: 7,
            vdci_test_transfer_mode: 1,
            update_pcc_limit: false,
            pqos: Pqos {
                io: true,
                gmb: true,
                fmb: true,
                ccd: false,
            },
            ..Policy::DEFAULT
        };
        let buffers = Buffers {
            dram_buffer: Some(0x8000_0000),
            ..BUFFERS
        };
        let controls = Controls {
            standard: features::bit::PCC_LIMIT,
            extended: 0,
            standard64: 0,
        };
        initialize(&smu, &policy, &buffers, controls, false, false, 3).unwrap();
        let sent = smu.0.into_inner();
        let ids: Vec<u32> = sent.iter().map(|m| m.0).collect();
        assert_eq!(
            ids,
            [
                0x05, 0x10, 0x06, 0x07, 0x4e, 0x03, 0x42, 0x55, 0x55, 0x2f, 0x4a, 0x2b, 0x56, 0x59
            ]
        );
        assert_eq!(sent[3].1[..3], [0x8000_0000, 0, 2 << 20]);
        assert_eq!(sent[13].1[0], 1, "GMB/FMB off at NPS token 3");
        assert_eq!(
            Pqos {
                io: true,
                gmb: true,
                fmb: true,
                ccd: false
            }
            .mask(2),
            0b1101
        );
        assert_eq!(Policy::DEFAULT.agm_log_bytes(), 0x1_0000);
    }
}
