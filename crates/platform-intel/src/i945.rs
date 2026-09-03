//! i945/ICH7 platform defaults and fixed handwritten flow.

#[cfg(feature = "stage")]
pub use stage::{I945Ich7, I945Ich7Board, I945Ich7Mainstage, run_i945_ich7_mainstage};

#[cfg(feature = "host")]
use fstart_core::board::{IntelMicrocodeConfig, MicrocodeConfig};
use fstart_core::{
    CarConfig, Compression, FirmwareImageConfig, FlashLayout, MemoryMap, MemoryRegion,
    MpBuildConfig, RegionKind, RunsFrom, StageBuildConfig, StageConfig, StageLayout, TempRamBuffer,
    hstr, hvec,
};
use fstart_driver_intel::i945;
pub use fstart_driver_intel::i945::{I945Variant, IntelI945Config};
use fstart_driver_intel::ich7;
pub use fstart_driver_intel::ich7::{
    HdaConfig, HdaVerbTable, IntelIch7Config, LpcDecodeConfig, LpcFixedIoDecode, LpcFloppyDecode,
    LpcGenericIoDecode, LpcParallelDecode, LpcSerialDecode, PinColor, PinConfig, PinConn,
    PinConnector, PinDevice, PinGeoLoc, PinLoc, SataConfig, SataMode, UsbConfig,
};
use fstart_driver_intel::southbridge::gpio_ich as gpio;
use serde::Serialize;

pub const I945_NORTHBRIDGE_NODE: &str = "northbridge";
pub const I945_BOOTBLOCK_LOAD_ADDR: u64 = 0xffff_ffff;
pub const I945_RAMSTAGE_LOAD_ADDR: u64 = 0x0400_0000;
pub const I945_RAMSTAGE_HEAP_SIZE: usize = 0x200000;
pub const I945_NEXT_STAGE_NAME: &str = "ramstage";
pub const I945_MCHBAR: u64 = 0xFED1_4000;
pub const I945_DMIBAR: u64 = 0xFED1_8000;
pub const I945_EPBAR: u64 = 0xFED1_9000;
pub const I945_ECAM_BASE: u64 = 0xF000_0000;
pub const I945_ECAM_BUSES: u16 = 64;
pub const ICH7_RCBA: u64 = 0xFED1_C000;
pub const ICH7_PMBASE: u32 = 0x0500;
pub const ICH7_SMBUS_BASE: u16 = 0x0400;
/// Socket 441 CAR window (matches coreboot `DCACHE_RAM_BASE/SIZE`).
pub const I945_CAR_BASE: u64 = 0xFEFC_0000;
pub const I945_CAR_SIZE: u64 = 0x8000;

/// Closed i945/ICH7 chipset policy consumed by fixed stage code.
///
/// Board-attached devices stay in board hooks/code; fixed chipset windows
/// (MCHBAR/DMIBAR/EPBAR/RCBA/SMBus base) are platform constants.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(deny_unknown_fields)]
pub struct I945Ich7Config {
    pub variant: I945Variant,
    pub gfx_gms: u8,
    pub pci_mmio_size: u32,
    pub pcie_ports: [bool; 4],
    pub pirq_routing: [u8; 8],
    pub gpi_routing: [u8; 16],
    /// Internal LAN function present (`FD_INTLAN` when false).
    pub lan: bool,
    /// AC97 audio/modem functions present (`FD_ACAUD`/`FD_ACMOD` when false).
    pub ac97_audio: bool,
    pub ac97_modem: bool,
    pub lpc_decode: LpcDecodeConfig,
    pub gpe0_en: u32,
    pub sata: Option<SataConfig>,
    pub usb: Option<UsbConfig>,
    pub hda: Option<HdaConfig>,
    pub gpio: gpio::GpioConfig,
    /// Maximum logical CPU count (BSP + APs) the board populates.
    pub max_cpus: u16,
}

impl I945Ich7Config {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            variant: I945Variant::DesktopGc,
            gfx_gms: 4,
            pci_mmio_size: 768,
            pcie_ports: [false; 4],
            pirq_routing: [0; 8],
            gpi_routing: [0; 16],
            lan: true,
            ac97_audio: true,
            ac97_modem: true,
            lpc_decode: LpcDecodeConfig::new(),
            gpe0_en: 0,
            sata: None,
            usb: None,
            hda: None,
            gpio: gpio::GpioConfig::new(),
            max_cpus: 1,
        }
    }

    #[must_use]
    pub const fn northbridge_config(self) -> i945::IntelI945Config {
        let mut config = i945::IntelI945Config::new();
        config.mchbar = I945_MCHBAR;
        config.dmibar = I945_DMIBAR;
        config.epbar = I945_EPBAR;
        config.ecam_base = I945_ECAM_BASE;
        config.ecam_buses = I945_ECAM_BUSES;
        config.rcba = ICH7_RCBA;
        config.variant = self.variant;
        config.gfx_gms = self.gfx_gms;
        config.pci_mmio_size = self.pci_mmio_size;
        config.smbus_base = ICH7_SMBUS_BASE;
        config
    }

    #[must_use]
    pub const fn southbridge_config(self) -> ich7::IntelIch7Config {
        let mut config = ich7::IntelIch7Config::new();
        config.rcba = ICH7_RCBA;
        config.pirq_routing = self.pirq_routing;
        config.gpi_routing = self.gpi_routing;
        config.pcie_ports = self.pcie_ports;
        config.lan = self.lan;
        config.ac97_audio = self.ac97_audio;
        config.ac97_modem = self.ac97_modem;
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
    pub const fn variant(mut self, variant: I945Variant) -> Self {
        self.variant = variant;
        self
    }
    #[must_use]
    pub const fn gfx_gms(mut self, gms: u8) -> Self {
        self.gfx_gms = gms;
        self
    }
    #[must_use]
    pub const fn pci_mmio_size(mut self, size_mb: u32) -> Self {
        self.pci_mmio_size = size_mb;
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
    pub const fn gpi_routing(mut self, routing: [u8; 16]) -> Self {
        self.gpi_routing = routing;
        self
    }
    #[must_use]
    pub const fn lan(mut self, present: bool) -> Self {
        self.lan = present;
        self
    }
    #[must_use]
    pub const fn ac97_audio(mut self, present: bool) -> Self {
        self.ac97_audio = present;
        self
    }
    #[must_use]
    pub const fn ac97_modem(mut self, present: bool) -> Self {
        self.ac97_modem = present;
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
    pub const fn build(self) -> Self {
        if self.lpc_decode.fixed_io.com_a as u8 == self.lpc_decode.fixed_io.com_b as u8 {
            panic!("ICH7 COMA and COMB decode the same port");
        }
        if let Some(sata) = self.sata
            && !ich7::valid_sata_ports(sata.ports) {
                panic!("ICH7 SATA enabled with invalid ports");
            }
        validate_lpc_generic_io_decodes(&self.lpc_decode.generic_io);
        validate_pirq_routing(&self.pirq_routing);
        if !ich7::valid_gpe0_en(self.gpe0_en) {
            panic!("ICH7 GPE0 enable has reserved bits set");
        }
        if self.gfx_gms > 7 {
            panic!("i945 GMS must be 0..=7");
        }
        if self.max_cpus == 0 {
            panic!("i945 max_cpus must be non-zero");
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

impl Default for I945Ich7Config {
    fn default() -> Self {
        Self::new()
    }
}

/// Platform-owned ACPI namespace context for i945/ICH7 flows.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct I945Ich7AcpiContext;

impl I945Ich7AcpiContext {
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

/// Diamondville (Atom 230, family 6 model 0x1c) microcode.
///
/// The 106cx blobs already ship in `intel-microcode/`; the D945GCLF's Atom
/// 230 is stepping 0x106c2 (`06-1c-02`), with `06-1c-0a` covering later
/// steppings — the same pair the Pineview port uses for its 0x1c Atoms.
#[cfg(feature = "host")]
pub fn i945_ich7_microcode() -> MicrocodeConfig {
    MicrocodeConfig::Intel(IntelMicrocodeConfig {
        files: hvec([
            hstr("../../../intel-microcode/intel-ucode/06-1c-02"),
            hstr("../../../intel-microcode/intel-ucode/06-1c-0a"),
        ]),
        early: true,
        mp: true,
    })
}

pub fn i945_ich7_memory(flash_layout: Option<FlashLayout>) -> MemoryMap {
    MemoryMap {
        regions: hvec([MemoryRegion {
            name: hstr("workram"),
            base: 0x0010_0000,
            size: 0x3FF0_0000,
            kind: RegionKind::Ram,
        }]),
        flash_layout,
        car: Some(CarConfig {
            // Socket 441 hardware constraint: 32 KiB CAR window (coreboot
            // DCACHE_RAM_BASE/SIZE). Same budgeting rules as Pineview: keep
            // the bootblock small, never grow this.
            base: I945_CAR_BASE,
            size: I945_CAR_SIZE,
        }),
    }
}

pub fn i945_ich7_stages(config: &I945Ich7Config) -> StageLayout {
    StageLayout::MultiStage(hvec([
        StageConfig {
            name: hstr("bootblock"),
            build: StageBuildConfig {
                firmware_image: Some(FirmwareImageConfig {
                    temp_ram_buffer: None,
                }),
                load_next_stage: Some(hstr(I945_NEXT_STAGE_NAME)),
                ..StageBuildConfig::default()
            },
            load_addr: I945_BOOTBLOCK_LOAD_ADDR,
            stack_size: 0x2000,
            heap_size: None,
            runs_from: RunsFrom::Rom,
            compression: Compression::None,
            data_addr: None,
            page_table_addr: None,
            page_size: Default::default(),
        },
        StageConfig {
            name: hstr(I945_NEXT_STAGE_NAME),
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
                    smm: true,
                }),
                ..StageBuildConfig::default()
            },
            load_addr: I945_RAMSTAGE_LOAD_ADDR,
            stack_size: 0x400000,
            heap_size: Some(I945_RAMSTAGE_HEAP_SIZE as u32),
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
    use fstart_driver_intel::i945::IntelI945;
    use fstart_driver_intel::ich7::IntelIch7;
    use fstart_stage::StageEnvironment;
    use fstart_stage::payload::MainstagePayload;

    /// i945 northbridge + ICH7 southbridge Intel early-flow platform.
    pub struct I945Ich7;

    impl IntelPlatform for I945Ich7 {}

    impl IntelEarlyPlatform for I945Ich7 {
        type Southbridge = IntelIch7;
        type State = ();
        #[cfg(feature = "acpi")]
        type AcpiContext = I945Ich7AcpiContext;
    }

    impl I945Ich7 {
        pub fn run_early<B>(hooks: &mut B::Hooks) -> Result<(), ServiceError>
        where
            B: I945Ich7Board,
        {
            run_i945_ich7_bootblock::<B>(hooks)
        }

        pub fn run_stage<B>(env: StageEnvironment, _handoff: usize) -> !
        where
            B: I945Ich7Board,
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
                run_i945_ich7_mainstage::<B>()
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
                    StageEnvironment::Ram => run_i945_ich7_mainstage::<B>(),
                    StageEnvironment::Monolithic => fstart_arch::x86_64::halt(),
                }
            }
        }
    }

    /// Board facts required by the i945/ICH7 flow.
    pub trait I945Ich7Board: IntelEarlyBoard<Platform = I945Ich7> {
        type Payload: MainstagePayload<I945Ich7Mainstage<Self>>;

        /// Board platform policy. Points at a board `static` so the config lives
        /// in `.rodata`, never on the early-stage stack.
        const CONFIG: &'static I945Ich7Config;

        /// Derived driver configs, const-evaluated into `.rodata`. Boards do not
        /// override these.
        const NB_CONFIG: &'static IntelI945Config = &Self::CONFIG.northbridge_config();
        const SB_CONFIG: &'static IntelIch7Config = &Self::CONFIG.southbridge_config();

        fn flash_layout() -> fstart_core::FlashLayout;
        type Console: fstart_core::services::ConsoleDevice;

        fn console_config() -> <Self::Console as fstart_core::services::ConsoleDevice>::Config;
        fn console_node() -> &'static str;
        #[cfg(feature = "smbios")]
        fn smbios_desc() -> &'static crate::tables::SmbiosDesc<'static>;
    }

    /// Bring up BSP + APs with the platform's CPU driver. The Diamondville
    /// Atom 230 is family 6 model 0x1c, covered by the 106cx Pineview CPU
    /// driver (signatures 0x106c0/0x106ca); only the PMBASE is platform
    /// knowledge. The board states `max_cpus` in its config.
    ///
    /// When fbuild embedded an SMM image into this stage (`SMM_IMAGE`), MP
    /// setup also performs SMM relocation, installs the handler in TSEG, and
    /// locks SMRAM via [`fstart_arch::mp::SmmOps`].
    #[cfg(feature = "mp")]
    fn init_mp(nb_config: &'static IntelI945Config, max_cpus: u16) -> Result<(), ServiceError> {
        // APs must run the same updated microcode as the BSP, whose update
        // happens in pre-CAR assembly; the blob sits in boot flash.
        let microcode = crate::intel_microcode_blob();
        let cpu = fstart_arch::cpu_intel::pineview::PineviewCpuDriver::new(ICH7_PMBASE, microcode);
        let drivers: [&dyn fstart_arch::mp::CpuDriver; 1] = [&cpu];
        let northbridge = IntelI945::new_from_config(nb_config)?;
        let smm = crate::SMM_IMAGE.map(|_| &northbridge as &dyn fstart_arch::mp::SmmOps);
        fstart_arch::mp::mp_init(&fstart_arch::mp::MpConfig {
            cpu_drivers: &drivers,
            smm,
            smm_image: crate::SMM_IMAGE,
            max_cpus,
        })
        .map(|_| ())
        .map_err(|_| ServiceError::HardwareError)
    }

    /// Handwritten fixed i945/ICH7 bootblock flow. Ordering is this function.
    fn run_i945_ich7_bootblock<B>(hooks: &mut B::Hooks) -> Result<(), ServiceError>
    where
        B: I945Ich7Board,
    {
        let northbridge = IntelI945::new_from_config(B::NB_CONFIG)?;
        let southbridge = IntelIch7::new_from_config(B::SB_CONFIG)?;
        crate::run_intel_bootblock::<I945Ich7, _, _, _, B::Console>(
            BootblockSpec {
                platform: "i945/ich7",
                next_stage: I945_NEXT_STAGE_NAME,
                ramstage_load_addr: I945_RAMSTAGE_LOAD_ADDR,
                flash_layout: B::flash_layout(),
                console_config: B::console_config(),
                console_node: B::console_node(),
            },
            hooks,
            northbridge,
            southbridge,
        )
    }

    /// i945/ICH7 mainstage: fixed platform devices bound from typed config and
    /// driven through the shared Intel mainstage phases.
    pub type I945Ich7Mainstage<B> = crate::IntelMainstage<
        I945Ich7,
        IntelI945,
        IntelIch7,
        <B as IntelEarlyBoard>::Hooks,
        <B as I945Ich7Board>::Console,
        I945Ich7AcpiContext,
    >;

    #[cfg(feature = "mp")]
    fn init_mp_for_board<B: I945Ich7Board>() -> Result<(), ServiceError> {
        init_mp(B::NB_CONFIG, B::CONFIG.max_cpus)
    }

    /// Handwritten fixed i945/ICH7 mainstage flow. Ordering is this function.
    pub fn run_i945_ich7_mainstage<B>() -> !
    where
        B: I945Ich7Board,
    {
        let Ok(hooks) = B::hooks() else {
            fstart_arch::x86_64::halt();
        };
        let Ok(mainstage) = crate::bind_intel_mainstage::<
            I945Ich7,
            IntelI945,
            IntelIch7,
            B::Hooks,
            B::Console,
            I945Ich7AcpiContext,
        >(
            MainstageSpec {
                flash_layout: B::flash_layout(),
                nb_config: B::NB_CONFIG,
                sb_config: B::SB_CONFIG,
                console_config: B::console_config(),
                console_node: B::console_node(),
                platform_node: I945_NORTHBRIDGE_NODE,
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
            "i945/ich7",
            fstart_arch::x86_64::halt,
            mainstage,
        )
    }
}
