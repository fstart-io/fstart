//! User options on the Intel CAR platforms (`fstart_options`). These
//! boards have no flash settings store: the options live in CMOS RAM, by
//! key, and every stage reads them without allocating. The chipset drivers
//! declare their knobs ([`IntelNorthbridgeDriver::KNOBS`],
//! [`IntelSouthbridgeDriver::KNOBS`]); each stage applies them to its
//! drivers before their early init, so the bootblock (stolen memory, SATA
//! mode) and the ramstage (power after a failure, NMIs) agree.

use fstart_driver_intel::{IntelNorthbridgeDriver, IntelSouthbridgeDriver};
use fstart_options::Source;
use fstart_options::cmos::{Cmos, Ram, Window};

/// The CMOS bytes the options keep, after the clock and its
/// century/alarm bytes; the last two hold the checksum.
pub const WINDOW: Window = Window {
    first: 0x40,
    last: 0x7f,
};
const _: () = assert!(WINDOW.check());

/// CMOS RAM bank 0.
static RAM: fstart_driver_rtc::Cmos = fstart_driver_rtc::Cmos;

/// The options in CMOS.
pub fn store() -> Cmos<&'static (dyn Ram + Sync)> {
    Cmos::new(WINDOW, &RAM as &(dyn Ram + Sync))
}

unsafe extern "C" {
    /// The board's chipset option records, from `entry!`.
    static FSTART_CHIPSET_OPTIONS: u8;
}

/// Keep the chipset's option records in a stage that resolves its options.
fn records() {
    // SAFETY: a byte of the records `entry!` defines in every stage.
    unsafe { core::ptr::read_volatile(&raw const FSTART_CHIPSET_OPTIONS) };
}

/// The console level, then the chipset's options: what a stage does once
/// its console is up, before the chipset's early init. Allocates nothing.
pub fn chipset<N: IntelNorthbridgeDriver, S: IntelSouthbridgeDriver>(
    northbridge: &mut N,
    southbridge: &mut S,
) {
    use fstart_log::option::resolve;
    records();
    let source = store();
    level(&source);
    let mut options = northbridge.options();
    resolve(N::KNOBS, &source, &mut options, |_| Ok(()));
    northbridge.set_options(options);
    let mut options = southbridge.options();
    resolve(S::KNOBS, &source, &mut options, |_| Ok(()));
    southbridge.set_options(options);
}

/// The console level only (postcar).
pub fn level(source: &dyn Source) {
    let mut level = fstart_log::max_level();
    fstart_log::option::resolve(&fstart_log::option::KNOBS, source, &mut level, |_| Ok(()));
    // SAFETY: the BSP, before MP.
    unsafe { fstart_log::set_max_level(level) };
}

/// [`chipset`], keeping the outcomes in `report` for the payload's menu
/// (the ramstage).
pub fn chipset_into<N: IntelNorthbridgeDriver, S: IntelSouthbridgeDriver>(
    northbridge: &mut N,
    southbridge: &mut S,
    report: &mut fstart_options::Report,
) {
    use fstart_log::option::resolve_into;
    records();
    let source = store();
    let mut level = fstart_log::max_level();
    resolve_into(
        &fstart_log::option::KNOBS,
        &source,
        &mut level,
        report,
        |_| Ok(()),
    );
    // SAFETY: the BSP, before MP.
    unsafe { fstart_log::set_max_level(level) };
    let mut options = northbridge.options();
    resolve_into(N::KNOBS, &source, &mut options, report, |_| Ok(()));
    northbridge.set_options(options);
    let mut options = southbridge.options();
    resolve_into(S::KNOBS, &source, &mut options, report, |_| Ok(()));
    southbridge.set_options(options);
}
