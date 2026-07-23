//! Banana Pi M1 binding for the handwritten A20 flow.

use fstart_core::{mmio32, services::ServiceError};
use fstart_driver_sunxi::a20_ccu::{A20_PIO_BASE, A20_UART0_BASE};
use fstart_driver_sunxi::pio::{PioGen, Pull, SunxiPio, PORT_B};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_sunxi::a20::{
    A20Board, A20BuildSelectedPayload, SunxiEarlyBoard, SunxiEarlyBoardHooks, SunxiEarlyCtx, A20,
};
use fstart_stage::{StageBoard, StageEnvironment};

use crate::Board;

impl StageBoard for Board {
    const NAME: &'static str = crate::BOARD_NAME;
    const PLATFORM: fstart_core::Platform = crate::PLATFORM;

    fn run_stage(env: StageEnvironment, handoff: usize) -> ! {
        A20::run_stage::<Self>(env, handoff)
    }
}

/// Board-specific seams for the fixed A20 flow.
pub struct BananaPiM1Hooks;

impl SunxiEarlyBoardHooks<A20> for BananaPiM1Hooks {
    fn before_console(&mut self, _ctx: &mut SunxiEarlyCtx<A20>) -> Result<(), ServiceError> {
        let pio = SunxiPio::new(mmio32(A20_PIO_BASE), PioGen::Legacy);
        pio.set_function(PORT_B, 22, 2);
        pio.set_function(PORT_B, 23, 2);
        pio.set_pull(PORT_B, 23, Pull::Up);
        Ok(())
    }
}

impl SunxiEarlyBoard for Board {
    type Platform = A20;
    type Hooks = BananaPiM1Hooks;

    fn hooks() -> Result<Self::Hooks, ServiceError> {
        Ok(BananaPiM1Hooks)
    }
}

impl A20Board for Board {
    type Payload = A20BuildSelectedPayload;

    const CONFIG: &'static fstart_platform_sunxi::a20::A20Config = &crate::BANANAPI_M1_A20;
    const CONSOLE_CONFIG: Ns16550Config = Ns16550Config {
        regs: AccessMode::Mmio {
            base: A20_UART0_BASE,
            reg_shift: 2,
            reg_width: 4,
        },
        clock_freq: 24_000_000,
        baud_rate: 115_200,
    };
}
