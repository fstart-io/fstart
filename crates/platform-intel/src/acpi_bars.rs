//! Persist chipset topology and addresses used to build the cold DSDT.
//! On resume compare against, but never trust or rewrite, the saved values.

use fstart_core::services::ServiceError;
use fstart_store::{Store, tag::ACPI_BARS};

pub(crate) fn cold_snapshot(store: &mut Store) -> &[u8] {
    let Some(entry) = store.find(ACPI_BARS) else {
        return &[];
    };
    // SAFETY: validated store entry, exclusively accessed on the BSP.
    unsafe { store.bytes_mut(&entry) }
}

/// False means the cold boot can continue, but must not advertise S3.
pub(crate) fn retain(store: &mut Store, bars: &[u8], resume: bool) -> Result<bool, ServiceError> {
    if resume {
        if cold_snapshot(store) != bars {
            return Err(ServiceError::HardwareError);
        }
    } else if !bars.is_empty() {
        let Ok(entry) = store.add(ACPI_BARS, bars.len(), 0) else {
            return Ok(false);
        };
        // SAFETY: fresh entry in the cold store.
        unsafe { store.bytes_mut(&entry) }.copy_from_slice(bars);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_optional_root_port_in_either_direction_rejects_resume() {
        use fstart_driver_intel::{
            IntelSouthbridgeDriver,
            lynxpoint::{
                Gpios, LynxPoint, LynxPointConfig,
                gpio::{Mode, Pad},
            },
        };
        static CONFIG: LynxPointConfig = LynxPointConfig {
            pcie_ports: 3,
            ..LynxPointConfig::new(Gpios::Lp(&[Pad::new(Mode::Unused); 95]))
        };
        let chipset = |rp2: bool| {
            let mut sb = LynxPoint::new_from_config(&CONFIG).unwrap();
            if !rp2 {
                sb.disable_root_ports(2);
            }
            sb
        };
        for cold in [true, false] {
            #[repr(align(64))]
            struct Window([u8; 512]);
            let mut window = Window([0; 512]);
            let mut store = unsafe {
                Store::create(core::ptr::NonNull::from(&mut window.0).cast(), 512).unwrap()
            };
            let mut same = chipset(cold);
            let mut changed = chipset(!cold);
            assert!(!same.matches_acpi_population(cold_snapshot(&mut store)));
            let snapshot = same.acpi_bar_snapshot().unwrap();
            retain(&mut store, &snapshot, false).unwrap();
            // Pre-enumeration check and the full post-allocation check agree.
            assert!(!changed.matches_acpi_population(cold_snapshot(&mut store)));
            assert!(same.matches_acpi_population(cold_snapshot(&mut store)));
            assert!(retain(&mut store, &changed.acpi_bar_snapshot().unwrap(), true).is_err());
            retain(&mut store, &snapshot, true).unwrap();
        }
    }

    #[test]
    fn resume_compares_and_never_rewrites_cold_acpi_bars() {
        #[repr(align(64))]
        struct Window([u8; 512]);
        let mut window = Window([0; 512]);
        let mut store =
            unsafe { Store::create(core::ptr::NonNull::from(&mut window.0).cast(), 512).unwrap() };
        assert_eq!(retain(&mut store, &[1; 512], false), Ok(false));
        assert!(retain(&mut store, &[1, 2], true).is_err());
        retain(&mut store, &[1, 2], false).unwrap();
        let used = store.used();
        for mismatch in [&[1, 3][..], &[1], &[]] {
            assert!(retain(&mut store, mismatch, true).is_err());
        }
        retain(&mut store, &[1, 2], true).unwrap();
        assert_eq!(store.used(), used);
    }
}
