//! mc146818 RTC/CMOS configuration shared by Intel southbridges.
//!
//! Ported from coreboot `drivers/pc80/rtc/mc146818rtc.c` (`cmos_init`).
//!
//! A board whose RTC lost power (no CMOS battery, or a first-ever power-up)
//! comes up with unprogrammed clock registers: the update cycle never runs, so
//! the OS hangs on the update-in-progress bit and reports that it cannot read
//! the hardware clock. coreboot's answer, kept here, is to program the divider,
//! the control register and the validity bit, and to put a known date back.

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

/// 32.768 kHz reference with a 1024 Hz rate, coreboot's
/// `RTC_FREQ_SELECT_DEFAULT`.
const FREQ_SELECT_DEFAULT: u8 = 0x20 | 0x06;
/// 24-hour mode, BCD, no interrupts, counters running, coreboot's
/// `RTC_CONTROL_DEFAULT`.
const CONTROL_DEFAULT: u8 = 0x02;
/// `RTC_SET`: stops the divider so the time registers can be rewritten.
const CONTROL_SET: u8 = 0x80;
/// `RTC_VRT`: data in the time/RAM registers is valid.
const VALID_VRT: u8 = 0x80;

fn cmos_read(register: u8) -> u8 {
    // SAFETY: port I/O is available in every stage that programs the RTC.
    unsafe {
        fstart_core::pio::outb(0x70, register);
        fstart_core::pio::inb(0x71)
    }
}

/// Read one CMOS/RTC register, for callers that report the clock's state.
#[must_use]
pub fn read(register: u8) -> u8 {
    cmos_read(register)
}

fn cmos_write(value: u8, register: u8) {
    // SAFETY: as above; this never runs concurrently with another RTC user.
    unsafe {
        fstart_core::pio::outb(0x70, register);
        fstart_core::pio::outb(0x71, value);
    }
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
/// lost power. Both 24-hour (what this module programs) and 12-hour readings are
/// accepted, so a clock an OS already set is never reset by mistake.
#[must_use]
pub fn time_registers_are_valid() -> bool {
    time_registers_ok([
        cmos_read(reg::SECONDS),
        cmos_read(reg::MINUTES),
        cmos_read(reg::HOURS),
        cmos_read(reg::DAY_OF_MONTH),
        cmos_read(reg::MONTH),
        cmos_read(reg::YEAR),
    ])
}

/// Decode a raw `seconds, minutes, hours, day, month, year` reading.
fn time_registers_ok(raw: [u8; 6]) -> bool {
    let [seconds, minutes, hours, day, month, year] = raw;
    let _ = year;
    let hours_ok = if hours & 0x80 != 0 {
        // 12-hour mode: bit 7 is the PM flag, the rest is 1..12 in BCD.
        matches!(bcd_to_bin(hours & 0x7F), Some(1..=12))
    } else {
        matches!(bcd_to_bin(hours), Some(0..=23))
    };
    hours_ok
        && matches!(bcd_to_bin(minutes), Some(0..=59))
        && matches!(bcd_to_bin(seconds), Some(0..=59))
        && matches!(bcd_to_bin(month), Some(1..=12))
        && matches!(bcd_to_bin(day), Some(1..=31))
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
        cmos_write(bin_to_bcd(0), reg::SECONDS);
        cmos_write(bin_to_bcd(0), reg::MINUTES);
        cmos_write(bin_to_bcd(0), reg::HOURS);
        cmos_write(bin_to_bcd(self.day), reg::DAY_OF_MONTH);
        cmos_write(bin_to_bcd(self.month), reg::MONTH);
        cmos_write(bin_to_bcd((self.year % 100) as u8), reg::YEAR);
        cmos_write(bin_to_bcd(self.weekday()), reg::DAY_OF_WEEK);
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
    let power_lost = cmos_read(reg::VALID) & VALID_VRT == 0;
    let time_invalid = !time_registers_are_valid();

    if power_lost || battery_dead || time_invalid {
        // Stop the divider before rewriting the time registers.
        cmos_write(cmos_read(reg::CONTROL) | CONTROL_SET, reg::CONTROL);
    }

    let date_reset = (power_lost || battery_dead || time_invalid) && default_date.is_some();
    if let Some(date) = default_date.filter(|_| date_reset) {
        date.write();
    }

    cmos_write(CONTROL_DEFAULT, reg::CONTROL);
    cmos_write(FREQ_SELECT_DEFAULT, reg::FREQ_SELECT);
    // Mark the clock and CMOS RAM valid again, and drop any pending flags.
    cmos_write(VALID_VRT, reg::VALID);
    let _ = cmos_read(reg::INTR_FLAGS);

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
pub fn init_clock(label: &str, power_lost: bool, default_date: &str) {
    let date = match Date::parse_mm_dd_yyyy(default_date) {
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
            default_date,
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
    fn time_register_validation_accepts_clocks_and_rejects_garbage() {
        // 24-hour 23:59:59 on 2026-04-15, then the same clock at 11:59:59.
        assert!(time_registers_ok([0x59, 0x59, 0x23, 0x15, 0x04, 0x26]));
        assert!(time_registers_ok([0x59, 0x59, 0x11, 0x15, 0x04, 0x26]));
        // 12-hour mode carries the PM flag in bit 7.
        assert!(time_registers_ok([0x00, 0x00, 0x92, 0x15, 0x04, 0x26]));
        assert!(!time_registers_ok([0x00, 0x00, 0x9F, 0x15, 0x04, 0x26]));
        // Garbage a lost-power RTC reports: non-decimal digits and zero fields.
        assert!(!time_registers_ok([0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]));
        assert!(!time_registers_ok([0x00, 0x00, 0x00, 0x00, 0x1A, 0x26]));
        assert!(!time_registers_ok([0x00, 0x00, 0x00, 0x00, 0x04, 0x26]));
        assert!(!time_registers_ok([0x00, 0x00, 0x60, 0x15, 0x04, 0x26]));
    }
}
