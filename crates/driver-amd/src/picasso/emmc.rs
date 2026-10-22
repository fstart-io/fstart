// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso eMMC timing/preset mechanism; board owns mode
//! and drive strengths. This is not an SDHCI command/block-I/O driver.
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_structs,
    registers::ReadWrite,
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Timing {
    Disabled,
    SdLowSpeed,
    SdHighSpeed,
    SdSdr50,
    SdDdr50,
    SdSdr104,
    EmmcSdr26,
    EmmcSdr52,
    EmmcDdr104,
    Hs200,
    Hs400,
    Hs300,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Drive {
    B = 0,
    A = 1,
    C = 2,
    D = 3,
}
#[derive(Debug, Clone, Copy)]
pub enum InitClock {
    /// Explicit maximum divider (2046), the Dalboz reference design's
    /// documented `init_khz_preset = 1` sentinel, NOT an achievable 1-kHz rate.
    Minimum,
    Khz(u16),
}
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub timing: Timing,
    pub init_clock: InitClock,
    pub sdr50_drive: Drive,
    pub sdr104_hs400_drive: Drive,
    pub ddr50_drive: Drive,
}
#[derive(Debug, Clone, Copy)]
struct Mode {
    kind: u8,
    interface: u8,
    phy: u8,
    mhz: u8,
}
const MODES: [Mode; 12] = [
    Mode {
        kind: 0,
        interface: 0,
        phy: 0,
        mhz: 0,
    },
    Mode {
        kind: 0,
        interface: 0x20,
        phy: 0x14,
        mhz: 0x3f,
    },
    Mode {
        kind: 1,
        interface: 0x20,
        phy: 0x14,
        mhz: 0xc8,
    },
    Mode {
        kind: 2,
        interface: 0x21,
        phy: 0x14,
        mhz: 0xc8,
    },
    Mode {
        kind: 2,
        interface: 0x24,
        phy: 0x14,
        mhz: 0xc8,
    },
    Mode {
        kind: 2,
        interface: 0x27,
        phy: 0x14,
        mhz: 0xc8,
    },
    Mode {
        kind: 0,
        interface: 0x08,
        phy: 0x57,
        mhz: 0x3f,
    },
    Mode {
        kind: 2,
        interface: 0x08,
        phy: 0x57,
        mhz: 0xc8,
    },
    Mode {
        kind: 2,
        interface: 0x0c,
        phy: 0x57,
        mhz: 0xc8,
    },
    Mode {
        kind: 2,
        interface: 0x0b,
        phy: 0x57,
        mhz: 0xc8,
    },
    Mode {
        kind: 2,
        interface: 0x0f,
        phy: 0x57,
        mhz: 0xc8,
    },
    Mode {
        kind: 2,
        interface: 0x0f,
        phy: 0x57,
        mhz: 0x96,
    },
];
const TARGETS_SD: [u32; 9] = [
    0,
    25_000_000,
    50_000_000,
    12_000_000,
    25_000_000,
    50_000_000,
    104_000_000,
    50_000_000,
    0,
];
const TARGETS_EMMC: [u32; 9] = [
    0,
    26_000_000,
    52_000_000,
    12_000_000,
    26_000_000,
    52_000_000,
    200_000_000,
    52_000_000,
    200_000_000,
];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InitClock,
    PresetClock,
}

pub trait Registers {
    fn read32(&mut self, offset: u16) -> u32;
    fn write32(&mut self, offset: u16, value: u32);
    fn read16(&mut self, offset: u16) -> u16;
    fn write16(&mut self, offset: u16, value: u16);
}
register_structs! {
    Layout {
        (0x00 => _reserved0),
        (0xa4 => phy_timing: ReadWrite<u32>),
        (0xa8 => interface: ReadWrite<u32>),
        (0xac => _reserved1),
        (0xb0 => mode: ReadWrite<u32>),
        (0xb4 => _reserved2),
        (0xd0 => drive: ReadWrite<u32>),
        (0xd4 => _reserved3),
        (0xf0 => presets: [ReadWrite<u16>; 9]),
        (0x102 => _reserved4),
        (0x116 => mode_clear: ReadWrite<u16>),
        (0x118 => @END),
    }
}
pub struct Native {
    base: *const Layout,
}
impl Native {
    /// # Safety
    /// Picasso eMMC config is decoded UC; caller owns exclusive access.
    pub unsafe fn new() -> Self {
        Self {
            base: 0xfedd5800 as *const Layout,
        }
    }
    fn layout(&self) -> &Layout {
        unsafe { &*self.base }
    }
}
impl Registers for Native {
    fn read32(&mut self, offset: u16) -> u32 {
        let l = self.layout();
        match offset {
            0xa4 => l.phy_timing.get(),
            0xa8 => l.interface.get(),
            0xb0 => l.mode.get(),
            0xd0 => l.drive.get(),
            _ => panic!("invalid eMMC offset"),
        }
    }
    fn write32(&mut self, offset: u16, value: u32) {
        let l = self.layout();
        match offset {
            0xa4 => l.phy_timing.set(value),
            0xa8 => l.interface.set(value),
            0xb0 => l.mode.set(value),
            0xd0 => l.drive.set(value),
            _ => panic!("invalid eMMC offset"),
        }
    }
    fn read16(&mut self, offset: u16) -> u16 {
        if offset == 0x116 {
            return self.layout().mode_clear.get();
        }
        assert!((0xf0..=0x100).contains(&offset) && offset & 1 == 0);
        self.layout().presets[usize::from((offset - 0xf0) / 2)].get()
    }
    fn write16(&mut self, offset: u16, value: u16) {
        if offset == 0x116 {
            self.layout().mode_clear.set(value);
            return;
        }
        assert!((0xf0..=0x100).contains(&offset) && offset & 1 == 0);
        self.layout().presets[usize::from((offset - 0xf0) / 2)].set(value);
    }
}

/// Precompute every divider before any register writes. Native encoding is
/// divider>>1 (zero encodes the undivided clock), not a PCIe clock ratio.
pub fn preset_plan(config: Config) -> Result<[Option<(u16, Drive)>; 9], Error> {
    let mode = MODES[config.timing as usize];
    let mut plan = [None; 9];
    if mode.kind != 2 {
        return Ok(plan);
    }
    let init = match config.init_clock {
        InitClock::Minimum => None,
        InitClock::Khz(khz @ 1..=400) => Some(u32::from(khz) * 1000),
        InitClock::Khz(_) => return Err(Error::InitClock),
    };
    let targets = if mode.phy & 0xc0 != 0 {
        TARGETS_EMMC
    } else {
        TARGETS_SD
    };
    let strengths = [
        Drive::B,
        Drive::B,
        Drive::B,
        Drive::B,
        Drive::B,
        config.sdr50_drive,
        config.sdr104_hs400_drive,
        config.ddr50_drive,
        config.sdr104_hs400_drive,
    ];
    for i in 0..9 {
        if i == 0 && init.is_none() {
            plan[i] = Some((1023, strengths[i]));
            continue;
        }
        let target = if i == 0 { init.unwrap() } else { targets[i] };
        if target == 0 {
            continue;
        }
        // SDHCI v3 divided-clock mode represents 1 (encoded zero) or even
        // divisors only. Native C's odd divisor followed by >>1 rounds DOWN,
        // exceeding the target; choose a representable divisor before encoding.
        let divider = core::iter::once(1u32)
            .chain((2..=2046).step_by(2))
            .find(|&div| u64::from(target) * u64::from(div) >= u64::from(mode.mhz) * 1_000_000)
            .ok_or(Error::PresetClock)?;
        plan[i] = Some(((divider >> 1) as u16, strengths[i]));
    }
    Ok(plan)
}
pub fn initialize(registers: &mut impl Registers, config: Config) -> Result<(), Error> {
    let plan = preset_plan(config)?;
    let mode = MODES[config.timing as usize];
    for (offset, clear, set) in [
        (0xb0, 0xff000000, u32::from(mode.kind) << 24),
        (0xa8, 1, u32::from(mode.interface & 1)),
        (0xa8, 2, u32::from(mode.interface & 2)),
        (0xa8, 4, u32::from(mode.interface & 4)),
        (0xd0, 1 << 17, u32::from(mode.interface >> 3 & 1) << 17),
        (0xd0, 1 << 18, u32::from(mode.interface >> 4 & 1) << 18),
        (0xa8, 0xc000, u32::from(mode.interface >> 5 & 3) << 14),
        (0xa8, 1 << 13, u32::from(mode.interface >> 7) << 13),
        (0xd0, 0x0c000000, u32::from(mode.phy & 3) << 26),
        (0xa4, 0xc0000000, u32::from(mode.phy >> 6) << 30),
        (0xa4, 1 << 28, 0),
        (0xa4, 0xff00, u32::from(mode.mhz) << 8),
    ] {
        let value = registers.read32(offset);
        registers.write32(offset, (value & !clear) | set);
    }
    let clear = registers.read16(0x116);
    registers.write16(0x116, clear & !0x30);
    for (i, value) in plan.into_iter().enumerate() {
        let Some((divider, strength)) = value else {
            continue;
        };
        let offset = 0xf0 + i as u16 * 2;
        let previous = registers.read16(offset);
        registers.write16(offset, (previous & !0x3ff) | divider);
        let previous = registers.read16(offset);
        registers.write16(offset, (previous & !0xc000) | (strength as u16) << 14);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vilboz_hs400_presets_and_mode_geometry() {
        let config = Config {
            timing: Timing::Hs400,
            init_clock: InitClock::Khz(400),
            sdr50_drive: Drive::B,
            sdr104_hs400_drive: Drive::A,
            ddr50_drive: Drive::B,
        };
        let plan = preset_plan(config).unwrap();
        assert_eq!(plan[0], Some((250, Drive::B)));
        assert_eq!(plan[3], Some((9, Drive::B))); // divisor 18, not 17 truncated to 16
        let plan300 = preset_plan(Config {
            init_clock: InitClock::Khz(300),
            ..config
        })
        .unwrap();
        assert_eq!(plan300[0], Some((334, Drive::B)));
        for (plan, targets) in [
            (
                plan,
                [
                    400000u32, 26000000, 52000000, 12000000, 26000000, 52000000, 200000000,
                    52000000, 200000000,
                ],
            ),
            (
                plan300,
                [
                    300000u32, 26000000, 52000000, 12000000, 26000000, 52000000, 200000000,
                    52000000, 200000000,
                ],
            ),
        ] {
            for (i, preset) in plan.into_iter().enumerate() {
                if let Some((encoded, _)) = preset {
                    let divisor = if encoded == 0 {
                        1
                    } else {
                        u64::from(encoded) * 2
                    };
                    assert!(200000000 <= u64::from(targets[i]) * divisor);
                }
            }
        }
        assert_eq!(plan[6], Some((0, Drive::A)));
        assert_eq!(plan[8], Some((0, Drive::A)));
        assert_eq!(
            preset_plan(Config {
                init_clock: InitClock::Khz(0),
                ..config
            }),
            Err(Error::InitClock)
        );
        assert_eq!(
            preset_plan(Config {
                init_clock: InitClock::Khz(1),
                ..config
            }),
            Err(Error::PresetClock)
        );
        assert_eq!(
            preset_plan(Config {
                init_clock: InitClock::Minimum,
                ..config
            })
            .unwrap()[0],
            Some((1023, Drive::B))
        );
        assert_eq!(
            preset_plan(Config {
                init_clock: InitClock::Khz(401),
                ..config
            }),
            Err(Error::InitClock)
        );
        assert_eq!(core::mem::offset_of!(Layout, presets), 0xf0);
        assert_eq!(core::mem::offset_of!(Layout, mode_clear), 0x116);
    }
}
