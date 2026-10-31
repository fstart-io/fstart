// SPDX-License-Identifier: MIT OR Apache-2.0
//! The semantic part of what a board tells the AGESA boot loader: the
//! settings a board author can legitimately choose, 1:1 with the APCB
//! tokens named in each field's comment (the platform catalog owns the
//! token identities, widths and placement).
//!
//! Every field is an `Option`: `None` means "the generation's AGESA
//! reference value", which lives in the platform token catalog. That
//! matters because the reference values themselves differ between
//! generations (Rome enables bank swizzle, Milan does not), so a
//! board must be able to not have an opinion. [`Policy::default()`]
//! is no opinion anywhere; boards state their deltas as `Some(..)`,
//! like the driver policies (`PowerPolicy`, `EnvPolicy`) state theirs
//! over `..Default::default()`. Settings nobody has decoded yet are
//! not fields here; they stay catalog defaults in the platform until
//! a board actually needs to change one, which is the point at which
//! it earns a field.

/// Which DIMM kinds the board's slots take (`Mem*Capable` tokens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DimmTypes {
    pub udimm: bool,
    pub sodimm: bool,
    pub rdimm: bool,
    pub lrdimm: bool,
    pub quad_rank: bool,
}

impl DimmTypes {
    /// Server slots: registered and load-reduced DDR4.
    pub const RDIMM_LRDIMM: Self = Self {
        udimm: true,
        sodimm: true,
        rdimm: true,
        lrdimm: true,
        quad_rank: true,
    };
}

/// Memory-controller policy. Field comments name the APCB token each
/// becomes; nothing here knows a token id, width or group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryPolicy {
    /// `MemEnableEccFeature`.
    pub ecc: Option<bool>,
    /// `MemEnableParity` (command/address parity).
    pub parity: Option<bool>,
    /// `MemEccSyncFlood`.
    pub ecc_sync_flood: Option<bool>,
    /// `MemEccRedirection`.
    pub ecc_redirection: Option<bool>,
    /// `MemUncorrectedEccRetryDdr4`.
    pub uncorrected_ecc_retry: Option<bool>,
    /// `MemChannelInterleaving`.
    pub channel_interleaving: Option<bool>,
    /// `MemEnableChipSelectInterleaving`.
    pub chip_select_interleaving: Option<bool>,
    /// `MemEnableBankSwizzle`.
    pub bank_swizzle: Option<bool>,
    /// `MemEnableBankGroupSwap`.
    pub bank_group_swap: Option<bool>,
    /// `MemEnableBankGroupSwapAlt`.
    pub bank_group_swap_alt: Option<bool>,
    /// `MemModeUnganged`.
    pub mode_unganged: Option<bool>,
    /// `MemHoleRemapping`.
    pub hole_remapping: Option<bool>,
    /// `MemLimitMemoryToBelow1TiB`.
    pub limit_below_1tib: Option<bool>,
    /// `MemEnablePowerDown`.
    pub power_down: Option<bool>,
    /// `MemDqsTrainingControl`.
    pub dqs_training_control: Option<bool>,
    /// `MemIgnoreSpdChecksum`.
    pub ignore_spd_checksum: Option<bool>,
    /// `MemSpdReadOptimizationDdr4`.
    pub spd_read_optimization: Option<bool>,
    /// `MemClear` (zero DRAM in ABL).
    pub clear: Option<bool>,
    /// `MemRestoreControl` (memory context restore across S3/warm reset).
    pub restore_control: Option<bool>,
    /// `MemPstate` (memory P-states).
    pub pstate: Option<bool>,
    /// `MemOnDieThermalSensor`.
    pub on_die_thermal: Option<bool>,
    /// Transparent SME: `MemTsmeEnable` on Rome, `MemTsmeModeMilan` on
    /// Milan; the catalog maps the one policy to each generation's token.
    pub tsme: Option<bool>,
    /// The DIMM kinds the slots take.
    pub dimm_types: Option<DimmTypes>,
}

impl MemoryPolicy {
    /// The AGESA reference configuration (see the module docs), as a
    /// const so board facts can be statics.
    pub const DEFAULT: Self = Self {
        ecc: None,
        parity: None,
        ecc_sync_flood: None,
        ecc_redirection: None,
        uncorrected_ecc_retry: None,
        channel_interleaving: None,
        chip_select_interleaving: None,
        bank_swizzle: None,
        bank_group_swap: None,
        bank_group_swap_alt: None,
        mode_unganged: None,
        hole_remapping: None,
        limit_below_1tib: None,
        power_down: None,
        dqs_training_control: None,
        ignore_spd_checksum: None,
        spd_read_optimization: None,
        clear: None,
        restore_control: None,
        pstate: None,
        on_die_thermal: None,
        tsme: None,
        dimm_types: None,
    };
}

impl Default for MemoryPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// NUMA nodes per socket (`DfDramNumaPerSocket`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumaPerSocket {
    Auto,
    One,
    Two,
    Four,
}

/// An xGMI link speed ceiling (`Df4LinkMaxXgmiSpeed`); variants are the
/// Rome/Milan PHY's encodable rates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum XgmiSpeed {
    Auto = 0,
    Mbps6400 = 6_400,
    Mbps7467 = 7_467,
    Mbps8533 = 8_533,
    Mbps9600 = 9_600,
    Mbps10667 = 10_667,
    Mbps11000 = 11_000,
    Mbps12000 = 12_000,
    Mbps13000 = 13_000,
    Mbps14000 = 14_000,
    Mbps15000 = 15_000,
    Mbps16000 = 16_000,
    Mbps17000 = 17_000,
    Mbps18000 = 18_000,
    Mbps19000 = 19_000,
    Mbps20000 = 20_000,
    Mbps21000 = 21_000,
    Mbps22000 = 22_000,
    Mbps23000 = 23_000,
    Mbps24000 = 24_000,
    Mbps25000 = 25_000,
    Mbps26000 = 26_000,
    Mbps27000 = 27_000,
    Mbps28000 = 28_000,
    Mbps29000 = 29_000,
    Mbps30000 = 30_000,
    Mbps31000 = 31_000,
    Mbps32000 = 32_000,
}

impl XgmiSpeed {
    /// The rate in Mbit/s, or `None` for Auto.
    pub const fn mbps(self) -> Option<u32> {
        match self {
            Self::Auto => None,
            rate => Some(rate as u32),
        }
    }
}

/// Where the ABL console goes (`FchConsoleOutSerialPort`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AblSerialPort {
    /// The board's Super I/O UART at the legacy I/O base.
    SuperIo,
    /// FCH UART 0, MMIO.
    Uart0Mmio,
    /// FCH UART 1, MMIO.
    Uart1Mmio,
}

/// Full ABL policy of a board (or of one board instance). Subsumes the
/// token-backed settings the donors actually vary; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub mem: MemoryPolicy,
    /// `FchConsoleOutSerialPort`: where ABL's own logging goes. This is
    /// the console wiring, not a verbosity knob.
    pub abl_serial_port: Option<AblSerialPort>,
    /// `FchConsoleOutMode`: ABL logging enabled at all.
    pub abl_console_out: Option<bool>,
    /// `DfDramNumaPerSocket`. This remains a policy input even though
    /// fstart's own DF flow re-derives NUMA from the APOB, because
    /// ABL sizes the memory map with it.
    pub numa_per_socket: Option<NumaPerSocket>,
    /// The BMC's PCIe link as ABL must see it (`BmcSocket`,
    /// `BmcStartLane`, `BmcEndLane`).
    pub bmc_socket: Option<u8>,
    pub bmc_start_lane: Option<u8>,
    pub bmc_end_lane: Option<u8>,
    /// `Df4LinkMaxXgmiSpeed`: the 4-link xGMI speed ceiling. The
    /// S8030's second board instance caps it; boards leave Auto.
    pub xgmi_4link_speed: Option<XgmiSpeed>,
}

impl Policy {
    pub const DEFAULT: Self = Self {
        mem: MemoryPolicy::DEFAULT,
        abl_serial_port: None,
        abl_console_out: None,
        numa_per_socket: None,
        bmc_socket: None,
        bmc_start_lane: None,
        bmc_end_lane: None,
        xgmi_4link_speed: None,
    };
}

impl Default for Policy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The ABL debug console structure (not tokens): which log classes ABL
/// emits and on which port. Board policy only in the sense that a debug
/// build of a board may turn it up; the donors all ship the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AblConsole {
    pub port: u32,
    pub console_logging: bool,
    pub mem_flow: bool,
    pub mem_setreg: bool,
    pub mem_getreg: bool,
    pub mem_status: bool,
    pub mem_pmu: bool,
    pub mem_pmu_sram_read: bool,
    pub mem_pmu_sram_write: bool,
    pub mem_test_verbose: bool,
    pub mem_basic_output: bool,
    pub breakpoint: bool,
    pub break_on_all_dies: bool,
}

impl AblConsole {
    /// The value every SP3 donor ships: logging on port 0x80.
    pub const DEFAULT: Self = Self {
        port: 0x80,
        console_logging: true,
        mem_flow: true,
        mem_setreg: true,
        mem_getreg: false,
        mem_status: true,
        mem_pmu: true,
        mem_pmu_sram_read: false,
        mem_pmu_sram_write: false,
        mem_test_verbose: false,
        mem_basic_output: true,
        breakpoint: false,
        break_on_all_dies: false,
    };
}

impl Default for AblConsole {
    fn default() -> Self {
        Self::DEFAULT
    }
}
