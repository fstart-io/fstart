//! Lower native Rust board metadata into build-time board facts.
//!
//! RON board descriptions and generated stage source have been removed. This
//! module keeps only the host-side normalization needed by linker generation,
//! xtask feature derivation, and future static-board build support.

use std::collections::HashMap;

use fstart_board_meta::{DriverBinding, DriverFact, StructuralKind};
use fstart_services::ServiceSet;
use fstart_types::acpi::AcpiExtraDevice;
use fstart_types::{BoardConfig, DeviceId, DeviceNode, DeviceRole};

/// A fully-parsed board configuration.
///
/// Combines [`BoardConfig`] metadata with board-owned driver bindings.
pub struct ParsedBoard {
    /// Board metadata (name, platform, memory, stages, security, etc.).
    pub config: BoardConfig,
    /// Runtime driver facts supplied by the board/platform crate.
    pub driver_facts: Vec<DriverFact>,
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
    driver_bindings: Vec<DriverBinding>,
) -> Result<ParsedBoard, String> {
    load_parsed_board_from_rust_with_acpi(config, driver_bindings, Vec::new())
}

/// Load and validate a board from native Rust metadata plus ACPI-only devices.
///
/// ACPI-only descriptors are side-table metadata for table generation. They are
/// not runtime devices and therefore do not participate in the flat runtime
/// topology or driver binding validation.
pub fn load_parsed_board_from_rust_with_acpi(
    config: BoardConfig,
    driver_bindings: Vec<DriverBinding>,
    acpi_only_devices: Vec<AcpiExtraDevice>,
) -> Result<ParsedBoard, String> {
    let mut config = config;
    config
        .memory
        .normalize_derived_flash()
        .map_err(|err| err.to_string())?;

    let driver_count = driver_bindings.len();
    let mut facts_by_device = HashMap::with_capacity(driver_count);
    for binding in driver_bindings {
        let fact = DriverFact::from_binding(&binding);
        let device = fact.device.to_string();
        if facts_by_device.insert(device.clone(), fact).is_some() {
            return Err(format!(
                "board '{}' has duplicate driver binding for device '{}'",
                config.name, device
            ));
        }
    }

    let mut device_tree: Vec<DeviceNode> = Vec::with_capacity(config.devices.len());
    let mut runtime_driver_facts: Vec<DriverFact> = Vec::with_capacity(driver_count);
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

        if device.role.is_runtime() {
            let fact = facts_by_device
                .remove(device.name.as_str())
                .ok_or_else(|| {
                    format!(
                        "runtime device '{}' in board '{}' has no named driver binding",
                        device.name, config.name
                    )
                })?;
            device_services.push(fact.services);
            runtime_driver_facts.push(fact);
        } else {
            if facts_by_device.contains_key(device.name.as_str()) {
                return Err(format!(
                    "structural device '{}' in board '{}' must not have a runtime driver binding",
                    device.name, config.name
                ));
            }
            let _kind = structural_kind_for_role(device.role)?;
            device_services.push(ServiceSet::empty());
        };
    }

    if !facts_by_device.is_empty() {
        let mut names: Vec<_> = facts_by_device.keys().cloned().collect();
        names.sort();
        return Err(format!(
            "board '{}' has driver bindings for unknown devices: {}",
            config.name,
            names.join(", ")
        ));
    }

    Ok(ParsedBoard {
        config,
        driver_facts: runtime_driver_facts,
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

    #[test]
    fn structural_nodes_do_not_gain_pseudo_services() {
        let mut config = fstart_board_qemu_riscv64::board_config();
        config
            .devices
            .push(fstart_types::DeviceConfig {
                name: fstart_types::hstr("topology-only-bus"),
                parent: None,
                bus: None,
                role: fstart_types::DeviceRole::GenericBus,
                enabled: true,
            })
            .unwrap();

        let parsed =
            load_parsed_board_from_rust(config, fstart_board_qemu_riscv64::driver_bindings())
                .unwrap();

        let structural_idx = parsed
            .config
            .devices
            .iter()
            .position(|device| device.name.as_str() == "topology-only-bus")
            .expect("synthetic structural node is present");
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
