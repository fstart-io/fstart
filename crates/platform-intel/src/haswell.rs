//! Haswell native-DDR3 + Lynx Point platforms.
//!
//! Board code supplies wiring; this composition layer binds Haswell's
//! [`HaswellPch`] callbacks and apertures to Lynx Point while the shared Intel
//! flow retains ownership of cache integrity, CAR teardown, and S3.

pub use fstart_driver_intel::{
    haswell::{
        Haswell, HaswellConfig, HaswellIgdConfig, HaswellPch, Panel, PchResources, Peg, Slots,
        raminit::spd::Slot,
    },
    lynxpoint::{
        EhciController, Gpios, IrqRoute, Location, LynxPoint, LynxPointConfig, SerialIoMask,
        Usb2Port, Usb3Port,
        gpio::{LpGpio, Mode, Pad, PadTable, Pull},
        smm::Config as SmmConfig,
    },
    southbridge::{
        gpio_ich::GpioConfig,
        hda::HdaConfig,
        lpc::LpcGenericIoDecode,
        smi::{APM_CNT, PrepareError},
    },
};

pub struct HaswellLynxPoint<const LP: bool>;
pub type HaswellUlt = HaswellLynxPoint<true>;
pub type HaswellTrad = HaswellLynxPoint<false>;

const fn lynxpoint_pch<const LP: bool>(southbridge: &LynxPointConfig) -> HaswellPch {
    HaswellPch {
        resources: PchResources {
            rcba_base: fstart_driver_intel::lynxpoint::RCBA as u32,
            pm_base: southbridge.pm_base(),
            pm_size: 256,
            gpio_base: southbridge.gpio_base(),
            gpio_size: if LP { 1024 } else { 128 },
            gpio_fixed: LP,
        },
        me_uma: fstart_driver_intel::lynxpoint::me::uma,
        me_replaced: fstart_driver_intel::lynxpoint::me::replaced,
        me_done: fstart_driver_intel::lynxpoint::me::done,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Config<const LP: bool> {
    pub northbridge: HaswellConfig,
    pub southbridge: LynxPointConfig,
    pub max_cpus: u16,
    pub subsystem_ids: &'static [crate::facts::PciSubsystemId],
}

impl<const LP: bool> Config<LP> {
    #[must_use]
    pub const fn new(gpio: Gpios) -> Self {
        assert!(
            LP == gpio.lp(),
            "PCH GPIO layout does not match the CPU package"
        );
        let southbridge = LynxPointConfig::new(gpio);
        let mut northbridge = HaswellConfig::new(LP);
        northbridge.pch = lynxpoint_pch::<LP>(&southbridge);
        Self {
            northbridge,
            southbridge,
            max_cpus: if LP { 4 } else { 8 },
            subsystem_ids: &[],
        }
    }

    #[must_use]
    pub const fn build(mut self) -> Self {
        self.northbridge.lpc_generic_io = self.southbridge.lpc_generic_io;
        self.northbridge.pch = lynxpoint_pch::<LP>(&self.southbridge);
        let has_static_dimm = !matches!(self.northbridge.slots[0][0], Slot::Empty)
            || !matches!(self.northbridge.slots[0][1], Slot::Empty)
            || !matches!(self.northbridge.slots[1][0], Slot::Empty)
            || !matches!(self.northbridge.slots[1][1], Slot::Empty);
        assert!(
            self.northbridge.select_spd.is_some() || has_static_dimm,
            "the board must wire its DIMMs: set northbridge.slots or northbridge.select_spd"
        );
        assert!(
            self.max_cpus > 0
                && self.northbridge.memory.ult == LP
                && self.southbridge.gpio.lp() == LP
        );
        self
    }
}

impl<const LP: bool> crate::IntelPlatformConfig for Config<LP> {
    fn describe_options(&self, each: &mut dyn FnMut(fstart_options::Entry)) {
        use fstart_driver_intel::IntelSouthbridgeDriver;
        fstart_options::describe(
            LynxPoint::KNOBS,
            &LynxPoint::board_options(&self.southbridge),
            each,
        );
    }

    fn max_cpus(&self) -> u16 {
        self.max_cpus
    }

    fn pci_subsystem_ids(&self) -> &'static [crate::facts::PciSubsystemId] {
        self.subsystem_ids
    }

    fn ehci_debug(&self) -> Option<fstart_driver_usbdebug::Config> {
        self.southbridge
            .ehci_debug(fstart_driver_intel::haswell::ECAM as usize, 64 << 20)
    }
}

impl<const LP: bool> fstart_acpi::PcNamespace for HaswellLynxPoint<LP> {
    const LPC: fstart_acpi::AcpiPath = fstart_driver_intel::namespace::lpc::<Haswell, LynxPoint>();
    const IGD: fstart_acpi::AcpiPath = fstart_driver_intel::namespace::igd::<Haswell>();
}

impl<const LP: bool> crate::IntelPlatform for HaswellLynxPoint<LP> {
    type Config = Config<LP>;
    const NAME: &'static str = if LP {
        "haswell/lynxpoint-lp"
    } else {
        "haswell/lynxpoint"
    };
    const CAR_BASE: u64 = 0xff7c_0000;
    const CAR_SIZE: u64 = 0x40000;
    const BOOTBLOCK_STACK: u64 = 0x20000;
    const FIT: bool = true;
    const STAGE_WINDOWS: crate::facts::StageWindows = crate::facts::StageWindows::ABOVE_128M;
    const MICROCODE_SIGNATURES: &'static [&'static str] = if LP {
        &["06-45-01"]
    } else {
        &["06-3c-03", "06-46-01"]
    };
}

#[cfg(feature = "stage")]
impl<const LP: bool> crate::IntelEarlyPlatform for HaswellLynxPoint<LP> {
    const RESUME_DISPLAY_INIT: bool = false;
    const RELOCATE_BEFORE_CPU_INIT: bool = true;
    const IOAPIC_VIRTUAL_WIRE: bool = true;

    fn reset_failed_postcar_resume() -> ! {
        LynxPoint::reset_failed_resume()
    }

    type Northbridge = Haswell;
    type Southbridge = LynxPoint;
    type Cpu = fstart_arch::x86::cpu::intel::haswell::HaswellCpu;

    fn cpu_driver(microcode: Option<&'static [u8]>, n: &Haswell, _: &LynxPoint) -> Self::Cpu {
        use crate::IntelNorthbridgeDriver;
        Self::Cpu {
            microcode,
            tcc_offset: n.config().tcc_offset,
        }
    }

    fn training_buffers() -> Option<crate::memory_cache::TrainingBuffers<'static>> {
        static WORK: crate::memory_cache::TrainingWorkspace<
            { fstart_driver_intel::haswell::raminit::CACHE_BYTES },
        > = crate::memory_cache::TrainingWorkspace::new();
        WORK.take()
    }

    #[cfg(feature = "bundle-bootblock")]
    fn bootblock_car_log() -> Option<&'static mut [u8]> {
        static LOG: crate::store::CarLogWorkspace<0x4000> = crate::store::CarLogWorkspace::new();
        LOG.take()
    }
}

#[cfg(feature = "stage")]
impl<const LP: bool> crate::IntelChipsetConfig for Config<LP> {
    type Northbridge = Haswell;
    type Southbridge = LynxPoint;

    fn northbridge(&'static self) -> &'static HaswellConfig {
        &self.northbridge
    }

    fn southbridge(&'static self) -> &'static LynxPointConfig {
        &self.southbridge
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ult_and_traditional_geometry_bind_lynxpoint_resources() {
        static P: PadTable = [Pad::new(Mode::Native); 95];
        let mut ult = Config::<true>::new(Gpios::Lp(&P));
        ult.northbridge.slots[0][0] = Slot::SmBus(0x50);
        let ult = ult.build();
        assert!(ult.northbridge.memory.ult);
        assert_eq!(ult.southbridge.pm_base(), 0x1000);
        assert_eq!(ult.southbridge.gpio_base(), 0x1400);
        assert_eq!(
            ult.northbridge.pch.resources,
            PchResources {
                rcba_base: fstart_driver_intel::lynxpoint::RCBA as u32,
                pm_base: 0x1000,
                pm_size: 256,
                gpio_base: 0x1400,
                gpio_size: 1024,
                gpio_fixed: true,
            }
        );

        static G: GpioConfig = GpioConfig::new();
        let mut trad = Config::<false>::new(Gpios::Traditional(&G));
        trad.northbridge.slots[0][0] = Slot::SmBus(0x50);
        let trad = trad.build();
        assert!(!trad.northbridge.memory.ult);
        assert_eq!(trad.southbridge.pm_base(), 0x500);
        assert_eq!(trad.southbridge.gpio_base(), 0x480);
        assert_eq!(
            trad.northbridge.pch.resources,
            PchResources {
                rcba_base: fstart_driver_intel::lynxpoint::RCBA as u32,
                pm_base: 0x500,
                pm_size: 256,
                gpio_base: 0x480,
                gpio_size: 128,
                gpio_fixed: false,
            }
        );

        use crate::IntelPlatformConfig;
        let ult_dbg = ult.ehci_debug().unwrap();
        assert_eq!(
            (
                ult_dbg.ecam_base,
                ult_dbg.device,
                ult_dbg.function,
                ult_dbg.bar
            ),
            (0xf000_0000, 0x1d, 0, 0xfef0_0000)
        );
        let trad_dbg = trad.ehci_debug().unwrap();
        assert_eq!(
            (
                trad_dbg.ecam_base,
                trad_dbg.device,
                trad_dbg.function,
                trad_dbg.bar
            ),
            (0xf000_0000, 0x1a, 0, 0xfef0_0000)
        );
    }

    #[test]
    #[should_panic(expected = "the board must wire its DIMMs")]
    fn rejects_unwired_dimms() {
        static P: PadTable = [Pad::new(Mode::Native); 95];
        let _ = Config::<true>::new(Gpios::Lp(&P)).build();
    }
}
