//! Semantic context for board adapter emission.
//!
//! Keep this module focused on facts computed before token emission: stage
//! scope, runtime-device classification, and typed service queries.

use fstart_device_registry::{ConstructionKind, DriverInstance, Service, ServiceSet};
use fstart_types::{
    acpi::AcpiExtraDevice, BoardConfig, Capability, DeviceConfig, DeviceNode, SocImageFormat,
    StageLayout,
};

use crate::stage_gen::capabilities::find_dram_region;
use crate::stage_gen::validation::needs_ffs;

/// Per-stage semantic facts used by board adapter emitters.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub(super) struct StageScope<'a> {
    /// Name of the stage currently being emitted, or `None` for monolithic
    /// generation.
    pub(super) name: Option<&'a str>,
    /// Capabilities for this stage only.
    pub(super) capabilities: &'a [Capability],
    /// True for monolithic builds and the first stage in a multi-stage layout.
    pub(super) is_first_stage: bool,
    /// True when this stage needs FFS operations/imports.
    pub(super) uses_ffs: bool,
    /// True when this stage prepares or consumes an FDT.
    pub(super) uses_fdt: bool,
    /// True when this stage prepares ACPI tables.
    pub(super) uses_acpi_prepare: bool,
    /// True when this stage prepares or loads ACPI tables.
    pub(super) uses_acpi: bool,
    /// True when this stage prepares SMBIOS tables.
    pub(super) uses_smbios: bool,
    /// True for Allwinner eGON image handling paths in this stage.
    pub(super) uses_sunxi_egon: bool,
    /// True when this stage installs or enters SMM.
    pub(super) uses_smm: bool,
}

impl<'a> StageScope<'a> {
    /// Build stage facts from board layout and selected capabilities.
    pub(super) fn new(
        config: &BoardConfig,
        name: Option<&'a str>,
        capabilities: &'a [Capability],
    ) -> Self {
        let uses_fdt = capabilities
            .iter()
            .any(|cap| matches!(cap, Capability::FdtPrepare));
        let uses_acpi_prepare = capabilities
            .iter()
            .any(|cap| matches!(cap, Capability::AcpiPrepare));
        let uses_acpi = capabilities
            .iter()
            .any(|cap| matches!(cap, Capability::AcpiPrepare | Capability::AcpiLoad { .. }));
        let uses_smbios = capabilities
            .iter()
            .any(|cap| matches!(cap, Capability::SmBiosPrepare));
        let uses_sunxi_egon = config.soc_image_format == SocImageFormat::AllwinnerEgon;
        let uses_smm = capabilities
            .iter()
            .any(|cap| matches!(cap, Capability::MpInit { smm: true, .. }));
        Self {
            name,
            capabilities,
            is_first_stage: compute_is_first_stage(&config.stages, name),
            uses_ffs: needs_ffs(capabilities),
            uses_fdt,
            uses_acpi_prepare,
            uses_acpi,
            uses_smbios,
            uses_sunxi_egon,
            uses_smm,
        }
    }
}

/// Semantic model passed to every `emit_*` helper.
///
/// The model centralizes stage facts and runtime-device classification so
/// emitters do not rediscover services and topology from raw board strings.
pub(super) struct BoardEmitModel<'a> {
    pub(super) config: &'a BoardConfig,
    pub(super) devices: &'a [DeviceConfig],
    pub(super) instances: &'a [DriverInstance],
    /// Typed runtime-device view used by emitters.
    pub(super) runtime_devices: RuntimeDeviceTable<'a>,
    /// ACPI-only descriptors collected outside runtime device iteration.
    pub(super) acpi_only_devices: &'a [AcpiExtraDevice],
    /// Per-stage capability/scope facts.
    pub(super) stage: StageScope<'a>,
    pub(super) dram_base: u64,
    pub(super) dram_size_static: u64,
}

/// Raw inputs needed to construct a [`BoardEmitModel`].
pub(super) struct BoardEmitInputs<'a> {
    pub(super) config: &'a BoardConfig,
    pub(super) instances: &'a [DriverInstance],
    pub(super) device_tree: &'a [DeviceNode],
    pub(super) device_services: &'a [ServiceSet],
    pub(super) acpi_only_devices: &'a [AcpiExtraDevice],
    pub(super) excluded: &'a [usize],
    pub(super) capabilities: &'a [Capability],
    pub(super) stage_name: Option<&'a str>,
}

/// Codegen-side classification for a flattened device-tree entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeviceKind {
    /// Materialized runtime driver field in `_BoardDevices`.
    Runtime,
    /// Disabled by board policy.
    Disabled,
    /// Runtime-capable device excluded from this stage (usually bus child in
    /// a stage without `DriverInit`).
    Excluded,
    /// Topology-only node.
    Structural,
}

/// Semantic record for one flattened board device.
#[derive(Debug, Clone, Copy)]
pub(super) struct RuntimeDevice<'a> {
    pub(super) index: usize,
    pub(super) name: &'a str,
    pub(super) config: &'a DeviceConfig,
    pub(super) instance: &'a DriverInstance,
    pub(super) node: &'a DeviceNode,
    pub(super) services: ServiceSet,
    pub(super) kind: DeviceKind,
}

impl RuntimeDevice<'_> {
    /// Returns true when this entry is materialized as a runtime field.
    pub(super) fn is_runtime(&self) -> bool {
        self.kind == DeviceKind::Runtime
    }

    /// Returns true if this device provides `service` after board policy.
    pub(super) fn provides(&self, service: Service) -> bool {
        self.services.contains(service)
    }
}

/// Typed table for flattened board devices.
#[derive(Debug, Clone)]
pub(super) struct RuntimeDeviceTable<'a> {
    entries: Vec<RuntimeDevice<'a>>,
}

impl<'a> RuntimeDeviceTable<'a> {
    /// Build the semantic device table from the existing flattened arrays.
    pub(super) fn new(
        devices: &'a [DeviceConfig],
        instances: &'a [DriverInstance],
        device_tree: &'a [DeviceNode],
        device_services: &'a [ServiceSet],
        excluded: &'a [usize],
    ) -> Self {
        let entries = devices
            .iter()
            .zip(instances.iter())
            .zip(device_tree.iter())
            .zip(device_services.iter())
            .enumerate()
            .map(|(idx, (((config, instance), node), services))| {
                let kind = if !config.enabled {
                    DeviceKind::Disabled
                } else if instance.construction_kind() == ConstructionKind::Structural {
                    DeviceKind::Structural
                } else if excluded.contains(&idx) {
                    DeviceKind::Excluded
                } else {
                    DeviceKind::Runtime
                };
                RuntimeDevice {
                    index: idx,
                    name: config.name.as_str(),
                    config,
                    instance,
                    node,
                    services: *services,
                    kind,
                }
            })
            .collect();
        Self { entries }
    }

    /// Entries materialized as runtime fields in this stage.
    pub(super) fn runtime(&self) -> impl Iterator<Item = &RuntimeDevice<'a>> {
        self.entries.iter().filter(|entry| entry.is_runtime())
    }

    /// Indices of entries materialized as runtime fields in this stage.
    pub(super) fn runtime_indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.runtime().map(|entry| entry.index)
    }

    /// Runtime entries providing `service` after board policy.
    pub(super) fn providers(&self, service: Service) -> impl Iterator<Item = &RuntimeDevice<'a>> {
        self.runtime().filter(move |entry| entry.provides(service))
    }

    /// Runtime entries with driver-owned ACPI contributions.
    pub(super) fn acpi_runtime_devices(&self) -> impl Iterator<Item = &RuntimeDevice<'a>> {
        self.runtime()
            .filter(|entry| entry.instance.meta().has_acpi && entry.instance.acpi_name().is_some())
    }

    /// ACPI-only descriptors that have no runtime driver field.
    /// Lookup by flattened device index.
    pub(super) fn get(&self, idx: usize) -> Option<&RuntimeDevice<'a>> {
        self.entries.get(idx)
    }

    /// First non-structural parent name for a bus-attached runtime device.
    pub(super) fn real_parent_name(&self, child_idx: usize) -> Option<&'a str> {
        let mut current = self.get(child_idx)?.node.parent?;
        loop {
            let parent = self.get(usize::from(current))?;
            if parent.kind != DeviceKind::Structural {
                return Some(parent.name);
            }
            current = parent.node.parent?;
        }
    }
}

impl<'a> BoardEmitModel<'a> {
    /// Construct the board adapter emission context.
    pub(super) fn new(inputs: BoardEmitInputs<'a>) -> Self {
        let config = inputs.config;
        let stage = StageScope::new(config, inputs.stage_name, inputs.capabilities);
        let dram = find_dram_region(config).unwrap_or((0, 0));
        let runtime_devices = RuntimeDeviceTable::new(
            &config.devices,
            inputs.instances,
            inputs.device_tree,
            inputs.device_services,
            inputs.excluded,
        );
        Self {
            config,
            devices: &config.devices,
            instances: inputs.instances,
            runtime_devices,
            acpi_only_devices: inputs.acpi_only_devices,
            stage,
            dram_base: dram.0,
            dram_size_static: dram.1,
        }
    }
}

/// Stage-is-first predicate, matching `generate_fstart_main`'s rule.
fn compute_is_first_stage(stages: &StageLayout, stage_name: Option<&str>) -> bool {
    match (stages, stage_name) {
        (StageLayout::Monolithic(_), _) => true,
        (StageLayout::MultiStage(stages), Some(name)) => {
            stages.first().is_some_and(|s| s.name.as_str() == name)
        }
        (StageLayout::MultiStage(_), None) => true,
    }
}
