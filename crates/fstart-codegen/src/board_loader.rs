//! Lower native Rust board metadata into build-time board facts.
//!
//! RON board descriptions and generated stage source have been removed. This
//! module keeps only the host-side normalization needed by linker generation,
//! xtask feature derivation, and future static-board build support.

use std::collections::HashMap;

use fstart_board_meta::{StructuralConfig, StructuralKind};
use fstart_device_registry::{DriverInstance, DriverInstanceBinding};
use fstart_services::ServiceSet;
use fstart_types::acpi::AcpiExtraDevice;
use fstart_types::{BoardConfig, DeviceId, DeviceNode, DeviceRole};

/// A fully-parsed board configuration.
///
/// Combines [`BoardConfig`] metadata with typed driver bindings. The parallel
/// arrays share indices: `device_tree[i]` describes `config.devices[i]` /
/// `driver_instances[i]` while the registry is being migrated away.
pub struct ParsedBoard {
    /// Board metadata (name, platform, memory, stages, security, etc.).
    pub config: BoardConfig,
    /// Typed driver configs, one per device, parallel to `config.devices`.
    pub driver_instances: Vec<DriverInstance>,
    /// Flat index-based device tree, parallel to `config.devices`.
    pub device_tree: Vec<DeviceNode>,
    /// Effective service set per device after applying board policy.
    pub device_services: Vec<ServiceSet>,
    /// ACPI-only descriptors collected separately from runtime devices.
    pub acpi_only_devices: Vec<AcpiExtraDevice>,
}

/// Load and validate a board from native Rust metadata.
pub fn load_parsed_board_from_rust(
    config: BoardConfig,
    driver_bindings: Vec<DriverInstanceBinding>,
) -> Result<ParsedBoard, String> {
    load_parsed_board_from_rust_with_acpi(config, driver_bindings, Vec::new())
}

/// Load and validate a board from native Rust metadata plus ACPI-only devices.
///
/// ACPI-only descriptors are side-table metadata for table generation. They are
/// not runtime devices and therefore do not participate in the flat runtime
/// topology or driver binding validation.
pub fn load_parsed_board_from_rust_with_acpi(
    mut config: BoardConfig,
    driver_bindings: Vec<DriverInstanceBinding>,
    acpi_only_devices: Vec<AcpiExtraDevice>,
) -> Result<ParsedBoard, String> {
    config
        .memory
        .normalize_derived_flash()
        .map_err(|err| err.to_string())?;

    let mut bindings_by_device = HashMap::with_capacity(driver_bindings.len());
    for binding in driver_bindings {
        let device = binding.device.to_string();
        if bindings_by_device
            .insert(device.clone(), binding.instance)
            .is_some()
        {
            return Err(format!(
                "board '{}' has duplicate driver binding for device '{}'",
                config.name, device
            ));
        }
    }

    let mut device_tree: Vec<DeviceNode> = Vec::with_capacity(config.devices.len());
    let mut driver_instances: Vec<DriverInstance> = Vec::with_capacity(config.devices.len());
    let mut device_services: Vec<ServiceSet> = Vec::with_capacity(config.devices.len());

    for (idx, device) in config.devices.iter().enumerate() {
        let parent_idx = match &device.parent {
            Some(parent_name) => Some(
                config
                    .devices
                    .iter()
                    .take(idx)
                    .position(|candidate| candidate.name == *parent_name)
                    .ok_or_else(|| {
                        format!(
                            "device '{}' references missing or later parent '{}'",
                            device.name, parent_name
                        )
                    })? as DeviceId,
            ),
            None => None,
        };
        let depth = parent_idx
            .map(|parent_idx| device_tree[parent_idx as usize].depth.saturating_add(1))
            .unwrap_or(0);
        device_tree.push(DeviceNode {
            parent: parent_idx,
            depth,
        });

        let instance = if device.role.is_runtime() {
            bindings_by_device
                .remove(device.name.as_str())
                .ok_or_else(|| {
                    format!(
                        "runtime device '{}' in board '{}' has no named driver binding",
                        device.name, config.name
                    )
                })?
        } else {
            if bindings_by_device.contains_key(device.name.as_str()) {
                return Err(format!(
                    "structural device '{}' in board '{}' must not have a runtime driver binding",
                    device.name, config.name
                ));
            }
            DriverInstance::Structural(StructuralConfig {
                kind: structural_kind_for_role(device.role)?,
            })
        };
        device_services.push(instance.provided_services());
        driver_instances.push(instance);
    }

    if !bindings_by_device.is_empty() {
        let mut names: Vec<_> = bindings_by_device.keys().cloned().collect();
        names.sort();
        return Err(format!(
            "board '{}' has driver bindings for unknown devices: {}",
            config.name,
            names.join(", ")
        ));
    }

    Ok(ParsedBoard {
        config,
        driver_instances,
        device_tree,
        device_services,
        acpi_only_devices,
    })
}

fn structural_kind_for_role(role: DeviceRole) -> Result<StructuralKind, String> {
    match role {
        DeviceRole::Runtime => Err("runtime device role is not structural".to_string()),
        DeviceRole::PciBridge => Ok(StructuralKind::PciBridge),
        DeviceRole::LpcBus => Ok(StructuralKind::LpcBus),
        DeviceRole::SmBus => Ok(StructuralKind::SmBus),
        DeviceRole::GenericBus => Ok(StructuralKind::GenericBus),
        DeviceRole::PnpDevice => Ok(StructuralKind::PnpDevice),
    }
}

#[cfg(test)]
mod tests {
    use super::load_parsed_board_from_rust;
    use fstart_device_registry::DriverInstance;

    #[test]
    fn structural_nodes_do_not_gain_pseudo_services() {
        let parsed = load_parsed_board_from_rust(
            fstart_board_foxconn_d41s::board_config(),
            fstart_board_foxconn_d41s::driver_bindings(),
        )
        .unwrap();

        let structural_idx = parsed
            .config
            .devices
            .iter()
            .position(|device| !device.role.is_runtime())
            .expect("board has structural nodes");
        assert!(matches!(
            parsed.driver_instances[structural_idx],
            DriverInstance::Structural(_)
        ));
        assert!(parsed.device_services[structural_idx].is_empty());
    }

    #[test]
    fn accepts_acpi_only_side_table() {
        let parsed = load_parsed_board_from_rust(
            fstart_board_qemu_riscv64::board_config(),
            fstart_board_qemu_riscv64::driver_bindings(),
        )
        .unwrap();
        assert!(parsed.acpi_only_devices.is_empty());
    }
}
