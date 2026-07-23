//! GM965/ICH8 platform defaults and fixed handwritten flow.

#[cfg(feature = "stage")]
pub use stage::{run_gm965_ich8_mainstage, Gm965Ich8, Gm965Ich8Board, Gm965Ich8Mainstage};

#[cfg(feature = "host")]
use fstart_core::board::{IntelMicrocodeConfig, MicrocodeConfig};
use fstart_core::{
    hstr, hvec, CarConfig, Compression, ConstVec, FirmwareImageConfig, FlashLayout, MemoryMap,
    MemoryRegion, MpBuildConfig, RegionKind, RunsFrom, StageBuildConfig, StageConfig, StageLayout,
    TempRamBuffer,
};
use fstart_driver_intel::gm965;
pub use fstart_driver_intel::gm965::{Gm965IgdConfig, IntelGm965Config};
use fstart_driver_intel::ich8;
pub use fstart_driver_intel::ich8::{
    HdaConfig, HdaVerbTable, IdeConfig, IntelIch8Config, IoTrapAccess, IoTrapConfig,
    LpcDecodeConfig, LpcFixedIoDecode, LpcFloppyDecode, LpcGenericIoDecode, LpcParallelDecode,
    LpcSerialDecode, PinColor, PinConfig, PinConn, PinConnector, PinDevice, PinGeoLoc, PinLoc,
    SataConfig, SataMode, UsbConfig,
};
use fstart_driver_intel::southbridge::gpio_ich as gpio;
use serde::Serialize;

pub const GM965_NORTHBRIDGE_NODE: &str = "northbridge";
pub const GM965_BOOTBLOCK_LOAD_ADDR: u64 = 0xffff_ffff;
pub const GM965_RAMSTAGE_LOAD_ADDR: u64 = 0x0400_0000;
pub const GM965_RAMSTAGE_HEAP_SIZE: usize = 0x200000;
pub const GM965_NEXT_STAGE_NAME: &str = "ramstage";
pub const GM965_MCHBAR: u64 = 0xFED1_4000;
pub const GM965_DMIBAR: u64 = 0xFED1_8000;
pub const GM965_EPBAR: u64 = 0xFED1_9000;
pub const GM965_ECAM_BASE: u64 = 0xE000_0000;
pub const GM965_ECAM_BUSES: u16 = 64;
pub const ICH8_RCBA: u64 = 0xFED1_C000;
pub const ICH8_PMBASE: u32 = 0x0500;
pub const ICH8_SMBUS_BASE: u16 = 0x0400;

/// Closed GM965/ICH8 chipset policy consumed by fixed stage code.
///
/// Board-attached devices stay in board hooks/code; fixed chipset windows
/// (MCHBAR/DMIBAR/EPBAR/RCBA/SMBus base) are platform constants.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Gm965Ich8Config {
    pub igd: gm965::Gm965IgdConfig,
    pub enable_peg: bool,
    pub pcie_ports: [bool; 6],
    pub lpc_decode: LpcDecodeConfig,
    pub gpe0_en: u32,
    pub gpi_routing: [u8; 16],
    pub ide: Option<IdeConfig>,
    pub sata: Option<SataConfig>,
    pub usb: Option<UsbConfig>,
    pub hda: Option<HdaConfig>,
    pub io_traps: ConstVec<IoTrapConfig, 4>,
    pub gpio: gpio::GpioConfig,
    /// Maximum logical CPU count (BSP + APs) the board populates.
    pub max_cpus: u16,
}

impl Gm965Ich8Config {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            igd: gm965::Gm965IgdConfig::new(),
            enable_peg: false,
            pcie_ports: [false; 6],
            lpc_decode: LpcDecodeConfig::new(),
            gpe0_en: 0,
            gpi_routing: [0; 16],
            ide: None,
            sata: None,
            usb: None,
            hda: None,
            io_traps: ConstVec::new(empty_io_trap()),
            gpio: gpio::GpioConfig::new(),
            max_cpus: 1,
        }
    }

    #[must_use]
    pub const fn northbridge_config(self) -> gm965::IntelGm965Config {
        let mut config = gm965::IntelGm965Config::new();
        config.mchbar = GM965_MCHBAR;
        config.dmibar = GM965_DMIBAR;
        config.epbar = GM965_EPBAR;
        config.ecam_base = GM965_ECAM_BASE;
        config.ecam_buses = GM965_ECAM_BUSES;
        config.enable_peg = self.enable_peg;
        config.igd = self.igd;
        config.smbus_base = ICH8_SMBUS_BASE;
        config
    }

    #[must_use]
    pub const fn southbridge_config(self) -> ich8::IntelIch8Config {
        let mut config = ich8::IntelIch8Config::new();
        config.rcba = ICH8_RCBA;
        config.dmibar = GM965_DMIBAR;
        config.gpe0_en = self.gpe0_en;
        config.gpi_routing = self.gpi_routing;
        config.lpc_decode = self.lpc_decode;
        config.hda = self.hda;
        config.ide = self.ide;
        config.sata = self.sata;
        config.usb = self.usb;
        config.pcie_ports = self.pcie_ports;
        config.io_traps = self.io_traps;
        config.smbus_base = ICH8_SMBUS_BASE;
        config.gpio = self.gpio;
        config
    }

    #[must_use]
    pub const fn max_cpus(mut self, max_cpus: u16) -> Self {
        self.max_cpus = max_cpus;
        self
    }

    #[must_use]
    pub const fn igd(mut self, igd: gm965::Gm965IgdConfig) -> Self {
        self.igd = igd;
        self
    }

    #[must_use]
    pub const fn pcie_port(mut self, port_index: usize, enabled: bool) -> Self {
        if port_index >= self.pcie_ports.len() {
            panic!("ICH8 PCIe port index out of range");
        }
        self.pcie_ports[port_index] = enabled;
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
    pub const fn gpi_routing(mut self, routing: [u8; 16]) -> Self {
        self.gpi_routing = routing;
        self
    }

    #[must_use]
    pub const fn ide(mut self, ide: IdeConfig) -> Self {
        self.ide = Some(ide);
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
    pub const fn io_traps<const N: usize>(mut self, traps: [IoTrapConfig; N]) -> Self {
        let mut idx = 0;
        while idx < N {
            self.io_traps = self.io_traps.push(traps[idx]);
            idx += 1;
        }
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
    pub const fn hda_verb<const P: usize, const E: usize>(
        mut self,
        vendor_id: u32,
        subsystem_id: u32,
        pins: [PinConfig; P],
        extra_verbs: [u32; E],
    ) -> Self {
        let mut table = HdaVerbTable::new(vendor_id, subsystem_id);

        let mut idx = 0;
        while idx < P {
            table = table.pin(pins[idx]);
            idx += 1;
        }
        idx = 0;
        while idx < E {
            table = table.extra_verb(extra_verbs[idx]);
            idx += 1;
        }

        let mut hda = match self.hda {
            Some(hda) => hda,
            None => HdaConfig::new(),
        };
        hda = hda.verb(table);
        self.hda = Some(hda);
        self
    }

    #[must_use]
    pub const fn build(self) -> Self {
        if self.lpc_decode.fixed_io.com_a as u8 == self.lpc_decode.fixed_io.com_b as u8 {
            panic!("ICH8 COMA and COMB decode the same port");
        }
        if let Some(sata) = self.sata {
            if sata.ports == 0 {
                panic!("ICH8 SATA enabled with no ports");
            }
        }

        validate_lpc_generic_io_decodes(&self.lpc_decode.generic_io);
        validate_io_traps(&self.io_traps);

        self
    }
}

const fn empty_io_trap() -> IoTrapConfig {
    IoTrapConfig {
        index: 0,
        base: 0,
        size: 0,
        access: IoTrapAccess::Any,
    }
}

const fn validate_lpc_generic_io_decodes(decodes: &fstart_core::ConstVec<LpcGenericIoDecode, 4>) {
    if konst::iter::eval!(
        0..decodes.len(),
        any(|idx| { !ich8::valid_lpc_generic_io(decodes.get(idx)) })
    ) {
        panic!("ICH8 LPC generic I/O decode is invalid");
    }

    if konst::iter::eval!(
        0..decodes.len(),
        any(|idx| { lpc_generic_io_overlaps_later(decodes, idx) })
    ) {
        panic!("ICH8 LPC generic I/O decodes overlap");
    }
}

const fn lpc_generic_io_overlaps_later(
    decodes: &fstart_core::ConstVec<LpcGenericIoDecode, 4>,
    idx: usize,
) -> bool {
    let decode = decodes.get(idx);
    konst::iter::eval!(
        idx + 1..decodes.len(),
        any(|next_idx| { ich8::lpc_generic_io_overlaps(decode, decodes.get(next_idx)) })
    )
}

const fn validate_io_traps(traps: &fstart_core::ConstVec<IoTrapConfig, 4>) {
    if konst::iter::eval!(
        0..traps.len(),
        any(|idx| { !ich8::valid_io_trap(traps.get(idx)) })
    ) {
        panic!("ICH8 I/O trap is invalid");
    }
}

impl Default for Gm965Ich8Config {
    fn default() -> Self {
        Self::new()
    }
}
/// Platform-owned ACPI namespace context for GM965/ICH8 flows.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Gm965Ich8AcpiContext;

impl Gm965Ich8AcpiContext {
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
pub fn gm965_ich8_microcode() -> MicrocodeConfig {
    MicrocodeConfig::Intel(IntelMicrocodeConfig {
        files: hvec([
            hstr("../../intel-microcode/intel-ucode/06-0f-02"),
            hstr("../../intel-microcode/intel-ucode/06-0f-06"),
            hstr("../../intel-microcode/intel-ucode/06-0f-07"),
            hstr("../../intel-microcode/intel-ucode/06-0f-0a"),
            hstr("../../intel-microcode/intel-ucode/06-0f-0b"),
            hstr("../../intel-microcode/intel-ucode/06-0f-0d"),
            hstr("../../intel-microcode/intel-ucode/06-16-01"),
        ]),
        early: true,
        mp: true,
    })
}

pub fn gm965_ich8_memory(flash_layout: Option<FlashLayout>) -> MemoryMap {
    MemoryMap {
        regions: hvec([MemoryRegion {
            name: hstr("workram"),
            base: 0x0010_0000,
            size: 0x3FF0_0000,
            kind: RegionKind::Ram,
        }]),
        flash_layout,
        car: Some(CarConfig {
            base: 0xFEF0_0000,
            size: 0x80000,
        }),
    }
}

pub fn gm965_ich8_stages(config: &Gm965Ich8Config) -> StageLayout {
    StageLayout::MultiStage(hvec([
        StageConfig {
            name: hstr("bootblock"),
            build: StageBuildConfig {
                firmware_image: Some(FirmwareImageConfig {
                    temp_ram_buffer: None,
                }),
                load_next_stage: Some(hstr(GM965_NEXT_STAGE_NAME)),
                ..StageBuildConfig::default()
            },
            load_addr: GM965_BOOTBLOCK_LOAD_ADDR,
            stack_size: 0x2000,
            heap_size: None,
            runs_from: RunsFrom::Rom,
            compression: Compression::None,
            data_addr: None,
            page_table_addr: None,
            page_size: Default::default(),
        },
        StageConfig {
            name: hstr(GM965_NEXT_STAGE_NAME),
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
            load_addr: GM965_RAMSTAGE_LOAD_ADDR,
            stack_size: 0x400000,
            heap_size: Some(GM965_RAMSTAGE_HEAP_SIZE as u32),
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
    use fstart_driver_intel::gm965::IntelGm965;
    use fstart_driver_intel::ich8::IntelIch8;
    use fstart_stage::payload::MainstagePayload;
    use fstart_stage::StageEnvironment;

    /// GM965 northbridge + ICH8 southbridge Intel early-flow platform.
    pub struct Gm965Ich8;

    impl IntelPlatform for Gm965Ich8 {}

    impl IntelEarlyPlatform for Gm965Ich8 {
        type Southbridge = IntelIch8;
        type State = ();
        #[cfg(feature = "acpi")]
        type AcpiContext = Gm965Ich8AcpiContext;
    }

    impl Gm965Ich8 {
        pub fn run_early<B>(hooks: &mut B::Hooks) -> Result<(), ServiceError>
        where
            B: Gm965Ich8Board,
        {
            run_gm965_ich8_bootblock::<B>(hooks)
        }

        pub fn run_stage<B>(env: StageEnvironment, _handoff: usize) -> !
        where
            B: Gm965Ich8Board,
        {
            let _ = env;

            #[cfg(fstart_stage_env = "car")]
            {
                let Ok(mut hooks) = B::hooks() else {
                    fstart_arch::x86_64::halt();
                };
                if Self::run_early::<B>(&mut hooks).is_err() {
                    fstart_arch::x86_64::halt();
                }
                fstart_arch::x86_64::halt()
            }

            #[cfg(fstart_stage_env = "ram")]
            {
                run_gm965_ich8_mainstage::<B>()
            }

            #[cfg(not(any(fstart_stage_env = "car", fstart_stage_env = "ram")))]
            {
                match env {
                    StageEnvironment::Car => {
                        let Ok(mut hooks) = B::hooks() else {
                            fstart_arch::x86_64::halt();
                        };
                        if Self::run_early::<B>(&mut hooks).is_err() {
                            fstart_arch::x86_64::halt();
                        }
                        fstart_arch::x86_64::halt()
                    }
                    StageEnvironment::Ram => run_gm965_ich8_mainstage::<B>(),
                    StageEnvironment::Monolithic => fstart_arch::x86_64::halt(),
                }
            }
        }
    }

    /// Board facts required by the GM965/ICH8 flow.
    pub trait Gm965Ich8Board: IntelEarlyBoard<Platform = Gm965Ich8> {
        type Payload: MainstagePayload<Gm965Ich8Mainstage<Self>>;

        /// Board platform policy. Points at a board `static` so the config lives
        /// in `.rodata`, never on the early-stage stack.
        const CONFIG: &'static Gm965Ich8Config;

        /// Derived driver configs, const-evaluated into `.rodata`. Boards do not
        /// override these.
        const NB_CONFIG: &'static IntelGm965Config = &Self::CONFIG.northbridge_config();
        const SB_CONFIG: &'static IntelIch8Config = &Self::CONFIG.southbridge_config();

        fn flash_layout() -> fstart_core::FlashLayout;
        type Console: fstart_core::services::ConsoleDevice;

        fn console_config() -> <Self::Console as fstart_core::services::ConsoleDevice>::Config;
        fn console_node() -> &'static str;

        #[cfg(feature = "smbios")]
        fn smbios_desc() -> &'static crate::tables::SmbiosDesc<'static>;
    }

    /// Bring up BSP + APs with the platform's CPU driver. The CPU model and
    /// PMBASE are platform knowledge; the board only states `max_cpus` in its
    /// config.
    #[cfg(feature = "mp")]
    fn init_mp(config: &Gm965Ich8Config) -> Result<(), ServiceError> {
        let cpu = fstart_arch::cpu_intel::core2_cpu::Core2CpuDriver::new(ICH8_PMBASE, None);
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

    /// Handwritten fixed GM965/ICH8 bootblock flow. Ordering is this function.
    fn run_gm965_ich8_bootblock<B>(hooks: &mut B::Hooks) -> Result<(), ServiceError>
    where
        B: Gm965Ich8Board,
    {
        let northbridge = IntelGm965::new_from_config(B::NB_CONFIG)?;
        let southbridge = IntelIch8::new_from_config(B::SB_CONFIG)?;
        crate::run_intel_bootblock::<Gm965Ich8, _, _, _, B::Console>(
            BootblockSpec {
                platform: "gm965/ich8",
                next_stage: GM965_NEXT_STAGE_NAME,
                ramstage_load_addr: GM965_RAMSTAGE_LOAD_ADDR,
                flash_layout: B::flash_layout(),
                console_config: B::console_config(),
                console_node: B::console_node(),
            },
            hooks,
            northbridge,
            southbridge,
        )
    }

    /// GM965/ICH8 mainstage: fixed platform devices bound from typed config and
    /// driven through the shared Intel mainstage phases.
    pub type Gm965Ich8Mainstage<B> = crate::IntelMainstage<
        Gm965Ich8,
        IntelGm965,
        IntelIch8,
        <B as IntelEarlyBoard>::Hooks,
        <B as Gm965Ich8Board>::Console,
        Gm965Ich8AcpiContext,
    >;

    #[cfg(feature = "mp")]
    fn init_mp_for_board<B: Gm965Ich8Board>() -> Result<(), ServiceError> {
        init_mp(B::CONFIG)
    }

    /// Handwritten fixed GM965/ICH8 mainstage flow. Ordering is this function.
    pub fn run_gm965_ich8_mainstage<B>() -> !
    where
        B: Gm965Ich8Board,
    {
        let Ok(hooks) = B::hooks() else {
            fstart_arch::x86_64::halt();
        };
        let Ok(mainstage) = crate::bind_intel_mainstage::<
            Gm965Ich8,
            IntelGm965,
            IntelIch8,
            B::Hooks,
            B::Console,
            Gm965Ich8AcpiContext,
        >(
            MainstageSpec {
                flash_layout: B::flash_layout(),
                nb_config: B::NB_CONFIG,
                sb_config: B::SB_CONFIG,
                console_config: B::console_config(),
                console_node: B::console_node(),
                platform_node: GM965_NORTHBRIDGE_NODE,
                #[cfg(feature = "mp")]
                init_mp: init_mp_for_board::<B>,
                #[cfg(feature = "smbios")]
                smbios_desc: B::smbios_desc(),
            },
            hooks,
        ) else {
            fstart_arch::x86_64::halt();
        };
        crate::run_intel_mainstage::<_, B::Payload>(
            "gm965/ich8",
            fstart_arch::x86_64::halt,
            mainstage,
        )
    }
}
