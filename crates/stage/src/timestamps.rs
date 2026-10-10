//! Boot timestamp report at payload handoff.

/// Record the payload jump and report the boot timestamps.
pub fn handoff() {
    fstart_timestamp::add(fstart_timestamp::id::PAYLOAD_JUMP);
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    report(fstart_arch::x86::tsc_frequency_hz());
}

/// One report line. The report is printed whatever the log level, short
/// of `off`, so that quiet builds can be measured too.
macro_rules! line {
    ($($args:tt)*) => {
        if !matches!(fstart_log::BUILD_VERBOSITY, Some(fstart_log::Verbosity::Off)) {
            let mut w = fstart_log::writer();
            let _ = ::ufmt::uwrite!(w, "[TIME ] ");
            let _ = ::ufmt::uwriteln!(w, $($args)*);
            drop(w);
            fstart_log::flush();
        }
    };
}

/// Log every milestone with its time since reset and the time until the
/// next one, split into console output and the rest. The last milestone
/// has no duration.
pub fn report(tick_hz: u64) {
    let Some((records, _, _)) = fstart_timestamp::snapshot() else {
        return;
    };
    let us = |ticks: u64| (u128::from(ticks) * 1_000_000 / u128::from(tick_hz.max(1))) as u64;
    line!(
        "timestamps: us since reset, +us until the next (console us); TSC {} kHz",
        tick_hz / 1000
    );
    for (record, next) in records
        .iter()
        .zip(records.iter().skip(1).map(Some).chain([None]))
    {
        let name = fstart_timestamp::name(record.id);
        match next {
            Some(next) => line!(
                "ts {} {} +{} ({}) {}",
                record.id.0,
                us(record.stamp),
                us(next.stamp.wrapping_sub(record.stamp)),
                us(next.console.wrapping_sub(record.console)),
                name
            ),
            None => line!("ts {} {} {}", record.id.0, us(record.stamp), name),
        }
    }
    if let (Some(first), Some(last)) = (records.first(), records.last()) {
        let total = last.stamp;
        let console = last.console.wrapping_sub(first.console);
        line!(
            "timestamps: {} us from reset to {}, console {} us, other {} us",
            us(total),
            fstart_timestamp::name(last.id),
            us(console),
            us(total.saturating_sub(console))
        );
    }
}
