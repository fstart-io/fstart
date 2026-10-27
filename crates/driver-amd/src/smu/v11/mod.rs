//! SMU V11 (Rome, Milan; one SMU per IOD): mailbox registers, message IDs
//! (with the Rome/Milan differences), feature masks, the DXE table
//! transfers and the CCX-done sequence (NBIO_SMU §3.8).
//!
//! Every function takes any [`SmuMessages`]; on hardware that is a
//! [`Smu`](crate::smu::Smu) with [`MAILBOX`] through any root bus
//! of the socket.

#[cfg(feature = "acpi")]
pub mod cppc;
pub mod pptable;

use fstart_core::services::Timer;

use crate::cpu::Revision;
use crate::smn::SmnReg;
use crate::smu::{ARGUMENT_COUNT, Error as SmuError, Mailbox, SmuMessages, send_addr, send1};
use crate::sp3::soc::Generation;

/// V11 mailbox (`smu.v11.mailbox`; Oxide `milan/smu.h` agrees).
pub const MAILBOX: Mailbox = Mailbox {
    message: SmnReg::new(0x03b1_0530),
    response: SmnReg::new(0x03b1_057c),
    arguments: SmnReg::new(0x03b1_09c4),
    wait_before: false,
};

/// AGESA's V11 service rejects IDs at or above this.
pub const fn id_limit(generation: Generation) -> u32 {
    match generation {
        Generation::Milan => 0x4A,
        _ => 0x3E,
    }
}

/// Message IDs shared by Rome and Milan.
pub mod msg {
    /// Test / apply ThreadEn, downcore (`0x30081020 + ccd << 25`) and SMT
    /// (`0x3008101C |= 0x200`); arg 0xAA55AA55 (CCX track writes the SMN).
    pub const TEST: u32 = 0x01;
    /// arg0 = mask (Milan: arg1 = high mask).
    pub const ENABLE_FEATURES: u32 = 0x03;
    /// args lo, hi.
    pub const SET_BIOS_DRAM_ADDR: u32 = 0x05;
    /// args lo, hi (AGM log).
    pub const SET_TOOLS_DRAM_ADDR: u32 = 0x06;
    /// args lo, hi, size.
    pub const SET_DEBUG_DRAM_ADDR: u32 = 0x07;
    /// DXIO RPC (PCIe track).
    pub const DXIO: u32 = 0x08;
    /// CAC / DPM weight by index (CCX track).
    pub const READ_DPM_WEIGHT: u32 = 0x09;
    /// End of the USB configuration on a cold boot (FCH track).
    pub const USB_INIT_DONE: u32 = 0x0A;
    /// Last message at Sx entry (FCH track; semantics not public).
    pub const SLEEP_ENTRY: u32 = 0x0B;
    pub const DC_BTC: u32 = 0x0C;
    /// Brand string dword by index.
    pub const GET_BRAND_STRING: u32 = 0x0D;
    pub const READ_BIST_INFO: u32 = 0x0F;
    /// arg0 = table size (PPTable after [`SET_BIOS_DRAM_ADDR`]).
    pub const TRANSFER_TABLE_DRAM2SMU: u32 = 0x10;
    pub const TRANSFER_HOTPLUG_TABLE: u32 = 0x12;
    /// `{SMN reg, mask, data, group}` USB settings (FCH track).
    pub const USB_CONFIG_UPDATE: u32 = 0x13;
    /// USB step at Sx entry (FCH track).
    pub const USB_SX_ENTRY: u32 = 0x14;
    /// End of the USB configuration on S3 resume (FCH track).
    pub const USB_S3_RESUME_DONE: u32 = 0x15;
    pub const CPPC_SET_TABLE: u32 = 0x16;
    pub const CPPC_INFO: u32 = 0x17;
    pub const ENABLE_HOTPLUG: u32 = 0x18;
    pub const SET_I2C_SWITCH_BASE: u32 = 0x1A;
    pub const SET_HOTPLUG_DEBUG_FLAGS: u32 = 0x1D;
    /// NBIF power gating.
    pub const SET_POWER_GATE: u32 = 0x2A;
    pub const SET_MAX_ALL_CORES_FREQ: u32 = 0x2B;
    /// arg `nbio << 16 | 0x202`.
    pub const SET_NBIO_LCLK_DPM_LEVEL: u32 = 0x34;
    pub const SET_L3_CREDIT_MODE: u32 = 0x35;
}

/// Rome-only message IDs.
pub mod rome {
    pub const TRANSFER_LANE_MAP_TABLE: u32 = 0x11;
    /// AM4 fixup only.
    pub const GET_SUSTAINED_POWER_AND_THM_LIMIT: u32 = 0x23;
    /// APCB token 0x41315.
    pub const CAC_WEIGHT_ADJ: u32 = 0x36;
    /// APCB token 0x500A8 (VID cap). Milan uses 0x38 for SocDcBtc.
    pub const VID_CAP: u32 = 0x38;
    pub const SET_EDC_EXCURSION_REPORTING: u32 = 0x39;
    pub const SET_HOTPLUG_TABLE_ADDR: u32 = 0x3C;
}

/// Milan-only message IDs.
pub mod milan {
    pub const FLL_BTC: u32 = 0x37;
    pub const SOC_DC_BTC: u32 = 0x38;
    pub const ENABLE_HSMP_INTERRUPTS: u32 = 0x41;
    pub const SET_APML_FLOOD: u32 = 0x42;
    pub const FDD_BTC: u32 = 0x43;
    /// VddcrCpu limit (VID cap), APCB token 0x500A8.
    pub const VDDCR_CPU_LIMIT: u32 = 0x44;
    pub const SET_EDC_TRACK: u32 = 0x45;
    pub const SET_DF_IRRITATOR: u32 = 0x46;
    pub const SET_HOTPLUG_TABLE_ADDR: u32 = 0x47;
    pub const SET_L1_IO_DROP_CHAIN: u32 = 0x48;
}

/// V11 feature bits (`FEATURE_BITS` of the build tool; Oxide names).
pub mod features {
    pub const DATA_CALCULATION: u32 = 1 << 0;
    pub const PPT: u32 = 1 << 1;
    pub const TDC: u32 = 1 << 2;
    pub const THERMAL: u32 = 1 << 3;
    pub const FIT: u32 = 1 << 4;
    pub const EDC: u32 = 1 << 5;
    pub const CSTATE_BOOST: u32 = 1 << 6;
    pub const PROCHOT: u32 = 1 << 7;
    pub const CCLK_DPM: u32 = 1 << 8;
    pub const FCLK_DPM: u32 = 1 << 9;
    pub const LCLK_DPM: u32 = 1 << 10;
    pub const DLWM: u32 = 1 << 11;
    pub const DLDO: u32 = 1 << 12;
    pub const SOCCLK_DEEP_SLEEP: u32 = 1 << 13;
    pub const LCLK_DEEP_SLEEP: u32 = 1 << 14;
    pub const SHUBCLK_DEEP_SLEEP: u32 = 1 << 15;
    pub const CC6: u32 = 1 << 17;
    pub const PC6: u32 = 1 << 18;
    pub const DF_CSTATES: u32 = 1 << 19;
    pub const CLOCK_GATING: u32 = 1 << 20;
    pub const FAN_CONTROLLER: u32 = 1 << 21;
    pub const CPPC: u32 = 1 << 22;
    pub const DLDO_DROPOUT_LIMITER: u32 = 1 << 23;
    pub const CPPC_PREFERRED_CORES: u32 = 1 << 24;
    /// Rome: HSMP_BYPASS; Milan: DVO.
    pub const BIT25: u32 = 1 << 25;
    /// Rome: DIAGNOSTIC_MODE; Milan: AGE.
    pub const BIT26: u32 = 1 << 26;
    /// Milan: DIAGNOSTIC_MODE.
    pub const BIT27: u32 = 1 << 27;

    /// Rome SP3 mask (PCD 0x26C fixup).
    pub const ROME_SP3: u32 = 0xDA_1FFF;
    /// Milan SP3 mask (PCD 0x12A fixup).
    pub const MILAN_SP3: u32 = 0x6DA_FFFF;
}

/// The policy behind the SP3 feature-mask fixups; `Default` reproduces
/// the donor SP3 masks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeaturePolicy {
    /// Rome PCD 0x292 / Milan PCD 0x1A9 (B0+).
    pub dlwm: bool,
    /// Rome PCD 0x27A / Milan PCD 0x55.
    pub cppc: bool,
    /// Milan PCD 0x1C5.
    pub df_cstates: bool,
    /// Rome PCD 0x291 / Milan PCD 0x307.
    pub diagnostic: bool,
    /// Rome: HSMP_BYPASS = !PCD 0x290.
    pub hsmp_bypass: bool,
}

impl Default for FeaturePolicy {
    fn default() -> Self {
        Self {
            dlwm: true,
            cppc: true,
            df_cstates: true,
            diagnostic: false,
            hsmp_bypass: false,
        }
    }
}

const fn put(mask: u32, bit: u32, on: bool) -> u32 {
    if on { mask | bit } else { mask & !bit }
}

/// The SP3 feature mask for `revision` (`ssp.smu_fixup_sp3`,
/// `gn.smu_fixup_sp3`).
pub const fn feature_mask(revision: Revision, p: &FeaturePolicy) -> u32 {
    use features::*;
    match revision {
        // Rome before B0 drops FCLK_DPM, DLWM and DF_CSTATES.
        Revision::SspA0 => ROME_SP3 & !(FCLK_DPM | DLWM | DF_CSTATES),
        Revision::SspB0 => {
            let m = put(ROME_SP3, DLWM, p.dlwm);
            let m = put(m, CPPC, p.cppc);
            let m = put(m, BIT25, p.hsmp_bypass);
            put(m, BIT26, p.diagnostic)
        }
        // Milan A0 drops PC6 (not in the SP3 mask anyway).
        Revision::GnA0 => MILAN_SP3 & !PC6,
        Revision::GnB0 | Revision::GnB1 | Revision::GnB2 => {
            let m = put(MILAN_SP3, DLWM, p.dlwm);
            let m = put(m, CPPC, p.cppc);
            let m = put(m, DF_CSTATES, p.df_cstates);
            put(m, BIT27, p.diagnostic)
        }
        _ => 0,
    }
}

/// Enable the requested bits, without disabling previously enabled bits
/// (`hi`: Milan's second mask word, PCD 0x12B in PEI, 0 in DXE).
pub fn enable_features<M: SmuMessages + ?Sized>(
    smu: &M,
    mask: u32,
    hi: u32,
) -> Result<(), SmuError> {
    let mut args = [mask, hi, 0, 0, 0, 0];
    smu.send(msg::ENABLE_FEATURES, &mut args)
}

/// Milan PEI: enable CC6 before a topology-change reset, on the CCX PPI
/// b0aae673 (`gn.smu_early_cc6`), per socket. Do not call on a normal boot:
/// feature enables are additive, and DC BTC rejects an already-enabled CC6.
pub fn early_cc6<M: SmuMessages + ?Sized>(smu: &M, mask: u32, hi: u32) -> Result<(), SmuError> {
    enable_features(smu, mask | features::CC6, hi)
}

/// DMA the PPTable at `addr` (physical, `size` bytes: 0x27C Rome, 0x304
/// Milan) into the SMU: 0x05 then 0x10 (per socket, SMU DXE).
pub fn transfer_pptable<M: SmuMessages + ?Sized>(
    smu: &M,
    addr: u64,
    size: u32,
) -> Result<(), SmuError> {
    send_addr(smu, msg::SET_BIOS_DRAM_ADDR, addr)?;
    send1(smu, msg::TRANSFER_TABLE_DRAM2SMU, size).map(drop)
}

/// Rome: DMA the 0x80-byte lane-map table (content from the PCIe track).
pub fn transfer_lane_map<M: SmuMessages + ?Sized>(smu: &M, addr: u64) -> Result<(), SmuError> {
    send_addr(smu, msg::SET_BIOS_DRAM_ADDR, addr)?;
    send1(smu, rome::TRANSFER_LANE_MAP_TABLE, 0).map(drop)
}

/// Give the SMU its AGM log pages (Rome 1 page, Milan PCD 0x276 = 4).
pub fn set_tools_dram<M: SmuMessages + ?Sized>(smu: &M, addr: u64) -> Result<(), SmuError> {
    send_addr(smu, msg::SET_TOOLS_DRAM_ADDR, addr)
}

/// Optional debug DRAM (PCD 0x272 / 0x2C2 MiB, default 0 = not sent).
pub fn set_debug_dram<M: SmuMessages + ?Sized>(
    smu: &M,
    addr: u64,
    size: u32,
) -> Result<(), SmuError> {
    let mut args = [addr as u32, (addr >> 32) as u32, size, 0, 0, 0];
    smu.send(msg::SET_DEBUG_DRAM_ADDR, &mut args)
}

/// Read the 48-byte brand string.
pub fn brand_string<M: SmuMessages + ?Sized>(smu: &M) -> Result<[u8; 48], SmuError> {
    let mut out = [0u8; 48];
    for (i, chunk) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        *chunk = send1(smu, msg::GET_BRAND_STRING, i as u32)?.to_le_bytes();
    }
    Ok(out)
}

/// LCLK DPM level 2 for NBIO quadrant `nbio` (CCX-done step 4). The donor
/// default selects no NBIO.
pub fn set_lclk_dpm_level<M: SmuMessages + ?Sized>(smu: &M, nbio: u8) -> Result<(), SmuError> {
    send1(
        smu,
        msg::SET_NBIO_LCLK_DPM_LEVEL,
        (nbio as u32) << 16 | 0x202,
    )
    .map(drop)
}

/// Statically power-gate NBIF2 of quadrant `nbio` (SP3 workaround for
/// NBIOs whose NBIF2 F1 does not exist; Milan only with PCD 0x70).
pub fn nbif2_power_gate<M: SmuMessages + ?Sized>(
    smu: &M,
    generation: Generation,
    nbio: u8,
) -> Result<(), SmuError> {
    let mut args: [u32; ARGUMENT_COUNT] = match generation {
        Generation::Milan => [nbio as u32 | 0x120, 0, 0, 0, 0, 0],
        _ => [nbio as u32, 2, 1, 0, 0, 0],
    };
    smu.send(msg::SET_POWER_GATE, &mut args)
}

/// The SMU's CPPC answer (`CppcGetInfo`, message 0x17; Rome SmuV11Dxe
/// `0x1cdc`, Milan `0x2170`, sent when the CPPC feature bit is on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CppcInfo {
    /// Answer bits 1:0.
    pub revision: u8,
    /// Answer bits 31:2: bit `k` = `_CPC` register `k` is implemented.
    pub supported: u32,
}

impl CppcInfo {
    pub const fn from_answer(answer: u32) -> Self {
        Self {
            revision: (answer & 3) as u8,
            supported: answer >> 2,
        }
    }

    /// CPPC is on (answer bit 16 = `_CPC` CPPCEnable). AmdAcpiDxe also
    /// needs revision 2 or later before it builds `_CPC`.
    pub const fn enabled(&self) -> bool {
        self.supported & 1 << 14 != 0 && self.revision > 1
    }

    pub const fn implements(&self, register: u8) -> bool {
        self.supported & 1 << register != 0
    }
}

pub fn cppc_info<M: SmuMessages + ?Sized>(smu: &M) -> Result<CppcInfo, SmuError> {
    send1(smu, msg::CPPC_INFO, 0).map(CppcInfo::from_answer)
}

/// Hand the PCC shared memory to the SMU (`CppcSetTable`, 0x16).
pub fn cppc_set_table<M: SmuMessages + ?Sized>(smu: &M, base: u64) -> Result<(), SmuError> {
    send_addr(smu, msg::CPPC_SET_TABLE, base)
}

/// `(lowest, nominal)` frequency in MHz for `_CPC` entries 19/20 (Rome
/// `0x2130`: 0x3B, 0x3A; Milan: 0x3A, 0x39).
pub fn cppc_frequencies<M: SmuMessages + ?Sized>(
    smu: &M,
    generation: Generation,
) -> Result<(u32, u32), SmuError> {
    let (lowest, nominal) = match generation {
        Generation::Milan => (0x3A, 0x39),
        _ => (0x3B, 0x3A),
    };
    Ok((send1(smu, lowest, 0)?, send1(smu, nominal, 0)?))
}

/// The PCD-CPPC doorbell: `(SMU BAR & !0xFF) + 0x10540`
/// (`HarvestCppcDoorbellAddress`).
pub const fn cppc_doorbell(smu_bar: u64) -> u64 {
    (smu_bar & !0xFF) + 0x1_0540
}

/// Inputs of the CCX-done sequence (per socket, SMU DXE). Build with
/// [`CcxDonePolicy::donor`] for the donor defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CcxDonePolicy {
    /// DcBtc argument `hi << 8 | lo` (Rome PCDs 0x279/0x278, Milan
    /// 0x280/0x281); `None` = off (PCD 0x276 / 0x27F != 1).
    pub dc_btc: Option<u16>,
    /// Milan PCD 0x2C3.
    pub soc_dc_btc: bool,
    /// Milan PCD 0x292 (set by the SP3 fixup).
    pub fll_btc: bool,
    /// Milan PCD 0x290 (set by the SP3 fixup).
    pub fdd_btc: bool,
    /// Rome PCD 0x110 / Milan 0x15B; 0 = not sent.
    pub max_all_cores_freq: u32,
    /// Milan PCD 0x1EE.
    pub apml_flood: bool,
    /// Milan PCD 0x308 == 1 (default 0xFF: off).
    pub hsmp_interrupts: bool,
    /// Milan PCD 0x25C; 0 = not sent.
    pub df_irritator: u32,
}

impl CcxDonePolicy {
    /// Donor defaults: DcBtc 0x86 (Milan B0+: 0x74 from the SP3 fixup),
    /// SoC DC BTC on, FLL/FDD BTC on Milan B0+ (SP3 fixup), APML flood on.
    pub const fn donor(revision: Revision) -> Self {
        let milan_b0 = matches!(revision, Revision::GnB0 | Revision::GnB1 | Revision::GnB2);
        Self {
            dc_btc: Some(if milan_b0 { 0x74 } else { 0x86 }),
            soc_dc_btc: true,
            fll_btc: milan_b0,
            fdd_btc: milan_b0,
            max_all_cores_freq: 0,
            apml_flood: true,
            hsmp_interrupts: false,
            df_irritator: 0,
        }
    }
}

/// The CCX-done sequence for one socket (Rome `SmuV11Dxe 0x2ae8`, Milan
/// `SmuV11DxeGN 0xc40`, instruction-checked for Milan): the boot-time
/// calibrations require CC6 to be off on entry (feature enables are additive),
/// then CC6 alone is enabled; then the per-socket settings. LCLK DPM
/// ([`set_lclk_dpm_level`]) and hotplug
/// follow, and the AC BTC callback is the CCX track's.
pub fn ccx_done<M: SmuMessages + ?Sized, T: Timer + ?Sized>(
    smu: &M,
    generation: Generation,
    mask: u32,
    p: &CcxDonePolicy,
    timer: &T,
) -> Result<(), SmuError> {
    let milan = generation == Generation::Milan;
    let calibrate = p.dc_btc.is_some() || (milan && p.fll_btc);
    if calibrate {
        enable_features(smu, mask & !features::CC6, 0)?;
        if !milan {
            // Rome waits 10 ms on SP3 before DcBtc.
            timer.delay_ms(10);
        }
        if let Some(arg) = p.dc_btc {
            send1(smu, msg::DC_BTC, arg as u32)?;
        }
        if milan {
            [
                (p.soc_dc_btc, milan::SOC_DC_BTC),
                (p.fll_btc, milan::FLL_BTC),
                (p.fdd_btc, milan::FDD_BTC),
            ]
            .iter()
            .filter(|(on, _)| *on)
            .try_for_each(|&(_, id)| send1(smu, id, 0).map(drop))?;
        }
        enable_features(smu, mask & features::CC6, 0)?;
    } else {
        enable_features(smu, mask, 0)?;
    }
    if p.max_all_cores_freq != 0 {
        send1(smu, msg::SET_MAX_ALL_CORES_FREQ, p.max_all_cores_freq)?;
    }
    if milan {
        send1(smu, milan::SET_APML_FLOOD, p.apml_flood as u32)?;
        if p.hsmp_interrupts {
            send1(smu, milan::ENABLE_HSMP_INTERRUPTS, 0)?;
        }
        if p.df_irritator != 0 {
            send1(smu, milan::SET_DF_IRRITATOR, p.df_irritator)?;
        }
    }
    Ok(())
}

/// SP3 workaround inputs of the SMU DXE (`0x27a4` Rome, `0x163c` Milan).
/// The values are APCB tokens / PCDs (APCB track); `None` = not sent.
/// AGESA always sends the EDC excursion/track message.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sp3Workarounds {
    /// 0x35: Rome 1 when APCB 0x500AB == 0 and PCD 0x28F == 1; Milan PCD
    /// 0x2F7 when != 0xF.
    pub l3_credit_mode: Option<u32>,
    /// Rome 0x36: APCB 0x41315 when != 0xF.
    pub cac_weight_adj: Option<u32>,
    /// Rome 0x38 / Milan 0x44: APCB 0x500A8 when != 0xF.
    pub vid_cap: Option<u32>,
    /// Rome 0x39 / Milan 0x45: `0x500AD << 16 | 0x500AC`.
    pub edc_track: Option<u32>,
    /// Milan 0x48: PCD 0x1B1 (default 0).
    pub l1_io_drop_chain: Option<u32>,
}

/// Send the SP3 workaround messages (after the PPTable). NBIF2 power
/// gating ([`nbif2_power_gate`]) and NBIF ALT bit 11
/// (`nbio::sp3::smu_syshub_bit11`) are per NBIO.
pub fn sp3_workarounds<M: SmuMessages + ?Sized>(
    smu: &M,
    generation: Generation,
    w: &Sp3Workarounds,
) -> Result<(), SmuError> {
    let milan = generation == Generation::Milan;
    [
        (msg::SET_L3_CREDIT_MODE, w.l3_credit_mode),
        (
            rome::CAC_WEIGHT_ADJ,
            if milan { None } else { w.cac_weight_adj },
        ),
        (
            if milan {
                milan::VDDCR_CPU_LIMIT
            } else {
                rome::VID_CAP
            },
            w.vid_cap,
        ),
        (
            if milan {
                milan::SET_EDC_TRACK
            } else {
                rome::SET_EDC_EXCURSION_REPORTING
            },
            w.edc_track,
        ),
        (
            milan::SET_L1_IO_DROP_CHAIN,
            if milan { w.l1_io_drop_chain } else { None },
        ),
    ]
    .iter()
    .filter_map(|&(id, arg)| arg.map(|a| (id, a)))
    .try_for_each(|(id, a)| send1(smu, id, a).map(drop))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smu::model::{FakeTimer, NoDelay, SmuModel};
    use crate::smu::response;

    fn mailbox(m: &SmuModel, generation: Generation) -> crate::smu::Smu<SmuModel, NoDelay> {
        m.smu(id_limit(generation))
    }

    #[test]
    fn masks() {
        let d = FeaturePolicy::default();
        assert_eq!(feature_mask(Revision::SspB0, &d), 0xDA_1FFF);
        assert_eq!(feature_mask(Revision::SspA0, &d), 0xDA_1FFF & 0xFFF7_F5FF);
        assert_eq!(feature_mask(Revision::GnB1, &d), 0x6DA_FFFF);
        assert_eq!(feature_mask(Revision::ZpB2, &d), 0, "not V11");
    }

    #[test]
    fn ccx_done_milan() {
        let m = SmuModel::new(MAILBOX, response::OK);
        let mb = mailbox(&m, Generation::Milan);
        let mask = feature_mask(Revision::GnB1, &FeaturePolicy::default());
        let p = CcxDonePolicy::donor(Revision::GnB1);
        ccx_done(&mb, Generation::Milan, mask, &p, &FakeTimer::default()).unwrap();
        let sent = m.sent.borrow();
        let ids: Vec<_> = sent.iter().map(|s| s.0).collect();
        assert_eq!(ids, [0x03, 0x0C, 0x38, 0x37, 0x43, 0x03, 0x42]);
        assert_eq!(sent[0].1[0], 0x6DA_FFFF & !features::CC6);
        assert_eq!(sent[1].1[0], 0x74);
        assert_eq!(sent[5].1[0], features::CC6);
        assert_eq!(sent[6].1[0], 1);
    }

    #[test]
    fn ccx_done_rome_waits_and_rejects_milan_ids() {
        let m = SmuModel::new(MAILBOX, response::OK);
        let mb = mailbox(&m, Generation::Rome);
        let timer = FakeTimer::default();
        let p = CcxDonePolicy::donor(Revision::SspB0);
        ccx_done(&mb, Generation::Rome, features::ROME_SP3, &p, &timer).unwrap();
        assert_eq!(m.ids(), [0x03, 0x0C, 0x03]);
        assert_eq!(m.sent.borrow()[1].1[0], 0x86);
        assert!(timer.0.load(core::sync::atomic::Ordering::Relaxed) >= 10_000);
        assert_eq!(
            send1(&mb, milan::SET_HOTPLUG_TABLE_ADDR, 0),
            Err(SmuError::Rejected(response::UNKNOWN_CMD))
        );
    }

    #[test]
    fn table_transfers() {
        let m = SmuModel::new(MAILBOX, response::OK);
        let mb = mailbox(&m, Generation::Rome);
        transfer_pptable(&mb, 0x1_2345_6000, 0x27C).unwrap();
        set_lclk_dpm_level(&mb, 3).unwrap();
        nbif2_power_gate(&mb, Generation::Rome, 1).unwrap();
        let sent = m.sent.borrow();
        assert_eq!(sent[0], (0x05, [0x2345_6000, 1, 0, 0, 0, 0]));
        assert_eq!(sent[1], (0x10, [0x27C, 0, 0, 0, 0, 0]));
        assert_eq!(sent[2].1[0], 0x3_0202);
        assert_eq!(sent[3].1[..3], [1, 2, 1]);
        assert_eq!(cppc_doorbell(0xF110_0001), 0xF111_0540);
        let i = CppcInfo::from_answer(0x007F_FFFF);
        assert!(i.enabled() && i.implements(20) && !i.implements(21));
        assert!(
            !CppcInfo::from_answer(0x007E_FFFF).enabled(),
            "bit 16 clear"
        );
    }
}
