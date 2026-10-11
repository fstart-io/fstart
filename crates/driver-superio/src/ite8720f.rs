//! IT8720F GPIO mux, simple I/O, power-good and flash-bank policy.
//!
//! GPIO register meanings follow ITE IT8720F V0.1 sections 8.3 and 8.11.
//! Flash-bank select bit 0 at LDN7/EF follows flashrom's IT87 DualBIOS support.
//! It is deliberately a read/modify/write: the older datasheet does not define
//! EF's other bits. FC is read-only VID input (except its reload strobe); writing
//! a VID value there, as the GA-D510UD reference does, cannot program a voltage.
use crate::{IoResource, IrqResource, SuperIo, SuperIoChip};
use tock_registers::{
    interfaces::{ReadWriteable, Writeable},
    register_bitfields,
};

pub use crate::ite_gpio::{GpioDirection, GpioFunction, PullUp};
pub type GpioPinConfig = crate::ite_gpio::GpioPinConfig<Ite8720fChip>;

register_bitfields![u8,
    EXT_MUX [
        SMBUS_ISOLATION_DISABLE OFFSET(7) NUMBITS(1) [],
        K8_SOFTWARE_POWER_DISABLE OFFSET(6) NUMBITS(1) [],
        VID_TURBO OFFSET(5) NUMBITS(1) [],
        FAN_TAC5 OFFSET(4) NUMBITS(1) [], FAN_TAC4 OFFSET(3) NUMBITS(1) [],
        PCIRSTIN OFFSET(2) NUMBITS(1) [], VCCH5V_DIVIDER OFFSET(1) NUMBITS(1) [],
        ATXPG_DIVIDER OFFSET(0) NUMBITS(1) [],
    ],
    BUS_SELECT [
        RESET OFFSET(6) NUMBITS(2) [Rsmrst = 0, Lreset = 1, PowerOk = 2],
        DISABLE OFFSET(5) NUMBITS(1) [], OUTPUT_ENABLE OFFSET(4) NUMBITS(1) [],
        READ_OUTPUT OFFSET(3) NUMBITS(1) [],
        BSEL0 OFFSET(0) NUMBITS(1) [], BSEL1 OFFSET(1) NUMBITS(1) [], BSEL2 OFFSET(2) NUMBITS(1) [],
    ],
    WATCHDOG [
        SECONDS OFFSET(7) NUMBITS(1) [], KEYBOARD_RESET OFFSET(6) NUMBITS(1) [],
        SHORT_PERIOD OFFSET(5) NUMBITS(1) [], POWER_OK_RESET OFFSET(4) NUMBITS(1) [],
        IRQ OFFSET(0) NUMBITS(4) [],
    ],
    FLASH_SELECT [ BANK OFFSET(0) NUMBITS(1) [Main = 0, Backup = 1] ],
];

#[derive(Debug, Clone, Copy)]
pub struct Ite8720fChip;
impl SuperIoChip for Ite8720fChip {
    const ENTER_SEQ: &'static [u8] = &[0x87, 0x01, 0x55];
    const EXIT_REG: u8 = 2;
    const EXIT_VAL: u8 = 2;
    const CHIP_ID: u16 = 0x8720;
    const COM1_LDN: Option<u8> = Some(1);
    const COM2_LDN: Option<u8> = Some(2);
    const EC_LDN: Option<u8> = Some(4);
    const KBC_LDN: Option<u8> = Some(5);
    const MOUSE_LDN: Option<u8> = Some(6);
    const GPIO_LDN: Option<u8> = Some(7);
    const CIR_LDN: Option<u8> = Some(10);
    const PARALLEL_LDN: Option<u8> = Some(3);
    fn enter_last_byte(base_port: u16) -> Option<u8> {
        Some(if base_port == 0x4e { 0xaa } else { 0x55 })
    }
}
impl crate::ite_gpio::IteGpioChip for Ite8720fChip {
    const MAX_GPIO: u8 = 47;
    const HAS_GP15: bool = true;
    const EXTENDED_GPIO1: bool = false;
}
pub type Ite8720f = SuperIo<Ite8720fChip>;

/// Levels to preload into the BSEL latches, without enabling their outputs.
#[derive(Debug, Clone, Copy)]
pub struct BusSelect {
    pub bsel0_high: bool,
    pub bsel1_high: bool,
    pub bsel2_high: bool,
}
impl BusSelect {
    fn fields(self) -> tock_registers::fields::FieldValue<u8, BUS_SELECT::Register> {
        BUS_SELECT::BSEL0.val(self.bsel0_high as u8)
            + BUS_SELECT::BSEL1.val(self.bsel1_high as u8)
            + BUS_SELECT::BSEL2.val(self.bsel2_high as u8)
    }
}
impl Ite8720f {
    /// Select the internal ATX power-good divider rather than the VIN3 input.
    /// Other extended pin functions remain at their reset configuration.
    pub fn use_internal_power_good(&mut self) {
        self.enter_config();
        self.logical_device(7);
        self.register::<EXT_MUX::Register>(0x2c)
            .write(EXT_MUX::ATXPG_DIVIDER::SET);
        self.exit_config();
    }
    /// Preload BSEL without driving the pins; retain pass-through and RSMRST# reset.
    pub fn preset_bus_select(&mut self, levels: BusSelect) {
        self.enter_config();
        self.logical_device(7);
        self.register::<BUS_SELECT::Register>(0xe9).write(
            BUS_SELECT::RESET::Rsmrst
                + BUS_SELECT::DISABLE::CLEAR
                + BUS_SELECT::OUTPUT_ENABLE::CLEAR
                + BUS_SELECT::READ_OUTPUT::CLEAR
                + levels.fields(),
        );
        self.exit_config();
    }
    pub fn disable_watchdog(&mut self) {
        self.enter_config();
        self.logical_device(7);
        self.register::<WATCHDOG::Register>(0x72).write(
            WATCHDOG::SECONDS::CLEAR
                + WATCHDOG::KEYBOARD_RESET::CLEAR
                + WATCHDOG::SHORT_PERIOD::CLEAR
                + WATCHDOG::POWER_OK_RESET::CLEAR
                + WATCHDOG::IRQ.val(0),
        );
        self.write_reg(0x73, 0);
        self.write_reg(0x74, 0);
        self.exit_config();
    }
    /// Select main DualBIOS flash without changing undocumented pin bits.
    pub fn select_main_flash(&mut self) {
        self.enter_config();
        self.logical_device(7);
        self.register::<FLASH_SELECT::Register>(0xef)
            .modify(FLASH_SELECT::BANK::Main);
        self.exit_config();
    }
    pub fn disable_parallel_ecp_decode(&mut self) {
        self.enter_config();
        self.logical_device(3).set_io_base(IoResource::Secondary(0));
        self.exit_config();
    }
    pub fn disconnect_hwmon_irq(&mut self) {
        self.enter_config();
        self.logical_device(4).set_irq(IrqResource::Primary(0));
        self.exit_config();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bsel_lines_encode_independently() {
        for (levels, expected) in [
            (
                BusSelect {
                    bsel0_high: true,
                    bsel1_high: false,
                    bsel2_high: false,
                },
                1,
            ),
            (
                BusSelect {
                    bsel0_high: false,
                    bsel1_high: true,
                    bsel2_high: false,
                },
                2,
            ),
            (
                BusSelect {
                    bsel0_high: false,
                    bsel1_high: false,
                    bsel2_high: true,
                },
                4,
            ),
        ] {
            assert_eq!(levels.fields().value, expected);
        }
    }
}
