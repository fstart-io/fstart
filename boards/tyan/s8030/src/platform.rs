//! The S8030 as the SP3 flow sees it ([`fstart_platform_sp3::board::Board`]):
//! the AST2500 console path, the GPIO program, the DXIO lane map with the
//! detected M.2 modes and the BMC's Gen3 cap, and the silicon policy.

use alloc::vec::Vec;

use fstart_core::services::{Timer, TimerDelay};
use fstart_driver_amd::sp3::dxio::engine::MAX_ENGINES;
use fstart_driver_amd::sp3::dxio::lanes::Engine;
use fstart_driver_amd::sp3::fch::FchHw;
use fstart_driver_amd::sp3::fch::gpio::Pad;
use fstart_driver_uart::ns16550::Ns16550Config;
use fstart_platform_sp3::acpi::Board as AcpiBoard;
use fstart_platform_sp3::board::{self, BoardError, Gpio, Policy};
use fstart_platform_sp3::smm::BmcSxNotify;

use crate::dxio::{Detected, M2Mode, Options};
use fstart_driver_amd::sp3::soc::Generation;

use crate::{Board, bmc, dxio, gpio, policy};

/// The M.2 detect pads in each generation's OEM form (CN1, CN3).
static DETECT_ROME: [Pad; 2] = gpio::m2_detect_pads(Generation::Rome);
static DETECT_MILAN: [Pad; 2] = gpio::m2_detect_pads(Generation::Milan);

impl board::Board for Board {
    const NAME: &'static str = "Tyan S8030";
    const CONSOLE: Ns16550Config = bmc::CONSOLE;
    /// The vendor DSDT's `BSMI` = 0xC0 and BmcAcpi's SW SMIs 0xC0–0xC5.
    const BMC_SX_NOTIFY: Option<BmcSxNotify> = Some(BmcSxNotify {
        kcs_port: bmc::KCS_PORT,
        kcs_spacing: bmc::KCS_SPACING as u8,
        sw_smi_base: 0xC0,
    });

    fn policy(generation: Generation) -> Policy {
        Policy {
            df: policy::df(),
            nbio: policy::nbio(),
            power: policy::power(),
            ccx: policy::ccx(),
            fch_env: policy::fch_env(),
            dxio: policy::dxio(generation),
        }
    }

    fn early_init<H: FchHw>(fch: &H) -> Result<(), BoardError> {
        bmc::open_lpc_decodes(fch);
        bmc::early_init().map(drop).map_err(|_| BoardError::Device)
    }

    fn detect_pads(generation: Generation) -> &'static [Pad] {
        match generation {
            Generation::Milan => &DETECT_MILAN,
            Generation::Rome | Generation::Naples => &DETECT_ROME,
        }
    }

    fn gpio(generation: Generation) -> Gpio {
        Gpio {
            early: gpio::rome_oem_pads(generation),
            table: &gpio::CPM_GPIO,
        }
    }

    fn dxio_engines(
        generation: Generation,
        levels: &[bool],
        timer: &dyn Timer,
    ) -> Result<heapless::Vec<Engine<'static>, MAX_ENGINES>, BoardError> {
        let level = |i: usize| levels.get(i).copied().unwrap_or(true);
        // SAFETY: `early_init` opened the KCS window; the flow owns it.
        let mut kcs = unsafe { bmc::kcs(TimerDelay(timer)) };
        let detected = Detected {
            m2: [M2Mode::from_detect(level(0)), M2Mode::from_detect(level(1))],
            m2_gen3: bmc::m2_gen3_cap(&mut kcs),
        };
        Ok(dxio::engines(generation, &Options::BUILD, &detected))
    }

    fn acpi_lpc_aml() -> Vec<u8> {
        bmc::lpc_aml()
    }

    fn acpi_board(lpc_aml: &[u8]) -> AcpiBoard<'_> {
        bmc::acpi_board(lpc_aml)
    }
}
