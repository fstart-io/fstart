//! IT8718F resource and pin policy (ITE IT8718F V0.3 §§8.3–8.11).
//! GPIO has no documented CR30 activation bit. Undocumented EF writes and
//! reserved bits from vendor whole-register scripts are deliberately omitted.
use crate::{DmaResource, IoResource, IrqResource, SuperIo, SuperIoChip};
use fstart_core::services::device::DeviceError;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

pub use crate::ite_gpio::{GpioDirection, GpioFunction, PullUp};
pub type GpioPinConfig = crate::ite_gpio::GpioPinConfig<Ite8718fChip>;

register_bitfields![u8,
    CHIP_VERSION [ VERSION OFFSET(0) NUMBITS(4) [C = 1] ],
    EXT_MUX [
        VIN6_GPIO OFFSET(7) NUMBITS(1) [],
        FAN_TAC5 OFFSET(4) NUMBITS(1) [], FAN_TAC4 OFFSET(3) NUMBITS(1) [],
        PCIRSTIN_VCCH_DIVIDER OFFSET(1) NUMBITS(1) [], ATXPG_VCC_DIVIDER OFFSET(0) NUMBITS(1) [],
    ],
    WATCHDOG [
        SECONDS OFFSET(7) NUMBITS(1) [], KEYBOARD_RESET OFFSET(6) NUMBITS(1) [],
        SHORT_PERIOD OFFSET(5) NUMBITS(1) [], POWER_OK_RESET OFFSET(4) NUMBITS(1) [],
        IRQ OFFSET(0) NUMBITS(4) [],
    ],
    PIN_MAPPING [ PIN OFFSET(0) NUMBITS(6) [] ],
    VID_INPUT [ CAPTURE OFFSET(0) NUMBITS(1) [] ],
    PARALLEL [
        POST_DISABLE OFFSET(3) NUMBITS(1) [], IRQ_SHARING OFFSET(2) NUMBITS(1) [],
        MODE OFFSET(0) NUMBITS(2) [Spp = 0, Epp = 1, Ecp = 2, EppEcp = 3],
    ],
    GPIO5_MUX [ KEYBOARD_GPIO OFFSET(6) NUMBITS(1) [] ],
    APC_EVENTS [
        VCCH_OFF_STATUS OFFSET(7) NUMBITS(1) [],
        MOUSE OFFSET(4) NUMBITS(1) [], KEYBOARD OFFSET(3) NUMBITS(1) [],
        RI2 OFFSET(2) NUMBITS(1) [], RI1 OFFSET(1) NUMBITS(1) [], CIR OFFSET(0) NUMBITS(1) [],
    ],
    APC_CONTROL2 [ AUTO_SWAP_DISABLE OFFSET(7) NUMBITS(1) [] ],
    KEYBOARD [ IRQ_SHARING OFFSET(4) NUMBITS(1) [], CLOCK_8MHZ OFFSET(3) NUMBITS(1) [], KEYLOCK OFFSET(2) NUMBITS(1) [], DYNAMIC_IRQ OFFSET(1) NUMBITS(1) [] ],
    MOUSE [ IRQ_SHARING OFFSET(1) NUMBITS(1) [], DYNAMIC_IRQ OFFSET(0) NUMBITS(1) [] ],
];

#[derive(Debug, Clone, Copy)]
pub struct Ite8718fChip;
impl SuperIoChip for Ite8718fChip {
    const ENTER_SEQ: &'static [u8] = &[0x87, 0x01, 0x55];
    const EXIT_REG: u8 = 2;
    const EXIT_VAL: u8 = 2;
    const CHIP_ID: u16 = 0x8718;
    const COM1_LDN: Option<u8> = Some(1);
    const COM2_LDN: Option<u8> = Some(2);
    const EC_LDN: Option<u8> = Some(4);
    const KBC_LDN: Option<u8> = Some(5);
    const MOUSE_LDN: Option<u8> = Some(6);
    const GPIO_LDN: Option<u8> = None;
    const CIR_LDN: Option<u8> = Some(10);
    const PARALLEL_LDN: Option<u8> = Some(3);
    fn enter_last_byte(base_port: u16) -> Option<u8> {
        Some(if base_port == 0x4e { 0xaa } else { 0x55 })
    }
}
impl crate::ite_gpio::IteGpioChip for Ite8718fChip {
    const MAX_GPIO: u8 = 67;
    const HAS_GP15: bool = false;
    const EXTENDED_GPIO1: bool = true;
    // §5, Table 5-12 overrides the generic pull-up register description.
    const PULL_UP_EXCLUSIONS: &'static [u8] = &[56, 57, 60, 61];
}
pub type Ite8718f = SuperIo<Ite8718fChip>;

fn beep_fields(pin: Option<u8>) -> tock_registers::fields::FieldValue<u8, PIN_MAPPING::Register> {
    PIN_MAPPING::PIN.val(pin.map_or(0, |pin| {
        let _ = GpioPinConfig::new(pin, GpioFunction::Alternate);
        pin / 10 * 8 + pin % 10
    }))
}

impl Ite8718f {
    /// Verify the documented C-version code before using the V0.3 EC profile.
    /// Other version codes need verification; do not infer their ordering.
    pub fn verify_environment_revision(&mut self) -> Result<(), DeviceError> {
        self.enter_config();
        let supported = self
            .register::<CHIP_VERSION::Register>(0x22)
            .matches_all(CHIP_VERSION::VERSION::C);
        self.exit_config();
        if supported {
            Ok(())
        } else {
            Err(DeviceError::InitFailed)
        }
    }

    /// Select external VIN3/VIN7 rather than the internal 5 V dividers.
    /// This also disables the coupled ATXPG/PCIRSTIN pad functions (§8.3.15).
    /// Preserve tachometer selection, strap/reserved bits and VIN6 routing.
    pub fn use_external_voltage_inputs(&mut self) {
        self.enter_config();
        self.logical_device(7);
        self.register::<EXT_MUX::Register>(0x2c)
            .modify(EXT_MUX::PCIRSTIN_VCCH_DIVIDER::CLEAR + EXT_MUX::ATXPG_VCC_DIVIDER::CLEAR);
        self.exit_config();
    }

    /// Select the monitor's VIN6 pad rather than its GPIO function.
    pub fn use_voltage_input6(&mut self) {
        self.enter_config();
        self.logical_device(7);
        self.register::<EXT_MUX::Register>(0x2c)
            .modify(EXT_MUX::VIN6_GPIO::CLEAR);
        self.exit_config();
    }

    /// Route the VIDO2/VIDO3 pads to auxiliary tachometer inputs 5/4.
    /// Counter enables remain the environmental controller's policy.
    pub fn use_auxiliary_fan_tachometers(&mut self) {
        self.enter_config();
        self.logical_device(7);
        self.register::<EXT_MUX::Register>(0x2c)
            .modify(EXT_MUX::FAN_TAC4::SET + EXT_MUX::FAN_TAC5::SET);
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

    /// Route monitor alert output to a physical GPxx pin, or disconnect it.
    /// The caller must also select that pin's GPIO alternate function.
    pub fn map_monitor_beep(&mut self, pin: Option<u8>) {
        let value = beep_fields(pin);
        self.enter_config();
        self.logical_device(7);
        self.register::<PIN_MAPPING::Register>(0xf6).modify(value);
        self.exit_config();
    }

    /// Reload the read-only initial VID sample; this does not program voltage.
    pub fn capture_initial_vid(&mut self) {
        self.enter_config();
        self.logical_device(7);
        self.register::<VID_INPUT::Register>(0xfc)
            .write(VID_INPUT::CAPTURE::SET);
        self.exit_config();
    }

    pub fn disable_floppy(&mut self) {
        self.enter_config();
        self.logical_device(0).set_enabled(false);
        self.exit_config();
    }

    /// Standard LPT, no ECP window or DMA, and no POST-port interception.
    pub fn configure_parallel_spp(&mut self) {
        self.enter_config();
        let mut parallel = self.logical_device(3);
        parallel.set_io_base(IoResource::Secondary(0));
        parallel.set_dma(DmaResource::Primary(4));
        self.register::<PARALLEL::Register>(0xf0).modify(
            PARALLEL::POST_DISABLE::SET + PARALLEL::IRQ_SHARING::CLEAR + PARALLEL::MODE::Spp,
        );
        self.exit_config();
    }

    /// Disable APC wake sources without clearing the W1C power-loss status.
    pub fn disable_apc_wake_events(&mut self) {
        self.enter_config();
        self.logical_device(4);
        self.register::<APC_EVENTS::Register>(0xf0).modify(
            APC_EVENTS::VCCH_OFF_STATUS::CLEAR
                + APC_EVENTS::MOUSE::CLEAR
                + APC_EVENTS::KEYBOARD::CLEAR
                + APC_EVENTS::RI2::CLEAR
                + APC_EVENTS::RI1::CLEAR
                + APC_EVENTS::CIR::CLEAR,
        );
        self.exit_config();
    }

    /// Native PS/2 pads, no auto-swap, 8 MHz keyboard clock and fixed IRQ types.
    pub fn configure_keyboard_native(&mut self) {
        self.enter_config();
        self.logical_device(7);
        self.register::<GPIO5_MUX::Register>(0x29)
            .modify(GPIO5_MUX::KEYBOARD_GPIO::CLEAR);
        self.logical_device(4);
        self.register::<APC_CONTROL2::Register>(0xf4)
            .modify(APC_CONTROL2::AUTO_SWAP_DISABLE::SET);
        self.logical_device(5);
        self.register::<KEYBOARD::Register>(0xf0).modify(
            KEYBOARD::IRQ_SHARING::CLEAR
                + KEYBOARD::CLOCK_8MHZ::SET
                + KEYBOARD::KEYLOCK::CLEAR
                + KEYBOARD::DYNAMIC_IRQ::CLEAR,
        );
        self.logical_device(6);
        self.register::<MOUSE::Register>(0xf0)
            .modify(MOUSE::IRQ_SHARING::CLEAR + MOUSE::DYNAMIC_IRQ::CLEAR);
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
    fn descriptor_and_pin_mapping_match_the_chip() {
        assert_eq!(Ite8718fChip::CHIP_ID, 0x8718);
        assert!(CHIP_VERSION::VERSION::C.matches_all(0x81));
        assert!(!CHIP_VERSION::VERSION::C.matches_all(0x80));
        assert!(!CHIP_VERSION::VERSION::C.matches_all(0x82));
        assert_eq!(Ite8718fChip::GPIO_LDN, None);
        assert_eq!(Ite8718fChip::enter_last_byte(0x2e), Some(0x55));
        assert_eq!(Ite8718fChip::enter_last_byte(0x4e), Some(0xaa));
        assert_eq!(beep_fields(Some(46)).value, 0x26);
        assert_eq!(beep_fields(None).modify(0xc0), 0xc0);
        assert_eq!(beep_fields(Some(46)).modify(0xc0), 0xe6);
        assert_eq!(
            (EXT_MUX::PCIRSTIN_VCCH_DIVIDER::CLEAR + EXT_MUX::ATXPG_VCC_DIVIDER::CLEAR)
                .modify(0xff),
            0xfc
        );
        assert_eq!(VID_INPUT::CAPTURE::SET.value, 1);
        let disabled_wake = APC_EVENTS::VCCH_OFF_STATUS::CLEAR
            + APC_EVENTS::MOUSE::CLEAR
            + APC_EVENTS::KEYBOARD::CLEAR
            + APC_EVENTS::RI2::CLEAR
            + APC_EVENTS::RI1::CLEAR
            + APC_EVENTS::CIR::CLEAR;
        // Write zero to W1C status, preserving reserved bits 6:5.
        assert_eq!(disabled_wake.modify(0xff), 0x60);
    }
}
