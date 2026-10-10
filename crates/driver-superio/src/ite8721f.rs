//! ITE IT8721F SuperIO driver descriptor.
//!
//! Foxconn D41S exposes the IT8721F at LPC PnP config port `0x2e`.

use crate::{IoResource, IrqResource, SuperIo, SuperIoChip};

pub use crate::{
    CirConfig, ComPortConfig, EcConfig, GpioConfig, KbcConfig, LpcBaseProvider, MouseConfig,
    ParallelConfig, SuperIoConfig,
};

/// Zero-sized chip descriptor for the ITE IT8721F.
pub struct Ite8721fChip;

impl SuperIoChip for Ite8721fChip {
    const ENTER_SEQ: &'static [u8] = &[0x87, 0x01, 0x55];
    const EXIT_REG: u8 = 0x02;
    const EXIT_VAL: u8 = 0x02;
    const CHIP_ID: u16 = 0x8721;
    const COM1_LDN: Option<u8> = Some(0x01);
    const COM2_LDN: Option<u8> = Some(0x02);
    const KBC_LDN: Option<u8> = Some(0x05);
    const MOUSE_LDN: Option<u8> = Some(0x06);
    const EC_LDN: Option<u8> = Some(0x04);
    const GPIO_LDN: Option<u8> = Some(0x07);
    const CIR_LDN: Option<u8> = Some(0x0a);
    const PARALLEL_LDN: Option<u8> = Some(0x03);

    fn enter_last_byte(base_port: u16) -> Option<u8> {
        Some(if base_port == 0x4e { 0xaa } else { 0x55 })
    }
}

/// IT8721F SuperIO driver.
pub type Ite8721f = SuperIo<Ite8721fChip>;

/// IT8721F uses the shared named-device resource configuration directly.
pub type Ite8721fConfig = SuperIoConfig;

impl Ite8721f {
    /// Leave monitoring polled rather than routing it onto an ISA IRQ.
    pub fn disconnect_hwmon_irq(&mut self) {
        self.enter_config();
        self.logical_device(4).set_irq(IrqResource::Primary(0));
        self.exit_config();
    }

    /// Disable the parallel port's secondary ECP address window.
    pub fn disable_parallel_ecp_decode(&mut self) {
        self.enter_config();
        self.logical_device(3).set_io_base(IoResource::Secondary(0));
        self.exit_config();
    }

    pub fn disable_floppy(&mut self) {
        self.enter_config();
        self.logical_device(0).set_enabled(false);
        self.exit_config();
    }
}
