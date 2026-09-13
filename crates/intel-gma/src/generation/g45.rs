//! G45/GM965 Intel GMA generation support.
//!
//! This is the first real hardware modeset path, modeled after libgfxinit's
//! G45 GMCH PLL, pipe, plane, and connector sequencing. The live path covers
//! legacy GMCH VGA/LVDS/HDMI/DP flows, including DP AUX training with
//! libgfxinit-style link-setting retry and rollback.

use crate::dp_training::train_gmch_dp_with_retry;
use crate::error::GmaError;
use crate::framebuffer::{SurfaceConfig, TilingMode};
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
        unsafe { ctx.surface.fill_opaque_black()? };
        // No global teardown here: `GmaDisplayState` disables only the outputs
        // whose configuration changed, so enabling a second output leaves the
        // first running. Boot-state cleanup happens once in `clean()`.
        let panel = selected_lfp_panel(ctx);
        let cpu = ctx.config.cpu;
        pre_pll_enable_port(&mmio, pipeline.port, pipeline.pipe.pipe, pipeline.pipe.mode)?;
        if is_gmch_dp_port(pipeline.port) {
            train_gmch_dp_with_retry(
                &mmio,
                pipeline.port,
                pipeline.pipe.pipe,
                pipeline.pipe.mode,
                |config| {
                    // libgfxinit allocates the PLL per link setting, so the
                    // fixed DP tuple must match the candidate being tried.
                    program_pll_for_dp_rate(ctx, pipeline.pll, pipeline.port, config.link_rate)?;
                    Self::program_pipe(ctx, pipeline.pipe.pipe, pipeline.pipe.mode)?;
                    program_gmch_panel_fitter(ctx, pipeline.pipe.pipe, pipeline.pipe.mode)?;
                    Self::program_primary_plane(ctx, pipeline.plane.plane)?;
                    let _ = crate::port_detect::clear_hotplug_detect(&mmio, pipeline.port);
                    Ok(())
                },
                || {
                    disable_legacy_display_state(&mmio, cpu);
                    let _ = crate::port_detect::clear_hotplug_detect(&mmio, pipeline.port);
                },
            )?;
        } else {
            program_pll_for_port(ctx, &pipeline)?;
            Self::program_pipe(ctx, pipeline.pipe.pipe, pipeline.pipe.mode)?;
            program_gmch_panel_fitter(ctx, pipeline.pipe.pipe, pipeline.pipe.mode)?;
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
        let port = selected_port(ctx)?;
        let regs = PipeRegs::for_pipe(pipe)?;
        let pipe_config = crate::pipe::PipeConfig::new(pipe, mode);
        let mmio = ctx.mmio();
        // SAFETY: `regs.timing`/`regs.pipeconf` are generation-validated GMCH
        // pipe register offsets inside the decoded display MMIO BAR.
        let timing = unsafe { mmio.reg_block::<GmchPipeTimingRegs>(regs.timing) };
        // SAFETY: see the timing block above; pipeconf is a single typed register.
        let pipeconf = unsafe {
            mmio.reg_block::<fstart_core::mmio::MmioReadWrite<u32, PIPECONF::Register>>(
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
        pipeconf.set(PIPECONF::ENABLE::SET.value | crate::pipe::pipeconf_bpc_bits(port));
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
        let pipe = pipe_for_plane(plane)?;
        let regs = PlaneRegs::for_plane(plane)?;
        program_gmch_plane(
            &ctx.mmio(),
            regs.base,
            pipe,
            ctx.surface,
            crate::port::legacy_plane_address_model(ctx.config.cpu),
        )
    }

    fn enable_port(ctx: &mut GmaContext<'_>, port: Port, pipe: Pipe) -> Result<(), GmaError> {
        enable_port_with_mode(
            &ctx.mmio(),
            port,
            pipe,
            crate::choose_mode(ctx.resources, ctx.config)?,
        )
    }

    fn disable_output(mmio: &Mmio, cpu: Cpu, pipe: Pipe, port: Port) -> Result<(), GmaError> {
        if port == Port::Lvds {
            panel_backlight_off(mmio);
            panel_power_off(mmio);
        }
        disable_pipe_state(mmio, cpu, pipe);
        disable_port(mmio, port);
        let pll = crate::port::legacy_pll_for_pipe(pipe)?;
        pll::disable_legacy_pll(mmio, pll);
        Ok(())
    }

    fn clean(mmio: &Mmio, cpu: Cpu) {
        disable_legacy_display_state(mmio, cpu);
        disable_legacy_path(mmio, cpu);
    }
}

pub(crate) fn gmch_panel_regs(mmio: &Mmio) -> &'static GmchPanelRegs {
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

/// Program the legacy DPLL for a GMCH DisplayPort link rate.
///
/// libgfxinit uses fixed N/M1/M2/P1/P2 tuples for DP instead of searching the
/// pixel clock, and re-allocates the PLL for every link setting it tries.
fn program_pll_for_dp_rate(
    ctx: &GmaContext<'_>,
    pll: LegacyPll,
    port: Port,
    rate: crate::dp_aux::DpLinkRate,
) -> Result<(), GmaError> {
    let clock = pll::find_legacy_dp_clock(rate)?;
    let mmio = ctx.mmio();
    pll::disable_legacy_pll(&mmio, pll);
    delay_us(150);
    pll::program_legacy_pll(&mmio, ctx.config.cpu, pll, port, clock);
    Ok(())
}

/// Apply the port's pre-PLL plan, if it has one.
///
/// libgfxinit's GMCH `Connectors.Pre_On` is a no-op: LVDS is enabled in
/// `Post_On` after the PLL, pipe and plane are up, so there is nothing to do
/// before DPLL programming for the legacy GMCH ports we support.
fn pre_pll_enable_port(mmio: &Mmio, port: Port, pipe: Pipe, mode: Mode) -> Result<(), GmaError> {
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

pub(crate) fn apply_port_op(mmio: &Mmio, op: PortRegisterOp) {
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
    gtt::map_surface_to_stolen(ctx.resources, ctx.config.cpu, &ctx.surface)?;
    let mmio = ctx.mmio();
    gtt::clear_legacy_fences(&mmio, ctx.config.cpu);
    gtt::add_legacy_fence(&mmio, ctx.config.cpu, &ctx.surface)?;
    gtt::flush_gfx(&mmio);
    Ok(())
}

pub(crate) fn program_gmch_panel_fitter(
    ctx: &GmaContext<'_>,
    pipe: Pipe,
    mode: Mode,
) -> Result<(), GmaError> {
    let plan = scaler::ScalerPlan::resolve(
        ctx.config.cpu,
        pipe,
        ctx.surface,
        mode,
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
    let encoding = if matches!(crate::caps_for(ctx.config.cpu).generation, Generation::I945) {
        scaler::encode_gmch_pre_i965(
            ctx.surface.width,
            ctx.surface.height,
            mode.hdisplay,
            mode.vdisplay,
            ctx.config.framebuffer.scaling,
            24,
        )
    } else {
        scaler::encode_gmch_i965(
            pipe,
            ctx.surface.width,
            ctx.surface.height,
            mode.hdisplay,
            mode.vdisplay,
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

pub(crate) fn setup_gmch_panel_power_sequencer(mmio: &Mmio) {
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
    if plan.override_delays {
        apply_panel_op(mmio, plan.on_delays);
        apply_panel_op(mmio, plan.off_delays);
        apply_panel_op(mmio, plan.cycle_delay);
    }
    apply_panel_op(mmio, plan.control);
}

pub(crate) fn apply_panel_op(mmio: &Mmio, op: PanelRegisterOp) {
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

pub(crate) fn program_vbt_panel_registers(mmio: &Mmio, panel: LfpPanelMetadata) {
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
fn disable_legacy_path(mmio: &Mmio, cpu: Cpu) {
    for port in [
        Port::Vga,
        Port::Lvds,
        Port::HdmiA,
        Port::HdmiB,
        Port::DpA,
        Port::DpB,
        Port::DpC,
    ] {
        disable_port(mmio, port);
    }
    pll::disable_legacy_pll(mmio, LegacyPll::A);
    pll::disable_legacy_pll(mmio, LegacyPll::B);
    gtt::clear_legacy_fences(mmio, cpu);
}

/// Disable one legacy GMCH output port without touching its pipe or PLL.
pub(crate) fn disable_port(mmio: &Mmio, port: Port) {
    match port {
        Port::Lvds => apply_port_op(mmio, lvds_disable_op()),
        Port::Vga => apply_port_op(mmio, vga_disable_op()),
        Port::HdmiA | Port::HdmiB => {
            if let Ok(op) = hdmi_disable_op(port) {
                apply_port_op(mmio, op);
            }
        }
        Port::DpA | Port::DpB | Port::DpC => {
            // libgfxinit posts after both the idle and the zero write.
            for op in [dp_idle_op(port), dp_off_op(port)].into_iter().flatten() {
                let register = port_op_register(op);
                apply_port_op(mmio, op);
                mmio.posting_read(register);
            }
        }
        _ => {}
    }
}

/// Register address of a port operation.
const fn port_op_register(op: PortRegisterOp) -> usize {
    match op {
        PortRegisterOp::Write { register, .. } | PortRegisterOp::Update { register, .. } => {
            register
        }
    }
}

fn disable_legacy_display_state(mmio: &Mmio, cpu: Cpu) {
    legacy_vga_plane_off(mmio);
    for pipe in [Pipe::A, Pipe::B] {
        disable_pipe_state(mmio, cpu, pipe);
    }
}

/// Disable one legacy GMCH pipe: its plane, panel fitter and PIPECONF enable.
fn disable_pipe_state(mmio: &Mmio, cpu: Cpu, pipe: Pipe) {
    let Ok(regs) = PipeRegs::for_pipe(pipe) else {
        return;
    };
    // libgfxinit `Pipe_Setup.Off`: planes off, then the transcoder, then the
    // panel fitter. `Transcoder.Off` clears the enable and waits for the
    // enabled status to drop before the fitter is touched.
    planes_off(mmio, regs.plane);
    mmio.clear_bits32(regs.pipeconf, PIPECONF::ENABLE::SET.value);
    let mut timeout = 100_000u32;
    while timeout != 0 {
        if (mmio.read32(regs.pipeconf) & PIPECONF::ENABLED_STATUS::SET.value) == 0 {
            break;
        }
        timeout -= 1;
        core::hint::spin_loop();
    }
    panel_fitter_off_for_pipe(mmio, cpu, pipe);
}

pub(crate) fn legacy_vga_plane_off(mmio: &Mmio) {
    vga_sequencer_screen_off();
    // SAFETY: `GMCH_VGACNTRL_OFFSET` is the fixed legacy VGA control register
    // in the validated GMCH display MMIO BAR.
    let vga_control = unsafe {
        mmio.reg_block::<fstart_core::mmio::MmioReadWrite<u32, VGACNTRL::Register>>(
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

/// Program a legacy GMCH primary plane, following libgfxinit
/// `Setup_Hires_Plane`.
///
/// The control register is written without the enable bit first, then stride,
/// geometry and address, and only then enabled. pre-SKL hardware self-arms the
/// plane on the disabled-to-enabled transition and would otherwise latch stale
/// geometry values. Gen3 additionally requires size and position before the
/// address write that arms the double-buffered registers.
pub(crate) fn program_gmch_plane(
    mmio: &Mmio,
    base: usize,
    pipe: Pipe,
    surface: SurfaceConfig,
    address_model: PlaneAddressModel,
) -> Result<(), GmaError> {
    let plane = crate::plane::primary_for_pipe(pipe);
    let plane_config = crate::plane::PlaneConfig::new(plane, pipe, address_model, surface);
    // SAFETY: `base` is a generation-validated GMCH primary plane register
    // block inside the decoded display MMIO BAR.
    let regs = unsafe { mmio.reg_block::<GmchPlaneRegs>(base) };
    let pri = DSPCNTR::FORMAT::Xrgb8888.value | dspcntr_pipe_select(plane)?;
    let tiling = plane_config.legacy_tiling_bits();
    regs.cntr.set(pri | tiling);
    regs.stride.set(plane_config.stride_bytes()?);
    match address_model {
        PlaneAddressModel::Address => {
            regs.pos.set(0);
            regs.size.set(plane_config.encoded_size()?);
            if surface.tiling == TilingMode::Linear {
                regs.addr.set(plane_config.aperture_linear_address()?);
            } else {
                regs.addr.set(surface.plane_surface_offset());
            }
            let _ = regs.addr.get();
        }
        PlaneAddressModel::Surface => {
            if surface.tiling == TilingMode::Linear {
                regs.addr.set(plane_config.linear_offset_bytes()?);
                regs.tileoff.set(0);
            } else {
                regs.addr.set(0);
                regs.tileoff.set(plane_config.tile_offset());
            }
            regs.surf.set(surface.plane_surface_offset());
        }
    }
    regs.cntr.set(DSPCNTR::ENABLE::SET.value | pri | tiling);
    let _ = regs.cntr.get();
    Ok(())
}

fn planes_off(mmio: &Mmio, regs: PlaneRegs) {
    mmio.write32(regs.cursor_control, 0);
    mmio.clear_bits32(regs.sprite_control, DSPCNTR::ENABLE::SET.value);
    mmio.clear_bits32(regs.cntr, DSPCNTR::ENABLE::SET.value);
    mmio.write32(regs.surf, 0);
    mmio.posting_read(regs.surf);
}

pub(crate) fn panel_fitter_off_for_pipe(mmio: &Mmio, cpu: Cpu, pipe: Pipe) {
    let panel_regs = gmch_panel_regs(mmio);
    // Gen3 has no PFIT pipe-select field: the fitter is hardwired to pipe B.
    let owner = if matches!(
        cpu,
        Cpu::I945G | Cpu::I945GM | Cpu::Pineview | Cpu::PineviewM
    ) {
        Pipe::B
    } else {
        match panel_regs
            .pfit_control
            .extract()
            .read(PFIT_CONTROL::PIPE_SELECT)
        {
            1 => Pipe::B,
            2 => Pipe::C,
            _ => Pipe::A,
        }
    };
    if owner == pipe {
        // Clear every bit: clearing only ENABLE leaves stale Gen3 auto-scale
        // bits that confuse the hardware (libgfxinit `Panel_Fitter_Off`).
        panel_regs.pfit_control.set(0);
        let _ = panel_regs.pfit_control.get();
    }
}

pub(crate) fn panel_power_on(mmio: &Mmio) -> Result<(), GmaError> {
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
    // libgfxinit `Panel.Backlight_On` only opens the power-sequencer gate; the
    // duty cycle is set afterwards, so open the gate first.
    let panel_regs = gmch_panel_regs(mmio);
    let control = panel_regs.pp_control.get();
    panel_regs.pp_control.set(panel_control_unlocked(
        control | PP_CONTROL::BACKLIGHT_ENABLE::SET.value,
    ));
    let _ = panel_regs.pp_control.get();
    if panel
        .and_then(|panel| panel.backlight)
        .map(|backlight| backlight.is_pwm())
        .unwrap_or(false)
    {
        apply_panel_op(
            mmio,
            set_backlight_op(
                BacklightRegisterModel::Legacy {
                    duty_ctl: BLC_PWM_GMCH_CTL,
                    freq_ctl: BLC_PWM_GMCH_CTL2,
                },
                CPU_BLC_PWM_DUTY_MAX,
            ),
        );
    }
}

/// Disable the panel power-sequencer backlight gate (libgfxinit `Panel.Backlight_Off`).
pub(crate) fn panel_backlight_off(mmio: &Mmio) {
    let panel_regs = gmch_panel_regs(mmio);
    let control = panel_regs.pp_control.get();
    panel_regs.pp_control.set(panel_control_unlocked(
        control & !PP_CONTROL::BACKLIGHT_ENABLE::SET.value,
    ));
    let _ = panel_regs.pp_control.get();
}

/// Clear panel target power/VDD override and wait for the sequencer to settle
/// (libgfxinit `Panel.Off`).
pub(crate) fn panel_power_off(mmio: &Mmio) {
    let panel_regs = gmch_panel_regs(mmio);
    let was_on = panel_regs.pp_control.is_set(PP_CONTROL::TARGET_ON);
    let control = panel_regs.pp_control.get();
    panel_regs.pp_control.set(panel_control_unlocked(
        control & !(PP_CONTROL::TARGET_ON::SET.value | PP_CONTROL::VDD_OVERRIDE::SET.value),
    ));
    let _ = panel_regs.pp_control.get();
    if was_on {
        // libgfxinit `Panel.Off` waits the configured power-down delay.
        let delays = PanelPowerDelays::from_registers(
            panel_regs.pp_on_delays.get(),
            panel_regs.pp_off_delays.get(),
            panel_regs.pp_divisor.get(),
        )
        .with_defaults();
        delay_us(delays.power_down_us);
    }
    let mut timeout = 300_000u32;
    while timeout != 0 {
        if (panel_regs.pp_status.get() & PP_STATUS::SEQUENCE.mask) == 0 {
            break;
        }
        timeout -= 1;
        core::hint::spin_loop();
    }
}

const fn dspcntr_pipe_select(plane: Plane) -> Result<u32, GmaError> {
    match plane {
        Plane::PrimaryA => Ok(DSPCNTR::PIPE_SELECT::PipeA.value),
        Plane::PrimaryB => Ok(DSPCNTR::PIPE_SELECT::PipeB.value),
        Plane::PrimaryC => Err(GmaError::InvalidConfig),
    }
}

pub(crate) const fn panel_control_unlocked(control: u32) -> u32 {
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

pub(crate) const BLC_PWM_GMCH_CTL: usize = 0x61254;
pub(crate) const BLC_PWM_GMCH_CTL2: usize = 0x61250;
#[allow(dead_code)]
const CPU_BLC_PWM_DUTY_MAX: u32 = 0x0000_ffff;
pub(crate) const PP_CONTROL_UNLOCK_MASK: u32 = PP_CONTROL::UNLOCK_KEY.val(0xffff).value;
pub(crate) const PP_CONTROL_UNLOCK_KEY: u32 = PP_CONTROL::UNLOCK_KEY.val(0xabcd).value;
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
            0xd000_0000,
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
        // GNU/Linux i9xx (i965/G45) programs duty in BLC_PWM_CTL, not the
        // Ironlake CPU-side BLC_PWM_CPU_CTL.
        assert_eq!(BLC_PWM_GMCH_CTL, 0x61254);
        assert_eq!(BLC_PWM_GMCH_CTL2, 0x61250);
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
