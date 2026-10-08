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
use crate::scaler::{self, ScalerPlan};
use crate::types::{Cpu, Generation, Pipe, Port};
use crate::{
    GmaInitConfig, GmaInitResult, caps_for, clean_generation_state, disable_generation_output,
    init_candidate,
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
    /// Whether the one-time libgfxinit `Clean_State` teardown has run.
    clean: bool,
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
            clean: false,
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
    ///
    /// Time is passed in rather than read from a global so the update stays
    /// testable. Callers on hardware supply `Timer::timestamp_us()` from
    /// `fstart_core::services::timer`; `update_outputs` passes `u64::MAX` to
    /// disable hot-plug rechecking.
    pub fn update_outputs_at(
        &mut self,
        resources: &GmaResources,
        config: &GmaInitConfig<'_>,
        now_us: u64,
    ) -> Result<UpdateOutputsResult, GmaError> {
        resources.validate()?;
        crate::validate_outputs(config.cpu, config.outputs)?;
        let mmio = legacy_mmio(resources, config.cpu);
        let old_configs = self.current;
        // Clean before probing: cleaning after Panel.On would undo the power
        // state used for EDID. Never clean again when adding another output.
        if !self.clean {
            clean_generation_state(resources, config.cpu);
            self.clean = true;
        }
        let detect = crate::initialize_port_detect(resources, config.cpu);
        let wants_lvds = config
            .outputs
            .iter()
            .any(|output| output.enabled && output.port == Port::Lvds);
        let panel_ready = if wants_lvds {
            if let Some(mmio) = &mmio {
                crate::generation::g45::prepare_panel_probe(mmio, config.vbt).is_ok()
            } else {
                true // Split-PCH panel sequencing remains generation-owned.
            }
        } else {
            false
        };
        let resolved = self.resolve_requested_configs(resources, config, detect.as_ref(), |port| {
            if port == Port::Lvds && !panel_ready {
                return Err(GmaError::ModeUnavailable);
            }
            let outputs = [OutputConfig {
                port,
                enabled: true,
            }];
            let candidate = GmaInitConfig {
                outputs: &outputs,
                ..*config
            };
            let mode = crate::choose_mode(resources, &candidate)?;
            fstart_log::info!(
                "intel-gma: connector {:?} mode {}x{}@{}kHz",
                port,
                mode.hdisplay,
                mode.vdisplay,
                mode.pixel_clock_khz
            );
            Ok(crate::clamp_hdmi_dotclock(config.cpu, port, mode))
        });
        if wants_lvds
            && !resolved.as_ref().is_ok_and(|outputs| {
                outputs
                    .iter()
                    .flatten()
                    .any(|output| output.port == Port::Lvds)
            })
            && let Some(mmio) = &mmio
        {
            crate::generation::g45::panel_backlight_off(mmio);
            crate::generation::g45::panel_power_off(mmio);
        }
        let mut new_configs = resolved?;
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

        if let Some(output) = new_configs.iter().flatten().next()
            && !self
                .current
                .iter()
                .flatten()
                .any(|current| current.surface == output.surface)
        {
            crate::prepare_framebuffer(resources, config.cpu, &output.surface)?;
        }

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
                    match init_candidate(
                        resources,
                        &candidate_config,
                        new_config.pipe,
                        new_config.mode,
                        new_config.surface,
                    ) {
                        Ok(result) => {
                            self.current[pipe_index] = Some(new_config);
                            self.clear_wait_for_hpd(new_config.port);
                            enabled_outputs += 1;
                            if primary.is_none() {
                                primary = Some(result);
                            }
                        }
                        Err(err) => {
                            fstart_log::warn!(
                                "intel-gma: connector {:?} enable failed (code {})",
                                new_config.port,
                                err as u8
                            );
                            last_error = err;
                            self.current[pipe_index] = None;
                            self.set_wait_for_hpd_at(new_config.port, now_us);
                            // Only the failed pipe's output is torn down; other
                            // outputs must survive.
                            if let Ok(pipe) = pipe_from_index(pipe_index) {
                                disable_generation_output(
                                    resources,
                                    config.cpu,
                                    pipe,
                                    new_config.port,
                                );
                            }
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
        mut probe: impl FnMut(Port) -> Result<Mode, GmaError>,
    ) -> Result<[Option<PipeOutputConfig>; PIPE_COUNT], GmaError> {
        let mut modes = [None; PIPE_COUNT];
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
            if modes.iter().flatten().any(|(port, _)| *port == output.port) {
                continue;
            }
            match probe(output.port).and_then(|mode| {
                let available: heapless::Vec<_, PIPE_COUNT> = caps_for(config.cpu)
                    .pipes
                    .iter()
                    .copied()
                    .filter(|pipe| modes[PipeOutputConfig::pipe_index(*pipe)].is_none())
                    .collect();
                crate::generation::output_pipe(config.cpu, output.port, mode, &available)
                    .map(|pipe| (pipe, mode))
            }) {
                Ok((pipe, mode)) => {
                    modes[PipeOutputConfig::pipe_index(pipe)] = Some((output.port, mode))
                }
                Err(err) => {
                    fstart_log::info!(
                        "intel-gma: connector {:?} not selected (code {})",
                        output.port,
                        err as u8
                    );
                    last_error = err;
                }
            }
        }

        if !saw_enabled {
            return Err(GmaError::UnsupportedPort);
        }
        let (width, height) = modes
            .iter()
            .flatten()
            .map(|(_, mode)| (u32::from(mode.hdisplay), u32::from(mode.vdisplay)))
            .reduce(|(width, height), (w, h)| (width.min(w), height.min(h)))
            .ok_or(last_error)?;
        let mut framebuffer = config.framebuffer;
        // Like coreboot's hires_fb glue: one minimum-sized framebuffer, not
        // independent native-sized surfaces at the same physical address.
        framebuffer.width = width;
        framebuffer.height = height;
        if framebuffer.preferred_mode == crate::PreferredMode::Fixed
            && framebuffer.scaling != scaler::ScalingPolicy::None
        {
            framebuffer.width = config.framebuffer.width;
            framebuffer.height = config.framebuffer.height;
        }
        framebuffer.stride = Some(
            framebuffer
                .width
                .checked_add(framebuffer.start_x)
                .and_then(|width| {
                    width.checked_next_multiple_of(framebuffer.tiling.tile_width_units())
                })
                .ok_or(GmaError::InvalidConfig)?,
        );
        framebuffer.v_stride = Some(
            framebuffer
                .height
                .checked_add(framebuffer.start_y)
                .and_then(|height| height.checked_next_multiple_of(framebuffer.tiling.tile_rows()))
                .ok_or(GmaError::InvalidConfig)?,
        );
        // Explicit pitches remain board policy; malformed or too-small
        // pitches fail SurfaceConfig validation rather than being hidden.
        framebuffer.stride = config.framebuffer.stride.or(framebuffer.stride);
        framebuffer.v_stride = config.framebuffer.v_stride.or(framebuffer.v_stride);
        let surface = crate::gtt::choose_framebuffer_surface(resources, &framebuffer)?;
        let mut fitter_owner = None;
        for (index, candidate) in modes.into_iter().enumerate() {
            let Some((port, mode)) = candidate else {
                continue;
            };
            let pipe = pipe_from_index(index)?;
            let plan =
                ScalerPlan::resolve(config.cpu, pipe, surface, mode, config.framebuffer.scaling);
            if let Err(error) = plan.validate_future_enablement() {
                last_error = error;
                continue;
            }
            if plan.requires_scaling && scaler::caps_for(config.cpu).single_global_scaler {
                // Gen3's fitter is physically wired to B; i965 can select its
                // owner, but cannot scale both pipes simultaneously.
                if (matches!(caps_for(config.cpu).generation, Generation::I945) && pipe != Pipe::B)
                    || !plan.can_reserve_global(fitter_owner)
                {
                    fstart_log::warn!(
                        "intel-gma: pipe {} cannot reserve the shared fitter",
                        index as u8
                    );
                    last_error = GmaError::InvalidConfig;
                    continue;
                }
                fitter_owner = Some(pipe);
            }
            new_configs[index] = Some(PipeOutputConfig {
                pipe,
                port,
                mode,
                surface,
            });
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
                if let Ok(pipe) = pipe_from_index(pipe_index) {
                    disable_generation_output(resources, cpu, pipe, current.port);
                }
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

fn full_update(current: PipeOutputConfig, new_config: PipeOutputConfig) -> bool {
    current.port != new_config.port
        || current.mode != new_config.mode
        || current.surface != new_config.surface
}

fn legacy_mmio(resources: &GmaResources, cpu: Cpu) -> Option<Mmio> {
    if matches!(caps_for(cpu).generation, Generation::I945 | Generation::G45) {
        Some(crate::mmio_from_validated_resources(resources))
    } else {
        None
    }
}

/// Map a pipe array index back to a pipe.
const fn pipe_from_index(index: usize) -> Result<Pipe, GmaError> {
    match index {
        0 => Ok(Pipe::A),
        1 => Ok(Pipe::B),
        2 => Ok(Pipe::C),
        _ => Err(GmaError::InvalidConfig),
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
    use crate::types::PciAddress;
    use fstart_core::typed::mmio32;

    fn resources() -> GmaResources {
        GmaResources {
            pci_bdf: PciAddress::new(0, 0, 2, 0),
            gtt_mmio_base: mmio32(0x1000),
            gtt_mmio_size: 0x80000,
            gtt_pte_base: None,
            gmadr_base: Some(0x8000_0000),
            gmadr_size: 0x1000_0000,
            stolen_base: 0x7f00_0000,
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
            .resolve_requested_configs(&resources(), &config, None, |_| Ok(Mode::XGA_1024X768_60))
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
            .resolve_requested_configs(&resources(), &config, None, |_| Ok(Mode::XGA_1024X768_60))
            .unwrap();
        assert_eq!(
            configs[PipeOutputConfig::pipe_index(Pipe::A)].unwrap().port,
            Port::Vga
        );
    }

    fn native_mode(width: u16, height: u16) -> Mode {
        Mode {
            hdisplay: width,
            hsync_start: width + 24,
            hsync_end: width + 160,
            htotal: width + 320,
            vdisplay: height,
            vsync_start: height + 3,
            vsync_end: height + 9,
            vtotal: height + 40,
            ..Mode::XGA_1024X768_60
        }
    }

    fn edid_config<'a>(cpu: Cpu, outputs: &'a [OutputConfig]) -> GmaInitConfig<'a> {
        GmaInitConfig {
            cpu,
            outputs,
            framebuffer: FramebufferConfig {
                preferred_mode: PreferredMode::Edid,
                scaling: ScalingPolicy::PreserveAspect,
                ..framebuffer()
            },
            vbt: None,
        }
    }

    #[test]
    fn mirror_planning_keeps_native_timings_and_one_surface_across_generations() {
        let legacy = [
            OutputConfig {
                port: Port::Vga,
                enabled: true,
            },
            OutputConfig {
                port: Port::Lvds,
                enabled: true,
            },
        ];
        let ddi = [
            OutputConfig {
                port: Port::HdmiA,
                enabled: true,
            },
            OutputConfig {
                port: Port::HdmiB,
                enabled: true,
            },
            OutputConfig {
                port: Port::Edp,
                enabled: true,
            },
        ];
        for cpu in [
            Cpu::I945GM,
            Cpu::PineviewM,
            Cpu::Gm965,
            Cpu::Haswell,
            Cpu::Skylake,
            Cpu::Tigerlake,
        ] {
            let legacy_cpu = matches!(caps_for(cpu).generation, Generation::I945 | Generation::G45);
            let outputs = if legacy_cpu { &legacy[..] } else { &ddi[..] };
            let config = edid_config(cpu, outputs);
            let planned = GmaDisplayState::new()
                .resolve_requested_configs(&resources(), &config, None, |port| {
                    Ok(match port {
                        Port::Vga | Port::HdmiA => native_mode(800, 600),
                        Port::Lvds | Port::HdmiB => native_mode(1024, 768),
                        _ => native_mode(1280, 720),
                    })
                })
                .unwrap();
            assert_eq!(planned.iter().flatten().count(), outputs.len(), "{cpu:?}");
            let first = planned[0].unwrap();
            assert_eq!((first.surface.width, first.surface.height), (800, 600));
            for output in planned.iter().flatten() {
                assert_eq!(output.surface, first.surface);
            }
            assert_eq!(planned[1].unwrap().mode, native_mode(1024, 768));
            // Newer backends remain explicit about unimplemented scaling:
            // successful common planning is not hardware enablement.
            if !legacy_cpu {
                assert_eq!(
                    ScalerPlan::resolve(
                        cpu,
                        Pipe::B,
                        first.surface,
                        planned[1].unwrap().mode,
                        ScalingPolicy::PreserveAspect
                    )
                    .validate_current(),
                    Err(GmaError::UnsupportedPlatform)
                );
            }
        }
    }

    #[test]
    fn absent_connectors_leave_the_other_output_at_its_native_size() {
        let outputs = [
            OutputConfig {
                port: Port::Vga,
                enabled: true,
            },
            OutputConfig {
                port: Port::Lvds,
                enabled: true,
            },
        ];
        let config = edid_config(Cpu::Gm965, &outputs);
        for absent in [Port::Lvds, Port::Vga] {
            let planned = GmaDisplayState::new()
                .resolve_requested_configs(&resources(), &config, None, |port| {
                    if port == absent {
                        Err(GmaError::ModeUnavailable)
                    } else {
                        Ok(native_mode(1280, 720))
                    }
                })
                .unwrap();
            assert_eq!(planned.iter().flatten().count(), 1);
            let output = planned.iter().flatten().next().unwrap();
            assert_ne!(output.port, absent);
            assert_eq!((output.surface.width, output.surface.height), (1280, 720));
        }
        assert_eq!(
            GmaDisplayState::new().resolve_requested_configs(&resources(), &config, None, |_| Err(
                GmaError::ModeUnavailable
            )),
            Err(GmaError::ModeUnavailable)
        );
    }

    #[test]
    fn fixed_canvas_keeps_explicit_pitch_and_offsets() {
        let outputs = [OutputConfig {
            port: Port::Vga,
            enabled: true,
        }];
        let config = GmaInitConfig {
            cpu: Cpu::Gm965,
            outputs: &outputs,
            framebuffer: FramebufferConfig {
                width: 320,
                height: 200,
                stride: Some(512),
                v_stride: Some(256),
                start_x: 16,
                start_y: 8,
                offset: 0x4000,
                scaling: ScalingPolicy::Stretch,
                ..framebuffer()
            },
            vbt: None,
        };
        let planned = GmaDisplayState::new()
            .resolve_requested_configs(&resources(), &config, None, |_| Ok(Mode::XGA_1024X768_60))
            .unwrap();
        let surface = planned[0].unwrap().surface;
        assert_eq!((surface.width, surface.height), (320, 200));
        assert_eq!((surface.stride, surface.v_stride), (512, 256));
        assert_eq!(
            (surface.start_x, surface.start_y, surface.offset),
            (16, 8, 0x4000)
        );
    }

    #[test]
    fn shared_fitter_conflict_is_explicit_instead_of_overwriting_the_first_owner() {
        let outputs = [
            OutputConfig {
                port: Port::Vga,
                enabled: true,
            },
            OutputConfig {
                port: Port::Lvds,
                enabled: true,
            },
        ];
        let config = edid_config(Cpu::Gm965, &outputs);
        let planned = GmaDisplayState::new()
            .resolve_requested_configs(&resources(), &config, None, |port| {
                Ok(if port == Port::Vga {
                    native_mode(1280, 720)
                } else {
                    native_mode(1024, 768)
                })
            })
            .unwrap();
        assert!(planned[0].is_some());
        assert!(planned[1].is_none());
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
        let surface = SurfaceConfig::packed(0x8000_0000, 1024, 768, PixelFormat::Xrgb8888);
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
