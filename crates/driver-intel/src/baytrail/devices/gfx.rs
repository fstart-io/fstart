//! Graphics (00:02.0): stolen memory sizing before raminit, then power
//! wells, power management, panel power sequencing and the locks
//! (coreboot `romstage/gfx.c` and `gfx.c`). No VBIOS runs; the OS driver
//! sets the mode from the OpRegion's VBT.

use super::{Bar, poll_bar};
use crate::MmioBar;
use crate::baytrail::dev;
use crate::baytrail::iosf::{Iosf, PUNIT};
use crate::baytrail::punit::{self, SB_BIOS_CONFIG};
use crate::baytrail::raminit::{MemoryMap, Stolen};
use crate::igd::{OpRegionSci, VbtSource};
use fstart_core::services::ServiceError;
use fstart_pci::ConfigRegionAccess;
use fstart_pci::ecam::EcamDevice;
use tock_registers::interfaces::{ReadWriteable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields};

register_bitfields![u32,
    pub GGC [
        LOCK OFFSET(0) NUMBITS(1) [],
        VGA_DISABLE OFFSET(1) NUMBITS(1) [],
        /// Graphics stolen memory, 32 MiB units.
        GSM_SIZE OFFSET(3) NUMBITS(5) [],
        /// GTT stolen memory in MiB.
        GTT_SIZE OFFSET(8) NUMBITS(2) []
    ]
];
register_bitfields![u8,
    pub MSAC [ APERTURE_SIZE OFFSET(1) NUMBITS(2) [Mb128 = 0, Mb256 = 1, Mb512 = 3] ]
];

const GGC_REG: u16 = 0x50;
const GSM_BASE: u16 = 0x5c;
const MSAC_REG: u16 = 0x62;
const GTT_BASE: u16 = 0x70;
const VLV_DISPLAY_BASE: u32 = 0x18_0000;
const PP_CONTROL: u32 = 0x61204;
const PP_ON_DELAYS: u32 = 0x61208;
const PP_OFF_DELAYS: u32 = 0x6120c;
const PP_DIVISOR: u32 = 0x61210;
const BACKLIGHT_CTL: u32 = 0x61254;
const PP_CONTROL_UNLOCK: u32 = 0xabcd_0000;
const PP_CONTROL_EDP_FORCE_VDD: u32 = 1 << 3;
/// 100 ms.
const TIMEOUT_US: u32 = 100_000;

/// Panel power sequencing of one pipe.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PanelPipe {
    /// 1 = DP B, 2 = DP C.
    pub port_select: u8,
    pub power_on_delay: u16,
    pub light_on_delay: u16,
    pub power_off_delay: u16,
    pub light_off_delay: u16,
    pub power_cycle_delay: u16,
    /// Backlight PWM; 0 means 200 Hz.
    pub pwm_freq_hz: u32,
}

/// Graphics policy.
#[derive(Debug, Clone, Copy)]
pub struct GfxConfig {
    pub vbt: Option<VbtSource>,
    pub pipe_a: Option<PanelPipe>,
    pub pipe_b: Option<PanelPipe>,
}

impl GfxConfig {
    pub const fn new() -> Self {
        Self {
            vbt: None,
            pipe_a: None,
            pipe_b: None,
        }
    }
}

impl Default for GfxConfig {
    fn default() -> Self {
        Self::new()
    }
}

fn igd() -> EcamDevice {
    EcamDevice::new(0, dev::GFX, 0)
}

/// Stolen memory sizes before raminit: 64 MiB GSM (32 MiB needs C0),
/// 2 MiB GTT, VGA decode and a 256 MiB aperture.
pub fn pre_raminit() {
    let igd = igd();
    let mut ggc = LocalRegisterCopy::<u32, GGC::Register>::new(igd.read32(GGC_REG));
    ggc.modify(GGC::GTT_SIZE.val(2) + GGC::GSM_SIZE.val(2) + GGC::VGA_DISABLE::CLEAR);
    let mut msac = LocalRegisterCopy::<u8, MSAC::Register>::new(igd.read8(MSAC_REG));
    msac.modify(MSAC::APERTURE_SIZE::Mb256);
    igd.write32(GGC_REG, ggc.get());
    igd.write8(MSAC_REG, msac.get());
}

/// Graphics and GTT stolen sizes in MiB, as programmed in GGC.
pub fn stolen() -> Stolen {
    let ggc = LocalRegisterCopy::<u32, GGC::Register>::new(igd().read32(GGC_REG));
    let gsm = ggc.read(GGC::GSM_SIZE);
    Stolen {
        gsm_mb: if gsm <= 16 { gsm * 32 } else { 0 },
        gtt_mb: ggc.read(GGC::GTT_SIZE),
    }
}

/// Point GSM_BASE and GTT_BASE at the carve-outs raminit chose.
pub fn set_stolen_bases(map: &MemoryMap) {
    let igd = igd();
    igd.modify32(GSM_BASE, 0xf_ffff, map.gsm_base_mb << 20);
    igd.modify32(GTT_BASE, 0xf_ffff, map.gtt_base_mb << 20);
}

fn bar0(igd: &EcamDevice) -> Option<Bar> {
    igd.memory_bar(0x10).map(|base| Bar(base as usize))
}

fn punit_power_gate<P: ConfigRegionAccess>(
    iosf: &Iosf<P>,
    control: u32,
    mask: u32,
    status: u32,
) -> Result<(), ServiceError> {
    iosf.write(PUNIT, punit::PWRGT_CONTROL.address(), control);
    for _ in 0..TIMEOUT_US / 10 {
        if iosf.read(PUNIT, punit::PWRGT_STATUS.address()) & mask == status {
            return Ok(());
        }
        fstart_arch::x86::udelay(10);
    }
    fstart_log::error!("baytrail: GFX power gate {:#x} timeout", control);
    Err(ServiceError::Timeout)
}

/// Bus master and memory decode, then ungate the display, the lanes and
/// the display controller (coreboot `gpu_pre_vbios_script`).
fn pre_vbios<P: ConfigRegionAccess>(igd: &EcamDevice, iosf: &Iosf<P>) -> Result<(), ServiceError> {
    igd.or32(0x04, 0x6);
    for (control, mask, status) in [
        // Display.
        (0xc0, 0xc0, 0xc0),
        // TX/RX lanes.
        (0xfff0c0, 0xfff0c0, 0xfff0c0),
        // Common lane.
        (0xfffcc0, 0xfffcc0, 0xfffcc0),
        // Ungate TX only.
        (0xf00cc0, 0xfffcc0, 0xf00cc0),
        // Ungate the common lane only.
        (0xf000c0, 0xffffc0, 0xf000c0),
        // Ungate the display.
        (0xf00000, 0xfffff0, 0xf00000),
    ] {
        punit_power_gate(iosf, control, mask, status)?;
    }
    Ok(())
}

/// PowerMeter weights: OR-ed into BAR0, as the reference writes them.
const POWER_METER: &[(u32, u32)] = &[
    (0xa800, 0x0000_0000),
    (0xa804, 0x0000_0000),
    (0xa808, 0x0000_ff0a),
    (0xa80c, 0x1d00_0000),
    (0xa810, 0xac00_4900),
    (0xa814, 0x000f_0000),
    (0xa818, 0x5a00_0000),
    (0xa81c, 0x2600_001f),
    (0xa820, 0x0009_0000),
    (0xa824, 0x2000_ff00),
    (0xa828, 0xff09_0016),
    (0xa82c, 0x0000_0000),
    (0xa830, 0x0000_0100),
    (0xa834, 0x00a0_0f51),
    (0xa838, 0x000b_0000),
    (0xa83c, 0xcb7d_3307),
    (0xa840, 0x003c_0000),
    (0xa844, 0xffff_0000),
    (0xa848, 0x0022_0000),
    (0xa84c, 0x4300_0000),
    (0xa850, 0x0000_0800),
    (0xa854, 0x0000_0f00),
    (0xa858, 0x0000_0021),
    (0xa85c, 0x0000_0000),
    (0xa860, 0x0019_0000),
    (0xaa80, 0x00ff_00ff),
    (0xaa84, 0x0000_0000),
    (0x1300a4, 0x0000_0000),
    // Set 2.
    (0xa900, 0x0000_0000),
    (0xa904, 0x0000_0000),
    (0xa908, 0x0000_0000),
    (0xa90c, 0x1d00_0000),
    (0xa910, 0xac00_5000),
    (0xa914, 0x000f_0000),
    (0xa918, 0x5a00_0000),
    (0xa91c, 0x2600_001f),
    (0xa920, 0x0009_0000),
    (0xa924, 0x2000_ff00),
    (0xa928, 0xff09_0016),
    (0xa92c, 0x0000_0000),
    (0xa930, 0x0000_0100),
    (0xa934, 0x00a0_0f51),
    (0xa938, 0x000b_0000),
    (0xa93c, 0xcb7d_3307),
    (0xa940, 0x003c_0000),
    (0xa944, 0xffff_0000),
    (0xa948, 0x0022_0000),
    (0xa94c, 0x4300_0000),
    (0xa950, 0x0000_0800),
    (0xa954, 0x0000_0000),
    (0xa960, 0x0000_0000),
    // Set 3.
    (0xaa3c, 0x0000_0000),
    (0xaa54, 0x0000_0000),
    (0xaa60, 0x0000_0000),
    // Enable the counters.
    (0xa248, 0x0000_0058),
];

/// RC6, RC-state and turbo settings: OR-ed into BAR0.
const RC_TURBO: &[(u32, u32)] = &[
    (0xa090, 0x0000_0000),
    (0xa09c, 0x0028_0000),
    (0xa0a8, 0x0001_e848),
    (0xa0ac, 0x0000_0019),
    (0xa0b0, 0x0000_0000),
    (0xa0b8, 0x0000_0557),
    // Render/video/blitter idle max count.
    (0x2054, 0x0000_000a),
    (0x12054, 0x0000_000a),
    (0x22054, 0x0000_000a),
    // RP down timeout, up/down thresholds and evaluation intervals.
    (0xa010, 0x000f_4240),
    (0xa02c, 0x0000_e8e8),
    (0xa030, 0x0003_bd08),
    (0xa068, 0x0001_01d0),
    (0xa06c, 0x0005_5730),
];

/// Wake the wells and program power management (coreboot
/// `gfx_init_script`), then lock the power context base.
fn pm_init<P: ConfigRegionAccess>(
    igd: &EcamDevice,
    bar: Bar,
    iosf: &Iosf<P>,
) -> Result<(), ServiceError> {
    // Allow-wake the render and media wells, then force them awake.
    bar.modify32(0x130090, !1, 1);
    poll_bar(bar, 0x130094, 1, 1, TIMEOUT_US)?;
    bar.write32(0x1300b0, 0x8000_8000);
    poll_bar(bar, 0x1300b4, 0x8000, 0x8000, TIMEOUT_US)?;
    bar.write32(0x1300b8, 0x8000_8000);
    poll_bar(bar, 0x1300bc, 0x8000, 0x8000, TIMEOUT_US)?;
    // Workaround X0:261954/A0:261955.
    bar.modify32(0x182060, !0xf, 1);
    for &(offset, bits) in POWER_METER {
        bar.setbits32(offset, bits);
    }
    // GPU EC virus: 0xcf08 except for SDP profile 4.
    iosf.reg(punit::GPU_EC_VIRUS).set(0xcf08);
    for (offset, value) in [
        // GfxPause.
        (0xa000, 0x0007_1388),
        // Dynamic EU control.
        (0xa080, 0x0000_0004),
        // Lock ECO bits.
        (0xa180, 0x8000_0000),
        // DOP clock gating.
        (0x9424, 0x0000_0001),
    ] {
        bar.write32(offset, value);
    }
    // MBC sends the fuse (VCR) writes as non-posted.
    bar.modify32(0x907c, 0xfffe_ffff, 0x0001_0000);
    for &(offset, bits) in RC_TURBO {
        bar.setbits32(offset, bits);
    }
    for (offset, value) in [
        // RP idle hysteresis, HW RC6 control, RP control.
        (0xa070, 0x0000_000a),
        (0xa090, 0x1100_0000),
        (0xa024, 0x0000_0592),
        // PM interrupts.
        (0x44024, 0x0300_0000),
        (0x4402c, 0x0300_0076),
        (0xa168, 0x0000_007e),
        // Aggressive clock gating.
        (0x9400, 0),
        (0x9404, 0),
        (0x9408, 0),
        (0x940c, 0),
    ] {
        bar.write32(offset, value);
    }
    iosf.reg(punit::SB_BIOS_CONFIG_REG)
        .modify(SB_BIOS_CONFIG::GFX_TURBO_DIS::CLEAR);

    // Lock the 24 KiB power context save area below the top of GSM, under
    // the WOPCM.
    let gsm = LocalRegisterCopy::<u32, GGC::Register>::new(igd.read32(GGC_REG)).read(GGC::GSM_SIZE);
    if gsm <= 16 {
        let gmsize = gsm * 32;
        let pcbase = (igd.read32(GSM_BASE) & 0xfff0_0000)
            .wrapping_add(gmsize.wrapping_sub(1).wrapping_mul(0x10_0000))
            .wrapping_sub(24 << 10);
        bar.write32(0x182120, pcbase | 1);
    }
    Ok(())
}

fn panel_setup(bar: Bar, pipe: &PanelPipe, offset: u32) {
    let reg = |r| VLV_DISPLAY_BASE + offset + r;
    bar.write32(
        reg(PP_CONTROL),
        PP_CONTROL_UNLOCK | PP_CONTROL_EDP_FORCE_VDD,
    );
    bar.write32(
        reg(PP_ON_DELAYS),
        u32::from(pipe.port_select) << 30
            | u32::from(pipe.power_on_delay) << 16
            | u32::from(pipe.light_on_delay),
    );
    bar.write32(
        reg(PP_OFF_DELAYS),
        u32::from(pipe.power_off_delay) << 16 | u32::from(pipe.light_off_delay),
    );
    bar.modify32(reg(PP_DIVISOR), !0x1f, u32::from(pipe.power_cycle_delay));
    // 25 MHz base clock; only the divider, not the duty cycle.
    let hz = if pipe.pwm_freq_hz == 0 {
        200
    } else {
        pipe.pwm_freq_hz
    };
    bar.write32(reg(BACKLIGHT_CTL), (25_000_000 / (16 * hz)) << 16);
}

/// Release force-wake and lock GGC, GSM_BASE and GTT_BASE (coreboot
/// `gfx_post_vbios_script`).
fn post_vbios(igd: &EcamDevice, bar: Bar) -> Result<(), ServiceError> {
    bar.write32(0x1300b0, 0x8000_0000);
    poll_bar(bar, 0x1300b4, 0x8000, 0, TIMEOUT_US)?;
    bar.write32(0x1300b8, 0x8000_0000);
    poll_bar(bar, 0x1300bc, 0x8000, 0, TIMEOUT_US)?;
    for reg in [GGC_REG, GSM_BASE, GTT_BASE] {
        igd.or32(reg, 1);
    }
    Ok(())
}

/// Ramstage graphics init after PCI allocation.
pub fn init<P: ConfigRegionAccess>(config: &GfxConfig, iosf: &Iosf<P>) -> Result<(), ServiceError> {
    let igd = igd();
    if !igd.is_present() {
        return Ok(());
    }
    let Some(bar) = bar0(&igd) else {
        fstart_log::warn!("baytrail: GFX BAR0 unassigned, skipping graphics init");
        return Ok(());
    };
    fstart_log::info!("baytrail: GFX pre-VBIOS init");
    pre_vbios(&igd, iosf)?;
    fstart_log::info!("baytrail: GFX power management init");
    pm_init(&igd, bar, iosf)?;
    for (pipe, offset) in [(config.pipe_a, 0), (config.pipe_b, 0x100)] {
        if let Some(pipe) = pipe.filter(|pipe| pipe.port_select != 0) {
            panel_setup(bar, &pipe, offset);
        }
    }
    fstart_log::info!("baytrail: GFX post-VBIOS init");
    post_vbios(&igd, bar)
}

/// Publish the OpRegion with the board's VBT.
pub fn publish_opregion(config: &GfxConfig) {
    let igd = igd();
    if let Some(vbt) = config.vbt.as_ref().filter(|_| igd.is_present()) {
        crate::igd::publish_opregion(&igd, OpRegionSci::Swsmisci, vbt);
    }
}
