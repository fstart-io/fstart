//! The ICH's user options, under coreboot's names and values: the AC power
//! state after a power failure, the SATA mode and NMI sources. A driver
//! starts from its board config ([`Options::from_config`]) and the
//! platform resolves the knobs over it (`fstart_options`).

use fstart_options::{Availability, Choice, Kind, Knob, Meta};

/// SATA controller operating mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SataMode {
    Ide,
    Ahci,
}

/// What a board's ICH options can change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// After an AC power loss: 0 stay off, 1 power on, 2 the state before
    /// the loss (ICH9).
    pub power_on_after_fail: u8,
    /// The SATA controller's mode; `None` if the board has no SATA.
    pub sata_mode: Option<SataMode>,
    /// Let SERR# and IOCHK# raise NMIs.
    pub nmi: bool,
}

impl Options {
    /// The board's values: its power state and SATA mode, NMIs off (as
    /// coreboot's default).
    pub const fn from_config(power_on_after_fail: u8, sata_mode: Option<SataMode>) -> Self {
        Self {
            power_on_after_fail,
            sata_mode,
            nmi: false,
        }
    }
}

const POWER: &[Choice] = &[Choice::new(0, "Stay off"), Choice::new(1, "Power on")];
const POWER_KEEP: &[Choice] = &[
    Choice::new(0, "Stay off"),
    Choice::new(1, "Power on"),
    Choice::new(2, "Previous state"),
];

const fn power_meta(choices: &'static [Choice]) -> Meta {
    Meta {
        key: "power_on_after_fail",
        form: "Power",
        label: "Restore on AC power loss",
        help: "What the system does when AC power returns.",
        kind: Kind::Enum(choices),
    }
}

const fn power(meta: &'static Meta) -> Knob<Options> {
    Knob::new(
        meta,
        |o: &Options| o.power_on_after_fail.into(),
        |o: &mut Options, v| o.power_on_after_fail = v as u8,
    )
}

/// The power state after a power failure, off or on (ICH7, ICH8).
pub const POWER_ON_AFTER_FAIL: Knob<Options> = power(fstart_options::meta!(power_meta(POWER)));
/// The power state after a power failure, also the previous one (ICH9).
pub const POWER_ON_AFTER_FAIL_KEEP: Knob<Options> =
    power(fstart_options::meta!(power_meta(POWER_KEEP)));

/// The SATA mode, coreboot's 0 AHCI, 1 IDE.
pub const SATA_MODE: Knob<Options> = Knob::new(
    fstart_options::meta!(Meta {
        key: "sata_mode",
        form: "Storage",
        label: "SATA mode",
        help: "AHCI, or legacy IDE for operating systems without an AHCI driver.",
        kind: Kind::Enum(&[Choice::new(0, "AHCI"), Choice::new(1, "IDE")]),
    }),
    |o: &Options| matches!(o.sata_mode, Some(SataMode::Ide)).into(),
    |o: &mut Options, v| {
        o.sata_mode = Some(if v == 1 {
            SataMode::Ide
        } else {
            SataMode::Ahci
        })
    },
)
.when(|o: &Options| match o.sata_mode {
    Some(_) => Availability::Editable,
    None => Availability::Inactive("this board has no SATA controller"),
});

/// NMIs on SERR# and IOCHK#.
pub const NMI: Knob<Options> = Knob::new(
    fstart_options::meta!(Meta::bool(
        "nmi",
        "Advanced",
        "NMI sources",
        "Raise an NMI on PCI SERR# and IOCHK#.",
    )),
    |o: &Options| o.nmi.into(),
    |o: &mut Options, v| o.nmi = v != 0,
);

/// Program the legacy NMI sources (ports 0x61, 0x70/0x74), as coreboot's
/// ICH LPC init does: IOCHK# on, SERR# off, the NMI mask from `enable`.
pub fn nmi(enable: bool) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    // SAFETY: the legacy NMI control ports of every ICH.
    unsafe {
        use fstart_core::pio::{inb, outb};
        let port61 = (inb(0x61) & 0x0f & !(1 << 3)) | 1 << 2;
        outb(0x61, port61);
        // Port 0x70 is write-only; 0x74 is its readable alias.
        let index = inb(0x74);
        outb(
            0x70,
            if enable {
                index & !(1 << 7)
            } else {
                index | 1 << 7
            },
        );
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    let _ = enable;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sata_mode_is_only_offered_with_sata() {
        let none = Options::from_config(1, None);
        assert_eq!(
            (SATA_MODE.available)(&none),
            Availability::Inactive("this board has no SATA controller")
        );
        let mut ahci = Options::from_config(1, Some(SataMode::Ahci));
        assert_eq!((SATA_MODE.get)(&ahci), 0);
        (SATA_MODE.set)(&mut ahci, 1);
        assert_eq!(ahci.sata_mode, Some(SataMode::Ide));
        assert!(!POWER_ON_AFTER_FAIL.meta.kind.contains(2));
        assert!(POWER_ON_AFTER_FAIL_KEEP.meta.kind.contains(2));
    }
}
