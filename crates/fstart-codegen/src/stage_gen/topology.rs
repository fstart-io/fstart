//! Topological sort for device initialization order.
//!
//! Ensures parent bus controllers are initialized before their child devices.
//! Uses Kahn's algorithm and validates parent references and bus services.

use fstart_types::DeviceConfig;

use super::registry::is_bus_provider;

/// Topological sort of devices: parents before children.
///
/// Returns the devices sorted so that any device with a `parent` field comes
/// after its parent. Also validates:
/// - Every `parent` reference names an existing device
/// - Every parent device provides a bus service
/// - No cycles in the parent chain
///
/// Returns either the sorted indices or an error message.
pub(super) fn topological_sort_devices(devices: &[DeviceConfig]) -> Result<Vec<usize>, String> {
    let n = devices.len();

    // Build name -> index map
    let name_to_idx: std::collections::HashMap<&str, usize> = devices
        .iter()
        .enumerate()
        .map(|(i, d)| (d.name.as_str(), i))
        .collect();

    // Build adjacency: parent_idx -> vec of child indices
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut in_degree: Vec<usize> = vec![0; n];

    for (i, dev) in devices.iter().enumerate() {
        if let Some(ref parent_name) = dev.parent {
            let parent_str = parent_name.as_str();

            // Validate parent exists
            let Some(&parent_idx) = name_to_idx.get(parent_str) else {
                return Err(format!(
                    "device '{}' has parent '{}' which is not declared",
                    dev.name.as_str(),
                    parent_str,
                ));
            };

            // Validate parent provides a bus service
            if !is_bus_provider(&devices[parent_idx]) {
                return Err(format!(
                    "device '{}' has parent '{}' which does not provide a bus service \
                     (expected one of: I2cBus, SpiBus, GpioController)",
                    dev.name.as_str(),
                    parent_str,
                ));
            };

            children[parent_idx].push(i);
            in_degree[i] += 1;
        }
    }

    // Kahn's algorithm for topological sort
    let mut queue: Vec<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut sorted: Vec<usize> = Vec::with_capacity(n);

    while let Some(node) = queue.pop() {
        sorted.push(node);
        for &child in &children[node] {
            in_degree[child] -= 1;
            if in_degree[child] == 0 {
                queue.push(child);
            }
        }
    }

    if sorted.len() != n {
        // Cycle detected — find the devices involved
        let cycle_devices: Vec<&str> = (0..n)
            .filter(|&i| in_degree[i] > 0)
            .map(|i| devices[i].name.as_str())
            .collect();
        return Err(format!(
            "cycle detected in device parent chain involving: {}",
            cycle_devices.join(", "),
        ));
    }

    Ok(sorted)
}
