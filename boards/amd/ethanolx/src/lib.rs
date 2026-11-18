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

pub mod dxio;
mod i2c;
/// The SMM handler, built into the separate SMM image.
#[cfg(feature = "smm")]
mod smm;

use fstart_core::services::Timer;
use fstart_driver_amd::ccx::init as ccx;
use fstart_driver_amd::df;
use fstart_driver_amd::dxio::DxioPolicy;
use fstart_driver_amd::dxio::lanes::Engine;
use fstart_driver_amd::dxio::ports::PcieFeatures;
use fstart_driver_amd::dxio::wire::MAX_ENGINES;
use fstart_driver_amd::fch::gpio::{Pad, Pin};
use fstart_driver_amd::fch::huashan::sata::{Controller, SataPolicy};
use fstart_driver_amd::fch::irq;
use fstart_driver_amd::fch::lpc::{self, WideIo};
use fstart_driver_amd::fch::{FchHw, uart};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_amd::acpi::Board as AcpiBoard;
use fstart_platform_amd::board::SocId;
use fstart_platform_amd::sp3::board::{self, BoardError, Gpio, Policy};
use fstart_platform_amd::sp3::silicon::Generation;

pub struct Board;

// RXM100CB AmdCpmOemInitPeim, $A10 at 0xffd6e3e4: one pad then 0x00ff.
const GPIO: [Pad; 1] = [Pad::from_cpm(Pin(89), 0x0988)];
const CONSOLE_UART: u8 = 1;
const BAUD: u32 = 115_200;

impl fstart_platform_amd::board::Board for Board {
    const NAME: &'static str = "AMD Ethanol-X";
    const SOCS: &'static [SocId] = &[SocId::Milan];
    const MAX_CPUS: u16 = 256;
    const OPTION_SETS: &'static [&'static dyn fstart_options::Describe] =
        &[&POLICY_OPTIONS, &dxio::SETUP];
}

/// The policy knobs over Milan's policy, as the boot resolves them.
static POLICY_OPTIONS: board::PolicyOptions = board::PolicyOptions {
    config,
    generation: Generation::Milan,
};

/// The policy knobs the user may override.
pub static OPTIONS: [fstart_options::Knob<Policy>; 1] = [board::knob::POWER_FAIL];

/// The board's configuration of its SoC for `generation`.
pub fn config(generation: Generation) -> fstart_platform_amd::sp3::board::Config {
    fstart_platform_amd::sp3::board::Config {
        policy: policy(generation),
        options: &OPTIONS,
    }
}

/// The board's console: the FCH's MMIO UART 1.
pub const CONSOLE: Ns16550Config = Ns16550Config {
    regs: AccessMode::Mmio {
        base: uart::base(CONSOLE_UART),
        reg_shift: 2,
        reg_width: 1,
    },
    clock_freq: uart::MMIO_CLOCK_HZ,
    baud_rate: BAUD,
};

/// I/O bytes per root bridge, logical (bus) order, from the vendor's
/// request table: RXM100CB CrbPei fills FABRIC_RESOURCE_FOR_EACH_RB
/// `IoSize[socket][IOMS]` (stores at `0xffdbc2aa`–`0xffdbc31a`, PCD 0x42)
/// with s0 0x2000, 0x2000, 0x1000, 0x3000 and s1 0x2000, 0x1000, 0x1000,
/// 0x3000, and the vendor's NV copy `ResourceSizeForEachRb` keeps them.
/// The ABL bus map has IOMS 3 at the bottom of each socket's buses.
pub const IO_SIZES: [u32; df::MAX_ROOT_BRIDGES] = [
    0x3000, // rb 0.3, buses 0x00: primary, plus the 0x1000 legacy ports
    0x1000, // rb 0.2, buses 0x20
    0x2000, // rb 0.1, buses 0x40
    0x2000, // rb 0.0, buses 0x60: the AST2500 and slot PCIE1
    0x3000, // rb 1.3, buses 0x80
    0x1000, // rb 1.2, buses 0xa0
    0x1000, // rb 1.1, buses 0xc0
    0x2000, // rb 1.0, buses 0xe0: the last, to the end of the I/O space
];

/// The DF resource planner policy: the DF3 equal split with the vendor's
/// eight root buses (00/20/40/60/80/a0/c0/e0) and I/O requests.
pub const fn df() -> df::plan::Policy {
    df::plan::Policy::df3()
        .with_bus_counts([0x20; df::MAX_ROOT_BRIDGES])
        .with_io_sizes(IO_SIZES)
}

/// The silicon policy for `generation`.
pub fn policy(generation: Generation) -> Policy {
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
    let mut dxio = DxioPolicy::new(generation.silicon());
    // The hotplug expanders sit behind the PCA9545A at 0x70 on I2C0.
    dxio.hotplug.i2c_switch_base = 0x70;
    // Own RXM100CB PCDs 0x1C2 / 0x16D / 0x16E default true. These
    // are PCIe capability controls, not board reset/power signals.
    dxio.pcie_features = Some(PcieFeatures {
        ten_bit_tags: true,
        data_link_feature: true,
        data_link_exchange: true,
    });
    Policy {
        df: df(),
        nbio: Default::default(),
        power: Default::default(),
        ccx: ccx::Policy::default(),
        fch_env: Default::default(),
        sata: [sata0, sata1],
        dxio,
    }
}

/// The GPIO program: the CPM table's one pad.
pub const GPIO_PROGRAM: Gpio = Gpio {
    early: &[],
    table: &GPIO,
};

/// The table identity: board identity only.
pub fn smbios_identity() -> fstart_acpi::smbios::SmbiosIdentity<'static> {
    fstart_acpi::smbios::SmbiosIdentity::new("AMD", "Ethanol-X")
}

/// The board's ACPI: the BMC's KCS. The shared FCH ACPI inventory already
/// describes the powered MMIO UART, so there is no LPC AML.
pub fn acpi_board() -> AcpiBoard {
    AcpiBoard {
        ipmi: Some(fstart_driver_ipmi::kcs::DEFAULT_ACPI),
        ..AcpiBoard::default()
    }
}

/// Before the console: the MMIO console UART, its legacy IRQ and the KCS
/// decode.
pub fn early_init<H: FchHw>(fch: &H) -> Result<(), BoardError> {
    uart::set_legacy_decode(fch, 0);
    uart::prepare_mmio(fch, CONSOLE_UART);
    // Vendor FUR1 IRQ4; establish it before ACPI reads the live routes.
    irq::write(fch.io(), 0xf5, 4);
    lpc::set_wide_io(fch, WideIo::W2, 0xca0, true);
    Ok(())
}

/// The socket-0 PCIe layout from the board ID and PCIe straps on I2C and
/// the I/O fuses, with the strap routing set up.
pub fn dxio_layout<H: FchHw>(fch: &H, timer: &dyn Timer) -> Result<dxio::Layout, BoardError> {
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
    let fuses = fstart_driver_amd::nbio::Fuses::read(&fch.smn());
    fstart_log::info!("ethanolx: fuse1 {:#x}", fuses.word[1]);
    dxio::Layout::new(detected, fuses.limited_io()).map_err(|_| BoardError::Configuration)
}

/// The DXIO lane map of `layout`, with the redrivers configured to match.
pub fn dxio_engines(
    layout: &dxio::Layout,
    timer: &dyn Timer,
) -> Result<heapless::Vec<Engine<'static>, MAX_ENGINES>, BoardError> {
    let plan = dxio::plan(layout).map_err(|error| {
        let detail = alloc::format!("{error:?}");
        fstart_log::error!("ethanolx: PCIe policy: {}", detail.as_str());
        BoardError::Configuration
    })?;
    fstart_log::info!("ethanolx: {} DXIO engines", plan.engines.len());
    if plan.disabled_xgbe != 0 {
        fstart_log::warn!(
            "ethanolx: {} xGbE descriptors disabled (legacy layout; no Milan xGbE controller)",
            plan.disabled_xgbe
        );
    }
    i2c::configure_redrivers(timer, plan.redrivers)?;
    Ok(plan.engines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_offered_knob_has_its_own_key() {
        let metas = fstart_log::option::KNOBS
            .iter()
            .map(|k| &k.meta)
            .chain(OPTIONS.iter().map(|k| &k.meta))
            .chain(dxio::KNOBS.iter().map(|k| &k.meta));
        assert_eq!(fstart_options::duplicate_key(metas), None);
    }

    #[test]
    fn own_vendor_capability_and_cpm_policy() {
        let policy = policy(Generation::Milan);
        assert_eq!(
            policy.dxio.pcie_features,
            Some(PcieFeatures {
                ten_bit_tags: true,
                data_link_feature: true,
                data_link_exchange: true,
            })
        );
    }

    #[test]
    fn vendor_io_sizes_fit_the_bmc_and_pcie1() {
        use df::plan::{Input, Plan, Window, plan, plan_buses};
        use df::{RootBridge, Version, v3::Df3};

        // 2P Milan as the bench logs it (`df: rb s.i buses`): IOMS 3 at
        // the bottom of each socket's 0x80 buses.
        let rbs: Vec<RootBridge> = (0..8u8)
            .map(|k| {
                let (socket, index) = (k / 4, k % 4);
                let bus_base = socket * 0x80 + (3 - index) * 0x20;
                RootBridge {
                    socket,
                    index,
                    fabric_id: (u16::from(socket) << 5) + 0x18 + u16::from(index),
                    bus_base,
                    bus_limit: bus_base + 0x1f,
                    primary: (socket, index) == (0, 3),
                }
            })
            .collect();
        let input = Input {
            tom: 0xb000_0000,
            tom2: 0x40_5000_0000,
            ecam_base: 0xe000_0000,
            ecam_size: 0x1000_0000,
            phys_addr_bits: 48,
            root_bridges: &rbs,
            mmio_pairs: Df3::MMIO_PAIRS,
        };
        let policy = policy(Generation::Milan).df;
        assert_eq!(plan_buses(&rbs, &policy), Ok(None), "ABL map kept");
        let got = plan(&input, &policy).unwrap();

        // The vendor's windows, primary first, then by bus.
        let io: Vec<(u8, u64, u64)> = got
            .io
            .iter()
            .map(|r| (r.pair, r.window.base, r.window.end()))
            .collect();
        assert_eq!(
            io,
            [
                (0, 0, 0x4000),
                (1, 0x4000, 0x5000),
                (2, 0x5000, 0x7000),
                (3, 0x7000, 0x9000),
                (4, 0x9000, 0xc000),
                (5, 0xc000, 0xd000),
                (6, 0xd000, 0xe000),
                (7, 0xe000, 0x200_0000),
            ]
        );
        // MMIO below 4 GiB as on the bench, which the I/O sizes leave alone.
        let below: Vec<(u8, u8, u8, u64, u64)> = got
            .root_bridges
            .iter()
            .map(|r| (r.socket, r.index, r.bus.base, r.below.base, r.below.size))
            .collect();
        assert_eq!(
            below,
            [
                (0, 0, 0x60, 0xf000_0000, 0x700_0000),
                (0, 1, 0x40, 0xd800_0000, 0x800_0000),
                (0, 2, 0x20, 0xd000_0000, 0x800_0000),
                (0, 3, 0x00, 0xf700_0000, 0x7c0_0000),
                (1, 0, 0xe0, 0xc800_0000, 0x800_0000),
                (1, 1, 0xc0, 0xc000_0000, 0x800_0000),
                (1, 2, 0xa0, 0xb800_0000, 0x800_0000),
                (1, 3, 0x80, 0xb000_0000, 0x800_0000),
            ]
        );

        // rb 0.0 has two root ports with I/O: 60:05.2 (AST1150 bridge,
        // AST2500 VGA) and 60:03.1 (PCIE1). The allocator aligns an I/O
        // BAR behind a bridge to 4 KiB, so each takes a 4 KiB window.
        let rb00 = |p: &Plan| p.root_bridges[0].io_pool.unwrap();
        assert_eq!(rb00(&got), Window::new(0x7000, 0x2000));
        assert!(rb00(&got).size >= 2 * 0x1000);
        // The equal split left it 4 KiB, all of it taken by the AST2500.
        let equal = df::plan::Policy::df3().with_bus_counts([0x20; df::MAX_ROOT_BRIDGES]);
        assert_eq!(rb00(&plan(&input, &equal).unwrap()).size, 0x1000);
    }

    #[test]
    fn socket1_fixed_sata_is_not_disabled_by_the_donor_policy() {
        let policy = policy(Generation::Milan);
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
