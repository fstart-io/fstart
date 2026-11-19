//! CPU pipe, transcoder, primary plane, cursor and panel fitter (libgfxinit
//! `Pipe_Setup` and `Transcoder` for the Ironlake generation).

use super::{Caps, PortConfig};
use crate::gma::error::GmaError;
use crate::gma::framebuffer::{Rotation, SurfaceConfig, TilingMode};
use crate::gma::mmio::{Mmio, delay_us};
use crate::gma::mode::Mode;
use crate::gma::pipe::PipeConfig;
use crate::gma::plane::{PlaneAddressModel, PlaneConfig, primary_for_pipe};
use crate::gma::regs::{
    CPU_DSPCNTR, CPU_PIPECONF, CUR_CTL, CUR_POS, GMCH_DATA_M, GMCH_M_N, PF_CTL, VGACNTRL,
};
use crate::gma::scaler::{self, IronlakePfitEncoding, IronlakePfitRegs, ScalingPolicy};
use crate::gma::types::Pipe;

const CPU_VGACNTRL: usize = 0x41000;
/// libgfxinit waits 40 ms for a pipe to stop.
const PIPE_OFF_TIMEOUT_US: u64 = 40_000;
/// `Default_Cursor`: 64x64, centred on the origin.
const DEFAULT_CURSOR_WIDTH: u32 = 64;

/// Per-pipe CPU registers.
struct PipeRegs {
    timing: usize,
    pipesrc: usize,
    data_m: usize,
    data_n: usize,
    link_m: usize,
    link_n: usize,
    pipeconf: usize,
    dspcntr: usize,
    dsplinoff: usize,
    dspstride: usize,
    dspsurf: usize,
    dsptileoff: usize,
    plane_2_ctl: usize,
}

const fn pipe_regs(pipe: Pipe) -> PipeRegs {
    let stride = 0x1000 * pipe as usize;
    PipeRegs {
        timing: 0x60000 + stride,
        pipesrc: 0x6001c + stride,
        data_m: 0x60030 + stride,
        data_n: 0x60034 + stride,
        link_m: 0x60040 + stride,
        link_n: 0x60044 + stride,
        pipeconf: 0x70008 + stride,
        dspcntr: 0x70180 + stride,
        dsplinoff: 0x70184 + stride,
        dspstride: 0x70188 + stride,
        dspsurf: 0x7019c + stride,
        dsptileoff: 0x701a4 + stride,
        plane_2_ctl: 0x70280 + stride,
    }
}

/// Cursor registers (libgfxinit `Cursors`): Ironlake and Sandy Bridge put
/// cursor B next to cursor A, Ivy Bridge gives every pipe its own block.
struct CursorRegs {
    ctl: usize,
    base: usize,
    pos: usize,
    fbc_ctl: Option<usize>,
}

const fn cursor_regs(caps: Caps, pipe: Pipe) -> CursorRegs {
    if caps.ivybridge_cursors() {
        let stride = 0x1000 * pipe as usize;
        CursorRegs {
            ctl: 0x70080 + stride,
            base: 0x70084 + stride,
            pos: 0x70088 + stride,
            fbc_ctl: Some(0x700a0 + stride),
        }
    } else if matches!(pipe, Pipe::A) {
        CursorRegs {
            ctl: 0x70080,
            base: 0x70084,
            pos: 0x70088,
            fbc_ctl: None,
        }
    } else {
        CursorRegs {
            ctl: 0x700c0,
            base: 0x700c4,
            pos: 0x700c8,
            fbc_ctl: None,
        }
    }
}

/// Register values of libgfxinit `Setup_FB` that only depend on the
/// framebuffer and mode, computed once before the first attempt.
pub(super) struct DisplayPlan {
    dspcntr: u32,
    stride: u32,
    linear_offset: Option<u32>,
    tile_offset: u32,
    surface: u32,
    pipesrc: u32,
    fitter: Option<IronlakePfitEncoding>,
}

/// `DSPCNTR_MASK`.
const DSPCNTR_MASK: u32 = CPU_DSPCNTR::ENABLE::SET.value
    | CPU_DSPCNTR::GAMMA_CORRECTION::SET.value
    | CPU_DSPCNTR::FORMAT.val(0xf).value
    | CPU_DSPCNTR::PIPE_SELECT.val(3).value
    | CPU_DSPCNTR::DISABLE_TRICKLE_FEED::SET.value
    | CPU_DSPCNTR::TILED_SURFACE::SET.value;

impl DisplayPlan {
    /// Plan the plane, source size and fitter; rejects what libgfxinit's
    /// `Validate_Config` rejects on this generation (rotation, Y tiling).
    pub(super) fn new(
        caps: Caps,
        pipe: Pipe,
        surface: SurfaceConfig,
        mode: Mode,
        scaling: ScalingPolicy,
    ) -> Result<Self, GmaError> {
        if surface.rotation != Rotation::None || surface.tiling == TilingMode::Y {
            return Err(GmaError::InvalidConfig);
        }
        let plane = PlaneConfig::new(
            primary_for_pipe(pipe),
            pipe,
            PlaneAddressModel::Surface,
            surface,
        );
        let fitter = if scaler::requires_scaling(surface, mode) {
            Some(scaler::encode_ironlake_pfit(
                pipe,
                surface.width,
                surface.height,
                mode.hdisplay,
                mode.vdisplay,
                scaling,
                caps.pf_pipe_select(),
            )?)
        } else {
            None
        };
        Ok(Self {
            // Format XRGB8888 and `Disable_Trickle_Feed`; Ironlake planes have
            // no pipe select (`Has_DSPCNTR_Pipe_Select`).
            dspcntr: CPU_DSPCNTR::FORMAT::Xrgb8888.value
                | CPU_DSPCNTR::DISABLE_TRICKLE_FEED::SET.value
                | plane.legacy_tiling_bits(),
            stride: plane.stride_bytes()?,
            linear_offset: match surface.tiling {
                TilingMode::Linear => Some(plane.linear_offset_bytes()?),
                _ => None,
            },
            tile_offset: plane.tile_offset(),
            surface: surface.plane_surface_offset(),
            pipesrc: PipeConfig::surface_source(surface)?,
            fitter,
        })
    }
}

/// `TRANS_CONF_BPC` plus dithering when the 8 bpc framebuffer is reduced.
const fn pipeconf(bpc: u8) -> u32 {
    let bpc_bits = match bpc {
        6 => CPU_PIPECONF::BPC::Bits6.value,
        10 => CPU_PIPECONF::BPC::Bits10.value,
        12 => CPU_PIPECONF::BPC::Bits12.value,
        _ => CPU_PIPECONF::BPC::Bits8.value,
    };
    CPU_PIPECONF::ENABLE::SET.value
        | bpc_bits
        | if bpc != 8 {
            CPU_PIPECONF::DITHER::SET.value
        } else {
            0
        }
}

/// libgfxinit `Update_Cursor` with `Default_Cursor`: cursor off on `pipe`,
/// placed at its centre (0, 0).
fn update_cursor(mmio: &Mmio, caps: Caps, pipe: Pipe) {
    let regs = cursor_regs(caps, pipe);
    mmio.write32(
        regs.ctl,
        CUR_CTL::MODE::Off.value | CUR_CTL::PIPE_SELECT.val(pipe as u32).value,
    );
    let half = DEFAULT_CURSOR_WIDTH / 2;
    mmio.write32(
        regs.pos,
        CUR_POS::Y_SIGN::SET.value
            | CUR_POS::Y.val(half).value
            | CUR_POS::X_SIGN::SET.value
            | CUR_POS::X.val(half).value,
    );
    mmio.write32(regs.base, 0);
}

/// libgfxinit `Setup_Hires_Plane`: control without enable, geometry and
/// surface, then the enabling write.
fn setup_plane(mmio: &Mmio, regs: &PipeRegs, plan: &DisplayPlan) {
    mmio.update32(regs.dspcntr, DSPCNTR_MASK, plan.dspcntr);
    mmio.write32(regs.dspstride, plan.stride);
    match plan.linear_offset {
        Some(offset) => {
            mmio.write32(regs.dsplinoff, offset);
            mmio.write32(regs.dsptileoff, 0);
        }
        None => {
            mmio.write32(regs.dsplinoff, 0);
            mmio.write32(regs.dsptileoff, plan.tile_offset);
        }
    }
    mmio.write32(regs.dspsurf, plan.surface);
    mmio.write32(regs.dspcntr, CPU_DSPCNTR::ENABLE::SET.value | plan.dspcntr);
}

/// libgfxinit `Display_Controller.On`: `Transcoder.Setup`, `Setup_FB`, the
/// cursor and `Transcoder.On`.
pub(super) fn on(mmio: &Mmio, caps: Caps, pipe: Pipe, cfg: &PortConfig, plan: &DisplayPlan) {
    let regs = pipe_regs(pipe);
    let mode = cfg.mode;
    // `Transcoder.Setup`: FDI M/N, or eDP M/N on port A.
    let link = if cfg.route.fdi.is_some() {
        cfg.fdi_link
    } else {
        cfg.dp_link
    };
    let mn = link.m_n(mode, u64::from(cfg.bpc));
    mmio.write32(
        regs.data_m,
        (GMCH_DATA_M::TU_SIZE.val(64 - 1) + GMCH_DATA_M::VALUE.val(mn.data_m)).value,
    );
    mmio.write32(regs.data_n, GMCH_M_N::VALUE.val(mn.data_n).value);
    mmio.write32(regs.link_m, GMCH_M_N::VALUE.val(mn.link_m).value);
    mmio.write32(regs.link_n, GMCH_M_N::VALUE.val(mn.link_n).value);
    for (offset, value) in [
        (0x00, PipeConfig::encode_range(mode.hdisplay, mode.htotal)),
        (0x04, PipeConfig::encode_range(mode.hdisplay, mode.htotal)),
        (
            0x08,
            PipeConfig::encode_range(mode.hsync_start, mode.hsync_end),
        ),
        (0x0c, PipeConfig::encode_range(mode.vdisplay, mode.vtotal)),
        (0x10, PipeConfig::encode_range(mode.vdisplay, mode.vtotal)),
        (
            0x14,
            PipeConfig::encode_range(mode.vsync_start, mode.vsync_end),
        ),
    ] {
        mmio.write32(regs.timing + offset, value);
    }
    // `Setup_FB`.
    update_cursor(mmio, caps, pipe);
    setup_plane(mmio, &regs, plan);
    mmio.write32(regs.pipesrc, plan.pipesrc);
    let fitter = IronlakePfitRegs::for_pipe(pipe);
    match plan.fitter {
        Some(encoding) => {
            mmio.write32(fitter.control, encoding.control);
            mmio.write32(fitter.win_pos, encoding.win_pos);
            mmio.write32(fitter.win_size, encoding.win_size);
        }
        None => panel_fitter_off(mmio, pipe),
    }
    update_cursor(mmio, caps, pipe);
    // `Transcoder.On`.
    mmio.write32(regs.pipeconf, pipeconf(cfg.bpc));
    mmio.posting_read(regs.pipeconf);
}

/// libgfxinit `Planes_Off`.
fn planes_off(mmio: &Mmio, caps: Caps, pipe: Pipe) {
    let cursor = cursor_regs(caps, pipe);
    let regs = pipe_regs(pipe);
    mmio.write32(cursor.ctl, 0);
    if let Some(fbc_ctl) = cursor.fbc_ctl {
        mmio.write32(fbc_ctl, 0);
    }
    mmio.update32(regs.plane_2_ctl, CPU_DSPCNTR::ENABLE::SET.value, 0);
    mmio.update32(regs.dspcntr, CPU_DSPCNTR::ENABLE::SET.value, 0);
}

/// libgfxinit `Transcoder.Off` for the CPU pipe.
fn transcoder_off(mmio: &Mmio, pipe: Pipe) {
    let pipeconf = pipe_regs(pipe).pipeconf;
    let enable = CPU_PIPECONF::ENABLE::SET.value;
    if mmio.read32(pipeconf) & enable != 0 {
        mmio.update32(pipeconf, enable, 0);
    }
    let _ = mmio.wait_mask(
        pipeconf,
        CPU_PIPECONF::ENABLED_STATUS::SET.value,
        0,
        PIPE_OFF_TIMEOUT_US,
    );
}

/// libgfxinit `Panel_Fitter_Off`.
fn panel_fitter_off(mmio: &Mmio, pipe: Pipe) {
    let fitter = IronlakePfitRegs::for_pipe(pipe);
    mmio.update32(fitter.control, PF_CTL::ENABLE::SET.value, 0);
    mmio.write32(fitter.win_size, 0);
}

/// libgfxinit `Display_Controller.Off`.
pub(super) fn off(mmio: &Mmio, caps: Caps, pipe: Pipe) {
    planes_off(mmio, caps, pipe);
    transcoder_off(mmio, pipe);
    panel_fitter_off(mmio, pipe);
}

/// libgfxinit `Legacy_VGA_Off`: blank the VGA sequencer, then disable the
/// VGA plane.
fn legacy_vga_off(mmio: &Mmio) {
    vga_screen_off();
    delay_us(100);
    mmio.update32(CPU_VGACNTRL, 0, VGACNTRL::VGA_DISPLAY_DISABLE::SET.value);
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn vga_screen_off() {
    const VGA_SR_INDEX: u16 = 0x3c4;
    const VGA_SR_DATA: u16 = 0x3c5;
    const VGA_SR01: u8 = 0x01;
    const VGA_SR01_SCREEN_OFF: u8 = 1 << 5;
    // SAFETY: the architectural VGA sequencer index/data ports.
    unsafe {
        fstart_core::pio::outb(VGA_SR_INDEX, VGA_SR01);
        let value = fstart_core::pio::inb(VGA_SR_DATA);
        fstart_core::pio::outb(VGA_SR_DATA, value | VGA_SR01_SCREEN_OFF);
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn vga_screen_off() {}

/// libgfxinit `Display_Controller.All_Off`.
pub(super) fn all_off(mmio: &Mmio, caps: Caps) {
    legacy_vga_off(mmio);
    for pipe in caps.pipes() {
        off(mmio, caps, *pipe);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gma::framebuffer::PixelFormat;
    use crate::gma::types::Cpu;

    #[test]
    fn cursor_blocks_follow_the_cpu() {
        let snb = Caps::new(Cpu::Sandybridge).unwrap();
        let ivb = Caps::new(Cpu::Ivybridge).unwrap();
        assert_eq!(cursor_regs(snb, Pipe::B).ctl, 0x700c0);
        assert_eq!(cursor_regs(snb, Pipe::B).fbc_ctl, None);
        assert_eq!(cursor_regs(ivb, Pipe::B).ctl, 0x71080);
        assert_eq!(cursor_regs(ivb, Pipe::C).fbc_ctl, Some(0x720a0));
    }

    #[test]
    fn pipeconf_dithers_reduced_depths() {
        assert_eq!(pipeconf(6), (1 << 31) | (2 << 5) | (1 << 4));
        assert_eq!(pipeconf(8), 1 << 31);
    }

    #[test]
    fn plan_uses_the_fitter_only_when_sizes_differ() {
        let snb = Caps::new(Cpu::Sandybridge).unwrap();
        let mode = Mode {
            hdisplay: 1366,
            ..Mode::XGA_1024X768_60
        };
        let surface = SurfaceConfig::packed(0, 1366, 768, PixelFormat::Xrgb8888);
        let exact =
            DisplayPlan::new(snb, Pipe::A, surface, mode, ScalingPolicy::PreserveAspect).unwrap();
        assert!(exact.fitter.is_none());
        assert_eq!(exact.pipesrc, (1365 << 16) | 767);
        assert_eq!(exact.linear_offset, Some(0));
        assert_eq!(exact.dspcntr, (6 << 26) | (1 << 14));
        let small = SurfaceConfig::packed(0, 1024, 768, PixelFormat::Xrgb8888);
        let fit =
            DisplayPlan::new(snb, Pipe::A, small, mode, ScalingPolicy::PreserveAspect).unwrap();
        assert_eq!(fit.fitter.unwrap().win_pos, 171 << 16);
    }
}
