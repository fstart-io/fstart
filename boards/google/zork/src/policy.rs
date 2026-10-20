// SPDX-License-Identifier: MIT OR Apache-2.0
//! EC CBI decisions. Board data, not hardware sequences. Native driver APIs consume power/PHY
//! policy; complete boot/display integration remains pending.
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
    /// This quirk uses legacy EC command6 board_id(), NOT CBI tag0 used
    /// for schematic/WiFi revision. Unknown legacy ID must suppress it.
    pub const fn external_audio_clock(self, legacy_board_version: Option<u16>) -> bool {
        matches!(legacy_board_version, Some(v) if v > 2) && self.has_wwan()
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
    /// Dalboz voltage/thermal policy plus Vilboz PPT and EC-selected telemetry.
    pub fn native_power(self) -> fstart_driver_amd::smu::v10::power::Config {
        use fstart_driver_amd::smu::v10::power::{Config, Telemetry};
        let telemetry = self.telemetry();
        Config {
            sustained_mw: POWER.sustained_mw,
            fast_mw: POWER.fast_ppt_mw,
            slow_mw: POWER.slow_ppt_mw,
            slow_seconds: POWER.slow_ppt_seconds,
            stapm_seconds: POWER.stapm_seconds,
            system_config: POWER.system_config,
            thermal_c: 100,
            psi0_ma: 18000,
            soc_psi0_ma: 12000,
            prochot_ms: 20,
            core_dldo_bypass: true,
            telemetry: [
                Telemetry {
                    slope_ma: telemetry.vdd_slope_ma,
                    offset: telemetry.vdd_offset,
                },
                Telemetry {
                    slope_ma: telemetry.soc_slope_ma,
                    offset: telemetry.soc_offset,
                },
            ],
            ..Config::default()
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

use fstart_driver_amd::picasso::{
    dxio::{Aspm, ClockRequest, Descriptor},
    emmc, usb,
};

/// Zork's non-EM100 SPI defaults. A validated runtime EFS may
/// override fast speed/read mode via SpiConfig::with_fast_override().
pub const SPI: fstart_driver_amd::picasso::preinit::SpiConfig = {
    use fstart_driver_amd::picasso::preinit::{ReadMode, Speed, SpiConfig};
    SpiConfig {
        mode: ReadMode::Dual122,
        normal: Speed::Mhz33,
        fast: Speed::Mhz66,
        alternate: Speed::Mhz66,
        tpm: Speed::Mhz66,
        burst_4dw: false,
    }
};

/// Dalboz routing and ASPM/clock policy.
/// Native descriptors, not an FSP ABI. Pin0 is clock-request pin zero (the
/// wire code is one); it must not be confused with ClockRequest::Disabled.
const fn pcie(first: u8, last: u8, function: u8, clock: ClockRequest) -> Descriptor {
    let mut descriptor = Descriptor::pcie(first, last, 1, function);
    descriptor.aspm = Aspm::L1;
    descriptor.clock_request = clock;
    descriptor.l1_1 = true;
    descriptor.l1_2 = true;
    descriptor.turn_off_unused_lanes = true;
    descriptor
}
pub const PCIE: &[Descriptor] = &[
    pcie(4, 5, 7, ClockRequest::Pin2), // NVMe
    pcie(0, 0, 2, ClockRequest::Pin0), // WLAN
    pcie(1, 1, 3, ClockRequest::Pin1), // Bayhub SD reader
];
/// Dalboz eSPI resource wiring shared by all Vilboz variants.
pub const ESPI: fstart_driver_amd::picasso::espi::Config = {
    use fstart_driver_amd::picasso::espi::{Alert, Config, Frequency, IoRange, Mode};
    Config {
        io: [
            Some(IoRange {
                base: 0x62,
                size: 5,
            }),
            Some(IoRange {
                base: 0x800,
                size: 256,
            }),
            Some(IoRange {
                base: 0x900,
                size: 255,
            }),
            Some(IoRange {
                base: 0x200,
                size: 8,
            }),
        ],
        port80: true,
        keyboard: true,
        superio: false,
        mode: Mode::Quad,
        frequency: Frequency::Mhz33,
        alert: Alert::PushPull,
        crc: true,
        peripheral: true,
        virtual_wire: true,
        oob: false,
        flash: false,
        subtractive: false,
        irq_polarity: 0, // HIGH IRQ1/12 means incoming VW inversion enabled.
    }
};

/// Vilboz supplies all six USB2 tuning records, overriding Dalboz defaults.
/// Port 1 carries LTE rather than the second USB-C connector, so has no OC pin.
pub const USB: usb::Config = usb::Config {
    phy2: Some(
        [usb::Phy2 {
            com_pds: 7,
            ..usb::Phy2::DEFAULT
        }; 6],
    ),
    phy3_override: None,
    overcurrent: [
        usb::Overcurrent::Pin0,
        usb::Overcurrent::None,
        usb::Overcurrent::Pin0,
        usb::Overcurrent::Pin1,
        usb::Overcurrent::None,
        usb::Overcurrent::None,
    ],
    force_gen1: 0,
};
pub const EMMC: emmc::Config = emmc::Config {
    timing: emmc::Timing::Hs400,
    init_clock: emmc::InitClock::Khz(400),
    sdr50_drive: emmc::Drive::B,
    sdr104_hs400_drive: emmc::Drive::A,
    ddr50_drive: emmc::Drive::B,
};
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
        assert!(!Cbi::new(Some(3), Some(1 << 29)).external_audio_clock(Some(2)));
        assert!(!Cbi::new(Some(3), Some(1 << 29)).external_audio_clock(None));
        assert!(Cbi::new(Some(1), Some(1 << 29)).external_audio_clock(Some(3)));
        assert!(!Cbi::new(Some(3), Some(0)).external_audio_clock(Some(3)));
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
