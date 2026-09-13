//! G45/GM965 Intel GMA generation support.
//!
//! This is the first real hardware modeset path, modeled after libgfxinit's
//! G45 GMCH PLL, pipe, plane, and connector sequencing. The live path covers
//! legacy GMCH VGA/LVDS/HDMI/DP flows, including DP AUX training with
//! libgfxinit-style link-setting retry and rollback.

use crate::dp_training::train_gmch_dp_with_retry;
use crate::error::GmaError;
use crate::generation::{GenerationOps, sealed};
use crate::gtt;
use crate::mmio::{Mmio, delay_us};
use crate::mode::Mode;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use crate::GmaContext;
use crate::panel::{
    BacklightRegisterModel, LfpPanelMetadata, PanelPowerDelays, PanelPowerPortSelect,
    PanelPowerRegs, PanelRegisterOp, panel_power_sequencer_plan, set_backlight_op,
};
use crate::plane::PlaneAddressModel;
use crate::pll::{self, LegacyPll};
use crate::port::{
    LegacyPortPlan, LvdsPortConfig, OutputPipeline, PortRegisterOp, dp_idle_op, dp_off_op,
    hdmi_disable_op, lvds_disable_op, lvds_port_value_with_config, vga_disable_op,
};
use crate::power;
use crate::regs::{
    DSPCNTR, GMCH_VGACNTRL_OFFSET, GmchPanelRegs, GmchPipeTimingRegs, GmchPlaneRegs, PFIT_CONTROL,
    PIPECONF, PP_CONTROL, PP_STATUS, VGACNTRL,
};
use crate::scaler;
use crate::types::{Cpu, Generation, Pipe, Plane, Port};

/// G45 generation marker.
pub struct G45;

impl sealed::Sealed for G45 {}

impl GenerationOps for G45 {
    const GENERATION: Generation = Generation::G45;

    fn init_display(ctx: &mut GmaContext<'_>, mode: Mode) -> Result<(), GmaError> {
        let port = selected_port(ctx)?;
        let pipeline = OutputPipeline::legacy_gmch(ctx.config.cpu, port, mode, ctx.surface)?;
        let mmio = ctx.mmio();
        let clocks = power::initialize_legacy_gmch(&mmio, ctx.config.cpu, ctx.resources.gcfgc);
        if !clocks.allows_dotclock(mode.pixel_clock_khz) {
            return Err(GmaError::PllNoSolution);
        }

        map_gtt(ctx)?;
        // SAFETY: the framebuffer surface was selected from validated GMADR
        // aperture/stolen-memory resources and mapped into the GTT immediately
        // above, so the CPU-visible aperture covers this surface.
        unsafe { ctx.surface.fill_bringup_pattern()? };
        disable_legacy_display_state(&mmio);
        disable_legacy_path(&mmio);
        let panel = selected_lfp_panel(ctx);
        pre_pll_enable_port(
            &mmio,
            pipeline.port,
            pipeline.pipe.pipe,
            pipeline.pipe.mode,
            panel,
        )?;
        program_pll_for_port(ctx, &pipeline)?;
        if is_gmch_dp_port(pipeline.port) {
            train_gmch_dp_with_retry(
                &mmio,
                pipeline.port,
                pipeline.pipe.pipe,
                pipeline.pipe.mode,
                || {
                    Self::program_pipe(ctx, pipeline.pipe.pipe, pipeline.pipe.mode)?;
                    program_gmch_panel_fitter(ctx, &pipeline)?;
                    Self::program_primary_plane(ctx, pipeline.plane.plane)?;
                    let _ = crate::port_detect::clear_hotplug_detect(&mmio, pipeline.port);
                    Ok(())
                },
                || {
                    disable_legacy_display_state(&mmio);
                    let _ = crate::port_detect::clear_hotplug_detect(&mmio, pipeline.port);
                },
            )?;
        } else {
            Self::program_pipe(ctx, pipeline.pipe.pipe, pipeline.pipe.mode)?;
            program_gmch_panel_fitter(ctx, &pipeline)?;
            Self::program_primary_plane(ctx, pipeline.plane.plane)?;
            let _ = crate::port_detect::clear_hotplug_detect(&mmio, pipeline.port);
            enable_port_with_mode_and_panel(
                &mmio,
                pipeline.port,
                pipeline.pipe.pipe,
                pipeline.pipe.mode,
                panel,
            )?;
        }
        if pipeline.port == Port::Lvds {
            if let Some(panel) = panel {
                program_vbt_panel_registers(&mmio, panel);
            }
            setup_gmch_panel_power_sequencer(&mmio);
            panel_power_on(&mmio)?;
            panel_backlight_on(&mmio, panel);
        }
        Ok(())
    }

    fn program_pll(ctx: &mut GmaContext<'_>, pipe: Pipe, mode: Mode) -> Result<(), GmaError> {
        let port = selected_port(ctx)?;
        let pipeline = OutputPipeline::legacy_gmch(ctx.config.cpu, port, mode, ctx.surface)?;
        if pipeline.pipe.pipe != pipe {
            return Err(GmaError::UnsupportedPort);
        }
        program_pll_for_port(ctx, &pipeline)
    }

    fn program_pipe(ctx: &mut GmaContext<'_>, pipe: Pipe, mode: Mode) -> Result<(), GmaError> {
        let regs = PipeRegs::for_pipe(pipe)?;
        let pipe_config = crate::pipe::PipeConfig::new(pipe, mode);
        let mmio = ctx.mmio();
        // SAFETY: `regs.timing`/`regs.pipeconf` are generation-validated GMCH
        // pipe register offsets inside the decoded display MMIO BAR.
        let timing = unsafe { mmio.reg_block::<GmchPipeTimingRegs>(regs.timing) };
        // SAFETY: see the timing block above; pipeconf is a single typed register.
        let pipeconf = unsafe {
            mmio.reg_block::<tock_registers::registers::ReadWrite<u32, PIPECONF::Register>>(
                regs.pipeconf,
            )
        };
        timing.htotal.set(pipe_config.htotal());
        timing.hblank.set(pipe_config.hblank());
        timing.hsync.set(pipe_config.hsync());
        timing.vtotal.set(pipe_config.vtotal());
        timing.vblank.set(pipe_config.vblank());
        timing.vsync.set(pipe_config.vsync());
        timing.pipesrc.set(pipe_config.pipesrc());
        pipeconf.write(PIPECONF::ENABLE::SET + PIPECONF::BPC::Bits6);
        let _ = pipeconf.get();
        let mut timeout = 100_000u32;
        while timeout != 0 {
            if pipeconf.is_set(PIPECONF::ENABLED_STATUS) {
                return Ok(());
            }
            timeout -= 1;
            core::hint::spin_loop();
        }
        Err(GmaError::Timeout)
    }

    fn program_primary_plane(ctx: &mut GmaContext<'_>, plane: Plane) -> Result<(), GmaError> {
        let regs = PlaneRegs::for_plane(plane)?;
        let pipe = pipe_for_plane(plane)?;
        let plane_config = crate::plane::PlaneConfig::new(
            plane,
            pipe,
            crate::port::legacy_plane_address_model(ctx.config.cpu),
            ctx.surface,
        );
        let mmio = ctx.mmio();
        // SAFETY: `regs.base` is the generation-validated GMCH primary plane
        // register block for `plane` inside the decoded display MMIO BAR.
        let plane_regs = unsafe { mmio.reg_block::<GmchPlaneRegs>(regs.base) };
        let stride_bytes = plane_config.stride_bytes()?;
        let ctl = (DSPCNTR::ENABLE::SET + DSPCNTR::FORMAT::Xrgb8888).value
            | dspcntr_pipe_select(plane)?
            | plane_config.legacy_tiling_bits();
        if plane_config.address_model == PlaneAddressModel::Address {
            plane_regs.stride.set(stride_bytes);
            plane_regs.pos.set(0);
            plane_regs.size.set(plane_config.encoded_size()?);
            plane_regs.cntr.set(ctl);
            plane_regs.addr.set(0);
            let _ = plane_regs.addr.get();
        } else {
            plane_regs.stride.set(stride_bytes);
            if ctx.surface.tiling == crate::framebuffer::TilingMode::Linear {
                plane_regs.addr.set(plane_config.linear_offset_bytes()?);
                plane_regs.tileoff.set(0);
            } else {
                plane_regs.addr.set(0);
                plane_regs.tileoff.set(plane_config.tile_offset());
            }
            plane_regs.surf.set(ctx.surface.plane_surface_offset());
            plane_regs.cntr.set(ctl);
            plane_regs.surf.set(ctx.surface.plane_surface_offset());
            let _ = plane_regs.surf.get();
        }
        Ok(())
    }

    fn enable_port(ctx: &mut GmaContext<'_>, port: Port, pipe: Pipe) -> Result<(), GmaError> {
        enable_port_with_mode(
            &ctx.mmio(),
            port,
            pipe,
            crate::choose_mode(ctx.resources, ctx.config)?,
        )
    }
}

fn gmch_panel_regs(mmio: &Mmio) -> &'static GmchPanelRegs {
    // SAFETY: GMCH panel power/fitter registers live at the fixed PP_STATUS
    // block within the decoded display MMIO BAR for this generation.
    unsafe { mmio.reg_block::<GmchPanelRegs>(GmchPanelRegs::BASE) }
}

fn selected_port(ctx: &GmaContext<'_>) -> Result<Port, GmaError> {
    crate::selected_enabled_port(ctx.config.outputs)
}

fn program_pll_for_port(
    ctx: &mut GmaContext<'_>,
    pipeline: &OutputPipeline,
) -> Result<(), GmaError> {
    let clock = pll::find_legacy_clock(ctx.config.cpu, pipeline.port, pipeline.pipe.mode)?;
    pll::disable_legacy_pll(&ctx.mmio(), pipeline.pll);
    delay_us(150);
    pll::program_legacy_pll(
        &ctx.mmio(),
        ctx.config.cpu,
        pipeline.pll,
        pipeline.port,
        clock,
    );
    Ok(())
}

fn pre_pll_enable_port(
    mmio: &Mmio,
    port: Port,
    pipe: Pipe,
    mode: Mode,
    panel: Option<LfpPanelMetadata>,
) -> Result<(), GmaError> {
    if port == Port::Lvds {
        // Legacy GMCH LVDS requires the port enable bit and lane power to be
        // programmed before the LVDS DPLL is enabled. Linux i915 and
        // libgfxinit both preserve this ordering for LVDS PLL bring-up.
        apply_port_op(
            mmio,
            PortRegisterOp::Write {
                register: crate::port::GMCH_LVDS,
                value: lvds_port_value_with_config(pipe, mode, lvds_port_config(panel))?,
            },
        );
        return Ok(());
    }
    let plan = LegacyPortPlan::for_port(port, pipe, mode)?;
    if let Some(op) = plan.pre_pll {
        apply_port_op(mmio, op);
    }
    Ok(())
}

fn enable_port_with_mode(mmio: &Mmio, port: Port, pipe: Pipe, mode: Mode) -> Result<(), GmaError> {
    enable_port_with_mode_and_panel(mmio, port, pipe, mode, None)
}

fn enable_port_with_mode_and_panel(
    mmio: &Mmio,
    port: Port,
    pipe: Pipe,
    mode: Mode,
    panel: Option<LfpPanelMetadata>,
) -> Result<(), GmaError> {
    if port == Port::Lvds {
        apply_port_op(
            mmio,
            PortRegisterOp::Write {
                register: crate::port::GMCH_LVDS,
                value: lvds_port_value_with_config(pipe, mode, lvds_port_config(panel))?,
            },
        );
        return Ok(());
    }
    apply_port_op(mmio, LegacyPortPlan::for_port(port, pipe, mode)?.enable);
    Ok(())
}

const fn is_gmch_dp_port(port: Port) -> bool {
    matches!(port, Port::DpA | Port::DpB | Port::DpC)
}

fn apply_port_op(mmio: &Mmio, op: PortRegisterOp) {
    match op {
        PortRegisterOp::Write { register, value } => mmio.write32(register, value),
        PortRegisterOp::Update {
            register,
            mask_unset,
            mask_set,
        } => mmio.update32(register, mask_unset, mask_set),
    }
}

fn map_gtt(ctx: &GmaContext<'_>) -> Result<(), GmaError> {
    gtt::map_surface_to_stolen(ctx.resources, &ctx.surface)?;
    let mmio = ctx.mmio();
    gtt::clear_legacy_fences(&mmio);
    gtt::add_legacy_fence(&mmio, &ctx.surface)?;
    gtt::flush_gfx(&mmio);
    Ok(())
}

fn program_gmch_panel_fitter(
    ctx: &GmaContext<'_>,
    pipeline: &OutputPipeline,
) -> Result<(), GmaError> {
    let plan = scaler::ScalerPlan::resolve(
        ctx.config.cpu,
        pipeline.pipe.pipe,
        ctx.surface,
        pipeline.pipe.mode,
        ctx.config.framebuffer.scaling,
    );
    if !plan.requires_scaling {
        let mmio = ctx.mmio();
        let panel_regs = gmch_panel_regs(&mmio);
        panel_regs.pfit_control.set(0);
        panel_regs.pfit_pgm_ratios.set(0);
        return Ok(());
    }
    plan.validate_current()?;
    let encoding = if ctx.config.cpu == Cpu::Pineview {
        scaler::encode_gmch_pre_i965(
            ctx.surface.width,
            ctx.surface.height,
            pipeline.pipe.mode.hdisplay,
            pipeline.pipe.mode.vdisplay,
            ctx.config.framebuffer.scaling,
            24,
        )
    } else {
        scaler::encode_gmch_i965(
            pipeline.pipe.pipe,
            ctx.surface.width,
            ctx.surface.height,
            pipeline.pipe.mode.hdisplay,
            pipeline.pipe.mode.vdisplay,
            ctx.config.framebuffer.scaling,
        )?
    };
    let mmio = ctx.mmio();
    let panel_regs = gmch_panel_regs(&mmio);
    panel_regs.pfit_pgm_ratios.set(encoding.pgm_ratios);
    panel_regs.pfit_control.set(encoding.control);
    Ok(())
}

const fn pipe_for_plane(plane: Plane) -> Result<Pipe, GmaError> {
    match plane {
        Plane::PrimaryA => Ok(Pipe::A),
        Plane::PrimaryB => Ok(Pipe::B),
        Plane::PrimaryC => Err(GmaError::InvalidConfig),
    }
}

fn selected_lfp_panel(ctx: &GmaContext<'_>) -> Option<LfpPanelMetadata> {
    ctx.config
        .vbt
        .and_then(|bytes| crate::vbt::Vbt::parse(bytes).ok())
        .and_then(|vbt| vbt.lfp_panel_metadata().ok())
}

const fn lvds_port_config(panel: Option<LfpPanelMetadata>) -> LvdsPortConfig {
    let Some(panel) = panel else {
        return LvdsPortConfig::DEFAULT;
    };
    LvdsPortConfig {
        force_dual_channel: panel_lvds_dual_channel(panel),
        // Preserve the old GM965/X61 behavior unless VBT explicitly gives us a
        // reason to do otherwise. libgfxinit's GMCH LVDS path only programs the
        // 18bpp/dithered mode, and warns for higher BPC instead of changing the
        // register format.
        enable_dither: true,
    }
}

const fn panel_lvds_dual_channel(panel: LfpPanelMetadata) -> Option<bool> {
    match panel.options.lvds_panel_channel_bits {
        Some(bits) => Some((bits & (1 << panel.panel_type)) != 0),
        None => None,
    }
}

fn setup_gmch_panel_power_sequencer(mmio: &Mmio) {
    let panel_regs = gmch_panel_regs(mmio);
    let delays = PanelPowerDelays::from_registers(
        panel_regs.pp_on_delays.get(),
        panel_regs.pp_off_delays.get(),
        panel_regs.pp_divisor.get(),
    );
    let plan = panel_power_sequencer_plan(
        PanelPowerRegs::GMCH,
        delays,
        PanelPowerPortSelect::Lvds,
        true,
        true,
    );
    apply_panel_op(mmio, plan.on_delays);
    apply_panel_op(mmio, plan.off_delays);
    apply_panel_op(mmio, plan.cycle_delay);
    apply_panel_op(mmio, plan.control);
}

fn apply_panel_op(mmio: &Mmio, op: PanelRegisterOp) {
    match op {
        PanelRegisterOp::Write { register, value } => mmio.write32(register, value),
        PanelRegisterOp::Update {
            register,
            mask_unset,
            mask_set,
        } => mmio.update32(register, mask_unset, mask_set),
        PanelRegisterOp::Set { register, mask } => mmio.update32(register, 0, mask),
        PanelRegisterOp::Clear { register, mask } => mmio.update32(register, mask, 0),
    }
}

fn program_vbt_panel_registers(mmio: &Mmio, panel: LfpPanelMetadata) {
    let Some(timing) = panel.fp_timing else {
        return;
    };
    write_vbt_panel_register(mmio, timing.pp_on_reg, timing.pp_on_reg_val);
    write_vbt_panel_register(mmio, timing.pp_off_reg, timing.pp_off_reg_val);
    write_vbt_panel_register(mmio, timing.pp_cycle_reg, timing.pp_cycle_reg_val);
    write_vbt_panel_register(mmio, timing.pfit_reg, timing.pfit_reg_val);
}

fn write_vbt_panel_register(mmio: &Mmio, register: u32, value: u32) {
    if is_safe_vbt_panel_register(register) {
        mmio.write32(register as usize, value);
    }
}

const fn is_safe_vbt_panel_register(register: u32) -> bool {
    matches!(
        register as usize,
        GmchPanelRegs::PP_ON_DELAYS_OFFSET
            | GmchPanelRegs::PP_OFF_DELAYS_OFFSET
            | GmchPanelRegs::PP_DIVISOR_OFFSET
            | GmchPanelRegs::PFIT_CONTROL_OFFSET
    )
}

/// Best-effort rollback used when a configured output candidate fails during
/// bring-up. This mirrors the legacy clean-state off path without surfacing
/// secondary cleanup errors over the original failure.
pub(crate) fn cleanup_legacy_gmch_after_failure(mmio: &Mmio) {
    disable_legacy_display_state(mmio);
    disable_legacy_path(mmio);
}

fn disable_legacy_path(mmio: &Mmio) {
    apply_port_op(mmio, vga_disable_op());
    apply_port_op(mmio, lvds_disable_op());
    if let Ok(op) = hdmi_disable_op(Port::HdmiA) {
        apply_port_op(mmio, op);
    }
    if let Ok(op) = hdmi_disable_op(Port::HdmiB) {
        apply_port_op(mmio, op);
    }
    for port in [Port::DpA, Port::DpB, Port::DpC] {
        if let Ok(op) = dp_idle_op(port) {
            apply_port_op(mmio, op);
        }
        if let Ok(op) = dp_off_op(port) {
            apply_port_op(mmio, op);
        }
    }
    pll::disable_legacy_pll(mmio, LegacyPll::A);
    pll::disable_legacy_pll(mmio, LegacyPll::B);
    gtt::clear_legacy_fences(mmio);
}

fn disable_legacy_display_state(mmio: &Mmio) {
    legacy_vga_plane_off(mmio);
    for pipe in [Pipe::A, Pipe::B] {
        if let Ok(regs) = PipeRegs::for_pipe(pipe) {
            planes_off(mmio, regs.plane);
            panel_fitter_off_for_pipe(mmio, pipe);
            mmio.clear_bits32(regs.pipeconf, PIPECONF::ENABLE::SET.value);
            mmio.posting_read(regs.pipeconf);
        }
    }
}

fn legacy_vga_plane_off(mmio: &Mmio) {
    vga_sequencer_screen_off();
    // SAFETY: `GMCH_VGACNTRL_OFFSET` is the fixed legacy VGA control register
    // in the validated GMCH display MMIO BAR.
    let vga_control = unsafe {
        mmio.reg_block::<tock_registers::registers::ReadWrite<u32, VGACNTRL::Register>>(
            GMCH_VGACNTRL_OFFSET,
        )
    };
    vga_control.modify(VGACNTRL::VGA_DISPLAY_DISABLE::SET);
    let _ = vga_control.get();
    delay_us(100);
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn vga_sequencer_screen_off() {
    const VGA_SEQ_INDEX: u16 = 0x3c4;
    const VGA_SEQ_DATA: u16 = 0x3c5;
    const VGA_SEQ_CLOCKING_MODE: u8 = 0x01;
    const VGA_SEQ_SCREEN_OFF: u8 = 1 << 5;

    let current: u8;
    // SAFETY: VGA sequencer index/data ports are the architectural legacy VGA
    // I/O registers. This helper only sets SR01 bit 5 to blank legacy VGA
    // scanout before MMIO modesetting, matching libgfxinit's legacy VGA off
    // sequence on x86 platforms.
    unsafe {
        core::arch::asm!("out dx, al", in("dx") VGA_SEQ_INDEX, in("al") VGA_SEQ_CLOCKING_MODE);
        core::arch::asm!("in al, dx", in("dx") VGA_SEQ_DATA, out("al") current);
        core::arch::asm!("out dx, al", in("dx") VGA_SEQ_INDEX, in("al") VGA_SEQ_CLOCKING_MODE);
        core::arch::asm!("out dx, al", in("dx") VGA_SEQ_DATA, in("al") current | VGA_SEQ_SCREEN_OFF);
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn vga_sequencer_screen_off() {}

fn planes_off(mmio: &Mmio, regs: PlaneRegs) {
    mmio.write32(regs.cursor_control, 0);
    mmio.clear_bits32(regs.sprite_control, DSPCNTR::ENABLE::SET.value);
    mmio.clear_bits32(regs.cntr, DSPCNTR::ENABLE::SET.value);
    mmio.write32(regs.surf, 0);
    mmio.posting_read(regs.surf);
}

fn panel_fitter_off_for_pipe(mmio: &Mmio, pipe: Pipe) {
    let panel_regs = gmch_panel_regs(mmio);
    let control = panel_regs.pfit_control.extract();
    if !control.is_set(PFIT_CONTROL::ENABLE) {
        return;
    }
    let selected_pipe = match control.read(PFIT_CONTROL::PIPE_SELECT) {
        1 => Pipe::B,
        2 => Pipe::C,
        _ => Pipe::A,
    };
    if selected_pipe == pipe {
        panel_regs
            .pfit_control
            .set(panel_regs.pfit_control.get() & !PFIT_CONTROL::ENABLE::SET.value);
        let _ = panel_regs.pfit_control.get();
    }
}

fn panel_power_on(mmio: &Mmio) -> Result<(), GmaError> {
    let panel_regs = gmch_panel_regs(mmio);
    let control = panel_regs.pp_control.get();
    let was_on = panel_regs.pp_control.is_set(PP_CONTROL::TARGET_ON);
    panel_regs.pp_control.set(panel_control_unlocked(
        control | PP_CONTROL::TARGET_ON::SET.value,
    ));
    let _ = panel_regs.pp_control.get();
    if !was_on {
        delay_us(210_000);
    }
    let mut timeout = 300_000u32;
    while timeout != 0 {
        if (panel_regs.pp_status.get() & PP_STATUS::SEQUENCE.mask) == 0 {
            break;
        }
        timeout -= 1;
        core::hint::spin_loop();
    }
    if timeout == 0 {
        return Err(GmaError::Timeout);
    }
    timeout = 300_000;
    while timeout != 0 {
        if panel_regs.pp_status.is_set(PP_STATUS::ON) {
            return Ok(());
        }
        timeout -= 1;
        core::hint::spin_loop();
    }
    Err(GmaError::Timeout)
}

fn panel_backlight_on(mmio: &Mmio, panel: Option<LfpPanelMetadata>) {
    if panel
        .and_then(|panel| panel.backlight)
        .map(|backlight| backlight.is_pwm())
        .unwrap_or(false)
    {
        apply_panel_op(
            mmio,
            set_backlight_op(
                BacklightRegisterModel::Legacy {
                    cpu_ctl: BLC_PWM_CPU_CTL,
                    pch_ctl2: BLC_PWM_PCH_CTL2,
                },
                CPU_BLC_PWM_DUTY_MAX,
            ),
        );
    }
    let panel_regs = gmch_panel_regs(mmio);
    let control = panel_regs.pp_control.get();
    panel_regs.pp_control.set(panel_control_unlocked(
        control | PP_CONTROL::BACKLIGHT_ENABLE::SET.value,
    ));
    let _ = panel_regs.pp_control.get();
}

const fn dspcntr_pipe_select(plane: Plane) -> Result<u32, GmaError> {
    match plane {
        Plane::PrimaryA => Ok(DSPCNTR::PIPE_SELECT::PipeA.value),
        Plane::PrimaryB => Ok(DSPCNTR::PIPE_SELECT::PipeB.value),
        Plane::PrimaryC => Err(GmaError::InvalidConfig),
    }
}

const fn panel_control_unlocked(control: u32) -> u32 {
    (control & !PP_CONTROL_UNLOCK_MASK) | PP_CONTROL_UNLOCK_KEY
}

struct PipeRegs {
    timing: usize,
    pipeconf: usize,
    plane: PlaneRegs,
}

impl PipeRegs {
    const fn for_pipe(pipe: Pipe) -> Result<Self, GmaError> {
        match pipe {
            Pipe::A => Ok(Self {
                timing: 0x60000,
                pipeconf: 0x70008,
                plane: PlaneRegs::for_primary_a(),
            }),
            Pipe::B => Ok(Self {
                timing: 0x61000,
                pipeconf: 0x71008,
                plane: PlaneRegs::for_primary_b(),
            }),
            Pipe::C => Err(GmaError::InvalidConfig),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlaneRegs {
    base: usize,
    cntr: usize,
    surf: usize,
    sprite_control: usize,
    cursor_control: usize,
}

impl PlaneRegs {
    const fn for_plane(plane: Plane) -> Result<Self, GmaError> {
        match plane {
            Plane::PrimaryA => Ok(Self::for_primary_a()),
            Plane::PrimaryB => Ok(Self::for_primary_b()),
            Plane::PrimaryC => Err(GmaError::InvalidConfig),
        }
    }

    const fn for_primary_a() -> Self {
        Self {
            base: 0x70180,
            cntr: 0x70180,
            surf: 0x7019c,
            sprite_control: 0x72180,
            cursor_control: 0x70080,
        }
    }

    const fn for_primary_b() -> Self {
        Self {
            base: 0x71180,
            cntr: 0x71180,
            surf: 0x7119c,
            sprite_control: 0x73180,
            cursor_control: 0x700c0,
        }
    }
}

const BLC_PWM_CPU_CTL: usize = 0x48254;
const BLC_PWM_PCH_CTL2: usize = 0x61254;
#[allow(dead_code)]
const CPU_BLC_PWM_DUTY_MAX: u32 = 0x0000_ffff;
const PP_CONTROL_UNLOCK_MASK: u32 = PP_CONTROL::UNLOCK_KEY.val(0xffff).value;
const PP_CONTROL_UNLOCK_KEY: u32 = PP_CONTROL::UNLOCK_KEY.val(0xabcd).value;
#[cfg(test)]
const PP_CONTROL_TARGET_ON: u32 = PP_CONTROL::TARGET_ON::SET.value;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g45_selects_lvds_on_pipe_b() {
        assert_eq!(
            crate::port::pipe_for_legacy_gmch_port(Port::Lvds),
            Ok(Pipe::B)
        );
        assert_eq!(crate::port::legacy_pll_for_pipe(Pipe::B), Ok(LegacyPll::B));
    }

    #[test]
    fn g45_dp_pipeline_is_exposed_in_caps() {
        let surface = crate::framebuffer::SurfaceConfig::packed(
            crate::types::PhysAddr(0xd000_0000),
            1024,
            768,
            crate::framebuffer::PixelFormat::Xrgb8888,
        );
        let pipeline =
            OutputPipeline::legacy_gmch(Cpu::G45, Port::DpA, Mode::XGA_1024X768_60, surface)
                .unwrap();
        assert_eq!(pipeline.pipe.pipe, Pipe::A);
        assert_eq!(pipeline.plane.plane, Plane::PrimaryA);
        assert_eq!(pipeline.pll, LegacyPll::A);
        assert!(is_gmch_dp_port(pipeline.port));
        assert!(crate::caps_for(Cpu::G45).ports.contains(&Port::DpA));
    }

    #[test]
    fn encodes_gmch_timing_ranges() {
        assert_eq!(
            crate::pipe::PipeConfig::encode_range(1024, 1344),
            0x053f_03ff
        );
        assert_eq!(crate::pipe::PipeConfig::encode_range(771, 777), 0x0308_0302);
        assert_eq!(crate::plane::encode_size(1024, 768), 0x02ff_03ff);
    }

    #[test]
    fn pineview_uses_pre_i965_plane_addr_register() {
        let regs = PlaneRegs::for_plane(Plane::PrimaryA).unwrap();
        assert_eq!(regs.base + 0x04, 0x70184);
        assert_eq!(
            crate::port::legacy_plane_address_model(crate::types::Cpu::Pineview),
            PlaneAddressModel::Address
        );
        assert_eq!(
            crate::port::legacy_plane_address_model(crate::types::Cpu::Gm965),
            PlaneAddressModel::Surface
        );
    }

    #[test]
    fn primary_b_plane_selects_pipe_b() {
        assert_eq!(
            dspcntr_pipe_select(Plane::PrimaryA),
            Ok(DSPCNTR::PIPE_SELECT::PipeA.value)
        );
        assert_eq!(
            dspcntr_pipe_select(Plane::PrimaryB),
            Ok(DSPCNTR::PIPE_SELECT::PipeB.value)
        );
    }

    #[test]
    fn g45_internal_helpers_reject_pipe_c_and_primary_c() {
        assert!(matches!(
            PipeRegs::for_pipe(Pipe::C),
            Err(GmaError::InvalidConfig)
        ));
        assert!(matches!(
            PlaneRegs::for_plane(Plane::PrimaryC),
            Err(GmaError::InvalidConfig)
        ));
        assert_eq!(
            pipe_for_plane(Plane::PrimaryC),
            Err(GmaError::InvalidConfig)
        );
        assert_eq!(
            dspcntr_pipe_select(Plane::PrimaryC),
            Err(GmaError::InvalidConfig)
        );
    }

    #[test]
    fn panel_control_unlock_preserves_low_bits() {
        assert_eq!(
            panel_control_unlocked(PP_CONTROL_TARGET_ON),
            PP_CONTROL_UNLOCK_KEY | PP_CONTROL_TARGET_ON
        );
    }

    #[test]
    fn legacy_backlight_constants_match_libgfxinit_register_model() {
        assert_eq!(BLC_PWM_CPU_CTL, 0x48254);
        assert_eq!(CPU_BLC_PWM_DUTY_MAX, 0xffff);
    }

    #[test]
    fn gmch_panel_fitter_encoders_accept_upscale() {
        let encoding = scaler::encode_gmch_i965(
            Pipe::B,
            800,
            600,
            1024,
            768,
            scaler::ScalingPolicy::PreserveAspect,
        )
        .unwrap();
        assert_ne!(encoding.control & (1 << 31), 0);
        assert_eq!(encoding.pgm_ratios, 0);

        let pineview =
            scaler::encode_gmch_pre_i965(800, 600, 1024, 768, scaler::ScalingPolicy::Stretch, 24);
        assert_ne!(pineview.control & (1 << 31), 0);
    }

    #[test]
    fn vbt_panel_register_filter_accepts_only_panel_sequence_registers() {
        assert!(is_safe_vbt_panel_register(
            GmchPanelRegs::PP_ON_DELAYS_OFFSET as u32
        ));
        assert!(is_safe_vbt_panel_register(
            GmchPanelRegs::PP_OFF_DELAYS_OFFSET as u32
        ));
        assert!(is_safe_vbt_panel_register(
            GmchPanelRegs::PP_DIVISOR_OFFSET as u32
        ));
        assert!(is_safe_vbt_panel_register(
            GmchPanelRegs::PFIT_CONTROL_OFFSET as u32
        ));
        assert!(!is_safe_vbt_panel_register(
            GmchPanelRegs::PP_CONTROL_OFFSET as u32
        ));
        assert!(!is_safe_vbt_panel_register(0xfeed_cafe));
    }

    #[test]
    fn lvds_pre_pll_plan_writes_port_before_pll_enable() {
        let plan = LegacyPortPlan::for_port(Port::Lvds, Pipe::B, Mode::XGA_1024X768_60).unwrap();
        assert!(matches!(plan.pre_pll, Some(PortRegisterOp::Write { .. })));
        assert_eq!(plan.pre_pll, Some(plan.enable));
    }

    #[test]
    fn lvds_vbt_channel_metadata_overrides_threshold_safely() {
        let single = lvds_port_value_with_config(
            Pipe::B,
            Mode::XGA_1024X768_60,
            LvdsPortConfig {
                force_dual_channel: Some(false),
                enable_dither: true,
            },
        )
        .unwrap();
        assert_eq!(single & ((3 << 4) | (3 << 2)), 0);

        let dual = lvds_port_value_with_config(
            Pipe::B,
            Mode::XGA_1024X768_60,
            LvdsPortConfig {
                force_dual_channel: Some(true),
                enable_dither: true,
            },
        )
        .unwrap();
        assert_eq!(dual & ((3 << 4) | (3 << 2)), (3 << 4) | (3 << 2));
    }
}
