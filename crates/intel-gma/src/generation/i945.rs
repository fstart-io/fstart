//! i945/Pineview (Gen3) Intel GMA display support.
//!
//! This mirrors libgfxinit's `common/i945` module: LVDS forced onto pipe B, VGA
//! on pipe A, the Gen3 plane/pipe swap (Plane A feeds pipe B so the panel
//! fitter and LVDS can use it), i945 or Pineview DPLL encoding, GMCH panel
//! sequencing, and the Gen3 CDClk/port-detect rules.
//!
//! Cross-checked against GNU/Linux i915:
//! - `i9xx_plane.c`: `i9xx_plane = !pipe` on gen2/3 with two pipes.
//! - `intel_cdclk.c`: `i945gm_get_cdclk` and `pnv_get_cdclk`.
//! - `intel_backlight.c`: `i9xx_set_backlight` (bits 15:1 on pre-i965).

use fstart_core::mmio::MmioReadWrite;
use tock_registers::interfaces::{Readable, Writeable};

use crate::GmaContext;
use crate::error::GmaError;
use crate::generation::g45;
use crate::generation::{GenerationOps, sealed};
use crate::gtt;
use crate::mmio::{Mmio, delay_us};
use crate::mode::Mode;
use crate::panel::{LfpPanelMetadata, set_pnv_backlight_op};
use crate::pipe::pipeconf_bpc_bits;
use crate::pll::{self, LegacyPll};
use crate::port::{self, LegacyPortPlan};
use crate::power;
use crate::regs::{DSPCNTR, GmchPipeTimingRegs, GmchPlaneRegs, PIPECONF, PP_CONTROL};
use crate::types::{Cpu, Generation, Pipe, Plane, Port};

/// i945 generation marker.
pub struct I945;

impl sealed::Sealed for I945 {}

impl GenerationOps for I945 {
    const GENERATION: Generation = Generation::I945;

    fn init_display(ctx: &mut GmaContext<'_>, mode: Mode) -> Result<(), GmaError> {
        let port = selected_port(ctx)?;
        let pipe = pipe_for_i945_port(port)?;
        let pll = legacy_pll_for_pipe(pipe)?;
        let mmio = ctx.mmio();
        let clocks = power::initialize_i945(&mmio, ctx.config.cpu, ctx.resources.gcfgc);
        if !clocks.allows_dotclock(mode.pixel_clock_khz) {
            return Err(GmaError::PllNoSolution);
        }

        map_gtt(ctx)?;
        // SAFETY: the framebuffer surface was selected from validated GMADR
        // aperture/stolen-memory resources and mapped into the GTT immediately
        // above, so the CPU-visible aperture covers this surface.
        unsafe { ctx.surface.fill_opaque_black()? };

        // libgfxinit's i945 `Connectors.Pre_On` is a no-op; the LVDS port
        // register is written by `Post_On` after the PLL and pipe are up.
        let panel = selected_lfp_panel(ctx);
        let clock = pll::find_legacy_clock(ctx.config.cpu, port, mode)?;
        pll::disable_legacy_pll(&mmio, pll);
        delay_us(150);
        pll::program_legacy_pll(&mmio, ctx.config.cpu, pll, port, clock);

        program_pipe(&mmio, pipe, mode, port)?;
        g45::program_gmch_panel_fitter(ctx, pipe, mode)?;
        program_primary_plane(ctx, pipe, port)?;
        enable_port(&mmio, port, pipe, mode)?;

        if port == Port::Lvds {
            if let Some(panel) = panel {
                g45::program_vbt_panel_registers(&mmio, panel);
            }
            g45::setup_gmch_panel_power_sequencer(&mmio);
            g45::panel_power_on(&mmio)?;
            panel_backlight_on(&mmio, panel);
        }
        Ok(())
    }

    fn program_pll(ctx: &mut GmaContext<'_>, pipe: Pipe, mode: Mode) -> Result<(), GmaError> {
        let port = selected_port(ctx)?;
        if pipe_for_i945_port(port)? != pipe {
            return Err(GmaError::UnsupportedPort);
        }
        let pll = legacy_pll_for_pipe(pipe)?;
        let mmio = ctx.mmio();
        let clock = pll::find_legacy_clock(ctx.config.cpu, port, mode)?;
        pll::disable_legacy_pll(&mmio, pll);
        delay_us(150);
        pll::program_legacy_pll(&mmio, ctx.config.cpu, pll, port, clock);
        Ok(())
    }

    fn program_pipe(ctx: &mut GmaContext<'_>, pipe: Pipe, mode: Mode) -> Result<(), GmaError> {
        let port = selected_port(ctx)?;
        program_pipe(&ctx.mmio(), pipe, mode, port)
    }

    fn program_primary_plane(ctx: &mut GmaContext<'_>, plane: Plane) -> Result<(), GmaError> {
        let pipe = pipe_for_plane(plane)?;
        program_primary_plane(ctx, pipe, selected_port(ctx)?)
    }

    fn enable_port(ctx: &mut GmaContext<'_>, port: Port, pipe: Pipe) -> Result<(), GmaError> {
        let mode = crate::choose_mode(ctx.resources, ctx.config)?;
        enable_port(&ctx.mmio(), port, pipe, mode)
    }

    fn disable_output(mmio: &Mmio, cpu: Cpu, pipe: Pipe, port: Port) -> Result<(), GmaError> {
        if port == Port::Lvds {
            g45::panel_backlight_off(mmio);
            g45::panel_power_off(mmio);
        }
        disable_pipe_state(mmio, cpu, pipe);
        g45::disable_port(mmio, port);
        let _ = cpu;
        pll::disable_legacy_pll(mmio, legacy_pll_for_pipe(pipe)?);
        Ok(())
    }

    fn clean(mmio: &Mmio, cpu: Cpu) {
        g45::legacy_vga_plane_off(mmio);
        for pipe in [Pipe::A, Pipe::B] {
            disable_pipe_state(mmio, cpu, pipe);
        }
        for port in [Port::Vga, Port::Lvds] {
            g45::disable_port(mmio, port);
        }
        pll::disable_legacy_pll(mmio, LegacyPll::A);
        pll::disable_legacy_pll(mmio, LegacyPll::B);
        gtt::clear_legacy_fences(mmio, cpu);
    }
}

fn selected_port(ctx: &GmaContext<'_>) -> Result<Port, GmaError> {
    crate::selected_enabled_port(ctx.config.outputs)
}

/// Ports implemented for the Gen3 generation.
pub(crate) const fn pipe_for_i945_port(port: Port) -> Result<Pipe, GmaError> {
    match port {
        Port::Vga => Ok(Pipe::A),
        // Pre-i965 LVDS can only source from pipe B (`LVDS_Needs_Pipe_B`).
        Port::Lvds => Ok(Pipe::B),
        _ => Err(GmaError::UnsupportedPort),
    }
}

const fn legacy_pll_for_pipe(pipe: Pipe) -> Result<LegacyPll, GmaError> {
    port::legacy_pll_for_pipe(pipe)
}

const fn pipe_for_plane(plane: Plane) -> Result<Pipe, GmaError> {
    match plane {
        Plane::PrimaryA => Ok(Pipe::A),
        Plane::PrimaryB => Ok(Pipe::B),
        Plane::PrimaryC => Err(GmaError::InvalidConfig),
    }
}

/// Plane register block for a Gen3 pipe.
///
/// Gen3 swaps plane and pipe: pipe A is driven by the Plane B register block
/// and pipe B by the Plane A block, so Plane A (the FBC-capable plane) feeds
/// the LVDS/panel-fitter pipe B. Linux encodes this as `i9xx_plane = !pipe`.
const fn plane_base_for_pipe(pipe: Pipe) -> Result<usize, GmaError> {
    match pipe {
        Pipe::A => Ok(PLANE_B_BASE),
        Pipe::B => Ok(PLANE_A_BASE),
        Pipe::C => Err(GmaError::InvalidConfig),
    }
}

const PLANE_A_BASE: usize = 0x70180;
const PLANE_B_BASE: usize = 0x71180;
const PLANE_A_CURSOR: usize = 0x70080;
const PLANE_B_CURSOR: usize = 0x700c0;

/// Gen3 sprite/overlay plane control, per pipe (not part of the plane/pipe swap).
const fn plane_sprite_for_pipe(pipe: Pipe) -> Result<usize, GmaError> {
    match pipe {
        Pipe::A => Ok(0x72180),
        Pipe::B => Ok(0x73180),
        Pipe::C => Err(GmaError::InvalidConfig),
    }
}

const fn plane_cursor_for_pipe(pipe: Pipe) -> Result<usize, GmaError> {
    match pipe {
        Pipe::A => Ok(PLANE_A_CURSOR),
        Pipe::B => Ok(PLANE_B_CURSOR),
        Pipe::C => Err(GmaError::InvalidConfig),
    }
}

const fn pipe_regs(pipe: Pipe) -> Result<(usize, usize), GmaError> {
    match pipe {
        Pipe::A => Ok((0x60000, 0x70008)),
        Pipe::B => Ok((0x61000, 0x71008)),
        Pipe::C => Err(GmaError::InvalidConfig),
    }
}

/// DSPCNTR pipe-select bit, keyed on the pipe (not the plane register letter).
#[cfg_attr(not(test), allow(dead_code))]
const fn dspcntr_pipe_select(pipe: Pipe) -> Result<u32, GmaError> {
    match pipe {
        Pipe::A => Ok(DSPCNTR::PIPE_SELECT::PipeA.value),
        Pipe::B => Ok(DSPCNTR::PIPE_SELECT::PipeB.value),
        Pipe::C => Err(GmaError::InvalidConfig),
    }
}

fn map_gtt(ctx: &GmaContext<'_>) -> Result<(), GmaError> {
    gtt::map_surface_to_stolen(ctx.resources, ctx.config.cpu, &ctx.surface)?;
    let mmio = ctx.mmio();
    gtt::clear_legacy_fences(&mmio, ctx.config.cpu);
    gtt::add_legacy_fence(&mmio, ctx.config.cpu, &ctx.surface)?;
    gtt::flush_gfx(&mmio);
    Ok(())
}

fn selected_lfp_panel(ctx: &GmaContext<'_>) -> Option<LfpPanelMetadata> {
    ctx.config
        .vbt
        .and_then(|bytes| crate::vbt::Vbt::parse(bytes).ok())
        .and_then(|vbt| vbt.lfp_panel_metadata().ok())
}

fn program_pipe(mmio: &Mmio, pipe: Pipe, mode: Mode, port: Port) -> Result<(), GmaError> {
    let (timing_off, pipeconf_off) = pipe_regs(pipe)?;
    let pipe_config = crate::pipe::PipeConfig::new(pipe, mode);
    // SAFETY: `pipe_regs` returns legacy Gen3 pipe register offsets inside the
    // decoded display MMIO BAR supplied by chipset code.
    let timing = unsafe { mmio.reg_block::<GmchPipeTimingRegs>(timing_off) };
    // SAFETY: see the timing block above.
    let pipeconf =
        unsafe { mmio.reg_block::<MmioReadWrite<u32, PIPECONF::Register>>(pipeconf_off) };
    timing.htotal.set(pipe_config.htotal());
    timing.hblank.set(pipe_config.hblank());
    timing.hsync.set(pipe_config.hsync());
    timing.vtotal.set(pipe_config.vtotal());
    timing.vblank.set(pipe_config.vblank());
    timing.vsync.set(pipe_config.vsync());
    timing.pipesrc.set(pipe_config.pipesrc());
    pipeconf.set(PIPECONF::ENABLE::SET.value | pipeconf_bpc_bits(port));
    // Gen3 reports no pipe-active status: `PIPECONF` bit 30 is `DOUBLE_WIDE`
    // here and only became the active-status bit on 965+ (Linux's
    // `I965_PIPECONF_ACTIVE`). Polling it can only time out, so confirm the
    // write by reading the enable bit back instead.
    if pipeconf.is_set(PIPECONF::ENABLE) {
        Ok(())
    } else {
        Err(GmaError::HardwareError)
    }
}

fn program_primary_plane(ctx: &GmaContext<'_>, pipe: Pipe, _port: Port) -> Result<(), GmaError> {
    g45::program_gmch_plane(
        &ctx.mmio(),
        plane_base_for_pipe(pipe)?,
        pipe,
        ctx.surface,
        crate::port::legacy_plane_address_model(ctx.config.cpu),
    )
}

fn enable_port(mmio: &Mmio, port: Port, pipe: Pipe, mode: Mode) -> Result<(), GmaError> {
    match port {
        Port::Lvds | Port::Vga => {
            g45::apply_port_op(mmio, LegacyPortPlan::for_port(port, pipe, mode)?.enable);
            Ok(())
        }
        _ => Err(GmaError::UnsupportedPort),
    }
}

fn disable_pipe_state(mmio: &Mmio, cpu: Cpu, pipe: Pipe) {
    let Ok(base) = plane_base_for_pipe(pipe) else {
        return;
    };
    let Ok((_, pipeconf_off)) = pipe_regs(pipe) else {
        return;
    };
    // libgfxinit `Pipe_Setup.Off`: planes off, then the transcoder, then the
    // panel fitter. Gen3 has a second (overlay/sprite) plane to clear.
    if let Ok(sprite) = plane_sprite_for_pipe(pipe) {
        mmio.clear_bits32(sprite, DSPCNTR::ENABLE::SET.value);
    }
    if let Ok(cursor) = plane_cursor_for_pipe(pipe) {
        mmio.write32(cursor, 0);
    }
    // SAFETY: `base` is the Gen3 plane register block for this pipe.
    let regs = unsafe { mmio.reg_block::<GmchPlaneRegs>(base) };
    regs.cntr.set(0);
    regs.addr.set(0);
    let _ = regs.addr.get();
    mmio.clear_bits32(pipeconf_off, PIPECONF::ENABLE::SET.value);
    let mut timeout = 100_000u32;
    while timeout != 0 {
        if (mmio.read32(pipeconf_off) & PIPECONF::ENABLED_STATUS::SET.value) == 0 {
            break;
        }
        timeout -= 1;
        core::hint::spin_loop();
    }
    g45::panel_fitter_off_for_pipe(mmio, cpu, pipe);
}

const PNV_BLC_PWM_DUTY_MAX: u32 = 0x7fff;

fn panel_backlight_on(mmio: &Mmio, panel: Option<LfpPanelMetadata>) {
    if panel
        .and_then(|panel| panel.backlight)
        .map(|backlight| backlight.is_pwm())
        .unwrap_or(false)
    {
        g45::apply_panel_op(
            mmio,
            set_pnv_backlight_op(g45::BLC_PWM_GMCH_CTL, PNV_BLC_PWM_DUTY_MAX),
        );
    }
    let panel_regs = g45::gmch_panel_regs(mmio);
    let control = panel_regs.pp_control.get();
    panel_regs.pp_control.set(g45::panel_control_unlocked(
        control | PP_CONTROL::BACKLIGHT_ENABLE::SET.value,
    ));
    let _ = panel_regs.pp_control.get();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gen3_swaps_plane_blocks_and_forces_lvds_to_pipe_b() {
        // Plane A feeds pipe B on Gen3 so the panel fitter and LVDS can use it.
        assert_eq!(plane_base_for_pipe(Pipe::A).unwrap(), PLANE_B_BASE);
        assert_eq!(plane_base_for_pipe(Pipe::B).unwrap(), PLANE_A_BASE);
        // The DSPCNTR pipe-select bit still follows the pipe, not the plane.
        assert_eq!(
            dspcntr_pipe_select(Pipe::A).unwrap(),
            DSPCNTR::PIPE_SELECT::PipeA.value
        );
        assert_eq!(
            dspcntr_pipe_select(Pipe::B).unwrap(),
            DSPCNTR::PIPE_SELECT::PipeB.value
        );
        assert!(plane_base_for_pipe(Pipe::C).is_err());
    }

    #[test]
    fn gen3_ports_use_the_expected_pipes_and_plls() {
        assert_eq!(pipe_for_i945_port(Port::Lvds).unwrap(), Pipe::B);
        assert_eq!(pipe_for_i945_port(Port::Vga).unwrap(), Pipe::A);
        assert_eq!(
            pipe_for_i945_port(Port::HdmiA),
            Err(GmaError::UnsupportedPort)
        );
        assert_eq!(legacy_pll_for_pipe(Pipe::A).unwrap(), LegacyPll::A);
        assert_eq!(legacy_pll_for_pipe(Pipe::B).unwrap(), LegacyPll::B);
        assert!(legacy_pll_for_pipe(Pipe::C).is_err());
    }
}
