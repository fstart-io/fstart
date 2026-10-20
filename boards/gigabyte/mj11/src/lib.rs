// SPDX-License-Identifier: MIT OR Apache-2.0
//! Gigabyte MJ11-EC1 (G431-MM0) and MJ11-EC0: EPYC Embedded 3000 (SP4r2) with
//! an AST2500 BMC.
#![no_std]
extern crate alloc;

mod acpi;
mod bmc;
mod dxio;
mod gpio;
mod i2c;
#[cfg(fstart_stage_env = "smm")]
pub mod smm;
mod variant;

use fstart_driver_amd::{
    fam17h::registers::Smn,
    lpc::{IoPortDecode, Lpc},
    zeppelin::{
        self as soc,
        dxio::Descriptor,
        fch::{
            Acpimmio, Bus,
            irq::{self, Route},
        },
    },
};
use fstart_driver_superio::aspeed_ast2400::{self as ast, Ast2400, SuperIoConfig};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_pci::ConfigRegionAccess;
use fstart_platform_amd::zeppelin::BoardCtx;

pub struct Board;

/// The BMC's diagnostic interrupt button.
const NMI_BUTTON_GEVENT: u8 = 11;

const SIO_PORT: u16 = 0x2e;
const COM1: u16 = 0x3f8;
const COM1_IRQ: u8 = 4;

/// The IOAPIC side matches the vendor firmware; the vendor PIC values aren't
/// known, so INTA-D are left for the OS to route through ACPI. MISC..MISC2
/// keep their native values (the platform enforces that).
const IRQ_ROUTES: &[Route] = &[
    Route {
        source: irq::A,
        pic: irq::NC,
        apic: 16,
    },
    Route {
        source: irq::B,
        pic: irq::NC,
        apic: 17,
    },
    Route {
        source: irq::C,
        pic: irq::NC,
        apic: 18,
    },
    Route {
        source: irq::D,
        pic: irq::NC,
        apic: 19,
    },
    Route {
        source: irq::SCI,
        pic: 9,
        apic: 9,
    },
    Route {
        source: irq::GPIO,
        pic: 7,
        apic: 7,
    },
];

/// The PM1 write of the vendor AmdCpmOemInitPeim and SMBusSwitch: write back
/// the pending PM1 status plus bits 15:14 (all W1C) and clear
/// PCIEXP_WAKE_DIS.
fn clear_pm1(fch: &mut impl Bus) {
    let acpi = Acpimmio::DIE0;
    fch.update(acpi.acpi::<u16, ()>(0), 0, 0xc000);
    fch.update(acpi.acpi::<u16, ()>(2), 1 << 14, 0);
}

/// GbtPreBootSvid: the AST2500 VGA on the last bus behind 00:01.1 gets the
/// Gigabyte subsystem ID; config register 0x80 unlocks the write.
fn ast_vga_set_ssid(pci: &impl ConfigRegionAccess) {
    use fstart_pci::PciAddress;
    // SAFETY: the enumerated root port and the device behind it.
    unsafe {
        let bus = (pci.read(PciAddress::new(0, 0, 1, 1), 0x18) >> 16) as u8;
        let vga = PciAddress::new(0, bus, 0, 0);
        if bus == 0 || pci.read(vga, 0) != 0x2000_1a03 {
            return;
        }
        pci.write(vga, 0x80, 0xaa1a_03aa);
        pci.write(
            vga,
            0x2c,
            <Board as fstart_platform_amd::zeppelin::Board>::SUBSYSTEM_ID,
        );
        pci.write(vga, 0x80, 0);
    }
}

impl fstart_platform_amd::zeppelin::Board for Board {
    /// Gigabyte.
    const SUBSYSTEM_ID: u32 = 0x1000_1458;
    const IRQ_ROUTES: &'static [Route] = IRQ_ROUTES;
    const SMI_GEVENTS: u32 = 1 << NMI_BUTTON_GEVENT;

    fn dxio_descriptors(_ctx: &mut BoardCtx<'_>, die: u8) -> &'static [Descriptor] {
        if die == 0 { dxio::DEFAULT } else { &[] }
    }

    fn fch_reset_done(ctx: &mut BoardCtx<'_>) {
        let mut fch = ctx.fch();
        for pad in i2c::PADS {
            fch.set(Acpimmio::DIE0.iomux::<u8, ()>(pad.into()), 1);
        }
        // SAFETY: nothing else uses I2C3.
        match unsafe { soc::i2c::controller(i2c::BUS, ctx.clock) } {
            Ok(mut bus) => {
                i2c::program_vrs(&mut bus, ctx.clock, variant::VR_IMAGES);
                i2c::program_redrivers(&mut bus, variant::REDRIVERS);
            }
            Err(_) => fstart_log::error!("I2C3 setup failed, VRs not programmed"),
        }
        clear_pm1(&mut fch);
    }

    /// The vendor's PCIe TX presets and USB2 PHY values after training.
    fn dxio_training_done(ctx: &mut BoardCtx<'_>) {
        let mut smn = ctx.dies[0].smn();
        for &(register, value) in variant::POST_TRAINING_SMN {
            smn.write(register, value);
        }
        gpio::wait_for_bmc(ctx);
    }

    fn mainboard_final(ctx: &mut BoardCtx<'_>) {
        if fstart_driver_amd::gpio::configure(&mut ctx.gpio(), &soc::GPIO_BANK, gpio::VENDOR, &[])
            .is_err()
        {
            fstart_log::error!("Vendor GPIO table rejected");
        }
        ast_vga_set_ssid(&ctx.dies[0].pci());
        bmc::post_done(ctx);
    }

    /// SMBusSwitch at ReadyToBoot. Inferred purpose: hand the SMBus to the
    /// OS. The vendor writes the IOMUX of pins 136-139 through SMN; it is
    /// the same register as the ACPI MMIO one.
    fn ready_to_boot(ctx: &mut BoardCtx<'_>) {
        let mut fch = ctx.fch();
        let iomux = Acpimmio::DIE0;
        // Bit 1 of the GPIO137 IOMUX byte set, bits 7:4 of pin 139 cleared.
        fch.update(iomux.iomux::<u32, ()>(136), 0xf000_0200, 0x0000_0200);
        // GPIO137 driven low, keeping the drive strength and STS_EN bits.
        fch.update(iomux.gpio::<u32, ()>(4 * 137), !0x0006_0800, 1 << 23);
        clear_pm1(&mut fch);
        // Pins 113-117 back to function 0 (SMBus0): two unaligned dword
        // writes, as in the vendor code.
        fch.set(iomux.iomux::<u32, ()>(113), 0);
        fch.set(iomux.iomux::<u32, ()>(114), 0);
    }

    fn early_init(lpc: &mut Lpc<impl ConfigRegionAccess>) {
        lpc.enable_sio_2e();
        lpc.enable_io_decode(IoPortDecode::SERIAL0::SET);
        // AST2500 SIO runtime registers (0xa00-0xbff) and the IPMI KCS interface.
        let _ = lpc.set_wideio_range(0xa00, 512);
        let _ = lpc.set_wideio_range(0xca0, 16);
        let Ok(sio) = Ast2400::new_at_base(SuperIoConfig::default(), SIO_PORT) else {
            return;
        };
        // Route the BMC's UART1 pins (SCU84[23:16]: TXD1, RXD1 and the modem
        // lines); the vendor firmware also clears SCU90[5:4].
        sio.ahb_update32(ast::SCU_BASE, 0, ast::SCU_PROTECTION_KEY);
        sio.ahb_update32(ast::SCU_BASE + 0x84, 0xff00_ffff, 0x00ff_0000);
        sio.ahb_update32(ast::SCU_BASE + 0x90, 0xffff_ffcf, 0);
        // SUART4 carries the BMC's IPMI SOL; the host must leave it alone.
        sio.disable(&[
            ast::SUART2,
            ast::SWC,
            ast::KBC,
            ast::GPIO,
            ast::SUART3,
            ast::SUART4,
            ast::MAILBOX,
        ]);
        sio.enable_uart(ast::SUART1, COM1, COM1_IRQ, 0);
    }

    fn acpi() -> fstart_platform_amd::zeppelin::BoardAcpi {
        acpi::tables()
    }

    fn console() -> Ns16550Config {
        Ns16550Config {
            regs: AccessMode::Pio { base: COM1 as u64 },
            clock_freq: 1_843_200,
            baud_rate: 115_200,
        }
    }
}
