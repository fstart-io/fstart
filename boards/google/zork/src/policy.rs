// SPDX-License-Identifier: MIT OR Apache-2.0
//! EC CBI decisions.
//! Policy data only. FSP-owned power/display/PHY settings are retained here,
//! but have no native programming implementation yet (see docs/amd-picasso.md).
#[derive(Debug, Clone, Copy)]
pub struct Cbi {
    /// None means the EC read failed. Zero is a valid board version/config.
    pub board_version: Option<u32>,
    pub firmware_config: Option<u64>,
}
impl Cbi {
    pub const fn new(board_version: Option<u32>, firmware_config: Option<u64>) -> Self {
        Self {
            board_version,
            firmware_config,
        }
    }
    pub const fn field(self, shift: u8, mask: u64) -> u64 {
        match self.firmware_config {
            Some(value) => (value >> shift) & mask,
            None => 0,
        }
    }
    pub const fn has_wwan(self) -> bool {
        self.field(29, 1) != 0
    }
    pub const fn has_nvme(self) -> bool {
        self.field(18, 1) != 0
    }
    pub const fn has_emmc(self) -> bool {
        self.field(19, 1) != 0
    }
    pub const fn convertible(self) -> bool {
        self.field(15, 1) != 0
    }
    pub const fn sar(self) -> u8 {
        self.field(23, 7) as u8
    }
    pub const fn audio_amp_auto(self) -> bool {
        self.field(35, 1) != 0
    }
    pub const fn v3_6(self) -> bool {
        matches!(self.board_version, Some(v) if v >= 2)
    }
    // Vilboz does not support pre-v3 schematics. CBI failure still selects v3.
    pub const fn v3(self) -> bool {
        true
    }
    pub const fn active_low_wifi_power(self) -> bool {
        matches!(self.board_version, Some(v) if v >= 2)
    }
    pub const fn external_audio_clock(self) -> bool {
        matches!(self.board_version, Some(v) if v > 2) && self.has_wwan()
    }
    pub const fn telemetry(self) -> Telemetry {
        if self.field(26, 1) != 0 {
            Telemetry {
                vdd_slope_ma: 32453,
                vdd_offset: 168,
                soc_slope_ma: 22644,
                soc_offset: -70,
            }
        } else {
            Telemetry {
                vdd_slope_ma: 27225,
                vdd_offset: 187,
                soc_slope_ma: 26559,
                soc_offset: 89,
            }
        }
    }
    pub const fn sar_filename(self) -> Option<&'static str> {
        match self.sar() {
            1 => Some("wifi_sar-vilboz-0.hex"),
            3 => Some("wifi_sar-vilboz-2.hex"),
            5 => Some("wifi_sar-vilboz-1.hex"),
            7 => Some("wifi_sar-vilboz-3.hex"),
            _ => None, // platform's default SAR, not a guessed variant
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Telemetry {
    pub vdd_slope_ma: u32,
    pub vdd_offset: i32,
    pub soc_slope_ma: u32,
    pub soc_offset: i32,
}
#[derive(Debug, Clone, Copy)]
pub struct PowerPolicy {
    pub system_config: u8,
    pub slow_ppt_mw: u32,
    pub fast_ppt_mw: u32,
    pub slow_ppt_seconds: u32,
    pub stapm_seconds: u32,
    pub sustained_mw: u32,
}
pub const POWER: PowerPolicy = PowerPolicy {
    system_config: 1,
    slow_ppt_mw: 6000,
    fast_ppt_mw: 9000,
    slow_ppt_seconds: 5,
    stapm_seconds: 1400,
    sustained_mw: 4800,
};

/// Dalboz PCIe routing. These descriptors are NOT a native link-training API.
#[derive(Debug, Clone, Copy)]
pub struct PciePort {
    pub lanes: (u8, u8),
    pub device: u8,
    pub function: u8,
    pub clock_request: u8,
}
pub const PCIE: &[PciePort] = &[
    PciePort {
        lanes: (4, 5),
        device: 1,
        function: 7,
        clock_request: 2,
    },
    PciePort {
        lanes: (0, 0),
        device: 1,
        function: 2,
        clock_request: 0,
    },
    PciePort {
        lanes: (1, 1),
        device: 1,
        function: 3,
        clock_request: 1,
    },
];
#[derive(Debug, Clone, Copy)]
pub enum Connector {
    Edp,
    Hdmi,
    DisplayPort,
}
#[derive(Debug, Clone, Copy)]
pub struct Display {
    pub connector: Connector,
    pub aux: u8,
    pub hotplug: u8,
}
pub const DISPLAYS: &[Display] = &[
    Display {
        connector: Connector::Edp,
        aux: 1,
        hotplug: 1,
    },
    Display {
        connector: Connector::Hdmi,
        aux: 2,
        hotplug: 2,
    },
    Display {
        connector: Connector::DisplayPort,
        aux: 4,
        hotplug: 4,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revisions_and_failed_reads() {
        let missing = Cbi::new(None, None);
        assert!(missing.v3());
        assert!(!missing.v3_6());
        assert!(!missing.active_low_wifi_power());
        assert!(!Cbi::new(Some(2), Some(1 << 29)).external_audio_clock());
        assert!(Cbi::new(Some(3), Some(1 << 29)).external_audio_clock());
        assert!(Cbi::new(Some(2), None).active_low_wifi_power());
        assert_eq!(Cbi::new(None, Some(1 << 26)).telemetry().soc_offset, -70);
        assert!(Cbi::new(None, Some(1 << 35)).audio_amp_auto()); // FW_CONFIG is 64-bit
        assert!(!Cbi::new(Some(0), Some(0)).has_wwan());
    }
    #[test]
    fn sar_skus() {
        for (id, suffix) in [(1, "0"), (3, "2"), (5, "1"), (7, "3")] {
            let cbi = Cbi::new(None, Some(id << 23));
            assert!(
                cbi.sar_filename()
                    .unwrap()
                    .as_bytes()
                    .contains(&suffix.as_bytes()[0])
            );
        }
        assert!(Cbi::new(None, Some(2 << 23)).sar_filename().is_none());
    }
}
