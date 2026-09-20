//! mc146818 RTC/CMOS configuration shared by Intel southbridges.
//!
//! Ported from coreboot `drivers/pc80/rtc/mc146818rtc.c` (`cmos_init`).
//!
//! A board whose RTC lost power (no CMOS battery, or a first-ever power-up)
//! comes up with unprogrammed clock registers: the update cycle never runs, so
//! the OS hangs on the update-in-progress bit and reports that it cannot read
//! the hardware clock. coreboot's answer, kept here, is to program the divider,
//! the control register and the validity bit, and to put a known date back.

use tock_registers::register_bitfields;

register_bitfields![u8,
    RtcFrequency [
        RATE OFFSET(0) NUMBITS(4) [],
        DIVIDER OFFSET(4) NUMBITS(3) []
    ],
    RtcControl [
        MODE_24_HOUR OFFSET(1) NUMBITS(1) [],
        BINARY_MODE OFFSET(2) NUMBITS(1) [],
        SET OFFSET(7) NUMBITS(1) []
    ],
    RtcValid [
        VALID OFFSET(7) NUMBITS(1) []
    ]
];

/// CMOS/RTC register indices (mc146818, via ports 0x70/0x71).
pub mod reg {
    pub const SECONDS: u8 = 0x00;
    pub const MINUTES: u8 = 0x02;
    pub const HOURS: u8 = 0x04;
    pub const DAY_OF_MONTH: u8 = 0x07;
    pub const MONTH: u8 = 0x08;
    pub const YEAR: u8 = 0x09;
    pub const DAY_OF_WEEK: u8 = 0x06;
    /// Divider and rate selection.
    pub const FREQ_SELECT: u8 = 0x0A;
    /// Control register: mode, interrupts, and the SET bit.
    pub const CONTROL: u8 = 0x0B;
    /// Flags register, cleared by reading it.
    pub const INTR_FLAGS: u8 = 0x0C;
    /// Valid RAM and time.
    pub const VALID: u8 = 0x0D;
}

/// Build-selected date used when the RTC lost power.
const DEFAULT_DATE: &str = match option_env!("FSTART_SMBIOS_DATE") {
    Some(date) => date,
    None => "01/01/2000",
};

/// 32.768 kHz reference with a 1024 Hz rate, coreboot's
/// `RTC_FREQ_SELECT_DEFAULT`.
fn frequency_select_default() -> u8 {
    (RtcFrequency::DIVIDER.val(2) + RtcFrequency::RATE.val(6)).value
}
/// 24-hour mode, BCD, no interrupts, counters running, coreboot's
/// `RTC_CONTROL_DEFAULT`.
fn control_default() -> u8 {
    RtcControl::MODE_24_HOUR::SET.value
}

fn control_mode(control: u8) -> u8 {
    let hour_mode = if RtcControl::MODE_24_HOUR::SET.any_matching_bits_set(control) {
        RtcControl::MODE_24_HOUR::SET.value
    } else {
        0
    };
    let data_mode = if RtcControl::BINARY_MODE::SET.any_matching_bits_set(control) {
        RtcControl::BINARY_MODE::SET.value
    } else {
        0
    };
    hour_mode | data_mode
}

/// Indexed mc146818 register access through ports 0x70/0x71.
struct Rtc;

const RTC: Rtc = Rtc;

impl Rtc {
    fn read(&self, register: u8) -> u8 {
        // SAFETY: port I/O is available in every stage that programs the RTC.
        unsafe {
            fstart_core::pio::outb(0x70, register);
            fstart_core::pio::inb(0x71)
        }
    }

    fn write(&self, value: u8, register: u8) {
        // SAFETY: as above; this never runs concurrently with another RTC user.
        unsafe {
            fstart_core::pio::outb(0x70, register);
            fstart_core::pio::outb(0x71, value);
        }
    }
}

/// Read one CMOS/RTC register, for callers that report the clock's state.
#[must_use]
pub fn read(register: u8) -> u8 {
    RTC.read(register)
}

const fn bin_to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

/// Decode one BCD register, rejecting values that are not decimal digits.
const fn bcd_to_bin(value: u8) -> Option<u8> {
    let (tens, ones) = (value >> 4, value & 0x0F);
    if tens > 9 || ones > 9 {
        return None;
    }
    Some(tens * 10 + ones)
}

/// Whether the time registers decode to a real time and calendar date.
///
/// `RTC_VRT` is not trustworthy on a board that lost its battery mid-life: it
/// can still read as valid while the time registers hold whatever the chip had.
/// The OS then fails the read (`rtc_valid_tm`) instead of the firmware noticing,
/// which is exactly what happened on the D41S. Treat an undecodable reading as
/// lost power. Both binary and BCD data, and both 12- and 24-hour readings,
/// are accepted according to register B, so a clock an OS already set is never
/// reset or silently reinterpreted.
#[must_use]
pub fn time_registers_are_valid() -> bool {
    time_registers_are_valid_in_mode(RTC.read(reg::CONTROL))
}

fn time_registers_are_valid_in_mode(control: u8) -> bool {
    time_registers_ok(
        [
            RTC.read(reg::SECONDS),
            RTC.read(reg::MINUTES),
            RTC.read(reg::HOURS),
            RTC.read(reg::DAY_OF_MONTH),
            RTC.read(reg::MONTH),
            RTC.read(reg::YEAR),
        ],
        control,
    )
}

fn decode_time_value(value: u8, binary_mode: bool) -> Option<u8> {
    if binary_mode {
        Some(value)
    } else {
        bcd_to_bin(value)
    }
}

/// Decode a raw `seconds, minutes, hours, day, month, year` reading in the mode
/// selected by RTC register B.
fn time_registers_ok(raw: [u8; 6], control: u8) -> bool {
    let [seconds, minutes, hours, day, month, year] = raw;
    let binary_mode = RtcControl::BINARY_MODE::SET.any_matching_bits_set(control);
    let hour_24 = RtcControl::MODE_24_HOUR::SET.any_matching_bits_set(control);
    let hours_ok = if hour_24 {
        matches!(decode_time_value(hours, binary_mode), Some(0..=23))
    } else {
        // In 12-hour mode bit 7 is the PM flag in both BCD and binary modes.
        matches!(decode_time_value(hours & 0x7f, binary_mode), Some(1..=12))
    };
    hours_ok
        && matches!(decode_time_value(minutes, binary_mode), Some(0..=59))
        && matches!(decode_time_value(seconds, binary_mode), Some(0..=59))
        && matches!(decode_time_value(month, binary_mode), Some(1..=12))
        && matches!(decode_time_value(day, binary_mode), Some(1..=31))
        && matches!(decode_time_value(year, binary_mode), Some(0..=99))
}

/// A calendar date written into the RTC when it lost power.
///
/// Firmware has no better time than this: without a battery the clock restarts
/// from whatever it was programmed with, so coreboot writes its own build date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

impl Date {
    /// Parse the `MM/DD/YYYY` form the build already uses for SMBIOS.
    #[must_use]
    pub fn parse_mm_dd_yyyy(value: &str) -> Option<Self> {
        let mut fields = value.split('/');
        let month = fields.next()?.parse().ok()?;
        let day = fields.next()?.parse().ok()?;
        let year = fields.next()?.parse().ok()?;
        if fields.next().is_some() {
            return None;
        }
        let date = Self { year, month, day };
        // Keep a typo from programming an impossible date the OS rejects.
        if !(1..=12).contains(&date.month) || !(1..=31).contains(&date.day) || date.year < 2000 {
            return None;
        }
        Some(date)
    }

    /// Day of week for the RTC's day-of-week register: 1 = Sunday.
    ///
    /// Sakamoto's algorithm; the register is advisory, but leaving a stale
    /// value from a lost-power RTC in place would be worse than computing it.
    #[must_use]
    pub fn weekday(self) -> u8 {
        const OFFSETS: [u16; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        let year = if self.month < 3 {
            self.year - 1
        } else {
            self.year
        };
        let offset = OFFSETS[usize::from(self.month) - 1];
        let index = (year + year / 4 - year / 100 + year / 400 + offset + u16::from(self.day)) % 7;
        // Sakamoto yields 0 = Sunday; the RTC uses 1 = Sunday.
        index as u8 + 1
    }

    fn write(self) {
        RTC.write(bin_to_bcd(0), reg::SECONDS);
        RTC.write(bin_to_bcd(0), reg::MINUTES);
        RTC.write(bin_to_bcd(0), reg::HOURS);
        RTC.write(bin_to_bcd(self.day), reg::DAY_OF_MONTH);
        RTC.write(bin_to_bcd(self.month), reg::MONTH);
        RTC.write(bin_to_bcd((self.year % 100) as u8), reg::YEAR);
        RTC.write(bin_to_bcd(self.weekday()), reg::DAY_OF_WEEK);
    }
}

/// What [`init`] found and did, for the caller's log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitReport {
    /// The RTC reported lost power (`RTC_VRT` clear).
    pub power_lost: bool,
    /// The time registers did not decode to a real date.
    pub time_invalid: bool,
    /// A date was written back into the time registers.
    pub date_reset: bool,
}

/// Program the RTC and CMOS the way coreboot's `cmos_init` does.
///
/// `battery_dead` is the southbridge's own sticky flag, `default_date` the date
/// written back when the clock lost power. Returns what was found so the caller
/// can log it; the registers are always left valid for the OS.
pub fn init(battery_dead: bool, default_date: Option<Date>) -> InitReport {
    // coreboot's cmos_error(): the clock reports a power problem.
    let power_lost = !RtcValid::VALID::SET.any_matching_bits_set(RTC.read(reg::VALID));
    let control = RTC.read(reg::CONTROL);
    let time_invalid = !time_registers_are_valid_in_mode(control);

    if power_lost || battery_dead || time_invalid {
        // Stop the divider before rewriting the time registers.
        RTC.write(RtcControl::SET::SET.modify(control), reg::CONTROL);
    }

    let date_reset = (power_lost || battery_dead || time_invalid) && default_date.is_some();
    if let Some(date) = default_date.filter(|_| date_reset) {
        date.write();
    }

    // Date::write emits BCD in 24-hour form. Otherwise preserve the existing
    // representation while clearing SET and interrupt enables.
    RTC.write(
        if date_reset {
            control_default()
        } else {
            control_mode(control)
        },
        reg::CONTROL,
    );
    RTC.write(frequency_select_default(), reg::FREQ_SELECT);
    // Mark the clock and CMOS RAM valid again, and drop any pending flags.
    RTC.write(RtcValid::VALID::SET.value, reg::VALID);
    let _ = RTC.read(reg::INTR_FLAGS);

    InitReport {
        power_lost,
        time_invalid,
        date_reset,
    }
}

/// Report and repair the CMOS clock the way coreboot's `__cmos_init` does.
///
/// `label` names the chipset that owns the clock in the log lines, and
/// `power_lost` is that chipset's sticky battery-dead state, which the caller
/// reads and clears from its own power-management registers.
pub fn init_clock(label: &str, power_lost: bool) {
    let date = match Date::parse_mm_dd_yyyy(DEFAULT_DATE) {
        Some(date) => Some(date),
        None => {
            fstart_log::error!("{}: RTC default date is not MM/DD/YYYY", label);
            None
        }
    };
    let report = init(power_lost, date);
    fstart_log::info!(
        "{}: RTC power_lost={} time_invalid={} date_reset={} (A={:#04x} B={:#04x} D={:#04x})",
        label,
        report.power_lost,
        report.time_invalid,
        report.date_reset,
        read(reg::FREQ_SELECT),
        read(reg::CONTROL),
        read(reg::VALID)
    );
    if report.date_reset {
        fstart_log::info!(
            "{}: RTC date set to {} (sec={:#04x} min={:#04x} hour={:#04x} day={:#04x} mon={:#04x} year={:#04x})",
            label,
            DEFAULT_DATE,
            read(reg::SECONDS),
            read(reg::MINUTES),
            read(reg::HOURS),
            read(reg::DAY_OF_MONTH),
            read(reg::MONTH),
            read(reg::YEAR)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_smbios_date_form() {
        assert_eq!(
            Date::parse_mm_dd_yyyy("12/25/2026"),
            Some(Date {
                year: 2026,
                month: 12,
                day: 25
            })
        );
        assert_eq!(
            Date::parse_mm_dd_yyyy("04/15/2026"),
            Some(Date {
                year: 2026,
                month: 4,
                day: 15
            })
        );
        for bad in [
            "",
            "15/04/2026",
            "04/15",
            "04/15/2026/1",
            "13/01/2026",
            "01/32/2026",
            "01/01/1999",
        ] {
            assert_eq!(Date::parse_mm_dd_yyyy(bad), None, "{bad}");
        }
    }

    #[test]
    fn weekday_matches_the_calendar() {
        // 2026-04-15 is a Wednesday, and the register counts 1 = Sunday.
        assert_eq!(
            Date {
                year: 2026,
                month: 4,
                day: 15
            }
            .weekday(),
            4
        );
        // 2024-01-01 is a Monday.
        assert_eq!(
            Date {
                year: 2024,
                month: 1,
                day: 1
            }
            .weekday(),
            2
        );
    }

    #[test]
    fn bcd_encoding_is_decimal_packed() {
        assert_eq!(bin_to_bcd(0), 0x00);
        assert_eq!(bin_to_bcd(9), 0x09);
        assert_eq!(bin_to_bcd(15), 0x15);
        assert_eq!(bin_to_bcd(26), 0x26);
    }

    #[test]
    fn bcd_decoding_rejects_non_digits() {
        assert_eq!(bcd_to_bin(0x00), Some(0));
        assert_eq!(bcd_to_bin(0x59), Some(59));
        assert_eq!(bcd_to_bin(0x1A), None);
        assert_eq!(bcd_to_bin(0xFF), None);
    }

    #[test]
    fn time_register_validation_follows_register_b_mode() {
        let bcd_24 = RtcControl::MODE_24_HOUR::SET.value;
        let bcd_12 = 0;
        let binary_24 = (RtcControl::MODE_24_HOUR::SET + RtcControl::BINARY_MODE::SET).value;
        let binary_12 = RtcControl::BINARY_MODE::SET.value;

        assert!(time_registers_ok(
            [0x59, 0x59, 0x23, 0x15, 0x04, 0x26],
            bcd_24
        ));
        assert!(time_registers_ok(
            [0x00, 0x00, 0x89, 0x15, 0x04, 0x26],
            bcd_12
        ));
        assert!(time_registers_ok([59, 59, 23, 15, 4, 26], binary_24));
        assert!(time_registers_ok([0, 0, 0x80 | 9, 15, 4, 26], binary_12));

        // The same bytes can be valid in one representation and invalid in
        // another; validation must never guess from the hour's PM bit.
        assert!(!time_registers_ok(
            [0x59, 0x59, 0x23, 0x15, 0x04, 0x26],
            binary_24
        ));
        assert!(!time_registers_ok([59, 59, 23, 15, 4, 26], bcd_24));
        // Garbage a lost-power RTC reports: non-decimal digits and zero fields.
        assert!(!time_registers_ok(
            [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
            bcd_24
        ));
        assert!(!time_registers_ok(
            [0x00, 0x00, 0x00, 0x00, 0x1A, 0x26],
            bcd_24
        ));
        assert!(!time_registers_ok(
            [0x00, 0x00, 0x00, 0x00, 0x04, 0x26],
            bcd_24
        ));
        assert!(!time_registers_ok(
            [0x00, 0x00, 0x60, 0x15, 0x04, 0x26],
            bcd_24
        ));
    }

    #[test]
    fn valid_clocks_keep_their_data_and_hour_modes() {
        let modes = [
            0,
            RtcControl::MODE_24_HOUR::SET.value,
            RtcControl::BINARY_MODE::SET.value,
            (RtcControl::MODE_24_HOUR::SET + RtcControl::BINARY_MODE::SET).value,
        ];
        for mode in modes {
            let noisy_control = mode | RtcControl::SET::SET.value | 0x70;
            assert_eq!(control_mode(noisy_control), mode);
        }
    }
}
