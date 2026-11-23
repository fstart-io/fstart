//! Haswell GT power management, display clock, panel wiring, Mini-HD, OpRegion,
//! and native `gma::Haswell` display bring-up.

use crate::{
    gma::{
        panel::{HaswellBacklightPwmPlan, PanelPowerDelays, PanelPowerRegs, PanelRegisterOp},
        types::Cpu,
    },
    igd::{IgdDisplayPolicy, VbtSource},
    southbridge::hda::{HdaConfig, HdaController, HdaVerbTable},
};
use fstart_core::{mmio::MmioWindow, services::ServiceError};
use fstart_pci::ecam::EcamDevice;

/// GTTMMADR size on Haswell: 2 MiB of display/GT MMIO + 2 MiB of GTT PTEs.
pub(crate) const GTTMMADR_SIZE: u32 = 4 << 20;

/// Mini-HD (`00:03.0`) BAR0 aperture size (16 KiB).
const MINIHD_BAR_SIZE: u32 = 0x4000;

/// Board panel wiring and backlight policy for Haswell IGD.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Panel {
    /// Hotplug pulse programming for PCH ports B, C, and D (`SHOTPLUG_CTL`).
    pub hotplug: [u8; 3],
    /// Panel power-sequencer delays (T1..T5 / T4).
    pub delays: Option<PanelPowerDelays>,
    /// PCH backlight PWM frequency in Hz (`0` leaves PWM untouched).
    pub backlight_pwm_hz: u32,
    /// True when the backlight PWM output is active-low.
    pub backlight_active_low: bool,
    /// True when DDI-E is connected (leaves DDI-A at 2 lanes instead of 4).
    pub ddi_e_connected: bool,
}

impl Panel {
    /// Empty default panel configuration.
    pub const DEFAULT: Self = Self {
        hotplug: [0; 3],
        delays: None,
        backlight_pwm_hz: 0,
        backlight_active_low: false,
        ddi_e_connected: false,
    };
}

/// Board IGD policy for Haswell (panel wiring, OpRegion VBT, native modeset,
/// and ACPI brightness table).
#[derive(Clone, Copy, Debug)]
pub struct HaswellIgdConfig {
    /// Panel hotplug, power delays, backlight PWM, and DDI-E wiring.
    pub panel: Panel,
    /// Video BIOS Table source for OpRegion 2.0 publication and native modeset.
    pub vbt: Option<VbtSource>,
    /// Outputs and framebuffer policy for native `gma::Haswell` modeset;
    /// `None` leaves scanout to the payload/OS.
    pub display: Option<IgdDisplayPolicy>,
    /// Selectable LCD brightness percentages (ascending, including 0 and 100).
    /// Empty selects every percentage `0..=100`.
    pub brightness_levels: &'static [u8],
}

impl HaswellIgdConfig {
    /// Default headless Haswell IGD configuration.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            panel: Panel::DEFAULT,
            vbt: None,
            display: None,
            brightness_levels: &[],
        }
    }
}

impl Default for HaswellIgdConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Minimal GT/display MMIO abstraction over [`MmioWindow`] so host unit tests
/// can record register writes without touching hardware.
trait GtMmio: Copy {
    fn read32(self, offset: u32) -> u32;
    fn write32(self, offset: u32, value: u32);

    fn setbits32(self, offset: u32, bits: u32) {
        self.write32(offset, self.read32(offset) | bits);
    }

    fn clrbits32(self, offset: u32, bits: u32) {
        self.write32(offset, self.read32(offset) & !bits);
    }

    fn clrsetbits32(self, offset: u32, clear: u32, set: u32) {
        self.write32(offset, (self.read32(offset) & !clear) | set);
    }

    fn modify32(self, offset: u32, mask: u32, set: u32) {
        self.write32(offset, (self.read32(offset) & mask) | set);
    }
}

impl GtMmio for MmioWindow {
    fn read32(self, offset: u32) -> u32 {
        MmioWindow::read32(self, offset)
    }

    fn write32(self, offset: u32, value: u32) {
        MmioWindow::write32(self, offset, value);
    }
}

/// Classify the Haswell `gma::types::Cpu` SKU from ULT flag and IGD PCI device ID.
#[must_use]
pub(crate) const fn cpu_variant(ult: bool, device_id: u16) -> Cpu {
    if matches!(device_id, 0x0a0e | 0x0a1e) {
        Cpu::HaswellUlx
    } else if ult {
        Cpu::HaswellUlt
    } else {
        Cpu::Haswell
    }
}

/// Encode `PCH_PP_ON_DELAYS` directly from board-configured delays
/// (coreboot `gma_setup_panel`: both T1+T2 and T5 in 100 us units).
#[must_use]
pub(crate) const fn board_on_delays(delays: PanelPowerDelays) -> u32 {
    ((delays.power_up_us.div_ceil(100) & 0x1fff) << 16)
        | (delays.power_up_to_backlight_on_us.div_ceil(100) & 0x1fff)
}

/// Validate that `hz` produces a representable 16-bit PWM period before
/// building the [`HaswellBacklightPwmPlan`].
fn backlight_pwm_plan(lp: bool, hz: u32, active_low: bool) -> Option<HaswellBacklightPwmPlan> {
    if hz == 0 {
        return None;
    }
    let clock: u32 = if lp { 24_000_000 } else { 135_000_000 };
    let fine = hz > clock / 128 / 100;
    let period = clock / if fine { 16 } else { 128 } / hz;
    if !(1..=0xffff).contains(&period) {
        fstart_log::warn!("GT: skipping unrepresentable {} Hz PWM", hz);
        return None;
    }
    HaswellBacklightPwmPlan::new(lp, hz, active_low, 50)
}

fn apply_panel_op(bar: impl GtMmio, op: PanelRegisterOp) {
    match op {
        PanelRegisterOp::Write { register, value } => bar.write32(register as u32, value),
        PanelRegisterOp::Update {
            register,
            mask_unset,
            mask_set,
        } => bar.clrsetbits32(register as u32, mask_unset, mask_set),
        PanelRegisterOp::Set { register, mask } => bar.setbits32(register as u32, mask),
        PanelRegisterOp::Clear { register, mask } => bar.clrbits32(register as u32, mask),
    }
}

fn wait(bar: impl GtMmio, offset: u32, mask: u32, want: u32) -> Result<(), ServiceError> {
    let ready = || bar.read32(offset) & mask == want;
    if ready() {
        return Ok(());
    }
    fstart_arch::x86::wait_us(10_000, ready)
        .then_some(())
        .ok_or(ServiceError::Timeout)
        .inspect_err(|_| {
            fstart_log::warn!(
                "GT {:#x}: status {:#x}, expected {:#x}/{:#x}",
                offset,
                bar.read32(offset),
                want,
                mask
            );
        })
}

fn pcode(bar: impl GtMmio, cmd: u32, data: u32) -> Result<(), ServiceError> {
    wait(bar, 0x0013_8124, 1 << 31, 0)?;
    bar.write32(0x0013_8128, data);
    bar.write32(0x0013_812c, 0);
    bar.write32(0x0013_8124, (1 << 31) | cmd);
    wait(bar, 0x0013_8124, 1 << 31, 0)
}

tock_registers::register_bitfields![u32,
    /// `FORCEWAKE_ACK_HSW`, GT MMIO `0x130044` (32 bits).
    ForceAck [ ACTIVE OFFSET(0) NUMBITS(1) [] ]
];

const FORCE_WAKE: u32 = 0xa188;
const FORCE_ACK: u32 = 0x0013_0044;
const HSW_PWR_WELL_CTL1: u32 = 0x45400;

/// Program GT power management, power well, PCH panel wiring/PWM, DDI-A
/// detection, and CDCLK inside a force-wake bracket.
///
/// Caller must have already enabled `PCI_COMMAND_MEMORY | PCI_COMMAND_MASTER`
/// on `00:02.0` and verified `gtt_mmio` is non-zero.
pub(super) fn init_display_engine(gtt_mmio: u64, lp: bool, panel: Panel, device_id: u16) {
    // SAFETY: `gtt_mmio` is the PCI-assigned, memory-decoded 4 MiB GTTMMADR BAR.
    let bar = unsafe { MmioWindow::new(gtt_mmio as usize, GTTMMADR_SIZE) };
    bar.setbits32(0xa180, 32);
    with_force_wake(
        |value| bar.write32(FORCE_WAKE, value),
        |state| wait(bar, FORCE_ACK, ForceAck::ACTIVE::SET.value, state).is_ok(),
        || {
            let _ = configure(bar, lp, panel, device_id);
        },
    );
}

/// Re-assert the BIOS display power well request (`HSW_PWR_WELL_CTL1`) and wait
/// for hardware acknowledgement.
///
/// Native `gma::Haswell` modeset turns off the display power well when eDP is
/// the only enabled output; the Mini-HD audio codec resides in that well and
/// fails codec detection unless BIOS keeps the well powered.
pub(super) fn ensure_power_well(gtt_mmio: u64) {
    // SAFETY: `gtt_mmio` is the PCI-assigned, memory-decoded 4 MiB GTTMMADR BAR.
    let bar = unsafe { MmioWindow::new(gtt_mmio as usize, GTTMMADR_SIZE) };
    bar.setbits32(HSW_PWR_WELL_CTL1, 1 << 31);
    let _ = wait(bar, HSW_PWR_WELL_CTL1, 1 << 30, 1 << 30);
}

fn with_force_wake(
    mut write: impl FnMut(u32),
    mut acknowledged: impl FnMut(u32) -> bool,
    configure: impl FnOnce(),
) {
    write(0x10001);
    if acknowledged(1) {
        configure();
    }
    // Even a timed-out assertion may complete late; always request release.
    write(0x10000);
    if acknowledged(0) {
        write(1);
    }
}

fn configure(b: impl GtMmio, lp: bool, panel: Panel, device: u16) -> Result<(), ServiceError> {
    b.write32(0xa248, 0x16);
    b.write32(0xa000, 0x0007_0020);
    b.modify32(0xa180, 0xff3f_ffff, 0x1500_0000);
    for (off, value) in [
        (0x9424, 0x3fd),
        (0x9400, 0x80),
        (0x9404, 0x4040_1000),
        (0x9408, 0),
        (0x940c, 0x0200_0001),
        (0xa008, 0x0800_0000),
        (0xa090, 0),
        (0xa098, 0x03e8_0000),
        (0xa09c, 0x0028_0000),
        (0xa0a8, 0x0001_e848),
        (0xa0ac, 0x19),
        (0x2054, 10),
        (0x12054, 10),
        (0x22054, 10),
        (0x1a054, 10),
        (0xa0b0, 0),
        (0xa0b4, 0x3e8),
        (0xa0b8, 0xc350),
        (0xa010, 0x000f_4240),
        (0xa014, 0x1206_0000),
        (0xa02c, 0xe808),
        (0xa030, 0x0003_bd08),
        (0xa068, 0x0001_01d0),
        (0xa06c, 0x0005_5730),
        (0xa070, 10),
        (0xa024, 0xb92),
        (0xa090, 0x8804_0000),
        (0xa00c, 0x0800_0000),
    ] {
        b.write32(off, value);
    }
    if pcode(b, 4, 0).is_err() {
        fstart_log::warn!("GT: PCODE 0x04 RC6 request timed out");
    }
    b.write32(0x4402c, 0x0300_0076);
    b.write32(0xa094, 0x40000);
    for (off, bits) in [
        (0xa248, 1 << 31),
        (0xa004, 16),
        (0xa080, 4),
        (0xa180, 1 << 31),
    ] {
        b.setbits32(off, bits);
    }
    b.write32(HSW_PWR_WELL_CTL1, 1 << 31);
    wait(b, HSW_PWR_WELL_CTL1, 1 << 30, 1 << 30)?;
    if b.read32(0xc4030) == 0 {
        let [p, q, r] = panel.hotplug;
        b.write32(
            0xc4030,
            (u32::from(p & 7) << 2) | (u32::from(q & 7) << 10) | (u32::from(r & 7) << 18),
        );
    }
    if let Some(delays) = panel.delays {
        let regs = PanelPowerRegs::PCH;
        for (off, v) in [
            (regs.on_delays, board_on_delays(delays)),
            (regs.off_delays, delays.encode_off_delays()),
        ] {
            if b.read32(off as u32) == 0 {
                b.write32(off as u32, v);
            }
        }
        if delays.power_cycle_us != 0 {
            b.clrsetbits32(
                regs.divisor as u32,
                31,
                delays.encode_divisor_cycle_delay() & 31,
            );
        }
    }
    if let Some(plan) = backlight_pwm_plan(lp, panel.backlight_pwm_hz, panel.backlight_active_low) {
        apply_panel_op(b, plan.south_chicken2);
        apply_panel_op(b, plan.pch_ctl2);
        apply_panel_op(b, plan.pch_ctl1);
    }
    b.write32(0x64000, 0x81 | (u32::from(!panel.ddi_e_connected) << 4));
    let cdsel = u32::from(!lp && b.read32(0x42014) & (1 << 24) == 0);
    b.clrsetbits32(0x0013_0040, 3 << 26, cdsel << 26);
    if matches!(device, 0x0a0e | 0x0a1e) && pcode(b, 0x17, cdsel).is_err() {
        fstart_log::warn!("GT: PCODE 0x17 CDCLK request timed out");
    }
    Ok(())
}

const DISPLAY: HdaConfig = {
    let mut t = HdaVerbTable::new(0x8086_2807, 0x8086_0101);
    let verbs = [
        0x0087_8101,
        0x0057_1c10,
        0x0057_1d00,
        0x0057_1e56,
        0x0057_1f18,
        0x0067_1c20,
        0x0067_1d00,
        0x0067_1e56,
        0x0067_1f18,
        0x0077_1c30,
        0x0077_1d00,
        0x0077_1e56,
        0x0077_1f18,
        0x0087_8100,
    ];
    let mut i = 0;
    while i < verbs.len() {
        t = t.extra_verb(verbs[i]);
        i += 1;
    }
    HdaConfig::new().verb(t)
};

/// Initialize the CPU-integrated Haswell Mini-HD display audio controller (`00:03.0`).
pub(super) fn minihd() {
    let d = EcamDevice::new(0, 3, 0);
    let Some(addr) = d.memory_bar(0x10) else {
        return;
    };
    d.or16(4, 6);
    // SAFETY: Mini-HD BAR0 was assigned by PCI allocation and memory-decoded above.
    let bar = unsafe { MmioWindow::new(addr as usize, MINIHD_BAR_SIZE) };
    bar.clrsetbits32(0x100c, 0x3ffff, 4);
    bar.clrsetbits32(0x1010, 0x3ffff, 0x4b);
    let codec = HdaController::new(addr as usize);
    if codec.detect_sdins(15) != 0 {
        let display = &DISPLAY.verbs.as_slice()[0];
        codec.program_list(
            display
                .extra_verbs
                .as_slice()
                .iter()
                .copied()
                .chain([0x0087_8100; 2]),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::RefCell;

    #[test]
    fn hardware_restore_continues_to_cdclk_after_bad_pwm() {
        struct Trace(RefCell<alloc::vec::Vec<(u32, u32)>>);
        impl GtMmio for &Trace {
            fn read32(self, off: u32) -> u32 {
                if off == 0x45400 { 1 << 30 } else { 0 }
            }
            fn write32(self, off: u32, val: u32) {
                self.0.borrow_mut().push((off, val));
            }
        }
        for hz in [200, u32::MAX] {
            let mmio = Trace(RefCell::default());
            let panel = Panel {
                backlight_pwm_hz: hz,
                delays: Some(PanelPowerDelays::DEFAULT_EDP),
                ..Panel::default()
            };
            configure(&mmio, true, panel, 0x0a0e).unwrap();
            let writes = mmio.0.into_inner();
            let pos = |off| writes.iter().position(|r| r.0 == off);
            assert!(pos(0xa248).unwrap() < pos(0x45400).unwrap());
            assert!(pos(0x45400).unwrap() < pos(PanelPowerRegs::PCH.off_delays as u32).unwrap());
            assert_eq!(pos(0xc8254).is_some(), hz == 200);
            if hz == 200 {
                // 24 MHz / 128 / 200 Hz = 937; 50% duty = 468.
                assert!(writes.contains(&(0xc8254, (937 << 16) | 468)));
                assert!(writes.contains(&(0xc8250, 0xc000_0000)));
            }
            assert!(pos(0x64000).unwrap() < pos(0x0013_0040).unwrap());
            assert_eq!(writes.last(), Some(&(0x0013_8124, 0x8000_0017)));
        }
    }

    #[test]
    fn timed_out_assertion_still_deasserts_force_wake() {
        for (acquired, released) in [(false, true), (true, true), (true, false)] {
            let trace = RefCell::new(alloc::vec::Vec::new());
            with_force_wake(
                |v| trace.borrow_mut().push(v),
                |s| {
                    trace.borrow_mut().push(0x20000 | s);
                    if s != 0 { acquired } else { released }
                },
                || trace.borrow_mut().push(42),
            );
            let mut expected = alloc::vec![0x10001, 0x20001];
            if acquired {
                expected.push(42);
            }
            expected.extend([0x10000, 0x20000]);
            if released {
                expected.push(1);
            }
            assert_eq!(trace.into_inner(), expected);
        }
    }

    #[test]
    fn peppy_panel_delay_words() {
        let d = PanelPowerDelays {
            power_up_us: 40_000,
            power_down_us: 15_000,
            power_up_to_backlight_on_us: 210_000,
            backlight_off_to_power_down_us: 210_000,
            power_cycle_us: 400_000,
        };
        assert_eq!(board_on_delays(d), (400 << 16) | 2100);
        assert_eq!(d.encode_off_delays(), (150 << 16) | 2100);
    }

    #[test]
    fn cpu_variant_classifies_ulx_ult_and_traditional() {
        assert_eq!(cpu_variant(true, 0x0a0e), Cpu::HaswellUlx);
        assert_eq!(cpu_variant(true, 0x0a1e), Cpu::HaswellUlx);
        assert_eq!(cpu_variant(true, 0x0a16), Cpu::HaswellUlt);
        assert_eq!(cpu_variant(false, 0x0412), Cpu::Haswell);
    }
}
