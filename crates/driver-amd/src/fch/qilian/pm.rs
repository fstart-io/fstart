// SPDX-License-Identifier: MIT OR Apache-2.0
//! The steps of Qilian's HW-ACPI init that are not tables
//! ([`super::hwacpi`]): `FchInitResetHwAcpiQl 0xf26650` and the Qilian
//! transfer table's callbacks, as the vendor runs them on the primary FCH
//! (Dell PR9XJ SilVenicePei; GIGABYTE MD25-CG0 identical where checked;
//! DEVICES.md §2.8, [D] unless noted). In the vendor's order:
//!
//! 1. after [`super::hwacpi::RESET`]: the FCH watchdog's decode
//!    (`PMx00[7]`, [`Reset::watchdog_decode`]), the RTC extension's check
//!    ([`rtc_ext`]), the GEVENTs that become level SCIs at pre-OS
//!    (`SMIx78/79`, Ql `+0x24`, [`SCI_LEVEL_GEVENTS`]), then the WA
//!    features ([`wa_features`], `FchHwAcpiInitWaFeature`);
//! 2. after [`super::hwacpi::RESET2`], PrePcie (Ql `+0x08`):
//!    [`super::hwacpi::PRE_PCIE`], the SMBus function's `x10/x11`
//!    ([`SMBUS_PCI`]) and the watchdog's cold-reset acknowledge (`PMxC6`,
//!    [`ColdReset`]);
//! 3. then [`misc`]: `PMxED[1]`, `PMxBB[6]`, `PMx74[27:24]`, the AC-loss
//!    shadow `PMx5B`, the HPET decode and MSI (`PMx00[6]`, `[29]`; FCH.md
//!    §2.5 had it unset: `0xf25670` sets it), `FEDC_0010/0030[1]`, SD off,
//!    `PMxA0`, `PMx10[1]`, the serial debug bus;
//! 4. at ready-to-boot ([`pre_os`], `FchInitPreOsHwAcpiQl`, Ql `+0x0C`):
//!    the GEVENTs of `SMIx78/79` level-triggered (`SMIx0C`) and the late
//!    `PMxC0` write.
//!
//! Not here: the boot timer (`PMx44..47`, `PMx93`) and the S5 reset status
//! (`PMxC0`) before the late write, which the reset code owns; the
//! spread-spectrum step, a no-op on Qilian (its `+0x1C` is empty,
//! `0xf4aa10` [I]).
//!
//! The watchdog behind [`WDAT`]: no x86 code of either donor (PEI, DXE,
//! SMM; nor the PSP/ABL payloads' FCH references) writes its count period
//! (`PMx03[1:0]`) or its disable bits (`PMx03[3:2]`); [`watchdog_clock`]
//! sets them so the period a WDAT publishes is the counter's
//! (RESIDUAL.md §5).

use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

use crate::fch::acpimmio::{
    self, Decode, PM, PM_ACPI_PM1_EVT, PM_DECODE, PM_RESET_CONTROL0, PM_RTC_SHADOW, PM_SERIAL_IRQ,
    Pm1StatusHigh, ResetControl0, RtcShadow, SMI, SerialIrq, WDT_CONTROL, WatchdogControl,
};
use crate::fch::smbus::SMBUS_IO;
use crate::fch::{FchHw, irq, rtc, smi};
use crate::window::{At, Window};

register_bitfields![u16,
    /// PMxC6 (16-bit): the FCH watchdog's cold-reset acknowledge.
    pub WdtColdReset [
        TIMER OFFSET(0) NUMBITS(11) [],
        /// Bits 13:11, kept.
        KEEP OFFSET(11) NUMBITS(3) [],
        LOCK OFFSET(14) NUMBITS(1) [],
        ACK OFFSET(15) NUMBITS(1) []
    ]
];

register_bitfields![u8,
    /// PMxED.
    pub PmEd [
        /// `NoneSioKbcSupport`: no KBC behind LPC/eSPI.
        NONE_SIO_KBC OFFSET(1) NUMBITS(1) []
    ],
    /// PMxBB.
    pub PmBb [
        BIT6 OFFSET(6) NUMBITS(1) []
    ],
    /// PMx77: PMx74\[31:24\].
    pub AcpiConfig3 [
        /// PMx74\[27:24\]: 4 (`0xf25770`).
        FIELD OFFSET(0) NUMBITS(4) []
    ],
    /// ACPI MMIO `+0x1000` (`FED8_1000h`).
    pub SerialDebug [
        BUS OFFSET(0) NUMBITS(3) []
    ]
];

register_bitfields![u32,
    /// PMx80: the break events the FCH latched; the vendor's RAS DXE logs
    /// bits 11:0 and clears them by writing them 0 (`0x1eed0` [D]).
    pub BreakEvent [
        ERRORS OFFSET(0) NUMBITS(12) []
    ],
    /// PMxA0.
    pub PmA0 [
        BIT0 OFFSET(0) NUMBITS(1) [],
        /// `TimerTickTrack`.
        TIMER_TICK_TRACK OFFSET(1) NUMBITS(1) [],
        /// `ClockInterruptTag`.
        CLOCK_INTERRUPT_TAG OFFSET(10) NUMBITS(1) []
    ]
];

/// PMxC6.
pub const PM_WDT_COLD_RESET: At<u16, WdtColdReset::Register> = At::new(PM + 0xc6);
/// PMxED.
pub const PM_ED: At<u8, PmEd::Register> = At::new(PM + 0xed);
/// PMxBB.
pub const PM_BB: At<u8, PmBb::Register> = At::new(PM + 0xbb);
/// PMx77.
pub const PM_ACPI_CONFIG3: At<u8, AcpiConfig3::Register> = At::new(PM + 0x77);
/// PMxA0.
pub const PM_A0: At<u32, PmA0::Register> = At::new(PM + 0xa0);
/// `FED8_1000h`.
pub const SERIAL_DEBUG: At<u8, SerialDebug::Register> = At::new(0x1000);
/// SMIx78/79: the two GEVENTs made level SCIs at pre-OS.
pub const SMI_SCI_LEVEL_GEVENTS: At<u16> = At::new(SMI + 0x78);
/// PMx80 (32-bit): the FCH's break events.
pub const PM_BREAK_EVENT: At<u32, BreakEvent::Register> = At::new(PM + 0x80);

/// The vendor's `SMIx78/79` (Ql `+0x24`, `0xf4aa30`): GEVENT 11 twice.
pub const SCI_LEVEL_GEVENTS: u16 = 0x0b0b;

/// The SMBus function's (D14F0) `x10`/`x11` byte writes of PrePcie
/// (Dell `0xfab520`, GIGABYTE `0x709e8b68`, identical [I]), meaning not
/// known.
pub const SMBUS_PCI: [(u32, u8); 2] = [(0x10, 0x11), (0x11, 0x09)];

/// The FCH watchdog's WDAT actions as both donors' static WDAT has them
/// (Dell DXE FV, GIGABYTE `WDAT` "H2O BIOS", entries byte-identical [I];
/// DEVICES/acpi_{dell,gb}.json): `(action, instruction, register offset
/// in the ACPI MMIO, value, mask)` over the watchdog's control (`+0xB00`)
/// and count (`+0xB04`) registers. The donors' timer periods differ (Dell
/// 600 ms, GIGABYTE 1000 ms per count; max count 0x3FF, min 2, enabled
/// and stopped in sleep); neither firmware patches the table (Dell
/// AcpiPlatform `0x5860` only drops it without its `OsWatchdogTimer`
/// variable, GIGABYTE ChipsetSvcDxe patches `WDRT`, not `WDAT`) nor sets
/// the counter's clock. The period here is [`WatchdogClock::period_ms`]
/// of the clock [`watchdog_clock`] sets.
pub const WDAT: [(u8, u8, u32, u32, u32); 13] = [
    (0x01, 0x82, 0xb00, 0x80, 0x80),
    (0x04, 0x01, 0xb04, 0x0, 0xffff),
    (0x06, 0x83, 0xb04, 0x0, 0xffff),
    (0x08, 0x00, 0xb00, 0x1, 0x1),
    (0x09, 0x82, 0xb00, 0x1, 0x1),
    (0x0a, 0x00, 0xb00, 0x0, 0x1),
    (0x0b, 0x82, 0xb00, 0x0, 0x1),
    (0x10, 0x00, 0xb00, 0x0, 0x4),
    (0x11, 0x82, 0xb00, 0x0, 0x4),
    (0x12, 0x00, 0xb00, 0x4, 0x4),
    (0x13, 0x82, 0xb00, 0x4, 0x4),
    (0x20, 0x00, 0xb00, 0x2, 0x2),
    (0x21, 0x82, 0xb00, 0x2, 0x2),
];
/// The WDAT's maximum and minimum count and flags (both donors).
pub const WDAT_MAX_COUNT: u32 = 0x3ff;
pub const WDAT_MIN_COUNT: u32 = 2;
pub const WDAT_FLAGS: u8 = 0x81;

/// The watchdog's cold-reset acknowledge (`0xf26b80`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColdReset {
    /// `PcdFchWdtColdResetAck`: set the acknowledge (bit 15).
    pub ack: bool,
    /// Bit 14 when acknowledged.
    pub lock: bool,
    /// Bits 10:0.
    pub timer: u16,
}

impl ColdReset {
    /// Dell's build values [P]: no acknowledge, no lock, timer 46.
    pub const DEFAULT: Self = Self {
        ack: false,
        lock: false,
        timer: 46,
    };
}

/// The FCH watchdog's count period (`PMx03[1:0]`) a WDAT advertises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogClock {
    Ms10,
    Ms100,
    S1,
}

impl WatchdogClock {
    /// The WDAT's "timer period" (ms per count).
    pub const fn period_ms(self) -> u32 {
        match self {
            Self::Ms10 => 10,
            Self::Ms100 => 100,
            Self::S1 => 1000,
        }
    }
}

/// Stop the FCH watchdog (`WDTx00[0]` clear), as Linux's `sp5100_tco`
/// does before it sets a period: a warm reset after an OS that left it
/// running may keep its run bit. Its fired status (`[1]`, write-1-to-
/// clear) is kept for the OS (`sp5100_tco`'s boot status) and returned.
pub fn stop_watchdog(acpi: &impl Window) -> bool {
    let control = acpi.at(WDT_CONTROL);
    let value = control.extract();
    if value.get() == u32::MAX {
        return false;
    }
    control.modify(
        WatchdogControl::RUN::CLEAR
            + WatchdogControl::FIRED::CLEAR
            + WatchdogControl::TRIGGER::CLEAR,
    );
    value.is_set(WatchdogControl::FIRED)
}

/// Count the FCH watchdog in `clock` periods and leave it enabled
/// (`PMx03[3:2]` clear, as Linux's `sp5100_tco` does for the EFCH) but
/// stopped ([`stop_watchdog`], before and after the change); it runs only
/// once the OS sets its run bit through the WDAT. Returns whether it had
/// fired.
pub fn watchdog_clock(acpi: &impl Window, clock: WatchdogClock) -> bool {
    let fired = stop_watchdog(acpi);
    let freq = match clock {
        WatchdogClock::Ms10 => Decode::WATCHDOG_FREQ::Ms10,
        WatchdogClock::Ms100 => Decode::WATCHDOG_FREQ::Ms100,
        WatchdogClock::S1 => Decode::WATCHDOG_FREQ::S1,
    };
    acpi.at(PM_DECODE)
        .modify(freq + Decode::WATCHDOG_DISABLE.val(0));
    stop_watchdog(acpi) || fired
}

/// The inputs of `FchHwAcpiInitWaFeature` (Dell `0xf25ae0`, GIGABYTE
/// `0x70997058`, same steps [D]): FCH class input block bytes the host
/// glue fills from PCDs (Dell `FchDataInitClass 0xf0d490`; GIGABYTE's
/// values by its PCD database, VALUES/host-blocks.json).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaFeatures {
    /// FCH class `+0x2F` (unnamed; openSIL default 0, set by neither
    /// host): leave PCI_INTR 08h bits 1/3 and 09h bits 2:1 alone.
    pub keep_intr_misc: bool,
    /// `+0x31`, `PcdFchOscout1ClkContinous` (0/0): `PMx54[6]` cleared.
    pub oscout1_clk_continuous: bool,
    /// `+0x33`, `PcdSerialIrqEnable` (1/1): `PMx54[7]` set.
    pub serial_irq: bool,
}

impl WaFeatures {
    /// Both donors' build values.
    pub const DEFAULT: Self = Self {
        keep_intr_misc: false,
        oscout1_clk_continuous: false,
        serial_irq: true,
    };
}

/// What the reset steps read of the platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reset {
    /// `PcdFchWdtEnable` (Dell 1): the FCH watchdog's MMIO decode.
    pub watchdog_decode: bool,
    pub cold_reset: ColdReset,
    pub wa: WaFeatures,
}

impl Reset {
    pub const DEFAULT: Self = Self {
        watchdog_decode: true,
        cold_reset: ColdReset::DEFAULT,
        wa: WaFeatures::DEFAULT,
    };
}

/// What [`misc`] reads of the platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Misc {
    /// `NoneSioKbcSupport`: `PMxED[1]`.
    pub none_sio_kbc: bool,
    /// The AC-loss policy, `PMx5B[1:0]` (0 off, 1 on, 3 previous);
    /// `None` leaves it to the board's early code ([`crate::fch::early`]).
    pub power_failure: Option<u8>,
    /// `PcdHpetEnable` (Dell 1).
    pub hpet: bool,
    /// `!PcdHpetMsiDis` (Dell `PcdHpetMsiDis` = 1: off).
    pub hpet_msi: bool,
    /// `PcdSdConfig` non-zero keeps the SD controller (Dell 0: off).
    pub sd: bool,
    /// `PMxA0` bits 0 (HW-ACPI input `+0x20`), 1 (`PcdTimerTickTrack`),
    /// 10 (`PcdClockInterruptTag`); Dell 0/0/0.
    pub pm_a0: (bool, bool, bool),
    /// `PcdToggleAllPwrGoodOnCf9` (Dell 0).
    pub toggle_pwrgood_on_cf9: bool,
    /// `PcdSerialDebugBusEnable` (Dell 0): `FED8_1000h[2:0]` = 6.
    pub serial_debug_bus: bool,
}

impl Misc {
    /// Dell's build values [P].
    pub const DEFAULT: Self = Self {
        none_sio_kbc: false,
        power_failure: None,
        hpet: true,
        hpet_msi: false,
        sd: false,
        pm_a0: (false, false, false),
        toggle_pwrgood_on_cf9: false,
        serial_debug_bus: false,
    };
}

/// Step 1's register writes: the watchdog decode and `SMIx78/79`.
pub fn reset(acpi: &impl Window, policy: &Reset) {
    acpi.at(PM_DECODE)
        .modify(Decode::WATCHDOG.val(policy.watchdog_decode.into()));
    acpi.at(SMI_SCI_LEVEL_GEVENTS).set(SCI_LEVEL_GEVENTS);
}

/// The RTC extension's registers (`PMx5E` index, `PMx5F` data) hold valid
/// values (`0xf25150`): the vendor rewrites them all
/// ([`super::hwacpi::RTC_EXT`]) unless they do.
pub fn rtc_ext_valid(acpi: &impl Window) -> bool {
    let read = |index: u8| {
        acpi.at(acpimmio::PM_RTC_EXT_INDEX).set(index);
        acpi.at(acpimmio::PM_RTC_EXT_DATA).get()
    };
    let r1 = read(1);
    let r2 = read(2) & 0x1f;
    let r3 = read(3) & 0x3f;
    let r4 = read(4) & 0x1f;
    let r10 = read(0x10) >> 1 & 3;
    r1 != 0xff
        && r1 & 0x3f < 2
        && r2 < 2
        && !(10..=15).contains(&r3)
        && !(26..=31).contains(&r3)
        && r3 < 37
        && !(10..=15).contains(&r4)
        && r4 < 19
        && r10 < 3
}

/// Rewrite the RTC extension if [`rtc_ext_valid`] says it is not; `true`
/// if it was rewritten.
pub fn rtc_ext(acpi: &impl Window) -> bool {
    if rtc_ext_valid(acpi) {
        return false;
    }
    crate::window::apply(acpi, super::hwacpi::RTC_EXT);
    true
}

/// `FchHwAcpiInitWaFeature`, in the vendor's order: RTC register B's
/// daylight saving off with the RTC extension's bank bit (`PMx5E` index 0,
/// `PMx5F[0]`) set, register A's DV0 cleared with it clear; PCI_INTR
/// 08h/09h ([`WaFeatures::keep_intr_misc`]); the ASF status cleared
/// (`SMBus + 0x2A` = `2Fh`, the HW-ACPI input's SMBus base `0xB00`; the
/// vendor's `0xFFE0` guard never matches a `0xFF00`-masked base); a PCIe
/// wake status cleared when an enabled SMI event is pending or a power
/// button or RTC status is set; `PMx54` (serial IRQ); RTC register D's
/// invalid `3Fh` date alarm cleared. The vendor ends with the FCH IOAPIC's
/// ID (`PcdCfgFchIoapicId` 0x80), which the SoC sets with its IOAPIC.
pub fn wa_features(fch: &impl FchHw, policy: &WaFeatures) {
    let (acpi, io) = (fch.acpi(), fch.io());
    let bank = |set: bool| {
        acpi.at(acpimmio::PM_RTC_EXT_INDEX).set(0);
        let data = acpi.at(acpimmio::PM_RTC_EXT_DATA);
        data.set(data.get() & !1 | u8::from(set));
    };
    bank(true);
    rtc::register::<_, rtc::Control::Register>(io, rtc::CONTROL)
        .modify(rtc::Control::DST_EN::CLEAR);
    bank(false);
    rtc::register::<_, rtc::FreqSelect::Register>(io, rtc::FREQ_SELECT)
        .modify(rtc::FreqSelect::BIT4::CLEAR);
    let misc = irq::index(irq::MISC, irq::Mode::Pic);
    irq::update(io, misc, 0, if policy.keep_intr_misc { 0 } else { 0x0a });
    let misc0 = irq::index(irq::MISC0, irq::Mode::Pic);
    let keep = if policy.keep_intr_misc { 0xff } else { 0xf9 };
    irq::write(io, misc0, irq::read(io, misc0) & keep & 0x9f | 0x10);
    io.at(At::<u8>::new((SMBUS_IO + 0x2a).into())).set(0x2f);
    let status = io.at(At::<u8, Pm1StatusHigh::Register>::new(
        u32::from(acpi.at(PM_ACPI_PM1_EVT).get()) + 1,
    ));
    let pending = acpi.at(smi::EVENT_ENABLE).get() & acpi.at(smi::EVENT_STATUS).get();
    if pending != 0 {
        status.write(Pm1StatusHigh::PCIEXP_WAKE::SET);
    }
    let now = status.extract();
    if now.is_set(Pm1StatusHigh::PWRBTN) || now.is_set(Pm1StatusHigh::RTC) {
        status.write(Pm1StatusHigh::PCIEXP_WAKE::SET);
    }
    let serial = acpi.at(PM_SERIAL_IRQ);
    if policy.oscout1_clk_continuous {
        serial.modify(SerialIrq::QUIET_MODE::CLEAR);
    }
    if policy.serial_irq {
        serial.modify(SerialIrq::ENABLE::SET);
    }
    let valid = rtc::register::<_, rtc::Valid::Register>(io, rtc::VALID);
    let d = valid.get();
    if d & 0x3f == 0x3f {
        valid.set(d & 0x80);
    }
}

/// Step 2's writes beyond [`super::hwacpi::PRE_PCIE`]: the SMBus
/// function's bytes and the cold-reset acknowledge.
pub fn pre_pcie(fch: &impl FchHw, policy: &ColdReset) {
    let smbus = fch.config(crate::fch::smbus::function());
    for (offset, value) in SMBUS_PCI {
        smbus.write8(offset, value);
    }
    let reg = fch.acpi().at(PM_WDT_COLD_RESET);
    let timer = WdtColdReset::TIMER.val(policy.timer);
    let keep = WdtColdReset::KEEP.val(reg.read(WdtColdReset::KEEP));
    if policy.ack {
        // `(x & 0xF800) | timer`, bit 15 set, bit 14 the lock.
        reg.write(
            keep + WdtColdReset::ACK::SET + WdtColdReset::LOCK.val(policy.lock.into()) + timer,
        );
    } else {
        // `(x & 0x3800) | timer`.
        reg.write(keep + timer);
    }
}

/// Step 3 (`0xf26650` after PrePcie).
pub fn misc(fch: &impl FchHw, policy: &Misc) {
    let acpi = fch.acpi();
    acpi.at(PM_ED)
        .modify(PmEd::NONE_SIO_KBC.val(policy.none_sio_kbc.into()));
    acpi.at(PM_BB).modify(PmBb::BIT6::CLEAR);
    acpi.at(PM_ACPI_CONFIG3).modify(AcpiConfig3::FIELD.val(4));
    if let Some(mode) = policy.power_failure {
        // As the vendor writes PMx5B: `(x & 0xF0) | (policy & 3) | 4`.
        acpi.at(PM_RTC_SHADOW).modify(
            RtcShadow::POWER_FAILURE.val(mode & 3)
                + RtcShadow::POWER_STATE::SET
                + RtcShadow::BIT3::CLEAR,
        );
    }
    acpi.at(PM_DECODE).modify(
        Decode::HPET.val(policy.hpet.into()) + Decode::HPET_MSI.val(policy.hpet_msi.into()),
    );
    crate::fch::uart::env(fch);
    if !policy.sd {
        acpimmio::legacy_sd_off(acpi);
    }
    let (b0, tick, tag) = policy.pm_a0;
    acpi.at(PM_A0).modify(
        PmA0::TIMER_TICK_TRACK.val(tick.into()) + PmA0::CLOCK_INTERRUPT_TAG.val(tag.into()),
    );
    acpi.at(PM_RESET_CONTROL0)
        .modify(ResetControl0::TOGGLE_ALL_PWR_GOOD.val(policy.toggle_pwrgood_on_cf9.into()));
    if policy.serial_debug_bus {
        acpi.at(SERIAL_DEBUG).modify(SerialDebug::BUS.val(6));
    }
    // `0xf24f90`: the bits ORed in once more, bit 0 with them.
    let a0 = acpi.at(PM_A0);
    a0.set(a0.get() | u32::from(b0) | u32::from(tick) << 1 | u32::from(tag) << 10);
}

/// The S5 reset status's late write (`FchInitPreOsHwAcpiQl`):
/// `(x & 0xF5E8) | 0x6010_0217`, its set bits write-1-to-clear statuses.
pub const fn late_s5_reset_status(x: u32) -> u32 {
    (x & 0xf5e8) | 0x6010_0217
}

/// Step 4 (`FchInitPreOsHwAcpiQl 0xf4a900` → `0xf4a7a0`): the GEVENTs
/// `SMIx78/79` name level-triggered, then the late `PMxC0` write unless
/// `keep_s5_reset_status` (`PcdNoClearS5ResetSts`, Dell 0). Whoever
/// reports the S5 reset status reads it before. The CPPC SCI masks are
/// not set (their gate, `PcdSmuFeatureControlDefines[22]` with
/// `PcdCppcSciBitMap`, is 0 on both donors).
pub fn pre_os(acpi: &impl Window, keep_s5_reset_status: bool) {
    let gevents = acpi.at(SMI_SCI_LEVEL_GEVENTS).get();
    let level = acpi.at(smi::SCI_LEVEL);
    for gevent in gevents.to_le_bytes() {
        if gevent < 32 {
            level.modify(smi::event_bit(gevent, true));
        }
    }
    if !keep_s5_reset_status {
        let status = acpi.at(acpimmio::PM_RESET_STATUS);
        status.set(late_s5_reset_status(status.get()));
    }
}

/// The break events an FCH latched (`PMx80[11:0]`), cleared once read as
/// the vendor's RAS DXE does; 0 when none.
pub fn take_break_events(acpi: &impl Window) -> u16 {
    let reg = acpi.at(PM_BREAK_EVENT);
    let errors = reg.read(BreakEvent::ERRORS) as u16;
    if errors != 0 {
        reg.modify(BreakEvent::ERRORS::CLEAR);
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::model::fch;

    #[test]
    fn rtc_ext_is_rewritten_only_when_invalid() {
        let hw = fch();
        // An all-zero RTC extension is valid.
        assert!(rtc_ext_valid(&hw.acpi));
        assert!(!rtc_ext(&hw.acpi));
        hw.acpi.put(PM + 0x5f, 1, 0xff);
        assert!(!rtc_ext_valid(&hw.acpi));
        assert!(rtc_ext(&hw.acpi));
        assert_eq!(hw.acpi.get(PM + 0x5e, 1), 0x14);
    }

    #[test]
    fn pre_pcie_writes_the_smbus_bytes_and_the_cold_reset_timer() {
        let hw = fch();
        hw.acpi.put(PM + 0xc6, 2, 0xffff);
        pre_pcie(&hw, &ColdReset::DEFAULT);
        assert_eq!(hw.acpi.get(PM + 0xc6, 2), 0x3800 | 46);
        let ack = ColdReset {
            ack: true,
            lock: true,
            timer: 5,
        };
        hw.acpi.put(PM + 0xc6, 2, 0x0000);
        pre_pcie(&hw, &ack);
        assert_eq!(hw.acpi.get(PM + 0xc6, 2), 0xc005);
    }

    #[test]
    fn misc_sets_hpet_without_msi_and_turns_sd_off() {
        let hw = fch();
        hw.acpi.put(PM, 4, 1 << 29);
        hw.acpi.put(PM + 0xbb, 1, 0xff);
        misc(&hw, &Misc::DEFAULT);
        assert_eq!(hw.acpi.get(PM, 4), 1 << 6);
        assert_eq!(hw.acpi.get(PM + 0xbb, 1), 0xbf);
        assert_eq!(hw.acpi.get(PM + 0x77, 1) & 0xf, 4);
        assert_eq!(hw.mmio.get(crate::fch::uart::MISC_10, 1), 2);
        let tick = Misc {
            pm_a0: (true, true, false),
            ..Misc::DEFAULT
        };
        misc(&hw, &tick);
        assert_eq!(hw.acpi.get(PM + 0xa0, 4), 3);
    }

    #[test]
    fn pre_os_makes_gevent_11_level_and_clears_the_s5_statuses() {
        let hw = fch();
        reset(&hw.acpi, &Reset::DEFAULT);
        assert_eq!(hw.acpi.get(PM, 4) & 1 << 7, 1 << 7);
        hw.acpi.put(PM + 0xc0, 4, 0xffff_ffff);
        pre_os(&hw.acpi, false);
        assert_eq!(hw.acpi.get(SMI + 0x0c, 4), 1 << 11);
        assert_eq!(hw.acpi.get(PM + 0xc0, 4), 0x6010_f7ff);
        assert_eq!(late_s5_reset_status(0), 0x6010_0217);
    }

    #[test]
    fn wa_features_follow_the_vendor() {
        let hw = fch();
        // PM1 block at 0x800, PWRBTN_STS set.
        hw.acpi.put(PM + 0x60, 2, 0x800);
        hw.io.put(0x801, 1, 0x01);
        hw.acpi.put(PM + 0x54, 1, 0x50);
        hw.acpi.put(PM + 0x5f, 1, 0xf0);
        hw.io.set_at(0x71, 0x0b, 0xff);
        hw.io.set_at(0x71, 0x0a, 0xff);
        hw.io.set_at(0x71, 0x0d, 0xbf);
        hw.io.set_at(0xc01, 0x08, 0x00);
        hw.io.set_at(0xc01, 0x09, 0xff);
        wa_features(&hw, &WaFeatures::DEFAULT);
        // The bank bit set for register B, cleared again for A.
        assert_eq!(hw.acpi.writes_to(PM + 0x5f), [0xf1, 0xf0]);
        assert_eq!(hw.io.at(0x71, 0x0b), 0xfe);
        assert_eq!(hw.io.at(0x71, 0x0a), 0xef);
        assert_eq!(hw.io.at(0xc01, 0x08), 0x0a);
        assert_eq!(hw.io.at(0xc01, 0x09), 0x99);
        assert_eq!(hw.io.writes_to(0xb2a), [0x2f]);
        assert_eq!(hw.io.writes_to(0x801), [0x40]);
        assert_eq!(hw.acpi.get(PM + 0x54, 1), 0xd0);
        // Register D's all-ones date alarm cleared, VRT kept.
        assert_eq!(hw.io.at(0x71, 0x0d), 0x80);

        let keep = WaFeatures {
            keep_intr_misc: true,
            oscout1_clk_continuous: true,
            serial_irq: false,
        };
        let hw = fch();
        hw.io.set_at(0xc01, 0x09, 0xff);
        hw.acpi.put(PM + 0x54, 1, 0x50);
        wa_features(&hw, &keep);
        assert_eq!(hw.io.at(0xc01, 0x08), 0);
        assert_eq!(hw.io.at(0xc01, 0x09), 0x9f);
        assert_eq!(hw.acpi.get(PM + 0x54, 1), 0x10);
        assert!(hw.io.writes_to(0x1).is_empty());
        // An enabled SMI event pending clears the PCIe wake status too.
        hw.acpi.put(SMI, 4, 1 << 3);
        hw.acpi.put(SMI + 4, 4, 1 << 3);
        wa_features(&hw, &keep);
        assert_eq!(hw.io.writes_to(0x1), [0x40]);
    }

    #[test]
    fn watchdog_clock_stops_it_sets_the_period_and_enables() {
        let hw = fch();
        hw.acpi.put(PM, 4, 0x0f00_0080);
        // Left running and fired by the boot before, a count reload pending.
        hw.acpi.put(0xb00, 4, 0x87);
        assert!(watchdog_clock(&hw.acpi, WatchdogClock::S1));
        assert_eq!(hw.acpi.get(PM, 4), 0x0300_0080);
        // Stopped; fired and the action are not written (kept for the OS).
        assert_eq!(hw.acpi.get(0xb00, 4), 0x04);
        hw.acpi.put(0xb00, 4, 0);
        assert!(!watchdog_clock(&hw.acpi, WatchdogClock::Ms10));
        assert_eq!(hw.acpi.get(PM + 3, 1), 1);
        // Nothing decoded there: nothing written.
        hw.acpi.put(0xb00, 4, u32::MAX);
        assert!(!stop_watchdog(&hw.acpi));
        assert_eq!(hw.acpi.get(0xb00, 4), u32::MAX);
        assert_eq!(WatchdogClock::S1.period_ms(), 1000);
        assert_eq!(WatchdogClock::Ms100.period_ms(), 100);
    }

    #[test]
    fn break_events_are_read_once() {
        let hw = fch();
        assert_eq!(take_break_events(&hw.acpi), 0);
        hw.acpi.put(PM + 0x80, 4, 0x1234_5a5a);
        assert_eq!(take_break_events(&hw.acpi), 0xa5a);
        assert_eq!(hw.acpi.get(PM + 0x80, 4), 0x1234_5000);
        assert_eq!(take_break_events(&hw.acpi), 0);
    }
}
