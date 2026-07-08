//! Pineview/ICH7 platform defaults and fixed handwritten flow.

#[cfg(feature = "stage")]
pub use stage::{
    run_pineview_ich7_mainstage, PineviewIch7, PineviewIch7Board, PineviewIch7Mainstage,
};

#[cfg(feature = "host")]
use fstart_core::board::{IntelMicrocodeConfig, MicrocodeConfig};
use fstart_core::{
    hstr, hvec, CarConfig, Compression, FirmwareImageConfig, FlashLayout, MemoryMap, MemoryRegion,
    MpBuildConfig, RegionKind, RunsFrom, StageBuildConfig, StageConfig, StageLayout, TempRamBuffer,
};
use fstart_driver_intel::gpio_ich as gpio;
use fstart_driver_intel::ich7;
pub use fstart_driver_intel::ich7::{
    HdaConfig, HdaVerbTable, IntelIch7Config, LpcDecodeConfig, LpcFixedIoDecode, LpcFloppyDecode,
    LpcGenericIoDecode, LpcParallelDecode, LpcSerialDecode, PinColor, PinConfig, PinConn,
    PinConnector, PinDevice, PinGeoLoc, PinLoc, SataConfig, SataMode, UsbConfig,
};
use fstart_driver_intel::pineview;
pub use fstart_driver_intel::pineview::{IntelPineviewConfig, PineviewIgdConfig};
use serde::Serialize;

pub const PINEVIEW_NORTHBRIDGE_NODE: &str = "northbridge";
pub const PINEVIEW_BOOTBLOCK_LOAD_ADDR: u64 = 0xffff_ffff;
pub const PINEVIEW_RAMSTAGE_LOAD_ADDR: u64 = 0x0400_0000;
pub const PINEVIEW_RAMSTAGE_HEAP_SIZE: usize = 0x200000;
pub const PINEVIEW_NEXT_STAGE_NAME: &str = "ramstage";
pub const PINEVIEW_MCHBAR: u64 = 0xFED1_0000;
pub const PINEVIEW_DMIBAR: u64 = 0xFED1_8000;
pub const PINEVIEW_EPBAR: u64 = 0xFED1_9000;
pub const PINEVIEW_ECAM_BASE: u64 = 0xE000_0000;
pub const PINEVIEW_ECAM_BUSES: u16 = 64;
pub const ICH7_RCBA: u64 = 0xFED1_C000;
pub const ICH7_PMBASE: u32 = 0x0500;
pub const ICH7_SMBUS_BASE: u16 = 0x0400;

/// Closed Pineview/ICH7 chipset policy consumed by fixed stage code.
///
/// Board-attached devices stay in board hooks/code; fixed chipset windows
/// (MCHBAR/DMIBAR/EPBAR/RCBA/SMBus base) are platform constants.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PineviewIch7Config {
    pub igd: pineview::PineviewIgdConfig,
    pub pcie_ports: [bool; 4],
    pub pirq_routing: [u8; 8],
    pub lpc_decode: LpcDecodeConfig,
    pub gpe0_en: u32,
    pub sata: Option<SataConfig>,
    pub usb: Option<UsbConfig>,
    pub hda: Option<HdaConfig>,
    pub gpio: gpio::GpioConfig,
    pub ck505_pre_raminit: bool,
    /// Maximum logical CPU count (BSP + APs) the board populates.
    pub max_cpus: u16,
}

impl PineviewIch7Config {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            igd: pineview::PineviewIgdConfig::new(),
            pcie_ports: [false; 4],
            pirq_routing: [0; 8],
            lpc_decode: LpcDecodeConfig::new(),
            gpe0_en: 0,
            sata: None,
            usb: None,
            hda: None,
            gpio: gpio::GpioConfig::new(),
            ck505_pre_raminit: false,
            max_cpus: 1,
        }
    }

    #[must_use]
    pub const fn northbridge_config(self) -> pineview::IntelPineviewConfig {
        let mut config = pineview::IntelPineviewConfig::new();
        config.mchbar = PINEVIEW_MCHBAR;
        config.dmibar = PINEVIEW_DMIBAR;
        config.epbar = PINEVIEW_EPBAR;
        config.ecam_base = PINEVIEW_ECAM_BASE;
        config.igd = self.igd;
        config.ck505_pre_raminit = self.ck505_pre_raminit;
        config
    }

    #[must_use]
    pub const fn southbridge_config(self) -> ich7::IntelIch7Config {
        let mut config = ich7::IntelIch7Config::new();
        config.rcba = ICH7_RCBA;
        config.pirq_routing = self.pirq_routing;
        config.gpe0_en = self.gpe0_en;
        config.lpc_decode = self.lpc_decode;
        config.hda = self.hda;
        config.sata = self.sata;
        config.usb = self.usb;
        config.smbus_base = ICH7_SMBUS_BASE;
        config.gpio = self.gpio;
        config
    }

    #[must_use]
    pub const fn max_cpus(mut self, max_cpus: u16) -> Self {
        self.max_cpus = max_cpus;
        self
    }
    #[must_use]
    pub const fn igd(mut self, igd: pineview::PineviewIgdConfig) -> Self {
        self.igd = igd;
        self
    }
    #[must_use]
    pub const fn pcie_port(mut self, port_index: usize, enabled: bool) -> Self {
        if port_index >= self.pcie_ports.len() {
            panic!("ICH7 PCIe port index out of range");
        }
        self.pcie_ports[port_index] = enabled;
        self
    }
    #[must_use]
    pub const fn pirq_routing(mut self, routing: [u8; 8]) -> Self {
        self.pirq_routing = routing;
        self
    }
    #[must_use]
    pub const fn lpc_fixed_io(mut self, fixed_io: LpcFixedIoDecode) -> Self {
        self.lpc_decode.fixed_io = fixed_io;
        self
    }
    #[must_use]
    pub const fn lpc_generic_io<const N: usize>(mut self, ranges: [LpcGenericIoDecode; N]) -> Self {
        let mut idx = 0;
        while idx < N {
            self.lpc_decode.generic_io = self.lpc_decode.generic_io.push(ranges[idx]);
            idx += 1;
        }
        self
    }
    #[must_use]
    pub const fn gpe0_en(mut self, value: u32) -> Self {
        self.gpe0_en = value;
        self
    }
    #[must_use]
    pub const fn sata(mut self, sata: SataConfig) -> Self {
        self.sata = Some(sata);
        self
    }
    #[must_use]
    pub const fn usb(mut self, usb: UsbConfig) -> Self {
        self.usb = Some(usb);
        self
    }
    #[must_use]
    pub const fn hda(mut self, hda: HdaConfig) -> Self {
        self.hda = Some(hda);
        self
    }
    #[must_use]
    pub const fn gpio_pins<const N: usize>(mut self, pins: [gpio::GpioPin; N]) -> Self {
        let mut idx = 0;
        while idx < N {
            self.gpio.pins = self.gpio.pins.push(pins[idx]);
            idx += 1;
        }
        self
    }
    #[must_use]
    pub const fn ck505_pre_raminit(mut self, enabled: bool) -> Self {
        self.ck505_pre_raminit = enabled;
        self
    }
    #[must_use]
    pub const fn build(self) -> Self {
        if self.lpc_decode.fixed_io.com_a as u8 == self.lpc_decode.fixed_io.com_b as u8 {
            panic!("ICH7 COMA and COMB decode the same port");
        }
        if let Some(sata) = self.sata {
            if !ich7::valid_sata_ports(sata.ports) {
                panic!("ICH7 SATA enabled with invalid ports");
            }
        }
        validate_lpc_generic_io_decodes(&self.lpc_decode.generic_io);
        validate_pirq_routing(&self.pirq_routing);
        if !ich7::valid_gpe0_en(self.gpe0_en) {
            panic!("ICH7 GPE0 enable has reserved bits set");
        }
        if self.max_cpus == 0 {
            panic!("Pineview max_cpus must be non-zero");
        }
        self
    }
}

const fn validate_lpc_generic_io_decodes(decodes: &fstart_core::ConstVec<LpcGenericIoDecode, 4>) {
    if konst::iter::eval!(
        0..decodes.len(),
        any(|idx| { !ich7::valid_lpc_generic_io(decodes.get(idx)) })
    ) {
        panic!("ICH7 LPC generic I/O decode is invalid");
    }

    if konst::iter::eval!(
        0..decodes.len(),
        any(|idx| { lpc_generic_io_overlaps_later(decodes, idx) })
    ) {
        panic!("ICH7 LPC generic I/O decodes overlap");
    }
}

const fn lpc_generic_io_overlaps_later(
    decodes: &fstart_core::ConstVec<LpcGenericIoDecode, 4>,
    idx: usize,
) -> bool {
    let decode = decodes.get(idx);
    konst::iter::eval!(
        idx + 1..decodes.len(),
        any(|next_idx| { ich7::lpc_generic_io_overlaps(decode, decodes.get(next_idx)) })
    )
}

const fn validate_pirq_routing(routing: &[u8; 8]) {
    if konst::iter::eval!(
        0..routing.len(),
        any(|idx| { !ich7::valid_pirq_route(routing[idx]) })
    ) {
        panic!("ICH7 PIRQ route is invalid");
    }
}

impl Default for PineviewIch7Config {
    fn default() -> Self {
        Self::new()
    }
}
/// Platform-owned ACPI namespace context for Pineview/ICH7 flows.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PineviewIch7AcpiContext;

impl PineviewIch7AcpiContext {
    #[must_use]
    pub const fn sb_scope(self) -> &'static str {
        "\\_SB_"
    }

    #[must_use]
    pub const fn lpc_scope(self) -> &'static str {
        "\\_SB_.PCI0.LPCB"
    }

    #[must_use]
    pub const fn gpe_scope(self) -> &'static str {
        "\\_GPE"
    }
}

#[cfg(feature = "host")]
pub fn pineview_ich7_microcode() -> MicrocodeConfig {
    MicrocodeConfig::Intel(IntelMicrocodeConfig {
        files: hvec([
            hstr("../../intel-microcode/intel-ucode/06-1c-02"),
            hstr("../../intel-microcode/intel-ucode/06-1c-0a"),
            hstr("../../intel-microcode/intel-ucode/06-1c-02"),
            hstr("../../intel-microcode/intel-ucode/06-1c-0a"),
            hstr("../../intel-microcode/intel-ucode/06-1c-02"),
            hstr("../../intel-microcode/intel-ucode/06-1c-0a"),
            hstr("../../intel-microcode/intel-ucode/06-1c-02"),
        ]),
        early: true,
        mp: true,
    })
}

pub fn pineview_ich7_memory(flash_layout: Option<FlashLayout>) -> MemoryMap {
    MemoryMap {
        regions: hvec([MemoryRegion {
            name: hstr("workram"),
            base: 0x0010_0000,
            size: 0x3FF0_0000,
            kind: RegionKind::Ram,
        }]),
        flash_layout,
        car: Some(CarConfig {
            // Hardware constraint: 32 KiB CAR window. The bootblock's
            // writable footprint (.data/.bss + stack + heap) must fit; do
            // NOT grow this to make an oversized bootblock fit — shrink the
            // bootblock instead (no mainstage-sized statics in drivers).
            base: 0xFEFC_0000,
            size: 0x8000,
        }),
    }
}

pub fn pineview_ich7_stages(config: &PineviewIch7Config) -> StageLayout {
    StageLayout::MultiStage(hvec([
        StageConfig {
            name: hstr("bootblock"),
            build: StageBuildConfig {
                firmware_image: Some(FirmwareImageConfig {
                    temp_ram_buffer: None,
                }),
                load_next_stage: Some(hstr(PINEVIEW_NEXT_STAGE_NAME)),
                ..StageBuildConfig::default()
            },
            load_addr: PINEVIEW_BOOTBLOCK_LOAD_ADDR,
            stack_size: 0x2000,
            // Small CAR heap for FFS/LZ4 scratch allocations.
            heap_size: Some(0x100),
            runs_from: RunsFrom::Rom,
            compression: Compression::None,
            data_addr: None,
            page_table_addr: None,
            page_size: Default::default(),
        },
        StageConfig {
            name: hstr(PINEVIEW_NEXT_STAGE_NAME),
            build: StageBuildConfig {
                firmware_image: Some(FirmwareImageConfig {
                    temp_ram_buffer: Some(TempRamBuffer {
                        base: 0x0200_0000,
                        size: 0x0100_0000,
                    }),
                }),
                verify_firmware: true,
                payload: true,
                pci: true,
                acpi: true,
                smbios: true,
                mp: Some(MpBuildConfig {
                    max_cpus: config.max_cpus,
                    smm: false,
                }),
                ..StageBuildConfig::default()
            },
            load_addr: PINEVIEW_RAMSTAGE_LOAD_ADDR,
            stack_size: 0x400000,
            heap_size: Some(PINEVIEW_RAMSTAGE_HEAP_SIZE as u32),
            runs_from: RunsFrom::Ram,
            compression: Compression::Lz4,
            data_addr: None,
            page_table_addr: None,
            page_size: Default::default(),
        },
    ]))
}

#[cfg(feature = "stage")]
mod stage {
    use super::*;
    use crate::{
        BootblockSpec, IntelEarlyBoard, IntelEarlyPlatform, IntelNorthbridgeDriver, IntelPlatform,
        IntelSouthbridgeDriver, MainstageSpec,
    };
    use fstart_core::services::ServiceError;
    use fstart_driver_intel::ich7::IntelIch7;
    use fstart_driver_intel::pineview::IntelPineview;
    use fstart_driver_uart::ns16550::Ns16550Config;
    use fstart_stage::payload::MainstagePayload;
    use fstart_stage::StageEnvironment;

    /// PINEVIEW northbridge + ICH7 southbridge Intel early-flow platform.
    pub struct PineviewIch7;

    impl IntelPlatform for PineviewIch7 {}

    impl IntelEarlyPlatform for PineviewIch7 {
        type Southbridge = IntelIch7;
        type State = ();
        #[cfg(feature = "acpi")]
        type AcpiContext = PineviewIch7AcpiContext;
    }

    impl PineviewIch7 {
        pub fn run_early<B>(hooks: &mut B::Hooks) -> Result<(), ServiceError>
        where
            B: PineviewIch7Board,
        {
            run_pineview_ich7_bootblock::<B>(hooks)
        }

        pub fn run_stage<B>(env: StageEnvironment, _handoff: usize) -> !
        where
            B: PineviewIch7Board,
        {
            let _ = env;

            #[cfg(fstart_stage_env = "car")]
            {
                let Ok(mut hooks) = B::hooks() else {
                    fstart_platform_x86_64::halt();
                };
                if Self::run_early::<B>(&mut hooks).is_err() {
                    fstart_platform_x86_64::halt();
                }
                fstart_platform_x86_64::halt()
            }

            #[cfg(fstart_stage_env = "ram")]
            {
                run_pineview_ich7_mainstage::<B>()
            }

            #[cfg(not(any(fstart_stage_env = "car", fstart_stage_env = "ram")))]
            {
                match env {
                    StageEnvironment::Car => {
                        let Ok(mut hooks) = B::hooks() else {
                            fstart_platform_x86_64::halt();
                        };
                        if Self::run_early::<B>(&mut hooks).is_err() {
                            fstart_platform_x86_64::halt();
                        }
                        fstart_platform_x86_64::halt()
                    }
                    StageEnvironment::Ram => run_pineview_ich7_mainstage::<B>(),
                    StageEnvironment::Monolithic => fstart_platform_x86_64::halt(),
                }
            }
        }
    }

    /// Board facts required by the Pineview/ICH7 flow.
    pub trait PineviewIch7Board: IntelEarlyBoard<Platform = PineviewIch7> {
        type Payload: MainstagePayload<PineviewIch7Mainstage<Self>>;

        /// Board platform policy. Points at a board `static` so the config lives
        /// in `.rodata`, never on the early-stage stack.
        const CONFIG: &'static PineviewIch7Config;

        /// Derived driver configs, const-evaluated into `.rodata`. Boards do not
        /// override these.
        const NB_CONFIG: &'static IntelPineviewConfig = &Self::CONFIG.northbridge_config();
        const SB_CONFIG: &'static IntelIch7Config = &Self::CONFIG.southbridge_config();

        fn ifd_flash_layout() -> fstart_core::IntelIfdFlashLayout;
        fn console_config() -> Ns16550Config;
        fn console_node() -> &'static str;
        #[cfg(feature = "smbios")]
        fn smbios_desc() -> &'static crate::tables::SmbiosDesc<'static>;
    }

    /// Bring up BSP + APs with the platform's CPU driver. The CPU model and
    /// PMBASE are platform knowledge; the board only states `max_cpus` in its
    /// config.
    #[cfg(feature = "mp")]
    fn init_mp(config: &PineviewIch7Config) -> Result<(), ServiceError> {
        let cpu = fstart_arch::cpu_intel::pineview::PineviewCpuDriver::new(ICH7_PMBASE, None);
        let drivers: [&dyn fstart_arch::mp::CpuDriver; 1] = [&cpu];
        fstart_arch::mp::mp_init(&fstart_arch::mp::MpConfig {
            cpu_drivers: &drivers,
            smm: None,
            smm_image: None,
            max_cpus: config.max_cpus,
        })
        .map(|_| ())
        .map_err(|_| ServiceError::HardwareError)
    }

    /// Handwritten fixed Pineview/ICH7 bootblock flow. Ordering is this function.
    fn run_pineview_ich7_bootblock<B>(hooks: &mut B::Hooks) -> Result<(), ServiceError>
    where
        B: PineviewIch7Board,
    {
        let northbridge = IntelPineview::new_from_config(B::NB_CONFIG)?;
        let southbridge = IntelIch7::new_from_config(B::SB_CONFIG)?;
        crate::run_intel_bootblock::<PineviewIch7, _, _, _>(
            BootblockSpec {
                platform: "pineview/ich7",
                next_stage: PINEVIEW_NEXT_STAGE_NAME,
                ramstage_load_addr: PINEVIEW_RAMSTAGE_LOAD_ADDR,
                flash_layout: B::ifd_flash_layout(),
                console_config: B::console_config(),
                console_node: B::console_node(),
            },
            hooks,
            northbridge,
            southbridge,
        )
    }

    /// Pineview/ICH7 mainstage: fixed platform devices bound from typed config and
    /// driven through the shared Intel mainstage phases.
    pub type PineviewIch7Mainstage<B> = crate::IntelMainstage<
        PineviewIch7,
        IntelPineview,
        IntelIch7,
        <B as IntelEarlyBoard>::Hooks,
        PineviewIch7AcpiContext,
    >;

    #[cfg(feature = "mp")]
    fn init_mp_for_board<B: PineviewIch7Board>() -> Result<(), ServiceError> {
        init_mp(B::CONFIG)
    }

    /// Handwritten fixed Pineview/ICH7 mainstage flow. Ordering is this function.
    pub fn run_pineview_ich7_mainstage<B>() -> !
    where
        B: PineviewIch7Board,
    {
        let Ok(hooks) = B::hooks() else {
            fstart_platform_x86_64::halt();
        };
        let Ok(mainstage) = crate::bind_intel_mainstage::<
            PineviewIch7,
            IntelPineview,
            IntelIch7,
            B::Hooks,
            PineviewIch7AcpiContext,
        >(
            MainstageSpec {
                flash_layout: B::ifd_flash_layout(),
                nb_config: B::NB_CONFIG,
                sb_config: B::SB_CONFIG,
                console_config: B::console_config(),
                console_node: B::console_node(),
                platform_node: PINEVIEW_NORTHBRIDGE_NODE,
                #[cfg(feature = "mp")]
                init_mp: init_mp_for_board::<B>,
                #[cfg(feature = "smbios")]
                smbios_desc: B::smbios_desc(),
            },
            hooks,
        ) else {
            fstart_platform_x86_64::halt();
        };
        crate::run_intel_mainstage::<_, B::Payload>(
            "pineview/ich7",
            fstart_platform_x86_64::halt,
            mainstage,
        )
    }
}
