//! The S8030 as the SP3 flow sees it ([`fstart_platform_sp3::board::Board`]):
//! the AST2500 console path, the GPIO program, the DXIO lane map with the
//! detected M.2 modes and the BMC's Gen3 cap, and the silicon policy.

use alloc::vec::Vec;

use embedded_hal::delay::DelayNs;
use fstart_core::services::Timer;
use fstart_driver_uart::ns16550::Ns16550Config;
use fstart_driver_zen::dxio::engine::MAX_ENGINES;
use fstart_driver_zen::dxio::lanes::Engine;
use fstart_driver_zen::fch::FchHw;
use fstart_driver_zen::fch::gpio::Pad;
use fstart_platform_sp3::acpi::Board as AcpiBoard;
use fstart_platform_sp3::board::{self, BoardError, Gpio, Policy};

use crate::dxio::{Detected, M2Mode, Options};
use crate::{Board, GENERATION, bmc, dxio, gpio, policy};

/// The M.2 detect pads in the generation's OEM form (CN1, CN3).
static DETECT: [Pad; 2] = gpio::m2_detect_pads(GENERATION);

/// Microsecond delays for the KCS transport on the flow's timer.
struct TimerDelay<'a>(&'a dyn Timer);

impl DelayNs for TimerDelay<'_> {
    fn delay_ns(&mut self, ns: u32) {
        self.0.delay_us(u64::from(ns.div_ceil(1000)));
    }
}

impl board::Board for Board {
    const NAME: &'static str = "Tyan S8030";
    const CONSOLE: Ns16550Config = bmc::CONSOLE;

    fn policy() -> Policy {
        Policy {
            df: policy::df(),
            nbio: policy::nbio(),
            power: policy::power(),
            ccx: policy::ccx(),
            fch_env: policy::fch_env(),
            dxio: policy::dxio(GENERATION),
        }
    }

    fn early_init<H: FchHw>(fch: &H) -> Result<(), BoardError> {
        bmc::open_lpc_decodes(fch);
        bmc::early_init().map(drop).map_err(|_| BoardError::Device)
    }

    fn detect_pads() -> &'static [Pad] {
        &DETECT
    }

    fn gpio() -> Gpio {
        Gpio {
            early: gpio::rome_oem_pads(GENERATION),
            table: &gpio::CPM_GPIO,
        }
    }

    fn dxio_engines(
        levels: &[bool],
        timer: &dyn Timer,
    ) -> heapless::Vec<Engine<'static>, MAX_ENGINES> {
        let level = |i: usize| levels.get(i).copied().unwrap_or(true);
        // SAFETY: `early_init` opened the KCS window; the flow owns it.
        let mut kcs = unsafe { bmc::kcs(TimerDelay(timer)) };
        let detected = Detected {
            m2: [M2Mode::from_detect(level(0)), M2Mode::from_detect(level(1))],
            m2_gen3: bmc::m2_gen3_cap(&mut kcs),
        };
        dxio::engines(GENERATION, &Options::default(), &detected)
    }

    fn acpi_lpc_aml() -> Vec<u8> {
        bmc::lpc_aml()
    }

    fn acpi_board(lpc_aml: &[u8]) -> AcpiBoard<'_> {
        bmc::acpi_board(lpc_aml)
    }
}
