//! The S8030's facts and silicon policy ([`fstart_platform_sp3::board::Board`])
//! and the board work its reset `main` calls:
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
use fstart_platform_sp3::smm::SleepNotify;

use crate::dxio::{Detected, M2Mode, Options};
use fstart_driver_amd::sp3::soc::Generation;

use crate::{Board, bmc, dxio, gpio, policy};

/// The M.2 detect pads in each generation's OEM form (CN1, CN3).
static DETECT_ROME: [Pad; 2] = gpio::m2_detect_pads(Generation::Rome);
static DETECT_MILAN: [Pad; 2] = gpio::m2_detect_pads(Generation::Milan);

impl board::Board for Board {
    const NAME: &'static str = "Tyan S8030";

    fn smbios_identity() -> fstart_acpi::smbios::SmbiosIdentity<'static> {
        fstart_acpi::smbios::SmbiosIdentity::new("TYAN", "S8030")
    }
    const CONSOLE: Ns16550Config = bmc::CONSOLE;
    /// J40 takes an LPC TPM module (Tyan lists TM-TPM2-I-3353, SLB9665).
    /// Unlike the vendor's Setup default it is used when fitted: without a
    /// module the probe finds nothing.
    const DISCRETE_TPM: bool = true;
    /// The vendor DSDT's `BSMI` = 0xC0 and BmcAcpi's SW SMIs 0xC0–0xC5.
    const BMC_SX_NOTIFY: Option<SleepNotify> = Some(SleepNotify {
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
            sata: [
                fstart_driver_amd::sp3::fch::huashan::sata::SataPolicy::socket0(),
                fstart_driver_amd::sp3::fch::huashan::sata::SataPolicy::socket1(),
            ],
            dxio: policy::dxio(generation),
        }
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

    fn acpi_lpc_aml() -> Vec<u8> {
        bmc::lpc_aml()
    }

    fn acpi_board(lpc_aml: &[u8]) -> AcpiBoard<'_> {
        bmc::acpi_board(lpc_aml)
    }
}

/// Before the console: open the LPC decodes and set up the AST2500's
/// Super I/O for the console UART.
pub fn early_init<H: FchHw>(fch: &H) -> Result<(), BoardError> {
    bmc::open_lpc_decodes(fch);
    bmc::early_init().map(drop).map_err(|_| BoardError::Device)
}

/// The DXIO lane map: the detected M.2 modes and the BMC's Gen3 cap.
/// Needs the KCS window [`early_init`] opened.
pub fn dxio_engines<H: FchHw>(
    generation: Generation,
    levels: &[bool],
    _fch: &H,
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
