// SPDX-License-Identifier: MIT OR Apache-2.0
//! Qilian's I2C/I3C controllers as the vendor's `FchI2cMasterPei` brings
//! them up (Dell PR9XJ, `4ac318edf91d4a26`, base `0xe3f300`; DEVICES.md
//! §2.6). Each IOD's FCH has two DesignWare masters: controller 0 at
//! `FEDC_2000h` (shared with the I3C block, PCD `PcdAmdFchI2cI3c0..3`) and
//! controller 2 at `FEDC_4000h` (`PcdAmdFchI2c4..7`); the other dies reach
//! theirs over SMN (`0x02DC_2000`, `0x02DD_4000`). Controllers 1, 3, 4
//! and 5 of the older FCHs do not exist on Qilian.
//!
//! Timing (`I2cInfo` table `0xe4b378`, 8 × 0x24 bytes [I]; the init
//! [D]): `IC_CON` 0x63/0x65/0x67 (standard/fast/high speed, restart on),
//! `IC_FS_SPKLEN` 7, standard SCL high/low 0x285/0x357, fast and high
//! speed 0x71/0xDA, SDA hold `PcdAmdFchI2c{n}SdaHold` = 0x35 [P]. The
//! DesignWare driver ([`fstart_driver_i2c::designware`]) owns the
//! register sequence; this module holds the data.
//!
//! Power: the vendor's `FchRTDeviceEnableMap` (Dell 0x7E0 [P]) turns on
//! AOAC devices 5..10 (I2C 0..5) in the FCH's AOAC init; controller `n` is
//! AOAC device `5 + n` (the older FCHs' numbering, [`crate::fch::smbus`];
//! not re-traced on Qilian).
//!
//! The I3C blocks (`FchI3cPei/Dxe`) serve the DIMMs' SPD hubs for the ABL;
//! the host leaves them alone. SMBus and ASF need no host step to boot
//! (`PcdAmdFchDisableAsfSlave` 0, SMBus SSID/SSVID 0 on Dell [P]).

use fstart_driver_i2c::designware::{Speed, Timing};

use crate::fch::FchHw;
use crate::fch::acpimmio::{self, AoacDevice};

/// The controllers of one FCH: `(index, local MMIO, SMN address)`.
pub const CONTROLLERS: [(u8, u32, u32); 2] =
    [(0, 0xfedc_2000, 0x02dc_2000), (2, 0xfedc_4000, 0x02dd_4000)];

/// SDA hold time in controller clocks (`PcdAmdFchI2c{n}SdaHold`, Dell 53).
pub const SDA_HOLD: u16 = 0x35;

/// The vendor's standard-mode timing.
pub const STANDARD: Timing = Timing {
    speed: Speed::Standard,
    scl_high: 0x285,
    scl_low: 0x357,
    spike_length: Some(7),
    sda_hold: Some(SDA_HOLD),
};

/// The vendor's fast-mode timing (its high-speed row has the same counts).
pub const FAST: Timing = Timing {
    speed: Speed::Fast,
    scl_high: 0x71,
    scl_low: 0xda,
    spike_length: Some(7),
    sda_hold: Some(SDA_HOLD),
};

/// The vendor's `IC_CON` for a speed: master, 7-bit, restart, slave off,
/// speed in bits 2:1 (`0x63`, `0x65`, `0x67` for standard, fast, high).
pub const fn ic_con(speed: u8) -> u32 {
    0x61 | ((speed as u32 & 3) << 1)
}

/// The local MMIO base of controller `n`, `None` if Qilian has no such
/// controller.
pub fn base(n: u8) -> Option<u32> {
    CONTROLLERS
        .iter()
        .find(|c| c.0 == n)
        .map(|&(_, base, _)| base)
}

/// Power controller `n` (AOAC device `5 + n`); `false` if Qilian has no
/// such controller.
pub fn enable(fch: &impl FchHw, n: u8) -> bool {
    if base(n).is_none() {
        return false;
    }
    acpimmio::aoac_power(fch.acpi(), AoacDevice(5 + n), true);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::model::fch;

    #[test]
    fn two_controllers_with_the_vendor_timing() {
        assert_eq!(base(0), Some(0xfedc_2000));
        assert_eq!(base(2), Some(0xfedc_4000));
        assert_eq!(base(1), None);
        assert_eq!([ic_con(1), ic_con(2), ic_con(3)], [0x63, 0x65, 0x67]);
        assert_eq!((STANDARD.scl_high, STANDARD.scl_low), (0x285, 0x357));
        assert_eq!((FAST.scl_high, FAST.scl_low), (0x71, 0xda));
        let hw = fch();
        assert!(enable(&hw, 2));
        assert_eq!(
            hw.acpi.get(acpimmio::AOAC + 0x40 + 2 * 7, 1) & 8,
            8,
            "AOAC device 7"
        );
        assert!(!enable(&hw, 3));
    }
}
