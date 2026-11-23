// SPDX-License-Identifier: MIT OR Apache-2.0
//! Gen9 DisplayPort SST, HDMI TMDS and board-wired eDP scanout.
//! Sequences follow libgfxinit's Skylake Power_And_Clocks, Connectors.DDI,
//! Transcoder and Pipe_Setup.

use crate::gma::ddi::{
    DdiClockRouting, DdiClockSelect, DdiPort, DpTrainingPattern, SklDpll, SklDpllPlan,
    calculate_skl_hdmi_dpll, encode_dp_tp_ctl, skl_dp_dpll_ctrl1_update, skl_dpll_enable_ops,
    skl_hdmi_dpll_ctrl1_update,
};
use crate::gma::dp_aux::{
    AuxRegs, DPCD_RECEIVER_CAPS, DpLinkRate, PreEmphasis, ReceiverCaps, TrainSet, TrainingPattern,
    VoltageSwing, native_aux_read, native_aux_write,
};
use crate::gma::dp_training::{DpLinkConfig, link_config_candidates, train_ddi_link};
use crate::gma::framebuffer::{PixelFormat, Rotation, TilingMode};
use crate::gma::gmbus::{DDC_EDID_ADDRESS, DdcBus, GmbusPin, HardwareGmbus, read_base_edid};
use crate::gma::mmio::{Mmio, delay_us};
use crate::gma::mode::ModeFlags;
use crate::gma::panel::{
    PanelPowerDelays, PanelPowerPortSelect, PanelPowerRegs, panel_power_control_plan,
    panel_power_sequencer_plan,
};
use crate::gma::pipe::PipeConfig;
use crate::gma::power::{apply_panel_op, panel_power_on};
use crate::gma::regs::{GMCH_DATA_M, GMCH_M_N, GmchDpMnRegs, GmchPipeTimingRegs};
use crate::gma::scaler::{DestinationRect, ScalerPlan};
use crate::gma::types::{Pipe, Port};
use crate::gma::vbt::Vbt;
use crate::gma::{GmaContext, GmaError, Mode};
use fstart_core::mmio::MmioReadWrite;

mod converter;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{register_bitfields, register_structs};

// Gen9 display MMIO offsets (all 32-bit).
const PWR_WELL: usize = 0x45404;
const FUSE_STATUS: usize = 0x42000;
const LCPLL1: usize = 0x46010;
const CDCLK: usize = 0x46000;
const DFSM: usize = 0x51000;
const DBUF: usize = 0x45008;
const MAILBOX: usize = 0x138124;
const DATA: usize = 0x138128;
const DATA1: usize = 0x13812c;
const TRANS_CLOCK: usize = 0x46140;
const DDI_FUNC: usize = 0x60400;
const PIPECONF: usize = 0x70008;
const PRIMARY_PLANE: usize = 0x70180;
const TIMING_BASE: usize = 0x60000;
const EDP_TIMING: usize = 0x6f000;
const EDP_CONF: usize = 0x7f008;

#[derive(Clone, Copy)]
struct Scanout {
    pipe: usize,
    ddi: usize,
    panel: bool,
}

impl Scanout {
    fn new(pipe: Pipe, port: Port) -> Result<Self, GmaError> {
        Ok(Self {
            pipe: match pipe {
                Pipe::A => 0,
                Pipe::B => 1,
                Pipe::C => 2,
            },
            ddi: index(port)?,
            panel: port == Port::Edp,
        })
    }
    fn conf(self) -> usize {
        if self.panel {
            EDP_CONF
        } else {
            PIPECONF + self.pipe * 0x1000
        }
    }
    fn timing(self) -> usize {
        if self.panel {
            EDP_TIMING
        } else {
            TIMING_BASE + self.pipe * 0x1000
        }
    }
    fn func(self) -> usize {
        self.timing() + (DDI_FUNC - TIMING_BASE)
    }
    fn plane(self) -> usize {
        PRIMARY_PLANE + self.pipe * 0x1000
    }
    fn scaler(self) -> usize {
        0x68180 + self.pipe * 0x800
    }
    fn clock(self) -> usize {
        TRANS_CLOCK + 4 * self.pipe
    }
    fn pll(self) -> SklDpll {
        [SklDpll::Dpll1, SklDpll::Dpll2, SklDpll::Dpll3][self.pipe]
    }
    fn buffer(self) -> usize {
        0x64000 + 0x100 * self.ddi
    }
}

register_bitfields![u32,
    /// Display fuse straps, DFSM (0x51000, 32 bits).
    DisplayFuses [
        CDCLK_LIMIT OFFSET(23) NUMBITS(2) [
            Mhz675 = 0,
            Mhz540 = 1,
            Mhz450 = 2,
            Mhz337 = 3
        ]
    ],
    /// CDCLK_CTL (0x46000, 32 bits).
    CdclkControl [
        SELECT OFFSET(26) NUMBITS(2) [
            Mhz450 = 0,
            Mhz540 = 1,
            Mhz337 = 2,
            Mhz675 = 3
        ],
        DIVMUX_OVERRIDE OFFSET(19) NUMBITS(1) [],
        DECIMAL OFFSET(0) NUMBITS(11) []
    ],
    /// Pipe A primary plane control (0x70180, 32 bits).
    PlaneControl [
        ENABLE OFFSET(31) NUMBITS(1) [],
        FORMAT OFFSET(24) NUMBITS(4) [Xrgb8888 = 4],
        GAMMA_DISABLE OFFSET(13) NUMBITS(1) []
    ]
];

register_structs! {
    /// Pipe A's Gen9 primary plane registers, starting at 0x70180.
    PrimaryPlane {
        (0x00 => control: MmioReadWrite<u32, PlaneControl::Register>),
        (0x04 => _reserved0),
        (0x08 => stride: MmioReadWrite<u32>),
        (0x0c => position: MmioReadWrite<u32>),
        (0x10 => size: MmioReadWrite<u32>),
        (0x14 => _reserved1),
        (0x1c => surface: MmioReadWrite<u32>),
        (0x20 => _reserved2),
        (0x24 => offset: MmioReadWrite<u32>),
        (0x28 => _reserved3),
        (0x40 => aux_distance: MmioReadWrite<u32>),
        (0x44 => @END),
    }
}
const ENABLE: u32 = 1 << 31;
const RUNNING: u32 = 1 << 30;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cdclk {
    khz: u32,
    select: u32,
    voltage: u32,
}

const CLOCKS: [Cdclk; 4] = [
    Cdclk {
        khz: 337_500,
        select: CdclkControl::SELECT::Mhz337.value,
        voltage: 0,
    },
    Cdclk {
        khz: 450_000,
        select: CdclkControl::SELECT::Mhz450.value,
        voltage: 1,
    },
    Cdclk {
        khz: 540_000,
        select: CdclkControl::SELECT::Mhz540.value,
        voltage: 2,
    },
    Cdclk {
        khz: 675_000,
        select: CdclkControl::SELECT::Mhz675.value,
        voltage: 3,
    },
];

impl Cdclk {
    pub(crate) fn for_outputs(mmio: &Mmio, fastest_khz: u32) -> Result<Self, GmaError> {
        Self::select(fastest_khz, mmio.read32(DFSM))
    }

    fn select(fastest_khz: u32, dfsm: u32) -> Result<Self, GmaError> {
        let highest = 3 - DisplayFuses::CDCLK_LIMIT.read(dfsm) as usize;
        CLOCKS[..=highest]
            .iter()
            .copied()
            .find(|c| c.khz >= fastest_khz)
            .ok_or(GmaError::ModeUnavailable)
    }

    fn register(self) -> u32 {
        self.select | CdclkControl::DECIMAL.val((self.khz - 1_000) / 500).value
    }

    pub(crate) fn is_current(self, mmio: &Mmio) -> bool {
        let mask = CdclkControl::SELECT.val(3).value | CdclkControl::DECIMAL.val(0x7ff).value;
        mmio.read32(CDCLK) & mask == self.register()
    }

    pub(crate) fn apply(self, mmio: &Mmio) -> Result<(), GmaError> {
        if self.is_current(mmio) {
            return Ok(());
        }
        if pipes_running(mmio) {
            return Err(GmaError::HardwareError);
        }
        for _ in 0..20 {
            if cdclk_request(mmio, 3)? & 1 != 0 {
                // WaDisplay#1183: hold the CD clock mux while changing the
                // divider. With the 8.1 GHz VCO unchanged, DPLL0 stays on.
                let mux = CdclkControl::DIVMUX_OVERRIDE::SET.value;
                let fields =
                    CdclkControl::SELECT.val(3).value | CdclkControl::DECIMAL.val(0x7ff).value;
                mmio.update32(CDCLK, 0, mux);
                let _ = mmio.read32(CDCLK);
                mmio.update32(CDCLK, fields, 0);
                mmio.update32(CDCLK, 0, self.register());
                mmio.update32(CDCLK, mux, 0);
                let _ = mmio.read32(CDCLK);
                cdclk_request(mmio, self.voltage)?;
                if !self.is_current(mmio) {
                    return Err(GmaError::HardwareError);
                }
                mmio.update32(DBUF, 0, ENABLE);
                return wait(mmio, DBUF, RUNNING, RUNNING);
            }
            delay_us(10);
        }
        Err(GmaError::Timeout)
    }
}

pub(crate) fn supports_mode(mmio: &Mmio, mode: Mode) -> bool {
    mode.pixel_clock_khz != 0
        && !mode.flags.contains(ModeFlags::INTERLACE)
        && Cdclk::for_outputs(mmio, mode.pixel_clock_khz).is_ok()
}

fn pipes_running(mmio: &Mmio) -> bool {
    (0..3).any(|i| mmio.read32(PIPECONF + 0x1000 * i) & (ENABLE | RUNNING) != 0)
        || mmio.read32(EDP_CONF) & (ENABLE | RUNNING) != 0
}

fn wait(mmio: &Mmio, reg: usize, mask: u32, value: u32) -> Result<(), GmaError> {
    for _ in 0..2_000 {
        if mmio.read32(reg) & mask == value {
            return Ok(());
        }
        delay_us(10);
    }
    Err(GmaError::Timeout)
}

fn power_well(mmio: &Mmio, request: u32, fuse: u32) -> Result<(), GmaError> {
    mmio.update32(PWR_WELL, 0, request);
    wait(mmio, PWR_WELL, request >> 1, request >> 1)?;
    wait(mmio, FUSE_STATUS, fuse, fuse)
}

fn cdclk_request(mmio: &Mmio, data: u32) -> Result<u32, GmaError> {
    wait(mmio, MAILBOX, ENABLE, 0)?;
    mmio.write32(DATA, data);
    mmio.write32(DATA1, 0);
    mmio.write32(MAILBOX, ENABLE | 7);
    wait(mmio, MAILBOX, ENABLE, 0)?;
    Ok(mmio.read32(DATA))
}

fn index(port: Port) -> Result<usize, GmaError> {
    match port {
        Port::Edp => Ok(0),
        Port::DpB | Port::HdmiB => Ok(1),
        Port::DpC | Port::HdmiC => Ok(2),
        Port::DpD | Port::HdmiD => Ok(3),
        _ => Err(GmaError::UnsupportedPort),
    }
}

/// Initialize display power before probing. Start at the lowest CDCLK and
/// select the final clock after gathering all requested output modes.
pub(super) fn prepare(
    mmio: &Mmio,
    ports: impl Iterator<Item = Port>,
    vbt: Option<&[u8]>,
) -> Result<(), GmaError> {
    mmio.update32(0x46408, 0, 1 << 4); // PCH reset handshake
    wait(mmio, FUSE_STATUS, 1 << 27, 1 << 27)?;
    power_well(mmio, 1 << 29, 1 << 26)?;
    power_well(mmio, 1 << 1, 0)?;
    // Cold-only: don't clobber a pipe or DDI left running by another owner.
    if pipes_running(mmio) || (0..5).any(|i| mmio.read32(0x64000 + 0x100 * i) & ENABLE != 0) {
        return Err(GmaError::HardwareError);
    }
    mmio.update32(LCPLL1, 0, ENABLE);
    wait(mmio, LCPLL1, RUNNING, RUNNING)?;
    mmio.update32(0x46430, 0, 1 << 13); // WaEnableChickenDCPR
    mmio.update32(0x45000, 0, ENABLE); // WaFbcWakeMemOn
    CLOCKS[0].apply(mmio)?;
    mmio.update32(DBUF, 0, ENABLE);
    wait(mmio, DBUF, RUNNING, RUNNING)?;
    power_well(mmio, 1 << 31, 1 << 25)?;
    mmio.update32(0x41000, 0, ENABLE); // disable legacy VGA plane
    prepare_ports(mmio, ports, vbt)
}

pub(crate) fn prepare_ports(
    mmio: &Mmio,
    ports: impl Iterator<Item = Port>,
    vbt: Option<&[u8]>,
) -> Result<(), GmaError> {
    for port in ports {
        power_well(mmio, 1 << (3 + 2 * index(port)?), 0)?;
        if matches!(port, Port::DpB | Port::DpC | Port::DpD)
            && mmio.read32(Scanout::new(Pipe::A, port)?.buffer()) & ENABLE == 0
            && let Err(error) = converter::prepare(mmio, port)
        {
            fstart_log::warn!("intel-gma: LSPCON on {:?}: {}", port, error.as_str());
        }
        if port == Port::Edp
            && let Err(error) = prepare_panel(mmio, vbt)
        {
            fstart_log::warn!(
                "intel-gma: eDP panel preparation failed: {}",
                error.as_str()
            );
        }
    }
    Ok(())
}

/// Release driver-owned DDI power requests only after the connector's probe
/// and any teardown. A failed teardown may have left a live DDI buffer.
pub(crate) fn release_unused_ports(
    mmio: &Mmio,
    requested: impl Iterator<Item = Port>,
    active: impl Iterator<Item = Port>,
) {
    let needed = active
        .filter_map(|port| index(port).ok())
        .fold(0, |mask, ddi| mask | (1 << ddi));
    for ddi in requested.filter_map(|port| index(port).ok()) {
        if needed & (1 << ddi) == 0 && mmio.read32(0x64000 + 0x100 * ddi) & ENABLE == 0 {
            if ddi == 0 && mmio.read32(EDP_CONF) & (ENABLE | RUNNING) == 0 {
                let panel = PanelPowerRegs::PCH;
                if mmio.read32(panel.control) & 1 != 0 {
                    apply_panel_op(mmio, panel_power_control_plan(panel).panel_off);
                }
            }
            mmio.update32(PWR_WELL, 1 << (3 + 2 * ddi), 0);
        }
    }
}

// Desktop translation table, libgfxinit Skylake_Trans_DP. Slot 9 is for HDMI;
// the desktop DP PHY is limited to swing level 2.
const DP_TRANSLATIONS: [(u32, u32); 9] = [
    (0x00002016, 0x00a0),
    (0x00005012, 0x009b),
    (0x00007011, 0x0088),
    (0x80009010, 0x00c0),
    (0x00002016, 0x009b),
    (0x00005012, 0x0088),
    (0x80007011, 0x00c0),
    (0x00002016, 0x00df),
    (0x80005012, 0x00c0),
];

fn is_mobile_gen9() -> bool {
    matches!(
        fstart_arch::x86::cpuid(1).0 & 0x0fff_0ff0,
        0x406e0 | 0x806e0
    )
}

fn is_ulx(ctx: &GmaContext<'_>) -> bool {
    let gpu = fstart_pci::ecam::EcamDevice::new_address(ctx.resources.pci_bdf);
    matches!(
        gpu.read16(2),
        0x190e | 0x1915 | 0x191e | 0x590e | 0x5915 | 0x591e
    )
}

fn dp_translations(cpu: crate::gma::types::Cpu, low_swing: bool, ulx: bool) -> [(u32, u32); 9] {
    use crate::gma::types::Cpu;
    let mobile = is_mobile_gen9();
    if ulx && low_swing {
        // libgfxinit Skylake_Y_Trans_EDP, also used on Kaby Lake Y.
        return [
            (0x18, 0x00a8),
            (0x4013, 0x00ab),
            (0x7011, 0x00a4),
            (0x9010, 0x00df),
            (0x18, 0x00aa),
            (0x6013, 0x00a4),
            (0x7011, 0x009d),
            (0x18, 0x00a0),
            (0x6012, 0x00df),
        ];
    }
    if low_swing {
        // libgfxinit Skylake_Trans_EDP and Skylake_U_Trans_EDP (also for KBL).
        return [
            (0x18, 0x00a8),
            (0x4013, 0x00a9),
            (0x7011, 0x00a2),
            (0x9010, 0x009c),
            (0x18, 0x00a9),
            (0x6013, 0x00a2),
            (0x7011, 0x00a6),
            (if mobile { 0x2016 } else { 0x18 }, 0x00ab),
            (if mobile { 0x5013 } else { 0x7013 }, 0x009f),
        ];
    }
    if ulx {
        let mut table = [
            (0x1017, 0x00a1),
            (0x5012, 0x0088),
            (0x80007011, 0x00cd),
            (0x8000800f, 0x00c0),
            (0x1017, 0x009d),
            (0x80005012, 0x00c0),
            (0x80007011, 0x00c0),
            (0x18, 0x0088),
            (0x80005012, 0x00c0),
        ];
        if cpu == Cpu::Kabylake {
            table[7] = (0x1017, 0x004c);
        }
        return table;
    }
    if !mobile {
        let mut table = DP_TRANSLATIONS;
        if cpu == Cpu::Kabylake {
            table[6].0 = 0x00007011;
            table[7].1 = 0x0097;
        }
        return table;
    }
    // libgfxinit's U-series table. Use the standard DP electrical levels;
    // boards with low-swing eDP wiring need separate VBT drive policy.
    let mut table = [
        (0x201b, 0x00a2),
        (0x5012, 0x0088),
        (0x80007011, 0x00cd),
        (0x80009010, 0x00c0),
        (0x201b, 0x009d),
        (0x80005012, 0x00c0),
        (0x80007011, 0x00c0),
        (0x2016, 0x0088),
        (0x80005012, 0x00c0),
    ];
    if cpu == Cpu::Kabylake {
        table[0].1 = 0x00a1;
        table[7].1 = 0x004f;
    }
    table
}

fn prepare_panel(mmio: &Mmio, vbt: Option<&[u8]>) -> Result<(), GmaError> {
    // Only power an explicitly selected eDP port with a VBT, not a guessed
    // panel on a desktop's unused DDI A.
    let (delays, _, _) = panel_policy(vbt)?;
    let regs = PanelPowerRegs::PCH;
    // Do not change timing registers while the sequencer is running.
    if mmio.read32(regs.control) & 1 == 0 {
        // T11/T12 is hundreds of milliseconds on some panels, unlike a
        // normal display register handshake. Wait up to two seconds.
        let mut idle = false;
        for _ in 0..20_000 {
            if mmio.read32(regs.status) & ((3 << 28) | (1 << 27)) == 0 {
                idle = true;
                break;
            }
            delay_us(100);
        }
        if !idle {
            return Err(GmaError::Timeout);
        }
        let plan = panel_power_sequencer_plan(regs, delays, PanelPowerPortSelect::DpA, true, true);
        for op in [
            plan.control,
            plan.on_delays,
            plan.off_delays,
            plan.cycle_delay,
        ] {
            apply_panel_op(mmio, op);
        }
    }
    panel_power_on(mmio, regs)
}

fn panel_policy(vbt: Option<&[u8]>) -> Result<(PanelPowerDelays, u8, bool), GmaError> {
    vbt.and_then(|bytes| Vbt::parse(bytes).ok())
        .and_then(|vbt| vbt.edp_policy())
        .ok_or(GmaError::InvalidConfig)
}

fn enable_panel_backlight(mmio: &Mmio, vbt: Option<&[u8]>) -> Result<(), GmaError> {
    let pwm = vbt
        .and_then(|bytes| Vbt::parse(bytes).ok())
        .and_then(|vbt| vbt.lfp_panel_metadata().ok())
        .and_then(|panel| panel.backlight)
        .filter(|pwm| pwm.is_pwm() && pwm.pwm_freq_hz != 0)
        .ok_or(GmaError::UnsupportedPort)?;
    // Sunrise Point PWM uses 24 MHz and the strap-selected 16/128 prescaler.
    let prescale = if mmio.read32(0xc2000) & 1 == 0 {
        16
    } else {
        128
    };
    let divisor = u32::from(pwm.pwm_freq_hz) * prescale;
    let period = ((24_000_000 + divisor / 2) / divisor).min(0xffff);
    if period == 0 {
        return Err(GmaError::InvalidConfig);
    }
    mmio.update32(0xc8250, ENABLE, 0);
    mmio.write32(0xc8254, (period << 16) | period);
    mmio.update32(
        0xc8250,
        1 << 29,
        ENABLE | (1 << 30) | (u32::from(pwm.active_low_pwm) << 29),
    );
    apply_panel_op(
        mmio,
        panel_power_control_plan(PanelPowerRegs::PCH).backlight_on,
    );
    Ok(())
}

fn initialize_hdmi(
    ctx: &GmaContext<'_>,
    out: Scanout,
    port: Port,
    mode: Mode,
    stride: u32,
    rect: Option<DestinationRect>,
) -> Result<(), GmaError> {
    let mmio = ctx.mmio();
    let clock = calculate_skl_hdmi_dpll(u64::from(mode.pixel_clock_khz) * 1000)?;
    let translations = 0x64e00 + 0x60 * out.ddi;
    // SKL/KBL desktop/ULT default HDMI translation 0 goes in the dedicated
    // TMDS slot 9; translation select bits are not used in HDMI mode.
    mmio.write32(translations + 9 * 8, 0x00000018);
    mmio.write32(
        translations + 9 * 8 + 4,
        if is_ulx(ctx) { 0x000000a1 } else { 0x000000ac },
    );
    program_link_clock(&mmio, out, port, LinkClock::Hdmi(clock))?;
    let sink = if hdmi_sink(ctx, port)? {
        OutputLink::Hdmi
    } else {
        OutputLink::Dvi
    };
    program_pipe(ctx, out, mode, sink, stride, rect)?;
    mmio.update32(out.buffer(), (0xf << 24) | (1 << 16), ENABLE);
    let _ = mmio.read32(out.buffer());
    delay_us(600);
    Ok(())
}

fn hdmi_sink(ctx: &GmaContext<'_>, port: Port) -> Result<bool, GmaError> {
    let pin = match port {
        Port::HdmiB => GmbusPin::DigitalB,
        Port::HdmiC => GmbusPin::DigitalC,
        Port::HdmiD => GmbusPin::DigitalD,
        _ => return Err(GmaError::UnsupportedPort),
    };
    // SAFETY: the checked IGD BAR is live, with the PCH DDC block mapped.
    let mut bus = unsafe { HardwareGmbus::pch(ctx.resources.gtt_mmio_base, pin) };
    let mut base = [0; 128];
    let edid = read_base_edid(&mut bus, &mut base)?;
    for i in 1..=edid.extension_count().min(8) {
        let mut ext = [0; 128];
        if bus.read_edid_block(DDC_EDID_ADDRESS, i, &mut ext).is_ok() && cta_has_hdmi(&ext) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn cta_has_hdmi(block: &[u8; 128]) -> bool {
    if block[0] != 2 || block[1] < 3 || !crate::gma::edid::extension_is_valid(block) {
        return false;
    }
    let end = usize::from(block[2]);
    if !(4..=127).contains(&end) {
        return false;
    }
    let mut collection = &block[4..end];
    while let Some((&header, rest)) = collection.split_first() {
        let len = usize::from(header & 31);
        let Some((data, next)) = rest.split_at_checked(len) else {
            return false;
        };
        if header >> 5 == 3 && matches!(data.get(..3), Some([3, 12, 0] | [0xd8, 0x5d, 0xc4])) {
            return true;
        }
        collection = next;
    }
    false
}

fn avi_packet(mode: Mode) -> [u8; 32] {
    // Intel's DIP RAM inserts one pad byte between HB2 and the checksum.
    let mut packet = [0u8; 32];
    packet[..3].copy_from_slice(&[0x82, 2, 13]);
    packet[6] = if u32::from(mode.hdisplay) * 9 == u32::from(mode.vdisplay) * 16 {
        2 << 4
    } else if u32::from(mode.hdisplay) * 3 == u32::from(mode.vdisplay) * 4 {
        1 << 4
    } else {
        0
    };
    packet[7] = 2 << 2; // full-range RGB framebuffer
    packet[4] = 0u8.wrapping_sub(packet.iter().copied().fold(0, u8::wrapping_add));
    packet
}

fn program_avi(mmio: &Mmio, timing: usize, mode: Mode) {
    mmio.write32(timing + 0x200, 0);
    for (i, word) in avi_packet(mode).as_chunks::<4>().0.iter().enumerate() {
        mmio.write32(
            timing + 0x220 + i * 4,
            u32::from_le_bytes([word[0], word[1], word[2], word[3]]),
        );
    }
    mmio.write32(timing + 0x200, 1 << 12);
}

fn program_watermarks(mmio: &Mmio, pipe: usize) {
    // libgfxinit's disjoint DBUF entries: each pipe gets 169 blocks.
    for (buf, wm, first, count) in [
        (0x7027c, 0x70240, 0, 150),
        (0x7037c, 0x70340, 150, 11),
        (0x7017c, 0x70140, 161, 8),
    ] {
        let first = first + pipe as u32 * 169;
        mmio.write32(buf + pipe * 0x1000, ((first + count - 1) << 16) | first);
        let offset = wm + pipe * 0x1000;
        mmio.write32(offset, ENABLE | (2 << 14) | count);
        for level in 1..8 {
            mmio.write32(offset + level * 4, 0);
        }
    }
}

fn level(set: TrainSet) -> usize {
    match set.voltage_swing {
        VoltageSwing::Level0 => set.pre_emphasis as usize,
        VoltageSwing::Level1 => 4 + (set.pre_emphasis as usize).min(2),
        VoltageSwing::Level2 | VoltageSwing::Level3 => {
            7 + usize::from(set.pre_emphasis != PreEmphasis::Level0)
        }
    }
}

fn transport(mmio: &Mmio, buf: usize, link: DpLinkConfig, pattern: DpTrainingPattern) {
    mmio.write32(
        buf + 0x40,
        encode_dp_tp_ctl(true, false, false, link.enhanced_framing, false, pattern),
    );
}

fn disable_link(mmio: &Mmio, out: Scanout) -> Result<(), GmaError> {
    let buf = out.buffer();
    let enabled = mmio.read32(buf) & ENABLE != 0;
    mmio.update32(buf, ENABLE, 0);
    mmio.update32(buf + 0x40, ENABLE, 0);
    if enabled {
        wait(mmio, buf, 1 << 7, 1 << 7)?;
    }
    mmio.update32(0x6c05c, 0, 1 << (15 + out.ddi)); // DPLL_CTRL2 DDI clock off
    // DPLL0 also supplies CDCLK; never disable it during panel teardown.
    if !out.panel {
        mmio.update32(out.pll().registers().ctl, ENABLE, 0);
    }
    Ok(())
}

pub(super) fn disable(mmio: &Mmio, pipe: Pipe, port: Port) -> Result<(), GmaError> {
    let out = Scanout::new(pipe, port)?;
    if out.panel {
        let panel = panel_power_control_plan(PanelPowerRegs::PCH);
        apply_panel_op(mmio, panel.backlight_off);
        mmio.update32(0xc8250, ENABLE, 0);
    }
    // SAFETY: prepare validated and powered the Gen9 display BAR.
    let plane = unsafe { mmio.reg_block::<PrimaryPlane>(out.plane()) };
    plane.control.modify(PlaneControl::ENABLE::CLEAR);
    plane.surface.set(0);
    let _ = plane.surface.get();
    mmio.write32(out.scaler(), 0);
    mmio.write32(out.scaler() - 0xc, 0); // latch scaler disable
    mmio.update32(out.conf(), ENABLE, 0);
    wait(mmio, out.conf(), RUNNING, 0)?;
    mmio.write32(out.func(), 0);
    mmio.write32(out.timing() + 0x200, 0); // VIDEO_DIP_CTL
    if !out.panel {
        mmio.write32(out.clock(), 0);
    }
    disable_link(mmio, out)?;
    if out.panel {
        apply_panel_op(
            mmio,
            panel_power_control_plan(PanelPowerRegs::PCH).panel_off,
        );
    }
    Ok(())
}

enum LinkClock {
    Dp(DdiClockSelect),
    Hdmi(SklDpllPlan),
}

fn program_link_clock(
    mmio: &Mmio,
    out: Scanout,
    port: Port,
    clock: LinkClock,
) -> Result<(), GmaError> {
    let pll = out.pll();
    let hint = if out.panel { 0 } else { out.pipe as u32 + 1 };
    let DdiClockRouting::DpllCtrl2 { op } =
        DdiClockRouting::dpll_ctrl2(DdiPort::from_port(port)?, hint)?
    else {
        return Err(GmaError::InvalidConfig);
    };
    let (ctrl, clear, value) = match clock {
        LinkClock::Dp(rate) => skl_dp_dpll_ctrl1_update(pll, rate)?,
        LinkClock::Hdmi(plan) => {
            let regs = pll.registers();
            mmio.write32(regs.cfgr1, plan.encode_cfgr1());
            mmio.write32(regs.cfgr2, plan.encode_cfgr2());
            skl_hdmi_dpll_ctrl1_update(pll)
        }
    };
    if out.panel {
        // DPLL0 has the same field layout in the low six DPLL_CTRL1 bits.
        let shift = pll.registers().ctrl1_shift;
        mmio.update32(ctrl, clear >> shift, value >> shift);
        wait(mmio, LCPLL1, RUNNING, RUNNING)?;
    } else {
        mmio.update32(ctrl, clear, value);
        let (enable, bit, status, lock) = skl_dpll_enable_ops(pll);
        mmio.update32(enable, 0, bit);
        wait(mmio, status, lock, lock)?;
    }
    mmio.update32(op.register, op.mask_unset, op.mask_set);
    Ok(())
}

pub(super) fn init(ctx: &mut GmaContext<'_>, mode: Mode) -> Result<(), GmaError> {
    let port = super::selected_port(ctx)?;
    let out = Scanout::new(ctx.pipe, port)?;
    let surface = ctx.surface;
    let stride = surface.stride_bytes()?;
    let mmio = ctx.mmio();
    let active_cdclk = CdclkControl::DECIMAL.read(mmio.read32(CDCLK)) * 500 + 1_000;
    if !supports_mode(&mmio, mode)
        || mode.pixel_clock_khz > active_cdclk
        || surface.width > u32::from(mode.hdisplay)
        || surface.height > u32::from(mode.vdisplay)
        || surface.tiling != TilingMode::Linear
        || surface.rotation != Rotation::None
        || surface.pixel_format != PixelFormat::Xrgb8888
        || stride % 64 != 0
        || surface.start_x != 0
        || surface.start_y != 0
    {
        return Err(GmaError::ModeUnavailable);
    }
    let scaling = ScalerPlan::resolve(
        ctx.config.cpu,
        ctx.pipe,
        surface,
        mode,
        ctx.config.framebuffer.scaling,
    );
    let rect = if scaling.requires_scaling {
        let rect = scaling.constrained_destination_rect()?;
        let vertical_limit = if out.pipe == 2 && surface.width > 2048 {
            199
        } else {
            299
        };
        if rect.width * 100 > surface.width * 299
            || rect.height * 100 > surface.height * vertical_limit
        {
            return Err(GmaError::ModeUnavailable);
        }
        Some(rect)
    } else {
        None
    };
    if matches!(port, Port::HdmiB | Port::HdmiC | Port::HdmiD) {
        return initialize_hdmi(ctx, out, port, mode, stride, rect);
    }
    let (bpc, low_swing) = if out.panel {
        let (_, bpc, low_swing) = panel_policy(ctx.config.vbt)?;
        prepare_panel(&mmio, ctx.config.vbt)?;
        (bpc.min(8), low_swing)
    } else {
        (8, false)
    };
    let aux = AuxRegs::gen9_for_port(port)?;
    let mut bytes = [0; 15];
    native_aux_read(&mmio, aux, DPCD_RECEIVER_CAPS, &mut bytes)?;
    let mut caps = ReceiverCaps::parse(&bytes)?;
    if out.panel && mmio.read32(out.buffer()) & (1 << 4) == 0 {
        caps.max_lane_count = caps.max_lane_count.min(2);
    }
    if bytes[0] >= 0x11 {
        native_aux_write(&mmio, aux, 0x600, &[1])?;
        delay_us(1_000);
    }
    let links = link_config_candidates(caps, mode, true)?;
    let trans_base = 0x64e00 + 0x60 * out.ddi;
    for (slot, (low, high)) in dp_translations(ctx.config.cpu, low_swing, is_ulx(ctx))
        .into_iter()
        .enumerate()
    {
        mmio.write32(trans_base + 8 * slot, low);
        mmio.write32(trans_base + 8 * slot + 4, high);
    }
    let buf = out.buffer();
    let mut error = GmaError::ModeUnavailable;
    for i in 0..links.len() {
        let link = links.get(i).ok_or(GmaError::InvalidConfig)?;
        let clock = match link.link_rate {
            DpLinkRate::Rbr => DdiClockSelect::Lcpll810,
            DpLinkRate::Hbr => DdiClockSelect::Lcpll1350,
            DpLinkRate::Hbr2 => DdiClockSelect::Lcpll2700,
            DpLinkRate::Unknown(_) => continue,
        };
        let result = (|| {
            program_link_clock(&mmio, out, port, LinkClock::Dp(clock))?;
            train_ddi_link(&mmio, port, link, |pattern, levels| {
                let pattern = match pattern {
                    TrainingPattern::Pattern1 => DpTrainingPattern::Pattern1,
                    TrainingPattern::Pattern2 => DpTrainingPattern::Pattern2,
                    TrainingPattern::Pattern3 => DpTrainingPattern::Pattern3,
                    _ => return Err(GmaError::InvalidConfig),
                };
                let was_enabled = mmio.read32(buf) & ENABLE != 0;
                transport(&mmio, buf, link, pattern);
                mmio.update32(
                    buf,
                    (0xf << 24) | (1 << 16) | (7 << 1),
                    ENABLE | ((level(levels) as u32) << 24) | (u32::from(link.lane_count - 1) << 1),
                );
                let _ = mmio.read32(buf);
                if !was_enabled {
                    delay_us(600);
                }
                Ok(())
            })?;
            transport(&mmio, buf, link, DpTrainingPattern::Idle);
            if !out.panel {
                wait(&mmio, buf + 0x44, 1 << 25, 1 << 25)?;
            }
            transport(&mmio, buf, link, DpTrainingPattern::None);
            program_pipe(ctx, out, mode, OutputLink::Dp(link, bpc), stride, rect)?;
            if out.panel {
                enable_panel_backlight(&mmio, ctx.config.vbt)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => return Ok(()),
            Err(e) => {
                error = e;
                disable(&mmio, ctx.pipe, port)?;
                if out.panel {
                    prepare_panel(&mmio, ctx.config.vbt)?;
                }
            }
        }
    }
    Err(error)
}

#[derive(Clone, Copy)]
enum OutputLink {
    Dp(DpLinkConfig, u8),
    Hdmi,
    Dvi,
}

fn program_pipe(
    ctx: &GmaContext<'_>,
    out: Scanout,
    mode: Mode,
    link: OutputLink,
    stride: u32,
    scaler: Option<DestinationRect>,
) -> Result<(), GmaError> {
    let mmio = ctx.mmio();
    let timing = PipeConfig::new(ctx.pipe, mode);
    let source = PipeConfig::surface_source(ctx.surface)?;
    if !out.panel {
        mmio.write32(out.clock(), ((out.ddi as u32) + 1) << 29);
    }
    // SAFETY: Gen9 timing and DP ratios retain the same 32-bit layouts as the shared blocks.
    let regs = unsafe { mmio.reg_block::<GmchPipeTimingRegs>(out.timing()) };
    regs.htotal.set(timing.htotal());
    regs.hblank.set(timing.hblank());
    regs.hsync.set(timing.hsync());
    regs.vtotal.set(timing.vtotal());
    regs.vblank.set(timing.vblank());
    regs.vsync.set(timing.vsync());
    // The pipe source lives on the selected pipe even when using transcoder eDP.
    mmio.write32(TIMING_BASE + out.pipe * 0x1000 + 0x1c, source);
    let link_bits = if let OutputLink::Dp(link, bpc) = link {
        let mn = link.m_n(mode, u64::from(bpc));
        let ratios = unsafe { mmio.reg_block::<GmchDpMnRegs>(out.timing() + 0x30) };
        ratios
            .data_m
            .set(GMCH_DATA_M::TU_SIZE.val(63).value | mn.data_m);
        ratios.data_n.write(GMCH_M_N::VALUE.val(mn.data_n));
        ratios.link_m.write(GMCH_M_N::VALUE.val(mn.link_m));
        ratios.link_n.write(GMCH_M_N::VALUE.val(mn.link_n));
        let six_bpc = bpc == 6;
        mmio.write32(out.func() + 0x10, if six_bpc { 1 } else { (1 << 5) | 1 });
        (2 << 24) | (u32::from(six_bpc) << 21) | (u32::from(link.lane_count - 1) << 1)
    } else if matches!(link, OutputLink::Hdmi) {
        program_avi(&mmio, out.timing(), mode);
        0 // HDMI TMDS, 8 bpc
    } else {
        mmio.write32(out.timing() + 0x200, 0); // no packets on DVI
        1 << 24
    };
    let panel_select = if out.panel {
        (4 + out.pipe as u32) << 12
    } else {
        0
    };
    mmio.write32(
        out.func(),
        ENABLE
            | ((out.ddi as u32) << 28)
            | link_bits
            | panel_select
            | (u32::from(mode.flags.contains(ModeFlags::PVSYNC)) << 17)
            | (u32::from(mode.flags.contains(ModeFlags::PHSYNC)) << 16),
    );
    if let Some(rect) = scaler {
        // Gen9 pipe scaler, extended mode on A/B (C has only one scaler).
        mmio.write32(
            out.scaler(),
            ENABLE | if out.pipe < 2 { 1 << 28 } else { 0 },
        );
        mmio.write32(out.scaler() - 0x10, (rect.x << 16) | rect.y);
        mmio.write32(out.scaler() - 0xc, (rect.width << 16) | rect.height);
    }
    program_watermarks(&mmio, out.pipe);
    let dither = matches!(link, OutputLink::Dp(_, 6));
    mmio.write32(out.conf(), ENABLE | (u32::from(dither) << 4));
    wait(&mmio, out.conf(), RUNNING, RUNNING)?;
    // SAFETY: the Gen9 display BAR and selected primary plane are live.
    let plane = unsafe { mmio.reg_block::<PrimaryPlane>(out.plane()) };
    plane.aux_distance.set(0);
    plane.offset.set(0);
    plane
        .size
        .set(((ctx.surface.height - 1) << 16) | (ctx.surface.width - 1));
    plane.stride.set(stride / 64);
    plane.position.set(0);
    plane.control.write(
        PlaneControl::ENABLE::SET
            + PlaneControl::FORMAT::Xrgb8888
            + PlaneControl::GAMMA_DISABLE::SET,
    );
    plane.surface.set(ctx.surface.plane_surface_offset());
    let _ = plane.surface.get();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_core::typed::mmio32;

    #[test]
    fn shared_cdclk_rounds_up_and_respects_fuses_without_touching_running_pipes() {
        use core::cell::UnsafeCell;
        let registers: alloc::vec::Vec<_> =
            (0..0x80000 / 4).map(|_| UnsafeCell::new(0u32)).collect();
        // SAFETY: aligned backing for Gen9 display register offsets, no live MMIO.
        let mmio = unsafe { Mmio::new(mmio32(registers.as_ptr() as u64)) };
        let clock =
            Cdclk::for_outputs(&mmio, [148_500, 594_000].into_iter().max().unwrap()).unwrap();
        assert_eq!(clock.khz, 675_000);
        assert_eq!(clock.register(), (3 << 26) | 1348);
        mmio.write32(PIPECONF + 0x1000, RUNNING);
        assert_eq!(clock.apply(&mmio), Err(GmaError::HardwareError));
        assert_eq!(mmio.read32(CDCLK), 0);
        mmio.write32(DFSM, 1 << 23); // fuse limits us to 540 MHz
        assert_eq!(
            Cdclk::for_outputs(&mmio, 594_000),
            Err(GmaError::ModeUnavailable)
        );
        assert_eq!(Cdclk::for_outputs(&mmio, 400_000).unwrap().khz, 450_000);
    }

    #[test]
    fn distinct_pipes_keep_disjoint_buffer_and_pll_state() {
        use core::cell::UnsafeCell;
        let registers: alloc::vec::Vec<_> =
            (0..0x74000 / 4).map(|_| UnsafeCell::new(0u32)).collect();
        // SAFETY: unique aligned backing for the offsets in this test.
        let mmio = unsafe { Mmio::new(mmio32(registers.as_ptr() as u64)) };
        for pipe in 0..3 {
            program_watermarks(&mmio, pipe);
        }
        assert_eq!(mmio.read32(0x7027c), 149 << 16);
        assert_eq!(mmio.read32(0x7127c), (318 << 16) | 169);
        assert_eq!(mmio.read32(0x7217c), (506 << 16) | 499);

        let dp = Scanout::new(Pipe::A, Port::DpB).unwrap();
        let hdmi = Scanout::new(Pipe::B, Port::HdmiD).unwrap();
        mmio.write32(0x6c060, (1 << 8) | (1 << 16));
        program_link_clock(
            &mmio,
            dp,
            Port::DpB,
            LinkClock::Dp(DdiClockSelect::Lcpll1350),
        )
        .unwrap();
        let clock = calculate_skl_hdmi_dpll(148_500_000).unwrap();
        program_link_clock(&mmio, hdmi, Port::HdmiD, LinkClock::Hdmi(clock)).unwrap();
        assert_eq!(mmio.read32(0x6c048), clock.encode_cfgr1());
        assert_eq!(mmio.read32(0x6c05c) & (7 << 3), 3 << 3);
        disable_link(&mmio, hdmi).unwrap();
        assert_eq!(mmio.read32(dp.pll().registers().ctl), ENABLE);
        assert_eq!(mmio.read32(hdmi.pll().registers().ctl), 0);
        assert_ne!(mmio.read32(0x6c05c) & (1 << 18), 0);
    }

    #[test]
    fn hdmi_requires_a_valid_cta_vendor_block_and_avi_has_a_checksum() {
        let mut ext = [0u8; 128];
        ext[..8].copy_from_slice(&[2, 3, 8, 0, 0x63, 3, 12, 0]);
        ext[127] = 0u8.wrapping_sub(ext.iter().copied().fold(0, u8::wrapping_add));
        assert!(cta_has_hdmi(&ext));
        ext[6] = 0;
        assert!(!cta_has_hdmi(&ext));
        let packet = avi_packet(Mode::XGA_1024X768_60);
        assert_eq!(&packet[..4], &[0x82, 2, 13, 0]);
        assert_eq!(packet[..18].iter().copied().fold(0, u8::wrapping_add), 0);
    }

    #[test]
    fn desktop_dp_b_routes_dpll1_through_shared_gen9_control() {
        use core::cell::UnsafeCell;
        let registers: alloc::vec::Vec<_> =
            (0..0x70000 / 4).map(|_| UnsafeCell::new(0u32)).collect();
        // SAFETY: the test supplies unique, aligned backing for every offset below.
        let mmio = unsafe { Mmio::new(mmio32(registers.as_ptr() as u64)) };
        mmio.write32(0x6c060, 1 << 8); // DPLL1 locked
        mmio.write32(0x6c05c, (1 << 18) | (1 << 16)); // D and B clocks gated
        let out = Scanout::new(Pipe::A, Port::DpB).unwrap();
        program_link_clock(
            &mmio,
            out,
            Port::DpB,
            LinkClock::Dp(DdiClockSelect::Lcpll1350),
        )
        .unwrap();
        assert_eq!(mmio.read32(0x46014), ENABLE);
        assert_eq!(mmio.read32(0x6c058) & (0x3f << 6), 3 << 6); // DP 2.7 Gbit/s
        assert_eq!(mmio.read32(0x6c05c), (1 << 18) | (3 << 3));
        assert_eq!(mmio.read32(0x46104), 0); // Not Haswell's per-port selector.
    }
}
