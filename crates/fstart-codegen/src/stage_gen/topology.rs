//! Device tree validation for bus hierarchies.
//!
//! With nested `children` in RON, topological ordering is guaranteed by
//! the pre-order DFS flattening in [`ron_loader`](crate::ron_loader).
//! This module validates the structural constraints:
//!
//! - Every parent device provides a bus service (`I2cBus`, `SpiBus`,
//!   `GpioController`)
//!
//! Cycles and missing parents are impossible with nested RON syntax.

use fstart_types::{DeviceConfig, DeviceNode};

use super::registry::is_bus_provider;

/// Validate the flattened device tree.
///
/// Checks that every device with a parent is attached to a bus provider.
/// Returns `Ok(())` on success, or an error message for `compile_error!`.
///
/// Ordering is not computed here — the `device_tree` array is already in
/// topological (pre-order) order from the RON flattening.
pub(super) fn validate_device_tree(
    devices: &[DeviceConfig],
    tree: &[DeviceNode],
) -> Result<(), String> {
    for (i, node) in tree.iter().enumerate() {
        if let Some(parent_idx) = node.parent {
            let parent_idx = parent_idx as usize;
            let dev = &devices[i];
            let parent = &devices[parent_idx];

            if !is_bus_provider(parent) {
                return Err(format!(
                    "device '{}' has parent '{}' which does not provide a bus service \
                     (expected one of: I2cBus, SpiBus, GpioController)",
                    dev.name.as_str(),
                    parent.name.as_str(),
                ));
            }
        }
    }

    Ok(())
}
