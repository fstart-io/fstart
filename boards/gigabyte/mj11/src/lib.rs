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
    fch::{
        self, ACPI_PM1_ENABLE, ACPI_PM1_STATUS, Bus, Fch, Pm1Enable, Pm1Status,
        gpio::Control,
        iomux,
        lpc::{IoPortDecode, Lpc},
        taishan::irq::{self, Route},
    },
    smn::{apply, write},
    soc::zeppelin::{self as soc, Zeppelin},
};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};
use fstart_driver_superio::aspeed_ast2400::{self as ast, Ast2400, SuperIoConfig};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_pci::ConfigRegionAccess;
use fstart_platform_amd::{
    noncar::BoardCtx,
    zeppelin::{Config, DxioConfig, FchConfig},
};

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
fn clear_pm1(fch: &Fch<impl Bus>) {
    fch.reg(ACPI_PM1_STATUS)
        .modify(Pm1Status::WAK::SET + Pm1Status::PCIEXP_WAKE::SET);
    fch.reg(ACPI_PM1_ENABLE)
        .modify(Pm1Enable::PCIEXP_WAKE_DIS::CLEAR);
}

register_bitfields![u32,
    /// The IOMUX bytes of pins 136-139 as one dword.
    Iomux136 [PIN137_BIT1 OFFSET(9) NUMBITS(1) [], PIN139_HIGH OFFSET(28) NUMBITS(4) []]
];
/// SMBusSwitch's GPIO137 and IOMUX settings.
const GPIO137: u8 = 137;

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
        pci.write(vga, 0x2c, CONFIG.subsystem_id);
        pci.write(vga, 0x80, 0);
    }
}

const CONFIG: Config = Config {
    // Gigabyte
    subsystem_id: 0x1000_1458,
    fch: FchConfig {
        irq_routes: IRQ_ROUTES,
        smi_gevents: 1 << NMI_BUTTON_GEVENT,
        ..FchConfig::DEFAULT
    },
    dxio: DxioConfig {
        descriptors: [dxio::DEFAULT, &[]],
    },
};

impl fstart_platform_amd::noncar::Board for Board {
    type Soc = Zeppelin;
    const CONFIG: &'static Config = &CONFIG;

    fn fch_reset_done(ctx: &mut BoardCtx<'_, Zeppelin>) {
        let gpio = ctx.gpio();
        if i2c::PADS
            .iter()
            .any(|&pad| gpio.set_function(pad, i2c::FUNCTION).is_err())
        {
            fstart_log::error!("I2C3 pads not routed");
        }
        // SAFETY: nothing else uses I2C3.
        match unsafe {
            fch::i2c::controller::<Zeppelin, _>(i2c::BUS, &soc::I2C_TIMING, ctx.clock)
        } {
            Ok(mut bus) => {
                i2c::program_vrs(&mut bus, ctx.clock, variant::VR_IMAGES);
                i2c::program_redrivers(&mut bus, variant::REDRIVERS);
            }
            Err(_) => fstart_log::error!("I2C3 setup failed, VRs not programmed"),
        }
        clear_pm1(&ctx.fch());
    }

    /// The vendor's PCIe TX presets and USB2 PHY values after training.
    fn dxio_training_done(ctx: &mut BoardCtx<'_, Zeppelin>) {
        let records = variant::POST_TRAINING_SMN
            .iter()
            .map(|&(register, value)| write(register, value));
        apply(&ctx.dies[0].smn(), records);
        gpio::wait_for_bmc(ctx);
    }

    fn mainboard_final(ctx: &mut BoardCtx<'_, Zeppelin>) {
        if ctx.gpio().configure(gpio::VENDOR, &[]).is_err() {
            fstart_log::error!("Vendor GPIO table rejected");
        }
        ast_vga_set_ssid(&ctx.dies[0].pci());
        bmc::post_done(ctx);
    }

    /// SMBusSwitch at ReadyToBoot. Inferred purpose: hand the SMBus to the
    /// OS. The vendor writes the IOMUX of pins 136-139 through SMN; it is
    /// the same register as the ACPI MMIO one.
    fn ready_to_boot(ctx: &mut BoardCtx<'_, Zeppelin>) {
        let fch = ctx.fch();
        // Bit 1 of the GPIO137 IOMUX byte set, bits 7:4 of pin 139 cleared.
        fch.reg(iomux::at::<u32, Iomux136::Register>(136))
            .modify(Iomux136::PIN137_BIT1::SET + Iomux136::PIN139_HIGH.val(0));
        // GPIO137 driven low, keeping the drive strength and STS_EN bits.
        if let Ok(control) = ctx.gpio().control(GPIO137) {
            let old = control.extract();
            control.write(
                Control::DRIVE_STRENGTH.val(old.read(Control::DRIVE_STRENGTH))
                    + Control::STATUS_ENABLE.val(old.read(Control::STATUS_ENABLE))
                    + Control::OUTPUT_ENABLE::SET,
            );
        }
        clear_pm1(&fch);
        // Pins 113-117 back to function 0 (SMBus0): two unaligned dword
        // writes, as in the vendor code.
        fch.reg(iomux::at::<u32, ()>(113)).set(0);
        fch.reg(iomux::at::<u32, ()>(114)).set(0);
    }

    fn early_init(lpc: &Lpc<impl Bus>) {
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

    fn acpi() -> fstart_platform_amd::acpi::BoardAcpi {
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
