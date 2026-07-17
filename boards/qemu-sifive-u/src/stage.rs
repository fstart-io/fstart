//! QEMU SiFive U binding for the direct DRAM-resident flow.

use fstart_driver_uart::sifive::SifiveUartConfig;
use fstart_platform_qemu::{
    QemuSifiveU, QemuSifiveUBoard, QemuSifiveUBuildSelectedPayload, QemuSifiveUConfig,
    QemuSifiveUHooks,
};
use fstart_stage::{StageBoard, StageEnvironment};

use crate::Board;

impl StageBoard for Board {
    const NAME: &'static str = crate::BOARD_NAME;
    const PLATFORM: fstart_core::Platform = crate::PLATFORM;

    fn run_stage(env: StageEnvironment, handoff: usize) -> ! {
        QemuSifiveU::run_stage::<Self>(env, handoff)
    }

    #[cfg(feature = "crabefi")]
    fn resume_sbi(hart_id: u64, dtb_addr: u64) -> ! {
        QemuSifiveU::resume_sbi::<Self>(hart_id, dtb_addr)
    }
}

/// Board-specific seams for QEMU's fixed SiFive U flow.
#[derive(Default)]
pub struct QemuSifiveUBoardHooks;

impl QemuSifiveUHooks for QemuSifiveUBoardHooks {}

impl QemuSifiveUBoard for Board {
    type Hooks = QemuSifiveUBoardHooks;
    type Payload = QemuSifiveUBuildSelectedPayload;

    const CONFIG: &'static QemuSifiveUConfig = &crate::QEMU_SIFIVE_U;
    const CONSOLE_CONFIG: &'static SifiveUartConfig = &crate::QEMU_SIFIVE_U_UART;
}
