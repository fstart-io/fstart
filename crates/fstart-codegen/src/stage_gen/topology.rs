//! Device tree validation for bus hierarchies.
//!
//! With nested `children` in RON, topological ordering is guaranteed by
//! the pre-order DFS flattening in [`ron_loader`](crate::ron_loader).
//! This module validates the structural constraints:
//!
//! - A **bus-device child** (driver impls `BusDevice`, constructed via
//!   `new_on_bus`) must have a parent providing a matching bus service
//!   (`I2cBus`, `SpiBus`, `PciRootBus`, `LpcBus`, `SmBus`, `PciBridge`,
//!   `PciHost`, `Southbridge`, `GpioController`).
//!
//! - A **plain-Device child** (e.g., an NS16550 UART nested under a
//!   SuperIO host for init ordering) has no such requirement — the
//!   parent link is purely ordering metadata for `ensure_device_ready`.
//!
//! Cycles and missing parents are impossible with nested RON syntax.

use fstart_device_registry::DriverInstance;
use fstart_types::{DeviceConfig, DeviceNode};

use super::registry::is_bus_provider;

/// Validate the flattened device tree.
///
/// Returns `Ok(())` on success, or an error message for `compile_error!`.
/// Ordering is not computed here — the `device_tree` array is already in
/// topological (pre-order) order from the RON flattening.
pub(super) fn validate_device_tree(
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    tree: &[DeviceNode],
) -> Result<(), String> {
    for (i, node) in tree.iter().enumerate() {
        let Some(parent_idx) = node.parent else {
            continue;
        };
        let parent_idx = parent_idx as usize;
        let dev = &devices[i];
        let parent = &devices[parent_idx];
        let inst = &instances[i];

        // Plain-Device children attach to their parent for init-ordering
        // only — no bus-service requirement. Structural nodes carry no
        // runtime state either.
        if !inst.meta().is_bus_device || inst.is_structural() {
            continue;
        }

        if !is_bus_provider(parent) {
            return Err(format!(
                "bus-device '{}' has parent '{}' which does not provide a bus service \
                 (expected one of: I2cBus, SpiBus, GpioController, PciRootBus, \
                 PciHost, Southbridge, PciBridge, LpcBus, SmBus)",
                dev.name.as_str(),
                parent.name.as_str(),
            ));
        }
    }

    Ok(())
}
