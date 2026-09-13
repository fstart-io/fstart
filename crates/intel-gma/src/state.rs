//! Persistent display-output state modeled after libgfxinit `Update_Outputs`.
//!
//! The state object keeps the currently active pipe/output configuration and
//! hotplug wait flags separate from board policy.  The legacy `init()` entry
//! uses a temporary state for backwards compatibility, while chipset drivers
//! that need repeated display updates can retain [`GmaDisplayState`] and call
//! [`GmaDisplayState::update_outputs`].

use crate::config::OutputConfig;
use crate::error::GmaError;
use crate::framebuffer::SurfaceConfig;
use crate::mmio::Mmio;
use crate::mode::Mode;
use crate::pci::GmaResources;
use crate::port;
use crate::types::{Cpu, Generation, Pipe, Port};
use crate::{
    GmaInitConfig, GmaInitResult, caps_for, cleanup_after_failed_candidate, init_candidate,
};

const MAX_PORTS: usize = 8;
const PIPE_COUNT: usize = 3;
/// libgfxinit waits roughly 333ms between hotplug re-probes.
pub const HPD_RECHECK_DELAY_US: u64 = 333_000;

/// Current or requested configuration for one display pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipeOutputConfig {
    /// Pipe programmed for this output.
    pub pipe: Pipe,
    /// Output port driven by the pipe.
    pub port: Port,
    /// Active mode.
    pub mode: Mode,
    /// Framebuffer surface used by the primary plane.
    pub surface: SurfaceConfig,
}

impl PipeOutputConfig {
    const fn pipe_index(pipe: Pipe) -> usize {
        match pipe {
            Pipe::A => 0,
            Pipe::B => 1,
            Pipe::C => 2,
        }
    }
}

/// Result of one `update_outputs` transition.
#[derive(Debug, Clone, Copy)]
pub struct UpdateOutputsResult {
    /// First successfully enabled framebuffer, suitable for payload handoff.
    pub primary: Option<GmaInitResult>,
    /// Number of outputs enabled by the transition.
    pub enabled_outputs: usize,
}

/// Persistent output state for repeated display updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GmaDisplayState {
    current: [Option<PipeOutputConfig>; PIPE_COUNT],
    wait_for_hpd: [Option<Port>; MAX_PORTS],
    wait_for_hpd_deadline_us: [u64; MAX_PORTS],
    wait_for_hpd_len: usize,
}

impl Default for GmaDisplayState {
    fn default() -> Self {
        Self::new()
    }
}

impl GmaDisplayState {
    /// Create an empty display state.
    pub const fn new() -> Self {
        Self {
            current: [None; PIPE_COUNT],
            wait_for_hpd: [None; MAX_PORTS],
            wait_for_hpd_deadline_us: [0; MAX_PORTS],
            wait_for_hpd_len: 0,
        }
    }

    /// Return the current config for a pipe.
    pub const fn current(&self, pipe: Pipe) -> Option<PipeOutputConfig> {
        self.current[PipeOutputConfig::pipe_index(pipe)]
    }

    /// Return true when the state is waiting for a hotplug event before trying `port` again.
    pub const fn waits_for_hpd(&self, port: Port) -> bool {
        let mut index = 0usize;
        while index < self.wait_for_hpd_len {
            if let Some(wait_port) = self.wait_for_hpd[index]
                && wait_port as u8 == port as u8
            {
                return true;
            }
            index += 1;
        }
        false
    }

    /// Return the next scheduled hotplug re-probe deadline in microseconds.
    pub const fn next_hpd_poll_deadline_us(&self) -> Option<u64> {
        if self.wait_for_hpd_len == 0 {
            return None;
        }
        let mut index = 0usize;
        let mut best = u64::MAX;
        while index < self.wait_for_hpd_len {
            if self.wait_for_hpd[index].is_some() && self.wait_for_hpd_deadline_us[index] < best {
                best = self.wait_for_hpd_deadline_us[index];
            }
            index += 1;
        }
        Some(best)
    }

    /// Update outputs from board policy using libgfxinit-style old/new sequencing.
    ///
    /// This compatibility entry point probes any pending HPD wait immediately.
    /// Use [`Self::update_outputs_at`] when the caller has a firmware timer and
    /// wants libgfxinit-style 333ms hotplug polling cadence.
    pub fn update_outputs(
        &mut self,
        resources: &GmaResources,
        config: &GmaInitConfig<'_>,
    ) -> Result<UpdateOutputsResult, GmaError> {
        self.update_outputs_at(resources, config, u64::MAX)
    }

    /// Update outputs using `now_us` as the current monotonic time in microseconds.
    pub fn update_outputs_at(
        &mut self,
        resources: &GmaResources,
        config: &GmaInitConfig<'_>,
        now_us: u64,
    ) -> Result<UpdateOutputsResult, GmaError> {
        let detect = crate::initialize_port_detect(resources, config.cpu);
        let mmio = legacy_mmio(resources, config.cpu);
        let old_configs = self.current;
        let mut new_configs = self.resolve_requested_configs(resources, config, detect.as_ref())?;

        if let Some(mmio) = &mmio {
            self.apply_hpd_filter(mmio, &mut new_configs, now_us);
        }

        self.disable_changed_outputs(
            resources,
            config.cpu,
            &old_configs,
            &new_configs,
            mmio.as_ref(),
        );

        let mut primary = None;
        let mut enabled_outputs = 0usize;
        let mut last_error = GmaError::UnsupportedPort;
        for (pipe_index, new_config) in new_configs.iter().copied().enumerate() {
            let Some(new_config) = new_config else {
                continue;
            };
            match self.current[pipe_index] {
                Some(current) if !full_update(current, new_config) => {
                    enabled_outputs += 1;
                    if primary.is_none() {
                        primary = Some(GmaInitResult {
                            framebuffer: current.surface.to_framebuffer_info(),
                        });
                    }
                }
                _ => {
                    let candidate_outputs = [OutputConfig {
                        port: new_config.port,
                        enabled: true,
                    }];
                    let candidate_config = GmaInitConfig {
                        cpu: config.cpu,
                        outputs: &candidate_outputs,
                        framebuffer: config.framebuffer,
                        vbt: config.vbt,
                    };
                    if let Some(mmio) = &mmio {
                        let _ = crate::port_detect::clear_hotplug_detect(mmio, new_config.port);
                    }
                    match init_candidate(resources, &candidate_config) {
                        Ok(result) => {
                            self.current[pipe_index] = Some(new_config);
                            self.clear_wait_for_hpd(new_config.port);
                            enabled_outputs += 1;
                            if primary.is_none() {
                                primary = Some(result);
                            }
                        }
                        Err(err) => {
                            last_error = err;
                            self.current[pipe_index] = None;
                            self.set_wait_for_hpd_at(new_config.port, now_us);
                            cleanup_after_failed_candidate(resources, config.cpu);
                        }
                    }
                }
            }
        }

        if enabled_outputs == 0 {
            Err(last_error)
        } else {
            Ok(UpdateOutputsResult {
                primary,
                enabled_outputs,
            })
        }
    }

    fn resolve_requested_configs(
        &self,
        resources: &GmaResources,
        config: &GmaInitConfig<'_>,
        detect: Option<&crate::port_detect::LegacyPortDetectState>,
    ) -> Result<[Option<PipeOutputConfig>; PIPE_COUNT], GmaError> {
        let mut new_configs = [None; PIPE_COUNT];
        let mut saw_enabled = false;
        let mut last_error = GmaError::UnsupportedPort;

        for output in config.outputs.iter().filter(|output| output.enabled) {
            saw_enabled = true;
            if let Some(detect) = detect
                && !detect.is_valid(output.port)
            {
                last_error = GmaError::ModeUnavailable;
                continue;
            }
            match resolve_pipe_config(resources, config, output.port) {
                Ok(pipe_config) => {
                    let index = PipeOutputConfig::pipe_index(pipe_config.pipe);
                    if new_configs[index].is_none() {
                        new_configs[index] = Some(pipe_config);
                    }
                }
                Err(err) => last_error = err,
            }
        }

        if !saw_enabled {
            return Err(GmaError::UnsupportedPort);
        }
        if new_configs.iter().all(Option::is_none) {
            return Err(last_error);
        }
        Ok(new_configs)
    }

    fn apply_hpd_filter(
        &mut self,
        mmio: &Mmio,
        new_configs: &mut [Option<PipeOutputConfig>; PIPE_COUNT],
        now_us: u64,
    ) {
        for config in new_configs.iter_mut() {
            let Some(pipe_config) = config else {
                continue;
            };
            if self.waits_for_hpd(pipe_config.port) {
                if !self.hpd_poll_due(pipe_config.port, now_us) {
                    *config = None;
                    continue;
                }
                match crate::port_detect::hotplug_detect(mmio, pipe_config.port) {
                    Ok(true) => self.clear_wait_for_hpd(pipe_config.port),
                    Ok(false) | Err(_) => {
                        self.set_wait_for_hpd_at(pipe_config.port, now_us);
                        *config = None;
                    }
                }
            }
        }
    }

    fn disable_changed_outputs(
        &mut self,
        resources: &GmaResources,
        cpu: Cpu,
        old_configs: &[Option<PipeOutputConfig>; PIPE_COUNT],
        new_configs: &[Option<PipeOutputConfig>; PIPE_COUNT],
        mmio: Option<&Mmio>,
    ) {
        for pipe_index in 0..PIPE_COUNT {
            let Some(current) = old_configs[pipe_index] else {
                continue;
            };
            let unplug_detected = mmio
                .and_then(|mmio| crate::port_detect::hotplug_detect(mmio, current.port).ok())
                .unwrap_or(false);
            if unplug_detected
                || new_configs[pipe_index]
                    .map(|new| full_update(current, new))
                    .unwrap_or(true)
            {
                cleanup_after_failed_candidate(resources, cpu);
                self.current[pipe_index] = None;
                if unplug_detected {
                    self.set_wait_for_hpd_at(current.port, u64::MAX);
                }
            }
        }
    }

    fn set_wait_for_hpd_at(&mut self, port: Port, now_us: u64) {
        let deadline = now_us.saturating_add(HPD_RECHECK_DELAY_US);
        let mut index = 0usize;
        while index < self.wait_for_hpd_len {
            if self.wait_for_hpd[index]
                .map(|wait_port| wait_port as u8 == port as u8)
                .unwrap_or(false)
            {
                self.wait_for_hpd_deadline_us[index] = deadline;
                return;
            }
            index += 1;
        }
        if self.wait_for_hpd_len >= self.wait_for_hpd.len() {
            return;
        }
        self.wait_for_hpd[self.wait_for_hpd_len] = Some(port);
        self.wait_for_hpd_deadline_us[self.wait_for_hpd_len] = deadline;
        self.wait_for_hpd_len += 1;
    }

    fn hpd_poll_due(&self, port: Port, now_us: u64) -> bool {
        let mut index = 0usize;
        while index < self.wait_for_hpd_len {
            if self.wait_for_hpd[index]
                .map(|wait_port| wait_port as u8 == port as u8)
                .unwrap_or(false)
            {
                return now_us >= self.wait_for_hpd_deadline_us[index];
            }
            index += 1;
        }
        true
    }

    fn clear_wait_for_hpd(&mut self, port: Port) {
        let mut write = 0usize;
        let mut read = 0usize;
        while read < self.wait_for_hpd_len {
            if self.wait_for_hpd[read]
                .map(|wait_port| wait_port as u8 != port as u8)
                .unwrap_or(false)
            {
                self.wait_for_hpd[write] = self.wait_for_hpd[read];
                self.wait_for_hpd_deadline_us[write] = self.wait_for_hpd_deadline_us[read];
                write += 1;
            }
            read += 1;
        }
        let new_len = write;
        while write < self.wait_for_hpd_len {
            self.wait_for_hpd[write] = None;
            self.wait_for_hpd_deadline_us[write] = 0;
            write += 1;
        }
        self.wait_for_hpd_len = new_len;
    }
}

fn resolve_pipe_config(
    resources: &GmaResources,
    config: &GmaInitConfig<'_>,
    port: Port,
) -> Result<PipeOutputConfig, GmaError> {
    let output = [OutputConfig {
        port,
        enabled: true,
    }];
    let candidate_config = GmaInitConfig {
        cpu: config.cpu,
        outputs: &output,
        framebuffer: config.framebuffer,
        vbt: config.vbt,
    };
    let mode = crate::choose_mode(resources, &candidate_config)?;
    let surface = crate::gtt::choose_framebuffer_surface(resources, &config.framebuffer)?;
    let pipeline = match caps_for(config.cpu).generation {
        Generation::I9xx | Generation::G45 => {
            port::OutputPipeline::legacy_gmch(config.cpu, port, mode, surface)?
        }
        _ => return Err(GmaError::UnsupportedPlatform),
    };
    Ok(PipeOutputConfig {
        pipe: pipeline.pipe.pipe,
        port,
        mode,
        surface,
    })
}

fn full_update(current: PipeOutputConfig, new_config: PipeOutputConfig) -> bool {
    current.port != new_config.port
        || current.mode != new_config.mode
        || current.surface != new_config.surface
}

fn legacy_mmio(resources: &GmaResources, cpu: Cpu) -> Option<Mmio> {
    if matches!(caps_for(cpu).generation, Generation::I9xx | Generation::G45) {
        Some(crate::mmio_from_validated_resources(resources))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PreferredMode;
    use crate::framebuffer::{FramebufferConfig, PixelFormat};
    use crate::mode::FallbackMode;
    use crate::pci::GmaResources;
    use crate::scaler::ScalingPolicy;
    use crate::types::{PciBdf, PhysAddr};

    fn resources() -> GmaResources {
        GmaResources {
            pci_bdf: PciBdf {
                bus: 0,
                dev: 2,
                func: 0,
            },
            gtt_mmio_base: PhysAddr(0x1000),
            gtt_mmio_size: 0x80000,
            gtt_pte_base: None,
            gmadr_base: Some(PhysAddr(0x8000_0000)),
            gmadr_size: 0x1000_0000,
            stolen_base: PhysAddr(0x7f00_0000),
            stolen_size: 0x800000,
            gtt_size: 0x10000,
            gcfgc: None,
        }
    }

    fn framebuffer() -> FramebufferConfig {
        FramebufferConfig {
            width: 1024,
            height: 768,
            bits_per_pixel: 32,
            stride: None,
            v_stride: None,
            start_x: 0,
            start_y: 0,
            offset: 0,
            tiling: Default::default(),
            rotation: Default::default(),
            preferred_mode: PreferredMode::Fixed,
            fallback_mode: Some(FallbackMode {
                width: 1024,
                height: 768,
                refresh_hz: 60,
            }),
            scaling: ScalingPolicy::None,
        }
    }

    #[test]
    fn resolves_two_legacy_outputs_on_distinct_pipes() {
        let outputs = [
            OutputConfig {
                port: Port::Lvds,
                enabled: true,
            },
            OutputConfig {
                port: Port::Vga,
                enabled: true,
            },
        ];
        let config = GmaInitConfig {
            cpu: Cpu::Gm965,
            outputs: &outputs,
            framebuffer: framebuffer(),
            vbt: None,
        };
        let state = GmaDisplayState::new();
        let configs = state
            .resolve_requested_configs(&resources(), &config, None)
            .unwrap();
        assert_eq!(
            configs[PipeOutputConfig::pipe_index(Pipe::A)].unwrap().port,
            Port::Vga
        );
        assert_eq!(
            configs[PipeOutputConfig::pipe_index(Pipe::B)].unwrap().port,
            Port::Lvds
        );
    }

    #[test]
    fn keeps_first_candidate_when_ports_share_pipe() {
        let outputs = [
            OutputConfig {
                port: Port::Vga,
                enabled: true,
            },
            OutputConfig {
                port: Port::HdmiA,
                enabled: true,
            },
        ];
        let config = GmaInitConfig {
            cpu: Cpu::G45,
            outputs: &outputs,
            framebuffer: framebuffer(),
            vbt: None,
        };
        let state = GmaDisplayState::new();
        let configs = state
            .resolve_requested_configs(&resources(), &config, None)
            .unwrap();
        assert_eq!(
            configs[PipeOutputConfig::pipe_index(Pipe::A)].unwrap().port,
            Port::Vga
        );
    }

    #[test]
    fn hpd_wait_list_is_persistent_and_clearable() {
        let mut state = GmaDisplayState::new();
        state.set_wait_for_hpd_at(Port::DpA, 1_000);
        assert!(state.waits_for_hpd(Port::DpA));
        assert_eq!(state.next_hpd_poll_deadline_us(), Some(334_000));
        assert!(!state.hpd_poll_due(Port::DpA, 333_999));
        assert!(state.hpd_poll_due(Port::DpA, 334_000));
        state.clear_wait_for_hpd(Port::DpA);
        assert!(!state.waits_for_hpd(Port::DpA));
        assert_eq!(state.next_hpd_poll_deadline_us(), None);
    }

    #[test]
    fn hpd_wait_deadline_is_refreshed_on_repeated_failure() {
        let mut state = GmaDisplayState::new();
        state.set_wait_for_hpd_at(Port::DpA, 1_000);
        state.set_wait_for_hpd_at(Port::DpA, 2_000);
        assert_eq!(state.next_hpd_poll_deadline_us(), Some(335_000));
        assert_eq!(state.wait_for_hpd_len, 1);
    }

    #[test]
    fn full_update_detects_port_mode_or_surface_change() {
        let surface =
            SurfaceConfig::packed(PhysAddr(0x8000_0000), 1024, 768, PixelFormat::Xrgb8888);
        let current = PipeOutputConfig {
            pipe: Pipe::A,
            port: Port::Vga,
            mode: Mode::XGA_1024X768_60,
            surface,
        };
        assert!(!full_update(current, current));
        assert!(full_update(
            current,
            PipeOutputConfig {
                port: Port::HdmiA,
                ..current
            }
        ));
    }
}
