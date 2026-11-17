use super::*;
use crate::IntelNorthbridgeDriver;
use alloc::boxed::Box;
use fstart_core::services::SmBus;

struct Sensors {
    present: bool,
    writes: usize,
}
impl SmBus for Sensors {
    fn read_byte(&mut self, _: u8, _: u8) -> Result<u8, ServiceError> {
        Err(ServiceError::IoError)
    }
    fn write_byte(&mut self, _: u8, _: u8, _: u8) -> Result<(), ServiceError> {
        Err(ServiceError::IoError)
    }
    fn read_word(&mut self, _: u8, _: u8) -> Result<u16, ServiceError> {
        if self.present {
            Ok(0)
        } else {
            Err(ServiceError::IoError)
        }
    }
    fn write_word(&mut self, _: u8, _: u8, _: u16) -> Result<(), ServiceError> {
        self.writes += 1;
        Ok(())
    }
}

#[test]
fn thermal_reporting_requires_a_responding_dimm_sensor() {
    let mut mmio = [0u32; 0x1000];
    let config = Box::leak(Box::new(IntelGm965Config {
        mchbar: mmio.as_mut_ptr() as u64,
        ..IntelGm965Config::new()
    }));
    let driver = IntelGm965::new_from_config(config).unwrap();
    let mut info = raminit::RaminitInfo::default();
    info.dimms[0].present = true;
    for present in [true, false] {
        let mut bus = Sensors { present, writes: 0 };
        driver.thermal_sensor_init(&info, &mut bus);
        assert_eq!(
            driver.mchbar().read8(mchbar::THERMAL_ENABLE),
            if present { 0xd0 } else { 0 }
        );
        assert_eq!(bus.writes, if present { 4 } else { 0 });
    }
}
