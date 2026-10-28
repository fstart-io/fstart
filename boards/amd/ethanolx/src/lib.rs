//! Experimental AMD Ethanol-X Milan port, using the board's own PSP/ABL.
//!
//! The console is FCH UART1 over MMIO, as in the vendor's early serial code.
//! Revision 0x30 PCIe policy uses board straps and I/O fuses, with native
//! NVMe hotplug and vendor-derived GPIO/redriver setup. Other revisions
//! are rejected before output changes. Mixed SATA/card layouts retain the
//! vendor Milan policy: legacy Ethernet descriptors are gated off because
//! this processor has no integrated xGbE controller.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod dxio;
mod i2c;
#[cfg(fstart_stage_env = "smm")]
mod smm;

use alloc::vec::Vec;
use fstart_acpi::devices::IpmiKcsAcpi;
use fstart_core::services::Timer;
use fstart_driver_amd::sp3::dxio::DxioPolicy;
use fstart_driver_amd::sp3::dxio::engine::MAX_ENGINES;
use fstart_driver_amd::sp3::dxio::lanes::Engine;
use fstart_driver_amd::sp3::dxio::ports::PcieFeatures;
use fstart_driver_amd::sp3::fch::gpio::{Pad, Pin};
use fstart_driver_amd::sp3::fch::huashan::sata::{Controller, SataPolicy};
use fstart_driver_amd::sp3::fch::lpc::{self, WideIo};
use fstart_driver_amd::sp3::fch::{FchHw, irq, uart};
use fstart_driver_amd::sp3::soc::Generation;
use fstart_driver_amd::sp3::{ccx, df};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_sp3::acpi::Board as AcpiBoard;
use fstart_platform_sp3::board::{self, BoardError, Gpio, Policy};
use fstart_platform_sp3::layout::Sp3BoardFacts;

pub struct Board;

impl Sp3BoardFacts for Board {
    const GENERATIONS: &'static [Generation] = &[Generation::Milan];
    // One 16 MiB BIOS image, not the BMC's mirrored 64 MiB export.
    const VENDOR_IMAGE: &'static str = "boards/amd/ethanolx/vendor/RXM100CB.bin";
    const MAX_CPUS: u16 = 256;
}

// RXM100CB AmdCpmOemInitPeim, $A10 at 0xffd6e3e4: one pad then 0x00ff.
const GPIO: [Pad; 1] = [Pad::from_cpm(Pin(89), 0x0988)];
const CONSOLE_UART: u8 = 1;
const BAUD: u32 = 115_200;

impl board::Board for Board {
    const NAME: &'static str = "AMD Ethanol-X";
    const CONSOLE: Ns16550Config = Ns16550Config {
        regs: AccessMode::Mmio {
            base: uart::base(CONSOLE_UART),
            reg_shift: 2,
            reg_width: 1,
        },
        clock_freq: uart::MMIO_CLOCK_HZ,
        baud_rate: BAUD,
    };

    fn policy(generation: Generation) -> Policy {
        let mut sata0 = SataPolicy::socket0();
        // Do not inherit the Tyan donor's SGPIO pads or external-port masks.
        // Ethanol-X's corresponding enclosure wiring is not established.
        for (index, controller) in sata0.controllers.iter_mut().enumerate() {
            controller.sgpio = None;
            controller.esp_ports = 0;
            // Mixed cards wire four SATA lanes on P0/P1. Retain them for
            // OS probing without claiming external/hotplug capabilities.
            controller.keep_ports = if index < 2 { 0x0f } else { 0 };
        }
        let mut sata1 = SataPolicy::socket1();
        // Fixed socket-1 P1 lanes 0..7 use AHCI controller 1. The same
        // P0/P1 -> controller 0/1 mapping is used by S8030's six/eight-port
        // engines and ESP masks. No socket-1 enclosure signals are assumed.
        sata1.controllers[1] = Controller {
            enabled: true,
            esp_ports: 0,
            keep_ports: 0xff,
            sgpio: None,
        };
        let mut dxio = DxioPolicy::new(generation);
        // Own RXM100CB PCDs 0x1C2 / 0x16D / 0x16E default true. These
        // are PCIe capability controls, not board reset/power signals.
        dxio.pcie_features = Some(PcieFeatures {
            ten_bit_tags: true,
            data_link_feature: true,
            data_link_exchange: true,
        });
        Policy {
            // Vendor UEFI: eight root buses, 00/20/40/60/80/a0/c0/e0.
            df: df::plan::Policy::df3().with_bus_counts([0x20; 8]),
            nbio: Default::default(),
            power: Default::default(),
            ccx: ccx::Policy::default(),
            fch_env: Default::default(),
            sata: [sata0, sata1],
            dxio,
        }
    }

    fn early_init<H: FchHw>(fch: &H) -> Result<(), BoardError> {
        uart::set_legacy_decode(fch, 0);
        uart::prepare_mmio(fch, CONSOLE_UART);
        // Vendor FUR1 IRQ4; establish it before ACPI reads the live routes.
        irq::write(fch.io(), 0xf5, 4);
        lpc::set_wide_io(fch, WideIo::W2, 0xca0, true);
        Ok(())
    }

    fn gpio(_generation: Generation) -> Gpio {
        Gpio {
            early: &[],
            table: &GPIO,
        }
    }

    fn dxio_engines<H: FchHw>(
        _generation: Generation,
        _levels: &[bool],
        fch: &H,
        timer: &dyn Timer,
    ) -> Result<heapless::Vec<Engine<'static>, MAX_ENGINES>, BoardError> {
        let detected = i2c::detect(timer)?;
        fstart_log::info!(
            "ethanolx: board id {:#x}, PCIe straps {:#x}",
            detected.board_id,
            detected.straps
        );
        if detected.board_id != 0x30 {
            fstart_log::error!(
                "ethanolx: unsupported board revision {:#x}",
                detected.board_id
            );
            return Err(BoardError::Configuration);
        }
        let detected = i2c::initialize(timer, detected)?;
        fstart_log::info!("ethanolx: PCIe straps after routing {:#x}", detected.straps);
        // BSP-only before MP, using the platform's socket-0 SMN pair.
        let fuses = fstart_driver_amd::sp3::nbio::Fuses::read(&fch.smn());
        let plan = dxio::plan(detected, fuses.limited_io()).map_err(|error| {
            let detail = alloc::format!("{error:?}");
            fstart_log::error!("ethanolx: PCIe policy: {}", detail.as_str());
            BoardError::Configuration
        })?;
        fstart_log::info!(
            "ethanolx: fuse1 {:#x}, {} DXIO engines",
            fuses.word[1],
            plan.engines.len()
        );
        if plan.disabled_xgbe != 0 {
            fstart_log::warn!(
                "ethanolx: {} xGbE descriptors disabled (legacy layout; no Milan xGbE controller)",
                plan.disabled_xgbe
            );
        }
        i2c::configure_redrivers(timer, plan.redrivers)?;
        Ok(plan.engines)
    }

    fn acpi_lpc_aml() -> Vec<u8> {
        // The shared FCH ACPI inventory describes the powered MMIO UART.
        Vec::new()
    }

    fn acpi_board(lpc_aml: &[u8]) -> AcpiBoard<'_> {
        AcpiBoard {
            ipmi: Some(IpmiKcsAcpi {
                port: fstart_driver_ipmi::kcs::DEFAULT_PORT,
                spacing: fstart_driver_ipmi::kcs::DEFAULT_SPACING as u8,
                uid: 0,
                spec_revision: fstart_driver_ipmi::SPEC_REVISION_2_0,
            }),
            lpc_aml,
            ..AcpiBoard::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_vendor_capability_and_cpm_policy() {
        let policy = <Board as board::Board>::policy(Generation::Milan);
        assert_eq!(
            policy.dxio.pcie_features,
            Some(PcieFeatures {
                ten_bit_tags: true,
                data_link_feature: true,
                data_link_exchange: true,
            })
        );
        assert_eq!(GPIO[0].cpm_bit19, Some(false));
        assert_eq!(
            GPIO[0].direction,
            fstart_driver_amd::sp3::fch::gpio::Direction::Keep
        );
    }

    #[test]
    fn socket1_fixed_sata_is_not_disabled_by_the_donor_policy() {
        let policy = <Board as board::Board>::policy(Generation::Milan);
        for (index, controller) in policy.sata[1].controllers.iter().enumerate() {
            assert_eq!(controller.enabled, index == 1);
            assert_eq!(controller.esp_ports, 0);
            assert_eq!(controller.keep_ports, if index == 1 { 0xff } else { 0 });
            assert!(controller.sgpio.is_none());
        }
        assert!(
            policy.sata[0]
                .controllers
                .iter()
                .all(|c| c.sgpio.is_none() && c.esp_ports == 0)
        );
    }
}
