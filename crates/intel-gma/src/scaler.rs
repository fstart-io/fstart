//! Panel-fitter/scaler planning helpers.
//!
//! libgfxinit decides whether a framebuffer requires scaling before reserving a
//! panel fitter or pipe scaler. fstart does not program those hardware blocks
//! yet, but keeping the decision as typed data makes the current exact-size
//! restriction explicit and keeps future hardware enablement local to this
//! module and the generation-specific pipe setup code.

use serde::{Deserialize, Serialize};

use crate::error::GmaError;
use crate::framebuffer::SurfaceConfig;
use crate::mode::Mode;
use crate::regs::{PF_CTL, PF_WIN, PFIT_CONTROL, PFIT_PGM_RATIOS};
use crate::types::{Cpu, Pipe};

/// Board policy for framebuffer-to-mode scaling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScalingPolicy {
    /// Require framebuffer dimensions to exactly match the display mode.
    #[default]
    None,
    /// Preserve the framebuffer aspect ratio using letterbox/pillarbox as needed.
    PreserveAspect,
    /// Stretch the framebuffer to the full mode active area.
    Stretch,
    /// Do not scale; center the source inside the target active area.
    Center,
}

/// libgfxinit-style aspect classification for a scaled framebuffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalingAspect {
    /// Source and destination aspect ratios match.
    Uniform,
    /// Destination is relatively wider/shorter than source; horizontal letterbox bars would be used.
    Letterbox,
    /// Destination is relatively taller/narrower than source; vertical pillarbox bars would be used.
    Pillarbox,
}

/// Hardware scaler family that would be used by a future implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalerKind {
    /// Pre-i965 GMCH panel fitter family used by i9xx/Pineview-style display blocks.
    GmchPanelFitterPreI965,
    /// i965/G45 GMCH global panel fitter, selected by `GMCH_PFIT_CONTROL`.
    GmchPanelFitterI965,
    /// Ironlake/PCH-style per-pipe panel fitter.
    IronlakePanelFitter,
    /// Skylake+ pipe scaler.
    PipeScaler,
}

/// Static scaler capabilities for a platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScalerCaps {
    /// Hardware scaler family, when known and supported by the hardware.
    pub kind: Option<ScalerKind>,
    /// Whether fstart currently programs this scaler family.
    pub implemented: bool,
    /// Whether scaler reservation is global to one pipe.
    pub single_global_scaler: bool,
}

/// Destination rectangle class produced by scaling policy resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestinationKind {
    /// Source exactly fills the target active area.
    Exact,
    /// Source is stretched to fill the target active area.
    Stretch,
    /// Source is aspect-scaled to full width with horizontal bars above/below.
    Letterbox,
    /// Source is aspect-scaled to full height with vertical bars left/right.
    Pillarbox,
    /// Source is unscaled and centered inside the target active area.
    Centered,
}

/// Data-only source-to-target active-area rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DestinationRect {
    /// Horizontal offset from target active-area origin.
    pub x: u32,
    /// Vertical offset from target active-area origin.
    pub y: u32,
    /// Destination width inside the target active area.
    pub width: u32,
    /// Destination height inside the target active area.
    pub height: u32,
    /// Classification of how the rectangle was selected.
    pub kind: DestinationKind,
}

/// Resolved scaler decision for one pipe/framebuffer/mode combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScalerPlan {
    /// Pipe that would own the scaler.
    pub pipe: Pipe,
    /// Board scaling policy.
    pub policy: ScalingPolicy,
    /// Whether source and destination active sizes differ.
    pub requires_scaling: bool,
    /// Aspect classification when scaling is required.
    pub aspect: ScalingAspect,
    /// Hardware scaler family for this platform, if any.
    pub kind: Option<ScalerKind>,
    /// Whether fstart currently programs this scaler family for this platform.
    pub implemented: bool,
    /// Framebuffer/source active width.
    pub source_width: u32,
    /// Framebuffer/source active height.
    pub source_height: u32,
    /// Display mode/destination active width.
    pub target_width: u16,
    /// Display mode/destination active height.
    pub target_height: u16,
}

impl ScalerPlan {
    /// Build a scaler plan without programming hardware.
    pub fn resolve(
        cpu: Cpu,
        pipe: Pipe,
        surface: SurfaceConfig,
        mode: Mode,
        policy: ScalingPolicy,
    ) -> Self {
        Self {
            pipe,
            policy,
            requires_scaling: requires_scaling(surface, mode),
            aspect: scaling_aspect(surface.width, surface.height, mode.hdisplay, mode.vdisplay),
            kind: caps_for(cpu).kind,
            implemented: caps_for(cpu).implemented,
            source_width: surface.width,
            source_height: surface.height,
            target_width: mode.hdisplay,
            target_height: mode.vdisplay,
        }
    }

    /// Validate the current implementation restriction.
    ///
    /// Scaling is modeled but intentionally not enabled yet. Exact-size modes
    /// are accepted. Mismatches are rejected explicitly even if the hardware
    /// has a panel fitter, because no generation path programs it today.
    pub const fn validate_current(self) -> Result<(), GmaError> {
        if !self.requires_scaling {
            return Ok(());
        }
        if !self.implemented {
            return Err(GmaError::UnsupportedPlatform);
        }
        self.validate_future_enablement()
    }

    /// Validate constraints that a future scaler implementation must satisfy.
    ///
    /// This does not imply programming is enabled. It exists so tests can lock
    /// down libgfxinit/Linux-style rules before hardware writes are added:
    /// policy must request scaling, a scaler family must exist, and GMCH panel
    /// fitters reject downscaling. PCH/Ironlake-style fitters and pipe scalers
    /// have generation-specific limits that are intentionally left to their
    /// encoders/programming paths instead of being overclaimed here.
    pub const fn validate_future_enablement(self) -> Result<(), GmaError> {
        if !self.requires_scaling {
            return Ok(());
        }
        if matches!(self.policy, ScalingPolicy::None) {
            return Err(GmaError::InvalidConfig);
        }
        let Some(kind) = self.kind else {
            return Err(GmaError::UnsupportedPlatform);
        };
        if matches!(
            kind,
            ScalerKind::GmchPanelFitterPreI965 | ScalerKind::GmchPanelFitterI965
        ) && (self.source_width > self.target_width as u32
            || self.source_height > self.target_height as u32)
        {
            return Err(GmaError::InvalidConfig);
        }
        Ok(())
    }

    /// Resolve the future destination rectangle for this scaler plan.
    pub const fn destination_rect(self) -> Result<DestinationRect, GmaError> {
        plan_destination_rect(
            self.source_width,
            self.source_height,
            self.target_width,
            self.target_height,
            self.policy,
        )
    }

    /// Resolve the future destination rectangle after applying generation constraints.
    pub const fn constrained_destination_rect(self) -> Result<DestinationRect, GmaError> {
        if let Err(err) = self.validate_future_enablement() {
            return Err(err);
        }
        let rect = if matches!(self.kind, Some(ScalerKind::IronlakePanelFitter)) {
            match plan_pch_destination_rect(
                self.source_width,
                self.source_height,
                self.target_width,
                self.target_height,
                self.policy,
            ) {
                Ok(rect) => rect,
                Err(err) => return Err(err),
            }
        } else {
            match self.destination_rect() {
                Ok(rect) => rect,
                Err(err) => return Err(err),
            }
        };
        if let Err(err) =
            validate_destination_for_kind(self.kind, self.source_width, self.source_height, rect)
        {
            return Err(err);
        }
        Ok(rect)
    }

    /// Return whether a single-global GMCH panel fitter can be reserved.
    pub const fn can_reserve_global(self, already_reserved_for: Option<Pipe>) -> bool {
        match already_reserved_for {
            Some(pipe) => pipe as u8 == self.pipe as u8,
            None => true,
        }
    }
}

/// MMIO offsets used by the legacy GMCH panel fitter block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GmchPfitRegs {
    /// `PFIT_CONTROL` / `GMCH_PFIT_CONTROL`.
    pub control: usize,
    /// `PFIT_PGM_RATIOS`.
    pub pgm_ratios: usize,
    /// `PFIT_AUTO_RATIOS`.
    pub auto_ratios: usize,
}

impl GmchPfitRegs {
    /// Legacy GMCH panel fitter registers from Linux/libgfxinit.
    pub const DEFAULT: Self = Self {
        control: 0x61230,
        pgm_ratios: 0x61234,
        auto_ratios: 0x61238,
    };
}

/// Encoded legacy GMCH panel-fitter state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GmchPfitEncoding {
    /// Value for `PFIT_CONTROL`.
    pub control: u32,
    /// Value for `PFIT_PGM_RATIOS`.
    pub pgm_ratios: u32,
    /// LVDS border bit requested by pre-i965 aspect-preserving paths.
    pub lvds_border: bool,
}

impl GmchPfitEncoding {
    /// Return disabled GMCH panel-fitter register values.
    pub const fn disabled() -> Self {
        Self {
            control: 0,
            pgm_ratios: 0,
            lvds_border: false,
        }
    }
}

/// Ironlake/PCH panel fitter register offsets for one pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IronlakePfitRegs {
    /// `PF_WIN_POS(pipe)`.
    pub win_pos: usize,
    /// `PF_WIN_SZ(pipe)`.
    pub win_size: usize,
    /// `PF_CTL(pipe)`.
    pub control: usize,
}

impl IronlakePfitRegs {
    /// Return the PCH panel-fitter registers used by Linux/libgfxinit for a pipe.
    pub const fn for_pipe(pipe: Pipe) -> Self {
        match pipe {
            Pipe::A => Self {
                win_pos: 0x68070,
                win_size: 0x68074,
                control: 0x68080,
            },
            Pipe::B => Self {
                win_pos: 0x68870,
                win_size: 0x68874,
                control: 0x68880,
            },
            Pipe::C => Self {
                win_pos: 0x69070,
                win_size: 0x69074,
                control: 0x69080,
            },
        }
    }
}

/// Encoded Ironlake/PCH-style panel-fitter state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IronlakePfitEncoding {
    /// Value for `PF_CTL(pipe)`.
    pub control: u32,
    /// Value for `PF_WIN_POS(pipe)`.
    pub win_pos: u32,
    /// Value for `PF_WIN_SZ(pipe)`.
    pub win_size: u32,
}

/// Return scaler capabilities for a CPU/platform.
pub const fn caps_for(cpu: Cpu) -> ScalerCaps {
    match cpu {
        Cpu::Gm965 => ScalerCaps {
            kind: Some(ScalerKind::GmchPanelFitterI965),
            implemented: true,
            single_global_scaler: true,
        },
        Cpu::G45 | Cpu::Gm45 => ScalerCaps {
            kind: Some(ScalerKind::GmchPanelFitterI965),
            implemented: false,
            single_global_scaler: true,
        },
        Cpu::Ironlake | Cpu::Sandybridge | Cpu::Ivybridge => ScalerCaps {
            kind: Some(ScalerKind::IronlakePanelFitter),
            implemented: false,
            single_global_scaler: false,
        },
        Cpu::Haswell | Cpu::Broadwell => ScalerCaps {
            // Haswell/Broadwell still use a PCH-style panel fitter in the
            // libgfxinit model. Do not group them with Skylake+ pipe scalers.
            kind: Some(ScalerKind::IronlakePanelFitter),
            implemented: false,
            single_global_scaler: false,
        },
        Cpu::Broxton | Cpu::Skylake | Cpu::Kabylake | Cpu::Tigerlake | Cpu::Alderlake => {
            ScalerCaps {
                kind: Some(ScalerKind::PipeScaler),
                implemented: false,
                single_global_scaler: false,
            }
        }
        Cpu::I945G | Cpu::I945GM | Cpu::Pineview | Cpu::PineviewM => ScalerCaps {
            kind: Some(ScalerKind::GmchPanelFitterPreI965),
            implemented: true,
            single_global_scaler: true,
        },
    }
}

/// Return whether the framebuffer active area differs from the mode active area.
pub const fn requires_scaling(surface: SurfaceConfig, mode: Mode) -> bool {
    surface.width != mode.hdisplay as u32 || surface.height != mode.vdisplay as u32
}

/// Classify source/destination aspect ratio like libgfxinit's `Scaling_Type`.
pub const fn scaling_aspect(
    width: u32,
    height: u32,
    scaled_width: u16,
    scaled_height: u16,
) -> ScalingAspect {
    let left = (scaled_width as u64) * (height as u64);
    let right = (scaled_height as u64) * (width as u64);
    if left < right {
        ScalingAspect::Letterbox
    } else if left > right {
        ScalingAspect::Pillarbox
    } else {
        ScalingAspect::Uniform
    }
}

/// Encode the i965+/G45 `PFIT_CONTROL` value used for GMCH panel fitting.
pub const fn encode_gmch_i965_control(
    pipe: Pipe,
    aspect: ScalingAspect,
    vga_plane_offset: bool,
) -> u32 {
    let scaling = if vga_plane_offset {
        PFIT_SCALING_AUTO
    } else {
        match aspect {
            ScalingAspect::Uniform => PFIT_SCALING_AUTO,
            ScalingAspect::Pillarbox => PFIT_SCALING_PILLAR,
            ScalingAspect::Letterbox => PFIT_SCALING_LETTER,
        }
    };
    PFIT_ENABLE | pfit_pipe(pipe) | scaling
}

/// Encode a conservative i965+/G45 GMCH panel-fitter plan.
pub const fn encode_gmch_i965(
    pipe: Pipe,
    source_width: u32,
    source_height: u32,
    target_width: u16,
    target_height: u16,
    policy: ScalingPolicy,
) -> Result<GmchPfitEncoding, GmaError> {
    if source_width == target_width as u32 && source_height == target_height as u32 {
        return Ok(GmchPfitEncoding::disabled());
    }
    if source_width > target_width as u32 || source_height > target_height as u32 {
        return Err(GmaError::InvalidConfig);
    }
    if matches!(policy, ScalingPolicy::None | ScalingPolicy::Center) {
        return Err(GmaError::InvalidConfig);
    }
    let aspect = scaling_aspect(source_width, source_height, target_width, target_height);
    Ok(GmchPfitEncoding {
        control: encode_gmch_i965_control(pipe, aspect, false),
        pgm_ratios: 0,
        lvds_border: matches!(policy, ScalingPolicy::PreserveAspect)
            && !matches!(aspect, ScalingAspect::Uniform),
    })
}

/// Encode the Linux pre-i965 panel-fitter programmed-scale ratio.
pub const fn panel_fitter_scaling(source: u32, target: u32) -> u32 {
    if target == 0 {
        return 0;
    }
    const FACTOR: u32 = 1 << 12;
    let ratio = source.saturating_mul(FACTOR) / target;
    (FACTOR.saturating_mul(ratio) + FACTOR / 2) / FACTOR
}

/// Encode pre-i965 `PFIT_PGM_RATIOS` horizontal/vertical fields.
pub const fn encode_gmch_pre_i965_ratios(horizontal: u32, vertical: u32) -> u32 {
    PFIT_PGM_RATIOS::PRE_I965_VERTICAL
        .val(vertical & 0x0fff)
        .value
        | PFIT_PGM_RATIOS::PRE_I965_HORIZONTAL
            .val(horizontal & 0x0fff)
            .value
}

/// Encode i965+ `PFIT_PGM_RATIOS` horizontal/vertical fields.
pub const fn encode_gmch_i965_ratios(horizontal: u32, vertical: u32) -> u32 {
    PFIT_PGM_RATIOS::I965_VERTICAL.val(vertical & 0x1fff).value
        | PFIT_PGM_RATIOS::I965_HORIZONTAL
            .val(horizontal & 0x1fff)
            .value
}

/// Encode a conservative pre-i965/Pineview GMCH panel-fitter plan.
///
/// Aspect-preserving pre-i965 programming is not just these ratio bits: future
/// hardware enablement also needs the adjusted/centered destination pipe mode
/// and border state to match the selected source/destination rectangle.
pub const fn encode_gmch_pre_i965(
    source_width: u32,
    source_height: u32,
    target_width: u16,
    target_height: u16,
    policy: ScalingPolicy,
    pipe_bpp: u8,
) -> GmchPfitEncoding {
    let aspect = scaling_aspect(source_width, source_height, target_width, target_height);
    let dither = if pipe_bpp == 18 {
        PFIT_PANEL_8TO6_DITHER_ENABLE
    } else {
        0
    };
    if source_width == target_width as u32 && source_height == target_height as u32 {
        return GmchPfitEncoding {
            control: dither,
            pgm_ratios: 0,
            lvds_border: false,
        };
    }
    match policy {
        ScalingPolicy::None | ScalingPolicy::Center => GmchPfitEncoding::disabled(),
        ScalingPolicy::Stretch => GmchPfitEncoding {
            control: PFIT_ENABLE
                | PFIT_VERT_AUTO_SCALE
                | PFIT_HORIZ_AUTO_SCALE
                | PFIT_VERT_INTERP_BILINEAR
                | PFIT_HORIZ_INTERP_BILINEAR
                | dither,
            pgm_ratios: 0,
            lvds_border: false,
        },
        ScalingPolicy::PreserveAspect => match aspect {
            ScalingAspect::Uniform => GmchPfitEncoding {
                control: PFIT_ENABLE
                    | PFIT_VERT_AUTO_SCALE
                    | PFIT_HORIZ_AUTO_SCALE
                    | PFIT_VERT_INTERP_BILINEAR
                    | PFIT_HORIZ_INTERP_BILINEAR
                    | dither,
                pgm_ratios: 0,
                lvds_border: false,
            },
            ScalingAspect::Pillarbox => {
                let bits = if source_height != target_height as u32 {
                    panel_fitter_scaling(source_height, target_height as u32)
                } else {
                    0
                };
                GmchPfitEncoding {
                    control: PFIT_ENABLE
                        | PFIT_VERT_INTERP_BILINEAR
                        | PFIT_HORIZ_INTERP_BILINEAR
                        | dither,
                    pgm_ratios: encode_gmch_pre_i965_ratios(bits, bits),
                    lvds_border: true,
                }
            }
            ScalingAspect::Letterbox => {
                let bits = if source_width != target_width as u32 {
                    panel_fitter_scaling(source_width, target_width as u32)
                } else {
                    0
                };
                GmchPfitEncoding {
                    control: PFIT_ENABLE
                        | PFIT_VERT_INTERP_BILINEAR
                        | PFIT_HORIZ_INTERP_BILINEAR
                        | dither,
                    pgm_ratios: encode_gmch_pre_i965_ratios(bits, bits),
                    lvds_border: true,
                }
            }
        },
    }
}

/// Encode an Ironlake/PCH-style window-positioned panel fitter.
pub const fn encode_ironlake_pfit(
    pipe: Pipe,
    source_width: u32,
    source_height: u32,
    target_width: u16,
    target_height: u16,
    policy: ScalingPolicy,
    has_pipe_select: bool,
) -> Result<IronlakePfitEncoding, GmaError> {
    // Linux permits limited downscale on some PCH/Ironlake-style fitters, but
    // the detailed generation-specific min/max ratios are not modeled in this
    // data-only encoder yet. Reject downscale here conservatively instead of
    // overclaiming a generic future-enable rule for all PCH fitters.
    if source_width > target_width as u32 || source_height > target_height as u32 {
        return Err(GmaError::InvalidConfig);
    }
    let rect = match plan_pch_destination_rect(
        source_width,
        source_height,
        target_width,
        target_height,
        policy,
    ) {
        Ok(rect) => rect,
        Err(err) => return Err(err),
    };

    Ok(IronlakePfitEncoding {
        control: PF_CTRL_ENABLE | pf_pipe_select(pipe, has_pipe_select) | PF_CTRL_FILTER_MED,
        win_pos: encode_window(rect.x, rect.y),
        win_size: encode_window(rect.width, rect.height),
    })
}

/// Resolve a generic destination rectangle without applying generation-specific limits.
///
/// The centering and odd-size behavior follows the Linux panel-fitter helpers:
/// centered/no-scale uses ceil-half offsets, and aspect scaling rounds odd
/// fitted dimensions up before centering.
pub const fn plan_destination_rect(
    source_width: u32,
    source_height: u32,
    target_width: u16,
    target_height: u16,
    policy: ScalingPolicy,
) -> Result<DestinationRect, GmaError> {
    if source_width == 0 || source_height == 0 || target_width == 0 || target_height == 0 {
        return Err(GmaError::InvalidConfig);
    }

    let target_width = target_width as u32;
    let target_height = target_height as u32;
    if source_width == target_width && source_height == target_height {
        return Ok(DestinationRect {
            x: 0,
            y: 0,
            width: target_width,
            height: target_height,
            kind: DestinationKind::Exact,
        });
    }

    match policy {
        ScalingPolicy::None => Err(GmaError::InvalidConfig),
        ScalingPolicy::Stretch => Ok(DestinationRect {
            x: 0,
            y: 0,
            width: target_width,
            height: target_height,
            kind: DestinationKind::Stretch,
        }),
        ScalingPolicy::Center => {
            if source_width > target_width || source_height > target_height {
                return Err(GmaError::InvalidConfig);
            }
            Ok(DestinationRect {
                x: ceil_half(target_width - source_width),
                y: ceil_half(target_height - source_height),
                width: source_width,
                height: source_height,
                kind: DestinationKind::Centered,
            })
        }
        ScalingPolicy::PreserveAspect => {
            let aspect = scaling_aspect(
                source_width,
                source_height,
                target_width as u16,
                target_height as u16,
            );
            let (mut width, mut height) =
                match scale_keep_aspect(source_width, source_height, target_width, target_height) {
                    Ok(size) => size,
                    Err(err) => return Err(err),
                };
            if width < target_width && width % 2 == 1 {
                width += 1;
            }
            if height < target_height && height % 2 == 1 {
                height += 1;
            }
            Ok(DestinationRect {
                x: ceil_half(target_width.saturating_sub(width)),
                y: ceil_half(target_height.saturating_sub(height)),
                width,
                height,
                kind: match aspect {
                    ScalingAspect::Uniform => DestinationKind::Stretch,
                    ScalingAspect::Letterbox => DestinationKind::Letterbox,
                    ScalingAspect::Pillarbox => DestinationKind::Pillarbox,
                },
            })
        }
    }
}

/// Resolve the Ironlake/PCH-style destination rectangle with hardware quirks applied.
pub const fn plan_pch_destination_rect(
    source_width: u32,
    source_height: u32,
    target_width: u16,
    target_height: u16,
    policy: ScalingPolicy,
) -> Result<DestinationRect, GmaError> {
    let mut rect = match plan_destination_rect(
        source_width,
        source_height,
        target_width,
        target_height,
        policy,
    ) {
        Ok(rect) => rect,
        Err(err) => return Err(err),
    };
    if matches!(rect.kind, DestinationKind::Exact) {
        return Ok(rect);
    }

    let target_width = target_width as u32;
    let target_height = target_height as u32;
    let horizontal_gap = target_width.saturating_sub(rect.width);
    if matches!(policy, ScalingPolicy::PreserveAspect) && horizontal_gap > 0 && horizontal_gap <= 3
    {
        rect.x = 0;
        rect.width = target_width;
        if rect.height == target_height {
            rect.kind = DestinationKind::Stretch;
        }
    }

    if let Err(err) = validate_pch_destination_rect(rect, target_width, target_height) {
        return Err(err);
    }
    Ok(rect)
}

const fn validate_pch_destination_rect(
    rect: DestinationRect,
    target_width: u32,
    target_height: u32,
) -> Result<(), GmaError> {
    // Linux explicitly rejects `x == 1` for PCH panel-fitter windows. Keep
    // the same minimum-position restriction symmetrical for `y == 1` until
    // hardware validation proves that one-line vertical centering is safe.
    if rect.x == 1 || rect.y == 1 {
        return Err(GmaError::InvalidConfig);
    }
    if rect.x.saturating_mul(2).saturating_add(rect.width) != target_width {
        return Err(GmaError::InvalidConfig);
    }
    if rect.y.saturating_mul(2).saturating_add(rect.height) != target_height {
        return Err(GmaError::InvalidConfig);
    }
    Ok(())
}

const fn validate_destination_for_kind(
    kind: Option<ScalerKind>,
    source_width: u32,
    source_height: u32,
    rect: DestinationRect,
) -> Result<(), GmaError> {
    let Some(kind) = kind else {
        return Err(GmaError::UnsupportedPlatform);
    };
    if matches!(
        kind,
        ScalerKind::GmchPanelFitterPreI965 | ScalerKind::GmchPanelFitterI965
    ) && (source_width > rect.width || source_height > rect.height)
    {
        return Err(GmaError::InvalidConfig);
    }
    Ok(())
}

/// Half the gap, rounded up.
///
/// Note: libgfxinit's `Align_Framebuffer` uses `Gap / 2` (floor), while the
/// Linux i915 PFIT paths the crate models round the odd remainder up. The
/// Linux behaviour is kept deliberately; see the scaler tests.
const fn ceil_half(value: u32) -> u32 {
    value.div_ceil(2)
}

const fn scale_keep_aspect(
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> Result<(u32, u32), GmaError> {
    let left = (target_width as u64) * (source_height as u64);
    let right = (target_height as u64) * (source_width as u64);
    if left > right {
        let width = (target_height as u64) * (source_width as u64) / (source_height as u64);
        if width > u32::MAX as u64 {
            return Err(GmaError::InvalidConfig);
        }
        Ok((width as u32, target_height))
    } else if left < right {
        let height = (target_width as u64) * (source_height as u64) / (source_width as u64);
        if height > u32::MAX as u64 {
            return Err(GmaError::InvalidConfig);
        }
        Ok((target_width, height as u32))
    } else {
        Ok((target_width, target_height))
    }
}

const fn pfit_pipe(pipe: Pipe) -> u32 {
    match pipe {
        Pipe::A => PFIT_CONTROL::PIPE_SELECT::PipeA.value,
        Pipe::B => PFIT_CONTROL::PIPE_SELECT::PipeB.value,
        Pipe::C => PFIT_CONTROL::PIPE_SELECT::PipeC.value,
    }
}

const fn pf_pipe_select(pipe: Pipe, has_pipe_select: bool) -> u32 {
    if !has_pipe_select {
        return 0;
    }
    match pipe {
        Pipe::A => PF_CTL::PIPE_SELECT::PipeA.value,
        Pipe::B => PF_CTL::PIPE_SELECT::PipeB.value,
        Pipe::C => PF_CTL::PIPE_SELECT::PipeC.value,
    }
}

const fn encode_window(width: u32, height: u32) -> u32 {
    PF_WIN::X_OR_WIDTH.val(width & 0xffff).value | PF_WIN::Y_OR_HEIGHT.val(height & 0xffff).value
}

const PFIT_ENABLE: u32 = PFIT_CONTROL::ENABLE::SET.value;
const PFIT_SCALING_AUTO: u32 = PFIT_CONTROL::SCALING_MODE::Auto.value;
const PFIT_SCALING_PILLAR: u32 = PFIT_CONTROL::SCALING_MODE::Pillarbox.value;
const PFIT_SCALING_LETTER: u32 = PFIT_CONTROL::SCALING_MODE::Letterbox.value;
const PFIT_VERT_INTERP_BILINEAR: u32 = PFIT_CONTROL::VERT_INTERP_BILINEAR::SET.value;
const PFIT_VERT_AUTO_SCALE: u32 = PFIT_CONTROL::VERT_AUTO_SCALE::SET.value;
const PFIT_HORIZ_INTERP_BILINEAR: u32 = PFIT_CONTROL::HORIZ_INTERP_BILINEAR::SET.value;
const PFIT_HORIZ_AUTO_SCALE: u32 = PFIT_CONTROL::HORIZ_AUTO_SCALE::SET.value;
const PFIT_PANEL_8TO6_DITHER_ENABLE: u32 = PFIT_CONTROL::PANEL_8TO6_DITHER_ENABLE::SET.value;
const PF_CTRL_ENABLE: u32 = PF_CTL::ENABLE::SET.value;
const PF_CTRL_FILTER_MED: u32 = PF_CTL::FILTER_MED::SET.value;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framebuffer::PixelFormat;
    use crate::mode::Mode;
    use crate::types::PhysAddr;

    #[test]
    fn classifies_scaling_aspect_like_libgfxinit() {
        assert_eq!(scaling_aspect(1024, 768, 1024, 768), ScalingAspect::Uniform);
        assert_eq!(scaling_aspect(800, 600, 1024, 768), ScalingAspect::Uniform);
        assert_eq!(
            scaling_aspect(800, 600, 1280, 720),
            ScalingAspect::Pillarbox
        );
        assert_eq!(
            scaling_aspect(1280, 720, 1024, 768),
            ScalingAspect::Letterbox
        );
    }

    #[test]
    fn exact_size_plan_is_accepted_without_programming_scaler() {
        let surface =
            SurfaceConfig::packed(PhysAddr(0xd000_0000), 1024, 768, PixelFormat::Xrgb8888);
        let plan = ScalerPlan::resolve(
            Cpu::Gm965,
            Pipe::B,
            surface,
            Mode::XGA_1024X768_60,
            ScalingPolicy::None,
        );
        assert!(!plan.requires_scaling);
        assert_eq!(plan.kind, Some(ScalerKind::GmchPanelFitterI965));
        assert_eq!(plan.validate_current(), Ok(()));
    }

    #[test]
    fn gmch_upscale_is_accepted_when_scaling_policy_is_set() {
        let surface = SurfaceConfig::packed(PhysAddr(0xd000_0000), 800, 600, PixelFormat::Xrgb8888);
        let gm965 = ScalerPlan::resolve(
            Cpu::Gm965,
            Pipe::B,
            surface,
            Mode::XGA_1024X768_60,
            ScalingPolicy::PreserveAspect,
        );
        assert!(gm965.requires_scaling);
        assert_eq!(gm965.kind, Some(ScalerKind::GmchPanelFitterI965));
        assert_eq!(gm965.validate_current(), Ok(()));

        let pineview = ScalerPlan::resolve(
            Cpu::Pineview,
            Pipe::A,
            surface,
            Mode::XGA_1024X768_60,
            ScalingPolicy::Stretch,
        );
        assert!(pineview.requires_scaling);
        assert_eq!(pineview.kind, Some(ScalerKind::GmchPanelFitterPreI965));
        assert_eq!(pineview.validate_current(), Ok(()));
    }

    #[test]
    fn future_validation_rejects_gmch_downscale_and_no_policy() {
        let gmch_downscale = ScalerPlan::resolve(
            Cpu::Gm965,
            Pipe::B,
            SurfaceConfig::packed(PhysAddr(0xd000_0000), 1280, 800, PixelFormat::Xrgb8888),
            Mode::XGA_1024X768_60,
            ScalingPolicy::PreserveAspect,
        );
        assert_eq!(
            gmch_downscale.validate_future_enablement(),
            Err(GmaError::InvalidConfig)
        );

        let pch_downscale = ScalerPlan::resolve(
            Cpu::Ironlake,
            Pipe::B,
            SurfaceConfig::packed(PhysAddr(0xd000_0000), 1280, 800, PixelFormat::Xrgb8888),
            Mode::XGA_1024X768_60,
            ScalingPolicy::PreserveAspect,
        );
        assert_eq!(pch_downscale.validate_future_enablement(), Ok(()));

        let no_policy = ScalerPlan::resolve(
            Cpu::Gm965,
            Pipe::B,
            SurfaceConfig::packed(PhysAddr(0xd000_0000), 800, 600, PixelFormat::Xrgb8888),
            Mode::XGA_1024X768_60,
            ScalingPolicy::None,
        );
        assert_eq!(
            no_policy.validate_future_enablement(),
            Err(GmaError::InvalidConfig)
        );
    }

    #[test]
    fn destination_rect_plans_stretch_preserve_aspect_and_centering() {
        assert_eq!(
            plan_destination_rect(800, 600, 1024, 768, ScalingPolicy::Stretch),
            Ok(DestinationRect {
                x: 0,
                y: 0,
                width: 1024,
                height: 768,
                kind: DestinationKind::Stretch,
            })
        );
        assert_eq!(
            plan_destination_rect(800, 600, 1280, 720, ScalingPolicy::PreserveAspect),
            Ok(DestinationRect {
                x: 160,
                y: 0,
                width: 960,
                height: 720,
                kind: DestinationKind::Pillarbox,
            })
        );
        assert_eq!(
            plan_destination_rect(1280, 720, 1024, 768, ScalingPolicy::PreserveAspect),
            Ok(DestinationRect {
                x: 0,
                y: 96,
                width: 1024,
                height: 576,
                kind: DestinationKind::Letterbox,
            })
        );
        assert_eq!(
            plan_destination_rect(800, 600, 1024, 768, ScalingPolicy::Center),
            Ok(DestinationRect {
                x: 112,
                y: 84,
                width: 800,
                height: 600,
                kind: DestinationKind::Centered,
            })
        );
    }

    #[test]
    fn destination_rect_rounds_odd_dimensions_like_linux_pfit() {
        assert_eq!(
            plan_destination_rect(800, 600, 1025, 768, ScalingPolicy::Center),
            Ok(DestinationRect {
                x: 113,
                y: 84,
                width: 800,
                height: 600,
                kind: DestinationKind::Centered,
            })
        );
        assert_eq!(
            plan_destination_rect(853, 480, 1024, 768, ScalingPolicy::PreserveAspect),
            Ok(DestinationRect {
                x: 0,
                y: 96,
                width: 1024,
                height: 576,
                kind: DestinationKind::Letterbox,
            })
        );
        assert_eq!(
            plan_destination_rect(700, 500, 1024, 768, ScalingPolicy::PreserveAspect),
            Ok(DestinationRect {
                x: 0,
                y: 18,
                width: 1024,
                height: 732,
                kind: DestinationKind::Letterbox,
            })
        );
    }

    #[test]
    fn destination_rect_rejects_unsupported_no_scale_cases() {
        assert_eq!(
            plan_destination_rect(800, 600, 1024, 768, ScalingPolicy::None),
            Err(GmaError::InvalidConfig)
        );
        assert_eq!(
            plan_destination_rect(1280, 800, 1024, 768, ScalingPolicy::Center),
            Err(GmaError::InvalidConfig)
        );
        assert_eq!(
            plan_destination_rect(0, 600, 1024, 768, ScalingPolicy::Stretch),
            Err(GmaError::InvalidConfig)
        );
    }

    #[test]
    fn scale_keep_aspect_uses_wide_intermediates_for_large_dimensions() {
        assert_eq!(
            scale_keep_aspect(u32::MAX, u32::MAX - 1, u32::MAX, u32::MAX - 2),
            Ok((u32::MAX - 2, u32::MAX - 2))
        );
    }

    #[test]
    fn constrained_destination_rect_rejects_gmch_downscale_but_models_pch() {
        let gmch_downscale = ScalerPlan::resolve(
            Cpu::Gm965,
            Pipe::B,
            SurfaceConfig::packed(PhysAddr(0xd000_0000), 1280, 720, PixelFormat::Xrgb8888),
            Mode::XGA_1024X768_60,
            ScalingPolicy::PreserveAspect,
        );
        assert_eq!(
            gmch_downscale.constrained_destination_rect(),
            Err(GmaError::InvalidConfig)
        );

        let pch_upscale = ScalerPlan::resolve(
            Cpu::Ironlake,
            Pipe::B,
            SurfaceConfig::packed(PhysAddr(0xd000_0000), 800, 600, PixelFormat::Xrgb8888),
            Mode::XGA_1024X768_60,
            ScalingPolicy::PreserveAspect,
        );
        assert_eq!(
            pch_upscale.constrained_destination_rect(),
            Ok(DestinationRect {
                x: 0,
                y: 0,
                width: 1024,
                height: 768,
                kind: DestinationKind::Stretch,
            })
        );
    }

    #[test]
    fn global_gmch_panel_fitter_reservation_is_pipe_owned() {
        let surface = SurfaceConfig::packed(PhysAddr(0xd000_0000), 800, 600, PixelFormat::Xrgb8888);
        let plan = ScalerPlan::resolve(
            Cpu::Gm965,
            Pipe::B,
            surface,
            Mode::XGA_1024X768_60,
            ScalingPolicy::PreserveAspect,
        );
        assert!(plan.can_reserve_global(None));
        assert!(plan.can_reserve_global(Some(Pipe::B)));
        assert!(!plan.can_reserve_global(Some(Pipe::A)));
    }

    #[test]
    fn scaler_caps_match_libgfxinit_families_and_current_enablement() {
        let pineview = caps_for(Cpu::Pineview);
        assert_eq!(pineview.kind, Some(ScalerKind::GmchPanelFitterPreI965));
        assert!(pineview.implemented);
        assert!(pineview.single_global_scaler);

        let gm965 = caps_for(Cpu::Gm965);
        assert_eq!(gm965.kind, Some(ScalerKind::GmchPanelFitterI965));
        assert!(gm965.implemented);
        assert!(gm965.single_global_scaler);

        let haswell = caps_for(Cpu::Haswell);
        assert_eq!(haswell.kind, Some(ScalerKind::IronlakePanelFitter));
        assert!(!haswell.implemented);
        assert!(!haswell.single_global_scaler);

        let broadwell = caps_for(Cpu::Broadwell);
        assert_eq!(broadwell.kind, Some(ScalerKind::IronlakePanelFitter));
        assert!(!broadwell.implemented);
        assert!(!broadwell.single_global_scaler);

        let broxton = caps_for(Cpu::Broxton);
        assert_eq!(broxton.kind, Some(ScalerKind::PipeScaler));
        assert!(!broxton.implemented);
        assert!(!broxton.single_global_scaler);

        let skylake = caps_for(Cpu::Skylake);
        assert_eq!(skylake.kind, Some(ScalerKind::PipeScaler));
        assert!(!skylake.implemented);
        assert!(!skylake.single_global_scaler);
    }

    #[test]
    fn gmch_register_offsets_match_linux_and_libgfxinit() {
        assert_eq!(GmchPfitRegs::DEFAULT.control, 0x61230);
        assert_eq!(GmchPfitRegs::DEFAULT.pgm_ratios, 0x61234);
        assert_eq!(GmchPfitRegs::DEFAULT.auto_ratios, 0x61238);
    }

    #[test]
    fn ironlake_pfit_pipe_b_and_c_offsets_are_distinct() {
        let pipe_b = IronlakePfitRegs::for_pipe(Pipe::B);
        assert_eq!(pipe_b.win_pos, 0x68870);
        assert_eq!(pipe_b.win_size, 0x68874);
        assert_eq!(pipe_b.control, 0x68880);

        let pipe_c = IronlakePfitRegs::for_pipe(Pipe::C);
        assert_eq!(pipe_c.win_pos, 0x69070);
        assert_eq!(pipe_c.win_size, 0x69074);
        assert_eq!(pipe_c.control, 0x69080);
    }

    #[test]
    fn i965_gmch_control_encodes_pipe_and_aspect_bits() {
        assert_eq!(
            encode_gmch_i965_control(Pipe::B, ScalingAspect::Pillarbox, false),
            (1 << 31) | (1 << 29) | (2 << 26)
        );
        assert_eq!(
            encode_gmch_i965_control(Pipe::A, ScalingAspect::Letterbox, false),
            (1 << 31) | (3 << 26)
        );
        assert_eq!(
            encode_gmch_i965_control(Pipe::B, ScalingAspect::Pillarbox, true),
            (1 << 31) | (1 << 29)
        );
    }

    #[test]
    fn gmch_ratio_helpers_match_linux_bit_layouts() {
        assert_eq!(panel_fitter_scaling(800, 1024), 0x0c80);
        assert_eq!(encode_gmch_pre_i965_ratios(0x0c80, 0x0c80), 0xc80_0c800);
        assert_eq!(encode_gmch_i965_ratios(0x1000, 0x1000), 0x1000_1000);
    }

    #[test]
    fn pre_i965_stretch_encoding_uses_auto_scale_and_bilinear_bits() {
        let encoding = encode_gmch_pre_i965(800, 600, 1024, 768, ScalingPolicy::Stretch, 18);
        assert_eq!(encoding.control & (1 << 31), 1 << 31);
        assert_ne!(encoding.control & (1 << 9), 0);
        assert_ne!(encoding.control & (1 << 5), 0);
        assert_ne!(encoding.control & (1 << 10), 0);
        assert_ne!(encoding.control & (1 << 6), 0);
        assert_ne!(encoding.control & (1 << 3), 0);
        assert_eq!(encoding.pgm_ratios, 0);
    }

    #[test]
    fn pch_destination_rect_applies_linux_minimal_gap_quirk() {
        assert_eq!(
            plan_destination_rect(800, 600, 1025, 768, ScalingPolicy::PreserveAspect),
            Ok(DestinationRect {
                x: 1,
                y: 0,
                width: 1024,
                height: 768,
                kind: DestinationKind::Pillarbox,
            })
        );
        assert_eq!(
            plan_pch_destination_rect(800, 600, 1025, 768, ScalingPolicy::PreserveAspect),
            Ok(DestinationRect {
                x: 0,
                y: 0,
                width: 1025,
                height: 768,
                kind: DestinationKind::Stretch,
            })
        );
    }

    #[test]
    fn pch_destination_rect_preserves_exact_and_rejects_bad_centering() {
        assert_eq!(
            plan_pch_destination_rect(1024, 768, 1024, 768, ScalingPolicy::Center),
            Ok(DestinationRect {
                x: 0,
                y: 0,
                width: 1024,
                height: 768,
                kind: DestinationKind::Exact,
            })
        );
        assert_eq!(
            plan_pch_destination_rect(1020, 760, 1024, 768, ScalingPolicy::Center),
            Ok(DestinationRect {
                x: 2,
                y: 4,
                width: 1020,
                height: 760,
                kind: DestinationKind::Centered,
            })
        );
        assert_eq!(
            plan_pch_destination_rect(1021, 768, 1024, 768, ScalingPolicy::Center),
            Err(GmaError::InvalidConfig)
        );
        assert_eq!(
            plan_pch_destination_rect(1022, 768, 1024, 768, ScalingPolicy::Center),
            Err(GmaError::InvalidConfig)
        );
        assert_eq!(
            plan_pch_destination_rect(1024, 765, 1024, 768, ScalingPolicy::Center),
            Err(GmaError::InvalidConfig)
        );
        assert_eq!(
            plan_pch_destination_rect(1024, 766, 1024, 768, ScalingPolicy::Center),
            Err(GmaError::InvalidConfig)
        );
    }

    #[test]
    fn constrained_destination_rect_routes_ironlake_through_pch_rules() {
        let plan = ScalerPlan::resolve(
            Cpu::Ironlake,
            Pipe::B,
            SurfaceConfig::packed(PhysAddr(0xd000_0000), 1021, 768, PixelFormat::Xrgb8888),
            Mode::XGA_1024X768_60,
            ScalingPolicy::Center,
        );
        assert_eq!(
            plan.destination_rect(),
            Ok(DestinationRect {
                x: 2,
                y: 0,
                width: 1021,
                height: 768,
                kind: DestinationKind::Centered,
            })
        );
        assert_eq!(
            plan.constrained_destination_rect(),
            Err(GmaError::InvalidConfig)
        );
    }

    #[test]
    fn ironlake_pfit_window_model_matches_libgfxinit_centering() {
        let encoding = encode_ironlake_pfit(
            Pipe::B,
            800,
            600,
            1024,
            768,
            ScalingPolicy::PreserveAspect,
            true,
        )
        .unwrap();
        assert_eq!(encoding.control, (1 << 31) | (1 << 29) | (1 << 23));
        assert_eq!(encoding.win_pos, 0);
        assert_eq!(encoding.win_size, (1024 << 16) | 768);

        let pillar = encode_ironlake_pfit(
            Pipe::A,
            800,
            600,
            1280,
            720,
            ScalingPolicy::PreserveAspect,
            false,
        )
        .unwrap();
        assert_eq!(pillar.win_pos, (160 << 16));
        assert_eq!(pillar.win_size, (960 << 16) | 720);
    }

    #[test]
    fn ironlake_encoder_rejects_downscale_until_limits_are_modeled() {
        assert_eq!(
            encode_ironlake_pfit(
                Pipe::B,
                1280,
                800,
                1024,
                768,
                ScalingPolicy::PreserveAspect,
                true,
            ),
            Err(GmaError::InvalidConfig)
        );
    }
}
