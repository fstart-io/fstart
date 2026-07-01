//! Lower native Rust board metadata into build-time board facts.
//!
//! This module keeps only the host-side normalization needed by linker setup,
//! xtask feature derivation, and static-board build support.

use fstart_services::ServiceSet;
use fstart_types::acpi::AcpiExtraDevice;
use fstart_types::{BoardConfig, DeviceId, DeviceNode};

/// A fully-parsed board configuration.
///
/// Combines [`BoardConfig`] metadata with normalized topology facts.
pub struct ParsedBoard {
    /// Board metadata (name, platform, memory, stages, security, etc.).
    pub config: BoardConfig,
    /// Flat index-based device tree, parallel to `config.devices`.
    pub device_tree: Vec<DeviceNode>,
    /// Effective service set per device. Metadata-only board loading leaves these empty.
    pub device_services: Vec<ServiceSet>,
    /// ACPI-only descriptors collected separately from runtime devices.
    pub acpi_only_devices: Vec<AcpiExtraDevice>,
}

/// Load and validate static board metadata.
///
/// Runtime driver binding is intentionally not part of build metadata. The
/// board-owned stage package selects and configures concrete runtime devices.
pub fn load_parsed_board_metadata_only(
    config: BoardConfig,
    acpi_only_devices: Vec<AcpiExtraDevice>,
) -> Result<ParsedBoard, String> {
    let mut config = config;
    config
        .memory
        .normalize_derived_flash()
        .map_err(|err| err.to_string())?;

    let device_tree = build_device_tree(&config)?;
    let device_services = vec![ServiceSet::empty(); config.devices.len()];

    Ok(ParsedBoard {
        config,
        device_tree,
        device_services,
        acpi_only_devices,
    })
}

fn build_device_tree(config: &BoardConfig) -> Result<Vec<DeviceNode>, String> {
    let mut device_tree: Vec<DeviceNode> = Vec::with_capacity(config.devices.len());

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
    }

    Ok(device_tree)
}

#[cfg(test)]
mod tests {
    use super::load_parsed_board_metadata_only;

    #[test]
    fn structural_nodes_do_not_gain_pseudo_services() {
        let mut config = fstart_board_lenovo_x61::board_config();
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

        let parsed = load_parsed_board_metadata_only(config, Vec::new()).unwrap();

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
        let parsed =
            load_parsed_board_metadata_only(fstart_board_lenovo_x61::board_config(), Vec::new())
                .unwrap();
        assert!(parsed.acpi_only_devices.is_empty());
    }
}
