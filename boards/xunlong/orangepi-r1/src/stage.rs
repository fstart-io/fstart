//! Orange Pi R1 binding for the handwritten H3 flow.
use crate::Board;
use fstart_core::{mmio32, services::ServiceError};
use fstart_driver_sunxi::h3_ccu::{H3_PIO_BASE, H3_UART0_BASE};
use fstart_driver_sunxi::pio::{PioGen, Pull, SunxiPio, PORT_A};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_sunxi::h3::{
    H3Board, H3BuildSelectedPayload, SunxiEarlyBoard, SunxiEarlyBoardHooks, SunxiEarlyCtx, H3,
};
pub struct OrangePiR1Hooks;
impl SunxiEarlyBoardHooks<H3> for OrangePiR1Hooks {
    fn before_console(&mut self, _: &mut SunxiEarlyCtx<H3>) -> Result<(), ServiceError> {
        let pio = SunxiPio::new(mmio32(H3_PIO_BASE), PioGen::Legacy);
        pio.set_function(PORT_A, 4, 2);
        pio.set_function(PORT_A, 5, 2);
        pio.set_pull(PORT_A, 5, Pull::Up);
        Ok(())
    }
}
impl SunxiEarlyBoard for Board {
    type Platform = H3;
    type Hooks = OrangePiR1Hooks;
    fn hooks() -> Result<Self::Hooks, ServiceError> {
        Ok(OrangePiR1Hooks)
    }
}
impl H3Board for Board {
    type Payload = H3BuildSelectedPayload;
    const CONFIG: &'static fstart_platform_sunxi::h3::H3Config = &crate::ORANGEPI_R1_H3;
    const CONSOLE_CONFIG: Ns16550Config = Ns16550Config {
        regs: AccessMode::Mmio {
            base: H3_UART0_BASE,
            reg_shift: 2,
            reg_width: 4,
        },
        clock_freq: 24_000_000,
        baud_rate: 115_200,
    };
}
