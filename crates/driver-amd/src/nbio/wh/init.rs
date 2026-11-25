// SPDX-License-Identifier: MIT OR Apache-2.0
//! The WH NBIO vendor sequence on one root bridge (IOHC instance `rb` of its
//! IOD), in the order of the Dell PR9XJ 1.0.8 openSIL (`InitializeNbioWhTp1`
//! `0xf1e3c0`, NBIO.md §2.3), with the tables of [`super::tables`]:
//!
//! | Vendor step | Here |
//! |---|---|
//! | `NbioBaseConfigurationWh` loop 1 (every RB) | [`RootBridge::early`] (the VDM bus ranges are [`RootBridge::set_vdm_bus_ranges`]) |
//! | `NbioBaseConfigurationWh` loop 3 | [`RootBridge::base_init`]: `BASE`, `BASE_B0_SP8`, A0 routes, NBIF (IOHC 0), per-RB tables |
//! | `NbioIommuInitWh` | [`RootBridge::iommu_init`] |
//! | `MpioCfgAfterDxioInitWh` per RB | [`RootBridge::perf_init`], [`RootBridge::enable_ari`] |
//! | DXE `PcieMiscInit` (after enumeration) | [`RootBridge::ras_init`], [`RootBridge::store_nbif_requester`], [`RootBridge::lock_nbif`] |
//!
//! Every register write of the vendor's code (outside its tables) is here
//! with its witness; bits of unknown meaning stay raw, as in the tables.

use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields};

use super::tables::*;
use super::{IOMMU_L1_SB_LOCATION, IOMMU_L2_SB_LOCATION, RB_SMN_STRIDE};
use super::{PRIMARY_SB_LOCATION, RootBridge};
use crate::dxio::wh::{Layout, Proxy};
use crate::nbio::iohc::{
    CFG_IOHC_PCI, CfgIohcPci, IOHC_FEATURE_CNTL, IohcBridgeCntl, IohcFeatureCntl, NB_ADAPTER_ID_W,
    NB_BUS_NUM_CNTL, NbBusNumCntl, SB_LOCATION, iohc_bridge_cntl,
};
use crate::nbio::iommu::{
    EFR_SHADOW0, EFR_SHADOW1, EfrShadow0, EfrShadow1, L2_CP_CONTROL_1, L2A_EFR_SHADOW0,
};
use crate::smn::{SmnAccess, SmnReg};
use crate::table;

/// One vendor table record; it applies when all bits of its property mask
/// are in the live property word ([`base_properties`], [`iommu_properties`],
/// [`ras_properties`]).
pub type Record = table::Record<SmnReg, u32, u32>;

/// One `MpioPerfAnalysis.c` entry (0x18 bytes: type, scope, latency mode,
/// address, clear, set). `group` is the vendor's list (0 SdpConnect, 1
/// EarlyWakeEn), `kind` the Dell entry type that selects the root bridges it
/// applies on ([`perf_applies`]), `scope` 0 an IOHC-relative address
/// (`rb << 20`), 1 a PCIe core one (`core << 20`), `latency` the latency
/// mode it belongs to ([`Policy::perf_latency`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PerfRecord {
    pub group: u8,
    pub kind: u8,
    pub scope: u8,
    pub latency: bool,
    pub address: u32,
    pub clear: u32,
    pub set: u32,
}

pub use crate::cpu::wh::Part;

/// The NBIF switches `NbifStrapsFromConfigWh` (`0xf690c0`) copies into the
/// straps, one bit each in [`NBIF_STRAPS`] order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nbif {
    /// `cfg+0x2A` (PCD 0x241): the `NBIF_OPT_2A` table. Dell 1.
    pub features: bool,
    /// `cfg+0x11` (SYSHUB MGCG hysteresis ≠ 0): `NBIF_OPT_SYSHUB_HYST`. Dell 0.
    pub syshub_hysteresis: bool,
    /// `cfg+0x90` (PCD 0x17F): `NBIF_OPT_90`. Dell 0.
    pub opt_90: bool,
    /// Bit `n` is the value of [`NBIF_STRAPS`]`[n]`.
    pub straps: u32,
    /// `cfg+0x8C`: the NBIF subsystem IDs (`0x1014_004C`, `0x1014_804C`), 0
    /// leaves them.
    pub ssid: u32,
}

/// The straps of [`Nbif::straps`]: `(register, bit, input block offset)`,
/// in the vendor's order. `0x1014_16E2` is unaligned in both donors [I]; it is
/// written as they write it ([`RootBridge::nbif_init`]).
pub const NBIF_STRAPS: [(u32, u8, u8); 21] = [
    (0x1013_1008, 2, 0x49),
    (0x1013_1008, 7, 0x4a),
    (0x1013_1014, 26, 0x39),
    (0x1013_420c, 17, 0x30),
    (0x1013_1014, 23, 0x36),
    (0x1013_1014, 24, 0x37),
    (0x1013_1014, 25, 0x38),
    (0x1013_1014, 27, 0x3a),
    (0x1013_1014, 28, 0x3b),
    (0x1013_1014, 29, 0x3c),
    (0x1013_1014, 31, 0x3d),
    (0x1014_16e2, 0, 0x43),
    (0x1014_16e2, 1, 0x44),
    (0x1014_16e2, 2, 0x45),
    (0x1014_16e2, 3, 0x46),
    (0x1014_16e2, 4, 0x47),
    (0x1014_16e2, 5, 0x48),
    (0x1013_4200, 28, 0x9a),
    (0x1014_17c8, 16, 0x9c),
    (0x1014_17b0, 31, 0x33),
    (0x1014_17b8, 0, 0x41),
];

/// Dell's build defaults of [`NBIF_STRAPS`] (`HostToSilNbioBlock` `0xf0dd50`):
/// `+0x49/4A` 1, `+0x39` 1, `+0x30` 1, `+0x36..3D` 1,1,1,1,1,0,1,1, `+0x43..48`
/// 1,0,1,1,1,0, `+0x9A` 1, `+0x33` 0, `+0x41` 0; `+0x9C` is not in the
/// decoded block and stays 0.
const NBIF_STRAPS_DELL: [bool; 21] = [
    true, true, true, true, true, true, true, true, false, true, true, true, false, true, true,
    true, false, true, false, false, false,
];

const fn mask(bits: &[bool]) -> u32 {
    let mut mask = 0;
    let mut i = 0;
    while i < bits.len() {
        if bits[i] {
            mask |= 1 << i;
        }
        i += 1;
    }
    mask
}

/// The RAS policy of `NbioRASControl` (AmdNbioDxe `0x13580`). The donors'
/// live values are not decoded; the defaults program the `RAS` table only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ras {
    /// PCD 0x1E1 (1: RAS property bit 1, else bit 0); non-zero with
    /// [`Self::pcd_18`] clear applies `RAS_SYNC_FLOOD`.
    pub pcd_1e1: u8,
    /// PCD 0x1E0: RAS property bit 4 (the sync-flood records of
    /// `RAS_SYNC_FLOOD`).
    pub pcd_1e0: bool,
    /// PCD 0x18: suppresses `RAS_SYNC_FLOOD`.
    pub pcd_18: bool,
}

/// The board's NBIO choices. [`Default`] is the Dell PR9XJ build default of
/// the openSIL NBIO input block (`HostToSilNbioBlock`, NBIO.md §2.5) where it
/// is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// `cfg+0x09` (PCD 0x108): TPH completer, base property bit 2. Dell 1.
    pub tph_completer: bool,
    /// `cfg+0x19` (PCD 0x145): the APIC mode, 1 xAPIC, 2 x2APIC (IOMMU
    /// property 0x40 / 0xC0, XT and `0x0240_001C[18]`). Dell 2.
    pub apic_mode: u8,
    /// `cfg+0x1A`: SNPAVICSup, EFR shadow `[23:21]` when SNP is off. Dell 1.
    pub snp_avic: u8,
    /// `cfg+0x1F` (PCD 0x223): the IOMMUs. Dell 1.
    pub iommu: bool,
    /// `cfg+0x78..0x7F`: per IOMMU enable, bit `socket * 4 + die * 2 + (rb != 0)`
    /// (only the performance tables test it). Dell all 1.
    pub iommu_instances: u8,
    /// `cfg+0x51` (PCD 0x23F): SNP, IOMMU property 0x100. Dell 0.
    pub snp: bool,
    /// `cfg+0x80` (DynEx 0x500AC): IOMMU property 0x200. Dell 0.
    pub iommu_200: bool,
    /// IOMMU property 0x20 (RcMgr input byte `+0x3BA0` == 0, NBIO.md Q6):
    /// `0x13F0_00E8[2]`. On by default, as fstart always set that bit.
    pub iommu_20: bool,
    /// `cfg+0x92`: EFR shadow 1 bit 5. Dell 0.
    pub iommu_efr_bit5: bool,
    /// `cfg+0x9D`: `0x1570_00C4 = 0x0200_1000`, `0x1570_00C8 = 0x0800_0000`
    /// before the IOMMU tables. Dell 0.
    pub iommu_l2a_c4: bool,
    /// `cfg+0x84`, `cfg+0x88`: IOMMU and NB subsystem IDs, 0 leaves them.
    pub iommu_ssid: u32,
    pub nb_ssid: u32,
    /// `cfg+0x91`: the latency (true) rather than the performance entries of
    /// the performance tables. Dell 0.
    pub perf_latency: bool,
    /// MPIO `cfg+0x35`: ARI (`IOHC_FEATURE_CNTL[22]`) after training.
    pub ari: bool,
    pub nbif: Nbif,
    pub ras: Ras,
    /// PCD 0x212: PCIe AER enable, SMU message 9 argument 0 at ready-to-boot.
    pub aer: bool,
    /// PCD 0x1E6 (AmdPcieAerReportMechanism): SMU message 9 argument 1.
    /// Dell 1 (CXL input block default).
    pub aer_mechanism: u8,
    /// `MPIO_ENABLE_CG` arguments at ready-to-boot: bits 20 and 14 of the
    /// SMU feature PCD 0xFE and the unused-tile clock gating PCD 0x367 (live
    /// values not decoded; off).
    pub mpio_clock_gating: [bool; 3],
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            tph_completer: true,
            apic_mode: 2,
            snp_avic: 1,
            iommu: true,
            iommu_instances: 0xff,
            snp: false,
            iommu_200: false,
            iommu_20: true,
            iommu_efr_bit5: false,
            iommu_l2a_c4: false,
            iommu_ssid: 0,
            nb_ssid: 0,
            perf_latency: false,
            ari: false,
            nbif: Nbif {
                features: true,
                syshub_hysteresis: false,
                opt_90: false,
                straps: mask(&NBIF_STRAPS_DELL),
                ssid: 0,
            },
            ras: Ras::default(),
            aer: false,
            aer_mechanism: 1,
            mpio_clock_gating: [false; 3],
        }
    }
}

/// The property bits the WH tables test.
pub mod property {
    /// Base word: TPH completer (`cfg+0x09`).
    pub const TPH_COMPLETER: u32 = 1 << 2;
    /// Base word: presence bits 16..25, one per descriptor flag (0 IOHC,
    /// 1 SYSHUB, 2..4 NBIF0..2, 5 SDPMUX, 6 SST, 7..9 unknown, NBIO.md Q4).
    pub const PRESENT: u32 = 0x03ff_0000;
    pub const PRESENT_IOHC: u32 = 1 << 16;
    pub const PRESENT_NBIF0: u32 = 1 << 18;
    pub const PRESENT_NBIF2: u32 = 1 << 20;
    pub const PRESENT_SST: u32 = 1 << 22;
    /// Base word: bits 22..25, forced to whether the IOHC has a PCIe
    /// wrapper. The tables use 22 for SST records, 23 for the IOAPIC routes,
    /// 24 for the per-RB tables.
    pub const PCIE: u32 = 0x03c0_0000;
    /// IOMMU word.
    pub const IOMMU_DISABLED: u32 = 0x01;
    pub const IOMMU_20: u32 = 0x20;
    pub const APIC_MODE_1: u32 = 0x40;
    pub const APIC_MODE_2: u32 = 0xc0;
    pub const SNP: u32 = 0x100;
    pub const IOMMU_200: u32 = 0x200;
    pub const IOMMU_PCIE: u32 = 0x0200_0000;
    /// RAS word.
    pub const RAS_1E1_OTHER: u32 = 0x01;
    pub const RAS_1E1_ONE: u32 = 0x02;
    pub const RAS_1E0: u32 = 0x10;
}

/// The base property word of IOHC `rb` (`NbioBaseConfigurationWh` loop 3,
/// identical in both donors): IOHCs other than 0 lack NBIF0, NBIF2 and the
/// SST (descriptor flags `0x54`), then bits 22..25 follow the PCIe wrapper.
pub const fn base_properties(rb: u8, part: Part, policy: &Policy) -> u32 {
    use property::*;
    let absent = if rb != 0 { 0x54 } else { 0 };
    let mut live = !(absent << 16) & PRESENT;
    if policy.tph_completer && live & PRESENT_IOHC != 0 {
        live |= TPH_COMPLETER;
    }
    if part.has_pcie(rb) {
        live | PCIE
    } else {
        live & !PCIE
    }
}

/// The IOMMU property word of IOHC `rb` (`NbioIommuInitWh`).
pub const fn iommu_properties(rb: u8, part: Part, policy: &Policy) -> u32 {
    use property::*;
    let mut live = match policy.apic_mode {
        1 => APIC_MODE_1,
        2 => APIC_MODE_2,
        _ => 0,
    };
    if !policy.iommu {
        live |= IOMMU_DISABLED;
    }
    if policy.snp {
        live |= SNP;
    }
    if policy.iommu_200 {
        live |= IOMMU_200;
    }
    if policy.iommu_20 {
        live |= IOMMU_20;
    }
    if part.has_pcie(rb) {
        live |= IOMMU_PCIE;
    }
    live
}

/// The RAS property word of IOHC `rb` (AmdNbioDxe `NbioRASControl`).
pub const fn ras_properties(rb: u8, part: Part, policy: &Policy) -> u32 {
    use property::*;
    let mut live = base_properties(rb, part, policy) & !TPH_COMPLETER;
    live |= if policy.ras.pcd_1e1 == 1 {
        RAS_1E1_ONE
    } else {
        RAS_1E1_OTHER
    };
    if policy.ras.pcd_1e0 {
        live |= RAS_1E0;
    }
    live
}

/// Whether performance entry `kind` (a Dell type) applies on IOHC `rb`
/// (`NbioPerfAnalysisSil`; GIGABYTE `0x709b9eb3` decoded, its types shifted
/// to Dell's): `iommu` is the IOMMU enable of this IOHC, `fuses` its IOD's.
pub const fn perf_applies(kind: u8, rb: u8, has_pcie: bool, iommu: bool, fuses: Fuses) -> bool {
    let rb_ok = match kind {
        0x05 | 0x0b | 0x11 | 0x12 => rb == 0,
        0x06 | 0x0c => rb == 1,
        0x07 => rb <= 1,
        0x08 => rb == 2 || rb == 3,
        0x09 | 0x0e => rb == 3,
        0x0d => rb == 1 || rb == 3,
        0x01 => rb == 1 && !fuses.no_l1_pcie1,
        0x02 => rb == 0 && !fuses.no_l1_sp8,
        _ => true,
    };
    let iommu_ok = !matches!(kind, 0..=3) || iommu;
    let pcie_ok = !matches!(kind, 0x00..=0x02 | 0x0a..=0x0c | 0x10) || has_pcie;
    rb_ok && iommu_ok && pcie_ok
}

/// The performance table of `part` (`GetPerfDataV1` `0xf7ffa0`).
pub const fn perf_table(part: Part) -> &'static [PerfRecord] {
    match part.layout() {
        Layout::Sp7A0 => PERF_SP7_A0,
        Layout::Sp7B0 => PERF_SP7_B0,
        Layout::Sp8 => PERF_SP8,
    }
}

register_bitfields![u32,
    /// SMN 0x13B0_0068 (32-bit): the IOD's NB fuses the NBIO code reads
    /// (absolute, IOHC 0's).
    pub NbFuse [
        /// No IOMMU L1 PCIE1 (`0x14C0_0000`; `0x14B0_0000` on A0).
        NO_L1_PCIE1 OFFSET(5) NUMBITS(1) [],
        /// No SP8 IOMMU L1 (`0x14B0_0000`).
        NO_L1_SP8 OFFSET(6) NUMBITS(1) [],
        /// Both set: NbifDaccHarvestOptions sets `0x0141_F048[11]`.
        DACC_HARVEST_A OFFSET(8) NUMBITS(2) [],
        /// Both set: `0x0141_F048[10]`, `0x0140_E00C[11]`, `0x1013_4200[28] = 0`.
        DACC_HARVEST_B OFFSET(11) NUMBITS(2) []
    ],
    /// SMN 0x13B0_0100 (32-bit): the NB's PCI IDs.
    pub NbDeviceId [
        VENDOR OFFSET(0) NUMBITS(16) [],
        DEVICE OFFSET(16) NUMBITS(16) []
    ],
    /// SMN 0x1013_101C / 0x1013_121C (32-bit): the PCI identity of the
    /// bridge in front of NBIF0 / its second function. The low byte is
    /// `0x80 | function` in both donors (meaning inferred from the values).
    pub NbifUpstream [
        FUNCTION_LOW OFFSET(0) NUMBITS(3) [],
        VALID OFFSET(7) NUMBITS(1) [],
        BUS OFFSET(16) NUMBITS(8) [],
        DEVICE OFFSET(24) NUMBITS(5) [],
        FUNCTION OFFSET(29) NUMBITS(3) []
    ],
    /// SMN 0x1012_37FC (32-bit): the requester of the endpoint behind
    /// NBIF0's bridge (`StoreNbifEpRequesterId`, AmdNbioDxe `0xf460`).
    pub NbifEpRequester [
        BUS OFFSET(0) NUMBITS(8) [],
        DEVICE OFFSET(8) NUMBITS(5) [],
        SEGMENT OFFSET(16) NUMBITS(8) []
    ],
    /// SMN 0x1013_11AC / 0x1013_13AC (32-bit): NBIF HW-init lock
    /// (`NbioBaseSetHwInitLock`, AmdNbioDxe `0x5290`).
    pub NbifHwInitLock [
        LOCK OFFSET(0) NUMBITS(1) []
    ],
    /// SMN 0x13F0_007C (32-bit): L2 register the caps step programs.
    pub IommuL2Cap7c [
        /// Set to 1 on parts other than A0 (`NbioIommuCapsWh` `0xf34760`).
        FIELD_7_4 OFFSET(4) NUMBITS(4) []
    ],
    /// SMN 0x13F0_115C (32-bit): L2_CP_CONTROL_1 low half.
    pub L2CpControl1 [
        LOW OFFSET(0) NUMBITS(16) []
    ]
];

/// SMN 0x13B0_0100: the NB's PCI vendor and device ID (D0F0x00 alias),
/// whose device ID SP8 rewrites.
pub const NB_DEVICE_ID: SmnReg<NbDeviceId::Register> = SmnReg::new(0x13b0_0100);
/// SP8's NB device ID.
pub const SP8_DEVICE_ID: u32 = 0x1b30;
/// SMN 0x13B0_0068: NB fuses (absolute).
pub const NB_FUSE: SmnReg<NbFuse::Register> = SmnReg::new(0x13b0_0068);
const NBIF_UPSTREAM: [(SmnReg<NbifUpstream::Register>, u32); 2] =
    [(SmnReg::new(0x1013_101c), 1), (SmnReg::new(0x1013_121c), 2)];
/// Device of the NBIF0 upstream bridge on the IOHC's root bus (Genoa: 7).
pub const NBIF_DEVICE: u8 = 8;
const NBIF_EP_REQUESTER: SmnReg<NbifEpRequester::Register> = SmnReg::new(0x1012_37fc);
const NBIF_HW_INIT_LOCK: [SmnReg<NbifHwInitLock::Register>; 2] =
    [SmnReg::new(0x1013_11ac), SmnReg::new(0x1013_13ac)];
const IOMMU_L2_7C: SmnReg<IommuL2Cap7c::Register> = SmnReg::new(0x13f0_007c);
const IOMMU_SSID: SmnReg = SmnReg::new(0x13f0_0078);
const L2_CP_CONTROL_1_LOW: SmnReg<L2CpControl1::Register> = SmnReg::new(L2_CP_CONTROL_1.address());
/// L2A register the `cfg+0x9D` step and parts other than A0 write.
const IOMMU_L2A_C4: SmnReg = SmnReg::new(0x1570_00c4);
const IOMMU_L2A_C8: SmnReg = SmnReg::new(0x1570_00c8);
/// D0F2 (IOMMU) config register the caps step ORs `0x84` into when the APIC
/// mode is not 1.
const IOMMU_CONFIG_80: u16 = 0x80;
/// DXE `PcieClearRASRegisters` (`0xe6e0`) and `NbioRASControl`.
const RAS_0800: SmnReg = SmnReg::new(0x13b2_0800);
const RAS_0DC0: SmnReg = SmnReg::new(0x13b2_0dc0);
const RAS_0DC4: SmnReg = SmnReg::new(0x13b2_0dc4);
const RAS_0100: SmnReg = SmnReg::new(0x13b2_0100);
const RAS_0000: SmnReg = SmnReg::new(0x13b2_0000);
const RAS_0808: SmnReg = SmnReg::new(0x13b2_0808);
/// NbifDaccHarvestOptions (`0xf68ee0`) targets.
const DACC_F048: SmnReg = SmnReg::new(0x0141_f048);
const DACC_E00C: SmnReg = SmnReg::new(0x0140_e00c);
const NBIF_4200: SmnReg = SmnReg::new(0x1013_4200);

/// The IOMMU fuses of an IOD as the vendor tracks them: once seen on any
/// root bridge they stay set for the later ones ("sticky").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Fuses {
    pub no_l1_pcie1: bool,
    pub no_l1_sp8: bool,
}

impl Fuses {
    pub fn read(raw: u32) -> Self {
        let fuse = LocalRegisterCopy::<u32, NbFuse::Register>::new(raw);
        Self {
            no_l1_pcie1: fuse.is_set(NbFuse::NO_L1_PCIE1),
            no_l1_sp8: fuse.is_set(NbFuse::NO_L1_SP8),
        }
    }

    pub fn merge(&mut self, other: Self) {
        self.no_l1_pcie1 |= other.no_l1_pcie1;
        self.no_l1_sp8 |= other.no_l1_sp8;
    }
}

impl<P: ConfigRegionAccess + Clone> RootBridge<P> {
    /// Apply `table` under `live`, at `rb << 20` (`shifted`) or absolute.
    pub fn apply(&self, table: &[Record], live: u32, shifted: bool) {
        let offset = if shifted {
            u32::from(self.iohc) * RB_SMN_STRIDE
        } else {
            0
        };
        table::apply_when(
            table.iter().copied(),
            |&mask| live & mask == mask,
            |reg: SmnReg| self.smn.reg(reg.offset(offset)),
        );
    }

    /// Whether this IOHC is its IOD's FCH IOHC (instance 0; `WhHasFch`
    /// `0xf2da00`), which keeps the SB location on every IOD of every socket.
    pub fn sb_present(&self) -> bool {
        self.iohc == 0
    }

    /// `NbioBaseConfigurationWh` loop 1 on this IOHC, in the vendor's order
    /// (the VDM table between the bus number and the SB location is
    /// [`Self::set_vdm_bus_ranges`]):
    /// - `NB_BUS_NUM_CNTL` = bus, LAT_MODE (segment 0);
    /// - SP8: `0x13B0_0100[31:16] = 0x1B30` (the device ID);
    /// - SB location: on IOHCs other than the FCH one `0x13B1_007C`, the L1
    ///   (`0x1530_0024`, with a PCIe wrapper) and L2 (`0x13F0_112C`) copies
    ///   are cleared, so that configuration requests nothing claims do not go
    ///   to an FCH link the IOHC does not have; the FCH IOHC's L1/L2 copies
    ///   get `0x4_0001`, its own SB_LOCATION is left;
    /// - the NB subsystem IDs (`0x13B0_0050`) when the policy has them;
    /// - every root port bridge hidden ([`Self::hide_bridges`]).
    pub fn early(&self, part: Part, policy: &Policy) {
        self.reg(NB_BUS_NUM_CNTL)
            .modify(NbBusNumCntl::BUS.val(self.bus().into()) + NbBusNumCntl::LAT_MODE::SET);
        if part.sp8() {
            self.reg(NB_DEVICE_ID)
                .modify(NbDeviceId::DEVICE.val(SP8_DEVICE_ID));
        }
        let pcie = part.has_pcie(self.iohc);
        if self.sb_present() {
            if pcie {
                self.reg(IOMMU_L1_SB_LOCATION).set(PRIMARY_SB_LOCATION);
            }
            self.reg(IOMMU_L2_SB_LOCATION).set(PRIMARY_SB_LOCATION);
        } else {
            self.reg(SB_LOCATION).set(0);
            if pcie {
                self.reg(IOMMU_L1_SB_LOCATION).set(0);
            }
            self.reg(IOMMU_L2_SB_LOCATION).set(0);
        }
        if policy.nb_ssid != 0 {
            self.reg(NB_ADAPTER_ID_W).set(policy.nb_ssid);
        }
        self.hide_bridges(part);
    }

    /// `NbioHideAllBridgesWh` (`0xf33060`): hide bridges 8..=21 (0..=21 on
    /// A0) and, on parts other than A0, bridges 29 and 44, then show every
    /// port of the bonus core (core 6) again if this IOHC has it. MPIO later
    /// shows the trained ports.
    pub fn hide_bridges(&self, part: Part) {
        let hide = IohcBridgeCntl::BRIDGE_DIS::SET
            + IohcBridgeCntl::CFG_DIS::SET
            + IohcBridgeCntl::CRS_ENABLE::SET;
        let first = if part.a0() { 0 } else { 8 };
        for bridge in first..=21 {
            self.reg(iohc_bridge_cntl(bridge)).modify(hide);
        }
        if !part.a0() {
            for bridge in [29, 44] {
                self.reg(iohc_bridge_cntl(bridge)).modify(hide);
            }
        }
        let (rb, _, first, end) = part.layout().bonus_core();
        if part.layout().iohc(rb) == self.iohc {
            for bridge in first..end {
                self.reg(iohc_bridge_cntl(usize::from(bridge))).modify(
                    IohcBridgeCntl::BRIDGE_DIS::CLEAR
                        + IohcBridgeCntl::CFG_DIS::CLEAR
                        + IohcBridgeCntl::CRS_ENABLE::SET,
                );
            }
        }
    }

    /// `NbioBaseConfigurationWh` loop 3 on this IOHC: `BASE`, then
    /// `BASE_B0_SP8`, the A0 IOAPIC routes of bridges 16..23 (B0 programs
    /// 0..15 only, in `BASE`; NBIO.md Q3), on the FCH IOHC the NBIF
    /// ([`Self::nbif_init`]), then the per-IOHC tables (not A0, absolute;
    /// `NbioPerRbTablesWh` `0xf332f0` keys them on the GnbHandle instance, so
    /// SP7 B0's IOHC 3 gets `PER_RB3_*`, which the generated gate comments
    /// call SP8 only).
    pub fn base_init(&self, part: Part, policy: &Policy) {
        let live = base_properties(self.iohc, part, policy);
        self.apply(BASE, live, true);
        if part.base_b0_sp8() {
            self.apply(BASE_B0_SP8, live, true);
        }
        if part.a0() {
            self.apply(BASE_A0_IOAPIC_ROUTES, live, true);
        }
        if self.iohc == 0 {
            self.nbif_init(part, policy, live);
        }
        if !part.a0() {
            let tables: &[&[Record]] = match self.iohc {
                0 => &[PER_RB0_A, PER_RB0_B],
                1 => &[PER_RB1_A, PER_RB1_B],
                3 => &[PER_RB3_A, PER_RB3_B],
                _ => &[],
            };
            for table in tables {
                self.apply(table, live, false);
            }
        }
    }

    /// `NbioNbifInitSilWh` (`0xf69730`) on the FCH IOHC: the NBIF tables, the
    /// upstream identities (bus:8.1, 8.2; Genoa 7.1, 7.2), the straps from
    /// the policy and the DACC harvest fix-ups. Function hiding is in
    /// `NBIF_FUNC_ENABLE`.
    pub fn nbif_init(&self, part: Part, policy: &Policy, live: u32) {
        self.apply(NBIF_A, live, false);
        self.apply(NBIF_FUNC_ENABLE, live, false);
        if part.sp8() {
            self.apply(NBIF_SP8, live, false);
        }
        self.apply(NBIF_INTR_LINE, live, false);
        self.apply(NBIF_DEV_STRAPS, live, false);
        // FUN_709ba8d0: `(old & 0xFFFF) | identity`, the low byte ORed in.
        for (reg, function) in NBIF_UPSTREAM {
            let identity = NbifUpstream::FUNCTION_LOW.val(function)
                + NbifUpstream::VALID::SET
                + NbifUpstream::BUS.val(self.bus().into())
                + NbifUpstream::DEVICE.val(NBIF_DEVICE.into())
                + NbifUpstream::FUNCTION.val(function);
            let reg = self.smn.reg(reg);
            reg.set((reg.get() & 0xffff) | identity.value);
        }
        self.apply(NBIF_STRAPS_B, live, false);
        self.apply(NBIF_STRAPS_C, live, false);
        self.apply(NBIF_SYSHUB, live, false);
        self.apply(NBIF_STRAPS_D, live, false);

        // NbifStrapsFromConfigWh (0xf690c0): one RMW per register, in order.
        let nbif = &policy.nbif;
        let mut i = 0;
        while i < NBIF_STRAPS.len() {
            let address = NBIF_STRAPS[i].0;
            let (mut clear, mut set) = (0u32, 0u32);
            while i < NBIF_STRAPS.len() && NBIF_STRAPS[i].0 == address {
                let bit = NBIF_STRAPS[i].1;
                clear |= 1 << bit;
                if nbif.straps & (1 << i) != 0 {
                    set |= 1 << bit;
                }
                i += 1;
            }
            // `0x1014_16E2` is not dword aligned: both donors pass it to the
            // index register as is (how the hardware decodes it is not
            // known).
            self.smn.modify_unaligned(address, clear, set);
            if address == 0x1013_420c && nbif.ssid != 0 {
                for ssid in [0x1014_004c, 0x1014_804c] {
                    self.smn.reg(SmnReg::<()>::new(ssid)).set(nbif.ssid);
                }
            }
        }
        if nbif.features {
            self.apply(NBIF_OPT_2A, live, false);
        }
        if nbif.syshub_hysteresis {
            self.apply(NBIF_OPT_SYSHUB_HYST, live, false);
        }
        if nbif.opt_90 {
            self.apply(NBIF_OPT_90, live, false);
        }

        // NbifDaccHarvestOptions (0xf68ee0, Dell only).
        let fuse = self.smn.reg(NB_FUSE);
        if fuse.read(NbFuse::DACC_HARVEST_A) == 3 {
            let reg = self.smn.reg(DACC_F048);
            reg.set(reg.get() | 1 << 11);
        }
        if fuse.read(NbFuse::DACC_HARVEST_B) == 3 {
            for (reg, clear, set) in [
                (DACC_F048, 0, 1 << 10),
                (DACC_E00C, 0, 1 << 11),
                (NBIF_4200, 1 << 28, 0),
            ] {
                let reg = self.smn.reg(reg);
                reg.set((reg.get() & !clear) | set);
            }
        }
    }

    /// `NbioIommuInitWh` (`0xf34e30`) on this IOHC, in order: the IOD's
    /// fuses (merged into `fuses`), the `cfg+0x9D` L2A writes, D0F2 hidden
    /// when the IOMMU is off, on IOHC 0 the L1 of PCIE1 (`0x14C0_0000`; A0
    /// `0x14B0_0000`) and on SP8 its own L1, then on IOHCs 0 and 1 the
    /// `IOMMU` table with the B0 tables and the capability step. The BAR is
    /// [`Self::map_iommu`]'s (the vendor's PEI writes its low half only and
    /// enables it in DXE; fstart enables it at once).
    pub fn iommu_init(&self, part: Part, policy: &Policy, fuses: &mut Fuses) {
        let rb = self.iohc;
        let live = iommu_properties(rb, part, policy);
        fuses.merge(Fuses::read(self.smn.reg(NB_FUSE).get()));
        if policy.iommu_l2a_c4 {
            self.reg(IOMMU_L2A_C4).set(0x0200_1000);
            self.reg(IOMMU_L2A_C8).set(0x0800_0000);
        }
        if !policy.iommu {
            self.reg(CFG_IOHC_PCI)
                .modify(CfgIohcPci::D0F2_ENABLE::CLEAR);
        }
        if rb == 0 && !fuses.no_l1_pcie1 {
            let l1 = if part.a0() {
                IOMMU_L1_PCIE1_A0
            } else {
                IOMMU_L1_PCIE1_B0
            };
            self.apply(l1, live, false);
        }
        if rb == 0 && !fuses.no_l1_sp8 && matches!(part.layout(), Layout::Sp8) {
            self.apply(IOMMU_L1_SP8, live, false);
        }
        if rb > 1 {
            return;
        }
        if !part.a0() {
            self.reg(IOMMU_L2A_C4).set(0x3000_0000);
        }
        self.apply(IOMMU, live, true);
        if !part.a0() {
            self.apply(IOMMU_B0_A, live, true);
            self.apply(IOMMU_B0_B, live, true);
        }
        self.iommu_caps(part, policy);
        let control = if part.has_pcie(rb) { 0 } else { 6 };
        let sp7 = !matches!(part.layout(), Layout::Sp8);
        let one = (rb == 0 && (sp7 || fuses.no_l1_sp8)) || (rb == 1 && fuses.no_l1_pcie1);
        self.reg(L2_CP_CONTROL_1_LOW)
            .modify(L2CpControl1::LOW.val(control | u32::from(one)));
    }

    /// `NbioIommuCapsWh` (`0xf34760`).
    fn iommu_caps(&self, part: Part, policy: &Policy) {
        if !part.a0() {
            self.reg(IOMMU_L2_7C).modify(IommuL2Cap7c::FIELD_7_4.val(1));
        }
        if policy.apic_mode != 1 {
            let addr = PciAddress::new(0, self.bus(), 0, 2);
            // SAFETY: D0F2 of this root bridge, the IOMMU function.
            unsafe {
                let value = self.pci.read(addr, IOMMU_CONFIG_80);
                self.pci.write(addr, IOMMU_CONFIG_80, value | 0x84);
            }
        }
        self.reg(EFR_SHADOW1)
            .modify(EfrShadow1::BIT5.val(policy.iommu_efr_bit5.into()));
        if !policy.snp {
            let gam = EfrShadow0::GAM.val(policy.snp_avic.into());
            self.reg(EFR_SHADOW0).modify(gam);
            self.reg(L2A_EFR_SHADOW0).modify(gam);
        }
        // The vendor also requires a CPU feature bit (NBIO.md Q6).
        self.reg(EFR_SHADOW1)
            .modify(EfrShadow1::BIT31.val(policy.snp.into()));
        if policy.iommu_ssid != 0 {
            self.reg(IOMMU_SSID).set(policy.iommu_ssid);
        }
    }

    /// `NbioPerfAnalysisSil` (`0xf806b0`) after training: the entries of
    /// `table` of the policy's latency mode that apply on this IOHC
    /// ([`perf_applies`]); IOHC-relative ones at `rb << 20`, PCIe-core ones
    /// (scope 1) on each core of this IOHC through `proxy`. `socket`/`die`
    /// select the IOMMU enable bit ([`Policy::iommu_instances`]).
    pub fn perf_init<A: SmnAccess>(
        &self,
        part: Part,
        policy: &Policy,
        table: &[PerfRecord],
        (socket, die): (u8, u8),
        proxy: &Proxy<'_, A>,
    ) -> Result<(), crate::dxio::rs::mailbox::Error> {
        let rb = self.iohc;
        let bit = socket * 4 + die * 2 + u8::from(rb != 0);
        let iommu = policy.iommu && policy.iommu_instances & (1 << bit) != 0;
        let fuses = Fuses::read(self.smn.reg(NB_FUSE).get());
        let pcie = part.has_pcie(rb);
        for e in table.iter().filter(|e| e.latency == policy.perf_latency) {
            match e.scope {
                0 if perf_applies(e.kind, rb, pcie, iommu, fuses) => {
                    let reg = self.reg(SmnReg::<()>::new(e.address));
                    reg.set((reg.get() & !e.clear) | e.set);
                }
                1 => {
                    for w in part.wrappers(rb) {
                        proxy.rmw(
                            e.address + (u32::from(w.core) << 20),
                            e.clear,
                            e.set,
                            crate::dxio::wh::Width::Dword,
                        )?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// `InitAriWh` (`0xf65300`): ARI on this IOHC when the policy asks.
    pub fn enable_ari(&self, policy: &Policy) {
        if policy.ari {
            self.reg(IOHC_FEATURE_CNTL)
                .modify(IohcFeatureCntl::ARI::SET);
        }
    }

    /// The RAS part of DXE `PcieMiscInit` after enumeration:
    /// `PcieClearRASRegisters` (`0xe6e0`) and `NbioRASControl` (`0x13580`):
    /// the `RAS` table, optionally `RAS_SYNC_FLOOD`, and the parity/poison
    /// controls.
    pub fn ras_init(&self, part: Part, policy: &Policy, first_of_system: bool) {
        for w1c in [RAS_0800, RAS_0DC0] {
            let reg = self.reg(w1c);
            reg.set(reg.get());
        }
        self.reg(RAS_0DC4).set(self.reg(RAS_0DC4).get() & !(1 << 8));
        self.reg(RAS_0100).set(self.reg(RAS_0100).get() | 1);

        let live = ras_properties(self.iohc, part, policy);
        self.apply(RAS, live, true);
        self.reg(RAS_0000).set(0);
        self.reg(RAS_0808).set(self.reg(RAS_0808).get() & !0x6);
        if policy.ras.pcd_1e1 != 0 && !policy.ras.pcd_18 {
            self.apply(RAS_SYNC_FLOOD, live, true);
        }
        self.reg(RAS_0100).set(self.reg(RAS_0100).get() & !(1 << 4));
        if first_of_system {
            self.reg(RAS_0DC4)
                .set(self.reg(RAS_0DC4).get() & !(1 << 12));
        }
    }

    /// `StoreNbifEpRequesterId` (AmdNbioDxe `0xf460`) after enumeration, FCH
    /// IOHC only: the requester of the device 0 behind NBIF0's bridge
    /// (bus:8.1), if one answers there.
    pub fn store_nbif_requester(&self) {
        if self.iohc != 0 {
            return;
        }
        let bridge = PciAddress::new(0, self.bus(), NBIF_DEVICE, 1);
        // SAFETY: config reads of this root bridge's NBIF bridge and of the
        // bus it forwards.
        let secondary = unsafe { (self.pci.read(bridge, 0x18) >> 8) as u8 };
        if secondary == 0 || secondary == 0xff {
            return;
        }
        let id = unsafe { self.pci.read(PciAddress::new(0, secondary, 0, 0), 0) };
        if id & 0xffff == 0xffff {
            return;
        }
        self.reg(NBIF_EP_REQUESTER).modify(
            NbifEpRequester::SEGMENT.val(0)
                + NbifEpRequester::DEVICE.val(0)
                + NbifEpRequester::BUS.val(secondary.into()),
        );
    }

    /// `NbioHwLock` (AmdNbioDxe `0x53b0`) after enumeration: the NBIF HW-init
    /// lock on IOHCs of even instance, at `rb << 20` (NBIO.md §2.11).
    pub fn lock_nbif(&self) {
        if self.iohc & 1 == 0 {
            for reg in NBIF_HW_INIT_LOCK {
                self.reg(reg).modify(NbifHwInitLock::LOCK::SET);
            }
        }
    }

    /// The ready-to-boot PCIe lock (`PcieLockRegistersCallBack` `0xf640`,
    /// `PcieLockCoreRegisters` `0xe940`): `0x1C48_0060 + rc << 20` bit 0 of
    /// every PCIe core of this IOHC, through `proxy`.
    pub fn lock_pcie<A: SmnAccess>(
        &self,
        part: Part,
        proxy: &Proxy<'_, A>,
    ) -> Result<(), crate::dxio::rs::mailbox::Error> {
        for w in part.wrappers(self.iohc) {
            super::pcie::lock_core(proxy, w.rc)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxio::rs::mailbox::{CONTROL, Poll};
    use crate::dxio::wh::proxy_firmware;
    use crate::nbio::ioapic::br_interrupt_routing;
    use crate::smn::model::Model;
    use crate::smn::testing::SmnSpace;
    use fstart_core::services::timer::ManualTimer;

    const B0: Part = Part::from_cpuid(0x00b6_0f00, 2 << 28);
    const B0_51: Part = Part::from_cpuid(0x00b5_0f10, 2 << 28);
    const SP8: Part = Part::from_cpuid(0x00b9_0f00, 4 << 28);
    const S: u32 = RB_SMN_STRIDE;

    fn smn(model: &Model, address: u32) -> u32 {
        model
            .0
            .borrow()
            .smn
            .get(&(0x20, address))
            .copied()
            .unwrap_or(0)
    }
    fn set(model: &Model, address: u32, value: u32) {
        model.0.borrow_mut().smn.insert((0x20, address), value);
    }

    #[test]
    fn properties_follow_the_vendor_formulas() {
        let policy = Policy::default();
        assert_eq!(base_properties(0, B0, &policy), 0x03ff_0004);
        // NBIF0, NBIF2 and SST absent on IOHC 1, SST forced back by PCIe.
        assert_eq!(base_properties(1, B0, &policy), 0x03eb_0004);
        // SP7 B0's third IOHC is 3; there is no PCIe on 2.
        assert_eq!(base_properties(3, B0, &policy), 0x03eb_0004);
        assert_eq!(base_properties(2, B0, &policy), 0x002b_0004);
        assert_eq!(iommu_properties(0, B0, &policy), 0x0200_00e0);
        let xapic = Policy {
            apic_mode: 1,
            iommu: false,
            snp: true,
            iommu_20: false,
            ..policy
        };
        assert_eq!(iommu_properties(3, B0, &xapic), 0x0200_0141);
        assert_eq!(ras_properties(0, B0, &policy), 0x03ff_0001);
        assert!(B0_51.base_b0_sp8() && SP8.base_b0_sp8() && !B0.base_b0_sp8());
        assert!(SP8.sp8() && !Part::from_cpuid(0x00ba_0fc0, 8 << 28).sp8());
        assert_eq!(Policy::default().nbif.straps, 0x2_eeff);
    }

    #[test]
    fn base_init_runs_base_nbif_and_per_rb_tables() {
        let model = Model::default();
        let rb0 = unsafe { RootBridge::new(model.clone(), 0, 0x20) }.unwrap();
        let rb1 = unsafe { RootBridge::new(model.clone(), 1, 0x20) }.unwrap();
        let rb2 = unsafe { RootBridge::new(model.clone(), 2, 0x20) }.unwrap();
        let policy = Policy::default();
        set(&model, 0x1013_101c, 0x0000_1234);
        set(&model, 0x1013_4200, 1 << 28);
        set(&model, 0x13b0_0068, 0x1b00);
        rb0.base_init(B0_51, &policy);
        assert_eq!(smn(&model, 0x13b1_4044), 0x1030_102f);
        assert_eq!(smn(&model, 0x13b1_0008), 1 << 22);
        assert_eq!(smn(&model, br_interrupt_routing(6).address()), 0x25);
        assert_eq!(smn(&model, 0x1740_0004), 0xf000);
        // NBIF behind 8.1 and 8.2 of the IOHC's bus.
        assert_eq!(smn(&model, 0x1013_101c), 0x2820_12b5);
        assert_eq!(smn(&model, 0x1013_121c), 0x4820_0082);
        assert_eq!(smn(&model, 0x1013_a008), 0x32);
        // TPH completer straps (property 4).
        assert_eq!(smn(&model, 0x1013_1018), 0x1_0000);
        // Straps from the Dell defaults; +0x3B (bit 28) clear.
        assert_eq!(smn(&model, 0x1013_1008), 0x84);
        assert_eq!(smn(&model, 0x1013_1014), 0xaf86_0000);
        assert_eq!(smn(&model, 0x1014_16e2), 0x1d);
        // DACC harvest: both fuse pairs set; 0x1013_4200[28] cleared last.
        assert_eq!(smn(&model, 0x0141_f048), 0xc00);
        assert_eq!(smn(&model, 0x0140_e00c), 0x800);
        assert_eq!(smn(&model, 0x1013_4200) & 1 << 28, 0);
        assert_eq!(smn(&model, 0x13b1_508c), 0);
        assert_eq!(smn(&model, 0x1013_4234), 0x8000_0000, "NBIF_OPT_2A");

        rb1.base_init(B0, &policy);
        assert_eq!(smn(&model, 0x13b1_4044 + S), 0x1030_102f);
        assert_eq!(smn(&model, 0x13b1_0008 + S), 0);
        // IOHC 1 has the SST records too (bit 22 forced by its PCIe).
        assert_eq!(smn(&model, 0x1740_0004 + S), 0xf000);
        set(&model, 0x15c0_0498, 0x55);
        rb1.base_init(B0, &policy);
        assert_eq!(smn(&model, 0x15c0_0498), 0);
        set(&model, 0x15c0_0498, 0x55);
        rb2.base_init(B0, &policy);
        assert_eq!(smn(&model, 0x15c0_0498), 0x55);
        assert_eq!(smn(&model, br_interrupt_routing(0).address() + 2 * S), 0);
        let rb3 = unsafe { RootBridge::new(model.clone(), 3, 0x20) }.unwrap();
        rb3.base_init(B0, &policy);
        assert_eq!(smn(&model, 0x15e0_0478), 0x0004_0004);
        assert_eq!(smn(&model, 0x13e1_4c90), 4);
        assert_eq!(smn(&model, br_interrupt_routing(6).address() + 3 * S), 0x25);
    }

    #[test]
    fn iommu_init_gates_l1_pcie1_b0_tables_caps_and_cp_control() {
        let model = Model::default();
        let rb0 = unsafe { RootBridge::new(model.clone(), 0, 0x20) }.unwrap();
        let rb1 = unsafe { RootBridge::new(model.clone(), 1, 0x20) }.unwrap();
        let rb2 = unsafe { RootBridge::new(model.clone(), 2, 0x20) }.unwrap();
        model
            .0
            .borrow_mut()
            .config
            .insert((0x20, 0, 2, 0x80), 0x1000);
        let policy = Policy::default();
        let mut fuses = Fuses::default();
        rb0.iommu_init(B0, &policy, &mut fuses);
        assert_eq!(smn(&model, 0x14c0_02d8), 0x1_ffff);
        assert_eq!(smn(&model, 0x14c0_0038), 0x1800_0000);
        assert_eq!(smn(&model, 0x1570_00c4), 0x3000_0000);
        assert_eq!(smn(&model, 0x1470_00dc), 0x20);
        // APIC mode 2: property 0x80 record.
        assert_eq!(smn(&model, 0x0240_001c), 0x1404_0000);
        assert_eq!(smn(&model, 0x13f0_00e8), 0x3004);
        assert_eq!(smn(&model, 0x13f0_007c), 0x0800_0010);
        assert_eq!(smn(&model, 0x13f0_0080), 0x0022_0000);
        assert_eq!(smn(&model, 0x1570_4330), 0x0020_0000);
        assert_eq!(smn(&model, 0x13f0_115c), 1);
        assert_eq!(model.0.borrow().config[&(0x20, 0, 2, 0x80)], 0x1084);

        // A fused-off PCIE1 L1 seen on IOHC 0 stays set for IOHC 1.
        set(&model, 0x13b0_0068, 1 << 5);
        let mut fuses = Fuses::default();
        rb0.iommu_init(B0, &policy, &mut fuses);
        set(&model, 0x13b0_0068, 0);
        rb1.iommu_init(B0, &policy, &mut fuses);
        assert!(fuses.no_l1_pcie1);
        assert_eq!(smn(&model, 0x13f0_115c + S), 1);
        assert_eq!(smn(&model, 0x1470_04e4 + S), 0);
        // IOHC 2 has no IOMMU tables.
        rb2.iommu_init(B0, &policy, &mut fuses);
        assert_eq!(smn(&model, 0x13f0_1200 + 2 * S), 0);

        // xAPIC, IOMMU off: no XT, no 0x80 record, D0F2 hidden.
        let model = Model::default();
        set(&model, 0x13b1_0034, 1);
        let rb0 = unsafe { RootBridge::new(model.clone(), 0, 0x20) }.unwrap();
        let off = Policy {
            apic_mode: 1,
            iommu: false,
            ..policy
        };
        rb0.iommu_init(B0, &off, &mut Fuses::default());
        assert_eq!(smn(&model, 0x1470_00dc), 0x20);
        assert_eq!(smn(&model, 0x0240_001c), 0x1400_0000);
        assert_eq!(smn(&model, 0x13b1_0034), 0);
        assert!(!model.0.borrow().config.contains_key(&(0x20, 0, 2, 0x80)));
    }

    fn proxied() -> SmnSpace {
        let mut space = SmnSpace::default();
        space.on_write = Some(proxy_firmware);
        space.set(CONTROL, 1 << 31);
        space
    }

    #[test]
    fn perf_entries_by_latency_kind_and_iohc_and_ari() {
        let space = proxied();
        let timer = ManualTimer::new();
        let proxy = unsafe { Proxy::new(space.smn(), Poll::default(), &timer) };
        let rb0 = unsafe { RootBridge::new(&space, 0, 0) }.unwrap();
        let rb1 = unsafe { RootBridge::new(&space, 1, 0) }.unwrap();
        let policy = Policy::default();
        let table = perf_table(B0);
        rb0.perf_init(B0, &policy, table, (0, 0), &proxy).unwrap();
        // Performance mode: only latency-0 entries (EarlyWakeEn).
        assert_eq!(space.get(0x1470_0184u32), 0);
        assert_eq!(space.get(0x13b1_0360u32), 0x0001_0004);
        assert_eq!(space.get(0x13b1_0364u32), 0x0002_0008);
        assert_eq!(space.get(0x15b0_00b0u32), 0x0001_00f8);
        assert_eq!(space.get(0x1013_a348u32), 0x5);
        rb1.perf_init(B0, &policy, table, (0, 0), &proxy).unwrap();
        assert_eq!(space.get(0x15c0_00b0u32), 0x0001_00d8);
        assert_eq!(space.get(0x1023_a348u32), 0);
        // Latency mode: SdpConnect, the IOMMU and PCIe-core (proxied) entries.
        let latency = Policy {
            perf_latency: true,
            ari: true,
            ..policy
        };
        space.set(0x1a38_0024u32 + (1 << 20), 0x1ff);
        rb0.perf_init(B0, &latency, table, (0, 0), &proxy).unwrap();
        assert_eq!(space.get(0x1470_0184u32), 0x00ff_ffff);
        assert_eq!(space.get(0x1a38_0024u32 + (1 << 20)), 0xff);
        assert_eq!(space.get(0x14b0_0184u32), 0, "kind 1 is IOHC 1's");
        rb1.perf_init(B0, &latency, table, (0, 0), &proxy).unwrap();
        assert_eq!(space.get(0x14c0_0184u32), 0x00ff_ffff);
        let no_iommu = Policy {
            iommu_instances: !0b10,
            ..latency
        };
        space.set(0x1480_0184u32, 0);
        rb1.perf_init(B0, &no_iommu, table, (0, 0), &proxy).unwrap();
        assert_eq!(space.get(0x1480_0184u32), 0);
        rb1.enable_ari(&latency);
        assert_eq!(space.get(0x13b1_0118u32 + S), 1 << 22);
        assert_eq!(proxy.error(), None);
    }

    #[test]
    fn pcie_requester_and_lock_go_through_mpio() {
        use crate::nbio::wh::pcie;
        let space = proxied();
        let timer = ManualTimer::new();
        let proxy = unsafe { Proxy::new(space.smn(), Poll::default(), &timer) };
        space.set(0x1a34_0014u32 + (2 << 20) + (3 << 12), 0xabcd_0000);
        let port = pcie::PortId {
            core: 2,
            rc: 3,
            port: 3,
        };
        pcie::set_requester(&proxy, port, (0, 0x40, 0x19)).unwrap();
        assert_eq!(
            space.get(0x1a34_0014u32 + (2 << 20) + (3 << 12)),
            0xabcd_4019
        );
        assert_eq!(space.get(0x1a34_0018u32 + (2 << 20) + (3 << 12)), 0);
        // Flit mode in the root port's PCIe capabilities.
        space.set(0x1c40_005au32 + (3 << 20) + (3 << 12), 1 << 15);
        pcie::set_requester(&proxy, port, (0, 0x40, 0x19)).unwrap();
        assert_eq!(space.get(0x1a34_0018u32 + (2 << 20) + (3 << 12)), 0x100);
        let rb0 = unsafe { RootBridge::new(&space, 0, 0) }.unwrap();
        rb0.lock_pcie(B0, &proxy).unwrap();
        assert_eq!(space.get(0x1c48_0060u32 + (2 << 20)), 1);
        assert_eq!(space.get(0x1c48_0060u32 + (5 << 20)), 1);
        assert_eq!(space.get(0x1c48_0060u32 + (1 << 20)), 0);
    }

    #[test]
    fn after_enumeration_ras_requester_and_nbif_lock() {
        let model = Model::default();
        let rb0 = unsafe { RootBridge::new(model.clone(), 0, 0x20) }.unwrap();
        let rb1 = unsafe { RootBridge::new(model.clone(), 1, 0x20) }.unwrap();
        let rb2 = unsafe { RootBridge::new(model.clone(), 2, 0x20) }.unwrap();
        {
            let mut m = model.0.borrow_mut();
            m.config.insert((0x20, 8, 1, 0x18), 0x0021_2120);
            m.config.insert((0x21, 0, 0, 0), 0x14dc_1022);
        }
        set(&model, 0x13b2_0dc4, 0x1100);
        rb0.ras_init(B0, &Policy::default(), true);
        assert_eq!(smn(&model, 0x13b2_0008), 0xaaaa_aaaa);
        assert_eq!(smn(&model, 0x1530_0034), 0x400);
        assert_eq!(smn(&model, 0x13b2_0dc4), 0);
        assert_eq!(smn(&model, 0x13b2_0100), 0x9);
        assert_eq!(smn(&model, 0x13b2_0828), 0, "no sync flood by default");
        rb0.store_nbif_requester();
        assert_eq!(smn(&model, 0x1012_37fc), 0x21);
        for rb in [&rb0, &rb1, &rb2] {
            rb.lock_nbif();
        }
        assert_eq!(smn(&model, 0x1013_11ac), 1);
        assert_eq!(smn(&model, 0x1013_13ac + 2 * S), 1);
        assert_eq!(smn(&model, 0x1013_11ac + S), 0);
    }
}
