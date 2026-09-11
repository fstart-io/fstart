//! Pineview/ICH7 platform defaults and fixed handwritten flow.

#[cfg(feature = "stage")]
pub use stage::{
    PineviewIch7, PineviewIch7Board, PineviewIch7Mainstage, run_pineview_ich7_mainstage,
};

/// Platform-owned adapter for fixed Pineview stage dispatch.
#[cfg(feature = "stage")]
pub struct Program<B>(core::marker::PhantomData<B>);
#[cfg(feature = "stage")]
impl<B: PineviewIch7Board> fstart_stage::StageProgram for Program<B> {
    fn run_stage(handoff: usize) -> ! {
        #[cfg(fstart_stage_env = "car")]
        PineviewIch7::run_stage::<B>(fstart_stage::StageEnvironment::Car, handoff);
        #[cfg(any(fstart_stage_env = "postcar", fstart_stage_env = "ram"))]
        PineviewIch7::run_stage::<B>(fstart_stage::StageEnvironment::Ram, handoff);
        #[cfg(not(any(
            fstart_stage_env = "car",
            fstart_stage_env = "postcar",
            fstart_stage_env = "ram"
        )))]
        panic!("pineview requires a fixed Intel stage selection");
    }
}

use fstart_driver_intel::ich7;
pub use fstart_driver_intel::ich7::{
    HdaConfig, HdaVerbTable, IntelIch7Config, LpcDecodeConfig, LpcFixedIoDecode, LpcFloppyDecode,
    LpcGenericIoDecode, LpcParallelDecode, LpcSerialDecode, PinColor, PinConfig, PinConn,
    PinConnector, PinDevice, PinGeoLoc, PinLoc, SataConfig, SataMode, UsbConfig,
};
use fstart_driver_intel::pineview;
pub use fstart_driver_intel::pineview::{IntelPineviewConfig, PineviewIgdConfig};
use fstart_driver_intel::southbridge::gpio_ich as gpio;
use serde::Serialize;

pub const PINEVIEW_NORTHBRIDGE_NODE: &str = "northbridge";
pub const PINEVIEW_POSTCAR_STAGE_NAME: &str = crate::POSTCAR_STAGE_NAME;
pub const PINEVIEW_NEXT_STAGE_NAME: &str = "ramstage";
/// Pineview 32 KiB CAR window.
pub const PINEVIEW_CAR_BASE: u64 = 0xFEFC_0000;
pub const PINEVIEW_CAR_SIZE: u64 = 0x8000;
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
        config.pcie_ports = self.pcie_ports;
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
        if let Some(sata) = self.sata
            && !ich7::valid_sata_ports(sata.ports)
        {
            panic!("ICH7 SATA enabled with invalid ports");
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

#[cfg(feature = "stage")]
mod stage {
    use super::*;
    use crate::{
        FfsLoadSpec, IntelEarlyBoard, IntelEarlyPlatform, IntelNorthbridgeDriver, IntelPlatform,
        IntelSouthbridgeDriver, MainstageSpec,
    };
    use fstart_core::services::ServiceError;
    use fstart_driver_intel::ich7::IntelIch7;
    use fstart_driver_intel::pineview::IntelPineview;
    use fstart_stage::StageEnvironment;
    use fstart_stage::payload::MainstagePayload;

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
                    fstart_arch::x86_64::halt();
                };
                if Self::run_early::<B>(&mut hooks).is_err() {
                    fstart_arch::x86_64::halt();
                }
                fstart_arch::x86_64::halt()
            }

            #[cfg(fstart_stage_env = "postcar")]
            {
                // Cut-B postcar loader: teardown already done by the entry;
                // load and decompress the ramstage cached. Noreturn.
                run_pineview_ich7_postcar::<B>()
            }

            #[cfg(fstart_stage_env = "ram")]
            {
                run_pineview_ich7_mainstage::<B>()
            }

            #[cfg(not(any(
                fstart_stage_env = "car",
                fstart_stage_env = "ram",
                fstart_stage_env = "postcar"
            )))]
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
                    StageEnvironment::Ram => run_pineview_ich7_mainstage::<B>(),
                    StageEnvironment::Monolithic => fstart_arch::x86_64::halt(),
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

        type Console: fstart_core::services::ConsoleDevice;

        fn console_config() -> <Self::Console as fstart_core::services::ConsoleDevice>::Config;
        fn console_node() -> &'static str;
        #[cfg(feature = "smbios")]
        fn smbios_desc() -> &'static crate::tables::SmbiosDesc<'static>;
    }

    /// Bring up BSP + APs with the platform's CPU driver. The Pineview Atom
    /// is family 6 model 0x1c, covered by the 106cx Pineview CPU driver
    /// (signatures 0x106c0/0x106ca); only the PMBASE is platform knowledge.
    /// The board states `max_cpus` in its config.
    ///
    /// When fbuild embedded an SMM image into this stage (`SMM_IMAGE`), MP
    /// setup also performs SMM relocation, installs the handler in TSEG, and
    /// locks SMRAM via [`fstart_arch::mp::SmmOps`].
    #[cfg(feature = "mp")]
    fn init_mp(nb_config: &'static IntelPineviewConfig, max_cpus: u16) -> Result<(), ServiceError> {
        // APs must run the same updated microcode as the BSP, whose update
        // happens in pre-CAR assembly; the blob sits in boot flash.
        let microcode = crate::intel_microcode_blob();
        let cpu = fstart_arch::cpu_intel::pineview::PineviewCpuDriver::new(ICH7_PMBASE, microcode);
        let drivers: [&dyn fstart_arch::mp::CpuDriver; 1] = [&cpu];
        let northbridge = IntelPineview::new_from_config(nb_config)?;
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

    /// Handwritten fixed Pineview/ICH7 bootblock flow. Ordering is this function.
    /// Ends by authenticating and loading postcar and publishing the MTRR stash (shared
    /// `run_intel_bootblock` tail); the bulk ramstage copy stays cached in
    /// postcar.
    fn run_pineview_ich7_bootblock<B>(hooks: &mut B::Hooks) -> Result<(), ServiceError>
    where
        B: PineviewIch7Board,
    {
        let northbridge = IntelPineview::new_from_config(B::NB_CONFIG)?;
        let southbridge = IntelIch7::new_from_config(B::SB_CONFIG)?;
        crate::run_intel_bootblock::<PineviewIch7, _, _, _, B::Console>(
            bootstrap_spec::<B>(0)?,
            hooks,
            northbridge,
            southbridge,
        )
    }

    /// Handwritten fixed Pineview/ICH7 postcar flow: fresh program, fresh
    /// stack, caching on. Re-inits the console from ROM constants, loads and
    /// decompresses the ramstage cached, and jumps to it. Noreturn.
    #[cfg(fstart_stage_env = "postcar")]
    pub fn run_pineview_ich7_postcar<B>() -> !
    where
        B: PineviewIch7Board,
    {
        let Ok(spec) = bootstrap_spec::<B>(1) else {
            fstart_arch::x86_64::halt()
        };
        crate::run_intel_postcar::<B::Console>(spec)
    }

    fn bootstrap_spec<B: PineviewIch7Board>(
        index: u16,
    ) -> Result<FfsLoadSpec<B::Console>, ServiceError> {
        use fstart_core::layout::RegionKind;
        let layout = crate::layout::IntelBootLayout::current(index)?;
        Ok(FfsLoadSpec {
            platform: "pineview/ich7",
            next_stage: PINEVIEW_POSTCAR_STAGE_NAME,
            next_load_addr: layout.region(RegionKind::BootstrapPostcar)?.base,
            geometry: layout,
            dram_end: layout
                .region(RegionKind::BootstrapRam)?
                .end()
                .ok_or(ServiceError::InvalidParam)?,
            ramstage_name: PINEVIEW_NEXT_STAGE_NAME,
            ramstage_load_addr: layout.region(RegionKind::BootstrapMainstage)?.base,
            console_config: B::console_config(),
            console_node: B::console_node(),
        })
    }

    /// Pineview/ICH7 mainstage: fixed platform devices bound from typed config and
    /// driven through the shared Intel mainstage phases.
    pub type PineviewIch7Mainstage<B> = crate::IntelMainstage<
        PineviewIch7,
        IntelPineview,
        IntelIch7,
        <B as IntelEarlyBoard>::Hooks,
        <B as PineviewIch7Board>::Console,
        PineviewIch7AcpiContext,
    >;

    #[cfg(feature = "mp")]
    fn init_mp_for_board<B: PineviewIch7Board>() -> Result<(), ServiceError> {
        let max_cpus = option_env!("FSTART_INTEL_MAX_CPUS")
            .ok_or(ServiceError::InvalidParam)?
            .parse()
            .map_err(|_| ServiceError::InvalidParam)?;
        init_mp(B::NB_CONFIG, max_cpus)
    }

    /// Handwritten fixed Pineview/ICH7 mainstage flow. Ordering is this function.
    pub fn run_pineview_ich7_mainstage<B>() -> !
    where
        B: PineviewIch7Board,
    {
        let Ok(hooks) = B::hooks() else {
            fstart_arch::x86_64::halt();
        };
        let Ok(layout) = crate::layout::IntelBootLayout::current(2) else {
            fstart_arch::x86_64::halt()
        };
        let Ok(mainstage) = crate::bind_intel_mainstage::<
            PineviewIch7,
            IntelPineview,
            IntelIch7,
            B::Hooks,
            B::Console,
            PineviewIch7AcpiContext,
        >(
            MainstageSpec {
                geometry: layout,
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
            fstart_arch::x86_64::halt();
        };
        crate::run_intel_mainstage::<_, B::Payload>(
            "pineview/ich7",
            PINEVIEW_NEXT_STAGE_NAME,
            fstart_arch::x86_64::halt,
            mainstage,
        )
    }
}
