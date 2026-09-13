//! Legacy GMCH display power/clock discovery helpers.
//!
//! libgfxinit's G45 `Power_And_Clocks.Initialize` records the current CDClk and
//! raw clock before modesetting.  The fstart northbridge drivers still own PCI
//! setup and pass the PCI-only GCFGC selector into this shared crate when
//! available.  When it is not available, this module mirrors the safe
//! MMIO-readable parts and falls back to the same conservative libgfxinit
//! defaults.

use tock_registers::LocalRegisterCopy;
use tock_registers::interfaces::Readable;
use tock_registers::registers::ReadWrite;

use crate::mmio::Mmio;
use crate::regs::{GCFGC, GMCH_CLKCFG, GMCH_HPLLVCO, GmchClockRegs};
use crate::types::Cpu;

/// Legacy GMCH clock state discovered before modesetting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LegacyPowerClocks {
    /// Core display clock in Hz.
    pub cdclk_hz: u64,
    /// Maximum core display clock in Hz. Legacy GMCH does not switch CDClk here.
    pub max_cdclk_hz: u64,
    /// Raw clock derived from GMCH CLKCFG in Hz.
    pub raw_clock_hz: u64,
}

impl LegacyPowerClocks {
    /// Return true when `pixel_clock_khz` fits libgfxinit's 90% CDClk guard.
    pub(crate) const fn allows_dotclock(self, pixel_clock_khz: u32) -> bool {
        (pixel_clock_khz as u64) * 1000 <= self.cdclk_hz * 90 / 100
    }
}

/// Initialize/read legacy GMCH clocks for GM965/G45/GM45/Pineview.
pub(crate) fn initialize_legacy_gmch(
    mmio: &Mmio,
    cpu: Cpu,
    gcfgc: Option<u16>,
) -> LegacyPowerClocks {
    let clocks = gmch_clock_regs(mmio);
    let cdclk_hz = cdclk_from_gcfgc(cpu, hpll_vco(mmio, cpu), gcfgc);
    let raw_clock_hz = raw_clock_from_clkcfg(clocks.clkcfg.read(GMCH_CLKCFG::FSB_FREQ_SEL));
    LegacyPowerClocks {
        cdclk_hz,
        max_cdclk_hz: cdclk_hz,
        raw_clock_hz,
    }
}

fn hpll_vco(mmio: &Mmio, cpu: Cpu) -> HpllVco {
    match cpu {
        Cpu::Gm965 => {
            let selector = read_hpllvco_mobile_selector(mmio);
            HpllVco {
                hz: gm965_vco_hz(selector),
                divisors: gm965_divisors(selector),
            }
        }
        Cpu::Gm45 => {
            let selector = read_hpllvco_mobile_selector(mmio);
            HpllVco {
                hz: gm45_vco_hz(selector),
                divisors: gm45_divisors(selector),
            }
        }
        Cpu::G45 => {
            let selector = gmch_clock_regs(mmio).hpllvco.read(GMCH_HPLLVCO::SELECTOR) as usize;
            HpllVco {
                hz: g45_vco_hz(selector),
                divisors: g45_divisors(selector),
            }
        }
        Cpu::Pineview => HpllVco {
            hz: 0,
            divisors: [1; 8],
        },
        _ => HpllVco {
            hz: 0,
            divisors: [1; 8],
        },
    }
}

fn read_hpllvco_mobile_selector(mmio: &Mmio) -> usize {
    // GMCH_HPLLVCO_MOBILE is an unaligned MCHBAR mirror byte. Read the aligned
    // containing dword and extract byte 3 on little-endian x86 hardware.
    hpllvco_mobile_reg(mmio).read(GMCH_HPLLVCO::MOBILE_SELECTOR) as usize
}

fn gmch_clock_regs(mmio: &Mmio) -> &'static GmchClockRegs {
    // SAFETY: legacy GMCH clock registers live in the validated display MMIO BAR
    // at the fixed CLKCFG-relative block used by GM965/G45/GM45/Pineview.
    unsafe { mmio.reg_block::<GmchClockRegs>(GMCH_CLKCFG_OFFSET) }
}

fn hpllvco_mobile_reg(mmio: &Mmio) -> &'static ReadWrite<u32, GMCH_HPLLVCO::Register> {
    // SAFETY: the mobile HPLLVCO selector is mirrored in the aligned dword at
    // 0x10c0c; `MOBILE_SELECTOR` extracts the top byte field that contains it.
    unsafe {
        mmio.reg_block::<ReadWrite<u32, GMCH_HPLLVCO::Register>>(GMCH_HPLLVCO_MOBILE_ALIGNED_OFFSET)
    }
}

fn cdclk_from_gcfgc(cpu: Cpu, vco: HpllVco, gcfgc: Option<u16>) -> u64 {
    let Some(gcfgc) = gcfgc else {
        return fallback_cdclk(cpu);
    };
    let gcfgc = LocalRegisterCopy::<u16, GCFGC::Register>::new(gcfgc);
    let selector = match cpu {
        Cpu::Gm965 => {
            let raw = gcfgc.read(GCFGC::GM965_CDCLK_SELECT) as usize;
            if (1..=3).contains(&raw) { raw - 1 } else { 0 }
        }
        Cpu::Gm45 => gcfgc.read(GCFGC::GM45_CDCLK_SELECT) as usize,
        Cpu::G45 => gcfgc.read(GCFGC::G45_CDCLK_SELECT) as usize,
        _ => return fallback_cdclk(cpu),
    };
    let divisor = vco.divisors[selector];
    if vco.hz == 0 || divisor == 0 {
        fallback_cdclk(cpu)
    } else {
        vco.hz / divisor
    }
}

const fn fallback_cdclk(cpu: Cpu) -> u64 {
    match cpu {
        Cpu::Gm965 | Cpu::Pineview => 4_000_000_000 / 20,
        Cpu::Gm45 => 5_333_333_333 / 24,
        Cpu::G45 => 5_333_333_333 / 28,
        _ => 200_000_000,
    }
}

const fn raw_clock_from_clkcfg(fsb_freq_sel: u32) -> u64 {
    match fsb_freq_sel {
        0 => CLKCFG_FSB_1067,
        1 => CLKCFG_FSB_533,
        2 => CLKCFG_FSB_800,
        3 => CLKCFG_FSB_667,
        4 => CLKCFG_FSB_1333,
        5 => CLKCFG_FSB_400,
        6 => CLKCFG_FSB_1067,
        _ => CLKCFG_FSB_1333,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HpllVco {
    hz: u64,
    divisors: [u64; 8],
}

const fn gm965_vco_hz(selector: usize) -> u64 {
    match selector {
        0 => 3_200_000_000,
        1 => 4_000_000_000,
        2 => 5_333_333_333,
        3 => 6_400_000_000,
        4 => 3_333_333_333,
        5 => 3_566_666_667,
        6 => 4_266_666_667,
        _ => 0,
    }
}

const fn gm45_vco_hz(selector: usize) -> u64 {
    match selector {
        0 => 3_200_000_000,
        1 => 4_000_000_000,
        2 => 5_333_333_333,
        4 => 2_666_666_667,
        _ => 0,
    }
}

const fn g45_vco_hz(selector: usize) -> u64 {
    match selector {
        0 => 3_200_000_000,
        1 => 4_000_000_000,
        2 => 5_333_333_333,
        3 => 4_800_000_000,
        _ => 0,
    }
}

const fn gm965_divisors(selector: usize) -> [u64; 8] {
    match selector {
        0 => [16, 10, 8, 1, 1, 1, 1, 1],
        1 => [20, 12, 10, 1, 1, 1, 1, 1],
        2 => [24, 16, 14, 1, 1, 1, 1, 1],
        _ => [1; 8],
    }
}

const fn gm45_divisors(selector: usize) -> [u64; 8] {
    match selector {
        0 => [14, 10, 1, 1, 1, 1, 1, 1],
        1 => [18, 12, 1, 1, 1, 1, 1, 1],
        2 => [24, 16, 1, 1, 1, 1, 1, 1],
        4 => [12, 8, 1, 1, 1, 1, 1, 1],
        _ => [1; 8],
    }
}

const fn g45_divisors(selector: usize) -> [u64; 8] {
    match selector {
        0 => [12, 10, 8, 7, 5, 16, 1, 1],
        1 => [14, 12, 10, 8, 6, 20, 1, 1],
        2 => [20, 16, 12, 12, 8, 28, 1, 1],
        3 => [20, 14, 12, 10, 8, 24, 1, 1],
        _ => [1; 8],
    }
}

const GMCH_CLKCFG_OFFSET: usize = 0x10c00;
const GMCH_HPLLVCO_MOBILE_ALIGNED_OFFSET: usize = 0x10c0c;
const CLKCFG_FSB_400: u64 = 100_000_000;
const CLKCFG_FSB_533: u64 = 133_333_333;
const CLKCFG_FSB_667: u64 = 166_666_666;
const CLKCFG_FSB_800: u64 = 200_000_000;
const CLKCFG_FSB_1067: u64 = 266_666_666;
const CLKCFG_FSB_1333: u64 = 333_333_333;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_clock_decodes_libgfxinit_clkcfg_table() {
        assert_eq!(raw_clock_from_clkcfg(0), CLKCFG_FSB_1067);
        assert_eq!(raw_clock_from_clkcfg(1), CLKCFG_FSB_533);
        assert_eq!(raw_clock_from_clkcfg(2), CLKCFG_FSB_800);
        assert_eq!(raw_clock_from_clkcfg(3), CLKCFG_FSB_667);
        assert_eq!(raw_clock_from_clkcfg(4), CLKCFG_FSB_1333);
        assert_eq!(raw_clock_from_clkcfg(5), CLKCFG_FSB_400);
        assert_eq!(raw_clock_from_clkcfg(6), CLKCFG_FSB_1067);
        assert_eq!(raw_clock_from_clkcfg(7), CLKCFG_FSB_1333);
    }

    #[test]
    fn gm965_cdclk_matches_libgfxinit_selector_formula() {
        let vco = HpllVco {
            hz: gm965_vco_hz(1),
            divisors: gm965_divisors(1),
        };
        assert_eq!(cdclk_from_gcfgc(Cpu::Gm965, vco, Some(1 << 8)), 200_000_000);
        assert_eq!(cdclk_from_gcfgc(Cpu::Gm965, vco, Some(2 << 8)), 333_333_333);
        assert_eq!(cdclk_from_gcfgc(Cpu::Gm965, vco, Some(0)), 200_000_000);
    }

    #[test]
    fn fallback_cdclk_preserves_current_legacy_modes() {
        let clocks = LegacyPowerClocks {
            cdclk_hz: fallback_cdclk(Cpu::Gm965),
            max_cdclk_hz: fallback_cdclk(Cpu::Gm965),
            raw_clock_hz: CLKCFG_FSB_800,
        };
        assert!(clocks.allows_dotclock(65_000));
        assert!(!clocks.allows_dotclock(181_000));
    }
}
