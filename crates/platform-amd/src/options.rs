// SPDX-License-Identifier: MIT OR Apache-2.0
//! The options the user may override (`fstart_options`), for every SoC:
//! the overrides in the SoC's settings store, applied over the board's
//! values ([`resolve`]). The store is CrabEFI's EFI variable store, which
//! its menu writes. Every payload's boot reads it; only the UEFI payload
//! can change it. A SoC without a store gets the board's values.

use fstart_options::varstore::VarstoreOptions;
use fstart_options::{Knob, Report, Source};

/// The overrides stored in `nvram`, the ROM view of the settings store.
pub fn source(nvram: Option<&'static [u8]>) -> impl Source {
    nvram.map(VarstoreOptions::new)
}

/// [`fstart_options::resolve_checked`] from the settings store, logged.
pub fn resolve<C: Clone>(
    nvram: Option<&'static [u8]>,
    knobs: &'static [Knob<C>],
    config: &mut C,
    report: &mut Report,
    check: impl FnOnce(&C) -> Result<(), &'static str>,
) {
    fstart_log::option::resolve_into(knobs, &source(nvram), config, report, check);
}
