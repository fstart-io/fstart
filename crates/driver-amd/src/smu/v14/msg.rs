// SPDX-License-Identifier: MIT OR Apache-2.0
//! SMU v14 message IDs and their arguments, as the Venice modules send
//! them (venice `reports/research/RUNTIME.md` §2.1, data
//! `RUNTIME/smu_v14_messages.json`; Dell PR9XJ 1.0.8 and GIGABYTE MD25-CG0
//! F01, decompiler-checked where a witness is given). Every message takes
//! four argument dwords ([`super::ARGUMENT_COUNT`]); unnamed arguments are
//! 0. The vendor's DXE messages go to the SMU of each socket (its first
//! GnbHandle), the FCH USB messages to the SMU behind each IOD's root bus.
//!
//! Messages with a known purpose have helpers here; `SmuInitialization`'s
//! (the PPTable, the feature words, HSMP, sync flood to APML, PQoS, ...) are
//! sent by [`super::init::initialize`] with the board's PCD values.

use super::super::{CppcInfo, Error, SmuMessages, send1};

/// TestMessage: argument 0 `0xAA55AA55`. openSIL passes it when the reply's
/// argument 0 is non-zero (`SmuFirmwareTestWh`, SilVenicePei `0xf5dc50`);
/// it does not compare it with `0xAA55AA56`. [`test`].
pub const TEST: u32 = 0x01;
/// EnableSmuFeatures: arguments 0/1 the standard and extended feature masks
/// (standard bit 17 cleared unless MSR `C001_0C02` bit 21), argument 2 a
/// PCD (`41211h`). AmdNbioDxe SmuInitialization (Dell `0x3b90`, MD25
/// `0x3400`), and again late with argument 0 only (`0x5190`).
pub const ENABLE_SMU_FEATURES: u32 = 0x03;
/// The PPTable's DRAM address: arguments 0/1 low/high of the 9Ch-byte
/// table (`RUNTIME/pptable_v14.json`); before [`TRANSFER_TABLE_DRAM_TO_SMU`].
pub const SET_PPTABLE_DRAM_ADDRESS: u32 = 0x05;
/// The AGM log's DRAM address: arguments 0/1 low/high, PCD `41210h` pages
/// (default 4).
pub const SET_AGM_LOG_DRAM_ADDRESS: u32 = 0x06;
/// The SMU's DRAM buffer: arguments low, high, size << 20 (PCD `41203h`
/// MiB); sent for handles whose byte `+18h` is 0.
pub const SET_SMU_DRAM_BUFFER: u32 = 0x07;
/// PCIe AER reporting: argument 0 PCD `212h` (bool), argument 1 PCD `1E6h`;
/// per socket from PcieAerReportingPathExtCallBack (Dell `0xf2b0`, MD25
/// `0x8b48`).
pub const PCIE_AER_REPORTING: u32 = 0x09;
/// Sleep entry, posted and never polled, all arguments 0: the FCH SMM
/// sleep and power-button handlers send it after SLP_TYP and the CPU stops
/// (Dell FchSmmDispatcher `0x1ca90`, FchSmmInit `0x11440`; openSIL SMU API
/// slot `+18h`, SilVenicePei `0xf5d160`). [`post_sleep_entry`].
pub const SLEEP_ENTRY: u32 = 0x0b;
/// PcieLinkClearToTrain, no arguments: with PCIe device authentication
/// only, to each socket's SMU once every MPIO instance finished phase 1
/// (`MpioCfgAfterAllInstancesWh`, SilVenicePei `0xf658c0`, instruction-
/// checked: MPIO API slot `+0x78`, called at the end of the phase-1 loop
/// `0xf5ec00`). [`clear_to_train`].
pub const PCIE_LINK_CLEAR_TO_TRAIN: u32 = 0x0c;
/// ReadBrandString: argument 0 the dword index, the reply's argument 0 four
/// name bytes (Dell AmdNbioPei `0xecfa60`, AmdNbioDxe `0x6e60`).
pub use super::READ_BRAND_STRING;
/// ReadBistInfo (named by string order): argument 0 an index, the reply's
/// arguments 0/1 (Dell AmdNbioPei `0xecfbd0`, MD25 `0x70a94260`).
pub const READ_BIST_INFO: u32 = 0x0f;
/// TransferTableDram2Smu: argument 0 `9Ch` (the PPTable), after
/// [`SET_PPTABLE_DRAM_ADDRESS`].
pub const TRANSFER_TABLE_DRAM_TO_SMU: u32 = 0x10;
/// GetSmuFeatureDefaults: the reply's arguments the standard, extended and
/// standard-64 masks (SmuFixupPlatformConfigSP7, Dell `0xfc50`, MD25
/// `0x9ffc`).
pub const GET_SMU_FEATURE_DEFAULTS: u32 = 0x11;
/// CxlSmuEnableScm, four arguments: CXL only (Dell AmdNbioDxe `0x3640`).
pub const CXL_ENABLE_SCM: u32 = 0x22;
/// SetBoostLimitFrequencyAllCores: argument 0 MHz, when PCD `50004h` is
/// non-zero.
pub const SET_BOOST_LIMIT_FREQUENCY_ALL_CORES: u32 = 0x2b;
/// Unnamed: argument 0 PCD `41283h`, sent when non-zero.
pub const UNNAMED_2F: u32 = 0x2f;
/// The HSMP mailbox's register offsets in the SMU's window, no arguments
/// (HsmpWhDxe, Dell `0x1c60`, once [D]): [`hsmp_offsets`].
pub const HSMP_MAILBOX_OFFSETS: u32 = 0x33;
/// FllBtc enable, no arguments: when PCD `41266h` is 1.
pub const FLL_BTC_ENABLE: u32 = 0x37;
/// CppcGetInfo: the reply's argument 0 as v11's CppcGetInfo ([`CppcInfo`]:
/// bits 1:0 the `_CPC` revision, bit `k + 2` register `k` implemented,
/// bit 16 CPPC on); 0 when CPPC is off. Sent when PCD `FEh` (the SMU
/// feature mask) has bit 22 (Dell AmdNbioDxe `0x75c0`). [`cppc_info`].
pub const CPPC_GET_INFO: u32 = 0x39;
/// The CPPC nominal frequency: the reply's argument 0 MHz, `_CPC` entry 20
/// (Dell AmdNbioDxe `0x7860`). [`cppc_frequencies`].
pub const CPPC_NOMINAL_FREQUENCY: u32 = 0x3a;
/// SetResetCpuOnSyncFlood: argument 0 a bool (PEI FchAbInitQl, openSIL SMU
/// API slot `+44h`, SilVenicePei `0xf2ef80`, `0xf74640`).
pub const SET_RESET_CPU_ON_SYNC_FLOOD: u32 = 0x3b;
/// The CPPC lowest frequency: the reply's argument 0 MHz, `_CPC` entry 19
/// (Dell AmdNbioDxe `0x7860`). [`cppc_frequencies`].
pub const CPPC_LOWEST_FREQUENCY: u32 = 0x40;
/// EnableHSMPInterrupts, no arguments: when PCD `4F01Fh` is 1.
pub const ENABLE_HSMP_INTERRUPTS: u32 = 0x41;
/// SetSyncFloodToApml: argument 0 a bool; always sent (PCD value).
pub const SET_SYNC_FLOOD_TO_APML: u32 = 0x42;
/// SetEdcExcursionReporting: argument 0 a PCD, when PCD `41552h` is set.
pub const SET_EDC_EXCURSION_REPORTING: u32 = 0x4a;
/// NVM self-refresh: argument 0 = 1, per socket in PEI if NVM is present
/// (Dell AmdNbioPei `0xecf500`).
pub const ENABLE_NVM_SELF_REFRESH: u32 = 0x4c;
/// SetDelayResetCpuOnSyncFlood: argument 0 the delay (below 5 sent as 0),
/// when ResetCpuOnSyncFlood is on (SilVenicePei `0xf74640`).
pub const SET_DELAY_RESET_CPU_ON_SYNC_FLOOD: u32 = 0x4d;
/// EnableSPGUnusedPcie: argument 0 UnusedTileClkGating; always sent.
pub const ENABLE_SPG_UNUSED_PCIE: u32 = 0x4e;
/// PcieMiscInit flag: argument 0 a PCD bool, per socket (Dell `0xf030`,
/// MD25 `0x8d14`).
pub const PCIE_MISC_INIT: u32 = 0x53;
/// SetHsmpCmdsReadWriteMask: arguments 0–2 the mask, argument 3 1 = read,
/// 0 = write; sent twice.
pub const SET_HSMP_CMDS_READ_WRITE_MASK: u32 = 0x55;
/// SetVdciTestTransferMode: argument 0 a PCD, when non-zero.
pub const SET_VDCI_TEST_TRANSFER_MODE: u32 = 0x56;
/// Periodic circuit recalibration, no arguments: when PCD `4126Ah` is set.
pub const PERIODIC_CIRCUIT_RECALIBRATION: u32 = 0x57;
/// UpdatePccLimit, no arguments: Dell only (PI 1.0.0.0).
pub const UPDATE_PCC_LIMIT: u32 = 0x58;
/// EnablePQoSTrafficPriorityGMBFMB: argument 0 a mask; outside emulation.
pub const ENABLE_PQOS_TRAFFIC_PRIORITY: u32 = 0x59;
/// "SetCaliptraErrorReporting": at or above [`super::ID_LIMIT`], so the
/// vendor's request function returns without sending it (SilVenicePei
/// `0xf74640`); listed for the record.
pub const SET_CALIPTRA_ERROR_REPORTING: u32 = 0xff;

/// Every message the Venice modules name, in ID order.
pub const ALL: [u32; 34] = [
    TEST,
    ENABLE_SMU_FEATURES,
    SET_PPTABLE_DRAM_ADDRESS,
    SET_AGM_LOG_DRAM_ADDRESS,
    SET_SMU_DRAM_BUFFER,
    PCIE_AER_REPORTING,
    SLEEP_ENTRY,
    PCIE_LINK_CLEAR_TO_TRAIN,
    READ_BRAND_STRING,
    READ_BIST_INFO,
    TRANSFER_TABLE_DRAM_TO_SMU,
    GET_SMU_FEATURE_DEFAULTS,
    CXL_ENABLE_SCM,
    SET_BOOST_LIMIT_FREQUENCY_ALL_CORES,
    UNNAMED_2F,
    HSMP_MAILBOX_OFFSETS,
    FLL_BTC_ENABLE,
    CPPC_GET_INFO,
    CPPC_NOMINAL_FREQUENCY,
    SET_RESET_CPU_ON_SYNC_FLOOD,
    CPPC_LOWEST_FREQUENCY,
    ENABLE_HSMP_INTERRUPTS,
    SET_SYNC_FLOOD_TO_APML,
    SET_EDC_EXCURSION_REPORTING,
    ENABLE_NVM_SELF_REFRESH,
    SET_DELAY_RESET_CPU_ON_SYNC_FLOOD,
    ENABLE_SPG_UNUSED_PCIE,
    PCIE_MISC_INIT,
    SET_HSMP_CMDS_READ_WRITE_MASK,
    SET_VDCI_TEST_TRANSFER_MODE,
    PERIODIC_CIRCUIT_RECALIBRATION,
    UPDATE_PCC_LIMIT,
    ENABLE_PQOS_TRAFFIC_PRIORITY,
    SET_CALIPTRA_ERROR_REPORTING,
];

/// The HSMP mailbox's registers in the SMU's window (`SMU BAR +
/// offset`), as HsmpWhDxe derives them from [`HSMP_MAILBOX_OFFSETS`]'s
/// reply `a0..a2`: each 16-bit half plus `0x1_0000`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HsmpOffsets {
    /// `MsgIdOffset` (`a0[15:0]`), `MsgRspOffset` (`a0[31:16]`),
    /// `MsgArgOffset` (`a1[15:0]`), then `a1[31:16]`, `a2[15:0]`,
    /// `a2[31:16]`, the six offsets of the `HSMP` table's instance entry.
    /// That the last three take the same `+0x1_0000` is read from the
    /// table builder's loop, not from a named field.
    pub offsets: [u32; 6],
}

impl HsmpOffsets {
    /// From the reply's first three arguments.
    pub fn from_reply(a: [u32; 3]) -> Self {
        let half = |w: u32, hi: bool| (if hi { w >> 16 } else { w & 0xffff }) + 0x1_0000;
        Self {
            offsets: [
                half(a[0], false),
                half(a[0], true),
                half(a[1], false),
                half(a[1], true),
                half(a[2], false),
                half(a[2], true),
            ],
        }
    }

    pub const fn message_id(&self) -> u32 {
        self.offsets[0]
    }

    pub const fn response(&self) -> u32 {
        self.offsets[1]
    }

    pub const fn argument(&self) -> u32 {
        self.offsets[2]
    }
}

/// [`HSMP_MAILBOX_OFFSETS`] on one socket's SMU.
pub fn hsmp_offsets<M: SmuMessages + ?Sized>(smu: &M) -> Result<HsmpOffsets, Error> {
    let mut args = [0; 6];
    smu.send(HSMP_MAILBOX_OFFSETS, &mut args)?;
    Ok(HsmpOffsets::from_reply([args[0], args[1], args[2]]))
}

/// [`TEST`]: `Ok` with the reply's argument 0, which must be non-zero
/// (openSIL's rule; the mailbox's response must be OK as for every
/// message).
pub fn test<M: SmuMessages + ?Sized>(smu: &M) -> Result<u32, Error> {
    match send1(smu, TEST, 0xaa55_aa55)? {
        0 => Err(Error::Rejected(0)),
        reply => Ok(reply),
    }
}

/// [`CPPC_GET_INFO`]; `None` when the SMU answers 0 (CPPC off).
pub fn cppc_info<M: SmuMessages + ?Sized>(smu: &M) -> Result<Option<CppcInfo>, Error> {
    let answer = send1(smu, CPPC_GET_INFO, 0)?;
    Ok((answer != 0).then(|| CppcInfo::from_answer(answer)))
}

/// `(lowest, nominal)` MHz for `_CPC` entries 19 and 20 ([`CPPC_LOWEST_FREQUENCY`],
/// [`CPPC_NOMINAL_FREQUENCY`]; the vendor sends 3Ah first).
pub fn cppc_frequencies<M: SmuMessages + ?Sized>(smu: &M) -> Result<(u32, u32), Error> {
    let nominal = send1(smu, CPPC_NOMINAL_FREQUENCY, 0)?;
    Ok((send1(smu, CPPC_LOWEST_FREQUENCY, 0)?, nominal))
}

/// [`PCIE_LINK_CLEAR_TO_TRAIN`] to one socket's SMU (its die-0 IOHC 0, as
/// the vendor's loop over the root bridges picks): MPIO, told to wait for
/// it by the global configuration's device authentication bit, trains the
/// links after it.
pub fn clear_to_train<M: SmuMessages + ?Sized>(smu: &M) -> Result<(), Error> {
    send1(smu, PCIE_LINK_CLEAR_TO_TRAIN, 0).map(drop)
}

/// [`CXL_ENABLE_SCM`], arguments 0: before a persistent CXL memory pool is
/// enabled.
pub fn cxl_enable_scm<M: SmuMessages + ?Sized>(smu: &M) -> Result<(), Error> {
    send1(smu, CXL_ENABLE_SCM, 0).map(drop)
}

/// [`SLEEP_ENTRY`], posted: the SMU powers the system down and never
/// answers. `smn` reaches socket 0's SMU (the vendor's SMM uses bus 0).
#[inline(always)]
pub fn post_sleep_entry(smn: &impl crate::smn::SmnAccess) {
    super::MAILBOX.post(smn, SLEEP_ENTRY, [0; super::super::ARGUMENT_COUNT]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smu::{model::SmuModel, response};

    #[test]
    fn the_table_is_the_report_s_33_messages_in_order() {
        assert_eq!(ALL.len(), 34);
        assert!(ALL.windows(2).all(|w| w[0] < w[1]));
        // All but the last are below the ID limit.
        assert!(ALL[..33].iter().all(|&id| id < super::super::ID_LIMIT));
        const { assert!(SET_CALIPTRA_ERROR_REPORTING >= super::super::ID_LIMIT) };
    }

    #[test]
    fn test_passes_on_a_non_zero_reply() {
        let m = SmuModel::new(super::super::MAILBOX, response::OK);
        // The model echoes argument 0 back: 0xAA55AA55 passes as well.
        assert_eq!(test(&m.smu(super::super::ID_LIMIT)), Ok(0xaa55_aa55));
        let mut m = SmuModel::new(super::super::MAILBOX, response::OK);
        m.echo = 1;
        assert_eq!(test(&m.smu(super::super::ID_LIMIT)), Ok(0xaa55_aa56));
        // A reply of 0 fails; so does a rejected message.
        let mut zero = SmuModel::new(super::super::MAILBOX, response::OK);
        zero.echo = 0x55aa_55ab;
        assert_eq!(
            test(&zero.smu(super::super::ID_LIMIT)),
            Err(Error::Rejected(0))
        );
        let rejected = SmuModel::new(super::super::MAILBOX, response::FAILED);
        assert!(test(&rejected.smu(super::super::ID_LIMIT)).is_err());
    }

    #[test]
    fn hsmp_offsets_are_the_reply_halves_plus_64k() {
        let o = HsmpOffsets::from_reply([0x0938_0934, 0x0940_093c, 0x0944_0948]);
        assert_eq!(
            (o.message_id(), o.response(), o.argument()),
            (0x1_0934, 0x1_0938, 0x1_093c)
        );
        assert_eq!(o.offsets[3..], [0x1_0940, 0x1_0948, 0x1_0944]);
        let m = SmuModel::new(super::super::MAILBOX, response::OK);
        assert_eq!(
            hsmp_offsets(&m.smu(super::super::ID_LIMIT))
                .unwrap()
                .offsets[0],
            0x1_0000
        );
    }

    #[test]
    fn cppc_messages() {
        let m = SmuModel::new(super::super::MAILBOX, response::OK);
        let smu = m.smu(super::super::ID_LIMIT);
        // The model echoes argument 0, which these send as 0: CPPC off.
        assert_eq!(cppc_info(&smu), Ok(None));
        assert_eq!(cppc_frequencies(&smu), Ok((0, 0)));
        assert_eq!(m.ids(), [0x39, 0x3a, 0x40]);
        let mut on = SmuModel::new(super::super::MAILBOX, response::OK);
        on.echo = 0x007f_ffff;
        let info = cppc_info(&on.smu(super::super::ID_LIMIT)).unwrap().unwrap();
        assert_eq!(info.revision, 3);
        assert!(info.enabled() && info.implements(20));
    }
}
