//! Tigerlake/Alderlake Intel GMA generation support.
//!
//! libgfxinit's Tigerlake backend is intentionally skeletal today: PLL
//! allocation succeeds with `Invalid_PLL`, register hints are zero, power/CDCLK
//! routines are no-ops, connector `Pre_On` succeeds without register writes,
//! connector `Post_On` turns panel backlight on, and hotplug detection always
//! reports false. This module captures that behavior as explicit compatibility
//! plans for Tigerlake/Type-C routing.

use crate::error::GmaError;
use crate::generation::{GenerationOps, sealed};
use crate::gtt;
use crate::mode::Mode;
use crate::regs::{TGL_DPLL_SELECT, TGL_HOTPLUG_STATUS, TGL_TYPEC_ORIENTATION};
use crate::types::{Cpu, Generation, Pipe, Port};

/// Tigerlake generation marker.
pub struct Tigerlake;

impl sealed::Sealed for Tigerlake {}

impl GenerationOps for Tigerlake {
    const GENERATION: Generation = Generation::Tigerlake;

    fn init_display(ctx: &mut crate::GmaContext<'_>, _mode: Mode) -> Result<(), GmaError> {
        map_gtt(ctx)?;
        // SAFETY: the selected surface is backed by the just-programmed GTT mapping.
        unsafe { ctx.surface.fill_bringup_pattern()? };
        let port = selected_port(ctx)?;
        let pipe = ddi_pipe_for_port(port);
        let pll = tgl_alloc_pll_plan(port);
        let _power = tgl_power_clock_plan(TglPowerClockStep::Initialize);
        let _pre = tgl_ddi_pre_on_plan(
            ctx.config.cpu,
            port,
            pipe,
            pll.register_value,
            TglTypeCOrientation::None,
        )?;
        let _post = tgl_ddi_post_on_plan(port, pipe, pll.register_value)?;
        let _hotplug = tgl_hotplug_detect_plan(port)?;
        let mmio = ctx.mmio();
        mmio.posting_read(0);
        Ok(())
    }
}

fn selected_port(ctx: &crate::GmaContext<'_>) -> Result<Port, GmaError> {
    crate::selected_enabled_port(ctx.config.outputs)
}

fn ddi_pipe_for_port(port: Port) -> Pipe {
    match port {
        Port::Edp | Port::DpA | Port::HdmiA => Pipe::A,
        Port::DpB | Port::HdmiB => Pipe::B,
        _ => Pipe::C,
    }
}

fn map_gtt(ctx: &crate::GmaContext<'_>) -> Result<(), GmaError> {
    gtt::map_surface_to_stolen(ctx.resources, &ctx.surface)?;
    gtt::flush_gfx(&ctx.mmio());
    Ok(())
}

/// Tigerlake PLL selector order from libgfxinit's `PLLs.T`.
///
/// Most variants are plan/test-only until the Tigerlake live DPLL allocator is
/// wired beyond the current libgfxinit-compatible no-op allocation stub.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TglPll {
    /// No PLL selected.
    Invalid,
    /// DPLL0.
    Dpll0,
    /// DPLL1.
    Dpll1,
    /// DPLL4, ordered before DPLL2 in libgfxinit.
    Dpll4,
    /// DPLL2.
    Dpll2,
}

/// Configurable Tigerlake DPLL set modeled by libgfxinit.
///
/// Retained for plan parity tests until the live Tigerlake allocator consumes it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const TGL_CONFIGURABLE_PLLS: &[TglPll] = &[TglPll::Dpll0, TglPll::Dpll1, TglPll::Dpll4];

/// Data-only result of libgfxinit Tigerlake PLL allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TglPllAllocPlan {
    /// PLL returned by `Alloc`.
    pub pll: TglPll,
    /// Success flag returned by `Alloc`.
    pub success: bool,
    /// Register hint returned by `Register_Value`.
    pub register_value: u32,
}

/// Return libgfxinit's current Tigerlake PLL allocation result.
pub(crate) const fn tgl_alloc_pll_plan(_port: Port) -> TglPllAllocPlan {
    TglPllAllocPlan {
        pll: TglPll::Invalid,
        success: true,
        register_value: tgl_pll_register_value(TglPll::Invalid),
    }
}

/// Return libgfxinit's current Tigerlake PLL register hint.
pub(crate) const fn tgl_pll_register_value(_pll: TglPll) -> u32 {
    TGL_DPLL_SELECT::REGISTER_VALUE.val(0).value
}

/// Tigerlake power/CDCLK operations matching libgfxinit routine names.
///
/// Only `Initialize` is reached by today's live skeleton; the remaining variants
/// are covered by tests as staged libgfxinit parity markers.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TglPowerClockStep {
    /// `Pre_All_Off`.
    PreAllOff,
    /// `Post_All_Off`.
    PostAllOff,
    /// `Initialize`.
    Initialize,
    /// `Limit_Dotclocks`.
    LimitDotclocks,
    /// `Update_CDClk`.
    UpdateCdclk,
    /// `Power_Set_To`.
    PowerSetTo,
    /// `Power_Up`.
    PowerUp,
    /// `Power_Down`.
    PowerDown,
}

/// Data-only Tigerlake power/CDCLK plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TglPowerClockPlan {
    /// Operation being modeled.
    pub step: TglPowerClockStep,
    /// Whether libgfxinit requests a CDCLK switch from `Limit_Dotclocks`.
    pub cdclk_switch: bool,
    /// Number of register writes in the current libgfxinit implementation.
    pub register_writes: u8,
}

/// Return a libgfxinit-compatible Tigerlake power/CDCLK no-op plan.
pub(crate) const fn tgl_power_clock_plan(step: TglPowerClockStep) -> TglPowerClockPlan {
    TglPowerClockPlan {
        step,
        cdclk_switch: false,
        register_writes: 0,
    }
}

/// Type-C/DDI orientation for future Tigerlake Type-C routing.
///
/// The live path currently passes `None`; concrete orientations are test/staged
/// coverage for the future Type-C routing implementation.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TglTypeCOrientation {
    /// Native/no Type-C retimer orientation selected.
    None,
    /// Normal Type-C lane orientation.
    Normal,
    /// Reversed Type-C lane orientation.
    Reversed,
}

/// Data-only Tigerlake DDI/Type-C connector pre-on plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TglDdiPreOnPlan {
    /// CPU/platform family.
    pub cpu: Cpu,
    /// Logical output port.
    pub port: Port,
    /// Pipe selected for the connector.
    pub pipe: Pipe,
    /// PLL hint passed into libgfxinit connector `Pre_On`.
    pub pll_hint: u32,
    /// Type-C orientation metadata; current libgfxinit does not consume it.
    pub type_c_orientation: TglTypeCOrientation,
    /// Typed register encoding for the Type-C orientation metadata.
    pub type_c_orientation_value: u32,
    /// Success flag returned by libgfxinit `Pre_On`.
    pub success: bool,
    /// Number of register writes in current libgfxinit `Pre_On`.
    pub register_writes: u8,
}

/// Data-only Tigerlake connector post-on plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TglDdiPostOnPlan {
    /// Logical output port.
    pub port: Port,
    /// Pipe selected for the connector.
    pub pipe: Pipe,
    /// PLL hint passed into libgfxinit connector `Post_On`.
    pub pll_hint: u32,
    /// Panel backlight is enabled by current libgfxinit `Post_On`.
    pub panel_backlight_on: bool,
    /// Success flag returned by libgfxinit `Post_On`.
    pub success: bool,
}

/// Build a Tigerlake DDI/Type-C pre-on plan matching current libgfxinit.
pub(crate) const fn tgl_ddi_pre_on_plan(
    cpu: Cpu,
    port: Port,
    pipe: Pipe,
    pll_hint: u32,
    type_c_orientation: TglTypeCOrientation,
) -> Result<TglDdiPreOnPlan, GmaError> {
    if !matches!(cpu, Cpu::Tigerlake | Cpu::Alderlake) {
        return Err(GmaError::UnsupportedPlatform);
    }
    if !is_tgl_ddi_port(port) {
        return Err(GmaError::UnsupportedPort);
    }
    Ok(TglDdiPreOnPlan {
        cpu,
        port,
        pipe,
        pll_hint,
        type_c_orientation,
        type_c_orientation_value: tgl_typec_orientation_value(type_c_orientation),
        success: true,
        register_writes: 0,
    })
}

/// Build a Tigerlake DDI/Type-C post-on plan matching current libgfxinit.
pub(crate) const fn tgl_ddi_post_on_plan(
    port: Port,
    pipe: Pipe,
    pll_hint: u32,
) -> Result<TglDdiPostOnPlan, GmaError> {
    if !is_tgl_ddi_port(port) {
        return Err(GmaError::UnsupportedPort);
    }
    Ok(TglDdiPostOnPlan {
        port,
        pipe,
        pll_hint,
        panel_backlight_on: true,
        success: true,
    })
}

/// Tigerlake connector off operation modeled from libgfxinit.
///
/// Currently used by parity tests; live Tigerlake off sequencing is not wired yet.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TglDdiOffPlan {
    /// Logical output port.
    pub port: Port,
    /// Whether panel backlight is disabled in `Pre_Off`/`Pre_All_Off`.
    pub panel_backlight_off: bool,
    /// Whether panel power is disabled in `Pre_Off`/`Pre_All_Off`.
    pub panel_off: bool,
    /// Number of register writes in current connector off path.
    pub register_writes: u8,
}

/// Build a Tigerlake DDI connector off plan matching current libgfxinit.
///
/// Currently used by parity tests; live Tigerlake off sequencing is not wired yet.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const fn tgl_ddi_pre_off_plan(port: Port) -> Result<TglDdiOffPlan, GmaError> {
    if !is_tgl_ddi_port(port) {
        return Err(GmaError::UnsupportedPort);
    }
    Ok(TglDdiOffPlan {
        port,
        panel_backlight_off: true,
        panel_off: true,
        register_writes: 0,
    })
}

/// Data-only Tigerlake hotplug result matching current libgfxinit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TglHotplugPlan {
    /// Logical output port.
    pub port: Port,
    /// Detection state returned by `Hotplug_Detect`.
    pub detected: bool,
    /// Typed hotplug status bits observed by the current stub.
    pub status_bits: u32,
    /// Whether `Clear_Hotplug_Detect` writes any registers today.
    pub clear_register_writes: u8,
}

/// Return current Tigerlake hotplug behavior from libgfxinit.
pub(crate) const fn tgl_hotplug_detect_plan(port: Port) -> Result<TglHotplugPlan, GmaError> {
    if !is_tgl_ddi_port(port) {
        return Err(GmaError::UnsupportedPort);
    }
    let status_bits = TGL_HOTPLUG_STATUS::DETECTED::CLEAR.value;
    Ok(TglHotplugPlan {
        port,
        detected: tgl_hotplug_detected(status_bits),
        status_bits,
        clear_register_writes: 0,
    })
}

const TGL_HOTPLUG_DETECTED: u32 = TGL_HOTPLUG_STATUS::DETECTED::SET.value;

const fn tgl_hotplug_detected(status_bits: u32) -> bool {
    (status_bits & TGL_HOTPLUG_DETECTED) != 0
}

const fn tgl_typec_orientation_value(orientation: TglTypeCOrientation) -> u32 {
    match orientation {
        TglTypeCOrientation::None => TGL_TYPEC_ORIENTATION::VALUE::None.value,
        TglTypeCOrientation::Normal => TGL_TYPEC_ORIENTATION::VALUE::Normal.value,
        TglTypeCOrientation::Reversed => TGL_TYPEC_ORIENTATION::VALUE::Reversed.value,
    }
}

const fn is_tgl_ddi_port(port: Port) -> bool {
    matches!(
        port,
        Port::Edp
            | Port::HdmiA
            | Port::HdmiB
            | Port::HdmiC
            | Port::DpA
            | Port::DpB
            | Port::DpC
            | Port::DpD
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tigerlake_pll_order_and_stub_allocation_match_libgfxinit() {
        assert_eq!(
            TGL_CONFIGURABLE_PLLS,
            &[TglPll::Dpll0, TglPll::Dpll1, TglPll::Dpll4]
        );
        let plan = tgl_alloc_pll_plan(Port::DpA);
        assert_eq!(plan.pll, TglPll::Invalid);
        assert!(plan.success);
        assert_eq!(
            plan.register_value,
            TGL_DPLL_SELECT::REGISTER_VALUE.val(0).value
        );
        assert_eq!(tgl_pll_register_value(TglPll::Dpll2), 0);
    }

    #[test]
    fn tigerlake_power_clock_steps_are_explicit_noops() {
        for step in [
            TglPowerClockStep::PreAllOff,
            TglPowerClockStep::PostAllOff,
            TglPowerClockStep::Initialize,
            TglPowerClockStep::LimitDotclocks,
            TglPowerClockStep::UpdateCdclk,
            TglPowerClockStep::PowerSetTo,
            TglPowerClockStep::PowerUp,
            TglPowerClockStep::PowerDown,
        ] {
            let plan = tgl_power_clock_plan(step);
            assert_eq!(plan.step, step);
            assert!(!plan.cdclk_switch);
            assert_eq!(plan.register_writes, 0);
        }
    }

    #[test]
    fn tigerlake_ddi_typec_pre_and_post_on_match_libgfxinit() {
        let pre = tgl_ddi_pre_on_plan(
            Cpu::Tigerlake,
            Port::DpD,
            Pipe::C,
            0x1234,
            TglTypeCOrientation::Reversed,
        )
        .unwrap();
        assert_eq!(pre.port, Port::DpD);
        assert_eq!(pre.pipe, Pipe::C);
        assert_eq!(pre.pll_hint, 0x1234);
        assert_eq!(pre.type_c_orientation, TglTypeCOrientation::Reversed);
        assert_eq!(
            pre.type_c_orientation_value,
            TGL_TYPEC_ORIENTATION::VALUE::Reversed.value
        );
        assert!(pre.success);
        assert_eq!(pre.register_writes, 0);

        let post = tgl_ddi_post_on_plan(Port::Edp, Pipe::A, 0).unwrap();
        assert_eq!(post.port, Port::Edp);
        assert!(post.panel_backlight_on);
        assert!(post.success);
    }

    #[test]
    fn tigerlake_connector_off_and_hotplug_match_libgfxinit() {
        let off = tgl_ddi_pre_off_plan(Port::HdmiB).unwrap();
        assert!(off.panel_backlight_off);
        assert!(off.panel_off);
        assert_eq!(off.register_writes, 0);

        let hotplug = tgl_hotplug_detect_plan(Port::DpC).unwrap();
        assert!(!hotplug.detected);
        assert_eq!(
            hotplug.status_bits,
            TGL_HOTPLUG_STATUS::DETECTED::CLEAR.value
        );
        assert_eq!(hotplug.clear_register_writes, 0);
    }

    #[test]
    fn tigerlake_plans_reject_non_tgl_platforms_and_non_ddi_ports() {
        assert_eq!(
            tgl_ddi_pre_on_plan(
                Cpu::Skylake,
                Port::DpA,
                Pipe::A,
                0,
                TglTypeCOrientation::None,
            ),
            Err(GmaError::UnsupportedPlatform)
        );
        assert_eq!(
            tgl_ddi_pre_on_plan(
                Cpu::Tigerlake,
                Port::Vga,
                Pipe::A,
                0,
                TglTypeCOrientation::Normal,
            ),
            Err(GmaError::UnsupportedPort)
        );
        assert_eq!(
            tgl_hotplug_detect_plan(Port::Lvds),
            Err(GmaError::UnsupportedPort)
        );
    }
}
