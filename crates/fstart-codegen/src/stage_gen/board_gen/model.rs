//! Semantic context for board adapter emission.
//!
//! Keep this module focused on facts computed before token emission.  It is
//! the staging point for the fuller `BoardEmitModel` described in
//! `docs/rust-owned-driver-services-plan.md`.

use fstart_device_registry::{DriverInstance, Service};
use fstart_types::{BoardConfig, Capability, DeviceConfig, DeviceNode, StageLayout};

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
}

impl<'a> StageScope<'a> {
    /// Build stage facts from board layout and selected capabilities.
    pub(super) fn new(
        stages: &StageLayout,
        name: Option<&'a str>,
        capabilities: &'a [Capability],
    ) -> Self {
        Self {
            name,
            capabilities,
            is_first_stage: compute_is_first_stage(stages, name),
            uses_ffs: needs_ffs(capabilities),
        }
    }
}

/// Bundle of references passed to every `emit_*` helper.
///
/// This is the transitional form of the planned `BoardEmitModel`: service
/// dispatch is already typed, while the remaining parallel arrays will be
/// folded into a runtime-device table in follow-up changes.
pub(super) struct BoardCtx<'a> {
    pub(super) config: &'a BoardConfig,
    pub(super) devices: &'a [DeviceConfig],
    pub(super) instances: &'a [DriverInstance],
    /// Parent-child structure of the board's devices. Indexed in lock-step
    /// with `devices` and `instances`.
    pub(super) device_tree: &'a [DeviceNode],
    /// Effective typed services for each device after board policy filters.
    pub(super) device_services: &'a [heapless::Vec<Service, 8>],
    pub(super) excluded: &'a [usize],
    /// Per-stage capability/scope facts.
    pub(super) stage: StageScope<'a>,
    pub(super) dram_base: u64,
    pub(super) dram_size_static: u64,
}

impl<'a> BoardCtx<'a> {
    /// Construct the board adapter emission context.
    pub(super) fn new(
        config: &'a BoardConfig,
        instances: &'a [DriverInstance],
        device_tree: &'a [DeviceNode],
        device_services: &'a [heapless::Vec<Service, 8>],
        excluded: &'a [usize],
        capabilities: &'a [Capability],
        stage_name: Option<&'a str>,
    ) -> Self {
        let stage = StageScope::new(&config.stages, stage_name, capabilities);
        let dram = find_dram_region(config).unwrap_or((0, 0));
        Self {
            config,
            devices: &config.devices,
            instances,
            device_tree,
            device_services,
            excluded,
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

/// Query whether a device provides a typed service in this stage's effective
/// service table.
pub(super) fn device_provides(ctx: &BoardCtx<'_>, idx: usize, service: Service) -> bool {
    ctx.device_services[idx].contains(&service)
}
