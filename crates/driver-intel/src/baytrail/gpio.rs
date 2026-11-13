//! Bay Trail GPIO: three pad communities (north core, south core and
//! suspend), each with per-pad configuration in IOBASE MMIO, and legacy
//! I/O registers at GPIO_BASE for the south core and suspend banks.
//!
//! Boards describe each bank as a list of [`Pad`]s indexed by GPIO number
//! (not pad number); the bank's lookup table maps it to the pad.

use super::{GPIO_BASE, IO_BASE};
use fstart_core::mmio::MmioReadWrite;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{register_bitfields, register_structs};

register_bitfields![u32,
    pub PAD_CONF0 [
        FUNC OFFSET(0) NUMBITS(3) [],
        PULL_ASSIGN OFFSET(7) NUMBITS(2) [
            None = 0,
            Up = 1,
            Down = 2
        ],
        PULL_STRENGTH OFFSET(9) NUMBITS(2) [
            K2 = 0,
            K10 = 1,
            K20 = 2,
            K40 = 3
        ],
        /// Bypass the input flop.
        FLOP_BYPASS OFFSET(11) NUMBITS(1) [],
        HYST_CTRL OFFSET(13) NUMBITS(2) [],
        /// Hysteresis disable (inverted enable).
        HYST_DISABLE OFFSET(15) NUMBITS(1) [],
        FAST_GLITCH_FILTER OFFSET(16) NUMBITS(1) [],
        SLOW_GLITCH_FILTER OFFSET(17) NUMBITS(1) [],
        /// Level (1) or edge (0) triggered direct IRQ.
        LEVEL_IRQ OFFSET(24) NUMBITS(1) [],
        TPE_IRQ OFFSET(25) NUMBITS(1) [],
        TNE_IRQ OFFSET(26) NUMBITS(1) [],
        DIRECT_IRQ_EN OFFSET(27) NUMBITS(1) [],
        MASK2_DISABLE OFFSET(29) NUMBITS(1) []
    ],
    pub PAD_VAL [
        VALUE OFFSET(0) NUMBITS(1) [],
        /// Output disable, active high.
        OUTPUT_DISABLE OFFSET(1) NUMBITS(1) [],
        /// Input disable, active high.
        INPUT_DISABLE OFFSET(2) NUMBITS(1) []
    ]
];

register_structs! {
    /// One pad's registers, 16 bytes apart.
    pub PadRegs {
        (0x00 => pub conf0: MmioReadWrite<u32, PAD_CONF0::Register>),
        (0x04 => pub conf1: MmioReadWrite<u32>),
        (0x08 => pub val: MmioReadWrite<u32, PAD_VAL::Register>),
        (0x0c => _reserved),
        (0x10 => @END),
    }
}

/// Pad configuration word 0 power-on defaults.
pub const CONF0_DEFAULT: u32 = PAD_CONF0::MASK2_DISABLE::SET.value
    | PAD_CONF0::SLOW_GLITCH_FILTER::SET.value
    | PAD_CONF0::FAST_GLITCH_FILTER::SET.value
    | PAD_CONF0::HYST_DISABLE::SET.value
    | PAD_CONF0::HYST_CTRL.val(2).value
    | PAD_CONF0::FLOP_BYPASS::SET.value;
/// Pad configuration word 1 power-on default.
pub const CONF1_DEFAULT: u32 = 0x8000;
/// Input enabled, output disabled.
pub const VAL_INPUT: u32 = PAD_VAL::OUTPUT_DISABLE::SET.value;
/// Output enabled, input disabled.
pub const VAL_OUTPUT: u32 = PAD_VAL::INPUT_DISABLE::SET.value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pull {
    None = 0,
    Up = 1,
    Down = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strength {
    K2 = 0,
    K10 = 1,
    K20 = 2,
    K40 = 3,
}

const fn pull(pull: Pull, strength: Strength) -> u32 {
    PAD_CONF0::PULL_ASSIGN.val(pull as u32).value
        | PAD_CONF0::PULL_STRENGTH.val(strength as u32).value
}

/// One GPIO's pad and legacy configuration (coreboot `soc_gpio_map`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pad {
    pub conf0: u32,
    pub conf1: u32,
    pub val: u32,
    /// Legacy I/O owns the GPIO (`use_sel`) instead of the pad MMIO.
    pub legacy: bool,
    /// Legacy direction: input.
    pub input: bool,
    pub level: bool,
    /// Positive and negative edge detection.
    pub tpe: bool,
    pub tne: bool,
    pub wake: bool,
    pub smi: bool,
    pub sci: bool,
    /// The pad's GPIO function: the bank decides function 0 or 1.
    pub is_gpio: bool,
}

impl Pad {
    const fn base(conf0: u32, val: u32) -> Self {
        Self {
            conf0,
            conf1: CONF1_DEFAULT,
            val,
            legacy: false,
            input: false,
            level: false,
            tpe: false,
            tne: false,
            wake: false,
            smi: false,
            sci: false,
            is_gpio: false,
        }
    }

    /// Native function `func` (`GPIO_FUNC(func, pull, strength)`).
    pub const fn func(func: u32, p: Pull, s: Strength) -> Self {
        Self::base(
            PAD_CONF0::FUNC.val(func).value | pull(p, s) | CONF0_DEFAULT,
            VAL_INPUT,
        )
    }

    /// Native function `func`, no pull (`GPIO_FUNCn`).
    pub const fn native(func: u32) -> Self {
        Self::func(func, Pull::None, Strength::K20)
    }

    /// MMIO GPIO input (`GPIO_INPUT_*`).
    pub const fn input(p: Pull, s: Strength) -> Self {
        let mut pad = Self::base(pull(p, s) | CONF0_DEFAULT, VAL_INPUT);
        pad.is_gpio = true;
        pad
    }

    /// Legacy-I/O GPIO input without pull (`GPIO_INPUT_LEGACY`).
    pub const fn input_legacy() -> Self {
        let mut pad = Self::input(Pull::None, Strength::K20);
        pad.legacy = true;
        pad.input = true;
        pad
    }

    /// Legacy-I/O GPIO output (`GPIO_OUT_LOW`/`GPIO_OUT_HIGH`).
    pub const fn output(high: bool) -> Self {
        let value = if high { PAD_VAL::VALUE::SET.value } else { 0 };
        let mut pad = Self::base(CONF0_DEFAULT, VAL_OUTPUT | value);
        pad.legacy = true;
        pad.level = high;
        pad.is_gpio = true;
        pad
    }

    /// Not connected: driven high (`GPIO_NC`).
    pub const fn nc() -> Self {
        Self::output(true)
    }

    /// Direct IRQ input to the APIC (`GPIO_DIRQ*`): level or edge, and the
    /// edges that trigger.
    pub const fn direct_irq(
        p: Pull,
        s: Strength,
        level: bool,
        rising: bool,
        falling: bool,
    ) -> Self {
        let mut conf0 = pull(p, s) | CONF0_DEFAULT | PAD_CONF0::DIRECT_IRQ_EN::SET.value;
        if level {
            conf0 |= PAD_CONF0::LEVEL_IRQ::SET.value;
        }
        if rising {
            conf0 |= PAD_CONF0::TPE_IRQ::SET.value;
        }
        if falling {
            conf0 |= PAD_CONF0::TNE_IRQ::SET.value;
        }
        Self::base(conf0, VAL_INPUT)
    }

    const fn acpi(sci: bool, wake: bool, smi: bool) -> Self {
        let mut pad = Self::base(CONF0_DEFAULT, VAL_INPUT);
        pad.legacy = true;
        pad.input = true;
        pad.tne = true;
        pad.sci = sci;
        pad.wake = wake;
        pad.smi = smi;
        pad
    }

    /// Falling-edge ACPI SCI, also a wake source (`GPIO_ACPI_SCI`).
    pub const fn acpi_sci() -> Self {
        Self::acpi(true, true, false)
    }

    /// Falling-edge wake source (`GPIO_ACPI_WAKE`).
    pub const fn acpi_wake() -> Self {
        Self::acpi(false, true, false)
    }

    /// Falling-edge SMI (`GPIO_ACPI_SMI`).
    pub const fn acpi_smi() -> Self {
        Self::acpi(false, false, true)
    }
}

/// A GPIO community.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bank {
    /// North core (GPIO_S0_NC), 27 GPIOs, no legacy interface.
    Ncore,
    /// South core (GPIO_S0_SC), 102 GPIOs.
    Score,
    /// Suspend well (GPIO_S5), 44 GPIOs.
    Ssus,
}

const NCORE_TO_PAD: [u8; 27] = [
    19, 18, 17, 20, 21, 22, 24, 25, 23, 16, 14, 15, 12, 26, 27, 1, 4, 8, 11, 0, 3, 6, 10, 13, 2, 5,
    9,
];
const SCORE_TO_PAD: [u8; 102] = [
    85, 89, 93, 96, 99, 102, 98, 101, 34, 37, 36, 38, 39, 35, 40, 84, 62, 61, 64, 59, 54, 56, 60,
    55, 63, 57, 51, 50, 53, 47, 52, 49, 48, 43, 46, 41, 45, 42, 58, 44, 95, 105, 70, 68, 67, 66,
    69, 71, 65, 72, 86, 90, 88, 92, 103, 77, 79, 83, 78, 81, 80, 82, 13, 12, 15, 14, 17, 18, 19,
    16, 2, 1, 0, 4, 6, 7, 9, 8, 33, 32, 31, 30, 29, 27, 25, 28, 26, 23, 21, 20, 24, 22, 5, 3, 10,
    11, 106, 87, 91, 104, 97, 100,
];
const SSUS_TO_PAD: [u8; 44] = [
    29, 33, 30, 31, 32, 34, 36, 35, 38, 37, 18, 7, 11, 20, 17, 1, 8, 10, 19, 12, 0, 2, 23, 39, 28,
    27, 22, 21, 24, 25, 26, 51, 56, 54, 49, 55, 48, 57, 50, 58, 52, 53, 59, 40,
];

// Legacy I/O registers of a 32-GPIO set, 0x20 apart.
const LEGACY_USE_SEL: u16 = 0x00;
const LEGACY_IO_SEL: u16 = 0x04;
const LEGACY_GP_LVL: u16 = 0x08;
const LEGACY_TPE: u16 = 0x0c;
const LEGACY_TNE: u16 = 0x10;
const LEGACY_TS: u16 = 0x14;
const LEGACY_WAKE_EN: u16 = 0x18;
/// Direct IRQ routing registers, from the pad base.
const DIRQ_OFFSET: usize = 0x980;

impl Bank {
    const fn to_pad(self) -> &'static [u8] {
        match self {
            Bank::Ncore => &NCORE_TO_PAD,
            Bank::Score => &SCORE_TO_PAD,
            Bank::Ssus => &SSUS_TO_PAD,
        }
    }

    /// MMIO base of the community's pads.
    pub const fn pad_base(self) -> usize {
        IO_BASE as usize
            + match self {
                Bank::Score => 0x0000,
                Bank::Ncore => 0x1000,
                Bank::Ssus => 0x2000,
            }
    }

    const fn legacy_base(self) -> Option<u16> {
        match self {
            Bank::Ncore => None,
            Bank::Score => Some(GPIO_BASE),
            Bank::Ssus => Some(GPIO_BASE + 0x80),
        }
    }

    /// GPIOs whose GPIO function is function 1 rather than 0.
    const fn function1(self, gpio: usize) -> bool {
        match self {
            Bank::Ncore => false,
            Bank::Score => matches!(gpio, 92..=93),
            Bank::Ssus => matches!(gpio, 11..=21),
        }
    }

    /// The registers of pad `pad` (a pad number, not a GPIO number).
    pub fn pad(self, pad: usize) -> &'static PadRegs {
        // SAFETY: IOBASE is decoded once the south cluster programmed its
        // base; every pad of the community has 16 bytes of registers.
        unsafe { &*((self.pad_base() + 16 * pad) as *const PadRegs) }
    }

    /// The registers of GPIO `gpio`.
    pub fn gpio(self, gpio: usize) -> &'static PadRegs {
        self.pad(usize::from(self.to_pad()[gpio]))
    }

    /// Program the bank from `pads`, indexed by GPIO number (coreboot
    /// `setup_gpios`).
    pub fn configure(self, pads: &[Pad]) {
        // Per 32-GPIO set: use_sel, io_sel, gp_lvl, tpe, tne, wake_en.
        let mut legacy = [[0u32; 6]; 4];
        for (gpio, cfg) in pads.iter().take(self.to_pad().len()).enumerate() {
            let (set, bit) = (gpio / 32, gpio % 32);
            if self.legacy_base().is_some() {
                let bits = &mut legacy[set];
                for (slot, on) in [
                    cfg.legacy,
                    cfg.input,
                    cfg.level,
                    cfg.tpe,
                    cfg.tne,
                    cfg.wake && self == Bank::Ssus,
                ]
                .into_iter()
                .enumerate()
                {
                    bits[slot] |= u32::from(on) << bit;
                }
            }
            let mut conf0 = cfg.conf0;
            if cfg.is_gpio {
                conf0 |= u32::from(self.function1(gpio));
            }
            let regs = self.gpio(gpio);
            regs.conf0.set(conf0);
            regs.conf1.set(cfg.conf1);
            regs.val.set(cfg.val);
        }
        if let Some(base) = self.legacy_base() {
            let sets = (self.to_pad().len() - 1) / 32 + 1;
            for (set, bits) in legacy.iter().enumerate().take(sets) {
                let reg = base + 0x20 * set as u16;
                // SAFETY: GPIO_BASE is decoded by the LPC bridge.
                unsafe {
                    use fstart_core::pio::outl;
                    outl(reg + LEGACY_USE_SEL, bits[0]);
                    outl(reg + LEGACY_IO_SEL, bits[1]);
                    outl(reg + LEGACY_GP_LVL, bits[2]);
                    outl(reg + LEGACY_TPE, bits[3]);
                    outl(reg + LEGACY_TNE, bits[4]);
                    // Write-one-to-clear status.
                    outl(reg + LEGACY_TS, 0);
                    if self == Bank::Ssus {
                        outl(reg + LEGACY_WAKE_EN, bits[5]);
                    }
                }
            }
        }
    }

    /// Route the 16 direct IRQs of the bank to IOAPIC inputs.
    pub fn set_direct_irqs(self, dirq: &[u8; 16]) {
        let base = self.pad_base() + DIRQ_OFFSET;
        for (i, chunk) in dirq.as_chunks::<4>().0.iter().enumerate() {
            // SAFETY: the bank's DIRQ registers in IOBASE.
            unsafe {
                fstart_core::mmio::write32(
                    (base + 4 * i) as *mut u32,
                    u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]),
                )
            };
        }
    }

    /// Select native function `func` of pad `pad` (romstage helpers
    /// `score_select_func` and friends).
    pub fn select_function(self, pad: usize, func: u32) {
        self.pad(pad).conf0.modify(PAD_CONF0::FUNC.val(func));
    }

    /// Make pad `pad` a function 0 input without internal pull, for
    /// reading straps before DRAM.
    pub fn strap_input(self, pad: usize) {
        let regs = self.pad(pad);
        regs.conf0.modify(
            PAD_CONF0::FUNC.val(0) + PAD_CONF0::PULL_ASSIGN::None + PAD_CONF0::PULL_STRENGTH::K2,
        );
        regs.val.set(VAL_INPUT);
    }

    /// The input level of a pad configured as GPIO input.
    pub fn level(self, pad: usize) -> bool {
        self.pad(pad).val.is_set(PAD_VAL::VALUE)
    }
}

/// South core pads the SoC itself needs before ramstage.
pub mod score_pad {
    pub const UART_RXD: usize = 82;
    pub const UART_TXD: usize = 83;
    pub const PCU_SMB_CLK: usize = 88;
    pub const PCU_SMB_DATA: usize = 90;
    pub const SOC_DDI1_VDDEN: usize = 16;
}

/// A board's GPIO configuration (coreboot `soc_gpio_config`).
#[derive(Debug, Clone, Copy)]
pub struct GpioConfig {
    pub ncore: &'static [Pad],
    pub score: &'static [Pad],
    pub ssus: &'static [Pad],
    pub core_dirq: Option<[u8; 16]>,
    pub sus_dirq: Option<[u8; 16]>,
    /// Keep the XDP debug port; otherwise TDO and TMS are tri-stated.
    pub xdp_tap: bool,
}

impl GpioConfig {
    pub const fn new() -> Self {
        Self {
            ncore: &[],
            score: &[],
            ssus: &[],
            core_dirq: None,
            sus_dirq: None,
            xdp_tap: false,
        }
    }

    /// PMC GPIO_ROUT: suspend GPIOs 0-7 and south core GPIOs 0-7, SMI over
    /// SCI (coreboot `setup_gpio_route`).
    pub fn smi_sci_route(&self) -> u32 {
        const ROUTE_SMI: u32 = 1;
        const ROUTE_SCI: u32 = 2;
        let route = |pad: Option<&Pad>| match pad {
            Some(pad) if pad.smi => ROUTE_SMI,
            Some(pad) if pad.sci => ROUTE_SCI,
            _ => 0,
        };
        (0..8).fold(0, |acc, i| {
            acc | route(self.ssus.get(i)) << (2 * i) | route(self.score.get(i)) << (2 * (i + 8))
        })
    }

    /// Program every bank, the direct IRQs and the TAP pads.
    pub fn apply(&self) {
        Bank::Ncore.configure(self.ncore);
        Bank::Score.configure(self.score);
        Bank::Ssus.configure(self.ssus);
        if let Some(dirq) = &self.core_dirq {
            Bank::Score.set_direct_irqs(dirq);
        }
        if let Some(dirq) = &self.sus_dirq {
            Bank::Ssus.set_direct_irqs(dirq);
        }
        if !self.xdp_tap {
            // On-die termination with pull-up, drive TDO and TMS high.
            for offset in [0x2fc, 0x2cc] {
                // SAFETY: suspend community pad registers in IOBASE.
                unsafe {
                    fstart_core::mmio::write32((Bank::Ssus.pad_base() + offset) as *mut u32, 0xc)
                };
            }
        }
    }
}

impl Default for GpioConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_words_match_the_reference_macros() {
        // GPIO_FUNC(0, PULL_UP, 20K) and PAD_CONFIG0_DEFAULT.
        assert_eq!(CONF0_DEFAULT, 0x2003_c800);
        assert_eq!(Pad::func(0, Pull::Up, Strength::K20).conf0, 0x2003_cc80);
        assert_eq!(Pad::nc().val, 0x5);
        assert_eq!(Pad::input(Pull::None, Strength::K20).val, 0x2);
        assert_eq!(Pad::input(Pull::Down, Strength::K20).conf0, 0x2003_cd00);
    }

    #[test]
    fn gpio_route_prefers_smi_and_covers_both_banks() {
        static SSUS: [Pad; 2] = [Pad::acpi_sci(), Pad::acpi_smi()];
        static SCORE: [Pad; 1] = [Pad::acpi_wake()];
        let config = GpioConfig {
            ssus: &SSUS,
            score: &SCORE,
            ..GpioConfig::new()
        };
        assert_eq!(config.smi_sci_route(), 0b0110);
    }
}
