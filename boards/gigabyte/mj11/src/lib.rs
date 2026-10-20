// SPDX-License-Identifier: MIT OR Apache-2.0
//! Gigabyte MJ11-EC1 (G431-MM0) and MJ11-EC0: EPYC Embedded 3000 (SP4r2) with
//! an AST2500 BMC.
#![no_std]
mod dxio;

use fstart_driver_amd::{
    lpc::{IoPortDecode, Lpc},
    zeppelin::{
        dxio::Descriptor,
        fch::irq::{self, Route},
    },
};
use fstart_platform_amd::zeppelin::BoardCtx;
use fstart_driver_superio::aspeed_ast2400::{self as ast, Ast2400, SuperIoConfig};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_pci::ConfigRegionAccess;

pub struct Board;

const SIO_PORT: u16 = 0x2e;
const COM1: u16 = 0x3f8;

/// The IOAPIC side matches the vendor firmware; the vendor PIC values aren't
/// known, so INTA-D are left for the OS to route through ACPI. MISC..MISC2
/// keep their native values (the platform enforces that).
const IRQ_ROUTES: &[Route] = &[
    Route { source: irq::A, pic: irq::NC, apic: 16 },
    Route { source: irq::B, pic: irq::NC, apic: 17 },
    Route { source: irq::C, pic: irq::NC, apic: 18 },
    Route { source: irq::D, pic: irq::NC, apic: 19 },
    Route { source: irq::SCI, pic: 9, apic: 9 },
    Route { source: irq::GPIO, pic: 7, apic: 7 },
];

impl fstart_platform_amd::zeppelin::Board for Board {
    /// Gigabyte.
    const SUBSYSTEM_ID: u32 = 0x1000_1458;
    const IRQ_ROUTES: &'static [Route] = IRQ_ROUTES;

    fn dxio_descriptors(_ctx: &mut BoardCtx<'_>, die: u8) -> &'static [Descriptor] {
        if die == 0 { dxio::DEFAULT } else { &[] }
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
        sio.enable_uart(ast::SUART1, COM1, 4, 0);
    }

    fn console() -> Ns16550Config {
        Ns16550Config {
            regs: AccessMode::Pio { base: COM1 as u64 },
            clock_freq: 1_843_200,
            baud_rate: 115_200,
        }
    }
}
