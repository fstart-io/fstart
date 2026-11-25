// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD Onyx: AMD's SP5 reference board, one socket, Genoa (EPYC 9004),
//! twelve DDR5 channels, an AST2600 BMC. Wired as AMD's public reference
//! design; straps and fitted cards may differ per board.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

/// The SMM handler, built into the separate SMM image.
#[cfg(feature = "smm")]
mod smm;
pub mod wiring;

use fstart_driver_amd::ccx::rs as ccx;
use fstart_driver_amd::ccx::tsc::Tsc;
use fstart_driver_amd::dxio::rs::mailbox::Encoding;
use fstart_driver_amd::fch::songshan::uart;
use fstart_driver_amd::fch::{FchHw, gpio::Pin, irq, songshan::Perst};
use fstart_driver_amd::nbio::rs as nbio;
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_amd::acpi::Board as AcpiBoard;
use fstart_platform_amd::board::SocId;
use fstart_platform_amd::rs::links::{Behind, Soldered};
use fstart_platform_amd::rs::{Config, Mpio};

pub struct Board;

impl fstart_platform_amd::board::Board for Board {
    const NAME: &'static str = "AMD Onyx";
    const SOCS: &'static [SocId] = &[SocId::Genoa];
    /// 96 cores, SMT.
    const MAX_CPUS: u16 = 192;
    /// Corrected errors counted in SMM and reported to the BMC's SEL
    /// ([`sel`]), ten per thresholding SMI, on the OS's channel.
    const RAS: fstart_platform_amd::smm::ras::Policy = fstart_platform_amd::smm::ras::Policy {
        shared_bmc: true,
        ..fstart_platform_amd::smm::ras::Policy::THRESHOLDS
    };
    const BMC_EVENT: Option<fstart_platform_amd::board::BmcEvent> = Some(sel);

    fn acpi_board() -> AcpiBoard {
        acpi_board()
    }
}

/// The BMC's one KCS channel, KCS3 (`0xCA2`/`0xCA3`), which the OS's IPMI
/// driver uses too: the Onyx BMC bridges no other (its OpenBMC v2.11.0.16
/// device tree enables only `kcs@2c`, one kcsbridged on `ipmi-kcs3`;
/// KCS1, KCS2, KCS4 and the iBT are disabled, and it has no SSIF).
pub const KCS: (u16, u16) = (
    fstart_driver_ipmi::kcs::DEFAULT_PORT,
    fstart_driver_ipmi::kcs::DEFAULT_SPACING,
);

/// The report of a machine check (`fstart_platform_amd::smm::ras`) to the
/// BMC's SEL as a Platform Event Message, on the OS's KCS channel: from
/// SMM, while every CPU is in SMM (`Policy::shared_bmc`), and at boot for
/// the last boot's errors. A transaction of the OS's in progress (the
/// interface not idle) is left alone: the report is busy and tried again
/// on a later SMI. A transaction of SMM's that fails, in its request, its
/// response or its closing byte, is aborted (the KCS error exit) and the
/// interface handed back idle; that failure, or an interface that would
/// not go idle again (`Error::Stuck`), ends SMM's reports on this channel
/// for the boot (`Delivery::Failed`): it never starts another transaction
/// on an interface it could not hand back.
pub fn sel(
    event: &fstart_platform_amd::smm::ras::Event<'_>,
    timer: Tsc,
) -> fstart_platform_amd::smm::ras::Delivery {
    use fstart_platform_amd::smm::ras::{self, Delivery};
    let (port, spacing) = KCS;
    // SAFETY: the BMC's KCS ports; the OS cannot run meanwhile and the
    // transaction starts only when idle.
    let mut kcs = unsafe { fstart_driver_ipmi::kcs::Kcs::new(port, spacing, timer) }
        .with_poll_budget(ras::SEL_POLL_BUDGET);
    if !kcs.idle() {
        return Delivery::Busy;
    }
    match kcs.platform_event(&ras::sel(event)) {
        Ok(()) => Delivery::Sent,
        Err(_) => Delivery::Failed,
    }
}

/// The console: the SoC's UART1, the BMC's SOL (the public Onyx coreboot
/// configuration).
const CONSOLE_UART: u8 = 1;
const CONSOLE_IRQ: u8 = 4;
const BAUD: u32 = 115_200;

pub const CONSOLE: Ns16550Config = Ns16550Config {
    regs: AccessMode::Mmio {
        base: uart::base(CONSOLE_UART),
        reg_shift: 2,
        reg_width: 1,
    },
    clock_freq: fstart_driver_amd::fch::uart::MMIO_CLOCK_HZ,
    baud_rate: BAUD,
};

/// The onboard BCM5720 (lane 135) must be there. The BMC's display link
/// (lane 134) is the PSP's: MPIO is not asked for it.
const REQUIRED: [Soldered; 1] = [Soldered {
    socket: 0,
    lane: wiring::BCM5720,
    behind: Behind::Device,
}];

/// The PCIe slot resets: FCH pad 26 and pad 10 of the remote bank, at
/// drive strength 2 (openSIL's `GpioSlotResetControl`, written for this
/// reference board).
const PERST: [Perst; 2] = [
    Perst {
        pin: Pin(26),
        drive_strength: 2,
    },
    Perst {
        pin: Pin(266),
        drive_strength: 2,
    },
];

/// The board's configuration of Genoa: openSIL's Onyx configuration.
pub fn config() -> Config {
    Config {
        gpio: &[],
        // NbioGlobalCgOverride 0Fh: IOHC, SST and NBIF clock gating and
        // TPH completion on, SYSHUB gating off.
        nbio: nbio::Policy {
            clock_gating: true,
            syshub_clock_gating: false,
            tph_completer: true,
        },
        ccx: ccx::Policy::default(),
        mpio: Mpio {
            // CONFIG_MPIO_SKIP_PSP_MSG=1, unlike Siena's vendor firmware.
            psp_vetting: false,
            encoding: Encoding::LowByte,
            required: REQUIRED.into_iter().collect(),
            psp_trained: &wiring::PSP_TRAINED,
            perst: &PERST,
        },
        usb: wiring::USB,
        // The AST2600's display, behind the PSP-trained link of lane 134:
        // the legacy VGA display, whatever VGA card another root has.
        display: Some(wiring::BMC),
        legacy_devices: false,
        memory_channels: 0xfff,
        // The 9004 reporting ceiling: a 256 GiB RDIMM per slot.
        max_dimm_mib: 256 * 1024,
        // The PSP's EINJ, for RAS validation on this development board.
        einj: true,
    }
}

/// Before the console: UART1 powered on its pads, its interrupt on IRQ 4.
pub fn early_init(fch: &impl FchHw, mut delay: Tsc) -> Result<(), &'static str> {
    uart::prepare(fch, CONSOLE_UART, &mut delay).map_err(|_| "console UART")?;
    for mode in [irq::Mode::Pic, irq::Mode::Apic] {
        irq::write(
            fch.io(),
            irq::index(uart::intr_source(CONSOLE_UART), mode),
            CONSOLE_IRQ,
        );
    }
    Ok(())
}

/// The board part of the ACPI tables: the SPCR and DBG2 of the console
/// (its device, `\_SB.FUR1`, is the SoC's).
pub fn acpi_board() -> AcpiBoard {
    use fstart_acpi::Aml;
    let base = uart::base(CONSOLE_UART);
    let mut spcr = alloc::vec::Vec::new();
    fstart_acpi::spcr::build_spcr_16550_mmio32(base, fstart_driver_amd::fch::uart::MMIO_CLOCK_HZ)
        .to_aml_bytes(&mut spcr);
    let mut dbg2 = alloc::vec::Vec::new();
    fstart_acpi::dbg2::build_dbg2_16550_mmio32(&fstart_acpi::dbg2::Dbg2MmioConfig {
        base_addr: base,
        addr_size: 0x1000,
        namespace: "\\_SB_.FUR1",
    })
    .to_aml_bytes(&mut dbg2);
    AcpiBoard {
        tables: alloc::vec![spcr, dbg2],
        // KCS3, the OS's channel (`IPI0001` and SPMI, as the S8040's
        // vendor publishes its own): SMM shares it (`Board::RAS`).
        ipmi: Some(fstart_driver_ipmi::kcs::DEFAULT_ACPI),
        ..AcpiBoard::default()
    }
}

/// The table identity.
pub fn smbios_identity() -> fstart_acpi::smbios::SmbiosIdentity<'static> {
    fstart_acpi::smbios::SmbiosIdentity::new("AMD", "Onyx")
}

#[cfg(test)]
mod tests {
    use fstart_platform_amd::rs::links;

    /// The legacy VGA display is the BMC's, behind lane 134, which the PSP
    /// trains: its root port is the live one MPIO did not train on IOS 0's
    /// wrapper 2, the BCM5720's port's wrapper.
    #[test]
    fn the_display_is_the_bmcs_behind_its_psp_trained_link() {
        let config = super::config();
        assert_eq!(config.display, Some(134));
        assert_eq!(config.mpio.psp_trained, [(134, 134)]);
        assert_eq!(
            links::psp_trained(&[0, 1, 2, 3], config.mpio.psp_trained, 0, 2),
            Some(134)
        );
        assert!(config.mpio.required.iter().all(|s| s.lane != 134));
    }
}
